use omni_core::OmniPerceptionBenchmark;
use serde_json::{json, Value};

#[test]
fn test_benchmark_flattened_serialization_and_roundtrip() {
    let mut bm = OmniPerceptionBenchmark {
        total_ms: 120,
        vision_ms: Some(85),
        extract_ms: Some(90),
        ..Default::default()
    };

    // 记录各种已有及未来新增细分子任务
    bm.record("clip_ms", 35);
    bm.record("clip_embed_ms", 15);
    bm.record("clip_mutual_ms", 5);
    bm.record("nsfw_ms", 18);
    bm.record("custom_future_detector_ms", 42);

    let val: Value = serde_json::to_value(&bm).expect("序列化 OmniPerceptionBenchmark 失败");

    // 验证宏观字段与细分子任务全部在顶层平铺 (Flattened)
    assert_eq!(val["total_ms"], json!(120));
    assert_eq!(val["vision_ms"], json!(85));
    assert_eq!(val["extract_ms"], json!(90));
    assert_eq!(val["clip_ms"], json!(35));
    assert_eq!(val["clip_embed_ms"], json!(15));
    assert_eq!(val["clip_mutual_ms"], json!(5));
    assert_eq!(val["nsfw_ms"], json!(18));
    assert_eq!(val["custom_future_detector_ms"], json!(42));

    // 严禁出现未展开的嵌套 "subtasks" 字段
    assert!(val.get("subtasks").is_none());

    // 反序列化无损往返
    let back: OmniPerceptionBenchmark = serde_json::from_value(val).expect("反序列化 OmniPerceptionBenchmark 失败");
    assert_eq!(back.total_ms, 120);
    assert_eq!(back.vision_ms, Some(85));
    assert_eq!(back.get("clip_ms"), Some(35));
    assert_eq!(back.get("custom_future_detector_ms"), Some(42));
}

#[test]
fn test_benchmark_accepts_legacy_json() {
    let legacy_json = json!({
        "total_ms": 200,
        "vision_ms": 150,
        "clip_ms": 50,
        "nsfw_ms": 20,
        "watermark_ms": 10,
        "tag_ms": 50
    });

    let bm: OmniPerceptionBenchmark = serde_json::from_value(legacy_json).expect("解析历史基准 JSON 失败");
    assert_eq!(bm.total_ms, 200);
    assert_eq!(bm.vision_ms, Some(150));
    assert_eq!(bm.get("clip_ms"), Some(50));
    assert_eq!(bm.get("nsfw_ms"), Some(20));
    assert_eq!(bm.get("watermark_ms"), Some(10));
    assert_eq!(bm.get("tag_ms"), Some(50));
}
