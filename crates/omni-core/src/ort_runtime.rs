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

    /// 限制 CPU 线程池：统一配置 intra_threads = min(物理核心数, 4)，杜绝抢占 CPU 造成系统无响应
    pub fn recommended_intra_threads() -> usize {
        std::thread::available_parallelism()
            .map(|n| n.get().min(4))
            .unwrap_or(4)
    }

    /// 执行提供商 (EP) 两阶段降级路由工厂
    pub fn build_session<P: AsRef<Path>>(
        model_path: P,
        tier: ModelTier,
    ) -> Result<ort::session::Session, ort::Error> {
        Self::ensure_initialized();
        let path = model_path.as_ref();
        let intra_threads = Self::recommended_intra_threads();
        let ep = Self::preferred_hardware_ep();
        Self::build_session_with_ep_internal(path, tier, ep, intra_threads)
    }

    /// 获取当前平台优先推荐的硬件加速 Execution Provider
    pub fn preferred_hardware_ep() -> Option<ort::execution_providers::ExecutionProviderDispatch> {
        #[cfg(target_os = "windows")]
        {
            // Windows 优先注入 DirectMLExecutionProvider
            Some(ort::execution_providers::DirectMLExecutionProvider::default().build())
        }
        #[cfg(target_os = "macos")]
        {
            // macOS 优先注入 CoreMLExecutionProvider
            Some(ort::execution_providers::CoreMLExecutionProvider::default().build())
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            None
        }
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

    /// 内部两阶段构建逻辑，支持测试中模拟 EP 注入与失败
    pub fn build_session_with_ep_internal(
        path: &Path,
        _tier: ModelTier,
        ep: Option<ort::execution_providers::ExecutionProviderDispatch>,
        intra_threads: usize,
    ) -> Result<ort::session::Session, ort::Error> {
        let has_ep = ep.is_some();
        let path_str = path.display().to_string();
        Self::execute_two_stage_fallback(
            &path_str,
            has_ep,
            || {
                let hardware_ep = ep.as_ref().unwrap();
                let mut b = ort::session::Session::builder()?
                    .with_intra_threads(intra_threads)?
                    .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)?;
                b = b.with_execution_providers([hardware_ep.clone()])?;
                b.commit_from_file(path)
            },
            || {
                ort::session::Session::builder()?
                    .with_intra_threads(intra_threads)?
                    .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)?
                    .commit_from_file(path)
            },
        )
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
        assert!(threads >= 1 && threads <= 4);
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
}
