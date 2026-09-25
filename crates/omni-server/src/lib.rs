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
    tag_identity::{normalize_tag_set_to_codes, normalize_tag_to_code, tag_matches_concept},
    tag_thresholds::{
        EXIT_CONFIDENCE_THRESHOLD, LAYER_FALLBACK_CLIP, LAYER_FALLBACK_OCR,
        LAYER_FALLBACK_PHYSICAL,
    },
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

pub use omni_pro::{OmwDb, SemanticPackLoader, VectorEngine, VectorMatch, VECTOR_DIM};
use omni_pro::omw_query;
use omni_pro::{
    OmwAntonymResult, OmwAntonymsRequest, OmwDescribeRequest, OmwHierarchyRequest, OmwLookupRequest,
    OmwMappingRequest, OmwSynsetNode, OmwSynsetResult, OmwTagResult, OmwTreeRequest, TreeNode,
    UnmappedStats,
};

pub mod routes;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Mutex<OmniConfig>>,
    /// 离线反向地理编码服务（数据集缺失或开源存根时为软不可用实例）
    pub geo: Arc<omni_pro::geo::GeoService>,
    /// OpenHowNet 语义槽位挖掘与对齐服务（数据集缺失或开源存根时为软不可用实例）
    pub hownet: Arc<omni_pro::hownet::OmniHowNetService>,
    /// 工业级混合检索与约束聚类服务 (支柱 5)
    pub search: Arc<omni_pro::search::OmniSearchService>,
    /// OMW 多语言标签词库只读连接池（未传入 --db-path 时为软不可用实例）
    pub omw: OmwDb,
    /// 阿里巴巴 zvec 嵌入式向量引擎 (RaBitQ + INT8 量化，适配 bekko-a8m 384 维，闭源优先 🔒)
    pub vector: Arc<VectorEngine>,
}

#[derive(Deserialize)]
pub struct ExtractRequest {
    pub file_path: String,
}

#[derive(Deserialize)]
pub struct FilePreviewRequest {
    pub path: String,
}

