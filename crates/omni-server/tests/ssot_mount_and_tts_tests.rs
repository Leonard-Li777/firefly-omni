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
    };
    create_app_router(state)
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
    assert_eq!(status_json["engine"], "m5-tts-50m-int8");
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
async fn test_mobileclip_en_vocab_missing_and_tampered_fail_closed_with_503() {
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

    // 生成合法 4x4 PNG 图片供 /api/vision/tags 测试
    let png_path = temp.path().join("sample.png");
    image::DynamicImage::new_rgb8(4, 4).save(&png_path).unwrap();

    let expected_sha = "a".repeat(64);
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
                "minPrimaryFileBytes": 1_000_000,
                "vocabMeta": {
                    "fileName": "tags_visual_en.bin",
                    "textFileName": "tags_visual_en.txt",
                    "vocabVersion": "2026-11-11.1",
                    "vocabSha256": expected_sha,
                    "vocabTextSha256": "",
                    "vocabEntryCount": 2,
                    "embedDim": 512
                }
            }
        }
    });
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let _guard = EnvRestoreGuard::set(&manifest_path, &shared);

    let app = setup_test_app();

    // 状态 1: 词表缺失 -> vocab_missing
    let clip_req = Request::builder()
        .method("POST")
        .uri("/api/v1/vector/embed-clip-text")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "text": "a golden retriever",
                "lang": "en"
            }))
            .unwrap(),
        ))
        .unwrap();
    let clip_resp = app.clone().oneshot(clip_req).await.unwrap();
    assert_eq!(clip_resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let clip_body: Value = serde_json::from_slice(
        &axum::body::to_bytes(clip_resp.into_body(), usize::MAX).await.unwrap(),
    )
    .unwrap();
    assert_eq!(clip_body["error"], "vocab_missing");
    assert_eq!(clip_body["encoderId"], 3);

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
    assert_eq!(tags_resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let tags_body: Value = serde_json::from_slice(
        &axum::body::to_bytes(tags_resp.into_body(), usize::MAX).await.unwrap(),
    )
    .unwrap();
    assert_eq!(tags_body["reason"], "vocab_missing");

    // 状态 2: 词表哈希被篡改 -> vocab_hash_mismatch
    fs::write(mobile_dir.join("tags_visual_en.bin"), vec![1u8; 2 * 512 * 4]).unwrap();
    fs::write(mobile_dir.join("tags_visual_en.txt"), "cat\ndog\n").unwrap();

    let clip_resp_tampered = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/vector/embed-clip-text")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "text": "a golden retriever",
                        "lang": "en"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(clip_resp_tampered.status(), StatusCode::SERVICE_UNAVAILABLE);
    let clip_body_tampered: Value = serde_json::from_slice(
        &axum::body::to_bytes(clip_resp_tampered.into_body(), usize::MAX).await.unwrap(),
    )
    .unwrap();
    assert_eq!(clip_body_tampered["error"], "vocab_hash_mismatch");

    let tags_resp_tampered = app
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
    assert_eq!(tags_resp_tampered.status(), StatusCode::SERVICE_UNAVAILABLE);
    let tags_body_tampered: Value = serde_json::from_slice(
        &axum::body::to_bytes(tags_resp_tampered.into_body(), usize::MAX).await.unwrap(),
    )
    .unwrap();
    assert_eq!(tags_body_tampered["reason"], "vocab_hash_mismatch");

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
    assert_eq!(status_json["mobileClipVocab"]["status"], "vocab_hash_mismatch");

    let health_resp = app
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
    assert_eq!(health_json["clipText"]["mobileClip"]["vocabReady"], false);
    assert_eq!(health_json["clipText"]["mobileClip"]["rejectReason"], "vocab_hash_mismatch");

    // 状态 3: 无效版本元数据格式 -> vocab_version_mismatch
    let bad_ver_manifest = serde_json::json!({
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
                "minPrimaryFileBytes": 1_000_000,
                "vocabMeta": {
                    "fileName": "tags_visual_en.bin",
                    "textFileName": "tags_visual_en.txt",
                    "vocabVersion": "invalid!",
                    "vocabSha256": expected_sha,
                    "vocabTextSha256": "",
                    "vocabEntryCount": 2,
                    "embedDim": 512
                }
            }
        }
    });
    fs::write(&manifest_path, serde_json::to_vec(&bad_ver_manifest).unwrap()).unwrap();
    omni_core::plugin_dir::invalidate_mount_cache();

    let app2 = setup_test_app();
    let ver_resp = app2
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/vector/embed-clip-text")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "text": "a golden retriever",
                        "lang": "en"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ver_resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let ver_body: Value = serde_json::from_slice(
        &axum::body::to_bytes(ver_resp.into_body(), usize::MAX).await.unwrap(),
    )
    .unwrap();
    assert_eq!(ver_body["error"], "vocab_version_mismatch");
}
