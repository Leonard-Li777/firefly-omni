//! vector.rs — 嵌入式向量引擎 HTTP 交互端点 (zvec / RaBitQ / INT8)
//!
//! 依据 ADR-0038 与 PRD #679 架构决议：
//! - POST /api/v1/vector/upsert: 写入/更新特征向量 (适配 bekko-a8m 384 维)；
//! - POST /api/v1/vector/search: < 1ms Top-K ANN 检索 (RaBitQ 初筛 + INT8 重排)；
//! - DELETE /api/v1/vector/delete: 批量删除特征向量。

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
    /// 384 维浮点查询特征向量
    pub vector: Vec<f32>,
    /// 最大召回数 (默认 10)
    #[serde(default = "default_top_k", alias = "top_k")]
    pub top_k: usize,
    /// 相似度阈值 ([-1.0, 1.0]，低于此阈值的项被过滤)
    pub threshold: Option<f32>,
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

    // 在 spawn_blocking 中执行批量量化与落盘，避免阻塞异步运行时
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
        let mut processed = 0;
        for item in items {
            vector_engine.upsert(&item.file_fingerprint, &item.vector)?;
            processed += 1;
        }
        Ok(processed)
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

    let start = Instant::now();
    let result = tokio::task::spawn_blocking(move || {
        vector_engine.search(&req.vector, req.top_k, req.threshold)
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

/// 密集向量段落对齐处理器: POST /api/v1/vector/match-passages
///
/// 依据 ADR-0039 与 PRD 0039 契约：
/// 1. 仅对 query 编码 1 次生成 384 维特征向量；
/// 2. 利用 bekko-a8m 模型与余弦相似度对各文件的候选段落进行批量点积打分；
/// 3. 返回最佳匹配段落及其下标与相似度，单次耗时控制在 10~15ms 以内。
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
                let p_vec = embedder.embed(passage)?;
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
