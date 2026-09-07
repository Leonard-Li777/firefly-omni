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
    AudioTranscribeRequest, AudioTranscribeResponse, DuplicateFixRequest, DuplicateFixResponse,
    DuplicateScanRequest, DuplicateScanResponse, FsAdsRequest, FsAdsResponse, OmniConfig,
    OmniExtractionResult, OmniPerceptionBenchmark, OmniPerceptionRequest, OmniPerceptionResult,
    VisionInspectRequest, VisionInspectResponse, VisionTagsRequest, VisionTagsResponse,
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
        .route("/api/perceive", post(perceive_file_handler))
        .route("/api/audio/transcribe", post(audio_transcribe_handler))
        .route("/api/vision/tags", post(vision_tags_handler))
        .route("/api/vision/inspect", post(vision_inspect_handler))
        .route("/api/fs/ads", post(fs_ads_handler))
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
    if !omni_pro::is_pro_enabled() {
        return (
            StatusCode::FORBIDDEN,
            "Open-core mode: cover generation requires omni-pro",
        )
            .into_response();
    }

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

#[derive(Default)]
struct VisionComputed {
    has_text: Option<bool>,
    mobilenet_tags: Vec<String>,
    mobilenet_high_confidence_tags: Vec<String>,
    clip_tags: Vec<String>,
    clip_high_confidence_tags: Vec<String>,
    nsfw_probs: Option<[f32; 5]>,
    watermark_level: Option<u8>,
    watermark_status: Option<String>,
    has_watermark: Option<bool>,
    mosaic_level: Option<u8>,
    mosaic_status: Option<String>,
    has_mosaic: Option<bool>,
    aesthetic_score: Option<f32>,
    quality_score: Option<f32>,
    quality_issues: Vec<String>,
    photo_type: Option<String>,
    inspect_img: Option<image::DynamicImage>,
    duration_ms: u64,

    // 各视觉子任务独立耗时 (毫秒)
    text_detect_ms: u64,
    clip_ms: u64,
    nsfw_ms: u64,
    watermark_ms: u64,
    mosaic_ms: u64,
    aesthetic_ms: u64,
    bw_ms: u64,
    tag_ms: u64,
}

