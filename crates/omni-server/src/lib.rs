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
    AudioConvertRequest, AudioConvertResponse, AudioTranscribeRequest, AudioTranscribeResponse,
    DuplicateFixRequest, DuplicateFixResponse, DuplicateScanRequest, DuplicateScanResponse,
    FsAdsRequest, FsAdsResponse, OmniConfig, OmniExtractionResult, OmniPerceptionBenchmark,
    OmniPerceptionRequest, OmniPerceptionResult, VisionInspectRequest, VisionInspectResponse,
    VisionTagsRequest, VisionTagsResponse,
};
use omni_extract::OmniExtractor;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::info;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Mutex<OmniConfig>>,
    /// 离线反向地理编码服务（数据集缺失或开源存根时为软不可用实例）
    pub geo: Arc<omni_pro::geo::GeoService>,
    /// OpenHowNet 语义槽位挖掘与对齐服务（数据集缺失或开源存根时为软不可用实例）
    pub hownet: Arc<omni_pro::hownet::OmniHowNetService>,
    /// 工业级混合检索与约束聚类服务 (支柱 5)
    pub search: Arc<omni_pro::search::OmniSearchService>,
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
        .route("/api/audio/convert", post(audio_convert_handler))
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
        .route("/api/hownet/describe", post(hownet_describe_handler))
        .route("/api/text/analyze", post(text_analyze_handler))
        .route("/api/search/index", post(search_index_handler))
        .route("/api/search/hybrid", post(search_hybrid_handler))
        .route("/api/search/cluster", post(search_cluster_handler))
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

