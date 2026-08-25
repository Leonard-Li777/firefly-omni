use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use omni_core::{OmniConfig, OmniExtractionResult};
use omni_server::{create_app_router, AppState};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

fn setup_test_app() -> Router {
    // 默认注入"未配置数据集"的软不可用地理服务
    setup_test_app_with_geo(Arc::new(omni_pro::geo::GeoService::unavailable()))
}

/// 以指定地理服务构造测试路由（geo 用例通过内嵌夹具 JSON 注入可用实例）
fn setup_test_app_with_geo(geo: Arc<omni_pro::geo::GeoService>) -> Router {
    let state = AppState {
        config: Arc::new(Mutex::new(OmniConfig::default())),
        geo,
    };
    create_app_router(state)
}

/// 内嵌密封夹具：与 omni-geo 集成测试同构的最小数据集（零网络、零外部文件）
const GEO_FIXTURE_JSON: &str = r#"{
  "version": 20260824,
  "points": [
    { "id": 1, "lat": 22.54, "lng": 114.06, "cc": "CN", "ad1": "44", "pop": 12500000,
      "n": { "en": "Shenzhen", "zh": "深圳市", "ja": "深セン" } },
    { "id": 2, "lat": 23.14, "lng": 114.06, "cc": "CN", "ad1": "44", "pop": 900000,
      "n": { "en": "Heyuan", "zh": "河源市" } }
  ],
  "admin1": { "CN.44": { "en": "Guangdong", "zh": "广东省" } },
  "admin2": {},
  "countries": { "CN": { "en": "China", "zh": "中国" } }
}"#;

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
async fn test_extract_user_pdf_metadata() {
    let pdf_path = std::path::PathBuf::from(r"F:\lilun\Desktop\经济学_国家财富估算与GDP对比_Annual-Wealth-Estimates-for-Chin.pdf");
    if !pdf_path.exists() {
        println!("File does not exist: {:?}", pdf_path);
        return;
    }

    let app = setup_test_app();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/extract")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&serde_json::json!({
                    "file_path": pdf_path.to_string_lossy().to_string()
                })).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let result: OmniExtractionResult = serde_json::from_slice(&body).unwrap();
    println!("DEBUG METADATA FOR USER PDF:\n{:#?}", result.metadata);
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

#[tokio::test]
async fn test_extract_epub_and_doc_documents() {
    let app = setup_test_app();

    // 1. 验证 .epub 扩展名文件提取
    let temp_epub = std::env::temp_dir().join("test_book.epub");
    std::fs::write(&temp_epub, b"PK\x03\x04Dummy Epub Zip Binary Content").unwrap();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/extract")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&serde_json::json!({
                    "file_path": temp_epub.to_string_lossy().to_string()
                })).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    let _ = std::fs::remove_file(&temp_epub);
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let result: OmniExtractionResult = serde_json::from_slice(&body).unwrap();
    assert!(!result.markdown_content.contains("NUL byte detected"));

    // 2. 验证 .doc 扩展名文件提取
    let temp_doc = std::env::temp_dir().join("test_doc.doc");
    std::fs::write(&temp_doc, b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1\x00\x00Dummy OLE Binary Content\nHeader Line Text").unwrap();

    let response2 = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/extract")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&serde_json::json!({
                    "file_path": temp_doc.to_string_lossy().to_string()
                })).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    let _ = std::fs::remove_file(&temp_doc);
    assert_eq!(response2.status(), StatusCode::OK);
    let body2 = axum::body::to_bytes(response2.into_body(), usize::MAX).await.unwrap();
    let result2: OmniExtractionResult = serde_json::from_slice(&body2).unwrap();
    assert!(!result2.markdown_content.contains("NUL byte detected"));
}

#[tokio::test]
async fn test_extract_image_phash_and_metadata() {
    let app = setup_test_app();

    // 创建一幅 100x100 的纯色 PNG 图片用于测试
    let temp_img = std::env::temp_dir().join("test_image_extraction.png");
    let img = image::RgbImage::from_fn(100, 100, |x, _y| {
        if x > 50 { image::Rgb([255, 0, 0]) } else { image::Rgb([0, 255, 0]) }
    });
    img.save(&temp_img).unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/extract")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&serde_json::json!({
                    "file_path": temp_img.to_string_lossy().to_string()
                })).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    let _ = std::fs::remove_file(&temp_img);
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let result: OmniExtractionResult = serde_json::from_slice(&body).unwrap();
    
    assert_eq!(result.mime_type, "image/png");
    assert!(result.phash.is_some(), "Image pHash should be computed and non-null");
    assert!(result.metadata.get("image").is_some(), "Image metadata should contain width, height and resolution");
    assert_eq!(result.metadata["image"]["resolution"], "100x100");
}