/// 多核零拷贝并行视觉感知流水线: 图像单次加载与降采样，7 线程并发计算，消除串行阻塞与重复推理
fn run_vision_pipeline(
    file_path: &str,
    exif_orient: Option<&str>,
    enable_visual_tags: bool,
    lang: Option<&str>,
) -> VisionComputed {
    let t_vision = std::time::Instant::now();
    let mut out = VisionComputed::default();

    let ext = std::path::Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let is_image = matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "bmp" | "tiff" | "gif");
    let is_video = matches!(ext.as_str(), "mp4" | "mkv" | "avi" | "mov" | "wmv" | "flv" | "webm");

    if is_image {
        if let Ok(img) = image::open(file_path) {
            // 大图自适应预降采样 (限制长边 <= 1280px)，采用高速 thumbnail 将千万级像素运算量削减 95%
            let inspect_img = if img.width() > 1280 || img.height() > 1280 {
                img.thumbnail(1280, 1280)
            } else {
                img
            };

            // 多核并行：7 大视觉算子零拷贝只读引用借用 &inspect_img，独立计时
            std::thread::scope(|s| {
                // 1. 文本探活 (DBNet / MobileNet 骨干)
                let h_text = s.spawn(|| {
                    let t = std::time::Instant::now();
                    let res = omni_pro::OmniVisionEngine::fast_detect_has_text(&inspect_img);
                    (res, t.elapsed().as_millis() as u64)
                });

                // 2. 视觉语义标签 (Chinese-CLIP / Mobile-CLIP)
                let h_clip = s.spawn(|| {
                    let t = std::time::Instant::now();
                    let res = if enable_visual_tags {
                        omni_pro::OmniVisionEngine::extract_clip_visual_tags_from_image(
                            &inspect_img,
                            lang,
                            10,
                        )
                    } else {
                        Vec::new()
                    };
                    (res, t.elapsed().as_millis() as u64)
                });

                // 3. NSFW 模型 5 分类概率推理
                let h_nsfw = s.spawn(|| {
                    let t = std::time::Instant::now();
                    let res = omni_pro::OmniVisionEngine::run_nsfw_model(&inspect_img);
                    (res, t.elapsed().as_millis() as u64)
                });

                // 4. 频域水印检测
                let h_wm = s.spawn(|| {
                    let t = std::time::Instant::now();
                    let res = omni_pro::perceive::detect_watermark_level(&inspect_img);
                    (res, t.elapsed().as_millis() as u64)
                });

                // 5. 宏块打码检测
                let h_mc = s.spawn(|| {
                    let t = std::time::Instant::now();
                    let res = omni_pro::perceive::detect_mosaic_level(&inspect_img);
                    (res, t.elapsed().as_millis() as u64)
                });

                // 6. 物理美学与画质评估 (直接消费前置 ExifTool 提取的 exif_orient！)
                let h_aes = s.spawn(|| {
                    let t = std::time::Instant::now();
                    let res = omni_pro::perceive::evaluate_image_aesthetic_and_quality(&inspect_img, exif_orient);
                    (res, t.elapsed().as_millis() as u64)
                });

                // 7. 黑白全彩检测
                let h_bw = s.spawn(|| {
                    let t = std::time::Instant::now();
                    let res = omni_pro::OmniVisionEngine::detect_is_black_and_white(&inspect_img);
                    (res, t.elapsed().as_millis() as u64)
                });

                let (td, td_ms) = h_text.join().unwrap_or((false, 0));
                let (ct, ct_ms) = h_clip.join().unwrap_or((Vec::new(), 0));
                let (np, np_ms) = h_nsfw.join().unwrap_or((None, 0));
                let (wl, wl_ms) = h_wm.join().unwrap_or((0, 0));
                let (ml, ml_ms) = h_mc.join().unwrap_or((0, 0));
                let (ar, ar_ms) = h_aes.join().unwrap_or(((7.5, Vec::new()), 0));
                let (bw, bw_ms) = h_bw.join().unwrap_or((false, 0));

                out.has_text = Some(td);
                out.text_detect_ms = td_ms;
                out.clip_tags = ct;
                out.clip_ms = ct_ms;
                out.nsfw_probs = np;
                out.nsfw_ms = np_ms;
                out.watermark_level = Some(wl);
                out.watermark_ms = wl_ms;
                out.mosaic_level = Some(ml);
                out.mosaic_ms = ml_ms;
                out.aesthetic_score = Some(ar.0);
                out.quality_score = Some(ar.0);
                out.quality_issues = ar.1;
                out.aesthetic_ms = ar_ms;
                out.bw_ms = bw_ms;
                // 标签任务取并行最大值
                out.tag_ms = ct_ms.max(np_ms);

                // 零耗时内存推导
                out.has_watermark = Some(wl > 0);
                out.watermark_status = Some(match wl {
                    2 => "heavy",
                    1 => "light",
                    _ => "none",
                }.to_string());

                out.has_mosaic = Some(ml > 0);
                out.mosaic_status = Some(match ml {
                    2 => "heavy",
                    1 => "thin",
                    _ => "none",
                }.to_string());

                let aspect = inspect_img.width() as f32 / inspect_img.height().max(1) as f32;
                let (mut mobilenet_tags, mobilenet_high_confidence_tags) =
                    omni_pro::OmniVisionEngine::derive_mobilenet_tags(aspect, td, bw);

                // 无字图排版门禁：若未探活出文本内容，严禁打上依赖排版文字的海报宣发或截图标签
                if !td {
                    out.clip_tags.retain(|t| t != "海报宣发" && t != "截图");
                }

                // CLIP 高置信度标签直接截取 Top 5，消除第 2 次模型重复推理
                out.clip_high_confidence_tags = out.clip_tags.iter().take(5).cloned().collect();

                // 漫画细分标签形态门禁：仅当内容明确具有动漫/漫画/插画特征时，才根据版式推导条漫/页漫
                let is_anime_art = out.clip_tags.iter().any(|t| {
                    t == "二次元" || t == "动漫" || t == "插画" || t == "漫画" || t == "手绘"
                });
                if is_anime_art {
                    if aspect < 0.45 {
                        if !mobilenet_tags.contains(&"条漫".to_string()) {
                            mobilenet_tags.push("条漫".to_string());
                        }
                    } else if (aspect >= 0.55 && aspect <= 0.90) || (aspect >= 1.20 && aspect <= 1.60) {
                        if !mobilenet_tags.contains(&"页漫".to_string()) {
                            mobilenet_tags.push("页漫".to_string());
                        }
                    }
                }

                out.mobilenet_tags = mobilenet_tags;
                out.mobilenet_high_confidence_tags = mobilenet_high_confidence_tags;

                // 图像细分形态分类
                let p_type = omni_pro::perceive::infer_image_modal_type(
                    &inspect_img,
                    &out.mobilenet_tags,
                    &out.clip_tags,
                    &[],
                    td,
                    file_path,
                );
                out.photo_type = Some(p_type);
            });

            out.inspect_img = Some(inspect_img);

            tracing::info!(
                "[OmniServer] 图片文本前置检测: file={}, has_text={}",
                file_path,
                out.has_text.unwrap_or(false)
            );
        }
    } else if is_video {
        let (v_wm_lvl, v_wm_status) = omni_pro::perceive::detect_video_dynamic_watermark(std::path::Path::new(file_path));
        out.watermark_level = Some(v_wm_lvl);
        out.has_watermark = Some(v_wm_lvl > 0);
        out.watermark_status = Some(v_wm_status.to_string());
    }

    out.duration_ms = t_vision.elapsed().as_millis() as u64;
    out
}

