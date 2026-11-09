use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

/// Inflight 文件结构体
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InflightMarker {
    pub pid: u32,
    pub boot_id: String,
    pub started_at: u64,
    pub epoch: u64,
}

/// 持久化崩溃状态文件结构体
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CrashState {
    pub crash_count: u32,
    pub last_crashed_at: u64,
    #[serde(default)]
    pub was_explicitly_preheated: bool,
}

/// 获取操作系统 Boot ID（整机启动唯一标识）
pub fn get_os_boot_id() -> String {
    #[cfg(target_os = "windows")]
    {
        // Windows 通过 GetTickCount64 获取系统启动至今毫秒数，
        // 算出系统启动的大致 Unix 时间戳作为 boot_id 区分跨重启
        unsafe {
            use windows_sys::Win32::System::SystemInformation::GetTickCount64;
            let uptime_ms = GetTickCount64();
            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let boot_time = now_ms.saturating_sub(uptime_ms);
            // 归一化到秒级
            format!("win-boot-{}", boot_time / 1000)
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(id) = fs::read_to_string("/proc/sys/kernel/random/boot_id") {
            return id.trim().to_string();
        }
        "linux-boot-unknown".to_string()
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        "os-boot-fallback".to_string()
    }
}

/// 检查指定 PID 进程是否存活
pub fn is_pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(target_os = "windows")]
    {
        unsafe {
            use windows_sys::Win32::Foundation::CloseHandle;
            use windows_sys::Win32::System::Threading::{
                GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            };
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            let mut exit_code: u32 = 0;
            let ok = GetExitCodeProcess(handle, &mut exit_code);
            CloseHandle(handle);
            if ok != 0 {
                // STILL_ACTIVE = 259
                exit_code == 259
            } else {
                false
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        // Unix 可以通过 kill(pid, 0) 探测
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
}

/// 获取运行期目录（默认 `{appData}/run`）
pub fn get_default_run_dir() -> PathBuf {
    if let Ok(val) = std::env::var("FIREFLY_RUN_DIR") {
        return PathBuf::from(val);
    }
    #[cfg(target_os = "windows")]
    if let Ok(appdata) = std::env::var("APPDATA") {
        return PathBuf::from(appdata).join("firefly-ai-folder").join("run");
    }
    #[cfg(target_os = "macos")]
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home)
            .join("Library/Application Support/firefly-ai-folder")
            .join("run");
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".firefly-ai-folder").join("run");
    }
    std::env::temp_dir().join("firefly-ai-folder-run")
}

pub struct CircuitBreakerManager {
    run_dir: PathBuf,
}

impl CircuitBreakerManager {
    pub fn new(run_dir: PathBuf) -> Self {
        let _ = fs::create_dir_all(&run_dir);
        Self { run_dir }
    }

    pub fn inflight_path(&self) -> PathBuf {
        self.run_dir.join("cpu-session.inflight")
    }

    pub fn state_path(&self) -> PathBuf {
        self.run_dir.join("cpu-session.state")
    }

    /// 在 Loading 前原子写入 .inflight 标记
    pub fn write_inflight(&self, pid: u32, boot_id: &str, epoch: u64) -> std::io::Result<()> {
        let _ = fs::create_dir_all(&self.run_dir);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let marker = InflightMarker {
            pid,
            boot_id: boot_id.to_string(),
            started_at: now,
            epoch,
        };
        let content = serde_json::to_string_pretty(&marker)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        
        // 原子覆盖写入：tmp -> rename
        let tmp_path = self.run_dir.join(format!("cpu-session.inflight.tmp.{}", std::process::id()));
        fs::write(&tmp_path, content)?;
        fs::rename(tmp_path, self.inflight_path())?;
        Ok(())
    }

    /// 在 Unloaded 或正常关机退出时删除 .inflight
    pub fn remove_inflight(&self) {
        let path = self.inflight_path();
        if path.exists() {
            let _ = fs::remove_file(path);
        }
    }

    /// 读取持久化崩溃状态
    pub fn load_state(&self) -> CrashState {
        let path = self.state_path();
        if path.exists() {
            if let Ok(content) = fs::read_to_string(&path) {
                if let Ok(state) = serde_json::from_str::<CrashState>(&content) {
                    return state;
                }
            }
        }
        CrashState::default()
    }

    /// 保存持久化崩溃状态
    pub fn save_state(&self, state: &CrashState) -> std::io::Result<()> {
        let _ = fs::create_dir_all(&self.run_dir);
        let content = serde_json::to_string_pretty(state)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        let tmp_path = self.run_dir.join(format!("cpu-session.state.tmp.{}", std::process::id()));
        fs::write(&tmp_path, content)?;
        fs::rename(tmp_path, self.state_path())?;
        Ok(())
    }

    /// 进程启动时执行 inflight 交叉验证与自愈对账
    pub fn check_and_recover_on_startup(&self, current_boot_id: &str, current_pid: u32) -> u32 {
        let inflight = self.inflight_path();
        if !inflight.exists() {
            return self.load_state().crash_count;
        }

        if let Ok(content) = fs::read_to_string(&inflight) {
            if let Ok(marker) = serde_json::from_str::<InflightMarker>(&content) {
                // 1. 若 boot_id 与当前 OS boot_id 不一致：判定为跨整机重启陈旧残留，直接删除，不计数
                if marker.boot_id != current_boot_id {
                    info!("发现跨重启的陈旧 .inflight 标记，予以清理，不计入崩溃计数");
                    self.remove_inflight();
                    return self.load_state().crash_count;
                }

                // 2. 若 boot_id 一致且 PID 进程存活（且不是自身）：说明另一实例正在运行，不计数
                if marker.pid != current_pid && is_pid_alive(marker.pid) {
                    warn!("发现存活进程 (PID: {}) 的 .inflight 标记，不计数", marker.pid);
                    return self.load_state().crash_count;
                }

                // 3. 若 boot_id 一致且 PID 进程已消亡（或自身重启）：确认为异常退出暴毙，计数 +1
                warn!(
                    "检测到同周期内底层崩溃导致进程非干净退出 (记录 PID: {}, epoch: {})，递增 crash_count",
                    marker.pid, marker.epoch
                );
                self.remove_inflight();

                let mut state = self.load_state();
                // 二次熔断防穿透保护：若此前曾被显式预热，再次崩溃直接跳至 2
                if state.was_explicitly_preheated {
                    state.crash_count = 2;
                } else {
                    state.crash_count = state.crash_count.saturating_add(1);
                }
                state.last_crashed_at = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let _ = self.save_state(&state);
                return state.crash_count;
            }
        }

        // 文件损坏，清理
        self.remove_inflight();
        self.load_state().crash_count
    }

    /// 用户在 UI 显式点击【预热模型】并确认后，主动清零崩溃计数
    pub fn clear_crash_count_on_preheat(&self) {
        let mut state = self.load_state();
        state.crash_count = 0;
        state.was_explicitly_preheated = true;
        let _ = self.save_state(&state);
        info!("用户显式预热模型：已重置 crash_count 为 0 并置位 was_explicitly_preheated");
    }

    /// 是否处于 CRASH_LOOP 熔断态（连续 2 次非干净退出）
    pub fn is_crash_loop(&self) -> bool {
        self.load_state().crash_count >= 2
    }
}
