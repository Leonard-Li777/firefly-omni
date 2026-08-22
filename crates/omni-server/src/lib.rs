use axum::{
    extract::Json,
    routing::{get, post},
    Router,
};
use omni_core::{OmniConfig, OmniExtractionResult};
use omni_extract::OmniExtractor;
use serde::{Deserialize, Serialize};
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
        .route("/health", get(|| async { "OK" }))
        .route("/api/config", get(get_config).post(update_config))
        .route("/api/extract", post(extract_file))
        .with_state(state);

    info!("firefly-omni HTTP server starting on {}", addr);
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

async fn extract_file(
    axum::extract::State(state): axum::extract::State<AppState>,
    Json(req): Json<ExtractRequest>,
) -> Json<OmniExtractionResult> {
    let cfg = state.config.lock().unwrap().clone();
    match OmniExtractor::extract(&req.file_path, &cfg).await {
        Ok(res) => Json(res),
        Err(err) => Json(OmniExtractionResult {
            file_path: req.file_path,
            mime_type: "application/octet-stream".to_string(),
            file_size: 0,
            markdown_content: format!("Error extracting file: {}", err),
            metadata: serde_json::json!({}),
            phash: None,
            is_corrupted: true,
        }),
    }
}
