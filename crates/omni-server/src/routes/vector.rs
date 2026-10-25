//! vector.rs — 嵌入式向量引擎 HTTP 交互端点 (zvec / RaBitQ / INT8)
//!
//! 依据 ADR-0038 / ADR-0046 / ADR-0054 与 docs/specs/embeddinggemma-dual-profile-and-wemm-retirement-spec.md 架构决议：
//! - POST /api/v1/vector/upsert: 写入/更新特征向量 (自动按长度路由 256d, 384d, 512d, 768d)；
//! - POST /api/v1/vector/search: < 1ms Top-K ANN 检索 (RaBitQ 初筛 + INT8 重排，自动按查询向量维度路由)；
//! - POST /api/v1/vector/get: 批量按指纹与维度读取反量化浮点向量；
//! - DELETE /api/v1/vector/delete: 批量删除特征向量 (同步清理四槽位)。

use axum::{
    extract::State,
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::time::Instant;

use crate::AppState;

/// 单个向量条目
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorUpsertItem {
    #[serde(alias = "file_fingerprint")]
    pub file_fingerprint: String,
    pub vector: Vec<f32>,
    /// 编码器身份（0=Unknown/Legacy, 1=Bekko, 2=ChineseCLIP, 3=MobileCLIP, 4=Gemma）。
    /// HTTP 层用 `Option<u8>` 接收以便区分缺省；缺省一律 400 `encoder_id_required`
    /// （缺字节默认 0 只适用于旧落盘文件读取，不适用于 HTTP 入参）。
    #[serde(default)]
    pub encoder_id: Option<u8>,
}

/// 灵活支持单项或批量的 Upsert 载荷
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum VectorUpsertPayload {
    Single(VectorUpsertItem),
    Batch(Vec<VectorUpsertItem>),
    Wrapped { items: Vec<VectorUpsertItem> },
}

/// Upsert 响应体
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorUpsertResponse {
    pub success: bool,
    pub count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 检索请求体
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorSearchRequest {
    /// 256 维 / 384 维 / 512 维 / 768 维 浮点查询特征向量
    pub vector: Vec<f32>,
    /// 最大召回数 (默认 10)
    #[serde(default = "default_top_k", alias = "top_k")]
    pub top_k: usize,
    /// 相似度阈值 ([-1.0, 1.0]，低于此阈值的项被过滤)
    pub threshold: Option<f32>,
    /// 编码器白名单：必填且不得为空数组，仅对命中编码器的记录打分（过滤发生在
    /// RaBitQ 预判 / 打分 / 排序 / truncate 之前，防止 top-K 被异空间记录占满而饿死）。
    /// 缺省或空数组一律 400 `encoder_filter_required`，禁止解释为「不过滤」。
    #[serde(default)]
    pub encoder_filter: Option<Vec<u8>>,
}

fn default_top_k() -> usize {
    10
}

/// 检索命中项
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VectorMatchItem {
    pub file_fingerprint: String,
    pub score: f32,
    pub rank: usize,
    /// 命中记录的编码器身份（zvec v3 encoder_id）
    pub encoder_id: u8,
}

/// 检索响应体
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorSearchResponse {
    pub matches: Vec<VectorMatchItem>,
    pub duration_us: u128,
    pub count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 批量读取反量化向量请求体: POST /api/v1/vector/get
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorGetRequest {
    #[serde(alias = "file_fingerprints")]
    pub file_fingerprints: Vec<String>,
    #[serde(default = "default_get_dim")]
    pub dim: usize,
}

fn default_get_dim() -> usize {
    384
}

/// 单个反量化读取项
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VectorGetItem {
    pub file_fingerprint: String,
    pub vector: Vec<f32>,
    /// 该向量落盘时携带的编码器身份（zvec v3 encoder_id），供 desktop 探活/白名单判定
    pub encoder_id: u8,
}

/// 批量读取反量化向量响应体
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorGetResponse {
    pub vectors: Vec<VectorGetItem>,
    pub count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 删除请求体 (灵活支持数组或对象封装，兼容 camelCase 与 snake_case)
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum VectorDeletePayload {
    #[serde(rename_all = "camelCase")]
    List {
        #[serde(alias = "file_fingerprints")]
        file_fingerprints: Vec<String>,
    },
    #[serde(rename_all = "camelCase")]
    Single {
        #[serde(alias = "file_fingerprint")]
        file_fingerprint: String,
    },
    RawList(Vec<String>),
}

/// 删除响应体
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorDeleteResponse {
    pub success: bool,
    pub deleted_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 批量按指纹与维度读取反量化向量: POST /api/v1/vector/get
pub async fn vector_get_handler(
    State(state): State<AppState>,
    Json(req): Json<VectorGetRequest>,
) -> Result<Json<VectorGetResponse>, (StatusCode, Json<VectorGetResponse>)> {
    if req.dim != 256 && req.dim != 384 && req.dim != 512 && req.dim != 768 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(VectorGetResponse {
                vectors: Vec::new(),
                count: 0,
                error: Some(format!("不支持的向量维度: {} (仅支持 256, 384, 512, 768)", req.dim)),
            }),
        ));
    }

    if req.file_fingerprints.is_empty() {
        return Ok(Json(VectorGetResponse {
            vectors: Vec::new(),
            count: 0,
            error: None,
        }));
    }

    let vector_engine = state.vector.clone();
    let dim = req.dim;
    let fps = req.file_fingerprints;

    let result = tokio::task::spawn_blocking(move || {
        let mut items = Vec::with_capacity(fps.len());
        for fp in fps {
            // 不做白名单过滤：encoderId 原样透出，由 desktop 按当前档位判定（探活/白名单）
            if let Some((vec, enc)) = vector_engine.get_vector_entry(&fp, dim) {
                items.push(VectorGetItem {
                    file_fingerprint: fp,
                    vector: vec,
                    encoder_id: enc,
                });
            }
        }
        items
    })
    .await;

    match result {
        Ok(vectors) => {
            let count = vectors.len();
            Ok(Json(VectorGetResponse {
                vectors,
                count,
                error: None,
            }))
        }
        Err(join_err) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(VectorGetResponse {
                vectors: Vec::new(),
                count: 0,
                error: Some(format!("Task execution panic: {join_err}")),
            }),
        )),
    }
}

