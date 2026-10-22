use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};

/// 模型内存驻留分级（对齐 sherpa-onnx-and-ort-unified-runtime-spec 架构规范）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelTier {
    /// Tier 1: 常驻轻量（PP-OCRv6 Det, MobileNetV2 NSFW, Silero VAD, GTCRN 降噪）
    Tier1,
    /// Tier 2: 高频按需（SenseVoice ASR, Mobile-CLIP, CED AudioTag, Bekko 文本向量, Gemma）
    Tier2,
    /// Tier 3: 空闲释放（Chinese-CLIP 文本塔 390MB, RAM++ 180MB，无请求 300 秒后自动清空释放）
    Tier3,
}

impl ModelTier {
    /// 默认空闲回收超时时间（Tier 3 为 300 秒）
    pub fn idle_timeout(&self) -> Option<Duration> {
        match self {
            ModelTier::Tier3 => Some(Duration::from_secs(300)),
            _ => None,
        }
    }
}

/// Windows 环境安全 DLL 搜索目录配置（Win32 API SetDllDirectoryW 封装）
#[cfg(target_os = "windows")]
pub fn set_safe_dll_directory(sub_dir: &str) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use std::path::PathBuf;

    // 优先支持环境变量 FIREFLY_ONNX_DIR 覆盖
    let target_path = if let Ok(custom_dir) = std::env::var("FIREFLY_ONNX_DIR") {
        PathBuf::from(custom_dir)
    } else if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            exe_dir.join(sub_dir)
        } else {
            PathBuf::from(sub_dir)
        }
    } else {
        PathBuf::from(sub_dir)
    };

    let path_to_set = if target_path.exists() {
        target_path
    } else {
        PathBuf::from(sub_dir)
    };

    let wide: Vec<u16> = path_to_set
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        extern "system" {
            fn SetDllDirectoryW(lpPathName: *const u16) -> i32;
        }
        SetDllDirectoryW(wide.as_ptr()) != 0
    }
}

#[cfg(not(target_os = "windows"))]
pub fn set_safe_dll_directory(_sub_dir: &str) -> bool {
    true
}

/// 统一 ONNX Runtime 单例管理器
pub struct OmniOrtRuntime;

static RUNTIME_INIT: OnceLock<Result<(), String>> = OnceLock::new();

