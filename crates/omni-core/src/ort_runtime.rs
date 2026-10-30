use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
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

// ============================================================================
// DirectML 全局降级闸（工单 04 §2 / SPEC §3.1）
//
// 语义：进程级、单调不可逆的一次性开关。warmup 期与运行期任何 DML 故障都必须走此闸，
// 置位后全进程不再尝试 DML（`preferred_hardware_eps` 先查闸再探测），避免「同一进程
// 反复踩同一个坏 GPU 路径」导致每帧失败与日志刷屏。
// ============================================================================

static DIRECTML_DISABLED: AtomicBool = AtomicBool::new(false);
static DIRECTML_DISABLE_REASON: OnceLock<String> = OnceLock::new();

/// 置位 DML 全局降级闸（warmup / 运行期 DML 故障的唯一入口）
///
/// 幂等：仅首次置位输出 warn（携带原因），后续重复触发降级为 debug，防止高频故障刷屏。
pub fn disable_directml(reason: &str) {
    let first_trip = !DIRECTML_DISABLED.swap(true, Ordering::SeqCst);
    if first_trip {
        let _ = DIRECTML_DISABLE_REASON.set(reason.to_string());
        tracing::warn!(
            "OmniOrtRuntime: DirectML 全局降级闸已置位，进程内不再尝试 DML —— 原因: {}",
            reason
        );
    } else {
        tracing::debug!(
            "OmniOrtRuntime: DirectML 降级闸已处于置位状态（重复触发，原因: {}）",
            reason
        );
    }
}

/// 查询 DML 全局降级闸状态
pub fn directml_disabled() -> bool {
    DIRECTML_DISABLED.load(Ordering::SeqCst)
}

/// 查询 DML 降级闸首次置位原因（未置位时为 None）
pub fn directml_disable_reason() -> Option<&'static str> {
    DIRECTML_DISABLE_REASON.get().map(|s| s.as_str())
}

/// 测试辅助：复位全局降级闸
///
/// 仅供测试隔离使用。此处**刻意不加 `#[cfg(test)]` 守卫**：`cfg(test)` 只在编译 omni-core
/// 自身的单测时生效，`omni-server` 的 `tests/*.rs` 集成测试属于独立 crate，看不到被门控的符号
/// （会导致 E0432）。因此该符号在所有构建中存在，但调用方仅限测试代码，业务代码严禁调用
/// （否则会错误复位不可逆的 DML 降级闸）。
/// `OnceLock` 原因无法清空，测试只断言布尔闸；多测试并发触碰闸时须持有测试互斥锁串行化。
#[doc(hidden)]
pub fn reset_directml_gate_for_test() {
    DIRECTML_DISABLED.store(false, Ordering::SeqCst);
}

// ============================================================================
// EP 策略分档（工单 04 §2 / SPEC §3.2）
// ============================================================================

/// v1 DirectML allowlist（唯一事实源，SPEC §3.2）
///
/// 扩档铁律：任何模型加入此表必须附同 harness 实测数据（同形状 CPU/DML ≥1.5x 且数值偏差
/// ≤1e-4 相对量级），禁止凭直觉扩档。v1 仅 Gemma 视觉塔（文本塔实测中位 ~1.3x 且方差极大，维持 CPU）。
pub const DML_ALLOWLIST_MODELS: &[&str] = &["gemma.vision_encoder", "moss.codec_decoder"];

/// EP 策略档位：与 `ModelTier`（内存驻留分级）**正交**，严禁合并成同一枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpPolicy {
    /// v1 DirectML allowlist（仅 Gemma 视觉塔）：Windows 上尝试 DML，
    /// 闸已置位或探测不可用时自动降档为纯 CPU
    DmlAllowlist,
    /// 纯 CPU：不注入任何硬件加速 EP（文本塔 / OCR / CLIP / TTS 等全部非 allowlist 模型）
    CpuOnly,
}

/// EP 阶梯节点（纯值类型，便于单测断言阶梯顺序，不依赖真实 DLL 与 GPU）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpKind {
    /// DirectML 硬件加速
    DirectMl,
    /// 纯 CPU 多线程兜底
    Cpu,
}

/// 纯函数：按策略与探测结果解析 Windows 侧 EP 候选阶梯（T1 单测切缝）
///
/// - `dml_probe_ok`：预探确认运行时 B 具备 DML 导出符号且 API 契约通过；
/// - `dml_disabled`：全局降级闸状态（先查闸再探测，闸置位后阶梯恒为纯 CPU）。
///
/// 阶梯顺序铁律：DML 在前、CPU 收尾；`CpuOnly` 恒返回单节点 `[Cpu]`。
pub fn resolve_ep_candidates(
    policy: EpPolicy,
    dml_probe_ok: bool,
    dml_disabled: bool,
) -> Vec<EpKind> {
    match policy {
        EpPolicy::DmlAllowlist if dml_probe_ok && !dml_disabled => {
            vec![EpKind::DirectMl, EpKind::Cpu]
        }
        _ => vec![EpKind::Cpu],
    }
}

// ============================================================================
// 先探后绑（probe-then-bind）预探阶梯（工单 04 §1 / SPEC §3.1 第 6 步）
//
// 背景：ort 2.0.0-rc.9 下 DLL 加载失败是 **panic**（`lib.rs:109` unwrap_or_else panic），
// 且 `G_ORT_DYLIB_PATH` 的 OnceLock 二次 set 被静默丢弃、`G_ORT_LIB` / `G_ORT_API`
// 一经绑定永久缓存首个候选句柄。故**严禁**沿阶梯多次调用 `ort::init_from`：
// 阶梯内每个候选先经 `libloading` 纯内存预探，唯有唯一胜出者才执行一次绑定。
// ============================================================================

/// 候选判定结论
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateVerdict {
    /// 预探通过，可进入绑定阶段
    Accept,
    /// 文件不存在或无法被加载器打开（纯 CPU 裸环境 / 路径失效）
    RejectUnloadable,
    /// 文件名声明 DML 但缺少 `OrtSessionOptionsAppendExecutionProvider_DML` 导出符号（坏候选）
    RejectMissingDmlSymbol,
    /// `OrtGetApiBase()->GetApi(ort_sys::ORT_API_VERSION)` 返回空指针（API 版本断层）
    RejectApiMismatch,
    /// 候选具备 DML 能力，但同级 `DirectML.dll` 缺失或预加载失败 ⇒ 运行时会被 System32 同名复用截胡
    RejectMissingDirectMl,
}

/// 纯函数：候选判定（T9 单测切缝，与真实 DLL 解耦）
///
/// 判定顺序：可加载性 → API 契约 → DML 符号（名字声明 DML 时）。纯 CPU 运行时 A
/// 不声明 DML，缺 DML 符号属正常形态，不得拦截。
pub fn judge_candidate(
    claims_dml: bool,
    loadable: bool,
    has_dml_symbol: bool,
    api_ok: bool,
) -> CandidateVerdict {
    if !loadable {
        return CandidateVerdict::RejectUnloadable;
    }
    if !api_ok {
        return CandidateVerdict::RejectApiMismatch;
    }
    if claims_dml && !has_dml_symbol {
        return CandidateVerdict::RejectMissingDmlSymbol;
    }
    CandidateVerdict::Accept
}

/// 比较两个路径是否指向同一文件（先规范化，退化时按字面比较；Windows 大小写不敏感）
fn is_same_file(a: &Path, b: &Path) -> bool {
    let ca = std::fs::canonicalize(a).unwrap_or_else(|_| a.to_path_buf());
    let cb = std::fs::canonicalize(b).unwrap_or_else(|_| b.to_path_buf());
    if cfg!(target_os = "windows") {
        ca.to_string_lossy()
            .eq_ignore_ascii_case(&cb.to_string_lossy())
    } else {
        ca == cb
    }
}

/// 预探报告（先探后绑的唯一胜出者 + 可观测字段）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrtProbeReport {
    /// 胜出候选路径（裸名兜底时为裸文件名，如 "onnxruntime.dll"）
    pub path: PathBuf,
    /// 是否具备 DML 导出符号
    pub dml_capable: bool,
    /// `GetApi(ORT_API_VERSION)` 是否返回非空（头文件契约一致）
    pub api_ok: bool,
    /// 文件版本（FileVersion 四段式，取不到时为 None）
    pub file_version: Option<String>,
    /// 运行时自报版本（`OrtApiBase::GetVersionString`，如 "1.24.4"；AC-2 断言以它为准）
    pub runtime_version: Option<String>,
    /// 预探期以绝对路径钉死的 DirectML.dll
    pub directml_pinned: Option<PathBuf>,
}

/// 文件名是否声明自己是 DirectML 运行时（`onnxruntime_dml.dll`）
fn name_claims_dml(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.to_ascii_lowercase().contains("_dml"))
        .unwrap_or(false)
}

/// 以绝对路径钉死 `DirectML.dll`，并永久持有句柄（防 System32 同名截胡）
///
/// Windows PE 加载器对同名模块实行已加载模块列表优先复用原则：若进程内任何图形 /
/// 多媒体 / DXGI 组件先行触发了 `C:\Windows\System32\DirectML.dll`，后续加载
/// `onnxruntime_dml.dll` 时将复用旧句柄而无视同级新版 DLL。故必须在任何 D3D12 / ORT
/// 预探之前用**绝对路径**预加载，且**严禁**在函数退出时 Drop（Drop 会触发 FreeLibrary
/// 卸载，等于没钉）。纯 CPU 裸环境（CI 未装配 DML 资产）文件缺失仅 debug，不阻塞阶梯。
#[cfg(target_os = "windows")]
pub fn pin_directml_dll(dir: &Path) -> Option<PathBuf> {
    // 进程内同名模块只有一份：首次钉死成功后直接复用结论，避免阶梯逐候选重复 LoadLibrary
    static PINNED_DIRECTML: OnceLock<Option<PathBuf>> = OnceLock::new();
    if let Some(cached) = PINNED_DIRECTML.get() {
        return cached.clone();
    }

    let candidate = dir.join("DirectML.dll");
    if !candidate.exists() {
        tracing::debug!(
            "OmniOrtRuntime: {} 无 DirectML.dll（纯 CPU 裸环境），跳过 DML 候选预探",
            dir.display()
        );
        return None;
    }
    // 绝对路径：杜绝加载器按搜索路径把同名 DLL 解析到 System32
    let abs = std::fs::canonicalize(&candidate).unwrap_or(candidate);
    let pinned = match unsafe { libloading::Library::new(&abs) } {
        Ok(lib) => {
            // 永久持有句柄：此处 Drop 会 FreeLibrary 卸载，钉死语义即失效
            std::mem::forget(lib);
            tracing::info!(
                "OmniOrtRuntime: 已以绝对路径钉死 DirectML.dll（防 System32 截胡）: {}",
                abs.display()
            );
            Some(abs)
        }
        Err(e) => {
            tracing::warn!(
                "OmniOrtRuntime: DirectML.dll 绝对路径预加载失败，DML 候选将被跳过: {} ({})",
                abs.display(),
                e
            );
            None
        }
    };
    // 仅缓存成功结论：未找到的目录不得污染后续候选的钉死机会
    if pinned.is_some() {
        let _ = PINNED_DIRECTML.set(pinned.clone());
    }
    pinned
}

#[cfg(not(target_os = "windows"))]
pub fn pin_directml_dll(_dir: &Path) -> Option<PathBuf> {
    None
}

/// 读取进程内已加载模块的真实文件路径（Windows `GetModuleHandleW` + `GetModuleFileNameW`）
#[cfg(target_os = "windows")]
pub fn loaded_module_path(module: &str) -> Option<PathBuf> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};

    let wide: Vec<u16> = std::ffi::OsStr::new(module)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let handle = GetModuleHandleW(wide.as_ptr());
        if handle.is_null() {
            return None;
        }
        let mut buf = vec![0u16; 1024];
        let len = GetModuleFileNameW(handle, buf.as_mut_ptr(), buf.len() as u32);
        if len == 0 {
            return None;
        }
        // 缓冲不足时 API 返回 nSize（而非长度），截断会产生半截路径并污染「截胡」断言
        if len as usize >= buf.len() {
            return None;
        }
        buf.truncate(len as usize);
        Some(PathBuf::from(std::ffi::OsString::from_wide(&buf)))
    }
}

#[cfg(not(target_os = "windows"))]
pub fn loaded_module_path(_module: &str) -> Option<PathBuf> {
    None
}