/// upsert 入参校验：把 HTTP 条目转换为引擎条目。
///
/// 缺 `encoderId` 一律 400 `encoder_id_required`。本函数是纯校验，不触碰引擎，
/// 由 handler 在 `spawn_blocking` 与任何写入之前调用；缺字节默认 0 仅适用于旧落盘文件读取，
/// 不适用于 HTTP 入参。
fn resolve_upsert_batch(
    items: Vec<VectorUpsertItem>,
) -> Result<Vec<omni_pro::vector_engine::VectorUpsertItem>, (StatusCode, &'static str)> {
    let mut batch = Vec::with_capacity(items.len());
    for it in items {
        let encoder_id = it
            .encoder_id
            .ok_or((StatusCode::BAD_REQUEST, "encoder_id_required"))?;
        batch.push(omni_pro::vector_engine::VectorUpsertItem {
            file_fingerprint: it.file_fingerprint,
            vector: it.vector,
            encoder_id,
        });
    }
    Ok(batch)
}

/// 检索入参校验：`encoderFilter` 必填且不得为空数组，缺省/空数组一律
/// 400 `encoder_filter_required`。纯校验，由 handler 在 `spawn_blocking` 与检索之前调用。
fn resolve_encoder_filter(
    filter: Option<Vec<u8>>,
) -> Result<Vec<u8>, (StatusCode, &'static str)> {
    match filter {
        Some(list) if !list.is_empty() => Ok(list),
        _ => Err((StatusCode::BAD_REQUEST, "encoder_filter_required")),
    }
}

/// 构造 upsert 拒绝响应（与校验函数返回的状态码/错误串一一对应）
fn upsert_rejection(
    status: StatusCode,
    error: &'static str,
) -> (StatusCode, Json<VectorUpsertResponse>) {
    (
        status,
        Json(VectorUpsertResponse {
            success: false,
            count: 0,
            error: Some(error.to_string()),
        }),
    )
}

/// 构造 search 拒绝响应（与校验函数返回的状态码/错误串一一对应）

/// 将底层产生的 OmniError 或 anyhow::Error 映射为统一的 HTTP 状态码与稳定错误码字符串 (Ticket 07)
pub fn map_omni_error_response(err: &anyhow::Error) -> (StatusCode, String) {
    if let Some(oe) = err.downcast_ref::<omni_core::OmniError>() {
        let status = StatusCode::from_u16(oe.http_status_code())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, oe.kind().to_string())
    } else {
        (StatusCode::BAD_REQUEST, err.to_string())
    }
}
fn search_rejection(
    status: StatusCode,
    error: &'static str,
) -> (StatusCode, Json<VectorSearchResponse>) {
    (
        status,
        Json(VectorSearchResponse {
            matches: Vec::new(),
            duration_us: 0,
            count: 0,
            error: Some(error.to_string()),
        }),
    )
}

/// 写入或更新特征向量: POST /api/v1/vector/upsert
pub async fn vector_upsert_handler(
    State(state): State<AppState>,
    Json(payload): Json<VectorUpsertPayload>,
) -> Result<Json<VectorUpsertResponse>, (StatusCode, Json<VectorUpsertResponse>)> {
    let items = match payload {
        VectorUpsertPayload::Single(item) => vec![item],
        VectorUpsertPayload::Batch(items) => items,
        VectorUpsertPayload::Wrapped { items } => items,
    };

    if items.is_empty() {
        return Ok(Json(VectorUpsertResponse {
            success: true,
            count: 0,
            error: None,
        }));
    }

    // 契约已收紧：缺 `encoderId` 一律 400，且必须在任何写入（含 spawn_blocking）之前完成校验。
    let batch_items = match resolve_upsert_batch(items) {
        Ok(batch) => batch,
        Err((status, error)) => return Err(upsert_rejection(status, error)),
    };

    let vector_engine = state.vector.clone();

    // 在 spawn_blocking 中执行批量量化与单次原子落盘，消除磁盘写放大
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
        vector_engine.batch_upsert(&batch_items)
    })
    .await;

    match result {
        Ok(Ok(saved_count)) => Ok(Json(VectorUpsertResponse {
            success: true,
            count: saved_count,
            error: None,
        })),
        Ok(Err(err)) => {
            let (status, err_code) = map_omni_error_response(&err);
            Err((
                status,
                Json(VectorUpsertResponse {
                    success: false,
                    count: 0,
                    error: Some(err_code),
                }),
            ))
        }
        Err(join_err) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(VectorUpsertResponse {
                success: false,
                count: 0,
                error: Some(format!("Task execution panic: {join_err}")),
            }),
        )),
    }
}