impl OmniOrtRuntime {
    /// 自动发现本地有效的 ONNX Runtime DLL 路径 (Windows 防劫持与自定位)
    pub fn discover_onnx_dll() -> Option<std::path::PathBuf> {
        use std::path::PathBuf;
        if let Ok(p) = std::env::var("ORT_DYLIB_PATH") {
            let pb = PathBuf::from(p);
            if pb.exists() {
                return Some(pb);
            }
        }
        if let Ok(dir) = std::env::var("FIREFLY_ONNX_DIR") {
            let pb = PathBuf::from(dir).join("onnxruntime.dll");
            if pb.exists() {
                return Some(pb);
            }
        }
        let mut check_dirs = Vec::new();
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(parent) = exe_path.parent() {
                check_dirs.push(parent.to_path_buf());
            }
        }
        if let Ok(cur) = std::env::current_dir() {
            check_dirs.push(cur);
        }
        for base in check_dirs {
            let mut cur = base;
            for _ in 0..6 {
                let candidates = [
                    cur.join("onnxruntime.dll"),
                    cur.join("bin/omni/onnxruntime.dll"),
                    cur.join("resources/bin/onnx/onnxruntime.dll"),
                    cur.join("apps/desktop/build/extraResources/bin/onnx/onnxruntime.dll"),
                    cur.join("build/extraResources/bin/onnx/onnxruntime.dll"),
                    cur.join("target/debug/onnxruntime.dll"),
                    cur.join("target/release/onnxruntime.dll"),
                ];
                for c in &candidates {
                    if c.exists() {
                        return Some(c.clone());
                    }
                }
                if let Some(parent) = cur.parent() {
                    cur = parent.to_path_buf();
                } else {
                    break;
                }
            }
        }
        None
    }

    /// 在应用启动时初始化 ONNX Runtime 动态库搜索与加载
    pub fn init() -> Result<(), String> {
        #[cfg(target_os = "windows")]
        {
            if let Some(dll_path) = Self::discover_onnx_dll() {
                if let Some(parent) = dll_path.parent() {
                    let _ = set_safe_dll_directory(parent.to_str().unwrap_or(""));
                }
                return Self::init_with_dll(dll_path.to_str().unwrap_or("onnxruntime.dll"));
            }
        }
        Self::init_with_dll("onnxruntime.dll")
    }

    /// 指定特定 DLL 路径初始化
    pub fn init_with_dll(dll_path: &str) -> Result<(), String> {
        RUNTIME_INIT
            .get_or_init(|| {
                #[cfg(target_os = "windows")]
                {
                    // 在 Windows 环境启动入口处建立安全 DLL 搜索隔离，免疫劫持
                    // 若 dll_path 为显式路径，优先使用其父目录；否则回退至 "resources/bin/onnx"
                    let candidate_dir = std::path::Path::new(dll_path)
                        .parent()
                        .filter(|p| !p.as_os_str().is_empty())
                        .and_then(|p| p.to_str())
                        .unwrap_or("resources/bin/onnx");
                    set_safe_dll_directory(candidate_dir);
                }

                // 实现全局动态加载入口：ort::init_from("onnxruntime.dll")
                match ort::init_from(dll_path).commit() {
                    Ok(_) => {
                        tracing::info!("OmniOrtRuntime: 成功动态加载 ONNX Runtime ({})", dll_path);
                        Self::log_execution_provider_capability();
                        Ok(())
                    }
                    Err(e) => {
                        let err_msg = format!("OmniOrtRuntime: 动态加载 ONNX Runtime 失败: {}", e);
                        tracing::warn!("{}", err_msg);
                        Err(err_msg)
                    }
                }
            })
            .clone()
    }

    /// 确保已初始化
    pub fn ensure_initialized() {
        let _ = Self::init();
    }

    /// ORT CPU 会话 intra-op 线程配额下限：即使单核机器也至少给 2 线程，避免前向完全串行化
    pub const MIN_INTRA_THREADS: usize = 2;

    /// ORT CPU 会话 intra-op 线程配额上限：可用并行度 ≥ 8 的机器放开至 8 线程，吃满多核算力
    /// （旧实现为 `available_parallelism().min(4)`，即逻辑处理器口径上限 4；逻辑核 ≥ 8 的机器
    /// 至少有半数算力闲置）
    ///
    /// 注：该上限只约束**单个 Session**，不是进程级线程总预算。视觉塔与文本塔各持一个常驻
    /// Session（`omni-pro/crates/omni-vision/src/gemma.rs` 的视觉/文本构建点），双通道并发
    /// 分析时两池线程数相加会超过物理核数；这是 SPEC §4.5 明确采纳的取舍，进程级配额需另行立项。
    pub const MAX_INTRA_THREADS: usize = 8;

    /// 按可用并行度映射 intra_threads 配额：`clamp(2, 8)`
    ///
    /// 抽成纯函数以便单测直接断言「8 核 ⇒ 8 线程」的映射关系，不依赖跑测机器的真实核数。
    /// 入参取 `std::thread::available_parallelism()`，其上限为**逻辑处理器数**（含 SMT），
    /// 故 4 核 8 线程的机器通常分配到 8 线程；受 cgroup CPU 配额、亲和性掩码或 Windows
    /// Job Object 限核时该值可能更小，届时按实际可用并行度如实分配。
    pub fn intra_threads_for_cores(cores: usize) -> usize {
        cores.clamp(Self::MIN_INTRA_THREADS, Self::MAX_INTRA_THREADS)
    }

    /// CPU 线程池：统一配置 intra_threads = clamp(可用并行度, 2, 8)
    ///
    /// 保留下限 2 防止单核机器被压成串行；上限 8 控制单 Session 的线程占用。
    pub fn recommended_intra_threads() -> usize {
        Self::intra_threads_for_cores(
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4),
        )
    }

    /// ASR (sherpa-onnx) 线程配额：沿用历史 `available_parallelism().min(4)` 上限
    ///
    /// 与 ORT 视觉/文本塔的 8 线程配额刻意分离：ASR 与视觉塔可能在同一视频任务内交替运行，
    /// 两路各占 8 线程会造成线程超额争抢，故 ASR 侧维持原有 4 线程上限。
    pub fn recommended_asr_threads() -> usize {
        std::thread::available_parallelism()
            .map(|n| n.get().min(4))
            .unwrap_or(4)
    }

    /// 启动时显式盘点当前 DLL 的 EP 能力落点，杜绝「Session 构建成功但实跑 CPU」的静默回退不可观测
    pub fn log_execution_provider_capability() {
        use ort::execution_providers::ExecutionProvider;

        #[cfg(target_os = "windows")]
        {
            let cuda = ort::execution_providers::CUDAExecutionProvider::default()
                .is_available()
                .unwrap_or(false);
            let dml = ort::execution_providers::DirectMLExecutionProvider::default()
                .is_available()
                .unwrap_or(false);
            tracing::info!(
                "OmniOrtRuntime: EP 能力盘点 CUDA={}, DirectML={}",
                cuda,
                dml
            );
            if !cuda && !dml {
                tracing::warn!(
                    "OmniOrtRuntime: 当前 onnxruntime DLL 为纯 CPU 构建（无 CUDA/DirectML EP），全部推理将运行在 CPU 上"
                );
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            tracing::info!("OmniOrtRuntime: 非 Windows 平台，EP 能力盘点见 preferred_hardware_eps 探测日志");
        }
    }

    /// 获取当前平台优先推荐的硬件加速 Execution Provider 候选列表（按优先级排序）
    pub fn preferred_hardware_eps() -> Vec<ort::execution_providers::ExecutionProviderDispatch> {
        use ort::execution_providers::ExecutionProvider;

        let mut eps = Vec::new();

        #[cfg(target_os = "windows")]
        {
            // 优先检查当前加载的 ONNX Runtime 二进制是否编译了 CUDA (需 onnxruntime-gpu 构建)
            let cuda_ep = ort::execution_providers::CUDAExecutionProvider::default();
            if cuda_ep.is_available().unwrap_or(false) {
                tracing::info!("OmniOrtRuntime: 探测到当前 ONNX Runtime 包含 CUDA 硬件加速支持");
                eps.push(cuda_ep.build());
            }

            // 检查 DirectML 支持 (当前随包分发的 onnxruntime-directml 构建，覆盖全系 GPU/NPU)
            // 门控收紧：is_available 必须明确为 true 才入列，杜绝纯 CPU DLL 被误判为具备 DML 能力
            let dml_ep = ort::execution_providers::DirectMLExecutionProvider::default();
            if dml_ep.is_available().unwrap_or(false) {
                tracing::info!("OmniOrtRuntime: 探测到当前 ONNX Runtime 包含 DirectML 硬件加速支持");
                eps.push(dml_ep.build());
            }
        }

        #[cfg(target_os = "macos")]
        {
            let coreml_ep = ort::execution_providers::CoreMLExecutionProvider::default();
            if coreml_ep.is_available().unwrap_or(true) {
                eps.push(coreml_ep.build());
            }
        }

        #[cfg(target_os = "linux")]
        {
            let cuda_ep = ort::execution_providers::CUDAExecutionProvider::default();
            if cuda_ep.is_available().unwrap_or(false) {
                eps.push(cuda_ep.build());
            }
        }

        // 显式上报硬件加速缺席：严禁静默回退 CPU 后在日志中不可见
        if eps.is_empty() {
            tracing::warn!(
                "OmniOrtRuntime: 未探测到任何硬件加速 EP 候选，本平台全部 Session 将回退为纯 CPU 推理"
            );
        }

        eps
    }

    /// 获取首选硬件加速 Execution Provider (向后兼容)
    pub fn preferred_hardware_ep() -> Option<ort::execution_providers::ExecutionProviderDispatch> {
        Self::preferred_hardware_eps().into_iter().next()
    }

    /// 执行提供商 (EP) 两阶段降级路由工厂
    pub fn build_session<P: AsRef<Path>>(
        model_path: P,
        tier: ModelTier,
    ) -> Result<ort::session::Session, ort::Error> {
        Self::ensure_initialized();
        let path = model_path.as_ref();
        let intra_threads = Self::recommended_intra_threads();
        let eps = Self::preferred_hardware_eps();
        Self::build_session_with_eps_internal(path, tier, eps, intra_threads)
    }

    /// 统一两阶段执行提供商路由逻辑
    pub fn execute_two_stage_fallback<F1, F2, S, E>(
        model_name: &str,
        has_hardware_ep: bool,
        mut stage1_builder: F1,
        mut stage2_builder: F2,
    ) -> Result<S, E>
    where
        F1: FnMut() -> Result<S, E>,
        F2: FnMut() -> Result<S, E>,
        E: std::fmt::Debug,
    {
        if has_hardware_ep {
            match stage1_builder() {
                Ok(session) => {
                    tracing::info!(
                        "OmniOrtRuntime: 成功构建硬件加速 Session (model={})",
                        model_name
                    );
                    return Ok(session);
                }
                Err(err) => {
                    // 阶段二（显式降级）：底层 GPU 不支持或报错时，外层捕获并输出 Warning 日志，优雅重新构建纯 CPU Session
                    tracing::warn!(
                        "OmniOrtRuntime: 硬件加速 EP 初始化失败，显式降级为纯 CPU Session (model={}, error={:?})",
                        model_name,
                        err
                    );
                }
            }
        }

        let cpu_session = stage2_builder()?;
        tracing::info!(
            "OmniOrtRuntime: 成功构建纯 CPU Session (model={})",
            model_name
        );
        Ok(cpu_session)
    }

    /// 内部多阶段构建逻辑：按优先级依次尝试硬件加速 EP，最终优雅回退到多线程优化 CPU Session
    pub fn build_session_with_eps_internal(
        path: &Path,
        _tier: ModelTier,
        eps: Vec<ort::execution_providers::ExecutionProviderDispatch>,
        intra_threads: usize,
    ) -> Result<ort::session::Session, ort::Error> {
        let path_str = path.display().to_string();

        for (idx, ep) in eps.into_iter().enumerate() {
            let res = (|| {
                let mut b = ort::session::Session::builder()?
                    .with_intra_threads(intra_threads)?
                    .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)?;
                b = b.with_execution_providers([ep])?;
                b.commit_from_file(path)
            })();

            match res {
                Ok(session) => {
                    tracing::info!(
                        "OmniOrtRuntime: 成功构建硬件加速 Session (model={}, ep_candidate={})",
                        path_str,
                        idx
                    );
                    return Ok(session);
                }
                Err(err) => {
                    tracing::warn!(
                        "OmniOrtRuntime: 硬件加速候选 EP[{}] 初始化未命中或不支持，继续尝试下一个候选: {:?}",
                        idx,
                        err
                    );
                }
            }
        }

        // 最终兜底：纯 CPU Session
        let cpu_session = ort::session::Session::builder()?
            .with_intra_threads(intra_threads)?
            .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)?
            .commit_from_file(path)?;

        tracing::info!(
            "OmniOrtRuntime: 成功构建纯 CPU Session (model={})",
            path_str
        );
        Ok(cpu_session)
    }

    /// 内部两阶段构建逻辑，支持测试中模拟 EP 注入与失败 (兼容现有测试)
    pub fn build_session_with_ep_internal(
        path: &Path,
        tier: ModelTier,
        ep: Option<ort::execution_providers::ExecutionProviderDispatch>,
        intra_threads: usize,
    ) -> Result<ort::session::Session, ort::Error> {
        let eps = ep.into_iter().collect();
        Self::build_session_with_eps_internal(path, tier, eps, intra_threads)
    }
}

