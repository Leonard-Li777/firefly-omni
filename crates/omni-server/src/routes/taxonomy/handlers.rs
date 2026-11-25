use axum::{
    extract::{Query, State},
    Json,
};
use crate::AppState;
use super::types::*;
use super::tree_query::*;
use super::alias_query::*;
use super::inspect::*;

/// 获取分类树：GET /api/v1/taxonomy/tree?locale={locale}&root={root}
pub async fn taxonomy_tree_handler(
    State(state): State<AppState>,
    Query(query): Query<TaxonomyTreeQuery>,
) -> Json<TaxonomyTreeResponse> {
    let locale = query.locale.unwrap_or_else(|| "zh-CN".to_string());
    let root_filter = query.root;
    let explicit_db_path = query.db_path.map(std::path::PathBuf::from);
    let include_files = query.include_files.unwrap_or(false);
    let workspace_id = query.workspace_id;
    let directory_prefix = query.directory_prefix;

    let locale_for_err = locale.clone();
    let res = tokio::task::spawn_blocking(move || {
        let master_db_path = explicit_db_path.or_else(|| {
            state.master_db_path.lock().ok().and_then(|opt| opt.clone())
        });
        state.omw.with_conn(|conn| {
            query_taxonomy_tree(
                conn,
                &locale,
                root_filter.as_deref(),
                master_db_path.as_deref(),
                include_files,
                workspace_id,
                directory_prefix.as_deref(),
            )
        })
    })
    .await;

    match res {
        Ok(Ok(tree)) => Json(tree),
        Ok(Err(err)) => {
            tracing::warn!("[taxonomy_tree_handler] 查询分类树异常或未就绪: {err}");
            Json(TaxonomyTreeResponse {
                locale: locale_for_err,
                root_nodes: Vec::new(),
                total_nodes: 0,
            })
        }
        Err(join_err) => {
            tracing::warn!("[taxonomy_tree_handler] 后台任务异常终止: {join_err}");
            Json(TaxonomyTreeResponse {
                locale: locale_for_err,
                root_nodes: Vec::new(),
                total_nodes: 0,
            })
        }
    }
}

/// 获取多语言别名字典：GET /api/v1/taxonomy/aliases?locale={locale}&source={source}&codes={codes}
pub async fn taxonomy_aliases_handler(
    State(state): State<AppState>,
    Query(query): Query<TaxonomyAliasesQuery>,
) -> Json<Vec<TagAliasRow>> {
    let locale = query
        .locale
        .or_else(|| state.config.lock().ok().and_then(|c| c.language.clone()))
        .unwrap_or_else(|| "zh-CN".to_string());
    let source = query.source.clone();
    let codes = query.codes.clone();
    let prefix = query.prefix.clone();

    let res = tokio::task::spawn_blocking(move || {
        state.omw.with_conn(|conn| {
            query_tag_alias_rows(
                conn,
                &locale,
                source.as_deref(),
                codes.as_deref(),
                prefix.as_deref(),
            )
        })
    })
    .await;

    match res {
        Ok(Ok(rows)) => Json(rows),
        Ok(Err(err)) => {
            tracing::warn!("[taxonomy_aliases_handler] 查询别名映射异常或未就绪: {err}");
            Json(Vec::new())
        }
        Err(join_err) => {
            tracing::warn!("[taxonomy_aliases_handler] 后台任务异常终止: {join_err}");
            Json(Vec::new())
        }
    }
}

/// 受控标签鉴别与祖先闭包判定端点: POST /api/v1/taxonomy/inspect-tag
pub async fn taxonomy_inspect_tag_handler(
    State(state): State<AppState>,
    Json(req): Json<InspectTagBatchRequest>,
) -> Json<InspectTagBatchResponse> {
    let locale = req
        .language
        .or_else(|| state.config.lock().ok().and_then(|c| c.language.clone()))
        .unwrap_or_else(|| "zh-CN".to_string());
    let items = req.items;

    let res = tokio::task::spawn_blocking(move || {
        state.omw.with_conn(move |conn| {
            inspect_tags_batch(conn, &items, &locale)
        })
    })
    .await;

    match res {
        Ok(Ok(results)) => Json(InspectTagBatchResponse {
            success: true,
            results,
        }),
        Ok(Err(err)) => {
            tracing::warn!("[taxonomy_inspect_tag_handler] 鉴别标签异常: {err}");
            Json(InspectTagBatchResponse {
                success: false,
                results: Vec::new(),
            })
        }
        Err(join_err) => {
            tracing::warn!("[taxonomy_inspect_tag_handler] 后台任务异常终止: {join_err}");
            Json(InspectTagBatchResponse {
                success: false,
                results: Vec::new(),
            })
        }
    }
}

pub async fn fast_recognize_handler(
    State(state): State<AppState>,
    Json(ctx): Json<omni_pro::text::FastRecognizeContext>,
) -> Json<omni_pro::text::FastRecognizeResponse> {
    let res = tokio::task::spawn_blocking(move || {
        let embedder = omni_pro::text::BekkoEmbedder::new();

        static GLOBAL_VECTOR_TABLE: std::sync::OnceLock<Option<omni_pro::text::PrecomputedVectorTable>> =
            std::sync::OnceLock::new();
        let vector_table_ref = GLOBAL_VECTOR_TABLE.get_or_init(|| {
            omni_pro::semantic_loader::SemanticPackLoader::load_dense_embeddings().ok()
        });

        match state.omw.with_conn(|conn| {
            Ok(omni_pro::text::FastTagRecognizer::recognize(
                &ctx,
                Some(conn),
                vector_table_ref.as_ref(),
                &embedder,
            ))
        }) {
            Ok(outcome) => outcome,
            Err(err) => {
                tracing::warn!("[fast_recognize_handler] 数据库离线或任务异常，执行降级纯文本识别: {err}");
                omni_pro::text::FastTagRecognizer::recognize(
                    &ctx,
                    None,
                    vector_table_ref.as_ref(),
                    &embedder,
                )
            }
        }
    })
    .await;

    match res {
        Ok(outcome) => Json(outcome),
        Err(join_err) => {
            tracing::warn!("[fast_recognize_handler] 后台任务异常终止: {join_err}");
            Json(omni_pro::text::FastRecognizeResponse {
                tags: Vec::new(),
                elapsed_ms: 0.0,
                total_candidates: 0,
            })
        }
    }
}
