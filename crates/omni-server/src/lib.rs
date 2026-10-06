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
    concepts::Concept,
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

pub use omni_pro::{
    OmwDb, SemanticPackLoader, VectorEngine, VectorMatch, VECTOR_DIM, VECTOR_DIM_2048,
    VECTOR_DIM_384, VECTOR_DIM_DENSE, VECTOR_DIM_WEMM,
};
use omni_pro::omw_query;
use omni_pro::{
    OmwAntonymResult, OmwAntonymsRequest, OmwDescribeRequest, OmwHierarchyRequest, OmwLookupRequest,
    OmwMappingRequest, OmwSynsetNode, OmwSynsetResult, OmwTagResult, OmwTreeRequest, TreeNode,
    UnmappedStats,
};

pub mod routes;

/// CLIP 互斥组标准名称定义
const GROUP_COLOR_MODE: &str = "色彩模式";
const GROUP_TEXT_PRESENCE: &str = "文字存在性";
const GROUP_SCREENSHOT_SUB: &str = "截图场景细分";
const GROUP_PHOTO_SUB: &str = "摄影题材细分";
const GROUP_CONTENT_FORM: &str = "内容形态";

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
    /// 阿里巴巴 zvec 嵌入式双槽位向量引擎 (RaBitQ + INT8 量化，适配 384d bekko-a8m 与 2048d WeMM-Embedding 2B，闭源优先 🔒)
    pub vector: Arc<VectorEngine>,
    /// 桌面端 SQLite 业务主库路径（只读访问 file_tag_relations 等主库表）
    pub master_db_path: Arc<Mutex<Option<PathBuf>>>,
    /// 动态维度执行策略（由本地主库 system_config.DIMENSION_POLICIES 动态驱动，缺省回退内置）
    pub dimension_policies: Arc<std::sync::RwLock<std::collections::HashMap<String, omni_core::DimensionExecutionPolicy>>>,
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
            .route("/api/taxonomy/reload-policies", post(routes::taxonomy::reload_policies_handler))
            .route("/api/v1/taxonomy/reload-policies", post(routes::taxonomy::reload_policies_handler))
            .route("/api/v1/vector/upsert", post(routes::vector::vector_upsert_handler))
            .route("/api/v1/vector/search", post(routes::vector::vector_search_handler))
            .route("/api/v1/vector/get", post(routes::vector::vector_get_handler))
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
    if let Some(p) = db_path {
        if let Ok(mut lock) = state.master_db_path.lock() {
            *lock = Some(std::path::PathBuf::from(p));
        }
        let new_policies = routes::taxonomy::load_dimension_policies_from_db(Some(std::path::Path::new(p)));
        if let Ok(mut lock) = state.dimension_policies.write() {
            *lock = new_policies;
        }
    }
    let result = match db_path {
        Some(path) => state.omw.connect(path),
        None => {
            state.omw.disconnect();
            Ok(())
        }
    };

    // 量词接线（GH #710 S9）：热重连/断开成功后刷新进程缓存——
    // 换库取新库量词；断开或源无 classifier 列则复位为空（后续造句量词 miss），杜绝陈旧量词
    if result.is_ok() {
        let omw_bg = state.omw.clone();
        tokio::task::spawn_blocking(move || refresh_classifier_map(&omw_bg))
            .await
            .unwrap_or_else(|err| tracing::warn!("classifier map refresh join failed: {err}"));
    }

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
    let t_cover = std::time::Instant::now();
    let outcome = tokio::task::spawn_blocking(move || {
        omni_pro::CoverRenderer::render_cover_with_options(&path, enable_office_cover)
    })
    .await;
    let cover_ms = t_cover.elapsed().as_millis() as u64;

    match outcome {
        Ok(Ok(bytes)) => {
            let body = Body::from(bytes);
            let mut resp = Response::new(body);
            // CoverRenderer 统一返回 WebP 格式
            resp.headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/webp"));
            if let Ok(val) = HeaderValue::from_str(&cover_ms.to_string()) {
                resp.headers_mut()
                    .insert(axum::http::HeaderName::from_static("x-cover-duration-ms"), val);
            }
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

/// 量词接线（GH #710 S9）：以当前词库刷新 omni-text 真实搭配量词进程缓存
///
/// 成功路径由 `hydrate_from_conn` 内部「局部建表 → `replace_map` 原子 swap」完成，
/// 灌库窗口内旧词库量词仍可读（无空窗）；失败/无源路径显式复位为空
/// （造句量词 miss，符合设计，不回退「张」，亦杜绝换库后的陈旧量词）。
/// 缺表/缺列与断开态属预期降级（info 级），其余异常按 warn 记录——均不阻断服务。
fn refresh_classifier_map(omw: &OmwDb) {
    if !omw.is_available() {
        // 断开态/未配置：无数据源 → 复位为空（主动断开是预期操作，记 info）
        omni_pro::text::classifier_lookup::reset_classifier_map();
        info!("classifier map cleared: omw unavailable (no classifier source)");
        return;
    }
    match omw.with_conn(|conn| omni_pro::text::classifier_lookup::hydrate_from_conn(conn)) {
        Ok(n) => info!("classifier map hydrated: {n} lemma entries"),
        Err(err) => {
            // 灌库失败即清空：防止换库/老库场景残留上一个词库的陈旧量词
            omni_pro::text::classifier_lookup::reset_classifier_map();
            let msg = format!("{err:#}");
            if msg.contains("no such column") || msg.contains("no such table") {
                info!("classifier map cleared: source has no classifier column ({msg})");
            } else {
                tracing::warn!("classifier map hydrate failed, cleared: {msg}");
            }
        }
    }
}

pub async fn start_server(
    addr: SocketAddr,
    db_path: Option<PathBuf>,
    pack_path: Option<PathBuf>,
) -> anyhow::Result<()> {
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
    // 规范解耦：--pack-path 专用于只读语义包；--db-path 专用于桌面业务主库
    let omw = OmwDb::unavailable();
    let mut pack_mounted = false;
    let mut omw_connected = false;
    let pack_to_load = pack_path.or_else(SemanticPackLoader::discover_pack_path);
    if let Some(target_pack_path) = pack_to_load {
        match omw.connect_pack(&target_pack_path) {
            Ok(()) => {
                info!("semantic.pack zero-disk mounted to OmwDb from {}", target_pack_path.display());
                pack_mounted = true;
                omw_connected = true;
            }
            Err(err) => tracing::warn!("Failed to mount semantic.pack: {err}"),
        }
    }

    if !pack_mounted {
        if let Some(path) = &db_path {
            match omw.connect(path) {
                Ok(()) => {
                    info!("omw db fallback connected read-only at {}", path.display());
                    omw_connected = true;
                }
                Err(err) => {
                    tracing::warn!("omw db open failed ({}), omw subsystem starts unavailable", err)
                }
            }
        }
    } else if let Some(path) = &db_path {
        info!("semantic.pack mounted into memory; db-path is reserved for desktop master database ({})", path.display());
    }

    // 量词接线（GH #710 S9）：词库就绪后刷新量词进程缓存
    // （spawn_blocking：41k 行全表 SQL 不阻塞 async runtime worker；await 保证首个请求前完成）
    if omw_connected {
        let omw_bg = omw.clone();
        tokio::task::spawn_blocking(move || refresh_classifier_map(&omw_bg))
            .await
            .unwrap_or_else(|err| tracing::warn!("classifier map refresh join failed: {err}"));
    }

    // 阿里巴巴 zvec 嵌入式向量引擎 (RaBitQ + INT8 量化，适配 WeMM-Embedding 2B 2048 维)
    let vector = Arc::new(VectorEngine::open_default().unwrap_or_else(|err| {
        tracing::warn!("Failed to open default vector engine ({err}), falling back to memory");
        VectorEngine::in_memory()
    }));

    let initial_policies = routes::taxonomy::load_dimension_policies_from_db(db_path.as_deref());
    let state = AppState {
        config: Arc::new(Mutex::new(initial_config)),
        geo,
        hownet,
        search,
        omw,
        vector,
        master_db_path: Arc::new(Mutex::new(db_path)),
        dimension_policies: Arc::new(std::sync::RwLock::new(initial_policies)),
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
    morphology_tags: Vec<String>,
    morphology_high_confidence_tags: Vec<String>,
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
    subtasks: std::collections::BTreeMap<String, u64>,
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
            let (raw_w, raw_h) = (img.width(), img.height());
            // 大图自适应预降采样 (限制长边 <= 1280px)，采用高速 thumbnail 将千万级像素运算量削减 95%
            let inspect_img = if raw_w > 1280 || raw_h > 1280 {
                img.thumbnail(1280, 1280)
            } else {
                img
            };

            // 多核并行：7 大视觉算子零拷贝只读引用借用 &inspect_img，独立计时
            std::thread::scope(|s| {
                // 1. 文本探活 (DBNet / MobileNet 骨干)
                let h_text = omni_core::timed_spawn!(s, {
                    omni_pro::OmniVisionEngine::fast_detect_has_text(&inspect_img)
                });

                // 2. 视觉语义标签 (Chinese-CLIP / Mobile-CLIP)
                // (WP2b) 带分提取：分数在提取层保留，贯通至 TagChainItem.confidence，杜绝 0.92 硬编码灌水
                let h_clip = omni_core::timed_spawn!(s, {
                    if enable_visual_tags {
                        omni_pro::OmniVisionEngine::extract_clip_visual_tags_scored_from_image(
                            &inspect_img,
                            lang,
                            10,
                        )
                    } else {
                        Vec::new()
                    }
                });

                // 3. NSFW 模型 5 分类概率推理
                let h_nsfw = omni_core::timed_spawn!(s, {
                    omni_pro::OmniVisionEngine::run_nsfw_model(&inspect_img)
                });

                // 4. 频域水印检测
                let h_wm = omni_core::timed_spawn!(s, {
                    omni_pro::perceive::detect_watermark_level(&inspect_img)
                });

                // 5. 宏块打码检测
                let h_mc = omni_core::timed_spawn!(s, {
                    omni_pro::perceive::detect_mosaic_level(&inspect_img)
                });

                // 6. 物理美学与画质评估 (直接消费前置 ExifTool 提取的 exif_orient！)
                let h_aes = omni_core::timed_spawn!(s, {
                    omni_pro::perceive::evaluate_image_aesthetic_and_quality(&inspect_img, exif_orient)
                });

                // 7. 黑白全彩检测
                let h_bw = omni_core::timed_spawn!(s, {
                    omni_pro::OmniVisionEngine::detect_is_black_and_white(&inspect_img)
                });

                // 8. RAM++ 细粒度实体与泛维度/泛标签投影提取
                let h_ram = omni_core::timed_spawn!(s, {
                    if enable_visual_tags {
                        omni_pro::OmniVisionEngine::extract_ram_tags(&inspect_img, lang, 10)
                    } else {
                        Vec::new()
                    }
                });

                // 9. CLIP 图像嵌入向量提取 (用于级联假设 CLIP 仲裁)，统一走 timed_spawn! 计时宏
                let h_embed = omni_core::timed_spawn!(s, {
                    omni_pro::OmniVisionEngine::extract_clip_image_embedding(&inspect_img, lang)
                });

                // 汇聚并行子任务: panic 时回退默认值并记录日志，避免零耗时误导瓶颈判定
                macro_rules! join_or_log {
                    ($handle:expr, $name:literal, $default:expr) => {
                        match $handle.join() {
                            Ok(v) => v,
                            Err(err) => {
                                tracing::warn!("视觉流水线子任务 {} panic，回退默认值: {:?}", $name, err);
                                $default
                            }
                        }
                    };
                }
                let (td, td_ms) = join_or_log!(h_text, "text_detect", (false, 0));
                let (ct_scored, ct_ms) = join_or_log!(h_clip, "clip", (Vec::new(), 0));
                let (np, np_ms) = join_or_log!(h_nsfw, "nsfw", (None, 0));
                let (wl, wl_ms) = join_or_log!(h_wm, "watermark", (0, 0));
                let (ml, ml_ms) = join_or_log!(h_mc, "mosaic", (0, 0));
                let (ar, ar_ms) = join_or_log!(h_aes, "aesthetic", ((7.5, Vec::new()), 0));
                let (bw, bw_ms) = join_or_log!(h_bw, "bw", (false, 0));
                let (ram_res, ram_ms) = join_or_log!(h_ram, "ram", (Vec::new(), 0));
                let (img_embed, embed_ms) = join_or_log!(h_embed, "clip_embed", (None, 0));

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
                let mut mutual_ms = 0u64;
                if let Some(ref emb) = out.image_embedding {
                    let t_mutual = std::time::Instant::now();
                    out.clip_mutual_tags = omni_pro::OmniVisionEngine::classify_mutual_exclusive_groups(
                        emb.as_slice(),
                        lang,
                    );
                    mutual_ms = t_mutual.elapsed().as_millis() as u64;
                }
                // 标签分支挂钟 = 并行段长尾 (CLIP/NSFW/RAM/Embed 取 Max) + 串行尾巴 (互斥分类)
                // 图像嵌入提取在 scope 内并行执行，互斥分类在 scope 外部消费嵌入向量顺序执行
                let tag_wall_ms = ct_ms.max(np_ms).max(ram_ms).max(embed_ms) + mutual_ms;
                out.tag_ms = tag_wall_ms;

                // 统一汇总至子任务度量字典
                out.subtasks.insert("text_detect_ms".to_string(), td_ms);
                out.subtasks.insert("clip_ms".to_string(), ct_ms);
                out.subtasks.insert("clip_embed_ms".to_string(), embed_ms);
                out.subtasks.insert("clip_mutual_ms".to_string(), mutual_ms);
                out.subtasks.insert("nsfw_ms".to_string(), np_ms);
                out.subtasks.insert("watermark_ms".to_string(), wl_ms);
                out.subtasks.insert("mosaic_ms".to_string(), ml_ms);
                out.subtasks.insert("aesthetic_ms".to_string(), ar_ms);
                out.subtasks.insert("bw_ms".to_string(), bw_ms);
                out.subtasks.insert("ram_ms".to_string(), ram_ms);
                out.subtasks.insert("tag_ms".to_string(), tag_wall_ms);

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
                let (mut morphology_tags, mut morphology_high_confidence_tags) =
                    omni_pro::OmniVisionEngine::derive_morphology_tags(aspect, td, bw);

                // 物理事实轻量算子注入 (DEC-05)
                // 1. 画幅算子 (ID 143)
                let aspect_concept = omni_pro::OmniVisionEngine::detect_aspect_ratio_type(raw_w, raw_h);
                let aspect_name = aspect_concept.zh_name().to_string();
                if !morphology_tags.contains(&aspect_name) {
                    morphology_tags.push(aspect_name.clone());
                    morphology_high_confidence_tags.push(aspect_name);
                }

                // 2. 背景算子 (ID 139)
                let bg_concept = omni_pro::OmniVisionEngine::detect_background_type(&inspect_img);
                let bg_name = bg_concept.zh_name().to_string();
                if !morphology_tags.contains(&bg_name) {
                    morphology_tags.push(bg_name.clone());
                    morphology_high_confidence_tags.push(bg_name);
                }

                // 3. 主色调与色系算子 (ID 149)
                let (main_color_opt, color_series_opt) = omni_pro::OmniVisionEngine::detect_main_color_and_series(&inspect_img);
                if let Some(mc) = main_color_opt {
                    let mc_name = mc.zh_name().to_string();
                    if !morphology_tags.contains(&mc_name) {
                        morphology_tags.push(mc_name.clone());
                        morphology_high_confidence_tags.push(mc_name);
                    }
                }
                if let Some(series_concept) = color_series_opt {
                    let cs_name = series_concept.zh_name().to_string();
                    if !morphology_tags.contains(&cs_name) {
                        morphology_tags.push(cs_name.clone());
                        morphology_high_confidence_tags.push(cs_name);
                    }
                }

                // 4. 画质等级算子 (ID 122)
                if let Some(rq) = omni_pro::OmniVisionEngine::detect_resolution_quality(raw_w, raw_h) {
                    let rq_name = rq.zh_name().to_string();
                    if !morphology_tags.contains(&rq_name) {
                        morphology_tags.push(rq_name.clone());
                        morphology_high_confidence_tags.push(rq_name);
                    }
                }

                // 无字图排版门禁：若未探活出文本内容，严禁打上依赖排版文字的海报宣发或截图标签
                // Spec D9：闭环规则按概念 code 匹配，兼容中英别名
                if !td {
                    out.clip_tags.retain(|t| {
                        !tag_matches_concept(t, "海报宣发") && !tag_matches_concept(t, "截图")
                    });
                }

                // CLIP 高置信度标签截取 Top 5，同互斥单选组内仅保留最高排序项 (杜绝 Windows/macOS/Linux 截图同存)
                let mut high_conf: Vec<String> = Vec::with_capacity(5);
                let mut seen_groups: std::collections::HashSet<&'static str> = std::collections::HashSet::new();
                for t in &out.clip_tags {
                    if high_conf.len() >= 5 {
                        break;
                    }
                    let code = normalize_tag_to_code(t);
                    // G1 词形闸哨兵：空 code 表示该词未通过准入，不得进入高置信度截取
                    if omni_core::tag_admissibility::is_rejected_code(&code) {
                        continue;
                    }
                    // 经跨注册表桥接：`Concept` 注册表与别名表 code 分歧（GH #719）
                    // 会使直查静默落空，导致同概念在 zh/en 下分组不一致。
                    let group = match omni_core::tag_identity::concept_from_code(code.as_str()) {
                        Some(Concept::Windows截图)
                        | Some(Concept::macOS截图)
                        | Some(Concept::iOS截图)
                        | Some(Concept::Android截图)
                        | Some(Concept::Linux截图) => Some("system_ecology"),

                        Some(Concept::实拍)
                        | Some(Concept::手绘)
                        | Some(Concept::CG渲染)
                        | Some(Concept::AI生成) => Some("generation_carrier"),

                        Some(Concept::横屏)
                        | Some(Concept::竖屏)
                        | Some(Concept::超宽长条)
                        | Some(Concept::正方形) => Some("aspect_ratio"),

                        _ => None,
                    };

                    if let Some(g) = group {
                        if seen_groups.insert(g) {
                            high_conf.push(t.clone());
                        }
                    } else {
                        high_conf.push(t.clone());
                    }
                }
                out.clip_high_confidence_tags = high_conf;

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
                        if !morphology_tags.contains(&"条漫".to_string()) {
                            morphology_tags.push("条漫".to_string());
                        }
                    } else if (aspect >= 0.55 && aspect <= 0.90) || (aspect >= 1.20 && aspect <= 1.60) {
                        if !morphology_tags.contains(&"页漫".to_string()) {
                            morphology_tags.push("页漫".to_string());
                        }
                    }
                }

                out.morphology_tags = morphology_tags;
                out.morphology_high_confidence_tags = morphology_high_confidence_tags;

                // 图像细分形态分类
                let p_type = omni_pro::perceive::infer_image_modal_type(
                    &inspect_img,
                    &out.morphology_tags,
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

use omni_core::arbitrate_tags_by_dimension_policies;

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
    benchmark.record("magika_ms", magika_ms);

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
            // Issue 0046 §1：纯图片的 OCR 结果独占 `ocr` 字段，`markdown_content` 严格保持空字符串，
            // 避免把 OCR 文本伪装成「正文」污染排版结构（复合文档方允许原位嵌合 OCR）。
            let markdown_content = String::new();
            let mut ocr_ms = None;
            let mut text_ms = None;
            let mut ocr_text: Option<String> = None;
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
                    ocr_text = Some(ocr_res);
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

            // 如果提取到了文字识别内容，写入标准的 text_stats（纯图片走 OCR 字段）
            if let Some(text_for_stats) = ocr_text.as_deref().filter(|s| !s.is_empty()) {
                let lines = text_for_stats.lines().count();
                let words = text_for_stats.split_whitespace().count();
                let chars = text_for_stats.chars().count();
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

        benchmark.record_opt("metadata_ms", if metadata_ms > 0 { Some(metadata_ms) } else { None });
        benchmark.vision_ms = Some(vision_res.duration_ms);
        benchmark.extend_subtasks(vision_res.subtasks.clone());
        benchmark.record_opt("ocr_ms", ocr_ms);
        benchmark.record_opt("text_ms", text_ms);
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

            // 仅在音视频文件且 enable_asr 未显式关闭时执行 SenseVoice 转录
            let should_transcribe = is_audio_or_video
                && req.enable_asr.unwrap_or(true);

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
                    // Issue 0046 §1：ASR 结果独占 `asr` 字段（不再回填 markdown_content，
                    // 保证音视频文件的「正文」结构与语音转录事实物理分离）。
                    if let serde_json::Value::Object(ref mut map) = ext_res.metadata {
                        map.insert("asr".to_string(), serde_json::Value::String(transcript));
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
            benchmark.record_opt("metadata_ms", bm.metadata_ms);
            benchmark.record_opt("text_ms", bm.text_ms);
            benchmark.record_opt("ocr_ms", bm.ocr_ms);
            if let Some(t_ms) = bm.text_ms {
                benchmark.record("doc_parse_ms", t_ms);
            }
        }

        (ads_res, (ext_res.metadata, ext_res.markdown_content, ext_res.phash, ext_res.is_corrupted, v, None))
    };

    benchmark.ads_ms = ads_ms;

    // ASR 语音转录事实（Issue 0046 §1：metadata 键统一为 `asr`）
    let asr = metadata
        .get("asr")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty());
    let has_asr = asr.is_some();
    let asr_length = asr.as_deref().map(|s| s.chars().count()).unwrap_or(0);

    // 统一「可参与事实推导的文本正文」：
    // 复合文档正文 > 纯图片 OCR > 音视频 ASR。
    // 后两者不再写入 markdown_content（保持正文结构纯粹），但必须继续驱动
    // 文字存在性判定、Tier1 文本分析与混合索引，否则会丢失 OCR/ASR 语义。
    let effective_text: String =
        omni_core::resolve_effective_text(&markdown_content, ocr_text.as_deref(), asr.as_deref());

    // 内存零耗时推导: NSFW 敏感内容与高置信度标签 (结合 OCR/ASR 提取文本和 CLIP 标签，两层漏斗过滤)
    // 按媒体类型分流：图片/视频走视觉 NSFW 管线（nsfw_tags），文本/文档走文本 NSFW 管线（nsfw_text_tags）
    let (nsfw_tags, nsfw_text_tags, sensitive_types, content_rating) = if is_pro {
        let (tags, sens, rating) = omni_pro::OmniVisionEngine::derive_nsfw_tags_and_rating_full(
            vision_res.nsfw_probs,
            &effective_text,
            &vision_res.clip_tags,
            req.language.as_deref(),
            Some(&file_path),
        );
        if is_image || is_video {
            // 视觉路径：NSFW 结论写 nsfw_tags，nsfw_text_tags 为空
            (tags, vec![], sens, rating)
        } else {
            // 文本/文档路径：NSFW 结论写 nsfw_text_tags，nsfw_tags 为空（避免与视觉引擎字段混淆）
            (vec![], tags, sens, rating)
        }
    } else {
        (vec!["全年龄".to_string()], vec![], Vec::new(), Some("safe".to_string()))
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
    let mut morphology_tags = vision_res.morphology_tags;
    let mut clip_tags = vision_res.clip_tags;
    // (WP2b) CLIP 标定分快照（名 → 分），供 engine map 与 6.2 confidence 贯通
    let clip_tag_confs = vision_res.clip_tag_confs;

    // 基于实际 OCR 文本正向直通校准截图形态与文字客观事实 (彻底根除有字却输出无字图的倒挂)
    // (WP2a) ocr_forced_tag_names: OCR 事实强制注入的标签名，用于后续 engine 来源标记 (最高物理事实优先级)
    // 仅图片路径生效：纯文本/文档正文不是「图面有字」，不得注入有字图等视觉形态标签
    let mut ocr_forced_tag_names: Vec<String> = Vec::new();
    let visual_text_fact = is_image && !effective_text.trim().is_empty();
    if visual_text_fact {
        has_text = Some(true);
        clip_tags.retain(|t| !tag_matches_concept(t, "无字图"));
        if !clip_tags.iter().any(|t| tag_matches_concept(t, "有字图")) {
            clip_tags.push("有字图".to_string());
            ocr_forced_tag_names.push("有字图".to_string());
        }

        let text_lower = effective_text.to_lowercase();
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
    let mut morphology_high_confidence_tags = vision_res.morphology_high_confidence_tags;
    let mut clip_high_confidence_tags = vision_res.clip_high_confidence_tags;

    let ram_tags = vision_res.ram_tags;
    let image_embedding = vision_res.image_embedding;
    let clip_mutual_tags = vision_res.clip_mutual_tags;

    // CLIP 互斥分类结果增强 morphology_tags：
    // 若 CLIP 互斥分类成功，直接替换规则推导结果（语义更准确）；
    // 若 CLIP 不可用（无模型），则保留规则推导的 morphology_tags 作为兜底。
    let mut morphology_tags = if !clip_mutual_tags.is_empty() {
        let mut merged = morphology_tags.clone();
        for (tag, _conf, _group) in &clip_mutual_tags {
            if !merged.contains(tag) {
                merged.push(tag.clone());
            }
        }
        // CLIP 互斥分类保证组内只有一个胜出者，移除被覆盖的规则推导冲突项
        // 1. 色彩模式：CLIP 结果与黑白检测结果互斥对齐（概念 code 匹配，兼容中英）
        if clip_mutual_tags
            .iter()
            .any(|(t, _, g)| *g == GROUP_COLOR_MODE && tag_matches_concept(t, Concept::全彩.zh_name()))
        {
            merged.retain(|t| !tag_matches_concept(t, Concept::黑白.zh_name()));
        } else if clip_mutual_tags
            .iter()
            .any(|(t, _, g)| *g == GROUP_COLOR_MODE && tag_matches_concept(t, Concept::黑白.zh_name()))
        {
            merged.retain(|t| !tag_matches_concept(t, Concept::全彩.zh_name()));
        }
        // 2. 文字存在性：客观检测与 OCR 事实优先于语义猜测（仅图片路径，避免文本正文误标有字图）
        if is_image && (has_text == Some(true) || !effective_text.trim().is_empty()) {
            merged.retain(|t| !tag_matches_concept(t, Concept::无字图.zh_name()));
            if !merged.iter().any(|t| tag_matches_concept(t, Concept::有字图.zh_name())) {
                merged.push(Concept::有字图.zh_name().to_string());
            }
        } else if clip_mutual_tags
            .iter()
            .any(|(t, _, g)| *g == GROUP_TEXT_PRESENCE && tag_matches_concept(t, Concept::有字图.zh_name()))
        {
            merged.retain(|t| !tag_matches_concept(t, Concept::无字图.zh_name()));
        } else if clip_mutual_tags
            .iter()
            .any(|(t, _, g)| *g == GROUP_TEXT_PRESENCE && tag_matches_concept(t, Concept::无字图.zh_name()))
        {
            merged.retain(|t| !tag_matches_concept(t, Concept::有字图.zh_name()));
        }
        merged
    } else {
        if is_image && (has_text == Some(true) || !effective_text.trim().is_empty()) {
            morphology_tags.retain(|t| !tag_matches_concept(t, Concept::无字图.zh_name()));
            if !morphology_tags.iter().any(|t| tag_matches_concept(t, Concept::有字图.zh_name())) {
                morphology_tags.push(Concept::有字图.zh_name().to_string());
            }
        }
        morphology_tags
    };

    // 5. 文字密度物理算子注入 (ID 146, 依 OCR 文本字数区间确定，非模型余弦推断)
    if is_image {
        let ocr_char_count = ocr_text.as_deref().map(|s| s.chars().count()).unwrap_or(0);
        if let Some(density_concept) = omni_pro::OmniVisionEngine::detect_text_density(ocr_char_count, false) {
            let density_name = density_concept.zh_name().to_string();
            if !morphology_tags.contains(&density_name) {
                morphology_tags.push(density_name.clone());
                morphology_high_confidence_tags.push(density_name);
            }
        }
    }

    // CLIP 互斥组与 photo_type 细化联动更新（语义优先于规则推导）
    if !clip_mutual_tags.is_empty() {
        // 优先取截图细分（比泛截图更精确）
        if let Some((sub_tag, _, _)) = clip_mutual_tags.iter().find(|(_, _, g)| *g == GROUP_SCREENSHOT_SUB) {
            if photo_type.is_none()
                || photo_type.as_deref() == Some(Concept::截图.zh_name())
                || photo_type.as_deref() == Some(Concept::截图.code())
            {
                photo_type = Some(sub_tag.clone());
            }
        } else if let Some((photo_sub, _, _)) = clip_mutual_tags.iter().find(|(_, _, g)| *g == GROUP_PHOTO_SUB) {
            // 若为摄影照片，细化为风景照/人物照/静物照等
            if photo_type.is_none()
                || photo_type.as_deref() == Some(Concept::摄影照片.zh_name())
                || photo_type.as_deref() == Some(Concept::摄影照片.code())
            {
                photo_type = Some(photo_sub.clone());
            }
        } else if let Some((form_tag, _, _)) = clip_mutual_tags.iter().find(|(_, _, g)| *g == GROUP_CONTENT_FORM) {
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
        .chain(morphology_tags.iter())
        .chain(nsfw_tags.iter())
        .chain(quality_issues.iter())
        // (#718 汇聚侧同源化) ram 贡献改推 r.code（proj.code = 受控反查 zh 规范名，权威）：
        // 此前推 r.name（展示名）→ normalize 后未受控条目 code = 展示名 hash，与过滤侧
        // r.code（zh 规范名 hash）派生输入不同必然分叉；同源化后两侧消费同一 code，
        // 非 zh 会话未受控条目不再依赖 OR 兜底存活。r.code 为空的条目回落展示名（旧行为）。
        .chain(ram_tags.iter().map(|r| if r.code.is_empty() { &r.name } else { &r.code }))
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
        for t in &morphology_tags {
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
            // (#718) 汇聚侧同源化后 detected_visual_tags 内 ram 条目为 r.code 形态，
            // engine_lookup（域压制②按集合条目精确取来源）必须同时覆盖 code 键，
            // 否则「截图域压制 ram 来源」静默失效。
            if !r.code.is_empty() && r.code != r.name {
                register(&r.code, "ram", 3, Some(r.confidence));
            }
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

    // 文字存在性绝对保护：若已探活出文字或 OCR 内容，无条件排除无字图，确保有字图存在（仅图片路径）
    if is_image && (has_text == Some(true) || !effective_text.trim().is_empty()) {
        detected_visual_tags.retain(|t| !tag_matches_concept(t, "无字图"));
        if !detected_visual_tags.iter().any(|t| tag_matches_concept(t, "有字图")) {
            detected_visual_tags.push("有字图".to_string());
        }
    }

    // Spec D12：统一归一为稳定 code 后再做集合同步，保证 zh/en 输入幂等
    // 修复(问题2)：normalize 前先保留「原始名 → code」映射，供 6.2 步骤反查真实展示名
    let mut detected_visual_tag_name_to_code: std::collections::HashMap<String, String> = detected_visual_tags
        .iter()
        .map(|name| (name.clone(), normalize_tag_to_code(name)))
        .collect();
    // (#718) 同源化后 ram 条目在集合内是 code 形态，6.2 反查展示名需补「展示名 → code」
    // 条目；同 code 多键时反查侧优先非 code 形态键（见下方 original_name 查找）。
    for r in &ram_tags {
        if !r.code.is_empty() && r.code != r.name {
            detected_visual_tag_name_to_code.insert(r.name.clone(), r.code.clone());
        }
    }
    let detected_visual_tags = normalize_tag_set_to_codes(&detected_visual_tags);

    // 修复(问题4)：clip/morphology 只用 code 集合做内部比对，保留原始名供输出
    // 同步清洗 morphology_tags 与 clip_tags，确保互斥清洗结果一致贯通（防止被清洗的子标签混入下游主体池）
    // 输出保留原始中文/英文名（debug 字段，与 ram_tags 保持一致）
    let morphology_tags: Vec<String> = morphology_tags
        .into_iter()
        .filter(|name| {
            let code = normalize_tag_to_code(name);
            detected_visual_tags.contains(&code)
        })
        .collect();
    let mut clip_tags: Vec<String> = clip_tags
        .into_iter()
        .filter(|name| {
            let code = normalize_tag_to_code(name);
            detected_visual_tags.contains(&code)
        })
        .collect();
    // RAM 对称治理 (P3 来源对称)：与 clip/morphology 一致，按 gated 后 code 集回写过滤，
    // 杜绝原始 ram_tags 经「6.1 直注 / ram_tags_flat / 级联主体池」三条通道绕过互斥门禁
    let gated_ram_tags: Vec<omni_core::RamTagItem> = ram_tags
        .iter()
        .filter(|r| {
            // spec §6.9.6 待裁定项 2 —— #718 同源化后 OR 兜底退役为单判据：
            // 汇聚侧 ram 贡献已改推 r.code（权威 proj.code），过滤判据与派生源合一：
            // r.code 非空 → contains(r.code)（自身贡献在集合 = 未被门控剔除）；
            // r.code 为空（投影缺格）→ 回落名字归一（旧行为保底）。
            // 旧 OR 的名字归一分支曾依赖「展示名 hash」偶然命中，已随同源化失去存在意义。
            if r.code.is_empty() {
                detected_visual_tags.contains(&normalize_tag_to_code(&r.name))
            } else {
                detected_visual_tags.contains(&r.code)
            }
        })
        .cloned()
        .collect();
    let sem_graph = if is_pro { state.omw.semantic_graph() } else { None };
    let current_file_group = if is_image { Some("image") } else { None };
    let enrich_tag = |tag: &mut omni_core::TagChainItem| {
        if let Some(ref g) = sem_graph {
            g.enrich_tag_chain_item_with_context(tag, current_file_group);
        }
    };
    let mut gated_ram_tags = gated_ram_tags;
    for r in &mut gated_ram_tags {
        enrich_tag(r);
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
        let ws = ws_code.as_deref().map(|c| {
            // 机器码别名 → AOT 中文 Concept 变体（源码中文直写，展示名由构建期字典给出）
            match c {
                "unarchived" | "not_archived" => Concept::未归档.zh_name().to_string(),
                "archived" => Concept::已归档.zh_name().to_string(),
                "draft" => Concept::草稿.zh_name().to_string(),
                "reviewing" => Concept::审核中.zh_name().to_string(),
                "completed" => Concept::已完成.zh_name().to_string(),
                other => omni_core::get_canonical_concept_name(other).unwrap_or(other).to_string(),
            }
        });
        let sec_code = Some(omni_pro::perceive::detect_security_level(&file_path, &effective_text));
        let sec = sec_code.as_deref().map(|c| {
            match c {
                "public" => Concept::公开.zh_name().to_string(),
                "internal" => Concept::内部.zh_name().to_string(),
                "confidential" => Concept::机密.zh_name().to_string(),
                "secret" => Concept::绝密.zh_name().to_string(),
                other => omni_core::get_canonical_concept_name(other).unwrap_or(other).to_string(),
            }
        });
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
            // 从 normalize 前的映射反查原始名（找不到则用 code 作为 fallback）。
            // (#718) 同 code 多键（展示名 + code 自映射）时优先展示名键，
            // 防止 ram 条目的 TagChainItem.name 被写成 code 串。
            // (#718 R1/M1) 确定性仲裁：HashMap 迭代序不定，同 code 存在多个非 code 词面键
            // （如 zh 规范名与 en 别名并存）时选择结果随运行波动 → 规则化：
            // ① 非 ASCII（CJK）字符多者优先（zh 规范展示名优先于 translit 别名）；
            // ② 字符数更少者优先（短规范名优先于长复合词）；
            // ③ 字典序兜底，保证完全可复现。
            let is_code_form = |k: &str| {
                k.starts_with("builtin.") || k.starts_with("omw.") || k.starts_with("hownet.")
                    || k.starts_with("_ext.") || k.starts_with("dim.")
            };
            let original_name = detected_visual_tag_name_to_code
                .iter()
                .filter(|(_name, code): &(&String, &String)| code.as_str() == raw_code.as_str())
                .map(|(name, _code): (&String, &String)| name.clone())
                .filter(|name: &String| !is_code_form(name))
                .min_by_key(|name: &String| {
                    (
                        std::cmp::Reverse(name.chars().filter(|c| !c.is_ascii()).count()),
                        name.chars().count(),
                        name.clone(),
                    )
                })
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
                    omni_pro::OmniVisionEngine::resolve_tag_to_chain_item_with_lang(
                        &original_name,
                        confidence,
                        req.language.as_deref(),
                    );
                // G1 词形闸哨兵：空 code 表示该词未通过准入——**必须丢弃**，
                // 否则坏概念会写进概念表、别名表与 materialized_paths 标签树（spec §6.9.1）。
                // 正常路径下 normalize_tag_set_to_codes 已滤除坏词，此处是防御性二道保险；
                // 注意必须判在 code 覆盖**之前**，否则 raw_code 会把哨兵盖掉。
                if omni_core::tag_admissibility::is_rejected_code(&resolved.code) {
                    continue;
                }
                // 确保 code 与归一结果一致（仅当 raw_code 是合法受控码时才覆盖；若 raw_code 发生 _ext 漂移而 resolved 命中受控码则严格保留受控码）
                if omni_core::tag_identity::is_controlled_code(raw_code) || !omni_core::tag_identity::is_controlled_code(&resolved.code) {
                    resolved.code = raw_code.clone();
                }
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
            enrich_tag(&mut item);
            structured_visual_tags.push(item);
        }
    }

    // 7. 级联提示词合成 + CLIP 向量仲裁终局裁决 (Pro 专享，仅图片路径生效)
    // 注意：此处在 ram_tags 降级为 flat 前调用，以获取完整 TagChainItem 结构
    // (P3 来源对称：级联主体池同样只消费门禁后存活的 gated_ram_tags)
    let (cascade_candidates, winning_hypothesis, activated_dimension_tags, _smart_name, _content_description, pruned_ambiguous_words) = if is_pro && is_image {
        omni_pro::OmniVisionEngine::synthesize_cascade_hypotheses_and_arbitrate(
            &file_path,
            &detected_visual_tags,
            &gated_ram_tags,
            &morphology_tags,
            &nsfw_tags,
            image_embedding.as_deref(),
            req.language.as_deref(),
        )
    } else {
        (Vec::new(), None, Vec::new(), None, None, Vec::new())
    };

    // 6.3 ram_tags 平铺字符串数组：忠实透出 RAM++ 模型端原始识别结果 (全年龄/安全内容下过滤违禁敏感词)
    let is_all_ages_content = is_pro && sensitive_types.is_empty() && (
        content_rating.as_deref() == Some("safe")
            || content_rating.as_deref() == Some("all_ages")
            || content_rating.as_deref().map_or(false, |r| tag_matches_concept(r, Concept::全年龄.code()))
            || nsfw_tags.iter().any(|t| tag_matches_concept(t, Concept::全年龄.code()))
            || (!nsfw_tags.iter().any(|t| tag_matches_concept(t, Concept::色情.code()) || tag_matches_concept(t, Concept::血腥.code()) || t == "R-18" || t == "R-18G")
                && !nsfw_text_tags.iter().any(|t| tag_matches_concept(t, Concept::色情.code()) || tag_matches_concept(t, Concept::血腥.code()) || t == "R-18" || t == "R-18G"))
    );
    let ram_tags_flat: Vec<String> = if is_all_ages_content {
        ram_tags
            .iter()
            .filter(|r| !omni_pro::is_nsfw_or_restricted_tag(&r.name))
            .map(|r| r.name.clone())
            .collect()
    } else {
        ram_tags.iter().map(|r| r.name.clone()).collect()
    };

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
    let text_analysis = if is_pro && enable_frontend_text && !effective_text.trim().is_empty() {
        let t_text = std::time::Instant::now();
        let text = effective_text.clone();
        let fname = file_name.clone();
        let hownet = state.hownet.clone();
        let res = tokio::task::spawn_blocking(move || {
            omni_pro::text::OmniTextEngine::analyze(&text, &fname, mtime, Some(&*hownet))
        })
        .await;
        let text_duration = t_text.elapsed().as_millis() as u64;
        let cur_text_ms = benchmark.get("text_ms").unwrap_or(0);
        benchmark.record("text_ms", cur_text_ms + text_duration);
        if let Ok(ref result) = res {
            benchmark.record("bekko_embed_ms", result.bekko_embed_ms);
            benchmark.record("keybert_ms", result.keybert_ms);
            benchmark.record("slot_summary_ms", result.slot_summary_ms);
        }
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

    let (text_title, mut text_keywords, text_entities, text_summary, text_one_desc, text_slots, text_emb, text_smart_name) = match text_analysis {
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
    let current_policies = state.dimension_policies.read().unwrap_or_else(|e| e.into_inner()).clone();
    let multimodal_ctx = omni_core::MultimodalContext {
        file_path: file_path.clone(),
        file_name: file_name.clone(),
        mime_type: mime_type.clone(),
        document_text: if !is_image && !markdown_content.trim().is_empty() { Some(markdown_content.clone()) } else { None },
        ocr_text: ocr_text.clone(),
        // 对外契约字段为 `asr`（Issue 0046 §1），MultimodalContext 内部沿用历史名 audio_transcript
        audio_transcript: asr.clone(),
        lrc_text: lrc.clone(),
        visual_tags: structured_visual_tags.clone(),
        exif_metadata: metadata.clone(),
        is_image,
        is_document: !is_image && !is_video,
        is_audio_or_video: is_video || has_asr || lrc.is_some(),
        language: req.language.clone(),
        dimension_policies: Some(current_policies.clone()),
    };

    let t_fusion = std::time::Instant::now();
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
    let fusion_ms = t_fusion.elapsed().as_millis() as u64;
    benchmark.record("fusion_ms", fusion_ms);

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

    // 终局智能重命名与描述：优先取第三阶段双锚点交叉验证胜出者 (Fusion 轨单轨出厂，PRD §7.1.4)
    // 视觉内联旁路已退役，图片路径绝不回退至 text 5W 范畴错配
    let smart_name = fusion_outcome.smart_name.or(if is_image { None } else { text_smart_name });
    let content_description = fusion_outcome.content_description.or_else(|| if is_image { None } else { text_one_desc.clone() });
    
    // 置信度门限控制：低于 EXIT_CONFIDENCE_THRESHOLD 的候选标签严禁透出到接口返回结果中 (日志中已输出完整候选打分)
    structured_visual_tags.retain(|t| t.confidence >= EXIT_CONFIDENCE_THRESHOLD);

    let fused_tags = if !fusion_outcome.fused_tags.is_empty() {
        let mut filtered = fusion_outcome.fused_tags;
        filtered.retain(|t| t.confidence >= EXIT_CONFIDENCE_THRESHOLD);
        filtered
    } else {
        structured_visual_tags.clone()
    };

    // 缺口2（GH #712 S7）：融合标签组补挂词性回填。
    // 融合引擎（OmniMultimodalFusionEngine::fuse_and_arbitrate）产出的 fused_tags
    // 中的 TagChainItem 不一定经过了 enrich_tag，导致 pos 字段为空。
    // 物理事实标签组（fact_tags）按豁免规则可不挂（ADR-0052 §6 决策⑥）。
    let mut fused_tags = fused_tags;
    for item in &mut fused_tags {
        enrich_tag(item);
    }

    // 票 05：受控标签与扩展标签经由父 (via_parent_code) 与树状父 (parent_codes) 主链回填
    // 终结「工单 01 已知限制」，确保受控标签行落库时 via_parent_code != ''
    let backfill_chain_item = |mut tag: omni_core::TagChainItem| -> omni_core::TagChainItem {
        // 1. 若已有 parent_codes 且 via_parent_code 为空，优先以首个父级作为经由父消歧锚
        if tag.via_parent_code.is_none() && !tag.parent_codes.is_empty() {
            tag.via_parent_code = tag.parent_codes.first().cloned();
        }
        // 2. 若依然缺失经由父且不是真根节点（受控标签 builtin.* / omw.* / hownet.* 或扩展标签 _ext.*），在 Pro 下通过向量底座推导补全
        if tag.via_parent_code.is_none() && is_pro {
            if tag.code.starts_with("_ext.") || tag.code.starts_with("builtin.") || tag.code.starts_with("omw.") || tag.code.starts_with("hownet.") {
                let outcome = omni_pro::text::OmniMultimodalFusionEngine::resolve_ext_tag_parent(
                    &tag.name,
                    &tag.code,
                );
                if !outcome.is_empty() {
                    tag.via_parent_code = Some(outcome.clone());
                    if tag.parent_codes.is_empty() {
                        tag.parent_codes = vec![outcome];
                    }
                }
            }
        }
        tag
    };

    let structured_visual_tags: Vec<omni_core::TagChainItem> = structured_visual_tags
        .into_iter()
        .map(backfill_chain_item)
        .collect();

    let fused_tags: Vec<omni_core::TagChainItem> = fused_tags
        .into_iter()
        .map(backfill_chain_item)
        .collect();

    // 9.1 基于维度策略执行单选互斥 argmax 与多选维度放行最终一致性仲裁
    let mut structured_visual_tags = arbitrate_tags_by_dimension_policies(structured_visual_tags, &current_policies);
    let mut fused_tags = arbitrate_tags_by_dimension_policies(fused_tags, &current_policies);

    // 10. 原生事实标签抽取 (Task 2)：元数据直读 + 下沉物理事实 → fact_tags 直出
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
    let mut fact_tags: Vec<omni_core::TagChainItem> = {
        let ctx = omni_extract::FactTagContext {
            metadata: &metadata,
            file_source: file_source.clone(),
            workflow_state: workflow_state.clone(),
            security_level: security_level.clone(),
            quality_score,
            language_label: language_label.clone(),
        };
        omni_extract::OmniFactTagExtractor::extract(&ctx)
    };

    // 核心安全合规契约：无论什么类型的文件（图片/视频/音频/文本/文档），
    // 若通过 nsfw_tags 或 nsfw_text_tags 专职判定为非 NSFW 的内容 (is_all_ages_content)，全面过滤所有通道的敏感标签！
    // 若为真正违规/NSFW 的内容，则不过滤，忠实透出全部检测结果供审计与标记。
    if is_all_ages_content {
        clip_tags.retain(|t| !omni_pro::is_nsfw_or_restricted_tag(t));
        clip_high_confidence_tags.retain(|t| !omni_pro::is_nsfw_or_restricted_tag(t));
        structured_visual_tags.retain(|t| !omni_pro::is_nsfw_or_restricted_tag(&t.name));
        fused_tags.retain(|t| !omni_pro::is_nsfw_or_restricted_tag(&t.name));
        fact_tags.retain(|t| !omni_pro::is_nsfw_or_restricted_tag(&t.name));
        text_keywords.retain(|t| !omni_pro::is_nsfw_or_restricted_tag(t));
    }

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

    tracing::info!(
        "[事实标签抽取:fact_tags] 文件: {}, 产出标签数: {}, 标签: {:?}",
        file_name,
        fact_tags.len(),
        fact_tags.iter().map(|t| t.name.as_str()).collect::<Vec<_>>()
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
        morphology_tags,
        clip_tags,
        nsfw_tags,
        nsfw_text_tags,
        ram_tags: ram_tags_flat,
        morphology_high_confidence_tags,
        clip_high_confidence_tags,
        nsfw_high_confidence_tags,
        sensitive_types,
        content_rating,
        asr,
        has_asr,
        asr_length,
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
        fact_tags,
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
    if let (Some(ref emb), false) = (&text_emb, effective_text.trim().is_empty()) {
        let indexed_doc = omni_pro::search::IndexedDocument {
            fingerprint: file_path.clone(),
            embedding: emb.clone(),
            searchable_text: format!("{}\n{}", file_name, effective_text),
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
        bekko_embed_ms: 0,
        keybert_ms: 0,
        slot_summary_ms: 0,
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
    /// 语义标签消歧经由父级 code (ADR-0047，与 parent_code 双向对齐)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via_parent_code: Option<String>,
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
        via_parent_code: Some(outcome.parent_code.clone()),
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
    let vector = state.vector.clone();
    let res = tokio::task::spawn_blocking(move || {
        // 进程内向量补齐 (ADR-0054)：
        // 1. 若 doc.embedding.len() == 384，直接使用；
        // 2. 若 doc.embedding 为空或维度非 384，从端侧 zvec 384d 槽位读取反量化向量回填；
        // 3. 过滤未能获得 384d 有效向量的文档，若有效文档数 < 2 则拦截报错。
        let requested_count = req.documents.len();
        let mut docs: Vec<omni_pro::search::ClusterDocument> =
            Vec::with_capacity(requested_count);
        for mut doc in req.documents {
            if doc.embedding.len() == omni_pro::search::EMBEDDING_DIM {
                docs.push(doc);
            } else if let Some(vec) =
                vector.get_vector(&doc.fingerprint, omni_pro::search::EMBEDDING_DIM)
            {
                doc.embedding = vec;
                docs.push(doc);
            }
        }

        if requested_count > 0 && docs.len() < 2 {
            anyhow::bail!(
                "具备 384d 有效向量的文档不足 2 份 (请求 {} 份，有效 {} 份)，无法执行 HAC 聚类",
                requested_count,
                docs.len()
            );
        }

        // 跨支柱补缝：若未显式提供 prompt_embedding，但提供了自然语言 prompt，自动通过 BekkoEmbedder 计算聚类引导向量
        let prompt_emb = match (req.prompt_embedding, req.prompt.as_deref()) {
            (Some(emb), _) => Some(emb),
            (None, Some(p)) if !p.trim().is_empty() => {
                omni_pro::text::BekkoEmbedder::new().embed(p).ok()
            }
            _ => None,
        };

        search.cluster(
            &docs,
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
        // Step 2: 降级：从 OmniExtractor 元数据中取 asr（若已有提取结果）
        if let Ok(res) = OmniExtractor::extract(&file_path, &cfg).await {
            if let Some(t) = res.metadata.get("asr").and_then(|v| v.as_str()) {
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
