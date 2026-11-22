use axum::response::sse::Event;
use futures_util::stream::{self, Stream};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use tokio::net::TcpListener;
use tokio::sync::broadcast::error::RecvError;

/// Omni 服务默认端口段起始（与 `@firefly/shared` APP_PORTS.OMNI_SERVER 对齐）
pub const OMNI_DEFAULT_PORT_START: u16 = 38200;
/// Omni 服务默认端口段结束（含端点，最多尝试 20 个端口）
pub const OMNI_DEFAULT_PORT_END: u16 = 38219;
/// 发现文件名（落于 `<userData>/run/omni-config.json`）
pub const OMNI_DISCOVERY_FILE_NAME: &str = "omni-config.json";
/// 发现文件服务标识
pub const OMNI_SERVICE_ID: &str = "firefly-omni-server";
/// Windows 文件锁指数退避序列（ms），与 TS 侧 ServiceDiscoveryFile 对齐
pub const WINDOWS_LOCK_BACKOFFS_MS: [u64; 5] = [20, 40, 80, 160, 320];

/// Omni SSE 状态事件载荷（跨语言 JSON 契约，camelCase）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OmniStatusEvent {
    /// "starting" | "ok" | "stopping" | "error"
    pub status: String,
    pub pid: u32,
    pub instance_id: String,
    pub revision: u64,
    pub port: u16,
    pub version: String,
    pub profile: String,
    pub mrl_dimension: usize,
    pub user_data_dir: Option<String>,
    pub master_db_path: Option<String>,
    pub target_vectors_dir: Option<String>,
}

/// 实例级服务发现文件 Schema V1（与 TS 侧 ServiceDiscoveryFileV1 对齐）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OmniDiscoveryFileV1 {
    pub schema_version: u32,
    pub service: String,
    pub instance_id: String,
    pub revision: u64,
    pub host: String,
    pub port: u16,
    pub base_url: String,
    pub pid: u32,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_data_dir: Option<String>,
    pub capabilities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub master_db_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_vectors_dir: Option<String>,
}

/// 生成 RFC 4122 标准 UUID v4（零额外 crate 依赖，基于 workspace rand）
pub fn generate_uuid_v4() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}

/// `bind-first` 顺序原子端口绑定（彻底消灭探测-绑定 TOCTOU 竞态）：
/// - 若目标端口落在默认段 `38200..=38219`，从 `addr.port()` 起顺延至 `38219` 逐个 `TcpListener::bind`；
/// - 若显式传入非默认端口（含 `0`），单点绑定不滑动。
pub async fn bind_first_listener(addr: SocketAddr) -> anyhow::Result<(TcpListener, u16)> {
    let start_port = addr.port();
    if (OMNI_DEFAULT_PORT_START..=OMNI_DEFAULT_PORT_END).contains(&start_port) {
        let mut last_err: Option<std::io::Error> = None;
        for port in start_port..=OMNI_DEFAULT_PORT_END {
            let candidate = SocketAddr::new(addr.ip(), port);
            match TcpListener::bind(candidate).await {
                Ok(listener) => {
                    let actual_port = listener.local_addr()?.port();
                    return Ok((listener, actual_port));
                }
                Err(err) => {
                    tracing::debug!(
                        "[OmniServer] bind-first 端口 {} 被占用或绑定失败 ({})，尝试下一候选端口",
                        candidate,
                        err
                    );
                    last_err = Some(err);
                }
            }
        }
        let err = last_err.unwrap_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                format!("No available port in {}..={}", start_port, OMNI_DEFAULT_PORT_END),
            )
        });
        return Err(anyhow::anyhow!(
            "firefly-omni bind-first failed across {}..={}: {}",
            start_port,
            OMNI_DEFAULT_PORT_END,
            err
        ));
    }

    let listener = TcpListener::bind(addr).await?;
    let actual_port = listener.local_addr()?.port();
    Ok((listener, actual_port))
}

