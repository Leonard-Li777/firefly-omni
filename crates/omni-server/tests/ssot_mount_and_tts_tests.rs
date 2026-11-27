//! ssot_mount_and_tts_tests.rs — SSOT 单一挂载清单、M5-TTS 统一门面与 MobileCLIP 三态失败 503 契约测试

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use omni_core::OmniConfig;
use omni_server::{create_app_router, AppState, M5TtsManager, MossTtsManager, OmwDb, SherpaManager, VectorEngine};
use serde_json::Value;
use std::fs::{self, File};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

static MANIFEST_ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvRestoreGuard {
    prev_manifest: Option<String>,
    prev_strict: Option<String>,
    prev_shared: Option<String>,
}

impl EnvRestoreGuard {
    fn set(manifest: &Path, shared: &Path) -> Self {
        let prev_manifest = std::env::var("FIREFLY_ACTIVE_MODEL_MOUNTS_FILE").ok();
        let prev_strict = std::env::var("FIREFLY_STRICT_ACTIVE_PLUGINS").ok();
        let prev_shared = std::env::var("FIREFLY_SHARED_DIR").ok();
        unsafe {
            std::env::set_var("FIREFLY_ACTIVE_MODEL_MOUNTS_FILE", manifest);
            std::env::set_var("FIREFLY_STRICT_ACTIVE_PLUGINS", "1");
            std::env::set_var("FIREFLY_SHARED_DIR", shared);
        }
        omni_core::plugin_dir::invalidate_mount_cache();
        Self {
            prev_manifest,
            prev_strict,
            prev_shared,
        }
    }
}

impl Drop for EnvRestoreGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.prev_manifest {
                Some(v) => std::env::set_var("FIREFLY_ACTIVE_MODEL_MOUNTS_FILE", v),
                None => std::env::remove_var("FIREFLY_ACTIVE_MODEL_MOUNTS_FILE"),
            }
            match &self.prev_strict {
                Some(v) => std::env::set_var("FIREFLY_STRICT_ACTIVE_PLUGINS", v),
                None => std::env::remove_var("FIREFLY_STRICT_ACTIVE_PLUGINS"),
            }
            match &self.prev_shared {
                Some(v) => std::env::set_var("FIREFLY_SHARED_DIR", v),
                None => std::env::remove_var("FIREFLY_SHARED_DIR"),
            }
        }
        omni_core::plugin_dir::invalidate_mount_cache();
    }
}