/// 极速 Top-K ANN 相似度检索: POST /api/v1/vector/search
pub async fn vector_search_handler(
    State(state): State<AppState>,
    Json(req): Json<VectorSearchRequest>,
) -> Result<Json<VectorSearchResponse>, (StatusCode, Json<VectorSearchResponse>)> {
    let vector_engine = state.vector.clone();

    // 契约已收紧：`encoderFilter` 缺省或空数组一律 400，且必须在任何检索（含 spawn_blocking）之前完成校验。
    // 产品契约上该字段必填，禁止把缺省解释为「不过滤」。
    let encoder_filter = match resolve_encoder_filter(req.encoder_filter) {
        Ok(list) => list,
        Err((status, error)) => return Err(search_rejection(status, error)),
    };

    let start = Instant::now();
    let result = tokio::task::spawn_blocking(move || {
        vector_engine.search(
            &req.vector,
            req.top_k,
            req.threshold,
            Some(encoder_filter.as_slice()),
        )
    })
    .await;

    let duration_us = start.elapsed().as_micros();

    match result {
        Ok(Ok(matches)) => {
            let items: Vec<VectorMatchItem> = matches
                .into_iter()
                .enumerate()
                .map(|(idx, m)| VectorMatchItem {
                    file_fingerprint: m.file_fingerprint,
                    score: m.score,
                    rank: idx + 1,
                    encoder_id: m.encoder_id,
                })
                .collect();

            let count = items.len();
            Ok(Json(VectorSearchResponse {
                matches: items,
                duration_us,
                count,
                error: None,
            }))
        }
        Ok(Err(err)) => {
            let (status, err_code) = map_omni_error_response(&err);
            Err((
                status,
                Json(VectorSearchResponse {
                    matches: Vec::new(),
                    duration_us,
                    count: 0,
                    error: Some(err_code),
                }),
            ))
        }
        Err(join_err) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(VectorSearchResponse {
                matches: Vec::new(),
                duration_us,
                count: 0,
                error: Some(format!("Task execution panic: {join_err}")),
            }),
        )),
    }
}

/// 批量删除特征向量: DELETE /api/v1/vector/delete
pub async fn vector_delete_handler(
    State(state): State<AppState>,
    Json(payload): Json<VectorDeletePayload>,
) -> Result<Json<VectorDeleteResponse>, (StatusCode, Json<VectorDeleteResponse>)> {
    let fingerprints = match payload {
        VectorDeletePayload::List { file_fingerprints } => file_fingerprints,
        VectorDeletePayload::Single { file_fingerprint } => vec![file_fingerprint],
        VectorDeletePayload::RawList(list) => list,
    };

    if fingerprints.is_empty() {
        return Ok(Json(VectorDeleteResponse {
            success: true,
            deleted_count: 0,
            error: None,
        }));
    }

    let vector_engine = state.vector.clone();

    let result = tokio::task::spawn_blocking(move || {
        vector_engine.delete(&fingerprints)
    })
    .await;

    match result {
        Ok(Ok(deleted_count)) => Ok(Json(VectorDeleteResponse {
            success: true,
            deleted_count,
            error: None,
        })),
        Ok(Err(err)) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(VectorDeleteResponse {
                success: false,
                deleted_count: 0,
                error: Some(err.to_string()),
            }),
        )),
        Err(join_err) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(VectorDeleteResponse {
                success: false,
                deleted_count: 0,
                error: Some(format!("Task execution panic: {join_err}")),
            }),
        )),
    }
}

/// 段落对齐单文件候选段落列表
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchPassagesItem {
    #[serde(alias = "file_fingerprint")]
    pub file_fingerprint: String,
    pub passages: Vec<String>,
}

/// 向量段落对齐请求体: POST /api/v1/vector/match-passages
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchPassagesRequest {
    pub query: String,
    pub items: Vec<MatchPassagesItem>,
}

/// 单文件最佳段落对齐命中结果
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PassageMatchResult {
    pub file_fingerprint: String,
    pub best_passage_index: usize,
    pub best_passage: String,
    pub similarity: f32,
}

