use axum::{
    extract::{Json, Multipart, State},
    routing::{get, post},
    Router,
};
use omni_core::{OmniConfig, OmniExtractionResult};
use omni_extract::OmniExtractor;
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tracing::info;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Mutex<OmniConfig>>,
}

#[derive(Deserialize)]
pub struct ExtractRequest {
    pub file_path: String,
}

pub async fn start_server(addr: SocketAddr) -> anyhow::Result<()> {
    let state = AppState {
        config: Arc::new(Mutex::new(OmniConfig::default())),
    };

    let app = Router::new()
        .route("/health", get(|| async { axum::Json(serde_json::json!({ "status": "ok", "server": "firefly-omni" })) }))
        .route("/api/config", get(get_config).post(update_config))
        .route("/api/extract", post(extract_file_handler))
        .route("/api/extract/upload", post(extract_multipart_handler))
        .with_state(state);

    info!("firefly-omni Axum HTTP server starting on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
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
    Json(new_config)
}

/// 处理本地 JSON 文件路径提取请求: POST /api/extract { "file_path": "/path/to/file" }
async fn extract_file_handler(
    State(state): State<AppState>,
    Json(req): Json<ExtractRequest>,
) -> Json<OmniExtractionResult> {
    let cfg = state.config.lock().unwrap().clone();
    if !req.file_path.is_empty() {
        if let Ok(res) = OmniExtractor::extract(&req.file_path, &cfg).await {
            return Json(res);
        }
    }
    Json(OmniExtractionResult {
        file_path: req.file_path,
        mime_type: "application/octet-stream".to_string(),
        file_size: 0,
        markdown_content: "Error: File extraction failed".to_string(),
        metadata: serde_json::json!({}),
        phash: None,
        is_corrupted: true,
    })
}

/// 处理 Web UI 前端拖拽文件二进制流上传请求: POST /api/extract/upload
async fn extract_multipart_handler(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Json<OmniExtractionResult> {
    let cfg = state.config.lock().unwrap().clone();

    while let Ok(Some(field)) = multipart.next_field().await {
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

    Json(OmniExtractionResult {
        file_path: "unknown".to_string(),
        mime_type: "application/octet-stream".to_string(),
        file_size: 0,
        markdown_content: "Error: Multipart file upload extraction failed".to_string(),
        metadata: serde_json::json!({}),
        phash: None,
        is_corrupted: true,
    })
}




