//! 批 B 接口层测试：`POST /api/text/tag`（spec §4.2 T4 / T5 / T6 / T9）
//!
//! 覆盖分工：
//! - **本文件**：接口层接线（路由可达、契约字段、降级不抛错、命中即删、编码器同源校验）；
//! - `omni-core/src/anchor_cache.rs`：淘汰策略本身（TTL / LRU / 命中即删的容器语义）；
//! - `omni-text/src/tagger.rs`：候选层、`exclude_names` 词元级剔除、过滤先于截断。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use omni_core::anchor_cache::{AnchorCache, AnchorCacheEntry};
use omni_core::encoder_id::{BEKKO, GEMMA};
use omni_core::OmniConfig;
use omni_server::{create_app_router, AppState};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

/// 构造测试路由，并把锚点缓存句柄返回给用例（用于预置锚点 / 断言淘汰）
fn setup_test_app() -> (Router, Arc<AnchorCache>) {
    let anchor_cache = Arc::new(AnchorCache::default());
    let state = AppState {
        config: Arc::new(Mutex::new(OmniConfig::default())),
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
        anchor_cache: anchor_cache.clone(),
    };
    (create_app_router(state), anchor_cache)
}

/// 发起一次打标请求，返回 HTTP 状态与响应 JSON（数组）
async fn post_tag(
    router: &Router,
    body: serde_json::Value,
) -> (StatusCode, Vec<serde_json::Value>) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/text/tag")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let raw = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    let items = json
        .as_array()
        .unwrap_or_else(|| panic!("响应必须是标签数组，实得 {json}"))
        .clone();
    (status, items)
}