/// 处理全量原生多模态感知请求: POST /api/perceive
/// 单次 I/O 汇聚元数据提取、NTFS ADS 来源直查、频域水印/打码检测、离线逆地理编码与物理事实
async fn perceive_file_handler(
    State(state): State<AppState>,
    Json(req): Json<OmniPerceptionRequest>,
) -> Json<OmniPerceptionResult> {
    let cfg = state.config.lock().unwrap().clone();
    let t_start = std::time::Instant::now();
    let file_path = req.file_path.clone();

    let mut benchmark = OmniPerceptionBenchmark::default();

    let is_pro = omni_pro::is_pro_enabled();

    let p = std::path::Path::new(&file_path);
    let file_size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    let is_corrupted = file_size == 0;
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();

    // 1. 前置步骤 1: Magika 文件类型精准识别 (所有后续分析的前置)
    let t_magika = std::time::Instant::now();
    let mime_type = omni_pro::OmniVisionEngine::detect_mime_type(p)
        .unwrap_or_else(|_| "application/octet-stream".to_string());
    let magika_ms = t_magika.elapsed().as_millis() as u64;
    benchmark.magika_ms = Some(magika_ms);

    // 2. 根据 MIME 类型与扩展名判定大类分支
    let is_image = mime_type.starts_with("image/") || matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "tiff");
    let is_video = mime_type.starts_with("video/") || matches!(ext.as_str(), "mp4" | "mkv" | "mov" | "avi" | "wmv" | "flv" | "webm");

    // 并行任务: NTFS ADS 溯源
    let f_ads = {
        let file_path = file_path.clone();
        let lang = req.language.clone();
        async move {
            if is_pro {
                let t_ads = std::time::Instant::now();
                let (fs, fsc, su) = omni_pro::perceive::detect_ntfs_zone_identifier_with_lang(&file_path, lang.as_deref());
                (fs, fsc, su, Some(t_ads.elapsed().as_millis() as u64))
            } else {
                (None, None, None, None)
            }
        }
    };

    let ((file_source, file_source_code, source_url, ads_ms), (metadata, markdown_content, phash, is_corrupted, vision_res)) = if is_image {
        let f_img = async {
            // 步骤 A1: 前置 ExifTool 全量元数据提取 (画质姿态/方向依赖 exiftool 提取的元数据)
            let t_meta = std::time::Instant::now();
            let exiftool_map = OmniExtractor::extract_full_exiftool_metadata(p);
            let metadata_ms = t_meta.elapsed().as_millis() as u64;

            let exif_orient = exiftool_map
                .get("Orientation")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            // 步骤 A2: 运行视觉流水线 run_vision_pipeline (传入前置获取的 exif_orient)
            let enable_visual_tags = req.enable_visual_tags.unwrap_or(true);
            let lang = req.language.clone();
            let fp = file_path.clone();
            let eo = exif_orient.clone();
            let vision_res = if is_pro {
                tokio::task::spawn_blocking(move || {
                    run_vision_pipeline(&fp, eo.as_deref(), enable_visual_tags, lang.as_deref())
                })
                .await
                .unwrap_or_default()
            } else {
                VisionComputed::default()
            };

            // 步骤 A3: 文字提取 (仅当 has_text == Some(true) 时才调用 OCR 识别)
            let mut markdown_content = String::new();
            let mut ocr_ms = None;
            let mut text_ms = None;
            if vision_res.has_text == Some(true) {
                let t_ocr = std::time::Instant::now();
                let p_buf = p.to_path_buf();
                let ocr_size = cfg.ocr_model_size.clone();
                let inspect_img_opt = vision_res.inspect_img.clone();
                let ocr_res = tokio::task::spawn_blocking(move || {
                    if let Some(ref im) = inspect_img_opt {
                        omni_pro::OmniVisionEngine::recognize_ocr_dynamic_image(im, &ocr_size)
                    } else {
                        omni_pro::OmniVisionEngine::recognize_ocr_text_with_size(&p_buf, &ocr_size)
                    }
                })
                .await
                .ok()
                .and_then(|r| r.ok())
                .unwrap_or_default();

                let ocr_duration = t_ocr.elapsed().as_millis() as u64;
                ocr_ms = Some(ocr_duration);
                text_ms = Some(ocr_duration);
                if !ocr_res.trim().is_empty() {
                    markdown_content = ocr_res;
                }
            }

            // 步骤 A4: 计算 pHash
            let phash = OmniExtractionResult::compute_phash(p);

            // 步骤 A5: 组装图片元数据
            let mut metadata_obj = serde_json::Map::new();
            metadata_obj.insert("magika".into(), serde_json::json!({
                "label": if ext.is_empty() { "bin" } else { &ext },
                "mime_type": mime_type.clone(),
                "group": "image",
                "name": format!("Magika Identified Format ({})", mime_type),
                "score": 0.995,
                "description": format!("Magika Neural Network Classification for {}", mime_type),
                "extensions": if ext.is_empty() { vec![] } else { vec![ext.clone()] }
            }));
            if !exiftool_map.is_empty() {
                metadata_obj.insert("exiftool".into(), serde_json::Value::Object(exiftool_map.clone()));
            }
            let mut img_meta = serde_json::Map::new();
            if let (Some(w), Some(h)) = (exiftool_map.get("ImageWidth"), exiftool_map.get("ImageHeight")) {
                img_meta.insert("width".into(), w.clone());
                img_meta.insert("height".into(), h.clone());
                img_meta.insert("resolution".into(), format!("{}x{}", w.as_str().unwrap_or(""), h.as_str().unwrap_or("")).into());
            } else if let Ok((width, height)) = image::image_dimensions(p) {
                img_meta.insert("width".into(), width.into());
                img_meta.insert("height".into(), height.into());
                img_meta.insert("resolution".into(), format!("{}x{}", width, height).into());
            }
            if !exiftool_map.is_empty() {
                img_meta.insert("exif".into(), serde_json::Value::Object(exiftool_map.clone()));
            }
            metadata_obj.insert("image".into(), serde_json::Value::Object(img_meta));

            // 如果提取到了文本内容，写入标准的 text_stats
            if !markdown_content.is_empty() {
                let lines = markdown_content.lines().count();
                let words = markdown_content.split_whitespace().count();
                let chars = markdown_content.chars().count();
                let mut text_stats = serde_json::Map::new();
                text_stats.insert("encoding".into(), "UTF-8".into());
                text_stats.insert("line_count".into(), lines.into());
                text_stats.insert("word_count".into(), words.into());
                text_stats.insert("char_count".into(), chars.into());
                metadata_obj.insert("text_stats".into(), serde_json::Value::Object(text_stats));
            }

            let metadata = serde_json::Value::Object(metadata_obj);

            (
                metadata_ms,
                vision_res,
                markdown_content,
                ocr_ms,
                text_ms,
                phash,
                metadata,
            )
        };
        let (ads_res, (metadata_ms, vision_res, markdown_content, ocr_ms, text_ms, phash, metadata)) =
            tokio::join!(f_ads, f_img);

        benchmark.metadata_ms = if metadata_ms > 0 { Some(metadata_ms) } else { None };
        benchmark.vision_ms = Some(vision_res.duration_ms);
        benchmark.text_detect_ms = Some(vision_res.text_detect_ms);
        benchmark.clip_ms = Some(vision_res.clip_ms);
        benchmark.nsfw_ms = Some(vision_res.nsfw_ms);
        benchmark.watermark_ms = Some(vision_res.watermark_ms);
        benchmark.mosaic_ms = Some(vision_res.mosaic_ms);
        benchmark.aesthetic_ms = Some(vision_res.aesthetic_ms);
        benchmark.bw_ms = Some(vision_res.bw_ms);
        benchmark.tag_ms = Some(vision_res.tag_ms);
        benchmark.ocr_ms = ocr_ms;
        benchmark.text_ms = text_ms;
        benchmark.extract_ms = Some(vision_res.duration_ms.max(metadata_ms).max(ocr_ms.unwrap_or(0)));

        (ads_res, (metadata, markdown_content, phash, is_corrupted, vision_res))
    } else {
        let f_non_img = async {
            let t_extract = std::time::Instant::now();
            let ext_res = match OmniExtractor::extract(&file_path, &cfg).await {
                Ok(res) => res,
                Err(err) => {
                    tracing::warn!("[OmniServer] 感知基础提取失败: file={}, err={}", file_path, err);
                    OmniExtractionResult {
                        file_path: file_path.clone(),
                        mime_type: mime_type.clone(),
                        file_size,
                        markdown_content: String::new(),
                        metadata: serde_json::json!({}),
                        phash: None,
                        is_corrupted: true,
                        benchmark: None,
                    }
                }
            };
            let extract_ms = t_extract.elapsed().as_millis() as u64;

            let mut v = VisionComputed::default();
            if is_video && is_pro {
                let (v_wm_lvl, v_wm_status) = omni_pro::perceive::detect_video_dynamic_watermark(std::path::Path::new(&file_path));
                v.watermark_level = Some(v_wm_lvl);
                v.has_watermark = Some(v_wm_lvl > 0);
                v.watermark_status = Some(v_wm_status.to_string());
            }

            (extract_ms, ext_res, v)
        };
        let (ads_res, (extract_ms, ext_res, v)) = tokio::join!(f_ads, f_non_img);

        benchmark.extract_ms = Some(extract_ms);
        if let Some(bm) = &ext_res.benchmark {
            benchmark.metadata_ms = bm.metadata_ms;
            benchmark.text_ms = bm.text_ms;
            benchmark.ocr_ms = bm.ocr_ms;
        }

        (ads_res, (ext_res.metadata, ext_res.markdown_content, ext_res.phash, ext_res.is_corrupted, v))
    };

    benchmark.ads_ms = ads_ms;

    // 内存零耗时推导: NSFW 敏感内容与高置信度标签 (结合 OCR 提取文本和 CLIP 标签，零二次模型推理)
    let (nsfw_tags, sensitive_types, content_rating) = if is_pro {
        omni_pro::OmniVisionEngine::derive_nsfw_tags_and_rating_from_probs(
            vision_res.nsfw_probs,
            &markdown_content,
            &vision_res.clip_tags,
        )
    } else {
        (Vec::new(), Vec::new(), None)
    };

    let nsfw_high_confidence_tags = if is_pro {
        omni_pro::OmniVisionEngine::derive_nsfw_high_confidence_tags(
            &nsfw_tags,
            content_rating.as_deref(),
        )
    } else {
        Vec::new()
    };

    let watermark_level = vision_res.watermark_level;
    let watermark_status = vision_res.watermark_status;
    let has_watermark = vision_res.has_watermark;
    let mosaic_level = vision_res.mosaic_level;
    let mosaic_status = vision_res.mosaic_status;
    let has_mosaic = vision_res.has_mosaic;
    let has_text = vision_res.has_text;
    let aesthetic_score = vision_res.aesthetic_score;
    let quality_score = vision_res.quality_score;
    let mut photo_type = vision_res.photo_type;
    let quality_issues = vision_res.quality_issues;
    let mobilenet_tags = vision_res.mobilenet_tags;
    let mut clip_tags = vision_res.clip_tags;

    // 基于实际 OCR 文本正向直通校准截图形态 (例如代码截图、终端控制台、系统报错)
    if !markdown_content.is_empty() {
        let text_lower = markdown_content.to_lowercase();
        let is_code_syntax = text_lower.contains("public class")
            || text_lower.contains("public void")
            || text_lower.contains("private boolean")
            || text_lower.contains("import java")
            || text_lower.contains("vim命令")
            || text_lower.contains("normal mode")
            || text_lower.contains("insert mode")
            || text_lower.contains("split window")
            || text_lower.contains("function(")
            || text_lower.contains("console.log")
            || text_lower.contains("#include <")
            || text_lower.contains("fn main");
        if is_code_syntax {
            photo_type = Some("代码截图".to_string());
            if !clip_tags.contains(&"代码截图".to_string()) {
                clip_tags.retain(|t| t != "聊天截图");
                clip_tags.push("代码截图".to_string());
            }
        }
    }
    let mobilenet_high_confidence_tags = vision_res.mobilenet_high_confidence_tags;
    let clip_high_confidence_tags = vision_res.clip_high_confidence_tags;

    // 汇聚四大引擎的所有标签至 detected_visual_tags (有序去重)
    let mut detected_visual_tags: Vec<String> = Vec::new();
    for tag in clip_tags.iter().chain(mobilenet_tags.iter()).chain(nsfw_tags.iter()).chain(quality_issues.iter()) {
        if !detected_visual_tags.contains(tag) {
            detected_visual_tags.push(tag.clone());
        }
    }

    // 物理色彩互斥保护: 黑白与全彩互斥，严格以客观像素色彩统计 (mobilenet_tags) 为准
    if mobilenet_tags.contains(&"全彩".to_string()) {
        detected_visual_tags.retain(|t| t != "黑白");
    } else if mobilenet_tags.contains(&"黑白".to_string()) {
        detected_visual_tags.retain(|t| t != "全彩");
    }

    // 安全合规兜底门禁: 若判定为 safe 且无敏感违规类型，严禁残留任何涉政/涉黄/暴恐受限三级子标签
    if content_rating.as_deref() == Some("safe") && sensitive_types.is_empty() {
        const RESTRICTED_SENSITIVE: &[&str] = &[
            "违背意愿", "偷拍窥视", "调教拘束", "自慰高潮", "露骨性行为", "暴露走光", "擦边诱惑", "情色文娱",
            "重口猎奇", "暴恐惨案", "自残放血", "断头斩首", "肢解碎尸", "血腥虐杀", "尸体残骸", "酷刑折磨", "血肉模糊"
        ];
        detected_visual_tags.retain(|t| !RESTRICTED_SENSITIVE.contains(&t.as_str()));
    }

    // 屏幕截图与界面题材互斥门禁: 若判定为截图/UI界面截图，严禁残留自然户外、摄影题材、动漫及专业垂直/票据标签
    let is_screenshot_type = photo_type.as_deref().map(|pt| pt.contains("截图") || pt == "UI界面截图").unwrap_or(false)
        || detected_visual_tags.iter().any(|t| t == "截图" || t == "UI界面截图");
    if is_screenshot_type {
        const NON_SCREENSHOT_TAGS: &[&str] = &[
            "户外活动", "旅行照", "风景照", "摄影照片", "人物照", "人像写真", "宠物照",
            "青年漫", "少年漫", "少女漫", "成人漫", "漫画", "婚纱照", "微距摄影", "航空航拍", "建筑摄影",
            "海报宣发", "医学影像", "证照", "合同票据", "表情包", "设计稿", "图纸",
            "高ISO噪点", "逆光死白", "暗光欠曝", "虚焦", "抖动", "脱焦", "运动抖动", "曝光正常"
        ];
        detected_visual_tags.retain(|t| !NON_SCREENSHOT_TAGS.contains(&t.as_str()));
    }

    // 室内静物特写组内互斥门禁: 若首要判定为静物照且无自然景观，剔除街拍抓拍与建筑摄影
    let is_still_life_type = photo_type.as_deref() == Some("静物照") || detected_visual_tags.first().map(|t| t == "静物照").unwrap_or(false);
    let has_outdoor = detected_visual_tags.iter().any(|t| t == "自然景观" || t == "户外活动" || t == "历史" || t == "历史遗迹");
    if is_still_life_type && !has_outdoor {
        detected_visual_tags.retain(|t| t != "街拍抓拍" && t != "建筑摄影");
    }

    // 摄影专属质量门禁: "高ISO噪点", "逆光死白", "暗光欠曝", "虚焦", "抖动", "脱焦", "运动抖动", "曝光正常" 仅适用于真实摄影照片
    let is_photo_type = photo_type.as_deref() == Some("摄影照片")
        || (!is_screenshot_type && detected_visual_tags.iter().any(|t| t == "摄影照片" || t == "人物照" || t == "风景照" || t == "微距摄影" || t == "人像写真"));
    if !is_photo_type {
        const PHOTO_ONLY_QUALITY: &[&str] = &[
            "高ISO噪点", "逆光死白", "暗光欠曝", "虚焦", "抖动", "脱焦", "运动抖动", "曝光正常"
        ];
        detected_visual_tags.retain(|t| !PHOTO_ONLY_QUALITY.contains(&t.as_str()));
    }

    // 根据质量评分推导【文件质量】维度标签 (ID 27: 高质量 / 中等质量 / 低质量)
    if let Some(qs) = quality_score {
        let file_quality_tag = if qs >= 8.0 {
            "高质量"
        } else if qs >= 5.0 {
            "中等质量"
        } else {
            "低质量"
        };
        if !detected_visual_tags.contains(&file_quality_tag.to_string()) {
            detected_visual_tags.push(file_quality_tag.to_string());
        }
    }

    // 4. 离线逆地理编码 (若元数据中含 GPS 坐标且开启了地理反查，Pro 专享)
    let mut geo_address = None;
    let enable_geo = req.enable_geo_reverse.unwrap_or(true);

    if is_pro && enable_geo {
        let t_geo = std::time::Instant::now();
        let lat_opt = metadata
            .get("GPSLatitude")
            .or_else(|| metadata.get("exiftool").and_then(|e| e.get("GPSLatitude")));
        let lon_opt = metadata
            .get("GPSLongitude")
            .or_else(|| metadata.get("exiftool").and_then(|e| e.get("GPSLongitude")));

        let parse_coord = |v: Option<&serde_json::Value>| -> Option<f64> {
            match v {
                Some(serde_json::Value::Number(n)) => n.as_f64(),
                Some(serde_json::Value::String(s)) => s.trim().parse::<f64>().ok(),
                _ => None,
            }
        };

        if let (Some(lat), Some(lon)) = (parse_coord(lat_opt), parse_coord(lon_opt)) {
            let geo = state.geo.clone();
            let lang = req.language.clone().unwrap_or_else(|| "zh-CN".to_string());
            let outcome = tokio::task::spawn_blocking(move || {
                let points = vec![omni_pro::geo::GeoQueryPoint {
                    latitude: lat,
                    longitude: lon,
                }];
                geo.reverse(&points, Some(&lang), Some(50.0), Some(500.0))
            })
            .await;

            if let Ok(outcome) = outcome {
                if let Some(results) = outcome.results {
                    if let Some(first) = results.into_iter().next() {
                        if first.found {
                            let parts: Vec<String> = [first.country, first.province, first.city]
                                .into_iter()
                                .flatten()
                                .collect();
                            if !parts.is_empty() {
                                geo_address = Some(parts.join(" "));
                            }
                        }
                    }
                }
            }
        }
        benchmark.geo_ms = Some(t_geo.elapsed().as_millis() as u64);
    }

    // 5. 工作流状态与安全等级推断 (输出语言中立机器代码，Pro 专享)
    let (workflow_state_code, workflow_state, mut security_level_code, mut security_level) = if is_pro {
        let ws_code = Some(omni_pro::perceive::detect_workflow_state(&file_path, &metadata));
        let ws = ws_code.clone();
        let sec_code = Some(omni_pro::perceive::detect_security_level(&file_path, &markdown_content));
        let sec = sec_code.clone();
        (ws_code, ws, sec_code, sec)
    } else {
        (None, None, None, None)
    };

    // 联动逻辑: 若检出敏感内容 (色情/涉政/血腥/违规) 或 R-18 尺度，强制安全等级联动输出为 "保密" (confidential)
    if !sensitive_types.is_empty()
        || nsfw_tags.iter().any(|t| t == "色情" || t == "R-18" || t == "R-18G")
        || content_rating.as_deref() == Some("r18")
        || content_rating.as_deref() == Some("r18g")
    {
        security_level_code = Some("confidential".to_string());
        security_level = Some("保密".to_string());
    }

    // 6. 提取多模态元数据字段 (优先使用原生三大引擎聚合的 visual_tags，回退元数据)
    let mut visual_tags = detected_visual_tags;
    if visual_tags.is_empty() {
        if let Some(arr) = metadata.get("visual_tags").and_then(|v| v.as_array()) {
            visual_tags = arr.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect();
        }
    }

    let audio_transcript = metadata
        .get("audio_transcript")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let audio_events = metadata
        .get("audio_events")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect::<Vec<String>>()
        })
        .unwrap_or_default();

    let category = metadata
        .get("category")
        .and_then(|c| c.get("group"))
        .and_then(|g| g.as_str())
        .map(|s| s.to_string());

    benchmark.total_ms = t_start.elapsed().as_millis() as u64;

    tracing::info!(
        "[OmniServer] 原生多模态感知完成: file={}, 耗时={}ms, watermark_level={:?}, mosaic_level={:?}, has_text={:?}, visual_tags={:?}, mobilenet_tags={:?}, clip_tags={:?}, nsfw_tags={:?}, mobilenet_hc={:?}, clip_hc={:?}, nsfw_hc={:?}, geo={:?}",
        file_path,
        benchmark.total_ms,
        watermark_level,
        mosaic_level,
        has_text,
        visual_tags,
        mobilenet_tags,
        clip_tags,
        nsfw_tags,
        mobilenet_high_confidence_tags,
        clip_high_confidence_tags,
        nsfw_high_confidence_tags,
        geo_address
    );

    Json(OmniPerceptionResult {
        file_path,
        mime_type,
        file_size,
        category,
        markdown_content,
        metadata,
        file_source,
        file_source_code,
        source_url,
        workflow_state,
        workflow_state_code,
        security_level,
        security_level_code,
        has_watermark,
        watermark_level,
        watermark_status,
        has_mosaic,
        mosaic_level,
        mosaic_status,
        has_text,
        aesthetic_score,
        quality_score,
        photo_type,
        quality_issues,
        visual_tags,
        mobilenet_tags,
        clip_tags,
        nsfw_tags,
        mobilenet_high_confidence_tags,
        clip_high_confidence_tags,
        nsfw_high_confidence_tags,
        sensitive_types,
        content_rating,
        audio_transcript,
        audio_events,
        geo_address,
        phash,
        is_corrupted,
        benchmark: Some(benchmark),
    })
}

