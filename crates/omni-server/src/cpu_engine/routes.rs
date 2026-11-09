use super::generator::CpuGeneratorConfig;
use super::slot::CpuSlot;
use super::types::{
    ApiErrorResponse, ChatCompletionChoice, ChatCompletionMessage, ChatCompletionRequest,
    ChatCompletionResponse, ChatCompletionUsage, EngineHealthResponse, SessionDrainQuery,
    SessionPreheatRequest, TokenizeRequest,
};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// GET /v1/health 只读健康状态快照探针
pub async fn get_health_handler(State(slot): State<Arc<CpuSlot>>) -> impl IntoResponse {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let snapshot = slot.get_snapshot();
    let resp = EngineHealthResponse {
        status: "ok".to_string(),
        pid: std::process::id(),
        uptime_seconds: now.saturating_sub(snapshot.last_active_timestamp),
        active_tasks: snapshot.active_tasks,
        session: snapshot,
    };
    (StatusCode::OK, Json(resp))
}

/// POST /v1/engines/cpu/session 显式预热端点
pub async fn post_session_handler(
    State(slot): State<Arc<CpuSlot>>,
    Json(payload): Json<SessionPreheatRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiErrorResponse>)> {
    match slot.preheat(payload.reset_crash_count).await {
        Ok(resp) => Ok((StatusCode::OK, Json(resp))),
        Err((code, msg)) => Err((code, Json(ApiErrorResponse::new("engine_error", msg, "invalid_request_error")))),
    }
}

/// DELETE /v1/engines/cpu/session 资源销毁与排空端点
pub async fn delete_session_handler(
    State(slot): State<Arc<CpuSlot>>,
    Query(query): Query<SessionDrainQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiErrorResponse>)> {
    match slot.drain(query.wait_drain_ms).await {
        Ok(resp) => Ok((StatusCode::OK, Json(resp))),
        Err((code, msg)) => Err((code, Json(ApiErrorResponse::new("drain_error", msg, "invalid_request_error")))),
    }
}

/// POST /v1/tokenize 独立分词端点 (常驻分词器，绝不隐式加载模型)
pub async fn post_tokenize_handler(
    State(slot): State<Arc<CpuSlot>>,
    Json(payload): Json<TokenizeRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiErrorResponse>)> {
    match slot.tokenize(&payload.texts) {
        Ok(resp) => Ok((StatusCode::OK, Json(resp))),
        Err((code, msg)) => Err((code, Json(ApiErrorResponse::new("tokenize_error", msg, "invalid_request_error")))),
    }
}

/// POST /v1/chat/completions 标准对话补全端点
pub async fn post_chat_completions_handler(
    State(slot): State<Arc<CpuSlot>>,
    Json(payload): Json<ChatCompletionRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiErrorResponse>)> {
    // 1. 获取任务租约与 ActiveTaskGuard 守卫 (若模型未就绪或内存不足直接短路)
    let _guard = match slot.try_begin_task() {
        Ok(g) => g,
        Err((code, reason)) => {
            return Err((
                code,
                Json(ApiErrorResponse::new(reason, format!("CPU 推理引擎不可用: {reason}"), "service_unavailable")),
            ));
        }
    };

    // 2. 组装输入文本并计算 Prompt Tokens
    let mut combined_text = String::new();
    for msg in &payload.messages {
        combined_text.push_str(&msg.role);
        combined_text.push_str(": ");
        combined_text.push_str(&msg.content);
        combined_text.push('\n');
    }

    let prompt_tokens = match slot.tokenize(&[combined_text]) {
        Ok(mut t) => t.tokens.pop().unwrap_or_default(),
        Err((code, msg)) => return Err((code, Json(ApiErrorResponse::new("tokenize_failed", msg, "invalid_request_error")))),
    };

    let max_tokens = payload.max_tokens.unwrap_or(48);
    let mut config = CpuGeneratorConfig::default();
    config.task_id = payload.task_id.clone();
    config.max_new_tokens = max_tokens;
    if let Some(temp) = payload.temperature {
        config.temperature = temp.clamp(0.0, 1.0);
    }

    // 3. 执行自回归生成循环 (协作式取消句柄)
    let cancel = Arc::new(AtomicBool::new(false));
    let output = match slot.execute_generate(&prompt_tokens, &config, &cancel) {
        Ok(out) => out,
        Err((code, msg)) => return Err((code, Json(ApiErrorResponse::new("generation_failed", msg, "internal_error")))),
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let resp = ChatCompletionResponse {
        id: format!("chatcmpl-omni-{}", now),
        object: "chat.completion",
        created: now,
        model: payload.model,
        choices: vec![ChatCompletionChoice {
            index: 0,
            message: ChatCompletionMessage {
                role: "assistant".to_string(),
                content: output.text,
            },
            finish_reason: output.finish_reason,
        }],
        usage: ChatCompletionUsage {
            prompt_tokens: output.prompt_tokens_count,
            completion_tokens: output.completion_tokens_count,
            total_tokens: output.prompt_tokens_count + output.completion_tokens_count,
        },
    };

    Ok((StatusCode::OK, Json(resp)))
}