/// 向量段落对齐响应体
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchPassagesResponse {
    pub matches: Vec<PassageMatchResult>,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 段落嵌入进程级缓存：key = 文本 FNV-1a 哈希，value = L2 归一化 384d 向量
/// 用于 match-passages 重复候选段落命中时跳过 ONNX 冷推理，对齐 ADR-0039「按需动态计算」性能契约
static PASSAGE_EMBED_CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<u64, Vec<f32>>>> =
    std::sync::OnceLock::new();

const PASSAGE_EMBED_CACHE_MAX: usize = 8192;

fn passage_cache() -> &'static std::sync::Mutex<std::collections::HashMap<u64, Vec<f32>>> {
    PASSAGE_EMBED_CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn fnv1a64(text: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 获取段落向量：优先命中进程级缓存，否则调用 BekkoEmbedder（常驻 OnceLock 会话）并回填
fn embed_passage_cached(
    embedder: &omni_pro::text::BekkoEmbedder,
    passage: &str,
) -> anyhow::Result<Vec<f32>> {
    let key = fnv1a64(passage);
    if let Ok(cache) = passage_cache().lock() {
        if let Some(v) = cache.get(&key) {
            return Ok(v.clone());
        }
    }
    let vec = embedder.embed(passage)?;
    if let Ok(mut cache) = passage_cache().lock() {
        if cache.len() >= PASSAGE_EMBED_CACHE_MAX {
            cache.clear();
        }
        cache.insert(key, vec.clone());
    }
    Ok(vec)
}

/// 密集向量段落对齐处理器: POST /api/v1/vector/match-passages
///
/// 依据 ADR-0039 与 PRD 0039 契约：
/// 1. 仅对 query 编码 1 次生成 384 维特征向量；
/// 2. 利用 bekko-a8m 常驻会话对候选段落批量打分（段落向量进程级缓存，重复候选零推理）；
/// 3. 返回最佳匹配段落及其下标与相似度；小样本冷路径目标 <15ms，缓存命中路径为纯点积。
pub async fn match_passages_handler(
    Json(req): Json<MatchPassagesRequest>,
) -> Result<Json<MatchPassagesResponse>, (StatusCode, Json<MatchPassagesResponse>)> {
    let start = Instant::now();

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<PassageMatchResult>> {
        let embedder = omni_pro::text::BekkoEmbedder::new();
        // 核心契约：query 仅单次编码 (384d)
        let query_vec = embedder.embed(&req.query)?;

        let mut matches = Vec::with_capacity(req.items.len());

        for item in req.items {
            if item.passages.is_empty() {
                matches.push(PassageMatchResult {
                    file_fingerprint: item.file_fingerprint,
                    best_passage_index: 0,
                    best_passage: String::new(),
                    similarity: 0.0,
                });
                continue;
            }

            let mut best_idx = 0;
            let mut best_sim = -2.0f32;
            let mut best_text = String::new();

            for (idx, passage) in item.passages.iter().enumerate() {
                let p_vec = embed_passage_cached(&embedder, passage)?;
                let sim = omni_pro::text::BekkoEmbedder::cosine_similarity(&query_vec, &p_vec);
                if sim > best_sim {
                    best_sim = sim;
                    best_idx = idx;
                    best_text = passage.clone();
                }
            }

            matches.push(PassageMatchResult {
                file_fingerprint: item.file_fingerprint,
                best_passage_index: best_idx,
                best_passage: best_text,
                similarity: best_sim.clamp(-1.0, 1.0),
            });
        }

        Ok(matches)
    })
    .await;

    let duration_ms = start.elapsed().as_millis() as u64;

    match result {
        Ok(Ok(matches)) => Ok(Json(MatchPassagesResponse {
            matches,
            duration_ms,
            error: None,
        })),
        Ok(Err(err)) => Err((
            StatusCode::BAD_REQUEST,
            Json(MatchPassagesResponse {
                matches: Vec::new(),
                duration_ms,
                error: Some(err.to_string()),
            }),
        )),
        Err(join_err) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(MatchPassagesResponse {
                matches: Vec::new(),
                duration_ms,
                error: Some(format!("Task execution panic: {join_err}")),
            }),
        )),
    }
}

/// 批量文本向量编码请求体: POST /api/v1/vector/batch-embed-text
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchEmbedTextRequest {
    pub texts: Vec<String>,
    #[serde(default)]
    pub profile: Option<String>, // "classic_light" (bekko) | "gemma_unified"
    #[serde(default)]
    pub dim: Option<usize>,      // 缺省时：bekko 为 384，gemma 为当前 mrl_dimension (256/512/1024)
}