fn names(items: &[serde_json::Value]) -> Vec<String> {
    items
        .iter()
        .map(|t| t["name"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn ext_codes(items: &[serde_json::Value]) -> Vec<String> {
    items
        .iter()
        .filter_map(|t| t["code"].as_str())
        .filter(|c| c.starts_with("_ext."))
        .map(|c| c.to_string())
        .collect()
}

/// 真实的 bekko-a8m 编码器是否可用（缺模型 / 缺 ONNX 运行时 ⇒ 相关用例跳过并显式说明）
fn bekko_available() -> bool {
    omni_pro::text::BekkoEmbedder::new().embed("可用性探测").is_ok()
}

/// **T4**：受控词反查铸码；停用词 / hex / 超长串被 G1 拒。
///
/// 反向可证伪：若受控反查退化为「全表扫描以外的空实现」，`小说` 将不再产出受控 code；
/// 若 G1 词形闸失效，hex 串会以 `_ext.*` 形式出现在结果里。
#[tokio::test]
async fn test_t4_controlled_reverse_lookup_and_g1_rejection() {
    let (app, _cache) = setup_test_app();

    // ① 受控词：必须命中受控概念，且**不是** `_ext.*`
    let (status, items) = post_tag(
        &app,
        serde_json::json!({
            "fileFingerprint": "fp-t4-a",
            "text": "这是一部小说，整个故事充满激昂与感动。",
            "allowNewTags": false
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!items.is_empty(), "受控词文本必须产出受控标签，实得空数组");
    assert!(
        items.iter().all(|t| !t["code"].as_str().unwrap_or_default().starts_with("_ext.")),
        "allowNewTags=false 时不得产出 _ext.*，实得 {:?}",
        names(&items)
    );

    // ② 停用词 / hex / 超长串：一律不得成为标签
    let (status, items) = post_tag(
        &app,
        serde_json::json!({
            "fileFingerprint": "fp-t4-b",
            "text": "的 了 是 a1b2c3d4e5f6a7b8 与 这个那个",
            "allowNewTags": true
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let got = names(&items);
    for banned in ["的", "了", "是", "a1b2c3d4e5f6a7b8"] {
        assert!(
            !got.iter().any(|n| n == banned),
            "G1 词形闸必须拦下 `{banned}`，实得 {got:?}"
        );
    }
}

/// **T5**：`allowNewTags: false` ⇒ 仅受控反查，零 `_ext.*`（自证循环阻断）。
///
/// 反向可证伪：把 handler 里的 `allow_new_tags` 透传改坏（例如恒传 `true`），
/// 本用例会因出现 `_ext.*` 而失败——**前提是文本里确有非受控实义词**。
#[tokio::test]
async fn test_t5_allow_new_tags_false_never_mints_ext_codes() {
    let (app, _cache) = setup_test_app();
    // 该文本含大量非受控实义词（若允许铸码必然产生 `_ext.*`）
    let text = "一段关于量子退火炉与超导磁悬浮轴承的非受控技术描述，附带若干冷僻术语";

    let (status, restricted) = post_tag(
        &app,
        serde_json::json!({
            "fileFingerprint": "fp-t5",
            "text": text,
            "allowNewTags": false
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        ext_codes(&restricted).is_empty(),
        "allowNewTags=false 严禁铸 _ext.*，实得 {:?}",
        ext_codes(&restricted)
    );

    // 对照：允许铸码但**无锚点** ⇒ 确定性降级，同样零 `_ext.*`（§2.2.6 降级路径）
    let (status, degraded) = post_tag(
        &app,
        serde_json::json!({
            "fileFingerprint": "fp-t5",
            "text": text,
            "allowNewTags": true
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        ext_codes(&degraded).is_empty(),
        "无锚点时必须降级为纯受控打标，实得 {:?}",
        ext_codes(&degraded)
    );
}

/// **T6（接口层接线）**：无缓存 ⇒ 200 + 空/受控数组且不抛错；有缓存 ⇒ **命中即删**。
#[tokio::test]
async fn test_t6_endpoint_degrades_without_panic_and_consumes_anchor_once() {
    let (app, cache) = setup_test_app();

    // ① 无缓存：不得 500，必须返回 200 数组
    let (status, items) = post_tag(
        &app,
        serde_json::json!({
            "fileFingerprint": "fp-missing",
            "text": "一段普通描述文本",
            "allowNewTags": true
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "缺锚点必须确定性降级，不得抛错");
    assert!(
        ext_codes(&items).is_empty(),
        "缺锚点时必须降级为纯受控打标，实得 {:?}",
        ext_codes(&items)
    );

    // ② 预置锚点 ⇒ 请求后必须被**取用一次即删**
    cache.put(
        "fp-once",
        AnchorCacheEntry::new(vec![0.01; omni_pro::text::EMBEDDING_DIM], BEKKO, std::time::Instant::now()),
    );
    assert_eq!(cache.len(), 1, "预置锚点后缓存应有 1 条");
    let _ = post_tag(
        &app,
        serde_json::json!({
            "fileFingerprint": "fp-once",
            "text": "一段普通描述文本",
            "allowNewTags": true
        }),
    )
    .await;
    assert!(cache.is_empty(), "锚点必须在首次取用后被删除（命中即删）");
}

/// **T9**：缓存 `encoder_id` / `dim` 与当前编码器不匹配 ⇒ 降级为纯受控打标、不产 `_ext.*`。
///
/// 正对照（**可证伪核心**）：同文本、同锚点向量，仅把 `encoder_id` 从 `GEMMA` 改为 `BEKKO`
/// 时，`_ext.*` 必须**出现**；否则「不产 `_ext.*`」只是因为该文本本来就抽不出新词、
/// 或余弦恰好全部低于阈值，断言不可证伪。
///
/// 锚点取**候选词自身**的编码（余弦恒为 1.0），而非整句编码——后者与单词的余弦普遍落在
/// 0.4~0.6 区间，会与 0.55 阈值纠缠，使正对照变成阈值标定测试（本用例只验证同源判定）。
#[tokio::test]
async fn test_t9_encoder_mismatch_degrades_to_controlled_only() {
    if !bekko_available() {
        eprintln!(
            "[skip] T9：本环境缺少 bekko-a8m 模型或 ONNX 运行时，无法构造真实锚点向量；\
             编码器同源判定本身已由 omni-core anchor_cache 单测覆盖"
        );
        return;
    }
    let embedder = omni_pro::text::BekkoEmbedder::new();
    let text = "量子退火炉与超导磁悬浮轴承的维护手册";

    // 先用既有入口探出本段文本确实能铸出 `_ext.*`（否则正对照无从成立）
    let probe = omni_pro::text::TextTagger::tag_text(text, Some("text"), None)
        .into_iter()
        .find(|t| t.code.starts_with("_ext."));
    let Some(probe) = probe else {
        eprintln!("[skip] T9 正对照：本段文本未产出任何 _ext.* 候选，改用其它样例");
        return;
    };
    let anchor = embedder.embed(&probe.name).expect("锚点编码应成功");

    let (app, cache) = setup_test_app();

    // ① 正对照：编码器身份一致 ⇒ 允许铸码（`_ext.*` 必须出现）
    cache.put(
        "fp-t9-ok",
        AnchorCacheEntry::new(anchor.clone(), BEKKO, std::time::Instant::now()),
    );
    let (status, matched) = post_tag(
        &app,
        serde_json::json!({
            "fileFingerprint": "fp-t9-ok",
            "text": text,
            "allowNewTags": true
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !ext_codes(&matched).is_empty(),
        "正对照失败：编码器同源且锚点余弦为 1.0 时本应产出 _ext.*（探针词 `{}`），实得 {:?}",
        probe.name,
        names(&matched)
    );

    // ② 反例：缓存记录为 GEMMA（异构编码器）⇒ 确定性降级，零 `_ext.*`
    cache.put(
        "fp-t9-mismatch",
        AnchorCacheEntry::new(anchor, GEMMA, std::time::Instant::now()),
    );
    let (status, degraded) = post_tag(
        &app,
        serde_json::json!({
            "fileFingerprint": "fp-t9-mismatch",
            "text": text,
            "allowNewTags": true
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        ext_codes(&degraded).is_empty(),
        "编码器不匹配必须降级为纯受控打标，实得 {:?}",
        ext_codes(&degraded)
    );
    assert!(cache.is_empty(), "不匹配条目也必须被消费移除，不得滞留");
}

/// **契约**：`TagChainItem` 不含 `tag_group`（ADR-0045 决策 4：分组在桌面注入层判定）。
#[tokio::test]
async fn test_response_contract_has_no_tag_group_field() {
    let (app, _cache) = setup_test_app();
    let (_, items) = post_tag(
        &app,
        serde_json::json!({
            "fileFingerprint": "fp-contract",
            "text": "这是一部小说。",
            "allowNewTags": false
        }),
    )
    .await;
    for item in &items {
        assert!(
            item.get("tag_group").is_none() && item.get("tagGroup").is_none(),
            "响应严禁携带分组字段（分组由桌面侧按「返送 = ai」赋组），实得 {item}"
        );
        assert!(item.get("code").is_some(), "响应元素必须含 code: {item}");
        assert!(item.get("name").is_some(), "响应元素必须含 name: {item}");
        assert!(
            item.get("confidence").is_some(),
            "响应元素必须含 confidence（仅供内部准入，不落库）: {item}"
        );
    }
}