fn setup_test_app() -> axum::Router {
    let state = AppState {
        config: Arc::new(Mutex::new(OmniConfig::default())),
        geo: Arc::new(omni_pro::geo::GeoService::unavailable()),
        hownet: Arc::new(omni_pro::hownet::OmniHowNetService::unavailable()),
        search: Arc::new(omni_pro::search::OmniSearchService::default()),
        omw: OmwDb::unavailable(),
        vector: Arc::new(VectorEngine::in_memory()),
        master_db_path: Arc::new(Mutex::new(None)),
        target_vectors_dir: None,
        dimension_policies: Arc::new(std::sync::RwLock::new(
            omni_core::get_default_dimension_policies(),
        )),
        sherpa: Arc::new(SherpaManager::new()),
        moss: Arc::new(MossTtsManager::new()),
        m5: Arc::new(M5TtsManager::new()),
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

fn setup_test_app_with_state() -> (axum::Router, AppState) {
    let state = AppState {
        config: Arc::new(Mutex::new(OmniConfig::default())),
        geo: Arc::new(omni_pro::geo::GeoService::unavailable()),
        hownet: Arc::new(omni_pro::hownet::OmniHowNetService::unavailable()),
        search: Arc::new(omni_pro::search::OmniSearchService::default()),
        omw: OmwDb::unavailable(),
        vector: Arc::new(VectorEngine::in_memory()),
        master_db_path: Arc::new(Mutex::new(None)),
        target_vectors_dir: None,
        dimension_policies: Arc::new(std::sync::RwLock::new(
            omni_core::get_default_dimension_policies(),
        )),
        sherpa: Arc::new(SherpaManager::new()),
        moss: Arc::new(MossTtsManager::new()),
        m5: Arc::new(M5TtsManager::new()),
        anchor_cache: Arc::new(omni_core::anchor_cache::AnchorCache::default()),
        cpu_slot: Arc::new(omni_server::cpu_engine::CpuSlot::new(None, None)),
        instance_id: omni_server::service_discovery::generate_uuid_v4(),
        revision: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        actual_port: omni_server::OMNI_DEFAULT_PORT_START,
        user_data_dir: None,
        status_broadcast: tokio::sync::broadcast::channel(32).0,
        shutdown_tx: tokio::sync::watch::channel(false).0,
    };
    let router = create_app_router(state.clone());
    (router, state)
}

fn write_sized_file(path: &Path, len: u64) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let f = File::create(path).unwrap();
    f.set_len(len).unwrap();
}

#[tokio::test]
async fn test_m5_tts_unified_facade_status_501_and_unready_503() {
    if !omni_pro::is_pro_enabled() {
        return;
    }
    let _lock = MANIFEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let shared = temp.path().join("_shared");
    let m5_dir = shared.join("m5-tts");
    let sub = m5_dir.join("models/audio/m5");
    write_sized_file(&sub.join("m5_backbone_step_int8.onnx"), 1_050_000);
    write_sized_file(&sub.join("c0_head_fp32.onnx"), 512);
    write_sized_file(&sub.join("tokenizer.json"), 128);

    let manifest_path = temp.path().join("active-mounts.json");
    let manifest = serde_json::json!({
        "version": 1,
        "sharedDir": shared.to_string_lossy(),
        "strict": true,
        "mounts": {
            "m5-tts": {
                "resourceId": "m5-tts",
                "moduleId": "tts_m5_onnx",
                "capabilitySlot": "voice_tts",
                "dir": m5_dir.to_string_lossy(),
                "requiredFileGroups": [[
                    "models/audio/m5/m5_backbone_step_int8.onnx",
                    "models/audio/m5/c0_head_fp32.onnx",
                    "models/audio/m5/tokenizer.json"
                ]],
                "minPrimaryFileBytes": 1_000_000
            }
        }
    });
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let _guard = EnvRestoreGuard::set(&manifest_path, &shared);

    let app = setup_test_app();

    // 1. /api/audio/tts/status 暴露 M5 身份与 24kHz 采样率
    let status_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/audio/tts/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(status_resp.status(), StatusCode::OK);
    let status_json: Value = serde_json::from_slice(
        &axum::body::to_bytes(status_resp.into_body(), usize::MAX).await.unwrap(),
    )
    .unwrap();
    assert_eq!(status_json["engine"], "express-voice-24k");
    assert_eq!(status_json["sample_rate"], 24000);
    assert_eq!(status_json["ready"], false);

    // 2. M5 下声音克隆注册显式返回 HTTP 501 NOT_IMPLEMENTED
    let reg_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/audio/tts/register-voice")
                .header("content-type", "application/json")
                .body(
                    Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "voice_id": "custom_1",
                            "frames": 10,
                            "codes": []
                        }))
                        .unwrap(),
                    ),
                )
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reg_resp.status(), StatusCode::NOT_IMPLEMENTED);

    // 3. M5 下声音克隆分析显式返回 HTTP 501 NOT_IMPLEMENTED
    let analyze_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/audio/analyze-voice")
                .header("content-type", "application/json")
                .body(
                    Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "file_path": "dummy.wav"
                        }))
                        .unwrap(),
                    ),
                )
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(analyze_resp.status(), StatusCode::NOT_IMPLEMENTED);

    // 4. M5 未就绪时 /api/audio/tts/chunk 显式返回 HTTP 503
    let chunk_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/audio/tts/chunk")
                .header("content-type", "application/json")
                .body(
                    Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "text": "你好世界"
                        }))
                        .unwrap(),
                    ),
                )
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(chunk_resp.status(), StatusCode::SERVICE_UNAVAILABLE);

    // 4.1 M5 未就绪时 /api/audio/tts/stream 显式返回 HTTP 503
    let stream_unready_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/audio/tts/stream")
                .header("content-type", "application/json")
                .body(
                    Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "text": "你好世界"
                        }))
                        .unwrap(),
                    ),
                )
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stream_unready_resp.status(), StatusCode::SERVICE_UNAVAILABLE);

    // 4.2 空文本时 /api/audio/tts/stream 显式返回 HTTP 400
    let stream_bad_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/audio/tts/stream")
                .header("content-type", "application/json")
                .body(
                    Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "text": ""
                        }))
                        .unwrap(),
                    ),
                )
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stream_bad_resp.status(), StatusCode::BAD_REQUEST);

    // 5. /api/v1/engine/status 双向回显挂载清单遥测
    let engine_resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/engine/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(engine_resp.status(), StatusCode::OK);
    let engine_json: Value = serde_json::from_slice(
        &axum::body::to_bytes(engine_resp.into_body(), usize::MAX).await.unwrap(),
    )
    .unwrap();
    assert_eq!(engine_json["mountManifest"]["mode"], "manifest_single_track");
    assert_eq!(engine_json["mountManifest"]["activeResourceIds"][0], "m5-tts");
}