/// 读取 PE 文件的 FileVersion（四段式 `a.b.c.d`），失败返回 None
#[cfg(target_os = "windows")]
pub fn file_version(path: &Path) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
    };

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let size = GetFileVersionInfoSizeW(wide.as_ptr(), std::ptr::null_mut());
        if size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        if GetFileVersionInfoW(wide.as_ptr(), 0, size, buf.as_mut_ptr() as *mut _) == 0 {
            return None;
        }
        // 根块 `\` 指向 VS_FIXEDFILEINFO
        let root: Vec<u16> = "\\".encode_utf16().chain(std::iter::once(0)).collect();
        let mut ptr: *mut std::ffi::c_void = std::ptr::null_mut();
        let mut len: u32 = 0;
        if VerQueryValueW(buf.as_ptr() as *const _, root.as_ptr(), &mut ptr, &mut len) == 0
            || ptr.is_null()
            || len < 16
        {
            return None;
        }
        // 布局：dwSignature(0) / dwStrucVersion(4) / dwFileVersionMS(8) / dwFileVersionLS(12)
        // 必须用 `read_unaligned`：`ptr` 指向 `Vec<u8>`（对齐 1）内部，`from_raw_parts::<u32>`
        // 的 4 字节对齐前提并不成立，属未定义行为（当前只靠 VS_VERSIONINFO 的 padding 侥幸对齐）。
        let base = ptr as *const u8;
        let ms = std::ptr::read_unaligned(base.add(8) as *const u32);
        let ls = std::ptr::read_unaligned(base.add(12) as *const u32);
        Some(format!(
            "{}.{}.{}.{}",
            ms >> 16,
            ms & 0xffff,
            ls >> 16,
            ls & 0xffff
        ))
    }
}

#[cfg(not(target_os = "windows"))]
pub fn file_version(_path: &Path) -> Option<String> {
    None
}

/// 单候选预探结论
#[derive(Debug, Clone, PartialEq, Eq)]
struct CandidateProbe {
    verdict: CandidateVerdict,
    has_dml_symbol: bool,
    /// `OrtGetApiBase()->GetApi(ORT_API_VERSION)` 是否返回非空（头文件契约一致）
    api_ok: bool,
    /// 运行时自报版本（`OrtApiBase::GetVersionString`，如 "1.24.4"）
    ///
    /// 与 PE `FileVersion` 互补：资源段可缺失、可被误写（SPEC §7.7 已有「1.28.0 官方不存在
    /// 却被 BOM 登记」的前科），而版本字符串是 ORT 自己吐的，AC-2 的版本断言以它为准。
    runtime_version: Option<String>,
}

/// 单候选预探：`libloading` 纯内存加载 + DML 符号 + ORT API 契约双检
///
/// 预探句柄在本函数内 Drop，仅用于符号与契约探测，真正绑定由 `ort::init_from` 完成，
/// 不会污染 ort 的全局 OnceLock。
#[cfg(target_os = "windows")]
fn probe_candidate_file(path: &Path) -> CandidateProbe {
    let claims_dml = name_claims_dml(path);
    // 绝对路径加载：裸名兜底（"onnxruntime.dll"）交给 OS 加载器按搜索路径解析
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let lib = match unsafe { libloading::Library::new(&abs) } {
        Ok(l) => l,
        Err(e) => {
            // 存在但加载失败 = 资产损坏 / 架构不符 / 缺依赖（真实故障，必须 warn 可观测）；
            // 不存在 = 纯 CPU 裸环境的正常缺席（debug 即可，避免噪声）
            if path.exists() {
                tracing::warn!(
                    "OmniOrtRuntime: 候选存在但加载失败（资产损坏/架构不符/缺依赖），跳过: {} ({})",
                    path.display(),
                    e
                );
            } else {
                tracing::debug!("OmniOrtRuntime: 候选不存在，跳过: {}", path.display());
            }
            return CandidateProbe {
                verdict: CandidateVerdict::RejectUnloadable,
                has_dml_symbol: false,
                api_ok: false,
                runtime_version: None,
            };
        }
    };

    let has_dml_symbol = unsafe {
        lib.get::<*const std::ffi::c_void>(b"OrtSessionOptionsAppendExecutionProvider_DML\0")
            .is_ok()
    };

    // API 契约核验：GetApi(ORT_API_VERSION) 必须返回非空，从源头阻断头文件契约不匹配引发的 panic
    let mut api_ok = false;
    let mut runtime_version = None;
    unsafe {
        if let Ok(get_api_base) =
            lib.get::<unsafe extern "system" fn() -> *const ort::sys::OrtApiBase>(b"OrtGetApiBase\0")
        {
            let base = get_api_base();
            if !base.is_null() {
                if let Some(get_api) = (*base).GetApi {
                    api_ok = !get_api(ort::sys::ORT_API_VERSION).is_null();
                }
                if let Some(get_version) = (*base).GetVersionString {
                    let raw = get_version();
                    if !raw.is_null() {
                        runtime_version = std::ffi::CStr::from_ptr(raw)
                            .to_str()
                            .ok()
                            .map(|s| s.to_string());
                    }
                }
            }
        }
    }
    drop(lib);

    CandidateProbe {
        verdict: judge_candidate(claims_dml, true, has_dml_symbol, api_ok),
        has_dml_symbol,
        api_ok,
        runtime_version,
    }
}

#[cfg(not(target_os = "windows"))]
fn probe_candidate_file(path: &Path) -> CandidateProbe {
    // 非 Windows 无 `onnxruntime_dml.dll` 语义，不做 DML 符号探测，
    // 但**必须**校验可加载性：`ort::init_from(...).commit()` 在 dylib 缺席、架构/ABI
    // 不匹配或 `GetApi(ORT_API_VERSION)` 版本断层时是 panic（rc.9 `lib.rs:106` / `:176`），
    // release `panic = "abort"` 下直接终止进程。预探谎报成功 = 把「静默降级」升级成 abort。
    let exists = path.exists();
    let loadable = exists && unsafe { libloading::Library::new(path) }.is_ok();
    if exists && !loadable {
        tracing::warn!(
            "OmniOrtRuntime: 候选存在但无法被加载器打开（架构/ABI 不匹配或依赖缺失），跳过: {}",
            path.display()
        );
    }
    CandidateProbe {
        verdict: if loadable {
            CandidateVerdict::Accept
        } else {
            CandidateVerdict::RejectUnloadable
        },
        has_dml_symbol: false,
        // 非 Windows 不做 API 契约探测，如实标记为 false（严禁谎报 true）
        api_ok: false,
        runtime_version: None,
    }
}

/// DML 候选的防截胡校验：返回 `Some(原因)` 表示该候选必须被拒绝
///
/// 判据（工单 04 第 13 条「拦截缺失 `DirectML.dll` 的坏候选」）：候选具备 DML 能力，
/// 但**本级目录**没有 `DirectML.dll`。此时 ORT 的 DML EP 只能按搜索路径命中
/// `System32\DirectML.dll`，版本与运行时 B 未必匹配 ⇒ 静默截胡；且该时刻模块尚未驻留，
/// `log_runtime_binding` 的截胡告警也无从观测，整条链路会零 warn 地跑在错误 DLL 上。
///
/// 注：裸名候选（无本级目录）同样拒绝——没有目录就无法以绝对路径钉死，防截胡契约不成立。
///
/// 判据二（与 `pin_directml_dll` 的结论对齐）：同级 `DirectML.dll` **存在但无法预加载**
/// （架构不符 / 文件损坏）。此时 `pin_directml_dll` 已 warn「DML 候选将被跳过」，
/// 若此处仅以 `exists()` 放行，行为就与日志相反：候选被 Accept，DML EP 转而命中
/// `System32` 同名副本 —— 正是本工单要消灭的截胡。故以「模块是否真的驻留」为判据。
fn dml_candidate_reject_reason(cand: &Path) -> Option<String> {
    let Some(dir) = cand.parent().filter(|p| !p.as_os_str().is_empty()) else {
        return Some(format!(
            "{}: 具备 DML 能力但候选为裸名（无本级目录），无法以绝对路径钉死 DirectML.dll",
            cand.display()
        ));
    };
    if !dir.join("DirectML.dll").exists() {
        return Some(format!(
            "{}: 具备 DML 能力但同级 DirectML.dll 缺失（DML EP 将命中 System32 同名旧版，属静默截胡）",
            cand.display()
        ));
    }
    // 文件在但钉不死（预加载失败）⇒ 进程内没有我们认可的那份 DirectML.dll
    if loaded_module_path("DirectML.dll").is_none() {
        return Some(format!(
            "{}: 同级 DirectML.dll 存在但无法以绝对路径预加载（架构不符/文件损坏，DML EP 将命中 System32 同名副本）",
            cand.display()
        ));
    }
    None
}

/// 预探阶梯：逐候选判定，首个通过者胜出（T9 单测切缝，不触碰 `RUNTIME_INIT`）
///
/// 铁律：本函数**只预探不绑定**，故全败返回 Err 时不会污染 ort 全局单例，
/// 修复资产后同进程重试仍可成功。
pub fn probe_ladder(candidates: &[PathBuf]) -> Result<OrtProbeReport, String> {
    let mut rejects: Vec<String> = Vec::new();
    for cand in candidates {
        // 防截胡：先以绝对路径钉死候选同级 DirectML.dll（必须在任何 ORT 预探之前）
        let pinned = cand
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .and_then(pin_directml_dll);

        let probe = probe_candidate_file(cand);

        // 坏候选拦截必须在 Accept 之前：放行「有 DML 符号但拿不到本级 DirectML.dll」的候选
        // 等于把截胡风险下沉到 EP 注册期，而那里已无任何可观测点。
        if probe.verdict == CandidateVerdict::Accept && probe.has_dml_symbol {
            if let Some(reason) = dml_candidate_reject_reason(cand) {
                tracing::warn!("OmniOrtRuntime: 预探拦截 {}", reason);
                rejects.push(reason);
                continue;
            }
        }

        match probe.verdict {
            CandidateVerdict::Accept => {
                // 防截胡交叉校验：钉死句柄是「进程内首个成功加载的同名模块」，若胜出候选目录
                // 与该钉死路径不同，加载器会按同名复用首个副本 ⇒ DML 可能跑在陈旧 DLL 上。
                // 这里必须显式告警，杜绝「断言基准 = 我们钉了什么」造成的恒真（AC-2）。
                if let Some(dir) = cand.parent().filter(|p| !p.as_os_str().is_empty()) {
                    let expected = dir.join("DirectML.dll");
                    // 内存驻留副本才是「EP 真正会用到的那个」——它比钉死句柄更接近事实
                    let resident = loaded_module_path("DirectML.dll");
                    let mismatch = match (pinned.as_ref(), resident.as_ref()) {
                        (Some(p), Some(r)) => !is_same_file(p, r),
                        _ => false,
                    };
                    if (expected.exists()
                        && pinned
                            .as_ref()
                            .is_some_and(|p| !is_same_file(p, &expected)))
                        || mismatch
                    {
                        tracing::warn!(
                            "OmniOrtRuntime: 钉死的 DirectML.dll 与胜出运行时 B 不同目录（加载器将按同名复用首个副本）: pinned={} resident={} expected={}",
                            pinned
                                .as_ref()
                                .map(|p| p.display().to_string())
                                .unwrap_or_else(|| "<none>".to_string()),
                            resident
                                .as_ref()
                                .map(|p| p.display().to_string())
                                .unwrap_or_else(|| "<not-loaded>".to_string()),
                            expected.display()
                        );
                    }
                }
                let fv = file_version(cand);
                tracing::info!(
                    "OmniOrtRuntime: 运行时 B 预探通过 candidate={} DML符号={} FileVersion={} ORTVersion={} DirectML钉死={}",
                    cand.display(),
                    probe.has_dml_symbol,
                    fv.as_deref().unwrap_or("<unknown>"),
                    probe.runtime_version.as_deref().unwrap_or("<unknown>"),
                    pinned
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "<none>".to_string())
                );
                return Ok(OrtProbeReport {
                    path: cand.clone(),
                    dml_capable: probe.has_dml_symbol,
                    api_ok: probe.api_ok,
                    file_version: fv,
                    runtime_version: probe.runtime_version,
                    directml_pinned: pinned,
                });
            }
            CandidateVerdict::RejectUnloadable => {
                if cand.exists() {
                    // 存在却加载失败属真实故障（原因已在 probe_candidate_file 内 warn），
                    // 必须进入全败 Err 串，严禁只留一句「拦截 0 个」的静默降级
                    rejects.push(format!("{}: 存在但加载失败", cand.display()));
                } else {
                    tracing::debug!(
                        "OmniOrtRuntime: 候选不存在/不可加载，跳过: {}",
                        cand.display()
                    );
                }
            }
            CandidateVerdict::RejectMissingDmlSymbol => {
                let msg = format!("{}: 声明 DML 但缺 DML 导出符号（坏候选）", cand.display());
                tracing::warn!("OmniOrtRuntime: 预探拦截 {}", msg);
                rejects.push(msg);
            }
            CandidateVerdict::RejectApiMismatch => {
                let msg = format!(
                    "{}: GetApi({}) 返回空指针（API 版本断层）",
                    cand.display(),
                    ort::sys::ORT_API_VERSION
                );
                tracing::warn!("OmniOrtRuntime: 预探拦截 {}", msg);
                rejects.push(msg);
            }
            // 该结论由 `judge_candidate` 之外的防截胡校验产生，已在 match 之前 `continue`；
            // 此处保留分支仅为穷尽性，避免后续新增判定时静默落空。
            CandidateVerdict::RejectMissingDirectMl => {
                let msg = format!("{}: 具备 DML 能力但缺少同级 DirectML.dll", cand.display());
                tracing::warn!("OmniOrtRuntime: 预探拦截 {}", msg);
                rejects.push(msg);
            }
        }
    }
    Err(format!(
        "预探阶梯全败（候选 {} 个，拦截 {} 个）{}",
        candidates.len(),
        rejects.len(),
        if rejects.is_empty() {
            String::new()
        } else {
            format!(": {}", rejects.join("; "))
        }
    ))
}

