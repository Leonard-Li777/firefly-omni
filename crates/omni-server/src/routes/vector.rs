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
use omni_pro::vector_engine::encoder_id;

/// 单个向量条目
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorUpsertItem {
    #[serde(alias = "file_fingerprint")]
    pub file_fingerprint: String,
    pub vector: Vec<f32>,
    /// 编码器身份（0=Unknown/Legacy, 1=Bekko, 2=ChineseCLIP, 3=MobileCLIP, 4=Gemma）。
    /// HTTP 层用 `Option<u8>` 接收以便区分缺省；缺省处理见 handler 的 expand 过渡态注释。
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
    /// 编码器白名单：非空数组时，仅对命中编码器的记录打分（过滤发生在
    /// RaBitQ 预判 / 打分 / 排序 / truncate 之前，防止 top-K 被异空间记录占满而饿死）
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

    let vector_engine = state.vector.clone();

    // 【过渡态 · 工单 02 expand】HTTP upsert 缺 `encoderId` 时暂时写入 0（Unknown/Legacy）并告警。
    // 这是 expand 阶段的刻意放宽，**由工单 03 收紧为 400 `encoder_id_required`，不得长期保留**。
    // 缺字节默认 0 只适用于旧落盘文件读取，不适用于 HTTP 入参。
    let batch_items: Vec<omni_pro::vector_engine::VectorUpsertItem> = items
        .into_iter()
        .map(|it| {
            let encoder_id = match it.encoder_id {
                Some(value) => value,
                None => {
                    tracing::warn!(
                        "[vector/upsert] 缺省 encoderId (fp={})，过渡态暂写 0；工单 03 将收紧为 400 encoder_id_required",
                        it.file_fingerprint
                    );
                    encoder_id::UNKNOWN
                }
            };
            omni_pro::vector_engine::VectorUpsertItem {
                file_fingerprint: it.file_fingerprint,
                vector: it.vector,
                encoder_id,
            }
        })
        .collect();

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
        Ok(Err(err)) => Err((
            StatusCode::BAD_REQUEST,
            Json(VectorUpsertResponse {
                success: false,
                count: 0,
                error: Some(err.to_string()),
            }),
        )),
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

    // 【过渡态 · 工单 02 expand】查询缺省 `encoderFilter`（或空数组）时暂时不过滤并告警。
    // 这是 expand 阶段的刻意放宽，**由工单 03 收紧为 400 `encoder_filter_required`，不得长期保留**；
    // 产品契约上该字段必填，禁止把缺省解释为「不过滤」长期存在。
    let encoder_filter = match req.encoder_filter {
        Some(list) if !list.is_empty() => Some(list),
        _ => {
            tracing::warn!(
                "[vector/search] 缺省 encoderFilter（或为空数组），过渡态暂不过滤；工单 03 将收紧为 400 encoder_filter_required"
            );
            None
        }
    };

    let start = Instant::now();
    let result = tokio::task::spawn_blocking(move || {
        vector_engine.search(
            &req.vector,
            req.top_k,
            req.threshold,
            encoder_filter.as_deref(),
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
        Ok(Err(err)) => Err((
            StatusCode::BAD_REQUEST,
            Json(VectorSearchResponse {
                matches: Vec::new(),
                duration_us,
                count: 0,
                error: Some(err.to_string()),
            }),
        )),
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

