use axum::{
    extract::{Json, Multipart},
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
        .with_state(state);

    info!("firefly-omni Axum HTTP server starting on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn get_config(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Json<OmniConfig> {
    let cfg = state.config.lock().unwrap().clone();
    Json(cfg)
}

async fn update_config(
    axum::extract::State(state): axum::extract::State<AppState>,
    Json(new_config): Json<OmniConfig>,
) -> Json<OmniConfig> {
    let mut cfg = state.config.lock().unwrap();
    *cfg = new_config.clone();
    Json(new_config)
}

async fn extract_file_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
    mut multipart: Option<Multipart>,
) -> Json<OmniExtractionResult> {
    let cfg = state.config.lock().unwrap().clone();
    
    // 如果是 Web Multipart 上传
    if let Some(ref mut mp) = multipart {
        while let Ok(Some(field)) = mp.next_field().await {
            let file_name = field.file_name().unwrap_or("temp_file").to_string();
            if let Ok(bytes) = field.bytes().await {
                let temp_dir = std::env::temp_dir();
                let temp_path = temp_dir.join(&file_name);
                if std::fs::write(&temp_path, &bytes).is_ok() {
                    let path_str = temp_path.to_string_lossy().to_string();
                    if let Ok(res) = OmniExtractor::extract(&path_str, &cfg).await {
                        let _ = std::fs::remove_file(&temp_path);
                        return Json(res);
                    }
                    let _ = std::fs::remove_file(&temp_path);
                }
            }
        }
    }

    Json(OmniExtractionResult {
        file_path: "unknown".to_string(),
        mime_type: "application/octet-stream".to_string(),
        file_size: 0,
        markdown_content: "Error: Invalid request".to_string(),
        metadata: serde_json::json!({}),
        phash: None,
        is_corrupted: true,
    })
}
