use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Query, State},
    http::{header, HeaderValue, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use omni_core::{
    DuplicateFixRequest, DuplicateFixResponse, DuplicateScanRequest, DuplicateScanResponse,
    OmniConfig, OmniExtractionResult,
};
use omni_extract::OmniExtractor;
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::info;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Mutex<OmniConfig>>,
    /// 离线反向地理编码服务（数据集缺失或开源存根时为软不可用实例）
    pub geo: Arc<omni_pro::geo::GeoService>,
}

#[derive(Deserialize)]
pub struct ExtractRequest {
    pub file_path: String,
}

#[derive(Deserialize)]
pub struct FilePreviewRequest {
    pub path: String,
}

/// 反向地理编码请求体: POST /api/geo/reverse
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeoReverseRequest {
    /// 待解析坐标点列表（结果按下标对齐回传）
    pub points: Vec<omni_pro::geo::GeoQueryPoint>,
    /// BCP-47 语言标签（如 zh-CN），缺省 en
    #[serde(default)]
    pub language: Option<String>,
    /// 城市全量层级距离上限（公里，缺省 50）
    #[serde(default)]
    pub max_city_km: Option<f64>,
    /// 最外层检索半径上限（公里，缺省 500，硬上限 2000）
    #[serde(default)]
    pub max_any_km: Option<f64>,
}

pub fn create_app_router(state: AppState) -> Router {
    Router::new()
        .route(
            "/health",
            get(health_handler),
        )
        .route("/api/version", get(version_handler))
        .route("/api/config", get(get_config).post(update_config).put(update_config))
        .route("/api/extract", post(extract_file_handler))
        .route("/api/extract/upload", post(extract_multipart_handler))
        .route("/api/cleanup/scan", post(cleanup_scan_handler))
        .route("/api/cleanup/scan/stream", post(cleanup_scan_stream_handler))
        .route("/api/cleanup/fix", post(cleanup_fix_handler))
        .route("/api/duplicate/scan", post(cleanup_scan_handler))
        .route("/api/duplicate/scan/stream", post(cleanup_scan_stream_handler))
        .route("/api/duplicate/fix", post(cleanup_fix_handler))
        .route("/api/file/preview", get(file_preview_handler))
        .route("/api/cover", get(cover_handler))
        .route("/api/geo/reverse", post(geo_reverse_handler))
        .layer(DefaultBodyLimit::max(500 * 1024 * 1024))
        .with_state(state)
}

/// 版本信息查询: GET /api/version
async fn version_handler() -> Json<serde_json::Value> {
    let is_pro = omni_pro::is_pro_enabled();
    Json(serde_json::json!({
        "status": "ok",
        "server": "firefly-omni",
        "version": env!("CARGO_PKG_VERSION"),
        "isPro": is_pro
    }))
}

/// 健康检查：附 Pro 模块与地理子系统可用性，供前端 UI 与桌面端启动时探测
async fn health_handler(State(state): State<AppState>) -> Json<serde_json::Value> {
    let is_pro = omni_pro::is_pro_enabled();
    let geo_available = if is_pro {
        let geo = state.geo.clone();
        tokio::task::spawn_blocking(move || geo.is_available())
            .await
            .unwrap_or(false)
    } else {
        false
    };
    Json(serde_json::json!({
        "status": "ok",
        "server": "firefly-omni",
        "version": env!("CARGO_PKG_VERSION"),
        "isPro": is_pro,
        "geoAvailable": geo_available,
        "cleanupAvailable": is_pro
    }))
}

/// 离线反向地理编码: POST /api/geo/reverse（数据缺失或开源模式返回 200 + available:false 软失败）
async fn geo_reverse_handler(
    State(state): State<AppState>,
    Json(req): Json<GeoReverseRequest>,
) -> Json<omni_pro::geo::ReverseOutcome> {
    let geo = state.geo.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        geo.reverse(
            &req.points,
            req.language.as_deref(),
            req.max_city_km,
            req.max_any_km,
        )
    })
    .await
    .unwrap_or_else(|err| omni_pro::geo::ReverseOutcome {
        available: false,
        dataset_version: None,
        results: None,
        reason: Some(format!("地理查询任务执行失败: {err}")),
    });
    Json(outcome)
}

