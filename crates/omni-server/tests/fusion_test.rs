//! fusion_test.rs — ASR 标签融合接缝测试（Issue 0046 / PRD Seam 2）
//!
//! 验证 ASR 转录文本作为一等公民参与「事实推导文本」解析，从而可驱动
//! 受控标签融合（`fused_tags`）：
//!
//! 优先级契约：复合文档正文（markdown_content）> 纯图片 OCR 文本 > 音视频 ASR 转录。
//!
//! 说明：真实的 `fused_tags` 派生依赖语义融合引擎与 ASR/CLIP 模型，属集成验证范畴；
//! 本文件对「ASR → 有效文本 → 融合输入」这一确定性接缝做单元级断言，
//! 保证 ASR 文本不会因 OCR 隔离（不写 markdown_content）而在融合链路中丢失。

use omni_core::resolve_effective_text;

/// 纯音频场景：无文档正文、无 OCR → ASR 转录成为有效文本（驱动标签融合）。
#[test]
fn asr_becomes_effective_text_for_pure_audio() {
    let asr = "今天开会讨论了三季度预算与人员招聘计划";
    let text = resolve_effective_text("", None, Some(asr));
    assert_eq!(text, asr);
}

/// 纯图片场景：OCR 优先于 ASR，且图片不携带 ASR 时仅取 OCR。
#[test]
fn ocr_takes_precedence_over_asr_for_image() {
    let ocr = "发票代码 12345678";
    let text = resolve_effective_text("", Some(ocr), Some("无关的语音文本"));
    assert_eq!(text, ocr);
}

/// 复合文档场景：正文存在时，OCR 与 ASR 均不覆盖正文（正文结构最纯粹）。
#[test]
fn document_body_takes_precedence_over_ocr_and_asr() {
    let body = "# 合同\n甲方乙方约定如下条款……";
    let text = resolve_effective_text(body, Some("图片里的 OCR"), Some("语音里的 ASR"));
    assert_eq!(text, body);
}

/// 空白正文/空白 OCR 视为缺失，回退到 ASR（避免"空字符串"误占优先级）。
#[test]
fn blank_fields_fall_through_to_asr() {
    let asr = "语音事实";
    assert_eq!(resolve_effective_text("   ", Some("  "), Some(asr)), asr);
    assert_eq!(resolve_effective_text("", Some(""), Some(asr)), asr);
}

/// 三者皆空 → 空字符串（不 panic，不返回占位内容）。
///
/// 注：正文与 OCR 会做 trim 判定；ASR 沿用原语义原样透传（生产链路中 `asr` 已在
/// 提取阶段做过空白过滤，故不会传入纯空白串）。
#[test]
fn all_empty_yields_empty_string() {
    assert_eq!(resolve_effective_text("", None, None), "");
    assert_eq!(resolve_effective_text("  ", Some("  "), None), "");
    // 正文/OCR 空白被跳过，回退到原样透传的 ASR
    assert_eq!(resolve_effective_text("  ", Some("  "), Some("")), "");
}

/// 音视频同时带 OCR（视频封面文字）与 ASR：OCR 优先于 ASR（与感知主流程一致）。
#[test]
fn video_with_cover_ocr_prefers_ocr_then_asr() {
    assert_eq!(
        resolve_effective_text("", Some("封面标题文字"), Some("旁白语音")),
        "封面标题文字"
    );
    assert_eq!(resolve_effective_text("", None, Some("旁白语音")), "旁白语音");
}