/// 解析服务发现文件根目录（`<userData>`）：
/// 优先级：CLI `--discovery-dir` > 环境变量 `FIREFLY_USER_DATA_DIR` > `--db-path` 父目录
pub fn resolve_discovery_user_data_dir(
    discovery_dir: Option<&PathBuf>,
    db_path: Option<&PathBuf>,
) -> Option<PathBuf> {
    if let Some(dir) = discovery_dir {
        if !dir.as_os_str().is_empty() {
            return Some(dir.clone());
        }
    }
    if let Ok(env_dir) = std::env::var("FIREFLY_USER_DATA_DIR") {
        let trimmed = env_dir.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }
    db_path
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
}

/// 向 stdout 输出 `<1ms` 冷启动快路径通知并立刻 flush
pub fn emit_firefly_ready_stdout(port: u16, pid: u32, instance_id: &str) {
    let payload = serde_json::json!({
        "port": port,
        "pid": pid,
        "instanceId": instance_id,
    });
    println!("FIREFLY_READY:{}", payload);
    let _ = std::io::stdout().flush();
}

/// 带 Windows 文件锁退避 `[20, 40, 80, 160, 320]ms` 原子写出 `<userData>/run/omni-config.json`
pub fn write_discovery_file(
    user_data_dir: &Path,
    payload: &OmniDiscoveryFileV1,
) -> std::io::Result<PathBuf> {
    let run_dir = user_data_dir.join("run");
    std::fs::create_dir_all(&run_dir)?;

    let target_path = run_dir.join(OMNI_DISCOVERY_FILE_NAME);
    let tmp_path = run_dir.join(format!("{}.tmp.{}", OMNI_DISCOVERY_FILE_NAME, payload.pid));
    let serialized = serde_json::to_string_pretty(payload)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    std::fs::write(&tmp_path, serialized.as_bytes())?;

    let mut last_err: Option<std::io::Error> = None;
    for delay_ms in WINDOWS_LOCK_BACKOFFS_MS {
        match std::fs::rename(&tmp_path, &target_path) {
            Ok(()) => return Ok(target_path),
            Err(err) => {
                let retryable = matches!(
                    err.kind(),
                    std::io::ErrorKind::PermissionDenied
                        | std::io::ErrorKind::AlreadyExists
                        | std::io::ErrorKind::WouldBlock
                ) || matches!(err.raw_os_error(), Some(5) | Some(32) | Some(33));
                last_err = Some(err);
                if retryable {
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                } else {
                    break;
                }
            }
        }
    }

    let _ = std::fs::remove_file(&tmp_path);
    Err(last_err.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::Other,
            "failed to rename omni-config.json after backoff retries",
        )
    }))
}