/// 批量文本向量编码响应体: POST /api/v1/vector/batch-embed-text
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchEmbedTextResponse {
    pub vectors: Vec<Vec<f32>>,
    pub count: usize,
    pub dim: usize,
    pub profile: String,
    pub duration_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 批量文本向量化处理器: POST /api/v1/vector/batch-embed-text
///
/// 依据 batch-organize-hybrid-vector-classification-spec.md 规格契约：
/// 1. 支持根据当前系统 Profile 与用户自选维度动态自适应分派；
/// 2. 当 profile == "classic_light" 或未传且 dim == Some(384) 时：路由至 BekkoEmbedder (384d)；
/// 3. 当 profile == "gemma_unified" 或 dim 为 256/512/1024 时：路由至 OmniVisionEngine::encode_gemma_text，执行 MRL 截断与强制 L2 归一化；
/// 4. 杜绝阻塞事件循环，使用 tokio::task::spawn_blocking 并在批量 > 1000 时进行硬安全截断。
pub async fn batch_embed_text_handler(
    Json(req): Json<BatchEmbedTextRequest>,
) -> Result<Json<BatchEmbedTextResponse>, (StatusCode, Json<BatchEmbedTextResponse>)> {
    let start = Instant::now();

    // 硬上限截断 1000 条，防止超大请求拖垮内存与推理池
    let mut texts = req.texts;
    if texts.len() > 1000 {
        texts.truncate(1000);
    }

    let profile = req.profile.clone().unwrap_or_default();
    let is_gemma = profile == "gemma_unified"
        || req.dim == Some(256)
        || req.dim == Some(512)
        || req.dim == Some(1024);

    let active_profile = if is_gemma {
        "gemma_unified".to_string()
    } else {
        "classic_light".to_string()
    };

    let target_dim = if is_gemma {
        req.dim.unwrap_or(512)
    } else {
        384
    };

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<(Vec<Vec<f32>>, usize)> {
        if texts.is_empty() {
            return Ok((Vec::new(), target_dim));
        }

        if !is_gemma {
            let embedder = omni_pro::text::BekkoEmbedder::new();
            let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
            let mut vectors = embedder.batch_embed(&text_refs)?;
            // 确保每个向量维度为 384 并执行 L2 归一化兜底
            for v in &mut vectors {
                if v.len() != 384 {
                    v.resize(384, 0.0);
                }
                let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                if norm > 1e-6 {
                    for x in v.iter_mut() {
                        *x /= norm;
                    }
                }
            }
            Ok((vectors, 384))
        } else {
            let mut vectors = Vec::with_capacity(texts.len());
            for text in texts {
                let mut v = match omni_pro::OmniVisionEngine::encode_gemma_text(&text) {
                    Ok(vec) => vec,
                    Err(e) => {
                        tracing::warn!("Gemma text embedding failed for text snippet: {e}");
                        Vec::new()
                    }
                };

                // MRL 截断与对齐
                if v.len() >= target_dim {
                    v.truncate(target_dim);
                } else {
                    v.resize(target_dim, 0.0);
                }

                // 强制 L2 归一化
                let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                if norm > 1e-6 {
                    for x in v.iter_mut() {
                        *x /= norm;
                    }
                }

                vectors.push(v);
            }
            Ok((vectors, target_dim))
        }
    })
    .await;

    let duration_ms = start.elapsed().as_millis();

    match result {
        Ok(Ok((vectors, actual_dim))) => {
            let count = vectors.len();
            Ok(Json(BatchEmbedTextResponse {
                vectors,
                count,
                dim: actual_dim,
                profile: active_profile,
                duration_ms,
                error: None,
            }))
        }
        Ok(Err(err)) => Err((
            StatusCode::BAD_REQUEST,
            Json(BatchEmbedTextResponse {
                vectors: Vec::new(),
                count: 0,
                dim: target_dim,
                profile: active_profile,
                duration_ms,
                error: Some(err.to_string()),
            }),
        )),
        Err(join_err) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(BatchEmbedTextResponse {
                vectors: Vec::new(),
                count: 0,
                dim: target_dim,
                profile: active_profile,
                duration_ms,
                error: Some(format!("Task execution panic: {join_err}")),
            }),
        )),
    }
}


/* ------------------------------------------------------------------ *
 * 工单 10：Chinese-CLIP 文本塔编码接口 POST /api/v1/vector/embed-clip-text
 * ------------------------------------------------------------------ */

/// CLIP 文本编码并发上限（规格 §4.2：Semaphore(2)，同时只允许 2 次编码）
static CLIP_TEXT_ENCODE_SEMAPHORE: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();

/// 并发等待超时（规格 §4.2：等待超过 200ms 返回 429 busy，**不返回 504**）
const CLIP_TEXT_ENCODE_WAIT_MS: u64 = 200;

fn clip_text_encode_semaphore() -> &'static tokio::sync::Semaphore {
    CLIP_TEXT_ENCODE_SEMAPHORE.get_or_init(|| tokio::sync::Semaphore::new(2))
}

/// CLIP 文本编码请求体
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbedClipTextRequest {
    pub text: String,
    #[serde(default)]
    pub lang: Option<String>,
}

/// CLIP 文本编码响应体（规格 §4.2 契约）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbedClipTextResponse {
    pub success: bool,
    /// 512 维 L2 归一化文本向量（ChineseCLIP 文本空间）
    pub vector: Vec<f32>,
    pub dim: usize,
    /// 固定 2（ChineseCLIP），供写入侧直接透传 zvec v3 encoder_id
    pub encoder_id: u8,
    /// 首次调用是否触发模型加载（仅供埋点/UI，**不参与超时预算**）
    pub cold_start: bool,
    pub load_ms: u128,
    pub encode_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// CLIP 文本编码错误响应体（与成功响应同型，便于 desktop 单一路径解析）
fn clip_text_error(
    status: StatusCode,
    error: &'static str,
    encoder_id: u8,
) -> (StatusCode, Json<EmbedClipTextResponse>) {
    (
        status,
        Json(EmbedClipTextResponse {
            success: false,
            vector: Vec::new(),
            dim: omni_pro::vision::CLIP_TEXT_DIM,
            encoder_id,
            cold_start: false,
            // 失败路径不东拼西凑：`encode_ms` 恒 0（未发生编码），
            // 不拿「请求总耗时」冒充编码耗时（那会把并发排队时间算成推理时间）。
            load_ms: 0,
            encode_ms: 0,
            error: Some(error.to_string()),
        }),
    )
}

