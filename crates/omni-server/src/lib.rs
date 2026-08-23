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

pub fn create_app_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(|| async { axum::Json(serde_json::json!({ "status": "ok", "server": "firefly-omni" })) }))
        .route("/api/config", get(get_config).post(update_config))
        .route("/api/extract", post(extract_file_handler))
        .route("/api/extract/upload", post(extract_multipart_handler))
        .with_state(state)
}

pub async fn start_server(addr: SocketAddr) -> anyhow::Result<()> {
    let state = AppState {
        config: Arc::new(Mutex::new(OmniConfig::default())),
    };
    let app = create_app_router(state);

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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn setup_test_app() -> Router {
        let state = AppState {
            config: Arc::new(Mutex::new(OmniConfig::default())),
        };
        create_app_router(state)
    }

    #[tokio::test]
    async fn test_health_endpoint() {
        let app = setup_test_app();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["server"], "firefly-omni");
    }

    #[tokio::test]
    async fn test_get_and_update_config_api() {
        let app = setup_test_app();
        
        // 测试 GET /api/config
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let config: OmniConfig = serde_json::from_slice(&body).unwrap();
        assert!(config.enable_document_ocr);

        // 测试 POST /api/config
        let mut new_config = config.clone();
        new_config.max_file_size_mb = 250;
        let req_body = serde_json::to_vec(&new_config).unwrap();

        let post_response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/config")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(post_response.status(), StatusCode::OK);
        let post_body = axum::body::to_bytes(post_response.into_body(), usize::MAX).await.unwrap();
        let updated_config: OmniConfig = serde_json::from_slice(&post_body).unwrap();
        assert_eq!(updated_config.max_file_size_mb, 250);
    }

    #[tokio::test]
    async fn test_extract_file_path_api() {
        let app = setup_test_app();

        // 创建临时测试文件
        let temp_path = std::env::temp_dir().join("omni_api_test.txt");
        std::fs::write(&temp_path, "Firefly Omni API Unit Test Content\n测试段落").unwrap();

        let req_payload = serde_json::json!({
            "file_path": temp_path.to_string_lossy().to_string()
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/extract")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&req_payload).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        let _ = std::fs::remove_file(&temp_path);

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let result: OmniExtractionResult = serde_json::from_slice(&body).unwrap();
        assert_eq!(result.mime_type, "text/plain");
        assert!(result.markdown_content.contains("Firefly Omni API Unit Test"));
    }

    fn resolve_work_folder_path(relative_path: &str) -> std::path::PathBuf {
        if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
            let p = std::path::PathBuf::from(manifest_dir).join("../../../../tests/work-folder").join(relative_path);
            if p.exists() {
                return p;
            }
        }
        let candidates = [
            std::path::PathBuf::from("../../../../tests/work-folder").join(relative_path),
            std::path::PathBuf::from("../../../tests/work-folder").join(relative_path),
            std::path::PathBuf::from("../../tests/work-folder").join(relative_path),
            std::path::PathBuf::from("tests/work-folder").join(relative_path),
        ];
        for cand in candidates {
            if cand.exists() {
                return cand;
            }
        }
        std::path::PathBuf::from("../../../../tests/work-folder").join(relative_path)
    }

    #[tokio::test]
    async fn test_extract_real_pdf_from_work_folder() {
        let app = setup_test_app();
        let real_pdf_path = resolve_work_folder_path("SPEEDY/成都市解除静态管理通知.pdf");
        if !real_pdf_path.exists() {
            println!("Skipping real PDF test: file not found at {:?}", real_pdf_path);
            return;
        }

        let req_payload = serde_json::json!({
            "file_path": real_pdf_path.to_string_lossy().to_string()
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/extract")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&req_payload).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let result: OmniExtractionResult = serde_json::from_slice(&body).unwrap();
        
        assert_eq!(result.mime_type, "application/pdf");
        assert!(!result.is_corrupted);
        assert!(!result.markdown_content.is_empty());
        println!("✅ [Real PDF Test Result]:\n{}", result.markdown_content);
    }

    #[tokio::test]
    async fn test_extract_real_docx_from_work_folder() {
        let app = setup_test_app();
        let real_docx_path = resolve_work_folder_path("SPEEDY/项目模块_功能需求文档_日历调度AI集成需求_V1.docx");
        if !real_docx_path.exists() {
            println!("Skipping real DOCX test: file not found at {:?}", real_docx_path);
            return;
        }

        let req_payload = serde_json::json!({
            "file_path": real_docx_path.to_string_lossy().to_string()
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/extract")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&req_payload).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let result: OmniExtractionResult = serde_json::from_slice(&body).unwrap();
        
        assert!(!result.is_corrupted);
        assert!(!result.markdown_content.is_empty());
        println!("✅ [Real DOCX Test Result]:\n{}", result.markdown_content);
    }

    #[tokio::test]
    async fn test_extract_real_txt_from_work_folder() {
        let app = setup_test_app();
        let real_txt_path = resolve_work_folder_path("PRIVATE/微型小说-出租屋主.txt");
        if !real_txt_path.exists() {
            println!("Skipping real TXT test: file not found at {:?}", real_txt_path);
            return;
        }

        let req_payload = serde_json::json!({
            "file_path": real_txt_path.to_string_lossy().to_string()
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/extract")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&req_payload).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let result: OmniExtractionResult = serde_json::from_slice(&body).unwrap();
        
        assert_eq!(result.mime_type, "text/plain");
        assert!(!result.is_corrupted);
        assert!(!result.markdown_content.is_empty());
        println!("✅ [Real TXT Test Result]:\n{}", result.markdown_content);
    }
}




