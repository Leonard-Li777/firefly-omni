use std::sync::atomic::{AtomicU64, Ordering};

/// 热态推理安全门限 (512MB)
pub const HOT_INFER_FLOOR: u64 = 512 * 1024 * 1024;

/// 测试注入的可用内存（若不为 0 则优先返回）
static MOCK_AVAILABLE_MEMORY: AtomicU64 = AtomicU64::new(0);

/// 注入测试用可用内存
pub fn set_mock_available_memory_bytes(bytes: Option<u64>) {
    MOCK_AVAILABLE_MEMORY.store(bytes.unwrap_or(0), Ordering::SeqCst);
}

/// 查询系统当前可用物理内存字节数
pub fn get_available_physical_memory_bytes() -> u64 {
    let mock = MOCK_AVAILABLE_MEMORY.load(Ordering::SeqCst);
    if mock > 0 {
        return mock;
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
                return mem_status.ullAvailPhys;
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(content) = std::fs::read_to_string("/proc/meminfo") {
            for line in content.lines() {
                if line.starts_with("MemAvailable:") {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2 {
                        if let Ok(kb) = parts[1].parse::<u64>() {
                            return kb * 1024;
                        }
                    }
                }
            }
        }
    }

    // 保底兜底：返回 4GB
    4 * 1024 * 1024 * 1024
}

/// 计算冷启动门限：
/// COLD_LOAD_FLOOR = max(peak_rss_bytes * 1.2, HOT_INFER_FLOOR + estimated_resident_bytes)
pub fn compute_cold_load_floor(peak_rss_bytes: u64, estimated_resident_bytes: u64) -> u64 {
    let scaled_peak = (peak_rss_bytes as f64 * 1.2) as u64;
    let hot_with_model = HOT_INFER_FLOOR + estimated_resident_bytes;
    std::cmp::max(scaled_peak, hot_with_model)
}

/// 冷载入前内存预检
pub fn preflight_cold_load(peak_rss_bytes: u64, estimated_resident_bytes: u64) -> Result<u64, (u64, u64)> {
    let available = get_available_physical_memory_bytes();
    let floor = compute_cold_load_floor(peak_rss_bytes, estimated_resident_bytes);
    if available < floor {
        Err((available, floor))
    } else {
        Ok(available)
    }
}

/// 热态推理前内存预检
pub fn preflight_hot_infer() -> Result<u64, (u64, u64)> {
    let available = get_available_physical_memory_bytes();
    if available < HOT_INFER_FLOOR {
        Err((available, HOT_INFER_FLOOR))
    } else {
        Ok(available)
    }
}