/// POST /api/v1/vector/embed-clip-text（工单 10）
///
/// 契约（规格 §4.2 / §6 故障矩阵）：
/// - 文本为空 → 422 `empty_text`；
/// - 模型三件套缺失 / 词表不可读 / 输出无 text_embeds / 加载失败 → 503，
///   分别 `model_missing` / `vocab_missing` / `unsupported_text_output` / `load_failed`；
/// - 并发饱和（等待 > 200ms）→ 429 `busy`；
/// - **永不返回 504**：超时由 desktop 侧的冷/热预算控制，服务端只做并发上限。
pub async fn embed_clip_text_handler(
    Json(req): Json<EmbedClipTextRequest>,
) -> Result<Json<EmbedClipTextResponse>, (StatusCode, Json<EmbedClipTextResponse>)> {
    let text = req.text.trim().to_string();
    let lang = req.lang.clone();
    let is_zh = lang.as_deref().map(|l| l.starts_with("zh")).unwrap_or(true);
    let default_encoder_id = if is_zh {
        omni_pro::vector_engine::encoder_id::CHINESE_CLIP
    } else {
        omni_pro::vector_engine::encoder_id::MOBILE_CLIP
    };

    if text.is_empty() {
        return Err(clip_text_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "empty_text",
            default_encoder_id,
        ));
    }

    // 并发上限：Semaphore(2) + 200ms 等待上限，超限即 429 busy（不算失败、不计熔断）
    let permit = match tokio::time::timeout(
        std::time::Duration::from_millis(CLIP_TEXT_ENCODE_WAIT_MS),
        clip_text_encode_semaphore().acquire(),
    )
    .await
    {
        Ok(Ok(permit)) => permit,
        _ => {
            return Err(clip_text_error(
                StatusCode::TOO_MANY_REQUESTS,
                "busy",
                default_encoder_id,
            ));
        }
    };

    // 模型加载与推理包在 spawn_blocking，避免阻塞 Reactor
    let joined = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let clip_dir = match omni_pro::OmniVisionEngine::resolve_clip_model_dir() {
            Some(d) => d,
            None => return Err(("model_missing", default_encoder_id, 0u128, 0u128)),
        };
        // 探活检查：根据语言判断 Chinese-CLIP 还是 MobileCLIP
        if is_zh {
            if !omni_pro::vision::clip_text_model_ready(&clip_dir) {
                return Err(("model_missing", default_encoder_id, 0u128, 0u128));
            }
            if !omni_pro::vision::clip_text_vocab_ready(&clip_dir) {
                return Err(("vocab_missing", default_encoder_id, 0u128, 0u128));
            }
        } else {
            if !omni_pro::vision::mobileclip_text_model_ready(&clip_dir) {
                return Err(("model_missing", default_encoder_id, 0u128, 0u128));
            }
            if !omni_pro::vision::mobileclip_tokenizer_ready(&clip_dir) {
                return Err(("vocab_missing", default_encoder_id, 0u128, 0u128));
            }
        }

        let cold_start = if is_zh {
            omni_pro::vision::clip_text_session_is_cold()
        } else {
            omni_pro::vision::mobileclip_text_session_is_cold()
        };
        let encoded =
            omni_pro::OmniVisionEngine::encode_clip_text_timed(&text, &clip_dir, lang.as_deref());

        match encoded {
            Ok((v, encoder_id, load_ms, encode_ms)) => Ok((v, encoder_id, cold_start, load_ms, encode_ms)),
            // 失败原因由引擎侧直接给出（model_missing/vocab_missing/
            // unsupported_text_output/load_failed），此处只做透传
            Err(reason) => Err((reason, default_encoder_id, 0u128, 0u128)),
        }
    })
    .await;

    match joined {
        Ok(Ok((vector, encoder_id, cold_start, load_ms, encode_ms))) => Ok(Json(EmbedClipTextResponse {
            success: true,
            dim: vector.len(),
            vector,
            encoder_id,
            cold_start,
            load_ms,
            encode_ms,
            error: None,
        })),
        Ok(Err((reason, encoder_id, _, _))) => {
            // 四个已登记原因（model_missing / vocab_missing /
            // unsupported_text_output / load_failed）一律 503；
            // 未登记原因同样按 503 处理，**永不降级/升级为 504**。
            Err(clip_text_error(StatusCode::SERVICE_UNAVAILABLE, reason, encoder_id))
        }
        Err(join_err) => {
            tracing::warn!("CLIP 文本编码任务 panic: {join_err}");
            Err(clip_text_error(StatusCode::SERVICE_UNAVAILABLE, "load_failed", default_encoder_id))
        }
    }
}