/// 健康检查：附 Pro 模块与地理/HowNet 子系统可用性，供前端 UI 与桌面端启动时探测
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
    let hownet_available = if is_pro {
        let hownet = state.hownet.clone();
        tokio::task::spawn_blocking(move || hownet.is_available())
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
        "hownetAvailable": hownet_available,
        "searchAvailable": is_pro,
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

/// OpenHowNet 语义描述查询与自然语言描述句合成: POST /api/hownet/describe
async fn hownet_describe_handler(
    State(state): State<AppState>,
    Json(req): Json<omni_pro::hownet::HowNetDescribeRequest>,
) -> Json<omni_pro::hownet::HowNetDescribeResult> {
    let hownet = state.hownet.clone();
    let word = req.word.clone();
    let outcome = tokio::task::spawn_blocking(move || hownet.describe(&word))
        .await
        .unwrap_or_else(|err| {
            Ok(omni_pro::hownet::HowNetDescribeResult {
                word: req.word.clone(),
                found: false,
                is_aligned: false,
                alignment_level: None,
                top_concept: None,
                slots: Vec::new(),
                synonyms: Vec::new(),
                antonyms: Vec::new(),
                description: format!("HowNet 查询任务执行失败: {err}"),
            })
        })
        .unwrap_or_else(|err| {
            omni_pro::hownet::HowNetDescribeResult {
                word: req.word.clone(),
                found: false,
                is_aligned: false,
                alignment_level: None,
                top_concept: None,
                slots: Vec::new(),
                synonyms: Vec::new(),
                antonyms: Vec::new(),
                description: format!("HowNet 检索内部错误: {err}"),
            }
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
    // HowNet 知识库发现链：环境变量 → exe 相对目录 → cwd 候选；落空或开源存根时软不可用
    let hownet = match omni_pro::hownet::discover_hownet_db_path() {
        Some(path) => {
            info!("omni-hownet db found at {}", path.display());
            Arc::new(omni_pro::hownet::OmniHowNetService::open(path).unwrap_or_else(|err| {
                tracing::warn!("Failed to open omni-hownet db: {err}");
                omni_pro::hownet::OmniHowNetService::unavailable()
            }))
        }
        None => {
            info!("omni-hownet db not found or open-core stub mode, hownet subsystem starts unavailable");
            Arc::new(omni_pro::hownet::OmniHowNetService::unavailable())
        }
    };
    let search_dir = if let Ok(appdata) = std::env::var("APPDATA") {
        PathBuf::from(appdata).join("firefly-ai-folder").join("search_index")
    } else {
        std::env::temp_dir().join("firefly_omni_search_index")
    };
    let search = Arc::new(omni_pro::search::OmniSearchService::new(search_dir));
    let state = AppState {
        config: Arc::new(Mutex::new(initial_config)),
        geo,
        hownet,
        search,
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
    ram_tags: Vec<omni_core::RamTagItem>,
    inspect_img: Option<image::DynamicImage>,
    /// CLIP 图像嵌入向量（512 维归一化），用于级联假设仲裁
    image_embedding: Option<Vec<f32>>,
    /// CLIP 互斥组分类结果：(标签名, 置信度, 组名)
    /// 覆盖：内容形态/文字存在性/色彩模式/截图细分 四个互斥维度
    clip_mutual_tags: Vec<(String, f32, &'static str)>,
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
    ram_ms: u64,
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

                // 8. RAM++ 细粒度实体与泛维度/泛标签投影提取
                let h_ram = s.spawn(|| {
                    let t = std::time::Instant::now();
                    let res = if enable_visual_tags {
                        omni_pro::OmniVisionEngine::extract_ram_tags(&inspect_img, lang, 10)
                    } else {
                        Vec::new()
                    };
                    (res, t.elapsed().as_millis() as u64)
                });

                // 9. CLIP 图像嵌入向量提取 (用于级联假设 CLIP 仲裁)
                let h_embed = s.spawn(|| {
                    omni_pro::OmniVisionEngine::extract_clip_image_embedding(&inspect_img, lang)
                });

                let (td, td_ms) = h_text.join().unwrap_or((false, 0));
                let (ct, ct_ms) = h_clip.join().unwrap_or((Vec::new(), 0));
                let (np, np_ms) = h_nsfw.join().unwrap_or((None, 0));
                let (wl, wl_ms) = h_wm.join().unwrap_or((0, 0));
                let (ml, ml_ms) = h_mc.join().unwrap_or((0, 0));
                let (ar, ar_ms) = h_aes.join().unwrap_or(((7.5, Vec::new()), 0));
                let (bw, bw_ms) = h_bw.join().unwrap_or((false, 0));
                let (ram_res, ram_ms) = h_ram.join().unwrap_or((Vec::new(), 0));
                let img_embed = h_embed.join().unwrap_or(None);

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
                out.ram_tags = ram_res;
                out.ram_ms = ram_ms;
                out.image_embedding = img_embed;
                // CLIP 互斥分类：利用图像嵌入向量对内容形态/色彩/文字等互斥组分类
                if let Some(ref emb) = out.image_embedding {
                    out.clip_mutual_tags = omni_pro::OmniVisionEngine::classify_mutual_exclusive_groups(
                        emb.as_slice(),
                        lang,
                    );
                }
                // 标签任务取并行最大值
                out.tag_ms = ct_ms.max(np_ms).max(ram_ms);

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

    let ((file_source, file_source_code, source_url, ads_ms), (metadata, markdown_content, phash, is_corrupted, vision_res, ocr_text)) = if is_image {
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

            let ocr_text = if !markdown_content.trim().is_empty() {
                Some(markdown_content.clone())
            } else {
                None
            };

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
                ocr_text,
            )
        };
        let (ads_res, (metadata_ms, vision_res, markdown_content, ocr_ms, text_ms, phash, metadata, ocr_text)) =
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
        benchmark.ram_ms = Some(vision_res.ram_ms);
        benchmark.tag_ms = Some(vision_res.tag_ms);
        benchmark.ocr_ms = ocr_ms;
        benchmark.text_ms = text_ms;
        benchmark.extract_ms = Some(vision_res.duration_ms.max(metadata_ms).max(ocr_ms.unwrap_or(0)));

        (ads_res, (metadata, markdown_content, phash, is_corrupted, vision_res, ocr_text))
    } else {
        let f_non_img = async {
            let t_extract = std::time::Instant::now();
            let mut ext_res = match OmniExtractor::extract(&file_path, &cfg).await {
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

            // 音频/视频文件: 截取降噪 → SenseVoice 转录
            let is_audio = mime_type.starts_with("audio/")
                || matches!(ext.as_str(), "mp3" | "wav" | "flac" | "aac" | "ogg" | "m4a" | "wma" | "opus" | "ape" | "aiff");
            let is_audio_or_video = is_audio || is_video;
            let mut audio_ms: Option<u64> = None;

            // 仅在音视频文件且 enable_audio_transcript 未显式关闭时执行 SenseVoice 转录
            let should_transcribe = is_audio_or_video
                && req.enable_audio_transcript.unwrap_or(true);

            if should_transcribe {
                let t_audio = std::time::Instant::now();
                // 优先使用请求中传入的截取时长，否则读取全局配置
                let duration_seconds = req.audio_analysis_duration.unwrap_or(cfg.audio_analysis_duration);
                let file_path_for_audio = file_path.clone();
                let language = req.language.clone();

                // spawn_blocking: FFmpeg 截取降噪 + SenseVoice 转录（全部阻塞操作）
                let transcript_result = tokio::task::spawn_blocking(move || {
                    // Step 1: 截取降噪，输出标准 WAV
                    let wav_path = convert_audio_standard(&file_path_for_audio, duration_seconds)?;
                    // Step 2: SenseVoice ASR 转录
                    transcribe_with_sense_asr(&wav_path, language.as_deref())
                })
                .await
                .ok()
                .flatten();

                audio_ms = Some(t_audio.elapsed().as_millis() as u64);

                if let Some(transcript) = transcript_result {
                    tracing::info!(
                        "[OmniServer] 音频转录完成: file={}, len={}, audio_ms={:?}",
                        file_path, transcript.len(), audio_ms
                    );
                    // 写入 markdown_content（作为主内容字段）
                    if ext_res.markdown_content.is_empty() {
                        ext_res.markdown_content = transcript.clone();
                    }
                    // 同时写入 metadata["audio_transcript"]，供消费层使用
                    if let serde_json::Value::Object(ref mut map) = ext_res.metadata {
                        map.insert("audio_transcript".to_string(), serde_json::Value::String(transcript));
                    }
                } else {
                    tracing::info!("[OmniServer] 音频转录无结果或模型/ffmpeg未就绪: file={}", file_path);
                }
            }

            (extract_ms, ext_res, v, audio_ms)
        };
        let (ads_res, (extract_ms, ext_res, v, audio_ms)) = tokio::join!(f_ads, f_non_img);

        benchmark.extract_ms = Some(extract_ms);
        benchmark.audio_ms = audio_ms;
        if let Some(bm) = &ext_res.benchmark {
            benchmark.metadata_ms = bm.metadata_ms;
            benchmark.text_ms = bm.text_ms;
            benchmark.ocr_ms = bm.ocr_ms;
        }

        (ads_res, (ext_res.metadata, ext_res.markdown_content, ext_res.phash, ext_res.is_corrupted, v, None))
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
    let mut has_text = vision_res.has_text;
    let aesthetic_score = vision_res.aesthetic_score;
    let quality_score = vision_res.quality_score;
    let mut photo_type = vision_res.photo_type;
    let quality_issues = vision_res.quality_issues;
    let mut mobilenet_tags = vision_res.mobilenet_tags;
    let mut clip_tags = vision_res.clip_tags;

    // 基于实际 OCR 文本正向直通校准截图形态与文字客观事实 (彻底根除有字却输出无字图的倒挂)
    if !markdown_content.trim().is_empty() {
        has_text = Some(true);
        clip_tags.retain(|t| t != "无字图");
        if !clip_tags.contains(&"有字图".to_string()) {
            clip_tags.push("有字图".to_string());
        }

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

    let ram_tags = vision_res.ram_tags;
    let image_embedding = vision_res.image_embedding;
    let clip_mutual_tags = vision_res.clip_mutual_tags;

    // CLIP 互斥分类结果增强 mobilenet_tags：
    // 若 CLIP 互斥分类成功，直接替换规则推导结果（语义更准确）；
    // 若 CLIP 不可用（无模型），则保留规则推导的 mobilenet_tags 作为兜底。
    let mut mobilenet_tags = if !clip_mutual_tags.is_empty() {
        let mut merged = mobilenet_tags.clone();
        for (tag, _conf, _group) in &clip_mutual_tags {
            if !merged.contains(tag) {
                merged.push(tag.clone());
            }
        }
        // CLIP 互斥分类保证组内只有一个胜出者，移除被覆盖的规则推导冲突项
        // 1. 色彩模式：CLIP 结果与黑白检测结果互斥对齐
        if clip_mutual_tags.iter().any(|(t, _, g)| *g == "色彩模式" && t == "全彩") {
            merged.retain(|t| t != "黑白");
        } else if clip_mutual_tags.iter().any(|(t, _, g)| *g == "色彩模式" && t == "黑白") {
            merged.retain(|t| t != "全彩");
        }
        // 2. 文字存在性：客观检测与 OCR 事实优先于语义猜测
        if has_text == Some(true) || !markdown_content.trim().is_empty() {
            merged.retain(|t| t != "无字图");
            if !merged.contains(&"有字图".to_string()) {
                merged.push("有字图".to_string());
            }
        } else if clip_mutual_tags.iter().any(|(t, _, g)| *g == "文字存在性" && t == "有字图") {
            merged.retain(|t| t != "无字图");
        } else if clip_mutual_tags.iter().any(|(t, _, g)| *g == "文字存在性" && t == "无字图") {
            merged.retain(|t| t != "有字图");
        }
        merged
    } else {
        if has_text == Some(true) || !markdown_content.trim().is_empty() {
            mobilenet_tags.retain(|t| t != "无字图");
            if !mobilenet_tags.contains(&"有字图".to_string()) {
                mobilenet_tags.push("有字图".to_string());
            }
        }
        mobilenet_tags
    };

    // CLIP 互斥组与 photo_type 细化联动更新（语义优先于规则推导）
    if !clip_mutual_tags.is_empty() {
        // 优先取截图细分（比泛截图更精确）
        if let Some((sub_tag, _, _)) = clip_mutual_tags.iter().find(|(_, _, g)| *g == "截图场景细分") {
            if photo_type.is_none() || photo_type.as_deref() == Some("截图") {
                photo_type = Some(sub_tag.clone());
            }
        } else if let Some((photo_sub, _, _)) = clip_mutual_tags.iter().find(|(_, _, g)| *g == "摄影题材细分") {
            // 若为摄影照片，细化为风景照/人物照/静物照等
            if photo_type.is_none() || photo_type.as_deref() == Some("摄影照片") {
                photo_type = Some(photo_sub.clone());
            }
        } else if let Some((form_tag, _, _)) = clip_mutual_tags.iter().find(|(_, _, g)| *g == "内容形态") {
            // 内容形态胜出者作为 photo_type 的兜底
            if photo_type.is_none() {
                photo_type = Some(form_tag.clone());
            }
        }
    }

    // 汇聚各大引擎的所有标签至 detected_visual_tags (有序去重)
    let mut detected_visual_tags: Vec<String> = Vec::new();
    for tag in clip_tags
        .iter()
        .chain(mobilenet_tags.iter())
        .chain(nsfw_tags.iter())
        .chain(quality_issues.iter())
        .chain(ram_tags.iter().map(|r| &r.tag))
    {
        if !detected_visual_tags.contains(tag) {
            detected_visual_tags.push(tag.clone());
        }
    }

    // 架构级通用分类互斥门控引擎：统一处理组内竞争排他、跨形态互斥、安全合规阻断与光学质量限制
    omni_pro::OmniVisionEngine::apply_mutual_exclusion_gating(
        &mut detected_visual_tags,
        &clip_mutual_tags,
        content_rating.as_deref(),
        &sensitive_types,
        quality_score,
    );

    // 文字存在性绝对保护：若已探活出文字或 OCR 内容，无条件排除无字图，确保有字图存在
    if has_text == Some(true) || !markdown_content.trim().is_empty() {
        detected_visual_tags.retain(|t| t != "无字图");
        if !detected_visual_tags.contains(&"有字图".to_string()) {
            detected_visual_tags.push("有字图".to_string());
        }
    }

    // 同步清洗 mobilenet_tags 与 clip_tags，确保互斥清洗结果一致贯通（防止被清洗的子标签混入下游主体池）
    mobilenet_tags.retain(|t| detected_visual_tags.contains(t));
    clip_tags.retain(|t| detected_visual_tags.contains(t));

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

    // 6. 统一汇聚各大引擎标签并解析为标签链 (TagChainItem) 结构
    let mut structured_visual_tags: Vec<omni_core::TagChainItem> = Vec::new();

    // 6.1 首先将已具备完整维度与逻辑泛维度的 RAM++ 标签加入
    for r in &ram_tags {
        if !structured_visual_tags.iter().any(|t| t.tag.eq_ignore_ascii_case(&r.tag)) {
            structured_visual_tags.push(r.clone());
        }
    }

    // 6.2 将其他各大引擎的有效视觉标签 (经过互斥门禁过滤的 detected_visual_tags) 统一映射为 TagChainItem
    for raw_tag in &detected_visual_tags {
        if !structured_visual_tags.iter().any(|t| t.tag.eq_ignore_ascii_case(raw_tag)) {
            let item = if is_pro {
                omni_pro::OmniVisionEngine::resolve_tag_to_chain_item(raw_tag, 0.92)
            } else {
                omni_core::TagChainItem {
                    tag: raw_tag.clone(),
                    confidence: 0.92,
                    dimension_id: 28,
                    dimension_name: "内容标签".to_string(),
                    logic_pan_dimension: raw_tag.clone(),
                }
            };
            structured_visual_tags.push(item);
        }
    }

    // 7. 级联提示词合成 + CLIP 向量仲裁终局裁决 (Pro 专享，仅图片路径生效)
    // 注意：此处在 ram_tags 降级为 flat 前调用，以获取完整 TagChainItem 结构
    let (cascade_candidates, winning_hypothesis, activated_dimension_tags, smart_name, content_description, pruned_ambiguous_words) = if is_pro && is_image {
        omni_pro::OmniVisionEngine::synthesize_cascade_hypotheses_and_arbitrate(
            &file_path,
            &detected_visual_tags,
            &ram_tags,
            &mobilenet_tags,
            &nsfw_tags,
            image_embedding.as_deref(),
            req.language.as_deref(),
        )
    } else {
        (Vec::new(), None, Vec::new(), None, None, Vec::new())
    };

    // 6.3 ram_tags 降级为平铺字符串数组 (仅包含 RAM++ 检测出的纯实体标签名)
    let ram_tags_flat: Vec<String> = ram_tags.into_iter().map(|r| r.tag).collect();

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

    // 8. Tier 1 端侧纯 CPU 确定性文本特征与 384 维向量提取 (omni-text 支柱 4)
    // 包含: #615 Chunker, #616 fastText/KeyBERT, #617 bekko-a8m 384d, #618 实体槽位/5W摘要/智能重命名
    let mtime = std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .map(|t| chrono::DateTime::<chrono::Utc>::from(t));
    let file_name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();

    let text_analysis = if !markdown_content.trim().is_empty() {
        let t_text = std::time::Instant::now();
        let hownet_svc = Some(&*state.hownet);
        let res = omni_pro::text::OmniTextEngine::analyze(
            &markdown_content,
            &file_name,
            mtime,
            hownet_svc,
        );
        let text_duration = t_text.elapsed().as_millis() as u64;
        benchmark.text_ms = Some(benchmark.text_ms.unwrap_or(0) + text_duration);
        Some(res)
    } else {
        None
    };

    let (text_title, text_keywords, text_entities, text_summary, text_one_desc, text_slots, text_emb, text_smart_name) = match text_analysis {
        Some(res) => (
            res.title,
            res.keywords,
            res.entities.iter().filter_map(|e| serde_json::to_value(e).ok()).collect(),
            serde_json::to_value(&res.structured_summary).ok(),
            res.one_sentence_desc,
            serde_json::to_value(&res.name_slots).ok(),
            Some(res.embedding_dense),
            res.smart_name,
        ),
        None => (
            None,
            Vec::new(),
            Vec::new(),
            None,
            None,
            None,
            None,
            None,
        ),
    };

    // 智能重命名回填策略：若图像仲裁没有产出 smart_name，回填文本分析的确定性槽位重命名
    let smart_name = smart_name.or(text_smart_name);
    // 概要描述回填策略：若视觉没有给出 content_description，回填文本的一句话描述
    let content_description = content_description.or_else(|| text_one_desc.clone());

    benchmark.total_ms = t_start.elapsed().as_millis() as u64;

    tracing::info!(
        "[OmniServer] 原生多模态感知完成: file={}, 耗时={}ms, watermark_level={:?}, mosaic_level={:?}, has_text={:?}, visual_tags_count={}, ram_tags={:?}, geo={:?}",
        file_path,
        benchmark.total_ms,
        watermark_level,
        mosaic_level,
        has_text,
        structured_visual_tags.len(),
        ram_tags_flat,
        geo_address
    );

    Json(OmniPerceptionResult {
        file_path,
        mime_type,
        file_size,
        category,
        markdown_content,
        ocr_text,
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
        visual_tags: structured_visual_tags,
        mobilenet_tags,
        clip_tags,
        nsfw_tags,
        ram_tags: ram_tags_flat,
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
        // 级联假设仲裁终局字段
        candidate_hypotheses: cascade_candidates,
        winning_hypothesis,
        activated_dimension_tags,
        smart_name,
        content_description,
        pruned_ambiguous_words,
        // Tier 1 端侧纯 CPU 确定性文本特征与向量
        title: text_title,
        keywords: text_keywords,
        entities: text_entities,
        structured_summary: text_summary,
        one_sentence_desc: text_one_desc,
        name_slots: text_slots,
        embedding_dense: text_emb.clone(),
        benchmark: Some(benchmark),
    });

    // 跨支柱自动索引 (Pillar 4 -> Pillar 5): 若感知产出了稠密特征向量且有文本，自动异步入库双轨混合索引
    if let (Some(ref emb), false) = (&text_emb, markdown_content.trim().is_empty()) {
        let indexed_doc = omni_pro::search::IndexedDocument {
            fingerprint: file_path.clone(),
            embedding: emb.clone(),
            searchable_text: format!("{}\n{}", file_name, markdown_content),
        };
        let search_arc = state.search.clone();
        tokio::task::spawn_blocking(move || {
            let _ = search_arc.upsert_batch(&[indexed_doc]);
        });
    }

    result
}

/// 纯文本确定性特征分析请求体: POST /api/text/analyze
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextAnalyzeRequest {
    pub text: String,
    pub file_name: Option<String>,
    pub mtime: Option<chrono::DateTime<chrono::Utc>>,
}

/// 纯文本确定性特征与 384 维向量分析 API: POST /api/text/analyze
async fn text_analyze_handler(
    State(state): State<AppState>,
    Json(req): Json<TextAnalyzeRequest>,
) -> Json<omni_pro::text::TextAnalysisResult> {
    let file_name = req.file_name.unwrap_or_default();
    let hownet_svc = Some(&*state.hownet);
    let res = omni_pro::text::OmniTextEngine::analyze(
        &req.text,
        &file_name,
        req.mtime,
        hownet_svc,
    );
    Json(res)
}

/// 批量构建混合索引请求体: POST /api/search/index
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchIndexRequest {
    pub documents: Vec<omni_pro::search::IndexedDocument>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchIndexResponse {
    pub success: bool,
    pub total_indexed: usize,
    pub error: Option<String>,
}

/// 批量写入双轨混合索引 (USearch 密集向量 + Tantivy BM25): POST /api/search/index
async fn search_index_handler(
    State(state): State<AppState>,
    Json(req): Json<SearchIndexRequest>,
) -> Json<SearchIndexResponse> {
    let search = state.search.clone();
    let res = tokio::task::spawn_blocking(move || {
        let mut docs = req.documents;
        let embedder = omni_pro::text::BekkoEmbedder::new();
        for doc in &mut docs {
            // 若未提供向量或维度不匹配，自动调用 BekkoEmbedder 补充 384 维向量
            if doc.embedding.len() != omni_pro::search::EMBEDDING_DIM {
                if let Ok(vec) = embedder.embed(&doc.searchable_text) {
                    doc.embedding = vec;
                }
            }
        }
        search.upsert_batch(&docs)
    }).await;
    match res {
        Ok(Ok(total)) => Json(SearchIndexResponse {
            success: true,
            total_indexed: total,
            error: None,
        }),
        Ok(Err(e)) => Json(SearchIndexResponse {
            success: false,
            total_indexed: 0,
            error: Some(e.to_string()),
        }),
        Err(e) => Json(SearchIndexResponse {
            success: false,
            total_indexed: 0,
            error: Some(format!("Task panic: {e}")),
        }),
    }
}

/// 双轨混合检索与加权 RRF 融合重排请求体: POST /api/search/hybrid
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHybridRequest {
    pub query_text: Option<String>,
    pub query_embedding: Option<Vec<f32>>,
    #[serde(default = "default_search_top_k")]
    pub top_k: usize,
}

fn default_search_top_k() -> usize {
    20
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHybridResponse {
    pub success: bool,
    pub results: Vec<omni_pro::search::FusedResult>,
    pub error: Option<String>,
}

/// 双轨混合检索与 RRF 融合重排: POST /api/search/hybrid
async fn search_hybrid_handler(
    State(state): State<AppState>,
    Json(req): Json<SearchHybridRequest>,
) -> Json<SearchHybridResponse> {
    let search = state.search.clone();
    let res = tokio::task::spawn_blocking(move || {
        // 跨支柱补缝：若未显式提供 query_embedding，但提供了 query_text，自动通过 BekkoEmbedder 计算 384 维特征向量
        let query_emb = match (req.query_embedding, req.query_text.as_deref()) {
            (Some(emb), _) => Some(emb),
            (None, Some(text)) if !text.trim().is_empty() => {
                omni_pro::text::BekkoEmbedder::new().embed(text).ok()
            }
            _ => None,
        };

        search.search_hybrid(
            req.query_text.as_deref(),
            query_emb.as_deref(),
            req.top_k,
        )
    })
    .await;
    match res {
        Ok(Ok(results)) => Json(SearchHybridResponse {
            success: true,
            results,
            error: None,
        }),
        Ok(Err(e)) => Json(SearchHybridResponse {
            success: false,
            results: Vec::new(),
            error: Some(e.to_string()),
        }),
        Err(e) => Json(SearchHybridResponse {
            success: false,
            results: Vec::new(),
            error: Some(format!("Task panic: {e}")),
        }),
    }
}

/// 提示词引导层次凝聚聚类与目录树生成请求体: POST /api/search/cluster
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchClusterRequest {
    pub documents: Vec<omni_pro::search::ClusterDocument>,
    pub prompt: Option<String>,
    pub prompt_embedding: Option<Vec<f32>>,
    #[serde(default = "default_distance_threshold")]
    pub distance_threshold: f32,
    #[serde(default = "default_max_leaf_size")]
    pub max_leaf_size: usize,
}

fn default_distance_threshold() -> f32 {
    0.4
}

fn default_max_leaf_size() -> usize {
    50
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchClusterResponse {
    pub success: bool,
    pub result: Option<omni_pro::search::ClusterTreeResult>,
    pub error: Option<String>,
}

/// 约束层次聚类多级目录自动归档: POST /api/search/cluster
async fn search_cluster_handler(
    State(state): State<AppState>,
    Json(req): Json<SearchClusterRequest>,
) -> Json<SearchClusterResponse> {
    let search = state.search.clone();
    let res = tokio::task::spawn_blocking(move || {
        // 跨支柱补缝：若未显式提供 prompt_embedding，但提供了自然语言 prompt，自动通过 BekkoEmbedder 计算聚类引导向量
        let prompt_emb = match (req.prompt_embedding, req.prompt.as_deref()) {
            (Some(emb), _) => Some(emb),
            (None, Some(p)) if !p.trim().is_empty() => {
                omni_pro::text::BekkoEmbedder::new().embed(p).ok()
            }
            _ => None,
        };

        search.cluster(
            &req.documents,
            prompt_emb.as_deref(),
            req.distance_threshold,
            req.max_leaf_size,
        )
    })
    .await;
    match res {
        Ok(Ok(tree)) => Json(SearchClusterResponse {
            success: true,
            result: Some(tree),
            error: None,
        }),
        Ok(Err(e)) => Json(SearchClusterResponse {
            success: false,
            result: None,
            error: Some(e.to_string()),
        }),
        Err(e) => Json(SearchClusterResponse {
            success: false,
            result: None,
            error: Some(format!("Task panic: {e}")),
        }),
    }
}

/// 单指标音频转录处理: POST /api/audio/transcribe
async fn audio_transcribe_handler(
    State(state): State<AppState>,
    Json(req): Json<AudioTranscribeRequest>,
) -> Json<AudioTranscribeResponse> {
    let cfg = state.config.lock().unwrap().clone();
    let t_start = std::time::Instant::now();
    let file_path = req.file_path.clone();
    // 优先使用请求中的截取时长，否则使用配置默认值
    let duration_seconds = req.duration_seconds.unwrap_or(cfg.audio_analysis_duration);
    let language = req.language.clone();

    let mut transcript = None;
    let mut events = Vec::new();

    // Step 1: 优先通过 SenseVoice 转录（截取降噪 → ASR）
    let file_path_c = file_path.clone();
    let lang_c = language.clone();
    let sense_result = tokio::task::spawn_blocking(move || {
        let wav_path = convert_audio_standard(&file_path_c, duration_seconds)?;
        transcribe_with_sense_asr(&wav_path, lang_c.as_deref())
    })
    .await
    .ok()
    .flatten();

    if let Some(text) = sense_result {
        transcript = Some(text);
    } else {
        // Step 2: 降级：从 OmniExtractor 元数据中取 audio_transcript（若已有提取结果）
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

/// 将音频/视频截取指定时长并降噪重采样为标准格式 (16kHz Mono PCM WAV)
/// 结果缓存于系统临时目录 firefly-ai-audio-cache/<md5(path+duration)>.wav
/// 成功时返回缓存文件路径；失败时返回 None
fn convert_audio_standard(file_path: &str, duration_seconds: u32) -> Option<PathBuf> {
    // 复用 omni-cover video.rs 的 resolve_ffmpeg 定位思路（直接内联实现以解耦）
    let ffmpeg_exe = if cfg!(target_os = "windows") { "ffmpeg.exe" } else { "ffmpeg" };
    let ffmpeg = {
        let search_roots = [
            std::env::current_dir().unwrap_or_default(),
            std::env::current_exe()
                .map(|p| p.parent().unwrap_or(p.as_path()).to_path_buf())
                .unwrap_or_default(),
        ];
        let mut found: Option<PathBuf> = None;
        'outer: for root in &search_roots {
            let mut cur = root.clone();
            for _ in 0..8 {
                let candidates = [
                    cur.join(format!("apps/desktop/build/extraResources/bin/ffmpeg/{}", ffmpeg_exe)),
                    cur.join(format!("resources/bin/ffmpeg/{}", ffmpeg_exe)),
                    cur.join(format!("resources/bin/{}", ffmpeg_exe)),
                    cur.join(format!("Contents/Resources/bin/ffmpeg/{}", ffmpeg_exe)),
                ];
                for c in &candidates {
                    if c.exists() {
                        found = Some(c.clone());
                        break 'outer;
                    }
                }
                if let Some(parent) = cur.parent() {
                    cur = parent.to_path_buf();
                } else {
                    break;
                }
            }
        }
        // 兜底：系统 PATH
        found.or_else(|| {
            std::process::Command::new("where")
                .arg(ffmpeg_exe)
                .output()
                .ok()
                .and_then(|out| {
                    String::from_utf8(out.stdout).ok()
                        .and_then(|s| s.lines().next().map(|l| PathBuf::from(l.trim())))
                })
        })?
    };

    // 以 md5(file_path + duration) 为缓存键
    let cache_key = {
        let raw = format!("{}:{}", file_path, duration_seconds);
        let digest = md5_hex(raw.as_bytes());
        digest
    };
    let cache_dir = std::env::temp_dir().join("firefly-ai-audio-cache");
    let _ = std::fs::create_dir_all(&cache_dir);
    let cache_path = cache_dir.join(format!("{}.wav", cache_key));

    if cache_path.exists() {
        tracing::info!("[OmniServer] 音频转换缓存命中: {:?}", cache_path);
        return Some(cache_path);
    }

    tracing::info!(
        "[OmniServer] 开始音频截取降噪: file={}, duration={}s, output={:?}",
        file_path, duration_seconds, cache_path
    );

    // ffmpeg -y -i <input> -t <duration> -af highpass=f=80,lowpass=f=7800,afftdn=nf=-25dB -ar 16000 -ac 1 -c:a pcm_s16le <output>
    let status = std::process::Command::new(&ffmpeg)
        .args([
            "-y",
            "-i", file_path,
            "-t", &duration_seconds.to_string(),
            "-af", "highpass=f=80,lowpass=f=7800,afftdn=nf=-25dB",
            "-ar", "16000",
            "-ac", "1",
            "-c:a", "pcm_s16le",
            cache_path.to_str().unwrap_or(""),
        ])
        .status();

    match status {
        Ok(s) if s.success() && cache_path.exists() => {
            tracing::info!("[OmniServer] 音频截取降噪完成: {:?}", cache_path);
            Some(cache_path)
        }
        Ok(s) => {
            tracing::warn!("[OmniServer] ffmpeg 音频转换失败: exit={}", s);
            None
        }
        Err(e) => {
            tracing::warn!("[OmniServer] ffmpeg 启动失败: {}", e);
            None
        }
    }
}

/// 简单 MD5 十六进制字符串（内联实现，不引入额外依赖）
fn md5_hex(data: &[u8]) -> String {
    // 使用 std 库无 md5 依赖的简易 hash（Rust 标准库没有 md5，用 FNV-1a 64-bit 代替）
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x00000100000001b3);
    }
    format!("{:016x}", h)
}

/// 使用 audio.cpp (sense_asr) 对 WAV 文件进行语音转录，返回转录文本
fn transcribe_with_sense_asr(wav_path: &PathBuf, language: Option<&str>) -> Option<String> {
    let audio_exe_name = if cfg!(target_os = "windows") { "audio.exe" } else { "audio" };
    let model_gguf_name = "sensevoice-small-q4_k.gguf";

    // 定位 audio.exe
    let audio_exe = {
        let search_roots = [
            std::env::current_dir().unwrap_or_default(),
            std::env::current_exe()
                .map(|p| p.parent().unwrap_or(p.as_path()).to_path_buf())
                .unwrap_or_default(),
        ];
        let mut found: Option<PathBuf> = None;
        'outer: for root in &search_roots {
            let mut cur = root.clone();
            for _ in 0..8 {
                let candidates = [
                    cur.join(format!("apps/desktop/build/extraResources/bin/audio-cpp/{}", audio_exe_name)),
                    cur.join(format!("resources/bin/audio-cpp/{}", audio_exe_name)),
                    cur.join(format!("resources/bin/{}", audio_exe_name)),
                ];
                for c in &candidates {
                    if c.exists() {
                        found = Some(c.clone());
                        break 'outer;
                    }
                }
                if let Some(parent) = cur.parent() {
                    cur = parent.to_path_buf();
                } else {
                    break;
                }
            }
        }
        found?
    };

    // 定位 sensevoice-small-q4_k.gguf
    let model_path = {
        let search_roots = [
            std::env::current_dir().unwrap_or_default(),
            std::env::current_exe()
                .map(|p| p.parent().unwrap_or(p.as_path()).to_path_buf())
                .unwrap_or_default(),
        ];
        let mut found: Option<PathBuf> = None;
        'outer: for root in &search_roots {
            let mut cur = root.clone();
            for _ in 0..8 {
                let candidates = [
                    cur.join(format!("apps/desktop/build/extraResources/models/sensevoice/{}", model_gguf_name)),
                    cur.join(format!("resources/models/sensevoice/{}", model_gguf_name)),
                    cur.join(format!("resources/sensevoice/{}", model_gguf_name)),
                ];
                for c in &candidates {
                    if c.exists() {
                        found = Some(c.clone());
                        break 'outer;
                    }
                }
                if let Some(parent) = cur.parent() {
                    cur = parent.to_path_buf();
                } else {
                    break;
                }
            }
        }
        found?
    };

    tracing::info!(
        "[OmniServer] 开始 SenseVoice 语音转录: wav={:?}, model={:?}",
        wav_path, model_path
    );

    // 调用 audio.cpp CLI: audio --task asr --family sense_asr --model <gguf> --audio <wav>
    let mut cmd = std::process::Command::new(&audio_exe);
    cmd.args([
        "--task", "asr",
        "--family", "sense_asr",
        "--model", model_path.to_str().unwrap_or(""),
        "--audio", wav_path.to_str().unwrap_or(""),
    ]);
    if let Some(lang) = language {
        // 取前2位作为语言代码
        let lang_code = &lang[..lang.len().min(2)];
        cmd.args(["--language", lang_code]);
    }

    let output = cmd.output().ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        tracing::warn!("[OmniServer] SenseVoice 转录失败: stderr={}", stderr.trim());
        return None;
    }

    let text = stdout.trim().to_string();
    if text.is_empty() {
        tracing::info!("[OmniServer] SenseVoice 转录结果为空");
        None
    } else {
        tracing::info!(
            "[OmniServer] SenseVoice 转录成功: len={}, preview={}...",
            text.len(),
            &text[..text.len().min(100)]
        );
        Some(text)
    }
}

/// 音频标准化转换接口: POST /api/audio/convert
/// 截取指定时长、降噪、重采样至 16kHz Mono PCM WAV（适配 SenseVoice 等端侧模型）
async fn audio_convert_handler(
    State(state): State<AppState>,
    Json(req): Json<AudioConvertRequest>,
) -> Json<AudioConvertResponse> {
    let cfg = state.config.lock().unwrap().clone();
    let t_start = std::time::Instant::now();
    let file_path = req.file_path.clone();
    let duration_seconds = req.duration_seconds.unwrap_or(cfg.audio_analysis_duration);

    let file_path_c = file_path.clone();
    let output_path = tokio::task::spawn_blocking(move || {
        convert_audio_standard(&file_path_c, duration_seconds)
    }).await.ok().flatten();

    let duration_ms = t_start.elapsed().as_millis() as u64;
    let output_path_str = output_path
        .as_ref()
        .and_then(|p| p.to_str())
        .unwrap_or("")
        .to_string();

    Json(AudioConvertResponse {
        file_path,
        output_path: output_path_str,
        duration_seconds,
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