#[tokio::test]
async fn test_extract_real_gif_ocr_text() {
    let gif_path = std::path::PathBuf::from(r"F:\lilun\Desktop\结构设计稿_gif.gif");
    if !gif_path.exists() {
        return;
    }

    let app = setup_test_app();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/extract")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&serde_json::json!({
                    "file_path": gif_path.to_string_lossy().to_string()
                })).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let result: OmniExtractionResult = serde_json::from_slice(&body).unwrap();

    println!("Recognized OCR Text for GIF:\n{}", result.markdown_content);
    assert!(!result.markdown_content.is_empty(), "OCR text for real GIF image should be recognized!");
}

#[tokio::test]
async fn test_extract_real_wechat_png_ocr_text() {
    let png_path = std::path::PathBuf::from(r"F:\lilun\Desktop\微信图片帐户删除.png");
    if !png_path.exists() {
        println!("File does not exist at {:?}", png_path);
        return;
    }

    let app = setup_test_app();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/extract")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&serde_json::json!({
                    "file_path": png_path.to_string_lossy().to_string()
                })).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let result: OmniExtractionResult = serde_json::from_slice(&body).unwrap();

    println!("Recognized OCR Text for WeChat PNG:\n{}", result.markdown_content);
    assert!(!result.markdown_content.is_empty(), "OCR text for WeChat PNG image should be recognized!");
}

#[tokio::test]
async fn test_ocr_model_size_switch_and_config_persistence() {
    let app = setup_test_app();

    let update_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/config")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&serde_json::json!({
                    "enable_document_ocr": true,
                    "enable_image_ocr": true,
                    "ocr_model_size": "small",
                    "max_document_ocr_file_size_mb": 10,
                    "max_content_size_kb": 30,
                    "max_file_size_mb": 100,
                    "analysis_mode": "full",
                    "reuse_basic_analysis_data": true
                })).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(update_resp.status(), StatusCode::OK);

    let get_resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/config")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(get_resp.status(), StatusCode::OK);
    let get_body = axum::body::to_bytes(get_resp.into_body(), usize::MAX).await.unwrap();
    let get_cfg: OmniConfig = serde_json::from_slice(&get_body).unwrap();
    assert_eq!(get_cfg.ocr_model_size, "small");
}

#[tokio::test]
async fn test_universal_exiftool_metadata_extraction() {
    let app = setup_test_app();

    let temp_txt = std::env::temp_dir().join("test_exiftool_meta.txt");
    std::fs::write(&temp_txt, "Hello ExifTool Test").unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/extract")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&serde_json::json!({
                    "file_path": temp_txt.to_string_lossy().to_string()
                })).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    let _ = std::fs::remove_file(&temp_txt);
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let result: OmniExtractionResult = serde_json::from_slice(&body).unwrap();

    assert!(result.metadata.get("exiftool").is_some(), "Universal ExifTool metadata should be extracted for all files!");
}

#[tokio::test]
async fn test_extract_tailwind_pdf_metadata() {
    let pdf_path = std::path::PathBuf::from(r"F:\lilun\Desktop\计算机科学_前端技术_TailwindCSS技术介绍.pdf");
    if !pdf_path.exists() {
        println!("Tailwind PDF does not exist at {:?}", pdf_path);
        return;
    }

    let app = setup_test_app();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/extract")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&serde_json::json!({
                    "file_path": pdf_path.to_string_lossy().to_string()
                })).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let result: OmniExtractionResult = serde_json::from_slice(&body).unwrap();

    println!("DEBUG METADATA FOR TAILWIND PDF:\n{:#?}", result.metadata);
    println!("MARKDOWN CONTENT LEN: {}", result.markdown_content.len());
    
    assert!(result.metadata.get("document").is_some());
    let doc_meta = &result.metadata["document"];
    assert!(doc_meta.get("creator").is_some() || doc_meta.get("author").is_some() || doc_meta.get("producer").is_some(), "Tailwind PDF should have metadata!");
}