/// 优雅退出时按 `pid`（或 `instanceId`）安全自删 `<userData>/run/omni-config.json`
pub fn revoke_discovery_file(
    user_data_dir: &Path,
    expected_pid: u32,
    expected_instance_id: Option<&str>,
) -> std::io::Result<bool> {
    let target_path = user_data_dir.join("run").join(OMNI_DISCOVERY_FILE_NAME);
    if !target_path.exists() {
        return Ok(false);
    }

    let content = match std::fs::read_to_string(&target_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };

    let should_delete = match serde_json::from_str::<serde_json::Value>(&content) {
        Ok(val) => {
            let pid_matches = val
                .get("pid")
                .and_then(|v| v.as_u64())
                .map(|p| p == expected_pid as u64)
                .unwrap_or(false);
            let instance_matches = expected_instance_id
                .and_then(|eid| {
                    val.get("instanceId")
                        .and_then(|v| v.as_str())
                        .map(|fid| fid == eid)
                })
                .unwrap_or(false);
            pid_matches || instance_matches
        }
        Err(_) => false,
    };

    if !should_delete {
        return Ok(false);
    }

    match std::fs::remove_file(&target_path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// 构造 Omni SSE 状态推送流：
/// 1. 严格先 `subscribe()` 再构造 `initial_data` 首帧，杜绝首帧与广播间隙丢帧；
/// 2. `tokio::select!` 并行监听 `rx.recv()` 与 `shutdown_rx.changed()`；
/// 3. `RecvError::Lagged(skipped)` 时立即重取权威快照补发；
/// 4. 停机信号到达时下发 `status: "stopping"` 终帧并返回 `None` 闭合流，使 `axum::serve` `<100ms` 内优雅退出。
pub fn build_omni_status_sse_stream(
    state: crate::AppState,
) -> impl Stream<Item = Result<Event, Infallible>> + Send + 'static {
    // 第一步：先订阅广播与停机信号，再取初始快照
    let rx = state.status_broadcast.subscribe();
    let shutdown_rx = state.shutdown_tx.subscribe();

    struct SseLoopState {
        app_state: crate::AppState,
        rx: tokio::sync::broadcast::Receiver<OmniStatusEvent>,
        shutdown_rx: tokio::sync::watch::Receiver<bool>,
        sent_initial: bool,
        last_sent_revision: u64,
        ended: bool,
    }

    let loop_state = SseLoopState {
        app_state: state,
        rx,
        shutdown_rx,
        sent_initial: false,
        last_sent_revision: 0,
        ended: false,
    };

    stream::unfold(loop_state, |mut st| async move {
        if st.ended {
            return None;
        }

        // 首帧下发逻辑
        if !st.sent_initial {
            st.sent_initial = true;
            if *st.shutdown_rx.borrow() {
                st.ended = true;
                let stopping = st.app_state.build_status_event("stopping");
                let data = serde_json::to_string(&stopping).unwrap_or_else(|_| "{}".to_string());
                return Some((Ok(Event::default().data(data)), st));
            }
            let initial = st.app_state.build_status_event("ok");
            st.last_sent_revision = initial.revision;
            let data = serde_json::to_string(&initial).unwrap_or_else(|_| "{}".to_string());
            return Some((Ok(Event::default().data(data)), st));
        }

        // 后续事件循环
        loop {
            if *st.shutdown_rx.borrow() {
                st.ended = true;
                let stopping = st.app_state.build_status_event("stopping");
                let data = serde_json::to_string(&stopping).unwrap_or_else(|_| "{}".to_string());
                return Some((Ok(Event::default().data(data)), st));
            }

            tokio::select! {
                biased;
                changed = st.shutdown_rx.changed() => {
                    match changed {
                        Ok(()) if !*st.shutdown_rx.borrow() => continue,
                        _ => {
                            st.ended = true;
                            let stopping = st.app_state.build_status_event("stopping");
                            let data = serde_json::to_string(&stopping).unwrap_or_else(|_| "{}".to_string());
                            return Some((Ok(Event::default().data(data)), st));
                        }
                    }
                }
                recv = st.rx.recv() => {
                    match recv {
                        Ok(evt) => {
                            if evt.status == "stopping" {
                                st.ended = true;
                                st.last_sent_revision = st.last_sent_revision.max(evt.revision);
                                let data = serde_json::to_string(&evt).unwrap_or_else(|_| "{}".to_string());
                                return Some((Ok(Event::default().data(data)), st));
                            }
                            if evt.revision <= st.last_sent_revision {
                                continue;
                            }
                            st.last_sent_revision = evt.revision;
                            let data = serde_json::to_string(&evt).unwrap_or_else(|_| "{}".to_string());
                            return Some((Ok(Event::default().data(data)), st));
                        }
                        Err(RecvError::Lagged(skipped)) => {
                            tracing::warn!(
                                "[OmniServer] SSE 订阅者滞后丢弃 {} 帧，立即重取权威快照补发",
                                skipped
                            );
                            let status = if *st.shutdown_rx.borrow() {
                                st.ended = true;
                                "stopping"
                            } else {
                                "ok"
                            };
                            let snapshot = st.app_state.build_status_event(status);
                            st.last_sent_revision = st.last_sent_revision.max(snapshot.revision);
                            let data = serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".to_string());
                            return Some((Ok(Event::default().data(data)), st));
                        }
                        Err(RecvError::Closed) => {
                            return None;
                        }
                    }
                }
            }
        }
    })
}

/// 构造 `OmniDiscoveryFileV1` 快照对象
pub fn build_discovery_payload_from_state(state: &crate::AppState) -> OmniDiscoveryFileV1 {
    let revision = state.revision.load(Ordering::SeqCst);
    let master_db_path = state
        .master_db_path
        .lock()
        .ok()
        .and_then(|p| p.clone())
        .map(|p| p.to_string_lossy().to_string());
    let target_vectors_dir = state
        .target_vectors_dir
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
        .or_else(|| Some(state.vector.dir().to_string_lossy().to_string()));
    let user_data_dir = state
        .user_data_dir
        .as_ref()
        .map(|p| p.to_string_lossy().to_string());

    OmniDiscoveryFileV1 {
        schema_version: 1,
        service: OMNI_SERVICE_ID.to_string(),
        instance_id: state.instance_id.clone(),
        revision,
        host: "127.0.0.1".to_string(),
        port: state.actual_port,
        base_url: format!("http://127.0.0.1:{}", state.actual_port),
        pid: std::process::id(),
        started_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        user_data_dir,
        capabilities: Vec::new(),
        master_db_path,
        target_vectors_dir,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::atomic::AtomicU64;
    use std::sync::{Arc, Mutex};

    fn make_test_state(port: u16, user_data_dir: Option<PathBuf>) -> crate::AppState {
        let (status_broadcast, _) = tokio::sync::broadcast::channel(32);
        let (shutdown_tx, _) = tokio::sync::watch::channel(false);
        crate::AppState {
            config: Arc::new(Mutex::new(omni_core::OmniConfig::default())),
            geo: Arc::new(omni_pro::geo::GeoService::unavailable()),
            hownet: Arc::new(omni_pro::hownet::OmniHowNetService::unavailable()),
            search: Arc::new(omni_pro::search::OmniSearchService::default()),
            omw: crate::OmwDb::unavailable(),
            vector: Arc::new(crate::VectorEngine::in_memory()),
            master_db_path: Arc::new(Mutex::new(Some(PathBuf::from(
                r"C:\Users\tester\AppData\Roaming\firefly\firefly-ai-folder.db",
            )))),
            target_vectors_dir: Some(PathBuf::from(
                r"C:\Users\tester\AppData\Roaming\firefly\vectors",
            )),
            dimension_policies: Arc::new(std::sync::RwLock::new(
                omni_core::get_default_dimension_policies(),
            )),
            sherpa: Arc::new(crate::SherpaManager::new()),
            moss: Arc::new(crate::MossTtsManager::new_deferred()),
            m5: Arc::new(crate::M5TtsManager::new_deferred()),
            anchor_cache: Arc::new(omni_core::anchor_cache::AnchorCache::default()),
            cpu_slot: Arc::new(crate::cpu_engine::CpuSlot::new(None, None)),
            instance_id: "f47ac10b-58cc-4372-a567-0e02b2c3d479".to_string(),
            revision: Arc::new(AtomicU64::new(1)),
            actual_port: port,
            user_data_dir,
            status_broadcast,
            shutdown_tx,
        }
    }

    /// AC-8 跨语言 JSON 契约向量：验证 Rust 侧 camelCase 序列化与反序列化与 TS 侧 100% 对齐
    #[test]
    fn test_omni_status_event_cross_language_json_contract() {
        let contract_json = r#"{
            "status": "ok",
            "pid": 42424,
            "instanceId": "f47ac10b-58cc-4372-a567-0e02b2c3d479",
            "revision": 7,
            "port": 38201,
            "version": "1.2.3",
            "profile": "gemma_unified",
            "mrlDimension": 512,
            "userDataDir": "C:\\Users\\tester\\AppData\\Roaming\\firefly",
            "masterDbPath": "C:\\Users\\tester\\AppData\\Roaming\\firefly\\firefly-ai-folder.db",
            "targetVectorsDir": "C:\\Users\\tester\\AppData\\Roaming\\firefly\\vectors"
        }"#;

        let parsed: OmniStatusEvent =
            serde_json::from_str(contract_json).expect("应能无损反序列化跨语言契约向量");
        assert_eq!(parsed.status, "ok");
        assert_eq!(parsed.pid, 42424);
        assert_eq!(parsed.instance_id, "f47ac10b-58cc-4372-a567-0e02b2c3d479");
        assert_eq!(parsed.revision, 7);
        assert_eq!(parsed.port, 38201);
        assert_eq!(parsed.version, "1.2.3");
        assert_eq!(parsed.profile, "gemma_unified");
        assert_eq!(parsed.mrl_dimension, 512);
        assert_eq!(
            parsed.user_data_dir.as_deref(),
            Some(r"C:\Users\tester\AppData\Roaming\firefly")
        );
        assert_eq!(
            parsed.master_db_path.as_deref(),
            Some(r"C:\Users\tester\AppData\Roaming\firefly\firefly-ai-folder.db")
        );
        assert_eq!(
            parsed.target_vectors_dir.as_deref(),
            Some(r"C:\Users\tester\AppData\Roaming\firefly\vectors")
        );

        let re_encoded: serde_json::Value = serde_json::to_value(&parsed).unwrap();
        assert_eq!(re_encoded["instanceId"], "f47ac10b-58cc-4372-a567-0e02b2c3d479");
        assert_eq!(re_encoded["mrlDimension"], 512);
        assert_eq!(re_encoded["userDataDir"], r"C:\Users\tester\AppData\Roaming\firefly");
        assert_eq!(
            re_encoded["masterDbPath"],
            r"C:\Users\tester\AppData\Roaming\firefly\firefly-ai-folder.db"
        );
        assert_eq!(
            re_encoded["targetVectorsDir"],
            r"C:\Users\tester\AppData\Roaming\firefly\vectors"
        );
    }

    #[test]
    fn test_generate_uuid_v4_format() {
        let id1 = generate_uuid_v4();
        let id2 = generate_uuid_v4();
        assert_ne!(id1, id2);
        assert_eq!(id1.len(), 36);
        let parts: Vec<&str> = id1.split('-').collect();
        assert_eq!(parts.len(), 5);
        assert!(parts[2].starts_with('4'));
        let variant_char = parts[3].chars().next().unwrap();
        assert!(matches!(variant_char, '8' | '9' | 'a' | 'b'));
    }

    #[tokio::test]
    async fn test_bind_first_listener_slides_on_default_range_collision() {
        // 先在 38215 占位（避开日常开发常用的 38200），验证 bind-first 自动顺延至 38216..=38219
        let occupied = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 38215))
            .await
            .expect("测试占位 38215 应成功");
        let requested = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 38215);
        let (listener, actual_port) = bind_first_listener(requested)
            .await
            .expect("默认段冲突时应自动顺延绑定");
        assert!(
            actual_port > 38215 && actual_port <= OMNI_DEFAULT_PORT_END,
            "期望顺延端口在 38216..=38219，实际={actual_port}"
        );
        drop(listener);
        drop(occupied);
    }

    #[tokio::test]
    async fn test_bind_first_listener_does_not_slide_for_explicit_non_default_port() {
        // 申请 OS 随机端口后占用之，再以该非默认端口请求 bind_first_listener，必须报错而不滑动
        let occupied = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .await
            .unwrap();
        let occupied_addr = occupied.local_addr().unwrap();
        assert!(!(OMNI_DEFAULT_PORT_START..=OMNI_DEFAULT_PORT_END).contains(&occupied_addr.port()));

        let res = bind_first_listener(occupied_addr).await;
        assert!(res.is_err(), "显式非默认端口被占时严禁滑动");
        drop(occupied);
    }

    #[test]
    fn test_write_and_revoke_discovery_file_respects_pid() {
        let temp_dir = tempfile::tempdir().unwrap();
        let payload = OmniDiscoveryFileV1 {
            schema_version: 1,
            service: OMNI_SERVICE_ID.to_string(),
            instance_id: "inst-abc".to_string(),
            revision: 1,
            host: "127.0.0.1".to_string(),
            port: 38202,
            base_url: "http://127.0.0.1:38202".to_string(),
            pid: 99999,
            started_at: "2026-11-22T00:00:00.000Z".to_string(),
            user_data_dir: Some(temp_dir.path().to_string_lossy().to_string()),
            capabilities: vec![],
            master_db_path: Some(r"C:\db\firefly-ai-folder.db".to_string()),
            target_vectors_dir: Some(r"C:\db\vectors".to_string()),
        };

        let written = write_discovery_file(temp_dir.path(), &payload).unwrap();
        assert!(written.ends_with(Path::new("run").join(OMNI_DISCOVERY_FILE_NAME)));
        assert!(written.exists());

        // 不同 PID 与不同 instanceId 严禁误删
        let skipped = revoke_discovery_file(temp_dir.path(), 11111, Some("other-inst")).unwrap();
        assert!(!skipped);
        assert!(written.exists());

        // 匹配 PID 后正确自删
        let deleted = revoke_discovery_file(temp_dir.path(), 99999, Some("other-inst")).unwrap();
        assert!(deleted);
        assert!(!written.exists());
    }

    #[tokio::test]
    async fn test_sse_stream_subscribe_before_snapshot_lagged_recovery_and_shutdown() {
        let temp_dir = tempfile::tempdir().unwrap();
        let state = make_test_state(38205, Some(temp_dir.path().to_path_buf()));

        let stream = build_omni_status_sse_stream(state.clone());
        futures_util::pin_mut!(stream);

        // 1. 首帧应为当前权威快照 (revision = 1, status = "ok")
        let first = stream.next().await.expect("应收到首帧").unwrap();
        let first_debug = format!("{:?}", first);
        assert!(first_debug.contains("status") && first_debug.contains("ok"), "首帧应为 ok: {first_debug}");
        assert!(first_debug.contains("revision") && first_debug.contains("1"), "首帧 revision 应为 1: {first_debug}");

        // 2. 连续发送 40 次更新触发容量 32 广播通道的 Lagged 恢复
        for _ in 0..40 {
            state.publish_status_event("ok");
        }
        let lagged_recovered = stream.next().await.expect("Lagged 后应立即补发最新快照").unwrap();
        let lagged_debug = format!("{:?}", lagged_recovered);
        assert!(
            lagged_debug.contains("revision") && lagged_debug.contains("41"),
            "Lagged 恢复后应补发最新权威 revision=41: {lagged_debug}"
        );

        // 3. 触发停机信号：应下发 status="stopping" 终帧并立即闭合流（返回 None）
        state.trigger_shutdown();
        let stopping_frame = stream.next().await.expect("停机时应收到 stopping 终帧").unwrap();
        let stopping_debug = format!("{:?}", stopping_frame);
        assert!(
            stopping_debug.contains("status") && stopping_debug.contains("stopping"),
            "停机终帧 status 应为 stopping: {stopping_debug}"
        );

        let closed = tokio::time::timeout(std::time::Duration::from_millis(200), stream.next())
            .await
            .expect("停机终帧后 SSE 流必须在 200ms 内闭合");
        assert!(closed.is_none(), "停机终帧后流必须返回 None");
    }
}