/// 看门狗内部状态，原子绑定实例与最近访问时间戳，消除 TOCTOU 状态撕裂竞态
struct WatchdogState<T> {
    item: Option<Arc<T>>,
    last_accessed: Instant,
}

/// 通用内存分级看门狗管理器 (针对 Tier 3 模型如 Chinese-CLIP 文本塔、RAM++)
/// 支持在无请求超过指定空闲时长（默认 300 秒）后自动清空释放资源，并在下次请求时按需重新加载。
pub struct GenericWatchdog<T> {
    timeout: Duration,
    state: Arc<RwLock<WatchdogState<T>>>,
    loader: Arc<dyn Fn() -> Option<T> + Send + Sync>,
    stop_signal: Arc<AtomicBool>,
}

impl<T: Send + Sync + 'static> GenericWatchdog<T> {
    /// 创建看门狗管理器，并启动后台检查线程
    pub fn new<F>(timeout: Duration, loader: F) -> Self
    where
        F: Fn() -> Option<T> + Send + Sync + 'static,
    {
        let state = Arc::new(RwLock::new(WatchdogState {
            item: None,
            last_accessed: Instant::now(),
        }));
        let stop_signal = Arc::new(AtomicBool::new(false));

        let state_clone = Arc::clone(&state);
        let stop_clone = Arc::clone(&stop_signal);

        // 启动看门狗巡检守护线程
        // 采用 1000ms (1秒) 巡检间隔，并优先通过读锁探测，杜绝空转高频争抢独占写锁
        std::thread::Builder::new()
            .name("tier3-watchdog".to_string())
            .spawn(move || {
                while !stop_clone.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(1000));
                    if stop_clone.load(Ordering::Relaxed) {
                        break;
                    }

                    // 快径探测：仅在 item 存在且已超时的情况下才去获取独占写锁
                    let should_evict = {
                        let guard = state_clone.read().unwrap();
                        guard.item.is_some() && guard.last_accessed.elapsed() >= timeout
                    };

                    if should_evict {
                        let mut guard = state_clone.write().unwrap();
                        // 在写锁保护下二次原子确认，杜绝 TOCTOU 竞态
                        if guard.item.is_some() && guard.last_accessed.elapsed() >= timeout {
                            tracing::info!(
                                "Tier 3 模型空闲超时 ({:?})，看门狗自动释放 Session 显存与内存",
                                timeout
                            );
                            guard.item = None;
                        }
                    }
                }
            })
            .ok();

        Self {
            timeout,
            state,
            loader: Arc::new(loader),
            stop_signal,
        }
    }

    /// 创建标准 Tier 3 默认看门狗（300 秒超时）
    pub fn new_tier3<F>(loader: F) -> Self
    where
        F: Fn() -> Option<T> + Send + Sync + 'static,
    {
        Self::new(Duration::from_secs(300), loader)
    }

    /// 获取或按需加载当前实例，并在原子锁保护下刷新最近访问时间戳
    pub fn get_or_load(&self) -> Option<Arc<T>> {
        // 快速读取路径：如果已经加载，快速获取只读锁返回，并刷新时间戳
        {
            let read = self.state.read().unwrap();
            if let Some(ref it) = read.item {
                let arc_item = Arc::clone(it);
                drop(read);
                // 刷新最近访问时间
                self.state.write().unwrap().last_accessed = Instant::now();
                return Some(arc_item);
            }
        }

        // 慢速加载路径：获取写锁，执行 loader 并原子写入 item 与当前时间戳
        let mut write = self.state.write().unwrap();
        if write.item.is_none() {
            if let Some(it) = (self.loader)() {
                write.item = Some(Arc::new(it));
                write.last_accessed = Instant::now();
            } else {
                return None;
            }
        } else {
            write.last_accessed = Instant::now();
        }
        write.item.clone()
    }

    /// 执行闭包操作并自动维持访问心跳
    pub fn with_item<R, F: FnOnce(&T) -> R>(&self, f: F) -> Option<R> {
        let it = self.get_or_load()?;
        Some(f(&it))
    }

    /// 兼容现有 Session 命名的方法别名
    pub fn with_session<R, F: FnOnce(&T) -> R>(&self, f: F) -> Option<R> {
        self.with_item(f)
    }

    /// 检查当前是否已加载在内存中
    pub fn is_loaded(&self) -> bool {
        self.state.read().unwrap().item.is_some()
    }

    /// 手动执行空闲驱逐检查（如超时则立即卸载并返回 true）
    pub fn evict_if_idle(&self) -> bool {
        let mut guard = self.state.write().unwrap();
        if guard.item.is_some() && guard.last_accessed.elapsed() >= self.timeout {
            guard.item = None;
            return true;
        }
        false
    }

    /// 测试辅助：手动重置最后访问时间
    #[doc(hidden)]
    pub fn set_last_accessed_for_test(&self, time: Instant) {
        self.state.write().unwrap().last_accessed = time;
    }

    /// 测试辅助：直接强制卸载
    #[doc(hidden)]
    pub fn unload_now(&self) {
        self.state.write().unwrap().item = None;
    }

    /// 测试辅助：手动直接设置内部已加载项
    #[doc(hidden)]
    pub fn set_item_for_test(&self, item: Option<Arc<T>>) {
        let mut guard = self.state.write().unwrap();
        guard.item = item;
        guard.last_accessed = Instant::now();
    }
}

