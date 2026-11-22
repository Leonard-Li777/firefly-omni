//! routes/nsfw.rs — 原生多模态敏感内容鉴别端点 (POST /api/text/nsfw)
//!
//! 统一复用 `omni_pro::OmniVisionEngine`：
//! - 纯文本 / 文档路径：FastText + `nsfw-l3` + 规则引擎（支持暴恐/涉政/违规/色情/辱骂/血腥多维鉴别）
//! - 图片路径 / Base64 图片：`nsfw-mobilenetv2` 视觉 5 分类推理 + 文本审查联合仲裁

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use base64::Engine as _;
use omni_extract::OmniExtractor;
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::AppState;

/// 解码后最大允许图片字节数（16 MiB），防止恶意超大图 OOM
const MAX_DECODED_IMAGE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Deserialize)]
pub struct NsfwAssertRequest {
    /// 纯文本内容（可选）
    #[serde(default)]
    pub text: Option<String>,
    /// 本地文件路径（支持图片或文本文档，可选）
    #[serde(default)]
    pub file_path: Option<String>,
    /// Base64 编码图片（支持带 data:image/...;base64, 前缀或裸 Base64，可选）
    #[serde(default)]
    pub image_base64: Option<String>,
    /// 语言提示（可选，如 zh / en / ja）
    #[serde(default)]
    pub language: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct NsfwAssertResponse {
    pub success: bool,
    pub is_sensitive: bool,
    /// clean | flagged | skipped
    pub status: String,
    /// 命中的敏感标签（已过滤“全年龄”/“安全”）
    pub tags: Vec<String>,
    /// 敏感类型分类
    pub sensitive_types: Vec<String>,
    /// 内容分级（如 safe / R-15 / R-18 / R-18G）
    pub content_rating: Option<String>,
    /// 视觉 MobileNetV2 5 分类概率（仅图片输入时返回）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nsfw_probs: Option<[f32; 5]>,
    /// 声明支持鉴别的 6 大核心维度
    pub supported_dimensions: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct NsfwErrorBody {
    success: bool,
    error: String,
    message: String,
}

fn is_image_ext(file_path: &str) -> bool {
    let ext = Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    matches!(
        ext.as_deref(),
        Some(
            "jpg"
                | "jpeg"
                | "png"
                | "webp"
                | "bmp"
                | "gif"
                | "tif"
                | "tiff"
                | "avif"
                | "heic"
                | "heif"
        )
    )
}

fn strip_base64_data_uri(raw: &str) -> &str {
    let trimmed = raw.trim();
    if let Some(idx) = trimmed.find(";base64,") {
        &trimmed[idx + ";base64,".len()..]
    } else {
        trimmed
    }
}

/// POST /api/text/nsfw — 原生敏感内容断言（同时支持纯文本、本地图片/文档、Base64 图片）
pub async fn nsfw_assert_handler(
    State(state): State<AppState>,
    Json(req): Json<NsfwAssertRequest>,
) -> impl IntoResponse {
    let raw_text = req
        .text
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let raw_file_path = req
        .file_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let raw_image_b64 = req
        .image_base64
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    if raw_text.is_none() && raw_file_path.is_none() && raw_image_b64.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!(NsfwErrorBody {
                success: false,
                error: "INVALID_ARGUMENT".to_string(),
                message: "至少需要提供 text、file_path 或 image_base64 之一".to_string(),
            })),
        )
            .into_response();
    }

    let cfg = state.config.lock().unwrap().clone();
    let mut nsfw_probs: Option<[f32; 5]> = None;
    let mut text_parts: Vec<String> = Vec::new();
    if let Some(t) = raw_text {
        text_parts.push(t);
    }

    // 1. 若提供 Base64 图片：内存直接解码并执行 nsfw-mobilenetv2 推理
    if let Some(b64_str) = raw_image_b64 {
        let payload = strip_base64_data_uri(&b64_str);
        // 粗略长度前置守卫（Base64 膨胀约 4/3）
        if payload.len() > MAX_DECODED_IMAGE_BYTES * 4 / 3 + 16 {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(serde_json::json!(NsfwErrorBody {
                    success: false,
                    error: "PAYLOAD_TOO_LARGE".to_string(),
                    message: "Base64 图片体积超过 16MB 限制".to_string(),
                })),
            )
                .into_response();
        }
        let decoded = match base64::engine::general_purpose::STANDARD.decode(payload) {
            Ok(bytes) => bytes,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!(NsfwErrorBody {
                        success: false,
                        error: "INVALID_BASE64".to_string(),
                        message: format!("无效的 image_base64 数据: {}", e),
                    })),
                )
                    .into_response();
            }
        };
        if decoded.len() > MAX_DECODED_IMAGE_BYTES {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(serde_json::json!(NsfwErrorBody {
                    success: false,
                    error: "PAYLOAD_TOO_LARGE".to_string(),
                    message: "解码后图片体积超过 16MB 限制".to_string(),
                })),
            )
                .into_response();
        }
        let img = match image::load_from_memory(&decoded) {
            Ok(i) => i,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!(NsfwErrorBody {
                        success: false,
                        error: "INVALID_IMAGE".to_string(),
                        message: format!("无法解码图片数据: {}", e),
                    })),
                )
                    .into_response();
            }
        };
        if omni_pro::is_pro_enabled() {
            nsfw_probs = tokio::task::spawn_blocking(move || {
                omni_pro::OmniVisionEngine::run_nsfw_model(&img)
            })
            .await
            .ok()
            .flatten();
        }
    }

    // 2. 若提供本地文件路径：图片走 nsfw-mobilenetv2，文本文档抽取正文走 FastText + nsfw-l3
    if let Some(ref fp) = raw_file_path {
        if !Path::new(fp).exists() {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!(NsfwErrorBody {
                    success: false,
                    error: "FILE_NOT_FOUND".to_string(),
                    message: format!("目标文件不存在: {}", fp),
                })),
            )
                .into_response();
        }
        if is_image_ext(fp) {
            if nsfw_probs.is_none() && omni_pro::is_pro_enabled() {
                let fp_clone = fp.clone();
                let open_res = tokio::task::spawn_blocking(move || {
                    image::open(&fp_clone)
                        .map(|img| omni_pro::OmniVisionEngine::run_nsfw_model(&img))
                })
                .await;
                match open_res {
                    Ok(Ok(probs)) => nsfw_probs = probs,
                    Ok(Err(e)) => {
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(serde_json::json!(NsfwErrorBody {
                                success: false,
                                error: "INVALID_IMAGE".to_string(),
                                message: format!("无法打开图片文件: {}", e),
                            })),
                        )
                            .into_response();
                    }
                    Err(e) => {
                        tracing::warn!("[OmniServer] /api/text/nsfw 图片推理任务异常: {}", e);
                    }
                }
            }
        } else {
            // 文档/文本文件抽取正文并入审查文本
            match OmniExtractor::extract(fp, &cfg).await {
                Ok(ext) => {
                    let trimmed = ext.markdown_content.trim();
                    if !trimmed.is_empty() {
                        text_parts.push(trimmed.to_string());
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "[OmniServer] /api/text/nsfw 抽取文档正文失败 (file={}): {}",
                        fp,
                        e
                    );
                }
            }
        }
    }

    // 3. 联合仲裁：FastText + nsfw-l3 (文本) + nsfw-mobilenetv2 (视觉)
    let combined_text = text_parts.join("\n");
    let language = req.language.clone();
    let file_path_hint = raw_file_path.clone();

    let (tags, sensitive_types, content_rating) = if omni_pro::is_pro_enabled() {
        tokio::task::spawn_blocking(move || {
            omni_pro::OmniVisionEngine::derive_nsfw_tags_and_rating_full(
                nsfw_probs,
                &combined_text,
                &[],
                language.as_deref(),
                file_path_hint.as_deref(),
            )
        })
        .await
        .unwrap_or_else(|_| (vec!["全年龄".to_string()], Vec::new(), Some("safe".to_string())))
    } else {
        (vec!["全年龄".to_string()], Vec::new(), Some("safe".to_string()))
    };

    let flagged_tags: Vec<String> = tags
        .into_iter()
        .filter(|t| t != "全年龄" && t != "安全")
        .collect();
    let is_sensitive = !flagged_tags.is_empty() || !sensitive_types.is_empty();
    let status = if is_sensitive { "flagged" } else { "clean" }.to_string();

    (
        StatusCode::OK,
        Json(serde_json::json!(NsfwAssertResponse {
            success: true,
            is_sensitive,
            status,
            tags: flagged_tags,
            sensitive_types,
            content_rating,
            nsfw_probs,
            supported_dimensions: vec!["暴恐", "涉政", "违规", "色情", "辱骂", "血腥"],
        })),
    )
        .into_response()
}
