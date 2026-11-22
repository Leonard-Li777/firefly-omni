//! tests/nsfw_assert_endpoint_test.rs — 验证原生 POST /api/text/nsfw 端点
//!
//! 覆盖：空参 400、非法 base64 400、纯文本安全/敏感鉴别及 supported_dimensions 声明。

use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use omni_core::OmniConfig;
use omni_server::{create_app_router, AppState};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

fn setup_test_app() -> Router {
    let state = AppState {
        config: Arc::new(Mutex::new({
            let mut c = OmniConfig::default();
            c.enable_nsfw_perception = true;
            c
        })),
        geo: Arc::new(omni_pro::geo::GeoService::unavailable()),
        hownet: Arc::new(omni_pro::hownet::OmniHowNetService::unavailable()),
        search: Arc::new(omni_pro::search::OmniSearchService::default()),
        omw: omni_server::OmwDb::unavailable(),
        vector: Arc::new(omni_server::VectorEngine::in_memory()),
        master_db_path: Arc::new(Mutex::new(None)),
        target_vectors_dir: None,
        dimension_policies: Arc::new(std::sync::RwLock::new(
            omni_core::get_default_dimension_policies(),
        )),
        sherpa: Arc::new(omni_server::SherpaManager::new()),
        moss: Arc::new(omni_server::MossTtsManager::new()),
        m5: Arc::new(omni_server::M5TtsManager::new()),
        anchor_cache: Arc::new(omni_core::anchor_cache::AnchorCache::default()),
        cpu_slot: Arc::new(omni_server::cpu_engine::CpuSlot::new(None, None)),
        instance_id: omni_server::service_discovery::generate_uuid_v4(),
        revision: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        actual_port: omni_server::OMNI_DEFAULT_PORT_START,
        user_data_dir: None,
        status_broadcast: tokio::sync::broadcast::channel(32).0,
        shutdown_tx: tokio::sync::watch::channel(false).0,
    };
    create_app_router(state)
}

#[tokio::test]
async fn test_nsfw_endpoint_rejects_empty_and_invalid_base64() {
    if !omni_pro::is_pro_enabled() {
        return;
    }
    let app = setup_test_app();

    // 1. 全空参数返回 400 INVALID_ARGUMENT
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/text/nsfw")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // 2. 非法 Base64 返回 400 INVALID_BASE64
    let res2 = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/text/nsfw")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"image_base64":"!!!not-valid-base64!!!"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res2.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_nsfw_endpoint_text_assertion_clean_and_dimensions() {
    if !omni_pro::is_pro_enabled() {
        return;
    }
    let app = setup_test_app();

    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/text/nsfw")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"text":"今天天气真好，我们一起去公园散步并整理工作文档。"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["success"], true);
    assert_eq!(json["status"], "clean");
    assert_eq!(json["is_sensitive"], false);
    let dims = json["supported_dimensions"].as_array().unwrap();
    assert!(dims.iter().any(|v| v == "暴恐"));
    assert!(dims.iter().any(|v| v == "涉政"));
    assert!(dims.iter().any(|v| v == "违规"));
    assert!(dims.iter().any(|v| v == "色情"));
    assert!(dims.iter().any(|v| v == "辱骂"));
    assert!(dims.iter().any(|v| v == "血腥"));
}
