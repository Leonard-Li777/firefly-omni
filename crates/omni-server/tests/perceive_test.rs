//! perceive_test.rs — Omni 感知与 ASR 契约接缝测试（Issue 0046 / PRD Seam 1）
//!
//! 验证音频 ASR 契约标准化与纯图片 OCR 隔离的**对外数据契约**：
//!
//! 1. `OmniPerceptionRequest.enable_asr` 反序列化，且历史别名 `enable_audio_transcript` 兼容；
//! 2. `OmniPerceptionResult` 序列化字段为 `asr` / `has_asr` / `asr_length`，**不再**出现
//!    `audio_transcript`（旧命名已废弃）；
//! 3. `MultimodalContext` 内部沿用 `audio_transcript`（供融合引擎消费），不对外泄漏为契约字段。
//!
//! 说明：本文件聚焦「契约」层面的确定性断言（无需加载真实 ASR / OCR 模型即可运行）。
//! 真实音视频端到端转录（`/api/perceive` 实测耗时、`has_asr: true`）需在具备模型的
//! 集成环境中执行，见 PRD Seam 1 的集成验证路径。

use omni_core::{MultimodalContext, OmniPerceptionRequest, OmniPerceptionResult};
use serde_json::{json, Value};

/// 契约 1：`enable_asr` 反序列化 + 历史别名 `enable_audio_transcript` 兼容。
#[test]
fn perception_request_accepts_enable_asr_and_legacy_alias() {
    // 新字段名
    let req: OmniPerceptionRequest = serde_json::from_value(json!({
        "file_path": "/tmp/a.mp4",
        "enable_asr": true
    }))
    .expect("解析 enable_asr 失败");
    assert_eq!(req.enable_asr, Some(true));

    // 历史别名（老客户端/老缓存）仍可解析到同一字段
    let legacy: OmniPerceptionRequest = serde_json::from_value(json!({
        "file_path": "/tmp/a.mp4",
        "enable_audio_transcript": true
    }))
    .expect("解析历史别名 enable_audio_transcript 失败");
    assert_eq!(legacy.enable_asr, Some(true));

    // 缺省时为 None（沿用全局配置）
    let defaulted: OmniPerceptionRequest = serde_json::from_value(json!({
        "file_path": "/tmp/a.mp4"
    }))
    .expect("解析缺省 enable_asr 失败");
    assert_eq!(defaulted.enable_asr, None);
}

/// 契约 2：感知结果对外仅暴露 `asr` / `has_asr` / `asr_length`，且可无损往返。
#[test]
fn perception_result_exposes_asr_contract_fields() {
    let transcript = "今天开会讨论了三季度预算与人员招聘计划";

    let result = OmniPerceptionResult {
        file_path: "/tmp/meeting.mp4".to_string(),
        mime_type: "video/mp4".to_string(),
        file_size: 1024,
        asr: Some(transcript.to_string()),
        has_asr: true,
        asr_length: transcript.chars().count(),
        ..Default::default()
    };

    let value: Value = serde_json::to_value(&result).expect("序列化感知结果失败");

    // 新契约字段存在
    assert_eq!(value["asr"], json!(transcript));
    assert_eq!(value["has_asr"], json!(true));
    assert_eq!(value["asr_length"], json!(transcript.chars().count()));

    // 旧命名 audio_transcript 必须彻底消失（废弃字段不得回潮）
    assert!(
        value.get("audio_transcript").is_none(),
        "感知结果不应再输出废弃字段 audio_transcript"
    );

    // 无损往返
    let back: OmniPerceptionResult = serde_json::from_value(value).expect("反序列化感知结果失败");
    assert_eq!(back.asr.as_deref(), Some(transcript));
    assert!(back.has_asr);
    assert_eq!(back.asr_length, transcript.chars().count());
}

/// 契约 2b：`asr_length` 按 Unicode 标量计数，而非字节数（中文/emoji 场景）。
#[test]
fn asr_length_counts_unicode_scalars() {
    let text = "会议🎉纪要"; // 5 个 Unicode 标量，字节数远大于 5
    let result = OmniPerceptionResult {
        asr: Some(text.to_string()),
        has_asr: true,
        asr_length: text.chars().count(),
        ..Default::default()
    };
    let value = serde_json::to_value(&result).unwrap();
    assert_eq!(value["asr_length"], json!(5));
}

/// 契约 3：纯图片路径下 OCR 文本与 ASR 相互隔离——OCR 文本落在 `ocr_text`，
/// 不写入 ASR 字段；`asr` 保持 None、`has_asr` 为 false。
#[test]
fn pure_image_keeps_ocr_isolated_from_asr() {
    let ocr = "发票代码 12345678 金额 ¥1280.00";

    let result = OmniPerceptionResult {
        file_path: "/tmp/invoice.png".to_string(),
        mime_type: "image/png".to_string(),
        // 纯图片路径：markdown_content 强制为空串（Issue 0046 §1）
        markdown_content: String::new(),
        ocr_text: Some(ocr.to_string()),
        asr: None,
        has_asr: false,
        asr_length: 0,
        ..Default::default()
    };

    let value = serde_json::to_value(&result).unwrap();
    assert_eq!(value["markdown_content"], json!(""));
    assert_eq!(value["ocr_text"], json!(ocr));
    assert_eq!(value["asr"], Value::Null);
    assert_eq!(value["has_asr"], json!(false));
    assert_eq!(value["asr_length"], json!(0));
}

/// 契约 3b：`MultimodalContext` 内部沿用 `audio_transcript` 承载语音事实，
/// 与对外 `asr` 契约解耦（融合引擎消费内部命名）。
#[test]
fn multimodal_context_carries_internal_audio_transcript() {
    let ctx = MultimodalContext {
        file_path: "/tmp/meeting.mp4".to_string(),
        file_name: "meeting.mp4".to_string(),
        mime_type: "video/mp4".to_string(),
        audio_transcript: Some("季度预算评审".to_string()),
        is_audio_or_video: true,
        ..Default::default()
    };

    let value = serde_json::to_value(&ctx).unwrap();
    assert_eq!(value["audio_transcript"], json!("季度预算评审"));
    assert_eq!(value["is_audio_or_video"], json!(true));

    let back: MultimodalContext = serde_json::from_value(value).unwrap();
    assert_eq!(back.audio_transcript.as_deref(), Some("季度预算评审"));
}