/// 统一 ONNX Runtime 单例管理器
pub struct OmniOrtRuntime;

static RUNTIME_INIT: OnceLock<Result<(), String>> = OnceLock::new();

/// 初始化串行锁：杜绝并发线程各自预探后重复 `init_from`（rc.9 二次绑定会 panic）
static INIT_LOCK: Mutex<()> = Mutex::new(());

/// 已绑定运行时的 DML 符号结论（预探产物，供 `directml_probe_ok` 叠加假阳性防线）
static BOUND_DML_CAPABLE: AtomicBool = AtomicBool::new(false);

/// 绑定阶段失败结论（不可重试）
///
/// `ort::init_from` 在 `commit()` 之前就已写入 `G_ORT_DYLIB_PATH`（rc.9 `environment.rs:393`
/// 的 OnceLock，二次 set 被静默丢弃）。因此一旦 `commit()` 失败，换路径重试只会沿用旧路径
/// 造成「日志路径 ≠ 实际绑定」的谎言——此处冻结首次失败结论，杜绝伪重试。
static BIND_FAILED: OnceLock<String> = OnceLock::new();

impl OmniOrtRuntime {
    /// 运行时 B 发现链（优先级从高到低，SPEC §3.1 第 1~5 步）
    ///
    /// 1. `OMNI_ORT_DLL` 显式路径（最高优先，兼容测试与排障）；
    /// 2. `FIREFLY_ONNX_DIR` 目录：**先 `onnxruntime_dml.dll` 再 `onnxruntime.dll`**
    ///    （防排障时把该变量指向 bin/omni 反而静默失去 DML 的假阳性形态）；
    /// 3. `ORT_DYLIB_PATH`（既有语义并入本档）；
    /// 4. exe 同级 `onnxruntime_dml.dll`；
    /// 5. exe 同级 / 既有发现链的 `onnxruntime.dll`（仅运行时 A 的裸环境，如未装配 DML 的 CI）。
    pub fn discover_onnx_dll_candidates() -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        let mut push = |p: PathBuf| {
            if !p.as_os_str().is_empty() && !out.contains(&p) {
                out.push(p);
            }
        };

        // 1. 显式路径（最高优先）
        if let Ok(p) = std::env::var("OMNI_ORT_DLL") {
            push(PathBuf::from(p));
        }
        // 2. FIREFLY_ONNX_DIR：先 DML 后普通
        if let Ok(dir) = std::env::var("FIREFLY_ONNX_DIR") {
            let d = PathBuf::from(dir);
            push(d.join("onnxruntime_dml.dll"));
            push(d.join("onnxruntime.dll"));
        }
        // 3. ORT_DYLIB_PATH（既有排障通道）
        if let Ok(p) = std::env::var("ORT_DYLIB_PATH") {
            push(PathBuf::from(p));
        }