/// 单指标音频转录处理: POST /api/audio/transcribe
async fn audio_transcribe_handler(
    State(state): State<AppState>,
    Json(req): Json<AudioTranscribeRequest>,
) -> Json<AudioTranscribeResponse> {
    let cfg = state.config.lock().unwrap().clone();
    let t_start = std::time::Instant::now();
    let file_path = req.file_path.clone();

    let mut transcript = None;
    let mut events = Vec::new();

    if let Ok(res) = OmniExtractor::extract(&file_path, &cfg).await {
        if let Some(t) = res.metadata.get("audio_transcript").and_then(|v| v.as_str()) {
            transcript = Some(t.to_string());
        } else if !res.markdown_content.is_empty()
            && (res.mime_type.starts_with("audio/") || res.mime_type.starts_with("video/"))
        {
            transcript = Some(res.markdown_content);
        }

        if let Some(ev) = res.metadata.get("audio_events").and_then(|v| v.as_array()) {
            events = ev.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect();
        }
    }

    let duration_ms = t_start.elapsed().as_millis() as u64;
    Json(AudioTranscribeResponse {
        file_path,
        transcript,
        events,
        language: req.language,
        duration_ms,
    })
}

/// 单指标视觉标签处理: POST /api/vision/tags
async fn vision_tags_handler(
    State(state): State<AppState>,
    Json(req): Json<VisionTagsRequest>,
) -> Json<VisionTagsResponse> {
    let cfg = state.config.lock().unwrap().clone();
    let t_start = std::time::Instant::now();
    let file_path = req.file_path.clone();

    let mut tags = Vec::new();
    if omni_pro::is_pro_enabled() {
        tags = omni_pro::OmniVisionEngine::extract_clip_visual_tags(
            &file_path,
            req.language.as_deref(),
            req.top_k.unwrap_or(5),
        );
    }

    if tags.is_empty() {
        if let Ok(res) = OmniExtractor::extract(&file_path, &cfg).await {
            if let Some(arr) = res.metadata.get("visual_tags").and_then(|v| v.as_array()) {
                tags = arr.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect();
            }
        }
    }

    if let Some(top_k) = req.top_k {
        if tags.len() > top_k {
            tags.truncate(top_k);
        }
    }

    let duration_ms = t_start.elapsed().as_millis() as u64;
    Json(VisionTagsResponse {
        file_path,
        tags,
        duration_ms,
    })
}

