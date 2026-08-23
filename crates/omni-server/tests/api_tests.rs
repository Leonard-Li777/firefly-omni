use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use omni_core::{OmniConfig, OmniExtractionResult};
use omni_server::{create_app_router, AppState};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

fn setup_test_app() -> Router {
    let state = AppState {
        config: Arc::new(Mutex::new(OmniConfig::default())),
    };
    create_app_router(state)
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

#[tokio::test]
async fn test_extract_real_pdf_from_work_folder() {
    let app = setup_test_app();
    let real_pdf_path = resolve_work_folder_path("SPEEDY/成都市解除静态管理通知.pdf");
    assert!(real_pdf_path.exists(), "Real PDF file should exist at {:?}", real_pdf_path);

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
}

#[tokio::test]
async fn test_extract_real_docx_from_work_folder() {
    let app = setup_test_app();
    let real_docx_path = resolve_work_folder_path("SPEEDY/项目模块_功能需求文档_日历调度AI集成需求_V1.docx");
    assert!(real_docx_path.exists(), "Real DOCX file should exist at {:?}", real_docx_path);

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
    assert!(result.markdown_content.contains("视图与排程体验好"));
}

#[tokio::test]
async fn test_extract_real_txt_from_work_folder() {
    let app = setup_test_app();
    let real_txt_path = resolve_work_folder_path("PRIVATE/微型小说-出租屋主.txt");
    assert!(real_txt_path.exists(), "Real TXT file should exist at {:?}", real_txt_path);

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
    assert!(result.markdown_content.contains("出租屋成了社会治安的永久性热点"));
}