/// 根据文件扩展名推断浏览器可直接预览的多模态 MIME 类型（仅允许图片/视频/音频）
fn preview_mime_from_path(path: &str) -> Option<&'static str> {
    let lower = path.to_lowercase();
    if lower.ends_with(".png") {
        Some("image/png")
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        Some("image/jpeg")
    } else if lower.ends_with(".gif") {
        Some("image/gif")
    } else if lower.ends_with(".webp") {
        Some("image/webp")
    } else if lower.ends_with(".bmp") {
        Some("image/bmp")
    } else if lower.ends_with(".svg") {
        Some("image/svg+xml")
    } else if lower.ends_with(".avif") {
        Some("image/avif")
    } else if lower.ends_with(".mp4") || lower.ends_with(".m4v") {
        Some("video/mp4")
    } else if lower.ends_with(".webm") {
        Some("video/webm")
    } else if lower.ends_with(".ogv") || lower.ends_with(".ogg") {
        Some("video/ogg")
    } else if lower.ends_with(".mov") {
        Some("video/quicktime")
    } else if lower.ends_with(".mp3") {
        Some("audio/mpeg")
    } else if lower.ends_with(".wav") {
        Some("audio/wav")
    } else if lower.ends_with(".flac") {
        Some("audio/flac")
    } else if lower.ends_with(".aac") {
        Some("audio/aac")
    } else if lower.ends_with(".m4a") {
        Some("audio/mp4")
    } else if lower.ends_with(".opus") {
        Some("audio/opus")
    } else {
        None
    }
}

/// 本地多模态文件预览接口: GET /api/file/preview?path=<urlencoded 绝对路径>
/// 仅允许读取浏览器可直接渲染的图片/视频/音频文件，返回对应 Content-Type 字节流。
async fn file_preview_handler(Query(req): Query<FilePreviewRequest>) -> Response {
    let Some(mime) = preview_mime_from_path(&req.path) else {
        return (
            StatusCode::BAD_REQUEST,
            "Unsupported preview file type (仅支持图片/视频/音频)",
        )
            .into_response();
    };

    let path = PathBuf::from(&req.path);
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let body = Body::from(bytes);
            let mut resp = Response::new(body);
            resp.headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
            resp
        }
        Err(err) => {
            tracing::error!("File preview read failed: {} ({})", path.display(), err);
            (StatusCode::NOT_FOUND, "File not found").into_response()
        }
    }
}

/// 通用文件封面截取接口: GET /api/cover?path=<urlencoded 绝对路径>
/// 支持 PDF、PSD、视频（MP4/MOV/AVI/MKV）等格式的高清封面截取（WebP 格式返回）
/// 对于 Office 格式，仅在 enable_office_cover = true 时提取，未开启或暂不支持的格式返回 204 No Content
async fn cover_handler(
    State(state): State<AppState>,
    Query(req): Query<FilePreviewRequest>,
) -> Response {
    let path = PathBuf::from(&req.path);

    // 检查是否开启了 Office 完整封面截图选项 (LibreOffice)
    let enable_office_cover = state.config.lock().map(|c| c.enable_office_cover).unwrap_or(false);

    // 交由 CoverRenderer 按扩展名路由：
    // 对于 Office 文档，内部会默认先解压提取压缩包中的首张图；无图时仅在 enable_office_cover = true 时才执行 LO 渲染
    let outcome = tokio::task::spawn_blocking(move || {
        omni_pro::CoverRenderer::render_cover_with_options(&path, enable_office_cover)
    })
    .await;

    match outcome {
        Ok(Ok(bytes)) => {
            let body = Body::from(bytes);
            let mut resp = Response::new(body);
            // CoverRenderer 统一返回 WebP 格式
            resp.headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/webp"));
            resp
        }
        Ok(Err(err)) => {
            // 不支持的格式或渲染失败 → 204 静默降级
            tracing::warn!("Cover rendering failed or skipped for {}: {}", req.path, err);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(err) => {
            tracing::error!("Cover rendering task panicked: {}", err);
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal task error").into_response()
        }
    }
}

fn get_config_file_path() -> PathBuf {
    if let Ok(appdata) = std::env::var("APPDATA") {
        let dir = PathBuf::from(appdata).join("firefly-ai-folder");
        let _ = std::fs::create_dir_all(&dir);
        dir.join("omni_config.json")
    } else {
        std::env::temp_dir().join("omni_config.json")
    }
}

fn load_config_from_disk() -> OmniConfig {
    let path = get_config_file_path();
    if path.exists() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(cfg) = serde_json::from_str::<OmniConfig>(&content) {
                return cfg;
            }
        }
    }
    OmniConfig::default()
}

