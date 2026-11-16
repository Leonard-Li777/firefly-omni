//! routes/modules.rs — 统一模块休眠与状态对账端点 (spec §3.4.2)

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};

use crate::cpu_engine::types::SessionLifecycle;
use crate::AppState;

/// 严格受控可休眠模块白名单 (对齐 TS 端 HIBERNATABLE_TARGET_CATALOG)
pub const HIBERNATABLE_MODULE_IDS: &[&str] = &[
    "pocket-cpu-int8-onnx",
    "asr-sensevoice-onnx",
    "tts-m5-onnx",
    "tts-moss-onnx",
];

// 各模块预估内存释放字节数（对齐 TS 端 HIBERNATABLE_TARGET_CATALOG）
const POCKET_CPU_RELEASED_BYTES: u64 = 380 * 1024 * 1024;
const ASR_SENSEVOICE_RELEASED_BYTES: u64 = 250 * 1024 * 1024;
const TTS_M5_RELEASED_BYTES: u64 = 315 * 1024 * 1024;
const TTS_MOSS_RELEASED_BYTES: u64 = 450 * 1024 * 1024;

#[derive(Debug, Deserialize)]
pub struct ModuleDrainQuery {
    pub wait_drain_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleDrainResponse {
    pub success: bool,
    pub status: String,
    pub released_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleErrorResponse {
    pub success: bool,
    pub error: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleStatusResponse {
    pub loaded: bool,
    pub resident_bytes: u64,
    pub service_reachable: bool,
}

/// DELETE /v1/modules/:module_id/session
pub async fn delete_module_session_handler(
    State(state): State<AppState>,
    Path(module_id): Path<String>,
    Query(query): Query<ModuleDrainQuery>,
) -> impl IntoResponse {
    if !HIBERNATABLE_MODULE_IDS.contains(&module_id.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                serde_json::to_value(ModuleErrorResponse {
                    success: false,
                    error: "MODULE_NOT_HIBERNATABLE".to_string(),
                })
                .unwrap(),
            ),
        );
    }

    match module_id.as_str() {
        "pocket-cpu-int8-onnx" => {
            let snapshot = state.cpu_slot.get_snapshot();
            let is_already_unloaded = !matches!(snapshot.lifecycle, SessionLifecycle::Ready);
            if is_already_unloaded {
                return (
                    StatusCode::OK,
                    Json(
                        serde_json::to_value(ModuleDrainResponse {
                            success: true,
                            status: "unloaded".to_string(),
                            released_bytes: 0,
                            message: Some("module_already_unloaded".to_string()),
                        })
                        .unwrap(),
                    ),
                );
            }
            let wait_ms = query.wait_drain_ms.unwrap_or(3000);
            match state.cpu_slot.drain(wait_ms).await {
                Ok(drain_res) => (
                    StatusCode::OK,
                    Json(
                        serde_json::to_value(ModuleDrainResponse {
                            success: true,
                            status: "unloaded".to_string(),
                            released_bytes: drain_res.released_bytes,
                            message: None,
                        })
                        .unwrap(),
                    ),
                ),
                Err((code, msg)) => (
                    code,
                    Json(
                        serde_json::to_value(ModuleErrorResponse {
                            success: false,
                            error: msg,
                        })
                        .unwrap(),
                    ),
                ),
            }
        }
        "asr-sensevoice-onnx" => {
            if !state.sherpa.is_asr_ready() {
                return (
                    StatusCode::OK,
                    Json(
                        serde_json::to_value(ModuleDrainResponse {
                            success: true,
                            status: "unloaded".to_string(),
                            released_bytes: 0,
                            message: Some("module_already_unloaded".to_string()),
                        })
                        .unwrap(),
                    ),
                );
            }
            state.sherpa.drop_session();
            (
                StatusCode::OK,
                Json(
                    serde_json::to_value(ModuleDrainResponse {
                        success: true,
                        status: "unloaded".to_string(),
                        released_bytes: ASR_SENSEVOICE_RELEASED_BYTES,
                        message: None,
                    })
                    .unwrap(),
                ),
            )
        }
        "tts-m5-onnx" => {
            if !state.m5.is_ready() {
                return (
                    StatusCode::OK,
                    Json(
                        serde_json::to_value(ModuleDrainResponse {
                            success: true,
                            status: "unloaded".to_string(),
                            released_bytes: 0,
                            message: Some("module_already_unloaded".to_string()),
                        })
                        .unwrap(),
                    ),
                );
            }
            state.m5.drop_session();
            (
                StatusCode::OK,
                Json(
                    serde_json::to_value(ModuleDrainResponse {
                        success: true,
                        status: "unloaded".to_string(),
                        released_bytes: TTS_M5_RELEASED_BYTES,
                        message: None,
                    })
                    .unwrap(),
                ),
            )
        }
        "tts-moss-onnx" => {
            if !state.moss.is_ready() {
                return (
                    StatusCode::OK,
                    Json(
                        serde_json::to_value(ModuleDrainResponse {
                            success: true,
                            status: "unloaded".to_string(),
                            released_bytes: 0,
                            message: Some("module_already_unloaded".to_string()),
                        })
                        .unwrap(),
                    ),
                );
            }
            state.moss.drop_session();
            (
                StatusCode::OK,
                Json(
                    serde_json::to_value(ModuleDrainResponse {
                        success: true,
                        status: "unloaded".to_string(),
                        released_bytes: TTS_MOSS_RELEASED_BYTES,
                        message: None,
                    })
                    .unwrap(),
                ),
            )
        }
        _ => unreachable!(),
    }
}

/// GET /v1/modules/:module_id/session/status
pub async fn get_module_session_status_handler(
    State(state): State<AppState>,
    Path(module_id): Path<String>,
) -> impl IntoResponse {
    if !HIBERNATABLE_MODULE_IDS.contains(&module_id.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                serde_json::to_value(ModuleErrorResponse {
                    success: false,
                    error: "MODULE_NOT_HIBERNATABLE".to_string(),
                })
                .unwrap(),
            ),
        );
    }

    let (loaded, resident_bytes) = match module_id.as_str() {
        "pocket-cpu-int8-onnx" => {
            let loaded = matches!(
                state.cpu_slot.get_snapshot().lifecycle,
                SessionLifecycle::Ready
            );
            (loaded, if loaded { POCKET_CPU_RELEASED_BYTES } else { 0 })
        }
        "asr-sensevoice-onnx" => {
            let loaded = state.sherpa.is_asr_ready();
            (
                loaded,
                if loaded {
                    ASR_SENSEVOICE_RELEASED_BYTES
                } else {
                    0
                },
            )
        }
        "tts-m5-onnx" => {
            let loaded = state.m5.is_ready();
            (loaded, if loaded { TTS_M5_RELEASED_BYTES } else { 0 })
        }
        "tts-moss-onnx" => {
            let loaded = state.moss.is_ready();
            (loaded, if loaded { TTS_MOSS_RELEASED_BYTES } else { 0 })
        }
        _ => (false, 0),
    };

    (
        StatusCode::OK,
        Json(
            serde_json::to_value(ModuleStatusResponse {
                loaded,
                resident_bytes,
                service_reachable: true,
            })
            .unwrap(),
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hibernatable_module_whitelist() {
        assert_eq!(HIBERNATABLE_MODULE_IDS.len(), 4);
        assert!(HIBERNATABLE_MODULE_IDS.contains(&"pocket-cpu-int8-onnx"));
        assert!(HIBERNATABLE_MODULE_IDS.contains(&"asr-sensevoice-onnx"));
        assert!(HIBERNATABLE_MODULE_IDS.contains(&"tts-m5-onnx"));
        assert!(HIBERNATABLE_MODULE_IDS.contains(&"tts-moss-onnx"));

        // 核心基座或未纳管模块绝对不在休眠白名单中
        assert!(!HIBERNATABLE_MODULE_IDS.contains(&"bekko"));
        assert!(!HIBERNATABLE_MODULE_IDS.contains(&"fts5"));
        assert!(!HIBERNATABLE_MODULE_IDS.contains(&"unknown-module"));
    }
}