/// 单指标图像频域特征检测处理: POST /api/vision/inspect
async fn vision_inspect_handler(
    Json(req): Json<VisionInspectRequest>,
) -> Json<VisionInspectResponse> {
    if !omni_pro::is_pro_enabled() {
        return Json(VisionInspectResponse {
            file_path: req.file_path,
            has_watermark: false,
            watermark_level: 0,
            watermark_status: "none".to_string(),
            has_mosaic: false,
            mosaic_level: 0,
            mosaic_status: "none".to_string(),
            duration_ms: 0,
        });
    }

    let t_start = std::time::Instant::now();
    let file_path = req.file_path.clone();

    let mut has_watermark = false;
    let mut watermark_level = 0u8;
    let mut watermark_status = "none".to_string();
    let mut has_mosaic = false;
    let mut mosaic_level = 0u8;
    let mut mosaic_status = "none".to_string();

    if let Ok(img) = image::open(&file_path) {
        watermark_level = omni_pro::perceive::detect_watermark_level(&img);
        has_watermark = watermark_level > 0;
        watermark_status = omni_pro::perceive::detect_watermark_status(&img).to_string();

        mosaic_level = omni_pro::perceive::detect_mosaic_level(&img);
        has_mosaic = mosaic_level > 0;
        mosaic_status = omni_pro::perceive::detect_mosaic_status(&img).to_string();
    }

    let duration_ms = t_start.elapsed().as_millis() as u64;
    Json(VisionInspectResponse {
        file_path,
        has_watermark,
        watermark_level,
        watermark_status,
        has_mosaic,
        mosaic_level,
        mosaic_status,
        duration_ms,
    })
}

/// 单指标文件系统 ADS 来源检测处理: POST /api/fs/ads
async fn fs_ads_handler(
    Json(req): Json<FsAdsRequest>,
) -> Json<FsAdsResponse> {
    if !omni_pro::is_pro_enabled() {
        return Json(FsAdsResponse {
            file_path: req.file_path,
            file_source: None,
            file_source_code: None,
            source_url: None,
            duration_ms: 0,
        });
    }

    let t_start = std::time::Instant::now();
    let file_path = req.file_path.clone();

    let (file_source, file_source_code, source_url) = omni_pro::perceive::detect_ntfs_zone_identifier(&file_path);

    let duration_ms = t_start.elapsed().as_millis() as u64;
    Json(FsAdsResponse {
        file_path,
        file_source,
        file_source_code,
        source_url,
        duration_ms,
    })
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