fn save_config_to_disk(cfg: &OmniConfig) {
    let path = get_config_file_path();
    if let Ok(json) = serde_json::to_string_pretty(cfg) {
        let _ = std::fs::write(path, json);
    }
}

pub async fn start_server(addr: SocketAddr) -> anyhow::Result<()> {
    let initial_config = load_config_from_disk();
    // 地理数据集发现链：环境变量 → exe 相对目录 → cwd 候选；落空或开源存根时软不可用
    let geo = match omni_pro::geo::discover_dataset_path() {
        Some(path) => {
            info!("omni-geo dataset found at {}", path.display());
            Arc::new(omni_pro::geo::GeoService::from_path(path))
        }
        None => {
            info!("omni-geo dataset not found or open-core stub mode, geo subsystem starts unavailable");
            Arc::new(omni_pro::geo::GeoService::unavailable())
        }
    };
    let state = AppState {
        config: Arc::new(Mutex::new(initial_config)),
        geo,
    };

    // 启动即后台预热地理索引：避免首次用户查询承担秒级冷加载成本
    {
        let geo = state.geo.clone();
        tokio::spawn(async move {
            let start = std::time::Instant::now();
            let available = tokio::task::spawn_blocking(move || geo.is_available())
                .await
                .unwrap_or(false);
            info!(
                "omni-geo dataset pre-warm finished in {:.2}s (available: {})",
                start.elapsed().as_secs_f32(),
                available
            );
        });
    }

    let app = create_app_router(state);

    info!("firefly-omni Axum HTTP server v{} starting on {}", env!("CARGO_PKG_VERSION"), addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            // 当由 Electron / Node.js 宿主拉起时，stdin 管道随宿主退出而关闭 (EOF)
            // 监听 stdin EOF 或终止信号，确保宿主崩溃或强制关闭时 omni-server 立即退出不残留
            use tokio::io::AsyncReadExt;
            let mut stdin = tokio::io::stdin();
            let mut buf = [0u8; 1];
            tokio::select! {
                _ = stdin.read(&mut buf) => {
                    tracing::info!("firefly-omni stdin closed (host terminated), shutting down gracefully.");
                }
                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("firefly-omni received Ctrl+C, shutting down gracefully.");
                }
            }
        })
        .await?;
    Ok(())
}

async fn get_config(
    State(state): State<AppState>,
) -> Json<OmniConfig> {
    let cfg = state.config.lock().unwrap().clone();
    Json(cfg)
}

async fn update_config(
    State(state): State<AppState>,
    Json(new_config): Json<OmniConfig>,
) -> Json<OmniConfig> {
    let mut cfg = state.config.lock().unwrap();
    *cfg = new_config.clone();
    save_config_to_disk(&new_config);
    Json(new_config)
}

/// 处理本地 JSON 文件路径提取请求: POST /api/extract { "file_path": "/path/to/file" }
async fn extract_file_handler(
    State(state): State<AppState>,
    Json(req): Json<ExtractRequest>,
) -> Json<OmniExtractionResult> {
    let cfg = state.config.lock().unwrap().clone();
    let t_start = std::time::Instant::now();
    let res = match OmniExtractor::extract(&req.file_path, &cfg).await {
        Ok(res) => {
            let cost_ms = t_start.elapsed().as_millis();
            tracing::info!(
                "[OmniServer] 提取成功: file={}, enable_image_ocr={}, 耗时={}ms",
                req.file_path,
                cfg.enable_image_ocr,
                cost_ms
            );
            Json(res)
        }
        Err(err) => {
            let cost_ms = t_start.elapsed().as_millis();
            tracing::error!(
                "[OmniServer] 提取失败: file={}, 错误={}, 耗时={}ms",
                req.file_path,
                err,
                cost_ms
            );
            Json(OmniExtractionResult {
                file_path: req.file_path,
                mime_type: "application/octet-stream".to_string(),
                file_size: 0,
                markdown_content: format!("Error: Extraction failed - {}", err),
                metadata: serde_json::json!({}),
                phash: None,
                is_corrupted: true,
                benchmark: None,
            })
        }
    };
    res
}

