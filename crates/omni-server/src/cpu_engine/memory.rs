#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

/// 热态推理安全门限 (512MB)
pub const HOT_INFER_FLOOR: u64 = 512 * 1024 * 1024;

/// 测试注入的可用内存（仅测试构建存在；生产构建不得存在内存伪造后门）
#[cfg(test)]
static MOCK_AVAILABLE_MEMORY: AtomicU64 = AtomicU64::new(0);

/// 注入测试用可用内存（仅测试构建）
#[cfg(test)]
pub fn set_mock_available_memory_bytes(bytes: Option<u64>) {
    MOCK_AVAILABLE_MEMORY.store(bytes.unwrap_or(0), Ordering::SeqCst);
}

/// 内存门禁判定失败的两类原因（严禁用 0 或 4GB 等伪造值顶替探测失败）
#[derive(Debug, Clone)]
pub enum MemoryGateError {
    /// 物理内存探测本身失败（API 不可用 / 平台不支持 / 解析失败）
    ProbeFailed(String),
    /// 探测成功但低于门限
    Insufficient { available: u64, floor: u64 },
}

/// 查询系统当前可用物理内存字节数。
///
/// 探测失败必须上浮错误：历史上的「保底返回 4GB」会让冷载入门禁在内存不足时误放行，
/// 直接把进程推到 OOM，且门禁显示"通过"。
pub fn get_available_physical_memory_bytes() -> Result<u64, String> {
    #[cfg(test)]
    {
        let mock = MOCK_AVAILABLE_MEMORY.load(Ordering::SeqCst);
        if mock > 0 {
            return Ok(mock);
        }
    }

    #[cfg(target_os = "windows")]
    {
        unsafe {
            use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
            let mut mem_status = MEMORYSTATUSEX {
                dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
                dwMemoryLoad: 0,
                ullTotalPhys: 0,
                ullAvailPhys: 0,
                ullTotalPageFile: 0,
                ullAvailPageFile: 0,
                ullTotalVirtual: 0,
                ullAvailVirtual: 0,
                ullAvailExtendedVirtual: 0,
            };
            if GlobalMemoryStatusEx(&mut mem_status) != 0 {
                return Ok(mem_status.ullAvailPhys);
            }
            return Err("GlobalMemoryStatusEx 调用失败，无法探测可用物理内存".to_string());
        }
    }

    #[cfg(target_os = "linux")]
    {
        let content = std::fs::read_to_string("/proc/meminfo")
            .map_err(|e| format!("读取 /proc/meminfo 失败: {e}"))?;
        for line in content.lines() {
            if line.starts_with("MemAvailable:") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    let kb = parts[1]
                        .parse::<u64>()
                        .map_err(|e| format!("解析 MemAvailable 失败: {e}"))?;
                    return Ok(kb * 1024);
                }
            }
        }
        return Err("/proc/meminfo 中未找到 MemAvailable 字段".to_string());
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        Err("当前平台不支持可用物理内存探测".to_string())
    }
}

/// 计算冷启动门限：
/// COLD_LOAD_FLOOR = max(peak_rss_bytes * 1.2, HOT_INFER_FLOOR + estimated_resident_bytes)
pub fn compute_cold_load_floor(peak_rss_bytes: u64, estimated_resident_bytes: u64) -> u64 {
    let scaled_peak = (peak_rss_bytes as f64 * 1.2) as u64;
    let hot_with_model = HOT_INFER_FLOOR + estimated_resident_bytes;
    std::cmp::max(scaled_peak, hot_with_model)
}

/// 冷载入前内存预检
pub fn preflight_cold_load(
    peak_rss_bytes: u64,
    estimated_resident_bytes: u64,
) -> Result<u64, MemoryGateError> {
    let available = get_available_physical_memory_bytes().map_err(MemoryGateError::ProbeFailed)?;
    let floor = compute_cold_load_floor(peak_rss_bytes, estimated_resident_bytes);
    if available < floor {
        Err(MemoryGateError::Insufficient { available, floor })
    } else {
        Ok(available)
    }
}

/// 热态推理前内存预检
pub fn preflight_hot_infer() -> Result<u64, MemoryGateError> {
    let available = get_available_physical_memory_bytes().map_err(MemoryGateError::ProbeFailed)?;
    if available < HOT_INFER_FLOOR {
        Err(MemoryGateError::Insufficient {
            available,
            floor: HOT_INFER_FLOOR,
        })
    } else {
        Ok(available)
    }
}