/// OMW 词库热重连请求体: POST /api/reconnect（dbPath 为 null/空串时表示断开直连）
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconnectRequest {
    #[serde(default)]
    pub db_path: Option<String>,
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
    // 1. 基础开源接口 (Open-Core Endpoints)
    let mut router = Router::new()
        .route(
            "/health",
            get(health_handler),
        )
        .route("/api/version", get(version_handler))
        .route("/api/config", get(get_config).post(update_config).put(update_config))
        .route("/api/extract", post(extract_file_handler))
        .route("/api/extract/upload", post(extract_multipart_handler));

    // 2. 闭源专业版专享接口 (Closed-Source Pro Endpoints 🔒: 一次开发闭源优先)
    // 依据项目架构准则：除以上四个基础接口外，其余全部归属 Pro 闭源优先体系
    if omni_pro::is_pro_enabled() {
        router = router
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
            .route("/api/taxonomy/resolve-parent", post(taxonomy_resolve_parent_handler))
            .route("/api/reconnect", post(reconnect_omw_handler))
            .route("/api/v1/omw/lookup", post(omw_lookup_handler))
            .route("/api/v1/omw/hierarchy", post(omw_hierarchy_handler))
            .route("/api/v1/omw/antonyms", post(omw_antonyms_handler))
            // 概念描述句生成 API 已按 wayfinder #669 退役（恒返回 null）
            .route("/api/v1/omw/describe", post(omw_describe_handler))
            .route("/api/v1/omw/mapping", post(omw_mapping_handler))
            .route("/api/v1/omw/tree", get(omw_tree_handler))
            .route("/api/v1/omw/unmapped-stats", get(omw_unmapped_stats_handler))
            // ADR-0038 / PRD #679: 只读语义包与向量引擎专用服务化出口 (Pro 独占 🔒)
            .route("/api/v1/taxonomy/tree", get(routes::taxonomy::taxonomy_tree_handler))
            .route("/api/v1/taxonomy/aliases", get(routes::taxonomy::taxonomy_aliases_handler))
            .route("/api/taxonomy/fast-recognize", post(routes::taxonomy::fast_recognize_handler))
            .route("/api/v1/taxonomy/fast-recognize", post(routes::taxonomy::fast_recognize_handler))
            .route("/api/v1/vector/upsert", post(routes::vector::vector_upsert_handler))
            .route("/api/v1/vector/search", post(routes::vector::vector_search_handler))
            .route(
                "/api/v1/vector/delete",
                axum::routing::delete(routes::vector::vector_delete_handler)
                    .post(routes::vector::vector_delete_handler),
            )
            // ADR-0039 / Ticket 1: 高速未分析文件秒搜与密集向量段落对齐
            .route("/api/v1/search/fs", get(routes::fs_search::fs_search_handler))
            .route("/api/v1/vector/match-passages", post(routes::vector::match_passages_handler));
    }

    router
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
    let search_available = if is_pro {
        let search = state.search.clone();
        // 反映索引服务真实可用性：ensure_index 首调会打开/创建索引入口，
        // 磁盘或权限异常时返回 Err → false（而非仅凭 is_pro 恒真判断）
        tokio::task::spawn_blocking(move || search.ensure_index().is_ok())
            .await
            .unwrap_or(false)
    } else {
        false
    };
    // OMW 词库可用性：实际执行一次 SELECT 1 探测只读连接（与 is_pro 无关，直连会话独立）
    let omw = state.omw.clone();
    let omw_available = tokio::task::spawn_blocking(move || omw.validate())
        .await
        .unwrap_or(false);
    Json(serde_json::json!({
        "status": "ok",
        "server": "firefly-omni",
        "version": env!("CARGO_PKG_VERSION"),
        "isPro": is_pro,
        "geoAvailable": geo_available,
        "hownetAvailable": hownet_available,
        "searchAvailable": search_available,
        "cleanupAvailable": is_pro,
        "omwAvailable": omw_available
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

/// OMW 词库热重连: POST /api/reconnect
///
/// 支撑桌面端多语言切换：接收 `{ "dbPath": "..." }` 原子替换只读连接池，无需重启服务进程；
/// dbPath 为 null/空串时断开直连恢复初始不可用态。失败时保持原连接不变（软失败 200 回传）。
async fn reconnect_omw_handler(
    State(state): State<AppState>,
    Json(req): Json<ReconnectRequest>,
) -> Json<serde_json::Value> {
    let db_path = req.db_path.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let result = match db_path {
        Some(path) => state.omw.connect(path),
        None => {
            state.omw.disconnect();
            Ok(())
        }
    };

    match result {
        Ok(()) => Json(serde_json::json!({
            "status": "ok",
            "omwAvailable": state.omw.is_available(),
            "dbPath": state.omw.db_path().map(|p| p.to_string_lossy().to_string()),
            "reason": null
        })),
        Err(err) => Json(serde_json::json!({
            "status": "error",
            // 失败时原连接保持不变，如实反映当前真实可用性
            "omwAvailable": state.omw.is_available(),
            "dbPath": state.omw.db_path().map(|p| p.to_string_lossy().to_string()),
            "reason": err.to_string()
        })),
    }
}

/// OMW 只读查询统一软失败包装
///
/// 连接未配置（未传 --db-path / 已 disconnect）、查询报错或阻塞任务 panic 时，
/// 一律记录 warn 日志并回退默认值，与桌面端 `omw*` 方法「出错返回空结果」的语义一致；
/// 真实可用性由 `/health` 的 `omwAvailable` 单独反映。
async fn omw_query_or_default<T, F>(omw: OmwDb, label: &'static str, default: T, query: F) -> T
where
    T: Send + 'static,
    F: FnOnce(&rusqlite::Connection) -> anyhow::Result<T> + Send + 'static,
{
    match tokio::task::spawn_blocking(move || omw.with_conn(query)).await {
        Ok(Ok(value)) => value,
        Ok(Err(err)) => {
            tracing::warn!("OMW {label} 查询失败，回退默认结果: {err}");
            default
        }
        Err(err) => {
            tracing::warn!("OMW {label} 查询任务执行失败，回退默认结果: {err}");
            default
        }
    }
}

/// OMW 词汇查 synset: POST /api/v1/omw/lookup
async fn omw_lookup_handler(
    State(state): State<AppState>,
    Json(req): Json<OmwLookupRequest>,
) -> Json<Vec<OmwSynsetResult>> {
    let language = req.language.unwrap_or_else(|| "en".to_string());
    let word = req.word;
    Json(
        omw_query_or_default(state.omw.clone(), "lookup", Vec::new(), move |conn| {
            omw_query::lookup(conn, &word, &language)
        })
        .await,
    )
}

/// OMW 层级链查询: POST /api/v1/omw/hierarchy
async fn omw_hierarchy_handler(
    State(state): State<AppState>,
    Json(req): Json<OmwHierarchyRequest>,
) -> Json<Vec<OmwSynsetNode>> {
    let synset_id = req.synset_id;
    Json(
        omw_query_or_default(state.omw.clone(), "hierarchy", Vec::new(), move |conn| {
            omw_query::hierarchy(conn, &synset_id)
        })
        .await,
    )
}

/// OMW 反义词查询: POST /api/v1/omw/antonyms
async fn omw_antonyms_handler(
    State(state): State<AppState>,
    Json(req): Json<OmwAntonymsRequest>,
) -> Json<Vec<OmwAntonymResult>> {
    let language = req.language.unwrap_or_else(|| "cmn".to_string());
    let word = req.word;
    Json(
        omw_query_or_default(state.omw.clone(), "antonyms", Vec::new(), move |conn| {
            omw_query::antonyms(conn, &word, &language)
        })
        .await,
    )
}

/// OMW 概念描述句 API 已退役（wayfinder #669 / 决策包：删除 omwGenerateDescription 等价物）。
/// 路由 `/api/v1/omw/describe` 已摘除；本函数保留仅为避免编译引用断裂，恒返回 null。
#[allow(dead_code)]
async fn omw_describe_handler(
    State(_state): State<AppState>,
    Json(_req): Json<OmwDescribeRequest>,
) -> Json<Option<String>> {
    Json(None)
}

/// OMW 标签映射反查: POST /api/v1/omw/mapping（零桥表依赖，见 omw_query::mapping）
async fn omw_mapping_handler(
    State(state): State<AppState>,
    Json(req): Json<OmwMappingRequest>,
) -> Json<Vec<OmwTagResult>> {
    let tag_name = req.tag_name;
    Json(
        omw_query_or_default(state.omw.clone(), "mapping", Vec::new(), move |conn| {
            omw_query::mapping(conn, &tag_name)
        })
        .await,
    )
}

/// OMW 统一标签树单层懒加载: GET /api/v1/omw/tree?root={code}&depth=1
async fn omw_tree_handler(
    State(state): State<AppState>,
    Query(req): Query<OmwTreeRequest>,
) -> Json<Vec<TreeNode>> {
    Json(
        omw_query_or_default(state.omw.clone(), "tree", Vec::new(), move |conn| {
            omw_query::tree(conn, &req.root, req.depth)
        })
        .await,
    )
}

/// OMW 未映射概念聚合统计: GET /api/v1/omw/unmapped-stats
async fn omw_unmapped_stats_handler(
    State(state): State<AppState>,
) -> Json<UnmappedStats> {
    Json(
        omw_query_or_default(state.omw.clone(), "unmapped-stats", UnmappedStats { by_lexfile: Vec::new(), by_top_ancestor: Vec::new() }, move |conn| {
            omw_query::unmapped_stats(conn)
        })
        .await,
    )
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
        // 目录名优先取主进程注入的 APP_NAME（如 firefly-ai-folder-intl），
        // 避免硬编码裸 firefly-ai-folder 在 appData 下创建多余目录
        let app_name = std::env::var("APP_NAME")
            .ok()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| "firefly-ai-folder".to_string());
        let dir = PathBuf::from(appdata).join(app_name);
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

pub async fn start_server(addr: SocketAddr, db_path: Option<PathBuf>) -> anyhow::Result<()> {
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
    // HowNet 独立数据库已废弃，能力已完全合流至 semantic.pack；保持软不可用存根服务
    let hownet = Arc::new(omni_pro::hownet::OmniHowNetService::unavailable());
    // 索引目录发现链：优先持久化用户数据目录（APPDATA → LOCALAPPDATA → USERPROFILE），
    // 全部缺失时才降级到系统临时目录（临时目录存在被系统清理导致索引重建的风险，仅作兜底）
    let search_dir = if let Ok(appdata) = std::env::var("APPDATA") {
        PathBuf::from(appdata).join("firefly-ai-folder").join("search_index")
    } else if let Ok(local_appdata) = std::env::var("LOCALAPPDATA") {
        PathBuf::from(local_appdata).join("firefly-ai-folder").join("search_index")
    } else if let Ok(home) = std::env::var("USERPROFILE") {
        PathBuf::from(home).join(".firefly-ai-folder").join("search_index")
    } else {
        std::env::temp_dir().join("firefly_omni_search_index")
    };
    let search = Arc::new(omni_pro::search::OmniSearchService::new(search_dir));
    // OMW 词库直连与只读包零磁盘内存挂载 (ADR-0038 / PRD #679: 闭源优先 🔒)
    let omw = OmwDb::unavailable();
    if let Some(pack_path) = SemanticPackLoader::discover_pack_path() {
        match SemanticPackLoader::load_pack_raw_from_file(&pack_path) {
            Ok(bytes) => {
                match omw.load_pack_bytes(&bytes) {
                    Ok(()) => info!("semantic.pack zero-disk mounted to OmwDb from {}", pack_path.display()),
                    Err(err) => tracing::warn!("Failed to mount semantic.pack: {err}"),
                }
            }
            Err(err) => tracing::warn!("Failed to load semantic.pack at {}: {err}", pack_path.display()),
        }
    }

    if let Some(path) = &db_path {
        match omw.connect(path) {
            Ok(()) => info!("omw db connected read-only at {}", path.display()),
            Err(err) => {
                tracing::warn!("omw db open failed ({}), omw subsystem starts unavailable", err)
            }
        }
    }

    // 阿里巴巴 zvec 嵌入式向量引擎 (RaBitQ + INT8 量化，适配 bekko-a8m 384 维)
    let vector = Arc::new(VectorEngine::open_default().unwrap_or_else(|err| {
        tracing::warn!("Failed to open default vector engine ({err}), falling back to memory");
        VectorEngine::in_memory()
    }));

    let state = AppState {
        config: Arc::new(Mutex::new(initial_config)),
        geo,
        hownet,
        search,
        omw,
        vector,
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
    /// (WP2b) CLIP 标定置信度快照：标签名 → 标定分，供 6.2 TagChainItem.confidence 贯通
    clip_tag_confs: std::collections::HashMap<String, f32>,
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
                // (WP2b) 带分提取：分数在提取层保留，贯通至 TagChainItem.confidence，杜绝 0.92 硬编码灌水
                let h_clip = s.spawn(|| {
                    let t = std::time::Instant::now();
                    let res = if enable_visual_tags {
                        omni_pro::OmniVisionEngine::extract_clip_visual_tags_scored_from_image(
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
                let (ct_scored, ct_ms) = h_clip.join().unwrap_or((Vec::new(), 0));
                let (np, np_ms) = h_nsfw.join().unwrap_or((None, 0));
                let (wl, wl_ms) = h_wm.join().unwrap_or((0, 0));
                let (ml, ml_ms) = h_mc.join().unwrap_or((0, 0));
                let (ar, ar_ms) = h_aes.join().unwrap_or(((7.5, Vec::new()), 0));
                let (bw, bw_ms) = h_bw.join().unwrap_or((false, 0));
                let (ram_res, ram_ms) = h_ram.join().unwrap_or((Vec::new(), 0));
                let img_embed = h_embed.join().unwrap_or(None);

                out.has_text = Some(td);
                out.text_detect_ms = td_ms;
                // (WP2b) 分数快照与标签名拆分：clip_tags 保持 Vec<String> 兼容既有门禁/消歧流程
                out.clip_tag_confs = ct_scored.iter().cloned().collect();
                out.clip_tags = ct_scored.into_iter().map(|(t, _)| t).collect();
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
                // Spec D9：闭环规则按概念 code 匹配，兼容中英别名
                if !td {
                    out.clip_tags.retain(|t| {
                        !tag_matches_concept(t, "海报宣发") && !tag_matches_concept(t, "截图")
                    });
                }

                // CLIP 高置信度标签直接截取 Top 5，消除第 2 次模型重复推理
                out.clip_high_confidence_tags = out.clip_tags.iter().take(5).cloned().collect();

                // 漫画细分标签形态门禁：仅当内容明确具有动漫/漫画/插画特征时，才根据版式推导条漫/页漫
                let is_anime_art = out.clip_tags.iter().any(|t| {
                    tag_matches_concept(t, "二次元")
                        || tag_matches_concept(t, "动漫")
                        || tag_matches_concept(t, "插画")
                        || tag_matches_concept(t, "漫画")
                        || tag_matches_concept(t, "手绘")
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
    // 一次性读取文件元数据，file_size 与 Tier1 文本分析的 mtime 均复用该结果，避免重复 syscall
    let file_meta = std::fs::metadata(p).ok();
    let file_size = file_meta.as_ref().map(|m| m.len()).unwrap_or(0);
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
    // (WP2b) CLIP 标定分快照（名 → 分），供 engine map 与 6.2 confidence 贯通
    let clip_tag_confs = vision_res.clip_tag_confs;

    // 基于实际 OCR 文本正向直通校准截图形态与文字客观事实 (彻底根除有字却输出无字图的倒挂)
    // (WP2a) ocr_forced_tag_names: OCR 事实强制注入的标签名，用于后续 engine 来源标记 (最高物理事实优先级)
    let mut ocr_forced_tag_names: Vec<String> = Vec::new();
    if !markdown_content.trim().is_empty() {
        has_text = Some(true);
        clip_tags.retain(|t| !tag_matches_concept(t, "无字图"));
        if !clip_tags.iter().any(|t| tag_matches_concept(t, "有字图")) {
            clip_tags.push("有字图".to_string());
            ocr_forced_tag_names.push("有字图".to_string());
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
            if !clip_tags.iter().any(|t| tag_matches_concept(t, "代码截图")) {
                clip_tags.retain(|t| !tag_matches_concept(t, "聊天截图"));
                clip_tags.push("代码截图".to_string());
                ocr_forced_tag_names.push("代码截图".to_string());
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
    let mobilenet_tags = if !clip_mutual_tags.is_empty() {
        let mut merged = mobilenet_tags.clone();
        for (tag, _conf, _group) in &clip_mutual_tags {
            if !merged.contains(tag) {
                merged.push(tag.clone());
            }
        }
        // CLIP 互斥分类保证组内只有一个胜出者，移除被覆盖的规则推导冲突项
        // 1. 色彩模式：CLIP 结果与黑白检测结果互斥对齐（概念 code 匹配，兼容中英）
        if clip_mutual_tags.iter().any(|(t, _, g)| *g == "色彩模式" && tag_matches_concept(t, "全彩"))
        {
            merged.retain(|t| !tag_matches_concept(t, "黑白"));
        } else if clip_mutual_tags
            .iter()
            .any(|(t, _, g)| *g == "色彩模式" && tag_matches_concept(t, "黑白"))
        {
            merged.retain(|t| !tag_matches_concept(t, "全彩"));
        }
        // 2. 文字存在性：客观检测与 OCR 事实优先于语义猜测
        if has_text == Some(true) || !markdown_content.trim().is_empty() {
            merged.retain(|t| !tag_matches_concept(t, "无字图"));
            if !merged.iter().any(|t| tag_matches_concept(t, "有字图")) {
                merged.push("有字图".to_string());
            }
        } else if clip_mutual_tags
            .iter()
            .any(|(t, _, g)| *g == "文字存在性" && tag_matches_concept(t, "有字图"))
        {
            merged.retain(|t| !tag_matches_concept(t, "无字图"));
        } else if clip_mutual_tags
            .iter()
            .any(|(t, _, g)| *g == "文字存在性" && tag_matches_concept(t, "无字图"))
        {
            merged.retain(|t| !tag_matches_concept(t, "有字图"));
        }
        merged
    } else {
        if has_text == Some(true) || !markdown_content.trim().is_empty() {
            mobilenet_tags.retain(|t| !tag_matches_concept(t, "无字图"));
            if !mobilenet_tags.iter().any(|t| tag_matches_concept(t, "有字图")) {
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
        .chain(ram_tags.iter().map(|r| &r.name))
    {
        if !detected_visual_tags.contains(tag) {
            detected_visual_tags.push(tag.clone());
        }
    }

    // (WP2a) 来源标记 + (WP2b) 置信度贯通：登记 name → (engine, 优先级, 真实分数 Option) 映射，
    // 优先级 物理事实(ocr) > 互斥组 > 物理规则 > ram > clip > nsfw/quality；
    // 同名多来源时高优先级覆盖（分数随覆盖项携带），6.2 构建 TagChainItem 时按 original_name 反查
    let mut tag_engine_map: std::collections::HashMap<String, (&'static str, u8, Option<f32>)> =
        std::collections::HashMap::new();
    {
        let mut register = |name: &str, engine: &'static str, prio: u8, conf: Option<f32>| {
            match tag_engine_map.get(name) {
                Some((_, p, _)) if *p >= prio => {}
                _ => {
                    tag_engine_map.insert(name.to_string(), (engine, prio, conf));
                }
            }
        };
        for t in &clip_tags {
            // CLIP：提取层标定分 (calibrate_clip_score)，无分回退 0.55 起步值
            register(t, "clip", 2, clip_tag_confs.get(t).copied());
        }
        for t in &mobilenet_tags {
            // 物理规则推导：无模型分数，分层回退 0.90
            register(t, "physical", 4, None);
        }
        for t in &nsfw_tags {
            register(t, "nsfw", 1, None);
        }
        for t in &quality_issues {
            register(t, "quality", 1, None);
        }
        for r in &ram_tags {
            // RAM++：真实模型置信度直接贯通
            register(&r.name, "ram", 3, Some(r.confidence));
        }
        for (tag, conf, _group) in &clip_mutual_tags {
            // 互斥组胜出项：组内点积经同一标定函数换算
            register(
                tag,
                "mutual_group",
                5,
                Some(omni_pro::OmniVisionEngine::calibrate_clip_score(*conf)),
            );
        }
        for t in &ocr_forced_tag_names {
            // OCR 物理事实：无模型分数，分层回退 0.99
            register(t, "ocr", 6, None);
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

    // (WP3) 内容域矩阵门禁：按声明式矩阵（domain_tag_matrix.json）执行
    // ① 域不适用互斥组胜出项剔除（截图域压制「作品题材设定」→ 清除“体育”类自然内容组噪声）
    // ② 域不适用来源层整体压制（截图域压制 ram 来源 → 清除“癌症/药物”等 RAM 自然图像模型噪声）
    // ③ 跨域禁用标签剔除（concept code 匹配，兼容中英别名）
    let engine_lookup: std::collections::HashMap<String, String> = tag_engine_map
        .iter()
        .map(|(name, (engine, _, _))| (name.clone(), engine.to_string()))
        .collect();
    omni_pro::OmniVisionEngine::apply_domain_matrix_gating(
        &mut detected_visual_tags,
        &clip_mutual_tags,
        photo_type.as_deref(),
        &engine_lookup,
    );

    // 文字存在性绝对保护：若已探活出文字或 OCR 内容，无条件排除无字图，确保有字图存在
    if has_text == Some(true) || !markdown_content.trim().is_empty() {
        detected_visual_tags.retain(|t| !tag_matches_concept(t, "无字图"));
        if !detected_visual_tags.iter().any(|t| tag_matches_concept(t, "有字图")) {
            detected_visual_tags.push("有字图".to_string());
        }
    }

    // Spec D12：统一归一为稳定 code 后再做集合同步，保证 zh/en 输入幂等
    // 修复(问题2)：normalize 前先保留「原始名 → code」映射，供 6.2 步骤反查真实展示名
    let detected_visual_tag_name_to_code: std::collections::HashMap<String, String> = detected_visual_tags
        .iter()
        .map(|name| (name.clone(), normalize_tag_to_code(name)))
        .collect();
    let detected_visual_tags = normalize_tag_set_to_codes(&detected_visual_tags);

    // 修复(问题4)：clip/mobilenet 只用 code 集合做内部比对，保留原始名供输出
    // 同步清洗 mobilenet_tags 与 clip_tags，确保互斥清洗结果一致贯通（防止被清洗的子标签混入下游主体池）
    // 输出保留原始中文/英文名（debug 字段，与 ram_tags 保持一致）
    let mobilenet_tags: Vec<String> = mobilenet_tags
        .into_iter()
        .filter(|name| {
            let code = normalize_tag_to_code(name);
            detected_visual_tags.contains(&code)
        })
        .collect();
    let clip_tags: Vec<String> = clip_tags
        .into_iter()
        .filter(|name| {
            let code = normalize_tag_to_code(name);
            detected_visual_tags.contains(&code)
        })
        .collect();
    // RAM 对称治理 (P3 来源对称)：与 clip/mobilenet 一致，按 gated 后 code 集回写过滤，
    // 杜绝原始 ram_tags 经「6.1 直注 / ram_tags_flat / 级联主体池」三条通道绕过互斥门禁
    let gated_ram_tags: Vec<omni_core::RamTagItem> = ram_tags
        .iter()
        .filter(|r| {
            let code = normalize_tag_to_code(&r.name);
            detected_visual_tags.contains(&code)
        })
        .cloned()
        .collect();

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
        || nsfw_tags.iter().any(|t| tag_matches_concept(t, "色情") || tag_matches_concept(t, "R-18") || tag_matches_concept(t, "R-18G"))
        || content_rating.as_deref() == Some("r18")
        || content_rating.as_deref() == Some("r18g")
    {
        security_level_code = Some("confidential".to_string());
        security_level = Some("保密".to_string());
    }

    // 6. 统一汇聚各大引擎标签并解析为标签链 (TagChainItem) 结构
    let mut structured_visual_tags: Vec<omni_core::TagChainItem> = Vec::new();

    // 6.1 首先将已具备完整维度与逻辑泛维度的 RAM++ 标签加入
    // (P3 来源对称：只注入互斥门禁后存活的 gated_ram_tags，原始 ram_tags 不得绕过门禁直注)
    for r in &gated_ram_tags {
        if !structured_visual_tags.iter().any(|t| t.name.eq_ignore_ascii_case(&r.name)) {
            let mut item = r.clone();
            // (WP2a) 标记打标引擎来源
            item.engine = Some("ram".to_string());
            structured_visual_tags.push(item);
        }
    }

    // 6.2 将其他各大引擎的有效视觉标签 (经过互斥门禁过滤的 detected_visual_tags) 统一映射为 TagChainItem
    // 修复(问题2)：detected_visual_tags 此时为 code 字符串（normalize 后），
    // 需先从 name→code 映射反查原始展示名，再构建 TagChainItem，防止 name 被写成 code 串
    for raw_code in &detected_visual_tags {
        // 按 code 去重（normalize 后 code 已稳定，避免同一概念 zh/en 名称不同但指向同 code 时重复添加）
        if !structured_visual_tags.iter().any(|t| &t.code == raw_code) {
            // 从 normalize 前的映射反查原始名（找不到则用 code 作为 fallback）
            let original_name = detected_visual_tag_name_to_code
                .iter()
                .find(|(_name, code): &(&String, &String)| code.as_str() == raw_code.as_str())
                .map(|(name, _code): (&String, &String)| name.clone())
                .unwrap_or_else(|| raw_code.clone());
            // (WP2b) 分层置信度：优先真实分数（CLIP 标定分 / 互斥组标定分 / RAM 真实分），
            // 缺失时按引擎分层回退：OCR事实 0.99 > 物理/互斥/NSFW/画质/RAM 0.90 > CLIP 起步 0.55
            let (engine_name, real_conf) = match tag_engine_map.get(&original_name) {
                Some((engine, _prio, conf)) => (Some(*engine), *conf),
                None => (None, None),
            };
            let fallback_conf = match engine_name {
                Some("ocr") => LAYER_FALLBACK_OCR,
                Some("mutual_group") | Some("physical") | Some("nsfw") | Some("quality")
                | Some("ram") => LAYER_FALLBACK_PHYSICAL,
                Some("clip") => LAYER_FALLBACK_CLIP,
                _ => LAYER_FALLBACK_PHYSICAL,
            };
            let confidence = real_conf.unwrap_or(fallback_conf);
            let mut item = if is_pro {
                // 用原始名查 RAM++ 投影表 / builtin 字典，得到正确的 code + name + parent_codes
                let mut resolved =
                    omni_pro::OmniVisionEngine::resolve_tag_to_chain_item(&original_name, confidence);
                // 确保 code 与归一结果一致（防止别名映射漂移）
                resolved.code = raw_code.clone();
                resolved
            } else {
                omni_core::TagChainItem {
                    code: raw_code.clone(),
                    name: original_name.clone(),
                    confidence,
                    ..Default::default()
                }
            };
            // (WP2a) 按登记的来源映射回填打标引擎
            if let Some(engine) = engine_name {
                item.engine = Some(engine.to_string());
            }
            structured_visual_tags.push(item);
        }
    }

    // 7. 级联提示词合成 + CLIP 向量仲裁终局裁决 (Pro 专享，仅图片路径生效)
    // 注意：此处在 ram_tags 降级为 flat 前调用，以获取完整 TagChainItem 结构
    // (P3 来源对称：级联主体池同样只消费门禁后存活的 gated_ram_tags)
    let (cascade_candidates, winning_hypothesis, activated_dimension_tags, smart_name, content_description, pruned_ambiguous_words) = if is_pro && is_image {
        omni_pro::OmniVisionEngine::synthesize_cascade_hypotheses_and_arbitrate(
            &file_path,
            &detected_visual_tags,
            &gated_ram_tags,
            &mobilenet_tags,
            &nsfw_tags,
            image_embedding.as_deref(),
            req.language.as_deref(),
        )
    } else {
        (Vec::new(), None, Vec::new(), None, None, Vec::new())
    };

    // 6.3 ram_tags 降级为平铺字符串数组 (仅包含门禁后存活的 RAM++ 纯实体标签名)
    let ram_tags_flat: Vec<String> = gated_ram_tags.into_iter().map(|r| r.name).collect();

    let audio_transcript = metadata
        .get("audio_transcript")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let lrc = metadata
        .get("lrc")
        .or_else(|| metadata.get("audio").and_then(|a| a.get("lrc")))
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
    // 开源构建或无配置开关时跳过，避免存根空转下发全零向量并冲击感知 SLO
    let mtime = file_meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .map(|t| chrono::DateTime::<chrono::Utc>::from(t));
    let file_name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();

    let enable_frontend_text = req.enable_text_analysis.unwrap_or(cfg.enable_text_analysis);
    let text_analysis = if is_pro && enable_frontend_text && !markdown_content.trim().is_empty() {
        let t_text = std::time::Instant::now();
        let text = markdown_content.clone();
        let fname = file_name.clone();
        let hownet = state.hownet.clone();
        let res = tokio::task::spawn_blocking(move || {
            omni_pro::text::OmniTextEngine::analyze(&text, &fname, mtime, Some(&*hownet))
        })
        .await;
        let text_duration = t_text.elapsed().as_millis() as u64;
        benchmark.text_ms = Some(benchmark.text_ms.unwrap_or(0) + text_duration);
        match res {
            Ok(result) => Some(result),
            Err(e) => {
                tracing::error!("[OmniServer] 文本分析任务执行失败: {e}");
                None
            }
        }
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

    // 9. 第三阶段: 双锚点交叉向量验证与多模态终局融合 (Task 626)
    // 汇聚第二阶段收集到的所有多模态异构信息为 MultimodalContext
    let multimodal_ctx = omni_core::MultimodalContext {
        file_path: file_path.clone(),
        file_name: file_name.clone(),
        mime_type: mime_type.clone(),
        document_text: if !is_image && !markdown_content.trim().is_empty() { Some(markdown_content.clone()) } else { None },
        ocr_text: ocr_text.clone(),
        audio_transcript: audio_transcript.clone(),
        lrc_text: lrc.clone(),
        visual_tags: structured_visual_tags.clone(),
        exif_metadata: metadata.clone(),
        is_image,
        is_document: !is_image && !is_video,
        is_audio_or_video: is_video || audio_transcript.is_some() || lrc.is_some(),
        language: req.language.clone(),
    };

    let fusion_outcome = if is_pro {
        omni_pro::text::OmniMultimodalFusionEngine::fuse_and_arbitrate(&multimodal_ctx)
    } else {
        omni_pro::text::FusedPerceptionOutcome {
            smart_name: None,
            content_description: None,
            fused_tags: structured_visual_tags.clone(),
            candidate_hypotheses: Vec::new(),
        }
    };

    let candidate_hypotheses = if !fusion_outcome.candidate_hypotheses.is_empty() {
        fusion_outcome.candidate_hypotheses
    } else {
        cascade_candidates
    };

    // 若第三阶段双锚点融合产生了胜出假设，优先更新 winning_hypothesis
    let winning_hypothesis = candidate_hypotheses
        .iter()
        .find(|c| c.is_winner)
        .map(|c| omni_core::WinningHypothesisItem {
            prompt_text: c.prompt_text.clone(),
            confidence: c.confidence,
        })
        .or(winning_hypothesis);

    // 终局智能重命名与描述：优先取第三阶段双锚点交叉验证胜出者，平滑回退
    let smart_name = fusion_outcome.smart_name.or(smart_name).or(text_smart_name);
    let content_description = fusion_outcome.content_description.or(content_description).or_else(|| text_one_desc.clone());
    
    // 置信度门限控制：低于 EXIT_CONFIDENCE_THRESHOLD 的候选标签严禁透出到接口返回结果中 (日志中已输出完整候选打分)
    structured_visual_tags.retain(|t| t.confidence >= EXIT_CONFIDENCE_THRESHOLD);

    let fused_tags = if !fusion_outcome.fused_tags.is_empty() {
        let mut filtered = fusion_outcome.fused_tags;
        filtered.retain(|t| t.confidence >= EXIT_CONFIDENCE_THRESHOLD);
        filtered
    } else {
        structured_visual_tags.clone()
    };

    // 对 fused_tags 中 parent_codes 为空的扩展标签，
    // 通过 TaxonomyVectorBase bekko-a8m 语义向量推导补全 parent_codes，
    // 避免 desktop 端看到空父级或硬编码回退值。
    let fused_tags: Vec<omni_core::TagChainItem> = if is_pro {
        fused_tags.into_iter().map(|mut tag| {
            if tag.parent_codes.is_empty() && tag.code.starts_with("_ext.") {
                let outcome = omni_pro::text::OmniMultimodalFusionEngine::resolve_ext_tag_parent(
                    &tag.name,
                    &tag.code,
                );
                tag.parent_codes = vec![outcome];
            }
            tag
        }).collect()
    } else {
        fused_tags
    };

    benchmark.total_ms = t_start.elapsed().as_millis() as u64;

    tracing::info!(
        "[OmniServer] 原生多模态感知完成: file={}, 耗时={}ms, watermark_level={:?}, mosaic_level={:?}, has_text={:?}, visual_tags_count={}, fused_tags_count={}, ram_tags={:?}, geo={:?}",
        file_path,
        benchmark.total_ms,
        watermark_level,
        mosaic_level,
        has_text,
        structured_visual_tags.len(),
        fused_tags.len(),
        ram_tags_flat,
        geo_address
    );

    // 10. 原生元数据标签抽取 (Task 2)：元数据 + 下沉物理事实 → meta_tags 直出
    // 物理事实置信度 1.0 / 规则推导 0.95，engine 统一为 "metadata"，
    // Desktop 端作为第一权威物理事实无损落库至 file_tags。
    // 语言细分：由请求语言标识归一到母语展示名 (zh* → 中文 / en* → 英文)
    let language_label: Option<String> = req.language.as_deref().and_then(|l| {
        let lower = l.to_ascii_lowercase();
        if lower.starts_with("zh") || lower.starts_with("cmn") {
            Some("中文".to_string())
        } else if lower.starts_with("en") {
            Some("英文".to_string())
        } else {
            None
        }
    });
    let meta_tags: Vec<omni_core::TagChainItem> = {
        let ctx = omni_extract::MetadataTagContext {
            metadata: &metadata,
            file_source: file_source.clone(),
            workflow_state: workflow_state.clone(),
            security_level: security_level.clone(),
            quality_score,
            language_label: language_label.clone(),
        };
        omni_extract::OmniMetadataTagExtractor::extract(&ctx)
    };
    tracing::info!(
        "[元数据抽取:meta_tags] 文件: {}, 产出标签数: {}, 标签: {:?}",
        file_name,
        meta_tags.len(),
        meta_tags.iter().map(|t| t.name.as_str()).collect::<Vec<_>>()
    );

    let result = Json(OmniPerceptionResult {
        file_path: file_path.clone(),
        mime_type,
        file_size,
        category,
        markdown_content: markdown_content.clone(),
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
        lrc,
        audio_events,
        geo_address,
        phash,
        is_corrupted,
        // 级联假设仲裁终局字段
        candidate_hypotheses,
        winning_hypothesis,
        activated_dimension_tags,
        fused_tags,
        meta_tags,
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
    // P4: 开源构建下 omni-pro 为存根（仅空转返回全零向量），直接返回确定性空结果，避免全零 embedding 下发
    if !omni_pro::is_pro_enabled() {
        return Json(empty_text_analysis_result());
    }
    let file_name = req.file_name.unwrap_or_default();
    let hownet = state.hownet.clone();
    let text = req.text;
    let mtime = req.mtime;
    let res = tokio::task::spawn_blocking(move || {
        omni_pro::text::OmniTextEngine::analyze(&text, &file_name, mtime, Some(&*hownet))
    })
    .await;
    Json(match res {
        Ok(result) => result,
        Err(e) => {
            tracing::error!("[OmniServer] 文本分析任务执行失败: {e}");
            // 任务 panic 时返回确定性空结果，避免调用方误判为正常特征
            empty_text_analysis_result()
        }
    })
}

/// 确定性空文本特征分析结果（panic 兜底 / 开源存根模式共用）
fn empty_text_analysis_result() -> omni_pro::text::TextAnalysisResult {
    omni_pro::text::TextAnalysisResult {
        title: None,
        language: "zh".to_string(),
        keywords: Vec::new(),
        entities: Vec::new(),
        structured_summary: Default::default(),
        one_sentence_desc: None,
        smart_name: None,
        name_slots: Default::default(),
        embedding_dense: Vec::new(),
        chunks: Vec::new(),
        duration_ms: 0,
    }
}

/// 语义标签父级求解请求体: POST /api/taxonomy/resolve-parent
#[derive(Deserialize)]
pub struct ResolveParentRequest {
    pub tag_name: String,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub context_hint: Option<String>,
}

/// 语义标签父级求解响应体 (严格对齐 PRD §4.3 B 契约)
#[derive(Serialize)]
pub struct ResolveParentResponse {
    pub success: bool,
    pub parent_code: String,
    pub parent_name: String,
    pub confidence: f32,
    pub suggested_depth: u32,
    pub materialized_paths: Vec<omni_pro::text::MaterializedPathItem>,
}

/// 语义标签父级推荐与物化路径推导 API: POST /api/taxonomy/resolve-parent
async fn taxonomy_resolve_parent_handler(
    Json(req): Json<ResolveParentRequest>,
) -> Json<ResolveParentResponse> {
    let outcome = tokio::task::spawn_blocking(move || {
        let base = omni_pro::text::TaxonomyVectorBase::global();
        base.resolve_parent(
            &req.tag_name,
            req.language.as_deref(),
            req.context_hint.as_deref(),
        )
    })
    .await
    .unwrap_or_else(|_| {
        omni_pro::text::ResolveParentOutcome {
            success: true,
            parent_code: "builtin.zhu_ti_nei_rong.13364ec8".to_string(),
            parent_name: "主题内容".to_string(),
            confidence: 0.50,
            suggested_depth: 2,
            materialized_paths: vec![omni_pro::text::MaterializedPathItem {
                code_path: "/topic/builtin.zhu_ti_nei_rong.13364ec8".to_string(),
                name_path: "/通用/主题内容".to_string(),
                depth: 2,
            }],
        }
    });

    Json(ResolveParentResponse {
        success: outcome.success,
        parent_code: outcome.parent_code,
        parent_name: outcome.parent_name,
        confidence: outcome.confidence,
        suggested_depth: outcome.suggested_depth,
        materialized_paths: outcome.materialized_paths,
    })
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
        let batch_len = docs.len();
        let embedder = omni_pro::text::BekkoEmbedder::new();
        for doc in &mut docs {
            // 若未提供向量或维度不匹配，自动调用 BekkoEmbedder 补充 384 维向量
            if doc.embedding.len() != omni_pro::search::EMBEDDING_DIM {
                if let Ok(vec) = embedder.embed(&doc.searchable_text) {
                    doc.embedding = vec;
                }
            }
        }
        // 语义说明：返回本批提交/写入的文档数（而非索引存量），供调用方作为分批进度使用
        search.upsert_batch(&docs).map(|_| batch_len)
    }).await;
    match res {
        Ok(Ok(batch_len)) => Json(SearchIndexResponse {
            success: true,
            total_indexed: batch_len,
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