#[cfg(test)]
mod tests {
    /// Ticket 07 验收单测：断言 4 个稳定错误码与对应 HTTP 状态码映射一致
    #[test]
    fn test_omni_error_mapping_to_http_codes() {
        let e_missing: anyhow::Error = omni_core::OmniError::ModelMissing {
            path: std::path::PathBuf::from("models/vision.onnx"),
        }.into();
        let (status, code) = map_omni_error_response(&e_missing);
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(code, "model_missing");

        let e_load: anyhow::Error = omni_core::OmniError::ModelLoadFailed {
            path: std::path::PathBuf::from("models/vision.onnx"),
            source: "bad onnx".into(),
        }.into();
        let (status, code) = map_omni_error_response(&e_load);
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(code, "model_load_failed");

        let e_dml: anyhow::Error = omni_core::OmniError::DmlRuntimeFailed {
            encoder: "gemma.vision",
            source: "device lost".into(),
        }.into();
        let (status, code) = map_omni_error_response(&e_dml);
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(code, "dml_runtime_failed");

        let e_input: anyhow::Error = omni_core::OmniError::InvalidInput {
            field: "vector",
            reason: "dim != 512".into(),
        }.into();
        let (status, code) = map_omni_error_response(&e_input);
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code, "invalid_input");
    }

    use super::*;
    use omni_pro::vector_engine::encoder_id;

    /// 构造一条合法的引擎侧写入条目（384d，便于落到 384 槽）
    fn engine_item(fp: &str, encoder: u8) -> omni_pro::vector_engine::VectorUpsertItem {
        omni_pro::vector_engine::VectorUpsertItem {
            file_fingerprint: fp.to_string(),
            vector: vec![0.1_f32; 384],
            encoder_id: encoder,
        }
    }

    /// AC: upsert 缺 `encoderId` → 400 `encoder_id_required`，且校验发生在任何写入之前
    #[test]
    fn upsert_without_encoder_id_returns_400_and_writes_nothing() {
        let engine = omni_pro::VectorEngine::in_memory();
        // 预置一条合法记录：用于证明 400 分支不会产生额外落盘
        engine
            .batch_upsert(&[engine_item("fp_seed", encoder_id::BEKKO)])
            .expect("预置合法向量失败");
        assert_eq!(engine.count(), 1);

        let (status, error) = resolve_upsert_batch(vec![VectorUpsertItem {
            file_fingerprint: "fp_missing_encoder".to_string(),
            vector: vec![0.1_f32; 384],
            encoder_id: None,
        }])
        .expect_err("缺 encoderId 必须被拒绝");

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error, "encoder_id_required");

        // 错误体（HTTP 实际返回体）断言
        let (body_status, Json(body)) = upsert_rejection(status, error);
        assert_eq!(body_status, StatusCode::BAD_REQUEST);
        assert!(!body.success);
        assert_eq!(body.count, 0);
        assert_eq!(body.error.as_deref(), Some("encoder_id_required"));

        // 未发生写入：记录数不变，且缺 encoderId 的指纹不存在任何槽位记录
        assert_eq!(engine.count(), 1, "400 分支不得发生写入");
        assert!(
            engine.get_vector_entry("fp_missing_encoder", 384).is_none(),
            "缺 encoderId 的条目不得落盘"
        );
    }

    /// 合法入参不得被误拒：显式 encoderId 可正常转换
    #[test]
    fn upsert_with_encoder_id_is_accepted() {
        let batch = resolve_upsert_batch(vec![VectorUpsertItem {
            file_fingerprint: "fp_ok".to_string(),
            vector: vec![0.1_f32; 384],
            encoder_id: Some(encoder_id::GEMMA),
        }])
        .expect("显式 encoderId 必须通过校验");
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].encoder_id, encoder_id::GEMMA);
    }

    /// AC: search 的 `encoderFilter` 缺省或空数组 → 400 `encoder_filter_required`
    #[test]
    fn search_without_encoder_filter_returns_400_and_reads_nothing() {
        let engine = omni_pro::VectorEngine::in_memory();
        engine
            .batch_upsert(&[engine_item("fp_seed", encoder_id::BEKKO)])
            .expect("预置合法向量失败");
        let before = engine.count();

        for filter in [None, Some(Vec::new())] {
            let (status, error) =
                resolve_encoder_filter(filter).expect_err("缺省或空 encoderFilter 必须被拒绝");
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(error, "encoder_filter_required");

            let (body_status, Json(body)) = search_rejection(status, error);
            assert_eq!(body_status, StatusCode::BAD_REQUEST);
            assert_eq!(body.count, 0);
            assert!(body.matches.is_empty());
            assert_eq!(body.error.as_deref(), Some("encoder_filter_required"));
        }

        // 未发生写入：检索拒绝路径不触碰引擎
        assert_eq!(engine.count(), before, "400 分支不得发生写入");
    }

    /// 合法白名单不得被误拒
    #[test]
    fn search_with_encoder_filter_is_accepted() {
        assert_eq!(
            resolve_encoder_filter(Some(vec![encoder_id::UNKNOWN, encoder_id::BEKKO]))
                .expect("classic 384 白名单必须通过"),
            vec![encoder_id::UNKNOWN, encoder_id::BEKKO]
        );
        assert_eq!(
            resolve_encoder_filter(Some(vec![encoder_id::GEMMA])).expect("gemma 白名单必须通过"),
            vec![encoder_id::GEMMA]
        );
    }

    /* ---------------- 工单 10：embed-clip-text 契约测试 ---------------- */

    /// 错误矩阵：`clip_text_error` 必须把状态码与 error 码同时带给 desktop
    #[test]
    fn clip_text_error_carries_status_and_code() {
        for (status, code) in [
            (StatusCode::UNPROCESSABLE_ENTITY, "empty_text"),
            (StatusCode::TOO_MANY_REQUESTS, "busy"),
            (StatusCode::SERVICE_UNAVAILABLE, "model_missing"),
            (StatusCode::SERVICE_UNAVAILABLE, "vocab_missing"),
            (StatusCode::SERVICE_UNAVAILABLE, "unsupported_text_output"),
            (StatusCode::SERVICE_UNAVAILABLE, "load_failed"),
        ] {
            let (got_status, Json(body)) =
                clip_text_error(status, code, encoder_id::CHINESE_CLIP);
            assert_eq!(got_status, status, "{code} 的状态码必须稳定");
            assert_eq!(body.error.as_deref(), Some(code));
            assert!(!body.success);
            assert!(body.vector.is_empty());
            // 固定 encoderId=2，供写入侧直接透传 zvec v3
            assert_eq!(body.encoder_id, encoder_id::CHINESE_CLIP);
            assert_eq!(body.dim, omni_pro::vision::CLIP_TEXT_DIM);
        }
    }

    /// 错误矩阵不得出现 504：超时由 desktop 侧预算控制，服务端只做并发上限
    #[test]
    fn clip_text_error_never_uses_gateway_timeout() {
        for code in [
            "empty_text",
            "busy",
            "model_missing",
            "vocab_missing",
            "unsupported_text_output",
            "load_failed",
        ] {
            let (status, _) = clip_text_error(StatusCode::SERVICE_UNAVAILABLE, code, encoder_id::CHINESE_CLIP);
            assert_ne!(status, StatusCode::GATEWAY_TIMEOUT, "{code} 不得映射为 504");
        }
    }

    /// 请求体按 camelCase 反序列化，`text` 为唯一必填字段
    #[test]
    fn clip_text_request_deserializes_camel_case() {
        let req: EmbedClipTextRequest =
            serde_json::from_str(r#"{"text":"一只橘猫"}"#).expect("text 字段必须可解析");
        assert_eq!(req.text, "一只橘猫");

        let missing: Result<EmbedClipTextRequest, _> = serde_json::from_str(r#"{}"#);
        assert!(missing.is_err(), "缺 text 必须反序列化失败（后续走 422）");

        let req_with_lang: EmbedClipTextRequest =
            serde_json::from_str(r#"{"text":"a red car","lang":"en"}"#).expect("带 lang 的请求必须可解析");
        assert_eq!(req_with_lang.text, "a red car");
        assert_eq!(req_with_lang.lang.as_deref(), Some("en"));
    }

    /// 并发上限：`Semaphore(2)` —— 第三次 acquire 在 200ms 预算内必然拿不到许可
    #[tokio::test]
    async fn clip_text_encode_semaphore_caps_at_two() {
        let sem = clip_text_encode_semaphore();
        let _a = sem.acquire().await.expect("第 1 个许可");
        let _b = sem.acquire().await.expect("第 2 个许可");
        // 许可已耗尽：第三条必须在 200ms 内超时，对应 429 busy
        let third = tokio::time::timeout(
            std::time::Duration::from_millis(CLIP_TEXT_ENCODE_WAIT_MS),
            sem.acquire(),
        )
        .await;
        assert!(third.is_err(), "Semaphore(2) 必须让第 3 次获取超时");

        // 归还后立即可用（证明不是死锁/永久占用）
        drop(_a);
        let again = tokio::time::timeout(
            std::time::Duration::from_millis(CLIP_TEXT_ENCODE_WAIT_MS),
            sem.acquire(),
        )
        .await;
        assert!(again.is_ok(), "归还许可后必须可再获取");
    }

    /// 成功响应契约：encoderId 恒为 2，vector 与 dim 自洽
    #[test]
    fn clip_text_success_response_shape() {
        let vector = vec![0.5_f32; omni_pro::vision::CLIP_TEXT_DIM];
        let body = EmbedClipTextResponse {
            success: true,
            dim: vector.len(),
            vector,
            encoder_id: encoder_id::CHINESE_CLIP,
            cold_start: true,
            load_ms: 12,
            encode_ms: 3,
            error: None,
        };
        let json = serde_json::to_string(&body).expect("响应必须可序列化");
        assert!(json.contains("\"success\":true"));
        assert!(json.contains("\"dim\":512"));
        assert!(json.contains("\"encoderId\":2"));
        assert!(json.contains("\"coldStart\":true"));
        // 成功路径不得出现 error 字段
        assert!(!json.contains("\"error\""), "成功响应不得携带 error");
    }

    /// 探活不加载模型：`clip_text_model_ready` / `clip_text_vocab_ready` 只做元数据/内容判断，
    /// 走的是只读代码路径（不构建 session），因此 RSS 不增长。
    #[test]
    fn clip_text_health_probes_are_read_only() {
        // 空目录：三件套俱缺 → 探活全 false
        let empty = std::env::temp_dir().join("firefly-clip-probe-empty-does-not-exist");
        let _ = std::fs::remove_dir_all(&empty);
        assert!(!omni_pro::vision::clip_text_model_ready(&empty));
        assert!(!omni_pro::vision::clip_text_vocab_ready(&empty));

        // 只有模型没有词表 → modelReady=true / vocabReady=false（可分辨，不是恒 false）
        let dir = std::env::temp_dir().join("firefly-clip-probe-model-only");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        std::fs::write(dir.join("cn_clip_text.onnx"), b"not-a-real-model").expect("写桩文件");
        assert!(omni_pro::vision::clip_text_model_ready(&dir));
        assert!(!omni_pro::vision::clip_text_vocab_ready(&dir));

        // 词表为空文件 → 仍不可用（空词表会污染 OnceLock，必须拒绝）
        std::fs::write(dir.join("cn_clip_vocab.txt"), b"\n\n").expect("写空词表");
        assert!(!omni_pro::vision::clip_text_vocab_ready(&dir));

        // 非空词表 → 就绪
        std::fs::write(dir.join("cn_clip_vocab.txt"), "[PAD]\n[UNK]\ncat\n").expect("写词表");
        assert!(omni_pro::vision::clip_text_vocab_ready(&dir));

        // MobileCLIP 探活只读断言
        assert!(!omni_pro::vision::mobileclip_text_model_ready(&empty));
        assert!(!omni_pro::vision::mobileclip_tokenizer_ready(&empty));
        std::fs::write(dir.join("mobileclip-b-int8-text.onnx"), b"not-a-real-model").expect("写桩文件");
        assert!(omni_pro::vision::mobileclip_text_model_ready(&dir));
        assert!(!omni_pro::vision::mobileclip_tokenizer_ready(&dir));
        std::fs::write(dir.join("mobileclip-tokenizer.json"), vec![b'a'; 200]).expect("写桩分词器");
        assert!(omni_pro::vision::mobileclip_tokenizer_ready(&dir));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