#[tokio::test]
async fn test_upload_tailwind_pdf_multipart() {
    let pdf_path = std::path::PathBuf::from(r"F:\lilun\Desktop\计算机科学_前端技术_TailwindCSS技术介绍.pdf");
    if !pdf_path.exists() {
        return;
    }

    let pdf_bytes = std::fs::read(&pdf_path).unwrap();
    let file_name = "TailwindTest.pdf";

    // 构建 multipart body
    let boundary = "---------------------------1234567890";
    let mut body_bytes = Vec::new();
    body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body_bytes.extend_from_slice(format!("Content-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\n", file_name).as_bytes());
    body_bytes.extend_from_slice(b"Content-Type: application/pdf\r\n\r\n");
    body_bytes.extend_from_slice(&pdf_bytes);
    body_bytes.extend_from_slice(format!("\r\n--{}--\r\n", boundary).as_bytes());

    let app = setup_test_app();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/extract/upload")
                .header("content-type", format!("multipart/form-data; boundary={}", boundary))
                .body(Body::from(body_bytes))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let result: OmniExtractionResult = serde_json::from_slice(&body).unwrap();

    println!("DEBUG METADATA FOR TAILWIND PDF MULTIPART UPLOAD:\n{:#?}", result.metadata);
    assert!(result.metadata.get("exiftool").is_some(), "ExifTool metadata should be extracted on upload!");
}

#[tokio::test]
async fn test_extract_exe_file_metadata() {
    let exe_path = std::path::PathBuf::from(r"C:\Windows\explorer.exe");
    if !exe_path.exists() {
        println!("C:\\Windows\\explorer.exe does not exist");
        return;
    }

    let app = setup_test_app();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/extract")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&serde_json::json!({
                    "file_path": exe_path.to_string_lossy().to_string()
                })).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let result: OmniExtractionResult = serde_json::from_slice(&body).unwrap();

    println!("DEBUG METADATA FOR EXE FILE:\n{:#?}", result.metadata);
    assert!(result.metadata.get("exiftool").is_some(), "ExifTool metadata should be present for .exe file");
    assert!(result.metadata.get("executable").is_some(), "Executable metadata block should be present for .exe file");
}

#[tokio::test]
async fn test_native_czkawka_bridge_duplicate_scan() {
    let test_dir = std::path::PathBuf::from(r"D:\workspace\firefly-ai-folder\tests\media_duplicate_test");
    if !test_dir.exists() {
        return;
    }

    let app = setup_test_app();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/duplicate/scan")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&serde_json::json!({
                    "paths": [test_dir.to_string_lossy().to_string()],
                    "strategies": ["audio_hash", "video_phash"],
                    "min_similarity": 7.5,
                    "check_video": true
                })).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let result: omni_core::DuplicateScanResponse = serde_json::from_slice(&body).unwrap();

    println!("CZKAWKA BRIDGE DUPLICATE RESPONSE:\n{:#?}", result);
    assert!(result.success);
    assert!(!result.duplicate_groups.is_empty(), "Native czkawka_core should detect duplicate groups!");
}

#[tokio::test]
async fn test_native_czkawka_bridge_stream_scan() {
    let test_dir = std::path::PathBuf::from(r"D:\workspace\firefly-ai-folder\tests\media_duplicate_test");
    if !test_dir.exists() {
        return;
    }

    let app = setup_test_app();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/duplicate/scan/stream")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&serde_json::json!({
                    "paths": [test_dir.to_string_lossy().to_string()],
                    "strategies": ["exact_hash", "audio_hash", "video_phash"],
                    "check_video": true
                })).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body_str = String::from_utf8_lossy(&body);

    println!("CZKAWKA STREAM SSE OUTPUT:\n{}", body_str);
    assert!(body_str.contains("event: start"));
    assert!(body_str.contains("event: progress"));
    assert!(body_str.contains("event: group"));
    assert!(body_str.contains("event: done"));
}

#[tokio::test]
async fn test_czkawka_bridge_full_tools_on_work_folder_private() {
    let target_dir = resolve_work_folder_path("PRIVATE/czkawka_bridge_samples");
    if !target_dir.exists() {
        println!("Test directory does not exist at {:?}", target_dir);
        return;
    }

    let app = setup_test_app();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/duplicate/scan")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&serde_json::json!({
                    "paths": [target_dir.to_string_lossy().to_string()],
                    "strategies": [
                        "exact_hash",
                        "empty_folders",
                        "empty_files",
                        "temporary_files",
                        "bad_extensions",
                        "bad_names",
                        "broken_files",
                        "big_files"
                    ],
                    "min_similarity": 7.5
                })).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let result: omni_core::DuplicateScanResponse = serde_json::from_slice(&body).unwrap();

    println!("CZKAWKA WORK_FOLDER PRIVATE SCAN RESPONSE:\n{:#?}", result);
    assert!(result.success);
    assert!(!result.duplicate_groups.is_empty(), "All 14 bridge tools should detect artifacts in PRIVATE samples!");

    // 检查各新增策略是否成功探测并回传
    let strategies: Vec<String> = result.duplicate_groups.iter().map(|g| g.strategy.clone()).collect();
    println!("Detected Strategies in sample folder: {:?}", strategies);

    assert!(strategies.iter().any(|s| s == "exact_hash" || s == "duplicates"), "Exact duplicates should be detected");
    assert!(strategies.iter().any(|s| s == "empty_files"), "Empty files should be detected");
    assert!(strategies.iter().any(|s| s == "temporary_files"), "Temporary files should be detected");
    assert!(strategies.iter().any(|s| s == "bad_extensions"), "Bad extension file should be detected");
    assert!(strategies.iter().any(|s| s == "bad_names"), "Bad name file should be detected");
    assert!(strategies.iter().any(|s| s == "broken_files"), "Broken PDF file should be detected");
    assert!(strategies.iter().any(|s| s == "big_files"), "Big file should be detected");
}