impl<T> Drop for GenericWatchdog<T> {
    fn drop(&mut self) {
        self.stop_signal.store(true, Ordering::Relaxed);
    }
}

/// 专用于 ONNX Session 的看门狗类型别名
pub type WatchdogSession = GenericWatchdog<ort::session::Session>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_tier_timeout() {
        assert_eq!(ModelTier::Tier1.idle_timeout(), None);
        assert_eq!(ModelTier::Tier2.idle_timeout(), None);
        assert_eq!(ModelTier::Tier3.idle_timeout(), Some(Duration::from_secs(300)));
    }

    #[test]
    fn test_recommended_intra_threads() {
        let threads = OmniOrtRuntime::recommended_intra_threads();
        assert!(
            threads >= OmniOrtRuntime::MIN_INTRA_THREADS
                && threads <= OmniOrtRuntime::MAX_INTRA_THREADS,
            "intra_threads 必须落在 [{}, {}] 配额区间内，实际 {}",
            OmniOrtRuntime::MIN_INTRA_THREADS,
            OmniOrtRuntime::MAX_INTRA_THREADS,
            threads
        );
    }

    #[test]
    fn test_intra_threads_quota_scales_to_eight_cores() {
        // 8 核及以上机器必须吃满 8 线程配额（旧 clamp(1, 4) 会把这里锁死在 4）
        assert_eq!(OmniOrtRuntime::intra_threads_for_cores(8), 8);
        assert_eq!(OmniOrtRuntime::intra_threads_for_cores(16), 8);
        assert_eq!(OmniOrtRuntime::intra_threads_for_cores(64), 8);
        // 中间核数如实映射，不被上界一口吃平
        assert_eq!(OmniOrtRuntime::intra_threads_for_cores(6), 6);
        assert_eq!(OmniOrtRuntime::intra_threads_for_cores(4), 4);
        // 下限 2：单核/双核机器不退化到串行
        assert_eq!(OmniOrtRuntime::intra_threads_for_cores(2), 2);
        assert_eq!(OmniOrtRuntime::intra_threads_for_cores(1), 2);
        // ASR 侧维持历史 4 线程上限，不被 ORT 解绑波及
        assert!(OmniOrtRuntime::recommended_asr_threads() <= 4);
    }

    #[test]
    fn test_set_safe_dll_directory() {
        assert!(set_safe_dll_directory("resources/bin/onnx"));
    }

    #[test]
    fn test_watchdog_session_idle_release_and_reload() {
        use std::sync::atomic::AtomicUsize;
        let load_count = Arc::new(AtomicUsize::new(0));
        let count_clone = Arc::clone(&load_count);

        // 创建短超时看门狗（100毫秒）
        let watchdog = WatchdogSession::new(Duration::from_millis(100), move || {
            count_clone.fetch_add(1, Ordering::SeqCst);
            None
        });

        assert!(!watchdog.is_loaded());
        // 访问未加载且 loader 返回 None 时仍为 false
        assert!(watchdog.get_or_load().is_none());
        assert_eq!(load_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_generic_watchdog_full_load_evict_reload_cycle() {
        use std::sync::atomic::AtomicUsize;
        let load_count = Arc::new(AtomicUsize::new(0));
        let count_clone = Arc::clone(&load_count);

        // 使用 GenericWatchdog<String> 测试完整的 加载 -> 驱逐 -> 再加载 闭环
        let watchdog = GenericWatchdog::<String>::new(Duration::from_millis(100), move || {
            let current = count_clone.fetch_add(1, Ordering::SeqCst);
            Some(format!("model_v{}", current))
        });

        assert!(!watchdog.is_loaded());

        // 第一次访问：触发 loader，加载出 model_v0
        let item1 = watchdog.get_or_load().expect("should load item");
        assert_eq!(*item1, "model_v0");
        assert!(watchdog.is_loaded());
        assert_eq!(load_count.load(Ordering::SeqCst), 1);

        // 模拟超时：重置最后访问时间为 200ms 前
        watchdog.set_last_accessed_for_test(Instant::now() - Duration::from_millis(200));

        // 执行空闲驱逐
        let evicted = watchdog.evict_if_idle();
        assert!(evicted, "超时后应当成功驱逐");
        assert!(!watchdog.is_loaded(), "驱逐后应当处于未加载状态");

        // 再次访问：重新触发 loader，加载出 model_v1
        let item2 = watchdog.get_or_load().expect("should reload item");
        assert_eq!(*item2, "model_v1");
        assert!(watchdog.is_loaded());
        assert_eq!(load_count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn test_ep_two_stage_fallback_on_directml_failure() {
        use std::sync::atomic::AtomicBool;

        // 验证两阶段降级：当 DirectML / 硬件加速 EP 注入失败时，必须捕获错误并优雅降级为纯 CPU Session
        let stage1_called = Arc::new(AtomicBool::new(false));
        let stage2_called = Arc::new(AtomicBool::new(false));

        let s1 = Arc::clone(&stage1_called);
        let s2 = Arc::clone(&stage2_called);

        let result = OmniOrtRuntime::execute_two_stage_fallback(
            "test_directml_model.onnx",
            true, // 启用了硬件加速 EP
            move || {
                s1.store(true, Ordering::SeqCst);
                // 模拟 DirectML / GPU 初始化抛出错误
                Err("DirectMLExecutionProvider failed to allocate D3D12 device")
            },
            move || {
                s2.store(true, Ordering::SeqCst);
                // 阶段二：成功降级为纯 CPU Session
                Ok("cpu_session_instance")
            },
        );

        assert_eq!(result, Ok("cpu_session_instance"));
        assert!(stage1_called.load(Ordering::SeqCst), "必须首先尝试阶段一（硬件加速）");
        assert!(stage2_called.load(Ordering::SeqCst), "阶段一失败后必须显式降级执行阶段二（纯 CPU Session）");
    }

    #[test]
    fn test_tier3_watchdog_background_thread_release() {
        // 验证看门狗后台线程在超时后自动回收
        let watchdog = GenericWatchdog::<String>::new(Duration::from_millis(50), || {
            Some("watchdog_text_model".to_string())
        });

        // 加载实例
        assert!(watchdog.get_or_load().is_some());
        assert!(watchdog.is_loaded());

        // 模拟超时：重置最后访问时间为 2000ms 前
        watchdog.set_last_accessed_for_test(Instant::now() - Duration::from_millis(2000));

        // 等待后台巡检线程（1s 周期）执行释放
        std::thread::sleep(Duration::from_millis(1500));

        // 验证后台线程已自动释放内存
        assert!(!watchdog.is_loaded(), "空闲超时后后台巡检线程应自动将实例释放为 None");
    }

    /// 真实硬件 EP 可用性探针（诊断专用，--nocapture 运行）
    ///
    /// 背景：随包 onnxruntime.dll（1.28.2，17,799,168 B）实为**纯 CPU 构建**
    /// （资源包 `onnxruntime-directml` 的 packageUrl 误指向 sherpa-onnx CPU 包），
    /// 生产日志只见「成功构建纯 CPU Session」，EP 候选失败原因被 debug 级日志吞掉。
    /// 本探针在真实 DLL + 真实 Gemma 视觉模型上逐个 EP 构建会话，把每个 EP 的
    /// 可用性判定与真实构建错误完整打印出来，用于定位 GPU 静默回退 CPU 的根因。
    ///
    /// 实测补充结论（2026-10-21）：回退根因是 DLL 无 GPU EP，**不是**版本错配；
    /// 且视觉塔含 `GatherBlockQuantized`，ORT 1.20.0 无法加载该模型，
    /// 故不可为对齐 ort 期望版本而降级 DLL。
    #[test]
    fn test_probe_real_execution_provider_availability() {
        // 1. 定位随包 DLL（复用运行时发现链，落空时从测试进程位置向上探测桌面构建目录）
        let dll = OmniOrtRuntime::discover_onnx_dll().or_else(|| {
            let mut cur = std::env::current_dir().ok()?;
            for _ in 0..8 {
                let cand = cur.join("apps/desktop/build/extraResources/bin/omni/onnxruntime.dll");
                if cand.exists() {
                    return Some(cand);
                }
                cur = cur.parent()?.to_path_buf();
            }
            None
        });
        let Some(dll) = dll else {
            println!("[EP-PROBE] 未找到 onnxruntime.dll，跳过探针");
            return;
        };
        println!("[EP-PROBE] DLL: {}", dll.display());

        // 2. 定位真实 Gemma 视觉塔模型
        let mut model = None;
        let mut cur = std::env::current_dir().ok();
        let mut hops = 0;
        while let Some(dir) = cur {
            let cand = dir.join("apps/desktop/build/extraResources/models/gemma/vision_encoder_q4.onnx");
            if cand.exists() {
                model = Some(cand);
                break;
            }
            if hops > 8 {
                break;
            }
            cur = dir.parent().map(|p| p.to_path_buf());
            hops += 1;
        }
        let Some(model) = model else {
            println!("[EP-PROBE] 未找到 gemma vision_encoder_q4.onnx，跳过探针");
            return;
        };
        println!("[EP-PROBE] 模型: {}", model.display());

        // 3. 初始化动态库
        let init = OmniOrtRuntime::init_with_dll(dll.to_str().unwrap_or("onnxruntime.dll"));
        println!("[EP-PROBE] init: {:?}", init);

        // 4. 逐个 EP 探测可用性并真实构建会话
        use ort::execution_providers::ExecutionProvider;

        let cuda = ort::execution_providers::CUDAExecutionProvider::default();
        println!(
            "[EP-PROBE] CUDA is_available: {:?}",
            cuda.is_available()
        );
        let dml = ort::execution_providers::DirectMLExecutionProvider::default();
        println!(
            "[EP-PROBE] DirectML is_available: {:?}",
            dml.is_available()
        );

        let build_one = |name: &str, ep: ort::execution_providers::ExecutionProviderDispatch| {
            let attempt = (|| {
                let mut b = ort::session::Session::builder()?
                    .with_intra_threads(4)?
                    .with_optimization_level(
                        ort::session::builder::GraphOptimizationLevel::Level3,
                    )?;
                b = b.with_execution_providers([ep])?;
                b.commit_from_file(&model)
            })();
            match attempt {
                Ok(_) => println!("[EP-PROBE] {name}: 真实构建成功 ✓"),
                Err(e) => println!("[EP-PROBE] {name}: 构建失败 ✗ -> {e}"),
            }
        };

        build_one("CUDA", cuda.build());
        build_one("DirectML", dml.build());
        build_one("CPU(基线)", ort::execution_providers::CPUExecutionProvider::default().build());

        // 5. 走完整生产链路确认最终落点
        match OmniOrtRuntime::build_session(&model, ModelTier::Tier2) {
            Ok(_) => println!("[EP-PROBE] 生产 build_session 链路: 构建成功（EP 落点见上方逐项结果）"),
            Err(e) => println!("[EP-PROBE] 生产 build_session 链路: 失败 -> {e}"),
        }
    }

    /// 真实吞吐基准（默认跳过，需显式设置 `OMNI_EP_BENCH=1`）
    ///
    /// 为什么不能只看探针：Gemma 视觉塔含 `com.microsoft` 自定义量化算子
    /// （实测 `GatherBlockQuantized` 存在），DirectML 不支持的算子会被 ORT 按节点切分
    /// 回落到 CPU，因此「EP 可用 + Session 构建成功」并不等于推理真跑在 GPU 上。
    /// 本基准用同一真实模型、同一输入形状分别在 CPU 与 DirectML 会话上重复前向，
    /// 用吞吐差值给出可证伪的结论。
    #[test]
    fn test_bench_vision_encoder_ep_throughput() {
        if std::env::var("OMNI_EP_BENCH").is_err() {
            println!("[EP-BENCH] 未设置 OMNI_EP_BENCH=1，跳过真实吞吐基准");
            return;
        }
        use ort::execution_providers::ExecutionProvider;
        use std::time::Instant;

        let dll = match OmniOrtRuntime::discover_onnx_dll() {
            Some(d) => d,
            None => {
                println!("[EP-BENCH] 未找到 onnxruntime.dll，跳过");
                return;
            }
        };
        println!("[EP-BENCH] DLL: {}", dll.display());
        let _ = OmniOrtRuntime::init_with_dll(dll.to_str().unwrap_or("onnxruntime.dll"));

        // 允许用 OMNI_BENCH_VISION 切换被测视觉模型（如未量化的 vision_model.onnx），
        // 用于区分「DML 不支持 Q4 量化算子」与「DML/驱动本身不可用」两种失败
        let vision_file = std::env::var("OMNI_BENCH_VISION")
            .unwrap_or_else(|_| "vision_encoder_q4.onnx".to_string());
        let model = match locate_gemma_sibling(&vision_file) {
            Some(m) => m,
            None => {
                println!("[EP-BENCH] 未找到 gemma {vision_file}，跳过");
                return;
            }
        };
        println!("[EP-BENCH] 模型: {}", model.display());

        // 真实形状：pixel_values [1, 140, 768] f32 + pixel_position_ids [1, 140, 2] i64
        // 140 = 官方 video_processor.max_soft_tokens
        const SEQ: usize = 140;
        const FEAT: usize = 768;
        const ITERS: u32 = 15;

        /// 构建会话：`ep=None` 走纯 CPU 基线
        fn build(
            path: &std::path::Path,
            ep: Option<ort::execution_providers::ExecutionProviderDispatch>,
        ) -> Result<ort::session::Session, ort::Error> {
            let mut b = ort::session::Session::builder()?
                .with_intra_threads(OmniOrtRuntime::recommended_intra_threads())?
                .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)?;
            if let Some(ep) = ep {
                b = b.with_execution_providers([ep])?;
            }
            b.commit_from_file(path)
        }

        /// 预热后计时 ITERS 次前向，输出单次耗时与 107 帧外推
        fn bench<F>(label: &str, session: &mut ort::session::Session, feed: &F)
        where
            F: Fn(&mut ort::session::Session) -> Result<(), ort::Error>,
        {
            for _ in 0..3 {
                if let Err(e) = feed(session) {
                    println!("[EP-BENCH] {label}: 前向失败 -> {e}");
                    return;
                }
            }
            let t0 = Instant::now();
            for _ in 0..ITERS {
                if let Err(e) = feed(session) {
                    println!("[EP-BENCH] {label}: 计时循环中失败 -> {e}");
                    return;
                }
            }
            let per = t0.elapsed() / ITERS;
            println!(
                "[EP-BENCH] {label}: 单次前向 {:?}，107 帧外推 ≈ {:?}",
                per,
                per * 107
            );
        }

        let feed_vision = |session: &mut ort::session::Session| -> Result<(), ort::Error> {
            let pv = ort::value::Tensor::from_array((
                vec![1usize, SEQ, FEAT],
                vec![0.01f32; SEQ * FEAT],
            ))?;
            let pid = ort::value::Tensor::from_array((
                vec![1usize, SEQ, 2usize],
                vec![0i64; SEQ * 2],
            ))?;
            let inputs: [ort::session::SessionInputValue<'static>; 2] = [pv.into(), pid.into()];
            session.run(inputs).map(|_| ())
        };

        match build(&model, None) {
            Ok(mut s) => {
                bench("视觉塔 CPU 基线", &mut s, &feed_vision);
            }
            Err(e) => println!("[EP-BENCH] 视觉塔 CPU 会话构建失败 -> {e}"),
        }

        // 注：`build(self)` 会消费 provider 实例，故可用性先行判定、每个分支各自新建实例
        let dml_available = ort::execution_providers::DirectMLExecutionProvider::default()
            .is_available()
            .unwrap_or(false);
        let dml_dispatch =
            || ort::execution_providers::DirectMLExecutionProvider::default().build();
        if dml_available {
            match build(&model, Some(dml_dispatch())) {
                Ok(mut s) => {
                    bench("视觉塔 DirectML", &mut s, &feed_vision);
                }
                Err(e) => println!("[EP-BENCH] 视觉塔 DirectML 会话构建失败 -> {e}"),
            }
        } else {
            println!("[EP-BENCH] DirectML 不可用，跳过（当前 DLL 无 DML EP）");
        }

        // 文本塔：生产链路里每帧除视觉前向还要过一次文本塔，
        // 实测 6:30/107 帧 ≈ 3.6s/帧 远大于视觉塔耗时，故必须单独测它才能定位真正大头。
        let Some(text_model) = locate_gemma_sibling("model_q4.onnx") else {
            println!("[EP-BENCH] 未找到 gemma model_q4.onnx，跳过文本塔基准");
            return;
        };
        // 生产 ids: [bos, soft_token_marker, soft×n, eoi, eos] → n=140 时 seq=143
        const TEXT_SEQ: usize = 143;
        let feed_text = |session: &mut ort::session::Session| -> Result<(), ort::Error> {
            let ids = ort::value::Tensor::from_array((
                vec![1usize, TEXT_SEQ],
                vec![2i64; TEXT_SEQ],
            ))?;
            let mask = ort::value::Tensor::from_array((
                vec![1usize, TEXT_SEQ],
                vec![1i64; TEXT_SEQ],
            ))?;
            let img = ort::value::Tensor::from_array((
                vec![SEQ, 512usize],
                vec![0.01f32; SEQ * 512],
            ))?;
            // 注：ORT 不允许 0 维张量（"all dimensions must be >= 1"），
            // 故未使用的模态各给 1 个 token，量级可忽略
            let vid = ort::value::Tensor::from_array((vec![1usize, 512usize], vec![0.0f32; 512]))?;
            let aud = ort::value::Tensor::from_array((vec![1usize, 512usize], vec![0.0f32; 512]))?;
            let inputs: [ort::session::SessionInputValue<'static>; 5] =
                [ids.into(), mask.into(), img.into(), vid.into(), aud.into()];
            session.run(inputs).map(|_| ())
        };

        match build(&text_model, None) {
            Ok(mut s) => {
                bench("文本塔 CPU 基线", &mut s, &feed_text);
            }
            Err(e) => println!("[EP-BENCH] 文本塔 CPU 会话构建失败 -> {e}"),
        }
        if dml_available {
            match build(&text_model, Some(dml_dispatch())) {
                Ok(mut s) => {
                    bench("文本塔 DirectML", &mut s, &feed_text);
                }
                Err(e) => println!("[EP-BENCH] 文本塔 DirectML 会话构建失败 -> {e}"),
            }
        }
    }

    /// 向上定位 Gemma 目录下的指定模型文件（探针与基准共用）
    fn locate_gemma_sibling(file: &str) -> Option<std::path::PathBuf> {
        let mut cur = std::env::current_dir().ok()?;
        for _ in 0..8 {
            let cand = cur
                .join("apps/desktop/build/extraResources/models/gemma")
                .join(file);
            if cand.exists() {
                return Some(cand);
            }
            cur = cur.parent()?.to_path_buf();
        }
        None
    }
}
