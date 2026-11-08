//! perception_slot_tests.rs — 原生感知插槽 (perception_nsfw & perception_face) 集成测试 (Ticket 03)

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use omni_core::{OmniConfig, OmniPerceptionResult};
use omni_server::{create_app_router, AppState};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

fn setup_test_app(config: OmniConfig) -> Router {
    let state = AppState {
        config: Arc::new(Mutex::new(config)),
        geo: Arc::new(omni_pro::geo::GeoService::unavailable()),
        hownet: Arc::new(omni_pro::hownet::OmniHowNetService::unavailable()),
        search: Arc::new(omni_pro::search::OmniSearchService::default()),
        omw: omni_server::OmwDb::unavailable(),
        vector: Arc::new(omni_server::VectorEngine::in_memory()),
        master_db_path: Arc::new(Mutex::new(None)),
        target_vectors_dir: None,
        dimension_policies: Arc::new(std::sync::RwLock::new(omni_core::get_default_dimension_policies())),
        sherpa: Arc::new(omni_server::SherpaManager::new()),
        moss: Arc::new(omni_server::MossTtsManager::new()),
        anchor_cache: Arc::new(omni_core::anchor_cache::AnchorCache::default()),
    };
    create_app_router(state)
}

async fn perceive_file(app: &Router, path: &std::path::Path) -> OmniPerceptionResult {
    let req_body = serde_json::json!({
        "file_path": path.to_str().unwrap(),
        "language": "zh"
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/perceive")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&req_body).unwrap()))
                .unwrap(),
        )
        .await
        .expect("perceive 请求发送失败");

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("读取响应字节流失败");
    serde_json::from_slice(&bytes).expect("反序列化 OmniPerceptionResult 失败")
}

#[tokio::test]
async fn test_perception_slot_default_skipped_when_disabled() {
    let mut config = OmniConfig::default();
    config.enable_nsfw_perception = false;
    config.enable_face_perception = false;

    let app = setup_test_app(config);

    let temp = tempfile::Builder::new().suffix(".txt").tempfile().unwrap();
    std::fs::write(temp.path(), "这是一个普通的测试文档，内容安全健康。").unwrap();

    let res = perceive_file(&app, temp.path()).await;

    // 插件未安装或禁用时，必须显式返回 skipped
    let nsfw = res.nsfw_perception.expect("nsfw_perception 必须存在");
    assert_eq!(nsfw.status, "skipped");
    assert!(nsfw.tags.is_empty());

    let face = res.face_perception.expect("face_perception 必须存在");
    assert_eq!(face.status, "skipped");
    assert!(face.tags.is_empty());
}

#[tokio::test]
async fn test_perception_slot_clean_when_enabled_and_safe() {
    let mut config = OmniConfig::default();
    config.enable_nsfw_perception = true;
    config.enable_face_perception = true;

    let app = setup_test_app(config);

    let temp = tempfile::Builder::new().suffix(".txt").tempfile().unwrap();
    std::fs::write(temp.path(), "这是一份关于公司第三季度业务回顾的健康报告。").unwrap();

    let res = perceive_file(&app, temp.path()).await;

    // 插件激活且内容健康时，返回 clean
    let nsfw = res.nsfw_perception.expect("nsfw_perception 必须存在");
    assert_eq!(nsfw.status, "clean");
    assert!(nsfw.tags.is_empty());

    let face = res.face_perception.expect("face_perception 必须存在");
    assert_eq!(face.status, "clean");
    assert!(face.tags.is_empty());
}

#[tokio::test]
async fn test_perception_slot_flagged_when_sensitive_detected() {
    let mut config = OmniConfig::default();
    config.enable_nsfw_perception = true;

    let app = setup_test_app(config);

    let temp = tempfile::Builder::new().suffix(".txt").tempfile().unwrap();
    std::fs::write(
        temp.path(),
        "【色情黄色肉文】女主角脱光衣服赤身裸体正在做爱激情呻吟露出乳房和下体插穴高潮。",
    )
    .unwrap();

    let res = perceive_file(&app, temp.path()).await;

    let nsfw = res.nsfw_perception.expect("nsfw_perception 必须存在");
    assert_eq!(nsfw.status, "flagged");
    assert!(!nsfw.tags.is_empty());
}