/// 处理 Web UI 前端拖拽文件二进制流上传请求: POST /api/extract/upload
async fn extract_multipart_handler(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Json<OmniExtractionResult> {
    let cfg = state.config.lock().unwrap().clone();

    loop {
        match multipart.next_field().await {
            Ok(Some(field)) => {
                let file_name = field.file_name().unwrap_or("omni_upload.tmp").to_string();
                if let Ok(bytes) = field.bytes().await {
                    let temp_dir = std::env::temp_dir();
                    let temp_path = temp_dir.join(&file_name);
                    if std::fs::write(&temp_path, &bytes).is_ok() {
                        let path_str = temp_path.to_string_lossy().to_string();
                        if let Ok(mut res) = OmniExtractor::extract(&path_str, &cfg).await {
                            res.file_path = file_name;
                            let _ = std::fs::remove_file(&temp_path);
                            return Json(res);
                        }
                        let _ = std::fs::remove_file(&temp_path);
                    }
                }
            }
            Ok(None) => break,
            Err(err) => {
                tracing::error!("Axum Multipart parsing failed: {:?}", err);
                break;
            }
        }
    }

    Json(OmniExtractionResult {
        file_path: "unknown".to_string(),
        mime_type: "application/octet-stream".to_string(),
        file_size: 0,
        markdown_content: "Error: Multipart file upload extraction failed".to_string(),
        metadata: serde_json::json!({}),
        phash: None,
        is_corrupted: true,
        benchmark: None,
    })
}

/// 处理智能文件清理与去重扫描请求: POST /api/cleanup/scan
async fn cleanup_scan_handler(
    State(_state): State<AppState>,
    Json(req): Json<DuplicateScanRequest>,
) -> Json<DuplicateScanResponse> {
    let stop_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let resp = tokio::task::spawn_blocking(move || {
        omni_pro::cleanup::OmniCleanup::scan(&req, &stop_flag)
    })
    .await
    .unwrap_or_else(|_| DuplicateScanResponse {
        success: false,
        total_scanned: 0,
        duplicate_groups: Vec::new(),
        total_redundant_files: 0,
        total_freed_bytes: 0,
        duration_ms: 0,
    });

    Json(resp)
}

/// 实时 SSE 流式文件清理与去重扫描接口: POST /api/cleanup/scan/stream
pub async fn cleanup_scan_stream_handler(
    State(_state): State<AppState>,
    Json(req): Json<DuplicateScanRequest>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<Event, std::convert::Infallible>>();

    tokio::spawn(async move {
        let _ = tx.send(Ok(Event::default()
            .event("start")
            .data(serde_json::json!({ "status": "streaming" }).to_string())));
        tokio::task::yield_now().await;

        let stop_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let tx_group = tx.clone();
        let tx_prog = tx.clone();

        let resp = tokio::task::spawn_blocking(move || {
            omni_pro::cleanup::OmniCleanup::scan_streaming(
                &req,
                &stop_flag,
                move |group| {
                    let _ = tx_group.send(Ok(Event::default()
                        .event("group")
                        .data(serde_json::to_string(group).unwrap())));
                },
                move |scanned, total, stage| {
                    let _ = tx_prog.send(Ok(Event::default()
                        .event("progress")
                        .data(serde_json::json!({
                            "scanned": scanned,
                            "total_scanned": total,
                            "stage": stage
                        }).to_string())));
                },
            )
        })
        .await
        .unwrap_or_else(|_| DuplicateScanResponse {
            success: false,
            total_scanned: 0,
            duplicate_groups: Vec::new(),
            total_redundant_files: 0,
            total_freed_bytes: 0,
            duration_ms: 0,
        });

        let _ = tx.send(Ok(Event::default()
            .event("done")
            .data(serde_json::json!({
                "total_scanned": resp.total_scanned,
                "total_redundant_files": resp.total_redundant_files,
                "total_freed_bytes": resp.total_freed_bytes,
                "duration_ms": resp.duration_ms
            }).to_string())));
        tokio::task::yield_now().await;
    });

    let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(rx);
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// 处理清理/查重修复请求 (Exif 清理 / 视频转码): POST /api/cleanup/fix 或 POST /api/duplicate/fix
async fn cleanup_fix_handler(
    Json(req): Json<DuplicateFixRequest>,
) -> Json<DuplicateFixResponse> {
    let action = req.action.clone();
    let paths = req.paths.clone();

    let outcome = tokio::task::spawn_blocking(move || {
        omni_pro::cleanup::OmniCleanup::execute_fix(&action, paths)
    })
    .await
    .unwrap_or_else(|err| {
        (0, 0, Vec::new(), vec![format!("任务执行异常: {err}")])
    });

    Json(DuplicateFixResponse {
        success: outcome.1 == 0,
        action: req.action,
        success_count: outcome.0,
        failed_count: outcome.1,
        processed_paths: outcome.2,
        errors: outcome.3,
    })
}