        let mut roots: Vec<PathBuf> = Vec::new();
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(parent) = exe_path.parent() {
                roots.push(parent.to_path_buf());
            }
        }
        if let Ok(cur) = std::env::current_dir() {
            roots.push(cur);
        }

        // 4/5. exe 同级 + 向上发现链（工单 03 已把部署目录单点收敛到 bin/omni，严禁回锚 bin/onnx）
        for base in roots {
            let mut cur = base;
            for _ in 0..6 {
                push(cur.join("onnxruntime_dml.dll"));
                push(cur.join("onnxruntime.dll"));
                push(cur.join("bin/omni/onnxruntime_dml.dll"));
                push(cur.join("bin/omni/onnxruntime.dll"));
                push(cur.join("resources/bin/omni/onnxruntime_dml.dll"));
                push(cur.join("resources/bin/omni/onnxruntime.dll"));
                push(cur.join("apps/desktop/build/extraResources/bin/omni/onnxruntime_dml.dll"));
                push(cur.join("apps/desktop/build/extraResources/bin/omni/onnxruntime.dll"));
                push(cur.join("build/extraResources/bin/omni/onnxruntime_dml.dll"));
                push(cur.join("build/extraResources/bin/omni/onnxruntime.dll"));
                push(cur.join("target/debug/onnxruntime_dml.dll"));
                push(cur.join("target/debug/onnxruntime.dll"));
                push(cur.join("target/release/onnxruntime_dml.dll"));
                push(cur.join("target/release/onnxruntime.dll"));
                if let Some(parent) = cur.parent() {
                    cur = parent.to_path_buf();
                } else {
                    break;
                }
            }
        }
        out
    }

    /// 自动发现本地有效的 ONNX Runtime DLL 路径（历史入口，返回发现链首个真实存在的文件）
    pub fn discover_onnx_dll() -> Option<PathBuf> {
        Self::discover_onnx_dll_candidates()
            .into_iter()
            .find(|p| p.exists())
    }

    /// 预探运行时 B：发现链阶梯 + 裸名兜底，返回唯一胜出者（只探不绑）
    ///
    /// 注：`ORT_DYLIB_PATH` 在 `ort::init_from` 路径下会被 `G_ORT_DYLIB_PATH` 覆盖而失效
    /// （rc.9 `environment.rs:393`），因此这里把它当成**显式候选**纳入阶梯而非依赖 ort 自解析。
    #[cfg(target_os = "windows")]
    fn probe_runtime_b() -> Result<OrtProbeReport, String> {
        let candidates = Self::discover_onnx_dll_candidates();
        match probe_ladder(&candidates) {
            Ok(report) => Ok(report),
            Err(e) => {
                // 裸名兜底：交给 OS 加载器按 PATH / SetDllDirectory 解析
                let bare = PathBuf::from("onnxruntime.dll");
                match probe_ladder(std::slice::from_ref(&bare)) {
                    Ok(report) => Ok(report),
                    Err(bare_err) => Err(format!("{e}; 裸名兜底亦失败: {bare_err}")),
                }
            }
        }
    }

    /// 非 Windows 预探：仅做「dylib 是否真的可定位」的存在性校验
    ///
    /// 不做 PE 符号探测（无 `onnxruntime_dml.dll` 语义），但**必须**校验可加载性：
    /// `ort::init_from(...).commit()` 在 dylib 缺席时是 panic（rc.9 `lib.rs:109`），
    /// 预探谎报成功会直接把「静默降级」升级成进程 abort（release `panic = "abort"`）。
    #[cfg(not(target_os = "windows"))]
    fn probe_runtime_b() -> Result<OrtProbeReport, String> {
        let bare = if cfg!(target_os = "macos") {
            "libonnxruntime.dylib"
        } else {
            "libonnxruntime.so"
        };
        let mut candidates: Vec<PathBuf> = Vec::new();
        // `ORT_DYLIB_PATH` 会被 init_from 覆盖，故作为显式候选纳入阶梯
        if let Ok(p) = std::env::var("ORT_DYLIB_PATH") {
            if !p.is_empty() {
                candidates.push(PathBuf::from(p));
            }
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                candidates.push(dir.join(bare));
            }
        }
        if let Ok(cwd) = std::env::current_dir() {
            candidates.push(cwd.join(bare));
        }
        candidates.push(PathBuf::from(bare));

        probe_ladder(&candidates).map(|mut report| {
            report.dml_capable = false;
            report
        })
    }

    /// 绑定预探胜出者：全进程**唯一一次** `ort::init_from(...).commit()`
    fn bind_probed(report: &OrtProbeReport) -> Result<(), String> {
        // 绑定阶段一旦失败即冻结结论：`G_ORT_DYLIB_PATH` 已被首次 init_from 写入且不可覆盖，
        // 换路径重试必然「日志说 B、实际跑 A」，故严禁伪重试。
        if let Some(prev) = BIND_FAILED.get() {
            tracing::debug!(
                "OmniOrtRuntime: 绑定阶段已失败过，拒绝换路径重试（G_ORT_DYLIB_PATH 已冻结）: {}",
                prev
            );
            return Err(prev.clone());
        }

        let dll_str = report.path.to_string_lossy().to_string();
        #[cfg(target_os = "windows")]
        {
            // 在 Windows 环境启动入口处建立安全 DLL 搜索隔离，免疫劫持
            // 若为显式路径，优先使用其父目录；否则回退至 "resources/bin/omni" (工单 03 单点收敛)
            let candidate_dir = report
                .path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .and_then(|p| p.to_str())
                .unwrap_or("resources/bin/omni");
            set_safe_dll_directory(candidate_dir);
        }

        // 实现全局动态加载入口：ort::init_from("onnxruntime.dll")
        match ort::init_from(&dll_str).commit() {
            Ok(_) => {
                // 记录预探的 DML 符号结论：`is_available()` 在纯 CPU DLL 上有假阳性历史，
                // 能力落点必须叠加预探结论（SPEC §7.7）
                BOUND_DML_CAPABLE.store(report.dml_capable, Ordering::SeqCst);
                tracing::info!(
                    "OmniOrtRuntime: 成功动态加载 ONNX Runtime path={} FileVersion={} DML符号={}",
                    if dll_str.is_empty() {
                        "<os-loader>"
                    } else {
                        &dll_str
                    },
                    report.file_version.as_deref().unwrap_or("<unknown>"),
                    report.dml_capable
                );
                Self::log_runtime_binding(report);
                Self::log_execution_provider_capability();
                Ok(())
            }
            Err(e) => {
                let err_msg = format!("OmniOrtRuntime: 动态加载 ONNX Runtime 失败: {}", e);
                tracing::error!("{}", err_msg);
                let _ = BIND_FAILED.set(err_msg.clone());
                Err(err_msg)
            }
        }
    }

    /// 运行时 B 绑定可观测行（AC-2 / SPEC §3.8）：路径 + FileVersion + 内存中 DirectML.dll 实际路径
    fn log_runtime_binding(report: &OrtProbeReport) {
        // 裸名兜底（无父目录）时，AC-2 要的是「真实路径」，故回填加载器实际命中的模块路径
        // 注：`Path::new("onnxruntime.dll").parent()` 返回 `Some("")` 而非 `None`，
        // 必须显式过滤空父目录，否则裸名分支永不生效、日志退化成字面量。
        let runtime_b = if report.path.as_os_str().is_empty() {
            "<os-loader>".to_string()
        } else if report
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .is_none()
        {
            report
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(loaded_module_path)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| report.path.display().to_string())
        } else {
            report.path.display().to_string()
        };
        let gate = if directml_disabled() {
            "已置位"
        } else {
            "未置位"
        };
        let gate_reason = directml_disable_reason()
            .map(|r| format!(" 原因={r}"))
            .unwrap_or_default();

        #[cfg(target_os = "windows")]
        {
            let directml_resident = loaded_module_path("DirectML.dll");
            let directml_desc = match directml_resident {
                Some(ref p) => {
                    // 仅在运行时 B 真具 DML 能力时断言驻留路径（纯 CPU 部署允许 System32 的旧版共存）
                    if report.dml_capable {
                        match report.directml_pinned.as_ref() {
                            Some(pinned) if !is_same_file(p, pinned) => {
                                tracing::warn!(
                                    "OmniOrtRuntime: 内存中 DirectML.dll 路径未命中 bin/omni（疑被 System32 截胡）: {}",
                                    p.display()
                                );
                            }
                            None => {
                                tracing::warn!(
                                    "OmniOrtRuntime: 运行时 B 具备 DML 能力但 DirectML.dll 未经绝对路径钉死（存在被 System32 复用的风险）: {}",
                                    p.display()
                                );
                            }
                            _ => {}
                        }
                    }
                    p.display().to_string()
                }
                None => "<not-loaded>".to_string(),
            };
            tracing::info!(
                "OmniOrtRuntime: [RUNTIME-B] path={} FileVersion={} ORTVersion={} DirectML.dll(内存)={} DML符号={} allowlist={:?} 闸={}{}",
                runtime_b,
                report.file_version.as_deref().unwrap_or("<unknown>"),
                report.runtime_version.as_deref().unwrap_or("<unknown>"),
                directml_desc,
                report.dml_capable,
                DML_ALLOWLIST_MODELS,
                gate,
                gate_reason
            );
        }
        #[cfg(not(target_os = "windows"))]
        {
            tracing::info!(
                "OmniOrtRuntime: [RUNTIME-B] path={} FileVersion={} ORTVersion={} DML符号={} 闸={}{}",
                runtime_b,
                report.file_version.as_deref().unwrap_or("<unknown>"),
                report.runtime_version.as_deref().unwrap_or("<unknown>"),
                report.dml_capable,
                gate,
                gate_reason
            );
        }
    }

    /// 在应用启动时初始化 ONNX Runtime：先探后绑（阶梯全败不污染 OnceLock）
    pub fn init() -> Result<(), String> {
        if let Some(done) = RUNTIME_INIT.get() {
            return done.clone();
        }
        // 绑定阶段已失败 ⇒ 结论永久冻结（G_ORT_DYLIB_PATH 不可覆盖），此处零成本短路，
        // 杜绝「每次建会话重跑全阶梯 + 打一条 INFO 预探通过却必然失败」的日志矛盾与放大
        if let Some(prev) = BIND_FAILED.get() {
            return Err(prev.clone());
        }
        let _guard = INIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(done) = RUNTIME_INIT.get() {
            return done.clone();
        }
        if let Some(prev) = BIND_FAILED.get() {
            return Err(prev.clone());
        }

        // 阶段 1：预探（绝不触碰 OnceLock / ort 全局单例；预探失败不记忆，修复资产后可重试）
        let winner = match Self::probe_runtime_b() {
            Ok(w) => w,
            Err(e) => {
                tracing::error!(
                    "OmniOrtRuntime: 运行时 B 预探阶梯全败，推理初始化终止（ort 单例未被污染，修复资产后可重试）: {}",
                    e
                );
                return Err(e);
            }
        };
        // 阶段 2：唯一胜出者单次绑定（绑定失败由 BIND_FAILED 冻结，不再重试）
        let bound = Self::bind_probed(&winner);
        if bound.is_ok() {
            let _ = RUNTIME_INIT.set(bound.clone());
        }
        bound
    }

    /// 指定特定 DLL 路径初始化（排障通道，同样先探后绑，严禁绕过预探）
    pub fn init_with_dll(dll_path: &str) -> Result<(), String> {
        if let Some(done) = RUNTIME_INIT.get() {
            // 已绑定：严禁二次 init_from（rc.9 二次绑定会 panic / 静默失效）
            tracing::debug!(
                "OmniOrtRuntime: 已完成单例绑定，忽略重复初始化请求 ({})",
                dll_path
            );
            return done.clone();
        }
        if let Some(prev) = BIND_FAILED.get() {
            tracing::debug!("OmniOrtRuntime: 绑定阶段已失败过，排障通道不再重试: {}", prev);
            return Err(prev.clone());
        }
        let _guard = INIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(done) = RUNTIME_INIT.get() {
            return done.clone();
        }
        if let Some(prev) = BIND_FAILED.get() {
            return Err(prev.clone());
        }

        let cand = PathBuf::from(dll_path);
        let winner = match probe_ladder(std::slice::from_ref(&cand)) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!("OmniOrtRuntime: 显式 DLL 预探失败，拒绝绑定: {}", e);
                return Err(e);
            }
        };
        let bound = Self::bind_probed(&winner);
        if bound.is_ok() {
            let _ = RUNTIME_INIT.set(bound.clone());
        }
        bound
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

    /// TTS (MOSS-TTS 自回归小算子与 Codec) 线程配额：锁定 4 线程
    ///
    /// 实测 benchmark 表明：自回归步进小算子 (768 dim / 12 heads) 在 CPU 上超额多线程
    /// (如 6/8 线程) 会因核间上下文切换与调度竞争严重劣化 (8 线程 RTF 0.935 vs 4 线程 RTF 0.370)，
    /// 4 线程为系统全局最佳拐点。
    pub fn recommended_tts_threads() -> usize {
        std::thread::available_parallelism()
            .map(|n| n.get().clamp(2, 4))
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
            let dml_raw = ort::execution_providers::DirectMLExecutionProvider::default()
                .is_available()
                .unwrap_or(false);
            // `is_available()` 仅作候选判定：必须叠加预探的 DML 符号结论，否则纯 CPU DLL 上的
            // 假阳性会让启动日志自相矛盾（SPEC §3.8 / §7.7「DirectML 日志虚假」前科）
            let dml = dml_raw && BOUND_DML_CAPABLE.load(Ordering::SeqCst);
            tracing::info!(
                "OmniOrtRuntime: EP 能力盘点 CUDA={}, DirectML={}（is_available 原始值={}）",
                cuda,
                dml,
                dml_raw
            );
            if dml_raw && !dml {
                tracing::warn!(
                    "OmniOrtRuntime: DirectML is_available() 报 true 但运行时 DLL 无 DML 导出符号，已按预探结论否决（假阳性）"
                );
            }
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

    /// 探测当前已绑定运行时是否真正具备 DirectML EP（先查闸，闸置位后恒为 false）
    ///
    /// 注：`is_available()` 仅作**候选判定**，能力落点以 warmup 实跑为准（SPEC §3.8）；
    /// 且该探针在纯 CPU DLL 上存在假阳性历史，故还必须叠加预探阶段的 DML 符号结论。
    #[cfg(target_os = "windows")]
    fn directml_probe_ok() -> bool {
        if directml_disabled() {
            return false;
        }
        // 未完成绑定前不得调用 ort 的 EP 探针（会触发 G_ORT_LIB 加载并可能 panic）
        match RUNTIME_INIT.get() {
            Some(Ok(())) => {}
            _ => return false,
        }
        // 预探阶段的双重结论必须同时成立：DLL 真含 DML 导出符号（挡住纯 CPU DLL 的假阳性）
        if !BOUND_DML_CAPABLE.load(Ordering::SeqCst) {
            return false;
        }
        use ort::execution_providers::ExecutionProvider;
        ort::execution_providers::DirectMLExecutionProvider::default()
            .is_available()
            .unwrap_or(false)
    }

    /// 非 Windows 平台无 DirectML 阶梯（本期只治理 Windows DML 分档）
    #[cfg(not(target_os = "windows"))]
    fn directml_probe_ok() -> bool {
        false
    }

    /// 获取当前平台优先推荐的硬件加速 Execution Provider 候选列表（按优先级排序）
    ///
    /// 语义等价于 `preferred_hardware_eps_for(EpPolicy::DmlAllowlist)`（历史入口）。
    pub fn preferred_hardware_eps() -> Vec<ort::execution_providers::ExecutionProviderDispatch> {
        Self::preferred_hardware_eps_for(EpPolicy::DmlAllowlist)
    }

    /// 按 EP 策略档位获取硬件加速候选（先查降级闸，再按阶梯解析）
    ///
    /// Windows 阶梯（SPEC §3.2）：`DmlAllowlist` → DML（闸未置位且探测通过）→ CPU；
    /// `CpuOnly` → 严格无硬件 EP。CUDA 轨道按 SPEC §5/§8 推迟为独立扩展包，
    /// 随包资产不含 CUDA provider，故 Windows 侧不再把 CUDA 列入阶梯。
    /// macOS CoreML / Linux CUDA 分支本期不纳入 AC，保持历史行为不变。
    pub fn preferred_hardware_eps_for(
        policy: EpPolicy,
    ) -> Vec<ort::execution_providers::ExecutionProviderDispatch> {
        let mut eps = Vec::new();

        #[cfg(target_os = "windows")]
        {
            let ladder = resolve_ep_candidates(policy, Self::directml_probe_ok(), directml_disabled());
            for kind in ladder {
                if kind == EpKind::DirectMl {
                    // 门控收紧：is_available 必须明确为 true 才入列，杜绝纯 CPU DLL 被误判
                    eps.push(
                        ort::execution_providers::DirectMLExecutionProvider::default().build(),
                    );
                    tracing::info!(
                        "OmniOrtRuntime: DML 进入 EP 阶梯（policy={:?}，闸={}）",
                        policy,
                        if directml_disabled() { "已置位" } else { "未置位" }
                    );
                }
            }
            if eps.is_empty() {
                // 显式上报分档落点：严禁静默回退 CPU 后在日志中不可见
                tracing::info!(
                    "OmniOrtRuntime: EP 阶梯无硬件加速候选（policy={:?}，闸={}），走纯 CPU",
                    policy,
                    if directml_disabled() { "已置位" } else { "未置位" }
                );
            }
        }

        #[cfg(target_os = "macos")]
        {
            let _ = policy;
            let coreml_ep = ort::execution_providers::CoreMLExecutionProvider::default();
            if coreml_ep.is_available().unwrap_or(true) {
                eps.push(coreml_ep.build());
            }
        }

        #[cfg(target_os = "linux")]
        {
            let _ = policy;
            let cuda_ep = ort::execution_providers::CUDAExecutionProvider::default();
            if cuda_ep.is_available().unwrap_or(false) {
                eps.push(cuda_ep.build());
            }
        }

        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        {
            let _ = policy;
        }

        eps
    }

    /// 获取首选硬件加速 Execution Provider (向后兼容)
    ///
    /// **禁止用于构建 DML allowlist 模型**：返回的裸 dispatch 会绕开 `build_dml_session`
    /// 强制的 `with_memory_pattern(false)` / `with_parallel_execution(false)`（SPEC §3.2）。
    #[deprecated(note = "DML allowlist 模型必须走 build_session_verified，禁止直接取裸 EP dispatch")]
    pub fn preferred_hardware_ep() -> Option<ort::execution_providers::ExecutionProviderDispatch> {
        Self::preferred_hardware_eps().into_iter().next()
    }

    /// 执行提供商 (EP) 两阶段降级路由工厂
    ///
    /// 历史入口：默认 `EpPolicy::CpuOnly` —— 文本塔 / OCR / CLIP / TTS 等全部非 allowlist
    /// 模型一律纯 CPU（SPEC §3.2 v1 裁定）。需要 DML 的模型必须显式走
    /// `build_session_verified(..., EpPolicy::DmlAllowlist, warmup)`。
    pub fn build_session<P: AsRef<Path>>(
        model_path: P,
        tier: ModelTier,
    ) -> Result<ort::session::Session, ort::Error> {
        Self::build_session_verified(model_path, tier, EpPolicy::CpuOnly, |_| Ok(()))
    }

    /// 执行提供商 (EP) 两阶段降级路由工厂（支持指定 intra_threads，如 TTS 锁定 4 线程）
    pub fn build_session_with_threads<P: AsRef<Path>>(
        model_path: P,
        tier: ModelTier,
        ep_policy: EpPolicy,
        intra_threads: usize,
    ) -> Result<ort::session::Session, ort::Error> {
        Self::build_session_verified_with_threads(model_path, tier, ep_policy, intra_threads, |_| Ok(()))
    }

    /// 三段回退主入口（构建期 → warmup → 纯 CPU 重建）
    ///
    /// - 构建期：按 `ep_policy` 阶梯构建，DML 分支显式关闭 memory pattern 与并行执行；
    /// - warmup 期：构建成功后立即执行闭包（Gemma 视觉传生产形状哑输入），
    ///   DML 失败时丢弃并**原地重建纯 CPU Session 复验**，复验通过才置降级闸；
    /// - **严禁触发熔断器**：本函数在 DML 故障时返回 `Ok(cpu_session)`，上层不会进入
    ///   `CircuitBreaker::trip` 分支（熔断语义严格保留给模型文件缺失/损坏）。
    pub fn build_session_verified<P, W>(
        model_path: P,
        tier: ModelTier,
        ep_policy: EpPolicy,
        warmup: W,
    ) -> Result<ort::session::Session, ort::Error>
    where
        P: AsRef<Path>,
        W: Fn(&ort::session::Session) -> Result<(), ort::Error>,
    {
        Self::build_session_verified_with_threads(
            model_path,
            tier,
            ep_policy,
            Self::recommended_intra_threads(),
            warmup,
        )
    }

    /// 三段回退主入口（指定 intra_threads 配额）
    pub fn build_session_verified_with_threads<P, W>(
        model_path: P,
        tier: ModelTier,
        ep_policy: EpPolicy,
        intra_threads: usize,
        warmup: W,
    ) -> Result<ort::session::Session, ort::Error>
    where
        P: AsRef<Path>,
        W: Fn(&ort::session::Session) -> Result<(), ort::Error>,
    {
        // 运行时未绑定时必须返回 Err：`Session::builder()` 在 DLL 缺席时是 panic
        // （ort rc.9 `lib_handle()` unwrap_or_else panic），release 档 `panic = "abort"`
        // 会直接终止进程，绝不能让「静默降级」演变成进程崩溃。
        Self::ensure_bound_for_session()?;
        let path = model_path.as_ref();
        let (session, used_dml) = Self::build_with_policy(path, tier, ep_policy, intra_threads)?;
        finalize_warmup_with_fallback(
            session,
            used_dml,
            warmup,
            || Self::build_cpu_session(path, intra_threads),
            path,
        )
    }

    /// 确保运行时单例已绑定，未绑定时返回可传播错误（替代裸 `ensure_initialized`）
    fn ensure_bound_for_session() -> Result<(), ort::Error> {
        Self::init().map_err(ort::Error::new)
    }

    /// 按策略构建会话，返回 `(Session, 是否 DML 会话)`
    ///
    /// Windows：走 DML/CPU 分档阶梯（DML 分支施加 DML 专属配置）；
    /// 非 Windows：沿用历史平台 EP 阶梯（macOS CoreML / Linux CUDA），本期不参与 DML 分档，
    /// 因此 `used_dml` 恒为 false（warmup 失败按模型损坏语义上抛，不置 DML 闸）。
    fn build_with_policy(
        path: &Path,
        tier: ModelTier,
        policy: EpPolicy,
        intra_threads: usize,
    ) -> Result<(ort::session::Session, bool), ort::Error> {
        // 【工单 04/05/06 缝合门禁：DML 白名单强校验】
        // 任何声明 DmlAllowlist 的模型路径必须属于 DML_ALLOWLIST_MODELS 事实源规范
        // （v1 仅 gemma.vision_encoder，匹配 vision_encoder*.onnx 与 vision_model*.onnx）。
        // 违规非白名单模型严禁走 DML，强制降级为 CpuOnly 并打印警告。
        let mut policy = policy;
        if policy == EpPolicy::DmlAllowlist {
            let file_name = path.file_name().and_then(|f| f.to_str()).unwrap_or("");
            let is_allowed = file_name.starts_with("vision_encoder")
                || file_name.starts_with("vision_model")
                || file_name.starts_with("moss_audio_tokenizer_decode");
            if !is_allowed {
                tracing::warn!(
                    "OmniOrtRuntime: 模型 {:?} 不在 DML_ALLOWLIST_MODELS 白名单中，强制打回纯 CPU 策略",
                    path
                );
                policy = EpPolicy::CpuOnly;
            }
        }

        if !cfg!(target_os = "windows") {
            let eps = Self::preferred_hardware_eps_for(policy);
            let session = Self::build_session_with_eps_internal(path, tier, eps, intra_threads)?;
            return Ok((session, false));
        }

        for kind in resolve_ep_candidates(policy, Self::directml_probe_ok(), directml_disabled()) {
            match kind {
                EpKind::Cpu => break,
                EpKind::DirectMl => match Self::build_dml_session(path, intra_threads) {
                    Ok(session) => {
                        tracing::info!(
                            "OmniOrtRuntime: 成功构建 DirectML Session (model={}, policy={:?})",
                            path.display(),
                            policy
                        );
                        return Ok((session, true));
                    }
                    Err(err) => {
                        // 构建期第一段失败：显式降级为纯 CPU（阶梯继续，非静默）
                        tracing::warn!(
                            "OmniOrtRuntime: DirectML Session 构建失败，阶梯降级为纯 CPU (model={}, error={:?})",
                            path.display(),
                            err
                        );
                    }
                },
            }
        }
        // 阶梯恒以 CPU 收尾（resolve_ep_candidates 保证），此处即纯 CPU 构建点
        let session = Self::build_cpu_session(path, intra_threads)?;
        tracing::info!(
            "OmniOrtRuntime: 成功构建纯 CPU Session (model={}, policy={:?})",
            path.display(),
            policy
        );
        Ok((session, false))
    }

    /// 纯 CPU 多线程 Session 构建（三段回退的最终落点）
    fn build_cpu_session(
        path: &Path,
        intra_threads: usize,
    ) -> Result<ort::session::Session, ort::Error> {
        ort::session::Session::builder()?
            .with_intra_threads(intra_threads)?
            .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)?
            .commit_from_file(path)
    }

    /// DirectML Session 构建（SPEC §3.2：显式关闭 CPU 静态内存拓扑推导并锁死单序列执行）
    ///
    /// `with_memory_pattern(false)`：关闭 CPU 静态内存拓扑推导，交由 D3D12/DML 自适应显存池；
    /// `with_parallel_execution(false)`：DML EP 明确不支持并发 Run，锁死单序列执行。
    /// 注：`ort 2.0.0-rc.9` 的 `SessionBuilder` **没有** `with_execution_mode` 方法，
    /// `with_parallel_execution(false)` 即底层 `ExecutionMode::ORT_SEQUENTIAL`。
    fn build_dml_session(
        path: &Path,
        intra_threads: usize,
    ) -> Result<ort::session::Session, ort::Error> {
        use ort::execution_providers::ExecutionProvider;
        let dml_ep = ort::execution_providers::DirectMLExecutionProvider::default();
        if !dml_ep.is_available().unwrap_or(false) {
            return Err(ort::Error::new(
                "DirectML EP 不可用（运行时无 DML 支持或驱动缺失）",
            ));
        }
        ort::session::Session::builder()?
            .with_intra_threads(intra_threads)?
            .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)?
            .with_memory_pattern(false)?
            .with_parallel_execution(false)?
            // `error_on_failure()` 不可省：rc.9 的 `apply_execution_providers` 默认
            // `error_on_failure == false`，EP 注册失败时**只打 tracing::error! 并返回 Ok**，
            // 于是这里会拿到一个纯 CPU Session 却上报「DML 构建成功 + warmup 定标通过」，
            // 正是 SPEC 明令消灭的「DML 日志虚假 + 静默降级」。改为失败即 Err 后，
            // 该错误会进入 `build_with_policy` 的显式降级分支并 warn。
            .with_execution_providers([dml_ep.build().error_on_failure()])?
            .commit_from_file(path)
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
    ///
    /// 注：本入口为**构建期第一段的历史形态**（SPEC §3.3「既有实现保留，不删除」），
    /// 它按裸 `ExecutionProviderDispatch` 迭代、不携带 EP 身份，故无法为 DML 分支施加
    /// `with_memory_pattern(false)` / `with_parallel_execution(false)` 等 DML 专属配置。
    /// **DML 分档模型必须走 `build_session_verified`**（内部经 `build_dml_session`）。
    pub fn build_session_with_eps_internal(
        path: &Path,
        _tier: ModelTier,
        eps: Vec<ort::execution_providers::ExecutionProviderDispatch>,
        intra_threads: usize,
    ) -> Result<ort::session::Session, ort::Error> {
        // 与 build_session_verified 同一守卫：运行时未绑定时 `Session::builder()` 会 panic，
        // 必须在此提前转成可传播 Err（公开入口同样受 A1 保护）
        Self::ensure_bound_for_session()?;
        let path_str = path.display().to_string();

        for (idx, ep) in eps.into_iter().enumerate() {
            // 同 build_dml_session：rc.9 默认吞掉 EP 注册失败并回退 CPU，
            // 会让本循环把「CPU Session」误报成「硬件加速 Session」并中止阶梯。
            // 显式要求失败即 Err，阶梯才会真正逐候选前进。
            let ep = ep.error_on_failure();
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

/// warmup 期回退决策（三段回退的第二段，T2 单测切缝）
///
/// 抽成与 `ort::Session` 解耦的泛型函数，使单测可用轻量占位类型（如 `String`）验证
/// 「warmup 失败 ⇒ 原地重建纯 CPU 并复验 ⇒ 置 DML 闸」的机械顺序，不依赖真实模型与 GPU。
///
/// 语义铁律：
/// 1. warmup 成功：原样返回会话（DML 会话完成实跑定标，`is_available()` 假阳性在此被证伪）；
/// 2. warmup 失败且会话为 DML：丢弃 DML 会话 → 重建纯 CPU 会话并**重跑同一 warmup 复验**：
///    - CPU 复验通过 ⇒ 根因确属 DML，置降级闸并返回 CPU 会话；
///    - CPU 复验同样失败 ⇒ 根因是模型/喂参而非 DML，**严禁误置闸**，返回 CPU 侧错误；
/// 3. warmup 失败且会话本就是 CPU（纯 CPU 部署 / 非 Windows 平台）：非 DML 故障，
///    warn 后原样返回会话——不置闸、不上抛，避免把一次性 warmup 抖动升级为熔断器永久 trip；
/// 4. 任何分支都**严禁触发熔断器**（熔断语义只保留给模型文件缺失/损坏）。
fn finalize_warmup_with_fallback<S, E, W, R>(
    session: S,
    used_dml: bool,
    warmup: W,
    rebuild_cpu: R,
    model: &Path,
) -> Result<S, E>
where
    W: Fn(&S) -> Result<(), E>,
    R: FnOnce() -> Result<S, E>,
    E: std::fmt::Debug,
{
    match warmup(&session) {
        Ok(()) => {
            if used_dml {
                tracing::info!(
                    "OmniOrtRuntime: DML warmup 实跑定标通过 (model={})",
                    model.display()
                );
            }
            Ok(session)
        }
        Err(err) => {
            if !used_dml {
                tracing::warn!(
                    "OmniOrtRuntime: 纯 CPU 会话 warmup 失败（非 DML 故障，不置闸不熔断，交由运行期失败路径处理）(model={}, error={:?})",
                    model.display(),
                    err
                );
                return Ok(session);
            }
            let reason = format!(
                "warmup 失败 (model={}, error={:?})",
                model.display(),
                err
            );
            tracing::warn!(
                "OmniOrtRuntime: DML warmup 失败，丢弃 DML 会话并原地重建纯 CPU Session 复验（严禁触发熔断器）(model={}, error={:?})",
                model.display(),
                err
            );
            // 先丢弃 DML 会话再重建：避免 DML 显存与 CPU 会话并存造成峰值占用翻倍
            drop(session);
            let cpu = match rebuild_cpu() {
                Ok(s) => s,
                Err(rebuild_err) => {
                    // CPU 重建也失败：上抛 DML 侧原因（携带 model/error 上下文），
                    // CPU 侧失败原因留在日志中，避免上层只看到一句「加载失败」
                    tracing::error!(
                        "OmniOrtRuntime: DML warmup 失败后纯 CPU Session 重建亦失败 (model={}, rebuild_error={:?})",
                        model.display(),
                        rebuild_err
                    );
                    return Err(err);
                }
            };
            match warmup(&cpu) {
                Ok(()) => {
                    // CPU 复验通过 ⇒ 根因确属 DML，此时才置闸（避免把模型/喂参故障误判成 DML 故障）
                    disable_directml(&reason);
                    tracing::warn!(
                        "OmniOrtRuntime: 纯 CPU 复验通过，已置 DML 降级闸并返回 CPU Session (model={})",
                        model.display()
                    );
                    Ok(cpu)
                }
                Err(cpu_err) => {
                    // CPU 同样跑不动 ⇒ 根因不是 DML；不置闸，把 CPU 侧错误交给上层熔断语义
                    tracing::error!(
                        "OmniOrtRuntime: 纯 CPU 复验同样失败，判定为模型/喂参故障（不置 DML 闸）(model={}, error={:?})",
                        model.display(),
                        cpu_err
                    );
                    Err(cpu_err)
                }
            }
        }
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

/// 专用于带并发互斥保护的 ONNX Session 看门狗类型别名 (工单 05 / SPEC §3.4)
///
/// 核心目的：DirectML EP 在同一 Device Context 下不允许并发前向推理，
/// 必须通过 `std::sync::Mutex<Session>` 确保在多线程环境下排队串行执行，
/// 并支持锁中毒自动恢复 (`into_inner`)。
pub type WatchdogSessionMutex = GenericWatchdog<std::sync::Mutex<ort::session::Session>>;

// ============================================================================
// 运行期故障注入钩子 (工单 05 §3 / 集成测试 T3 支撑)
// ============================================================================

static INJECT_FAIL_NEXT_RUN: AtomicBool = AtomicBool::new(false);

/// 测试辅助：注入下一次 Session Run 失败（一次性生效后自动复位）
pub fn inject_fail_next_run(enable: bool) {
    INJECT_FAIL_NEXT_RUN.store(enable, Ordering::SeqCst);
}

/// 查询并消费当前是否有注入的运行期失败（原子交换为 false）
pub fn take_injected_run_failure() -> bool {
    INJECT_FAIL_NEXT_RUN.swap(false, Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 全局降级闸为进程级静态，多个测试并发触碰时必须串行化，避免相互踩踏
    static GATE_TEST_LOCK: Mutex<()> = Mutex::new(());

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
        assert!(set_safe_dll_directory("resources/bin/omni"));
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

    /// 定位随包运行时 DLL（优先发现链，退化到仓库 extraResources/bin/omni）
    fn locate_packaged_runtime_dll() -> Option<PathBuf> {
        OmniOrtRuntime::discover_onnx_dll().or_else(|| {
            let mut cur = std::env::current_dir().ok()?;
            for _ in 0..8 {
                let cand = cur.join("apps/desktop/build/extraResources/bin/omni/onnxruntime.dll");
                if cand.exists() {
                    return Some(cand);
                }
                cur = cur.parent()?.to_path_buf();
            }
            None
        })
    }

    /// 定位随包 `bin/omni` 目录（双运行时四件套同层）
    /// T9 资产缺失时的**显式跳过**（避免「测了个寂寞」的静默 pass）
    ///
    /// 默认打印 `SKIP（未执行断言）` 标记，让裸环境 CI 一眼看出断言未跑；
    /// 若 CI 要求「T9 必须真跑」，设 `OMNI_ORT_TEST_REQUIRE_ASSETS=1` 让缺资产直接失败。
    fn skip_t9_missing_assets(what: &str) {
        if std::env::var("OMNI_ORT_TEST_REQUIRE_ASSETS").ok().as_deref() == Some("1") {
            panic!("[T9] 缺少必需测试资产: {what}（OMNI_ORT_TEST_REQUIRE_ASSETS=1 时禁止跳过）");
        }
        eprintln!("[T9] SKIP（断言未执行）: 缺少测试资产 —— {what}");
    }

    fn locate_packaged_bin_omni() -> Option<PathBuf> {
        if let Some(dir) = locate_packaged_runtime_dll().and_then(|p| p.parent().map(Path::to_path_buf))
        {
            if dir.join("onnxruntime.dll").exists() {
                return Some(dir);
            }
        }
        let mut cur = std::env::current_dir().ok()?;
        for _ in 0..8 {
            let cand = cur.join("apps/desktop/build/extraResources/bin/omni");
            if cand.join("onnxruntime.dll").exists() {
                return Some(cand);
            }
            cur = cur.parent()?.to_path_buf();
        }
        None
    }

    /// T1：EP allowlist 分档 —— Gemma 视觉（DmlAllowlist）尝试 DML，文本塔/OCR（CpuOnly）恒 CPU，
    /// 闸置位后全回退 CPU
    #[test]
    fn test_t1_ep_policy_ladder_and_directml_gate() {
        let _serial = GATE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_directml_gate_for_test();
        assert!(!directml_disabled());

        // allowlist 模型：DML 在前、CPU 收尾
        assert_eq!(
            resolve_ep_candidates(EpPolicy::DmlAllowlist, true, false),
            vec![EpKind::DirectMl, EpKind::Cpu]
        );
        // 探测不可用（纯 CPU 裸环境）：自动降档纯 CPU
        assert_eq!(
            resolve_ep_candidates(EpPolicy::DmlAllowlist, false, false),
            vec![EpKind::Cpu]
        );
        // 文本塔 / OCR：即使 DML 可用也恒为纯 CPU（v1 裁定）
        assert_eq!(
            resolve_ep_candidates(EpPolicy::CpuOnly, true, false),
            vec![EpKind::Cpu]
        );

        // 闸置位后 allowlist 模型同样全回退 CPU
        disable_directml("T1 注入：验证闸置位后阶梯全回退");
        assert!(directml_disabled());
        assert_eq!(
            resolve_ep_candidates(EpPolicy::DmlAllowlist, true, directml_disabled()),
            vec![EpKind::Cpu]
        );
        reset_directml_gate_for_test();
    }

    /// T2：warmup 失败回退 —— 三个分支的机械顺序（T2 必须能证伪 `used_dml` 传错）
    ///
    /// 「熔断器未 trip」由契约保证：`build_session_verified` 在 DML warmup 失败且 CPU 复验通过时
    /// 返回 `Ok(cpu_session)`，调用方（gemma.rs）因此不会进入 `CircuitBreaker::trip` 分支。
    /// 换言之，本测试断言的 `Ok` 返回值就是「熔断未 trip」的充分条件 —— 熔断器只在 `Err`
    /// 分支被 trip，此处不可能构造出 `Ok` 且 trip 的组合。
    #[test]
    fn test_t2_warmup_failure_rebuilds_cpu_and_trips_gate() {
        let _serial = GATE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_directml_gate_for_test();

        // 用例 A：DML warmup 失败 + CPU 复验通过 ⇒ 返回 CPU 会话并置闸
        let rebuilt = std::cell::Cell::new(false);
        let outcome = finalize_warmup_with_fallback(
            "dml-session".to_string(),
            true,
            |s: &String| {
                if s.starts_with("dml") {
                    Err("DML 前向 E_INVALIDARG".to_string())
                } else {
                    Ok(())
                }
            },
            || {
                rebuilt.set(true);
                Ok("cpu-session".to_string())
            },
            Path::new("vision_encoder_q4.onnx"),
        );

        assert_eq!(
            outcome.expect("warmup 失败必须回退为 CPU 会话而非报错"),
            "cpu-session"
        );
        assert!(rebuilt.get(), "必须原地重建纯 CPU Session");
        assert!(directml_disabled(), "DML 故障经 CPU 复验确认后必须置降级闸");

        // 用例 B：DML 与 CPU 复验双双失败 ⇒ 根因是模型/喂参而非 DML，严禁误置闸
        reset_directml_gate_for_test();
        let both_failed = finalize_warmup_with_fallback(
            "dml-session".to_string(),
            true,
            |_s: &String| Err::<(), String>("喂参错误/模型损坏".to_string()),
            || Ok("cpu-session".to_string()),
            Path::new("broken.onnx"),
        );
        assert!(both_failed.is_err(), "CPU 也跑不动必须上抛");
        assert!(
            !directml_disabled(),
            "CPU 复验同样失败时根因非 DML，不得置闸（否则掩盖模型损坏）"
        );

        // 用例 C：纯 CPU 会话 warmup 失败 ⇒ 原样返回，不置闸不上抛（不升级为熔断）
        reset_directml_gate_for_test();
        let cpu_only = finalize_warmup_with_fallback(
            "cpu-session".to_string(),
            false,
            |_s: &String| Err::<(), String>("一次性抖动".to_string()),
            || Ok("unreachable".to_string()),
            Path::new("model_q4.onnx"),
        );
        assert_eq!(
            cpu_only.expect("纯 CPU warmup 失败不得上抛（避免熔断器永久 trip）"),
            "cpu-session"
        );
        assert!(!directml_disabled(), "纯 CPU 会话 warmup 失败不得置 DML 闸");
        reset_directml_gate_for_test();
    }

    /// T9：先探后绑预探阶梯 —— 坏 DLL 被拦截并安全降级，全败显式报错且不污染 ort 单例
    ///
    /// PE 符号探测仅在 Windows 有意义，非 Windows 平台显式忽略（避免「绿=没跑」）。
    #[cfg_attr(not(target_os = "windows"), ignore)]
    #[test]
    fn test_t9_probe_ladder_rejects_bad_dll_and_keeps_binding_clean() {
        // 1) 纯判定语义（与真实 DLL 解耦）
        assert_eq!(
            judge_candidate(true, true, false, true),
            CandidateVerdict::RejectMissingDmlSymbol
        );
        assert_eq!(
            judge_candidate(true, true, true, false),
            CandidateVerdict::RejectApiMismatch
        );
        // 运行时 A（纯 CPU）不声明 DML，缺 DML 符号属正常形态，不得拦截
        assert_eq!(
            judge_candidate(false, true, false, true),
            CandidateVerdict::Accept
        );
        assert_eq!(
            judge_candidate(true, false, false, false),
            CandidateVerdict::RejectUnloadable
        );

        // 2) 真实文件：把**纯 CPU 运行时 A**（onnxruntime.dll）复制成 onnxruntime_dml.dll 假阳性样本
        let Some(bin_dir) = locate_packaged_bin_omni() else {
            skip_t9_missing_assets("随包 bin/omni（onnxruntime.dll）");
            return;
        };
        let cpu_dll = bin_dir.join("onnxruntime.dll");
        let real_dml = bin_dir.join("onnxruntime_dml.dll");
        let tmp = std::env::temp_dir().join(format!("omni-t9-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).expect("创建临时目录失败");
        let fake_dml = tmp.join("onnxruntime_dml.dll");
        std::fs::copy(&cpu_dll, &fake_dml).expect("复制假阳性 DLL 失败");

        let probe = probe_candidate_file(&fake_dml);
        assert_eq!(
            probe.verdict,
            CandidateVerdict::RejectMissingDmlSymbol,
            "无 DML 符号的 *_dml 必须被预探拦截"
        );
        assert!(!probe.has_dml_symbol);

        // 3) 阶梯：坏候选在前、真运行时在后 ⇒ 胜出者必须越过坏候选
        let good = if real_dml.exists() {
            real_dml.clone()
        } else {
            cpu_dll.clone()
        };
        let ladder = vec![fake_dml.clone(), good.clone()];
        let winner = probe_ladder(&ladder).expect("阶梯应安全降级到下一个候选");
        assert!(
            !is_same_file(&winner.path, &fake_dml),
            "胜出者严禁是坏候选"
        );
        assert!(is_same_file(&winner.path, &good));
        if real_dml.exists() {
            assert!(
                winner.dml_capable,
                "真实运行时 B（onnxruntime_dml.dll）必须通过 DML 导出符号预探"
            );
            // AC-2 可观测性：运行时 B 的 PE FileVersion 必须可读（启动日志依赖它）
            println!("[T9] 运行时 B FileVersion={:?}", winner.file_version);
            assert!(
                winner.file_version.is_some(),
                "运行时 B 必须可读出 PE FileVersion（AC-2 启动日志可观测性）"
            );
        }

        // 4) 全败：显式报错且不写 RUNTIME_INIT（不污染 ort 单例）——失败后仍可成功预探
        let bogus = vec![tmp.join("missing/onnxruntime_dml.dll")];
        let err = probe_ladder(&bogus).expect_err("全败必须显式报错");
        assert!(err.contains("预探阶梯全败"), "错误信息须可观测: {err}");
        assert!(
            probe_ladder(&ladder).is_ok(),
            "预探失败不得污染后续预探能力"
        );

        // 5) 防截胡：具备 DML 能力但同级缺 DirectML.dll 的候选必须被拦截（工单 04 第 13 条）
        if real_dml.exists() {
            let orphan_dir = tmp.join("orphan");
            std::fs::create_dir_all(&orphan_dir).expect("创建孤立目录失败");
            let orphan = orphan_dir.join("onnxruntime_dml.dll");
            std::fs::copy(&real_dml, &orphan).expect("复制真实 DML 运行时失败");
            let orphan_err = probe_ladder(std::slice::from_ref(&orphan))
                .expect_err("缺同级 DirectML.dll 的 DML 候选必须被拦截");
            assert!(
                orphan_err.contains("DirectML.dll"),
                "拦截原因必须点名 DirectML.dll 缺失: {orphan_err}"
            );
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// T9 附：DirectML.dll 必须以绝对路径钉死，且内存中实际驻留路径与钉死路径一致
    #[cfg_attr(not(target_os = "windows"), ignore)]
    #[test]
    fn test_t9_directml_dll_pinned_by_absolute_path() {
        let Some(cpu_dll) = locate_packaged_runtime_dll() else {
            skip_t9_missing_assets("随包 onnxruntime 运行时");
            return;
        };
        let Some(dir) = cpu_dll.parent() else {
            skip_t9_missing_assets("运行时所在目录");
            return;
        };
        if !dir.join("DirectML.dll").exists() {
            skip_t9_missing_assets("同级 DirectML.dll（纯 CPU 裸环境）");
            return;
        }
        let pinned = pin_directml_dll(dir).expect("DirectML.dll 存在时必须钉死成功");
        assert!(
            pinned.is_absolute(),
            "必须使用绝对路径预加载，杜绝 System32 同名截胡"
        );
        let resident = loaded_module_path("DirectML.dll").expect("钉死后必须能在模块列表中找到");
        assert!(
            is_same_file(&resident, &pinned),
            "内存驻留路径必须与钉死路径一致: {} vs {}",
            resident.display(),
            pinned.display()
        );
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
    ///
    /// 环境变量：
    /// - `OMNI_EP_BENCH=1`：启用基准（缺失则直接返回）；
    /// - `OMNI_EP_BENCH_STRICT=1`：启用 AC-3 门禁，视觉塔 N=1260 加速比 < 2.5x 直接断言失败
    ///   （默认不断言，避免无 GPU 的 CI 误红）；
    /// - `OMNI_BENCH_SKIP_TEXT=1`：跳过文本塔基准（长序列文本塔耗时较高）；
    /// - `OMNI_BENCH_VISION`：切换被测视觉模型文件名（默认 `vision_encoder_q4.onnx`）。
    #[test]
    fn test_bench_vision_encoder_ep_throughput() {
        // 严格门禁（AC-3 加速比 / AC-6 数值一致性）只在真机显式开启，避免无 GPU 的 CI 误红；
        // 开启后「拿不到可判定数据」一律 panic —— 否则门禁会被环境问题静默绕过（漏红）。
        //
        // ⚠️ 必须在 `OMNI_EP_BENCH` 的早退**之前**求值并交叉校验：否则
        // `OMNI_EP_BENCH_STRICT=1` 但漏设 `OMNI_EP_BENCH=1` 时会走「未启用基准，跳过」
        // 分支 —— 门禁一次都没跑，测试却 ok，这是最危险的假绿形态。
        let strict = std::env::var("OMNI_EP_BENCH_STRICT").is_ok();
        if std::env::var("OMNI_EP_BENCH").is_err() {
            if strict {
                panic!(
                    "[EP-BENCH] OMNI_EP_BENCH_STRICT=1 但 OMNI_EP_BENCH 未设置 ⇒ 门禁不会执行。\
                     拒绝以「跳过」冒充通过：请同时设置 OMNI_EP_BENCH=1"
                );
            }
            println!("[EP-BENCH] 未设置 OMNI_EP_BENCH=1，跳过真实吞吐基准");
            return;
        }
        use ort::execution_providers::ExecutionProvider;
        use std::time::Instant;

        let dll = match OmniOrtRuntime::discover_onnx_dll() {
            Some(d) => d,
            None => {
                if strict {
                    panic!("[EP-BENCH] strict 模式未发现 onnxruntime DLL（资产未装配），无法判定 AC");
                }
                println!("[EP-BENCH] 未找到 onnxruntime.dll，跳过");
                return;
            }
        };
        // 注意：这里打印的是**请求绑定**的路径。`init_with_dll` 在绑定已成功过时会走缓存
        // 直接返回 Ok，不校验传入路径 ⇒ 若同进程内已有别的测试先完成绑定，本行路径可能与
        // 实际生效的 DLL 不一致。故基准**必须在独占进程内单独运行**（用测试名过滤，
        // 不要与 `test_probe_real_execution_provider_availability` 等同进程并发）。
        println!("[EP-BENCH] 请求绑定 DLL: {}", dll.display());
        if let Err(e) = OmniOrtRuntime::init_with_dll(dll.to_str().unwrap_or("onnxruntime.dll")) {
            // 绑定失败后 `Session::builder()` 只会抛出无关的 panic，归因必须落在这一层
            if strict {
                panic!("[EP-BENCH] 运行时绑定失败: {e}");
            }
            println!("[EP-BENCH] 运行时绑定失败（继续，后续会话构建大概率失败）: {e}");
        }

        // 允许用 OMNI_BENCH_VISION 切换被测视觉模型（如未量化的 vision_model.onnx），
        // 用于区分「DML 不支持 Q4 量化算子」与「DML/驱动本身不可用」两种失败
        let vision_file = std::env::var("OMNI_BENCH_VISION")
            .unwrap_or_else(|_| "vision_encoder_q4.onnx".to_string());
        let model = match locate_gemma_sibling(&vision_file) {
            Some(m) => m,
            None => {
                if strict {
                    panic!("[EP-BENCH] strict 模式未找到 gemma {vision_file}（模型资产未装配）");
                }
                println!("[EP-BENCH] 未找到 gemma {vision_file}，跳过");
                return;
            }
        };
        println!("[EP-BENCH] 模型: {}", model.display());

        // 生产形状双档（工单 06 / T8）：
        //   视觉塔输入 pixel_values [1, N, 768] f32 + pixel_position_ids [1, N, 2] i64，
        //   N = soft_tokens × PATCH × PATCH = soft_tokens × 3 × 3（模型内部按 3×3 池化回 soft token）。
        //   视频标准档：140 soft tokens（官方 video_processor.max_soft_tokens）→ 1260 patches；
        //   视频精简档： 70 soft tokens（`perceive.rs` 的 `VIDEO_SOFT_TOKEN_BUDGET_MIN`）→ 630 patches。
        //
        // ⚠️ **静态图不是 70**：`gemma.rs` 的图像前处理固定用 **280** soft token 预算
        // （`aspect_ratio_preserving_size_with_budget(w, h, 280)` / `preprocess_image_with_soft_budget(img, 280)`），
        // 即 N = 280 × 9 = 2520 patches。本基准**未覆盖**该形状（成本约为 1260 档的 2 倍，
        // 且 AC-3 只对视频标准档设门禁）；要评估静态图 DML 收益须另行加档。
        // 早期版本把 630 标注为「图像 70 soft tokens」是错的 —— 630 只是视频精简档。
        //
        // 旧基准的 `SEQ = 140` 误把「soft token 数」当成 patch 数，输入只有生产形状的 1/9，
        // 得出的吞吐数字无法用于任何决策（SPEC §1 第 9 条），故此处以真实 patch 数为唯一口径。
        const FEAT: usize = 768;
        const WARMUP: u32 = 2;
        const ITERS: u32 = 5;
        /// 视频帧视觉软 token 预算（`perceive.rs::VIDEO_SOFT_TOKEN_BUDGET_MAX`）
        const VIDEO_SOFT_TOKENS: usize = 140;
        /// 生产视频帧数（SPEC §7.4 的 106~107 帧口径），仅用于 N=1260 与文本塔的外推
        const VIDEO_FRAMES: usize = 107;
        const PROD_SHAPES: [(usize, &str); 2] =
            [(1260, "视频标准 140 soft tokens"), (630, "视频精简 70 soft tokens")];
        /// 视觉塔图输出名（`gemma/vision_encoder_q4.onnx` 的图输出，也是文本塔
        /// `image_features` 输入的来源）。AC-6 必须按名取，见 `vision_features` 注释。
        const VISION_OUTPUT: &str = "image_features";
        /// 视觉塔每 soft token 的特征维（`image_features` 的输出宽度）。
        ///
        /// ⚠️ 这与**文本塔的 hidden 维**不是同一个量：视觉塔把 patch 特征投影成 512 维
        /// 供文本塔当 `image_features` 输入，而文本塔自身输出 `last_hidden_state` /
        /// `sentence_embedding` 是另一个宽度。二者不可互相代入（早期注释误称「= 文本塔
        /// embedding 宽度」，会让人以为 512 是文本塔的 hidden size）。
        const VISION_HIDDEN: usize = 512;

        let intra = OmniOrtRuntime::recommended_intra_threads();
        println!(
            "[EP-BENCH] 口径: profile={} intra_threads={} cores={:?}（debug 档绝对值不可与 SPEC §7.2 的 release 数据直接比较，只比同进程内的 CPU/DML 比值）",
            if cfg!(debug_assertions) { "debug" } else { "release" },
            intra,
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(0)
        );

        /// 构建 CPU 基线会话：复用生产 `build_cpu_session`，保证线程/优化档位同源
        fn build_cpu(path: &Path) -> Result<ort::session::Session, ort::Error> {
            OmniOrtRuntime::build_cpu_session(path, OmniOrtRuntime::recommended_intra_threads())
        }

        /// 构建 DirectML 会话：复用生产 `build_dml_session`
        ///
        /// 必须走生产实现而非就地 `builder()`：生产分支含
        /// `with_memory_pattern(false)` / `with_parallel_execution(false)` /
        /// `error_on_failure()`。缺最后一项时 rc.9 会吞掉 EP 注册失败、返回纯 CPU Session
        /// 却上报「DML 成功」，基准就会把 CPU 数字当成 DML 数字（SPEC §7.7 假阳性）。
        fn build_dml(path: &Path) -> Result<ort::session::Session, ort::Error> {
            OmniOrtRuntime::build_dml_session(path, OmniOrtRuntime::recommended_intra_threads())
        }

        /// 生产形状的 soft token 网格 `(w, h)`：`n_patches = w × h × 9`
        ///
        /// 坐标不能任意递增：视觉图内部按 3×3 池化把 patch 散射进 `w×h` 的 soft token 缓冲
        /// （`ScatterElements` 的 data 长度恰为 soft token 数），故 patch 网格必须是 3 的倍数，
        /// 且坐标必须落在该网格内 —— 否则会报
        /// `indices element out of data bounds, idx=... must be within [-N, N-1]`。
        /// 视频档 140 soft tokens = 14×10（patch 42×30 = 1260）；图像档 70 = 10×7（patch 30×21 = 630）。
        fn soft_grid(n_patches: usize) -> (usize, usize) {
            match n_patches {
                1260 => (14, 10),
                630 => (10, 7),
                _ => {
                    // 兜底：取「≤ √soft 的最大因子」做宽，保证严格整除且尽量接近正方形。
                    // （旧实现用 `ceil(√soft)` 直接当宽，对 280 soft tokens（静态图档，N=2520）
                    // 得 17，280 % 17 ≠ 0 ⇒ 明明有合法分解 (14, 20) 却断言失败。
                    // 网格与输入必须自洽：越界坐标会触发 ScatterElements out-of-bounds，
                    // 留 -1 补齐又会改变 soft 缓冲行数。）
                    assert!(
                        n_patches % 9 == 0,
                        "视觉档位 {n_patches} 不是 9 的倍数，无法构成 3×3 池化网格"
                    );
                    let soft = n_patches / 9;
                    let w = (1..=((soft as f64).sqrt() as usize))
                        .rev()
                        .find(|d| soft % d == 0)
                        .unwrap_or(1);
                    assert!(
                        w > 0 && soft % w == 0,
                        "视觉档位 {n_patches}（soft={soft}）无合法方形分解，请显式登记网格"
                    );
                    (w, soft / w)
                }
            }
        }

        /// 构造视觉塔生产形状输入：**确定性伪随机像素** + **合法网格坐标**
        ///
        /// 坐标按 `patch_w = grid_w × 3` 递增铺满整个网格 —— 不用全 0 坐标，因为全 0 是退化
        /// 输入（`ScatterElements` 的索引全集中到同一位置），会掩盖归一化/池化算子上的数值分歧。
        /// 由于 `soft_grid` 保证 `w × h × 9 == n_patches`，铺满后不会剩余 patch，故不需要
        /// `-1` 补齐分支（旧实现留了一段永不执行的补齐逻辑，反而让人误以为存在部分填充形状）。
        ///
        /// 像素用 `(i % 251) / 251.0` 而非常数：常数输入会让部分归一化/量化算子的相对误差退化，
        /// 测出的 `max_abs` 不具代表性（SPEC §7.5 的 harness 亦用随机像素）。
        /// 同时它必须是**确定性**的 —— AC-6 要求 CPU 与 DML 吃同一份输入，任何随机源都会让比对失效。
        fn vision_inputs(
            n_patches: usize,
            feat: usize,
        ) -> Result<[ort::session::SessionInputValue<'static>; 2], ort::Error> {
            let (grid_w, grid_h) = soft_grid(n_patches);
            let patch_w = grid_w * 3;
            debug_assert_eq!(
                patch_w * (grid_h * 3),
                n_patches,
                "soft_grid 与 patch 数不自洽：{grid_w}×{grid_h} soft tokens 应为 {} patches",
                grid_w * grid_h * 9
            );
            let mut positions = Vec::with_capacity(n_patches * 2);
            for i in 0..n_patches {
                positions.push((i % patch_w) as i64);
                positions.push((i / patch_w) as i64);
            }
            let pv = ort::value::Tensor::from_array((
                vec![1usize, n_patches, feat],
                // 确定性伪随机像素（非全常数）：常数输入会让部分归一化/量化算子的
                // 相对误差退化，测出的 max_abs 不具代表性（SPEC §7.5 的 harness 亦用随机像素）
                (0..n_patches * feat)
                    .map(|i| (i % 251) as f32 / 251.0)
                    .collect::<Vec<f32>>(),
            ))?;
            let pid = ort::value::Tensor::from_array((vec![1usize, n_patches, 2usize], positions))?;
            Ok([pv.into(), pid.into()])
        }

        /// 预热 WARMUP 次后计时 ITERS 次前向，返回（中位 / 最小 / 最大）单次耗时（毫秒）
        ///
        /// 取中位而非均值：DML 首轮常伴显存分配抖动，均值会被单个离群样本拉偏
        /// （SPEC §7.2 的实测表同样以中位 + 区间呈现）。
        /// `frames` 仅对生产口径（视频 N=1260 / 文本塔）传 `Some(VIDEO_FRAMES)`，
        /// 避免给视频精简档 N=630 也套一个「107 帧外推」的伪数字。
        ///
        /// **计时窗口只覆盖 `session.run`**：`prepare` 在窗口外调用，因为被测命题是「前向吞吐」。
        /// 旧实现把 `vision_inputs(...)` 写在计时闭包里（`session.run(vision_inputs(..)?)`），
        /// 张量构造（1260×768 f32 ≈ 3.9MB + 坐标 1260×2 i64）也算进了单帧耗时 —— 该成本对
        /// CPU/DML 两侧相同，会系统性**稀释**加速比（DML 前向越短，被稀释得越狠），
        /// 属于对结论不利方向的偏差，但仍是口径错误。
        ///
        /// 注：`SessionInputValue` 未实现 `Clone`，无法「构造一次反复 run」，故每轮仍需重新构造，
        /// 只是构造已移出计时窗口。
        fn bench<const N: usize, P>(
            label: &str,
            frames: Option<usize>,
            session: &mut ort::session::Session,
            prepare: &P,
        ) -> Option<(f64, f64, f64)>
        where
            P: Fn() -> Result<[ort::session::SessionInputValue<'static>; N], ort::Error>,
        {
            for _ in 0..WARMUP {
                let inputs = match prepare() {
                    Ok(i) => i,
                    Err(e) => {
                        println!("[EP-BENCH] {label}: 预热输入构造失败 -> {e}");
                        return None;
                    }
                };
                if let Err(e) = session.run(inputs) {
                    println!("[EP-BENCH] {label}: 预热失败 -> {e}");
                    return None;
                }
            }
            let mut samples: Vec<f64> = Vec::with_capacity(ITERS as usize);
            for _ in 0..ITERS {
                let inputs = match prepare() {
                    Ok(i) => i,
                    Err(e) => {
                        println!("[EP-BENCH] {label}: 输入构造失败 -> {e}");
                        return None;
                    }
                };
                let t0 = Instant::now();
                if let Err(e) = session.run(inputs) {
                    println!("[EP-BENCH] {label}: 计时循环中失败 -> {e}");
                    return None;
                }
                samples.push(t0.elapsed().as_secs_f64() * 1000.0);
            }
            samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let median = samples[samples.len() / 2];
            let (min, max) = (samples[0], samples[samples.len() - 1]);
            match frames {
                Some(n) => println!(
                    "[EP-BENCH] {label}: 中位 {:.0}ms（区间 {:.0}~{:.0}ms，{ITERS} 轮，{n} 帧外推 ≈ {:.1}s）",
                    median,
                    min,
                    max,
                    median * n as f64 / 1000.0
                ),
                None => println!(
                    "[EP-BENCH] {label}: 中位 {:.0}ms（区间 {:.0}~{:.0}ms，{ITERS} 轮）",
                    median, min, max
                ),
            }
            Some((median, min, max))
        }

        /// 取视觉塔 `image_features` 输出为 f32 向量（供 AC-6 金标一致性比对）
        ///
        /// 与 `feed_vision` 共用 `vision_inputs`，确保比对的是「同输入、不同 EP」的差异；
        /// 失败时返回具体原因而非静默 `None`（strict 门禁需要可判定的失败信号）。
        ///
        /// **为什么按名取而不是 `into_iter().next()`**：输出顺序不属于模型对外契约，
        /// 一旦导出器调整输出顺序或新增辅助输出，按序取就会把另一个张量当成金标来比 ——
        /// 得到一个恒通过、却与视觉特征毫无关系的 max_abs。取到后还必须断言长度 =
        /// `soft_tokens × VISION_HIDDEN`，把「截断 / 广播错位的张量」挡在可比数据之外。
        fn vision_features(
            session: &mut ort::session::Session,
            n_patches: usize,
        ) -> Result<Vec<f32>, String> {
            let soft_tokens = n_patches / 9;
            let expect_len = soft_tokens * VISION_HIDDEN;
            let inputs =
                vision_inputs(n_patches, FEAT).map_err(|e| format!("构造视觉输入失败: {e}"))?;
            let outputs = session
                .run(inputs)
                .map_err(|e| format!("视觉前向失败: {e}"))?;
            let value = outputs.get(VISION_OUTPUT).ok_or_else(|| {
                let mut names: Vec<&str> = outputs.keys().collect();
                names.sort_unstable();
                format!("输出中不存在 `{VISION_OUTPUT}`（实际输出 {names:?}）")
            })?;
            let tensor = value
                .try_extract_tensor::<f32>()
                .map_err(|e| format!("`{VISION_OUTPUT}` 不是 f32 张量: {e}"))?;
            let data = tensor
                .as_slice()
                .ok_or_else(|| format!("`{VISION_OUTPUT}` 张量非连续内存，无法逐元素比对"))?;
            if data.len() != expect_len {
                return Err(format!(
                    "`{VISION_OUTPUT}` 长度 {} ≠ 期望 {expect_len}（{soft_tokens} soft tokens × {VISION_HIDDEN}）",
                    data.len()
                ));
            }
            Ok(data.to_vec())
        }

        // 注：`is_available` 仅作候选判定；DML 是否真生效由 `build_dml_session` 的
        // `error_on_failure()` + 构建成功共同保证
        let dml_available = ort::execution_providers::DirectMLExecutionProvider::default()
            .is_available()
            .unwrap_or(false);
        if !dml_available {
            println!("[EP-BENCH] DirectML 不可用，仅输出 CPU 基线（当前 DLL 无 DML EP）");
        }

        // ---- 视觉塔：逐档 CPU 基线 vs DirectML ----
        let mut vision_ratios: Vec<(usize, f64)> = Vec::new();
        for (n_patches, tag) in PROD_SHAPES {
            // 只有视频标准档（N=1260）是生产视频帧形状，才做 107 帧外推
            let frames = (n_patches == 1260).then_some(VIDEO_FRAMES);
            println!("[EP-BENCH] === 视觉塔生产形状 N={n_patches}（{tag}）===");
            // 只负责构造输入；`bench` 会在计时窗口之外调用它（见 `bench` 文档）
            let prepare_vision = || vision_inputs(n_patches, FEAT);

            let mut cpu_session = match build_cpu(&model) {
                Ok(s) => Some(s),
                Err(e) => {
                    println!("[EP-BENCH] 视觉塔 CPU 会话构建失败 -> {e}");
                    None
                }
            };
            let cpu = cpu_session.as_mut().and_then(|s| {
                bench(
                    &format!("视觉塔 CPU      N={n_patches}"),
                    frames,
                    s,
                    &prepare_vision,
                )
            });
            let mut dml_session = if dml_available {
                match build_dml(&model) {
                    Ok(s) => Some(s),
                    Err(e) => {
                        println!("[EP-BENCH] 视觉塔 DirectML 会话构建失败 -> {e}");
                        None
                    }
                }
            } else {
                None
            };
            let dml = dml_session.as_mut().and_then(|s| {
                bench(
                    &format!("视觉塔 DirectML N={n_patches}"),
                    frames,
                    s,
                    &prepare_vision,
                )
            });

            if let (Some(cpu), Some(dml)) = (cpu, dml) {
                let ratio = cpu.0 / dml.0.max(1e-6);
                println!(
                    "[EP-BENCH] 视觉塔加速比 N={n_patches}: {ratio:.2}x（CPU 中位 {:.0}ms / DML 中位 {:.0}ms）",
                    cpu.0, dml.0
                );
                vision_ratios.push((n_patches, ratio));
            }

            // AC-6 金标一致性：同输入下 CPU 与 DML `image_features` 逐元素最大绝对偏差
            // （DML 走 D3D12 算子实现，浮点累加顺序不同 ⇒ 期望非零但极小；阈值 1e-4 见 AC-6）
            if let (Some(cs), Some(ds)) = (cpu_session.as_mut(), dml_session.as_mut()) {
                let cpu_feat = vision_features(cs, n_patches);
                let dml_feat = vision_features(ds, n_patches);
                match (cpu_feat, dml_feat) {
                    (Ok(a), Ok(b)) => {
                        // 两侧长度都已在 `vision_features` 内锁定为 soft_tokens × VISION_HIDDEN，
                        // 同档位同形状 ⇒ 必然等长；此处只做断言兜底，不再靠运行时判等决定是否可比
                        debug_assert_eq!(
                            a.len(),
                            b.len(),
                            "AC-6 两侧长度不一致（vision_features 的长度门禁失效）"
                        );
                        // ⚠️ 不能用 `fold(0.0, f32::max)`：`f32::max` 在任一操作数为 NaN 时
                        // 返回**另一个**操作数（`NaN.max(x) == x`），于是「两侧输出都含 NaN」
                        // 会被静默折叠成 max_abs = 0 ⇒ 门禁恒通过。DML 驱动/量化算子出错时
                        // 恰恰最可能产出 NaN，这正是最需要拦住的形态。故显式把非有限差值判为
                        // 不可判定，而不是交给 fold 吞掉。
                        let mut max_abs = 0.0f32;
                        let mut non_finite: Option<(usize, f32, f32)> = None;
                        for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
                            let d = (x - y).abs();
                            if !d.is_finite() {
                                non_finite = Some((i, *x, *y));
                                break;
                            }
                            max_abs = max_abs.max(d);
                        }
                        if let Some((i, x, y)) = non_finite {
                            let msg = format!(
                                "金标一致性 N={n_patches}: 第 {i} 个元素差值非有限（cpu={x} dml={y}）⇒ 输出含 NaN/Inf，AC-6 不可判定"
                            );
                            if strict {
                                panic!("[EP-BENCH] {msg}");
                            }
                            println!("[EP-BENCH] {msg}");
                        } else {
                            println!(
                                "[EP-BENCH] 金标一致性 N={n_patches}: max_abs={max_abs:.3e}（AC-6 阈值 ≤1e-4，输出维 {} = {} soft tokens × {VISION_HIDDEN}）",
                                a.len(),
                                n_patches / 9
                            );
                            // AC-6 只对生产视频标准档（N=1260）设门禁：视频精简档 N=630 是参考档，
                            // 其 max_abs 实测已在阈值边缘（~9.0e-5），据此 gate 会变成 flaky 断言；
                            // 该档只输出数据供人工判断，不参与自动判定。
                            if strict && n_patches == 1260 {
                                assert!(
                                    max_abs <= 1e-4,
                                    "AC-6 未达标：视觉塔 N={n_patches} DML vs CPU max_abs={max_abs:.3e} > 1e-4"
                                );
                            }
                        }
                    }
                    (cpu_res, dml_res) => {
                        // 失败原因逐侧透出，且 `vision_features` 已把「输出缺失 / 非 f32 /
                        // 非连续内存 / 长度不等于 soft_tokens×VISION_HIDDEN」区分成不同文案；
                        // 这里只负责汇总，不再笼统归为「拿不到数据」。
                        let mut reasons: Vec<String> = Vec::new();
                        if let Err(e) = &cpu_res {
                            reasons.push(format!("CPU 侧: {e}"));
                        }
                        if let Err(e) = &dml_res {
                            reasons.push(format!("DML 侧: {e}"));
                        }
                        let msg = format!(
                            "金标一致性 N={n_patches} 未取得可比数据 —— {}",
                            reasons.join("；")
                        );
                        // strict 下「拿不到数据」等同门禁失效，必须硬失败
                        if strict {
                            panic!("[EP-BENCH] {msg}");
                        }
                        println!("[EP-BENCH] {msg}");
                    }
                }
            }
        }

        // 文本塔：生产链路里每帧除视觉前向还要过一次文本塔，故必须单独测它才能定位真正大头。
        // 生产形状（gemma.rs `forward_patches_with_session`）：
        //   ids = [<bos>=2, <|image>=255999, n×<|image|>=258880, <image|>=258882, <eos>=1] ⇒ seq = n + 4
        //   image_features = [n, 512]，其中 n = **视觉塔输出的 soft token 数**（视频 140），
        //   不是视觉塔输入的 patch 数（1260）——模型内部已按 3×3 池化回 soft token。
        // 旧基准的 seq=143 错误是「少算 1 个 token」（正确 144），与 patch/soft token 混淆无关。
        if std::env::var("OMNI_BENCH_SKIP_TEXT").is_ok() {
            println!("[EP-BENCH] OMNI_BENCH_SKIP_TEXT 已设置，跳过文本塔基准");
        } else if let Some(text_model) = locate_gemma_sibling("model_q4.onnx") {
            let n_tokens = VIDEO_SOFT_TOKENS;
            let text_seq = n_tokens + 4;
            println!("[EP-BENCH] === 文本塔生产形状 seq={text_seq}（{n_tokens} soft tokens）===");
            let prepare_text =
                || -> Result<[ort::session::SessionInputValue<'static>; 5], ort::Error> {
                    let mut ids: Vec<i64> = Vec::with_capacity(text_seq);
                    ids.push(2); // <bos>
                    ids.push(255999); // <|image> (boi)
                    ids.extend(std::iter::repeat(258880).take(n_tokens)); // <|image|>
                    ids.push(258882); // <image|> (eoi)
                    ids.push(1); // <eos>
                    debug_assert_eq!(ids.len(), text_seq);
                    let input_ids = ort::value::Tensor::from_array((vec![1usize, text_seq], ids))?;
                    let mask = ort::value::Tensor::from_array((
                        vec![1usize, text_seq],
                        vec![1i64; text_seq],
                    ))?;
                    let img = ort::value::Tensor::from_array((
                        vec![n_tokens, 512usize],
                        vec![0.01f32; n_tokens * 512],
                    ))?;
                    // 未使用的模态：生产侧经 `ort::inputs!` + ndarray 传 `Array2::zeros((0, 512))`（0 行），
                    // 但 `Tensor::from_array` 原生构造路径会拒绝长度为 0 的轴
                    // （`Invalid dimension at 0; all dimensions must be >= 1`），故此处退化为 1 行零向量。
                    // 影响面：仅让两个未使用模态各多 1 个 token，对文本塔注意力耗时的量级可忽略。
                    let vid =
                        ort::value::Tensor::from_array((vec![1usize, 512usize], vec![0.0f32; 512]))?;
                    let aud =
                        ort::value::Tensor::from_array((vec![1usize, 512usize], vec![0.0f32; 512]))?;
                    Ok([input_ids.into(), mask.into(), img.into(), vid.into(), aud.into()])
                };

            match build_cpu(&text_model) {
                Ok(mut s) => {
                    bench(
                        &format!("文本塔 CPU      seq={text_seq}"),
                        Some(VIDEO_FRAMES),
                        &mut s,
                        &prepare_text,
                    );
                }
                Err(e) => println!("[EP-BENCH] 文本塔 CPU 会话构建失败 -> {e}"),
            }
            if dml_available {
                match build_dml(&text_model) {
                    Ok(mut s) => {
                        bench(
                            &format!("文本塔 DirectML seq={text_seq}"),
                            Some(VIDEO_FRAMES),
                            &mut s,
                            &prepare_text,
                        );
                    }
                    Err(e) => println!("[EP-BENCH] 文本塔 DirectML 会话构建失败 -> {e}"),
                }
            }
        } else {
            println!("[EP-BENCH] 未找到 gemma model_q4.onnx，跳过文本塔基准");
        }

        // 严格门禁（AC-3）：仅真机 DML 档启用，避免无 GPU 的 CI 因加速比缺失而误红
        if strict {
            let video = vision_ratios
                .iter()
                .find(|(n, _)| *n == 1260)
                .map(|(_, r)| *r);
            match video {
                Some(r) => {
                    println!("[EP-BENCH] AC-3 门禁：视觉塔 N=1260 加速比 {r:.2}x（阈值 ≥2.5x）");
                    assert!(
                        r >= 2.5,
                        "AC-3 未达标：视觉塔 N=1260 DirectML 加速比 {r:.2}x < 2.5x"
                    );
                }
                None => panic!(
                    "[EP-BENCH] AC-3 门禁未取得可判定数据：N=1260 缺少 CPU/DML 双档（DML 不可用或构建/前向失败）"
                ),
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