#[tokio::test]
async fn test_visual_tagger_vocab_stale_409_missing_500_corrupt_422_and_assets_reload() {
    if !omni_pro::is_pro_enabled() {
        return;
    }
    let _lock = MANIFEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let shared = temp.path().join("_shared");
    let mobile_dir = shared.join("mobile-clip");
    write_sized_file(&mobile_dir.join("mobileclip-b-int8-vision.onnx"), 1_050_000);
    write_sized_file(&mobile_dir.join("mobileclip-b-int8-text.onnx"), 512);
    fs::write(mobile_dir.join("mobileclip-tokenizer.json"), vec![b'a'; 256]).unwrap();

    // 生成合法 4x4 PNG 图片与损坏假图供 /api/vision/tags 测试
    let png_path = temp.path().join("sample.png");
    image::DynamicImage::new_rgb8(4, 4).save(&png_path).unwrap();
    let bad_img_path = temp.path().join("corrupt.png");
    fs::write(&bad_img_path, b"not-a-valid-png-payload").unwrap();

    let manifest_path = temp.path().join("active-mounts.json");
    let manifest = serde_json::json!({
        "version": 1,
        "sharedDir": shared.to_string_lossy(),
        "strict": true,
        "mounts": {
            "mobile-clip": {
                "resourceId": "mobile-clip",
                "moduleId": "engine_classic_light",
                "capabilitySlot": "base_embedding",
                "dir": mobile_dir.to_string_lossy(),
                "requiredFileGroups": [[
                    "mobileclip-b-int8-vision.onnx",
                    "mobileclip-b-int8-text.onnx",
                    "mobileclip-tokenizer.json"
                ]],
                "minPrimaryFileBytes": 1_000_000
            }
        }
    });
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let _guard = EnvRestoreGuard::set(&manifest_path, &shared);

    let app = setup_test_app();

    // 1. 坏图输入 -> 422 Unprocessable Entity (CORRUPT_INPUT)
    let bad_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/vision/tags")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "filePath": bad_img_path.to_string_lossy(),
                        "language": "en",
                        "topK": 5
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bad_resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let bad_body: Value = serde_json::from_slice(
        &axum::body::to_bytes(bad_resp.into_body(), usize::MAX).await.unwrap(),
    )
    .unwrap();
    assert_eq!(bad_body["reason"], "CORRUPT_INPUT");
    assert_eq!(bad_body["error"]["code"], "INPUT_REJECTED");

    // 2. Stage 0（旧目录无 vocab_v.meta.json 且未声明 visualVocabMeta）-> 409 Conflict (ENGINE_NOT_READY / VISUAL_VOCAB_STALE)，绝不返回 503
    let tags_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/vision/tags")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "filePath": png_path.to_string_lossy(),
                        "language": "en",
                        "topK": 5
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(tags_resp.status(), StatusCode::CONFLICT);
    let tags_body: Value = serde_json::from_slice(
        &axum::body::to_bytes(tags_resp.into_body(), usize::MAX).await.unwrap(),
    )
    .unwrap();
    assert_eq!(tags_body["reason"], "VISUAL_VOCAB_STALE");
    assert_eq!(tags_body["error"]["code"], "ENGINE_NOT_READY");

    // 3. 验证 /api/v1/engine/status 与 /health.assets.tagger 暴露契约（含 visualVocab、taggerSource、byEncoder）
    let status_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/engine/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(status_resp.status(), StatusCode::OK);
    let status_json: Value = serde_json::from_slice(
        &axum::body::to_bytes(status_resp.into_body(), usize::MAX).await.unwrap(),
    )
    .unwrap();
    let status_obj = status_json.as_object().unwrap();
    assert!(!status_obj.contains_key("mobileClipVocab"), "已下线的 mobileClipVocab 不得残留在 /engine/status");
    assert!(
        !status_json["vision_engines_available"].as_object().unwrap().contains_key("ram"),
        "已退役的 ram 不得残留在 vision_engines_available"
    );
    assert!(status_json["visualTagger"].is_object());
    assert!(status_json["visualVocab"].is_object());
    assert_eq!(status_json["visualVocab"]["activeEncoderId"], 3);
    assert_eq!(status_json["visualVocab"]["activeStatus"], "stale");
    assert_eq!(status_json["visualVocab"]["byEncoder"]["3"]["status"], "stale");
    assert_eq!(status_json["taggerSource"], "unavailable");

    let health_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/health?lang=en")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(health_resp.status(), StatusCode::OK);
    let health_json: Value = serde_json::from_slice(
        &axum::body::to_bytes(health_resp.into_body(), usize::MAX).await.unwrap(),
    )
    .unwrap();
    assert!(health_json["assets"]["tagger"]["chineseClip"].is_object());
    assert!(health_json["assets"]["tagger"]["mobileClip"].is_object());
    assert!(health_json["assets"]["tagger"]["gemma"].is_object());
    assert!(health_json["assets"]["tagger"]["byEncoder"].is_object());
    assert!(health_json["assets"]["tagger"]["status"].is_string());
    assert_eq!(health_json["clipText"]["mobileClip"]["vocabReady"], true, "文本词表探活应仅认 tokenizer，不再被 deleted tags_visual_en.bin 阻塞");

    // 4. POST /api/v1/taxonomy/reload-policies 独立返回 200，POST /api/v1/assets/reload 未挂载 semantic.pack 时 Fail-Closed 返回 500
    let policy_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/taxonomy/reload-policies")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(policy_resp.status(), StatusCode::OK);

    let reload_resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/assets/reload")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reload_resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn test_m5_streaming_active_causes_scheme_a_409_conflict() {
    if !omni_pro::is_pro_enabled() {
        return;
    }
    let _lock = MANIFEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let shared = temp.path().join("_shared");
    let m5_dir = shared.join("m5-tts");
    let sub = m5_dir.join("models/audio/m5");
    write_sized_file(&sub.join("m5_backbone_step_int8.onnx"), 1_050_000);
    write_sized_file(&sub.join("c0_head_fp32.onnx"), 512);
    write_sized_file(&sub.join("tokenizer.json"), 128);

    let manifest_path = temp.path().join("active-mounts.json");
    let manifest = serde_json::json!({
        "version": 1,
        "sharedDir": shared.to_string_lossy(),
        "strict": true,
        "mounts": {
            "m5-tts": {
                "resourceId": "m5-tts",
                "moduleId": "tts_m5_onnx",
                "capabilitySlot": "voice_tts",
                "dir": m5_dir.to_string_lossy(),
                "requiredFileGroups": [[
                    "models/audio/m5/m5_backbone_step_int8.onnx",
                    "models/audio/m5/c0_head_fp32.onnx",
                    "models/audio/m5/tokenizer.json"
                ]],
                "minPrimaryFileBytes": 1_000_000
            }
        }
    });
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let _env_guard = EnvRestoreGuard::set(&manifest_path, &shared);

    let (app, state) = setup_test_app_with_state();

    // 1. 验证状态接口暴露 M5 身份
    let status_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/audio/tts/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(status_resp.status(), StatusCode::OK);

    // 2. 初始未激活流式会话时，chunk 请求由于模型未就绪返回 503
    let chunk_init_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/audio/tts/chunk")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({ "text": "测试分块" })).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(chunk_init_resp.status(), StatusCode::SERVICE_UNAVAILABLE);

    // 3. 激活流式会话
    let _handle = state
        .m5
        .coordinator()
        .acquire_or_preempt("stream_session_active".to_string())
        .await;
    assert!(state.m5.coordinator().is_active().await);

    // 4. 核心断言: 此时发起 Scheme A 分块请求，必须被强行拦截并返回 HTTP 409 Conflict 与规范 JSON
    let chunk_409_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/audio/tts/chunk")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({ "text": "测试分块" })).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(chunk_409_resp.status(), StatusCode::CONFLICT);
    let body_bytes = axum::body::to_bytes(chunk_409_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_json: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(body_json["code"], "ENGINE_BUSY_STREAMING");

    // 5. 清理流式会话后，409 解除并恢复常规逻辑 (503)
    state
        .m5
        .coordinator()
        .clear_if_current("stream_session_active")
        .await;
    assert!(!state.m5.coordinator().is_active().await);

    let chunk_restored_resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/audio/tts/chunk")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({ "text": "测试分块" })).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        chunk_restored_resp.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}
