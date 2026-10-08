//! audio_pipeline_tests.rs — Sherpa-ONNX 全栈音频子系统集成与 ASR/降噪/打标闭环测试 (Ticket 3)
//!
//! 验证：
//! 1. WAV / 音视频多模态文件在 POST /api/perceive 时的端到端感知契约；
//! 2. 全静音音频与 <0.1s 超短音频输入测试，无 Panic 且返回安全空结果；
//! 3. 声学事件打标 (CED-mini) 归一化输出符合受控本体契约；
//! 4. POST /api/audio/transcribe 与 POST /api/audio/convert 接口的纯内存流转与健壮性。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use omni_core::concepts::Concept;
use omni_core::{AudioConvertResponse, AudioTranscribeResponse, OmniConfig, OmniPerceptionResult};
use omni_server::{create_app_router, normalize_ced_audio_event, AppState, SherpaManager};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

fn setup_test_app() -> Router {
    let state = AppState {
        config: Arc::new(Mutex::new(OmniConfig::default())),
        geo: Arc::new(omni_pro::geo::GeoService::unavailable()),
        hownet: Arc::new(omni_pro::hownet::OmniHowNetService::unavailable()),
        search: Arc::new(omni_pro::search::OmniSearchService::default()),
        omw: omni_server::OmwDb::unavailable(),
        vector: Arc::new(omni_server::VectorEngine::in_memory()),
        master_db_path: Arc::new(Mutex::new(None)),
        dimension_policies: Arc::new(std::sync::RwLock::new(
            omni_core::get_default_dimension_policies(),
        )),
        sherpa: Arc::new(SherpaManager::new()),
    };
    create_app_router(state)
}

/// 辅助：生成标准的 16kHz 单声道 16-bit PCM WAV 字节
fn create_test_wav_bytes(num_samples: usize, amplitude: i16) -> Vec<u8> {
    let mut bytes = Vec::new();
    let data_len = (num_samples * 2) as u32;
    let file_len = 36 + data_len;

    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&file_len.to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes()); // subchunk size
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
    bytes.extend_from_slice(&16000u32.to_le_bytes()); // sample rate
    bytes.extend_from_slice(&32000u32.to_le_bytes()); // byte rate
    bytes.extend_from_slice(&2u16.to_le_bytes()); // block align
    bytes.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());

    for _ in 0..num_samples {
        bytes.extend_from_slice(&amplitude.to_le_bytes());
    }

    bytes
}

async fn perceive_audio_file(app: &Router, path: &std::path::Path) -> OmniPerceptionResult {
    let req_body = serde_json::json!({
        "file_path": path.to_str().unwrap(),
        "enable_asr": true
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
async fn test_perceive_silent_audio_safe_empty() {
    let app = setup_test_app();
    let temp_dir = tempfile::tempdir().unwrap();
    let wav_path = temp_dir.path().join("silent_1_5s.wav");

    // 1.5 秒全静音 WAV (24000 样本，振幅全为 0)
    let wav_bytes = create_test_wav_bytes(24000, 0);
    std::fs::write(&wav_path, wav_bytes).unwrap();

    let result = perceive_audio_file(&app, &wav_path).await;

    // 验证防 Panic 守卫：静音音频安全返回空转录，绝不 panic
    assert_eq!(result.has_asr, false);
    assert_eq!(result.asr, None);
    assert_eq!(result.asr_length, 0);
}

#[tokio::test]
async fn test_perceive_short_audio_safe_empty() {
    let app = setup_test_app();
    let temp_dir = tempfile::tempdir().unwrap();
    let wav_path = temp_dir.path().join("short_0_05s.wav");

    // 0.05 秒超短音频 (< 0.1s，800 样本)
    let wav_bytes = create_test_wav_bytes(800, 1500);
    std::fs::write(&wav_path, wav_bytes).unwrap();

    let result = perceive_audio_file(&app, &wav_path).await;

    // 验证防 Panic 守卫：<0.1s 音频切片直接安全返回空结果，避免空张量崩溃
    assert_eq!(result.has_asr, false);
    assert_eq!(result.asr, None);
    assert_eq!(result.asr_length, 0);
}

#[tokio::test]
async fn test_audio_transcribe_endpoint_silent_and_short() {
    let app = setup_test_app();
    let temp_dir = tempfile::tempdir().unwrap();

    // 1. 静音音频转录请求
    let silent_path = temp_dir.path().join("transcribe_silent.wav");
    std::fs::write(&silent_path, create_test_wav_bytes(16000, 0)).unwrap();

    let req_silent = serde_json::json!({
        "file_path": silent_path.to_str().unwrap(),
        "duration_seconds": 2
    });

    let resp_silent = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/audio/transcribe")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&req_silent).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp_silent.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp_silent.into_body(), usize::MAX).await.unwrap();
    let res: AudioTranscribeResponse = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(res.transcript, None);

    // 2. 超短音频转录请求 (<0.1s)
    let short_path = temp_dir.path().join("transcribe_short.wav");
    std::fs::write(&short_path, create_test_wav_bytes(600, 2000)).unwrap();

    let req_short = serde_json::json!({
        "file_path": short_path.to_str().unwrap()
    });

    let resp_short = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/audio/transcribe")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&req_short).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp_short.status(), StatusCode::OK);
    let bytes_short = axum::body::to_bytes(resp_short.into_body(), usize::MAX).await.unwrap();
    let res_short: AudioTranscribeResponse = serde_json::from_slice(&bytes_short).unwrap();
    assert_eq!(res_short.transcript, None);
}

#[tokio::test]
async fn test_audio_convert_endpoint_pure_memory() {
    let app = setup_test_app();
    let temp_dir = tempfile::tempdir().unwrap();
    let wav_path = temp_dir.path().join("convert_input.wav");

    // 1 秒 16000 样本的音频
    std::fs::write(&wav_path, create_test_wav_bytes(16000, 500)).unwrap();

    let req = serde_json::json!({
        "file_path": wav_path.to_str().unwrap(),
        "duration_seconds": 1
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/audio/convert")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&req).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let res: AudioConvertResponse = serde_json::from_slice(&bytes).unwrap();

    assert!(!res.output_path.is_empty());
    let out_p = std::path::Path::new(&res.output_path);
    assert!(out_p.exists());
    assert!(out_p.metadata().unwrap().len() > 0);
}

#[test]
fn test_ced_ontology_normalization_in_pipeline() {
    // 严格遵照受控本体三阶段生命周期：
    // 1. 音乐范畴 -> Concept::音乐.code()
    assert_eq!(
        normalize_ced_audio_event("Heavy Metal Music"),
        Concept::音乐.code()
    );
    assert_eq!(
        normalize_ced_audio_event("Acoustic Guitar Strumming"),
        Concept::音乐.code()
    );

    // 2. 语音范畴 -> Concept::语音备忘.code()
    assert_eq!(
        normalize_ced_audio_event("Human Speech"),
        Concept::语音备忘.code()
    );
    assert_eq!(
        normalize_ced_audio_event("Male Whispering"),
        Concept::语音备忘.code()
    );

    // 3. 音效范畴 -> Concept::音效.code()
    assert_eq!(
        normalize_ced_audio_event("Sound Effect"),
        Concept::音效.code()
    );

    // 4. 未收录事件必须且只能带有 custom.audio.* 前缀
    let env_tag = normalize_ced_audio_event("Door slamming");
    assert!(env_tag.starts_with("custom.audio."));
    assert_eq!(env_tag, "custom.audio.door_slamming");
}