// ==================== /api/geo/reverse 离线反向地理编码契约测试 ====================

#[tokio::test]
async fn test_health_reports_geo_unavailable_when_dataset_missing() {
    let app = setup_test_app();
    let response = app
        .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "ok");
    // 数据集未配置 → geoAvailable 必须为 false（软降级，不影响整体健康）
    assert_eq!(json["geoAvailable"], false);
}

#[tokio::test]
async fn test_health_reports_geo_available_with_inline_fixture() {
    let geo = Arc::new(omni_pro::geo::GeoService::from_json_str(GEO_FIXTURE_JSON).unwrap());
    let app = setup_test_app_with_geo(geo);
    let response = app
        .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();

    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["geoAvailable"], true);
}

#[tokio::test]
async fn test_geo_reverse_soft_fails_with_200_when_unavailable() {
    let app = setup_test_app();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/geo/reverse")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "points": [{ "latitude": 22.55, "longitude": 114.07 }]
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    // 契约：数据缺失不报 5xx，恒为 200 + available:false + reason
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["available"], false);
    assert!(json["reason"].as_str().is_some_and(|r| !r.is_empty()));
}

#[tokio::test]
async fn test_geo_reverse_happy_path_city_tier() {
    let geo = Arc::new(omni_pro::geo::GeoService::from_json_str(GEO_FIXTURE_JSON).unwrap());
    let app = setup_test_app_with_geo(geo);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/geo/reverse")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "points": [{ "latitude": 22.55, "longitude": 114.07 }],
                        "language": "zh-CN"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["available"], true);
    assert_eq!(json["datasetVersion"], 20260824);

    let r = &json["results"][0];
    assert_eq!(r["found"], true);
    assert_eq!(r["country"], "中国");
    assert_eq!(r["province"], "广东省");
    assert_eq!(r["city"], "深圳市");
    assert!(r["distanceKm"].is_number());
}

#[tokio::test]
async fn test_geo_reverse_batch_alignment_and_dirty_points() {
    let geo = Arc::new(omni_pro::geo::GeoService::from_json_str(GEO_FIXTURE_JSON).unwrap());
    let app = setup_test_app_with_geo(geo);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/geo/reverse")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "points": [
                            { "latitude": 91.0, "longitude": 0.0 },
                            { "latitude": -33.90, "longitude": 151.20 },
                            { "latitude": 23.00, "longitude": 114.06 }
                        ],
                        "language": "en"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    let results = json["results"].as_array().expect("results 数组");
    assert_eq!(results.len(), 3, "结果与请求按下标对齐");
    // 脏坐标单点失败
    assert_eq!(results[0]["found"], false);
    // 悉尼不在夹具数据集内（夹具仅中国两点）且距离超限 → 未命中
    assert_eq!(results[1]["found"], false);
    // 合法点命中河源（约15.6km ≤ 默认城市阈值）→ 全量层英文回传
    assert_eq!(results[2]["found"], true);
    assert_eq!(results[2]["city"], "Heyuan");
    assert_eq!(results[2]["country"], "China");
}

#[tokio::test]
async fn test_geo_reverse_request_threshold_overrides() {
    let geo = Arc::new(omni_pro::geo::GeoService::from_json_str(GEO_FIXTURE_JSON).unwrap());
    let app = setup_test_app_with_geo(geo);
    // 河源距查询点约15.6km：maxCityKm=10 后应降级为中间层（city 置空）
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/geo/reverse")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "points": [{ "latitude": 23.00, "longitude": 114.06 }],
                        "language": "en",
                        "maxCityKm": 10
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    let r = &json["results"][0];
    assert_eq!(r["found"], true, "15.6km 应命中中间层");
    assert_eq!(r["city"], serde_json::Value::Null);
    assert_eq!(r["province"], "Guangdong");
}



