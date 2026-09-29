//! text_nsfw_perceive_test.rs — Omni 文本敏感与 NSFW 鉴定集成测试 (PRD 0052 / Slice 2)
//!
//! 验证 POST /api/perceive 针对纯文本文档的两层漏斗过滤、全年龄纯净契约、
//! 五维敏感识别、三大中性学术/历史/法学对照组防误杀、白名单目录豁免与配置热重载。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use omni_core::{OmniConfig, OmniPerceptionResult};
use omni_server::{create_app_router, AppState};
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
    };
    create_app_router(state)
}

async fn perceive_file(app: &Router, path: &std::path::Path, lang: Option<&str>) -> OmniPerceptionResult {
    let req_body = serde_json::json!({
        "file_path": path.to_str().unwrap(),
        "language": lang.unwrap_or("zh")
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
async fn test_perceive_normal_document_outputs_all_ages_without_safe_tag() {
    let app = setup_test_app();
    let temp_dir = tempfile::tempdir().unwrap();
    let file_path = temp_dir.path().join("normal_document.md");
    std::fs::write(
        &file_path,
        "# 项目周报\n\n本周完成了核心模块的重构与性能压测，系统运行平稳，无内存泄漏。",
    )
    .unwrap();

    let res = perceive_file(&app, &file_path, Some("zh")).await;

    // 契约断言：未检出违规仅输出评级语义的「全年龄」，严禁输出伪标签「安全」
    assert_eq!(res.content_rating.as_deref(), Some("safe"));
    assert_eq!(res.nsfw_tags, vec!["全年龄".to_string()]);
    assert!(!res.nsfw_tags.contains(&"安全".to_string()));
    assert!(res.sensitive_types.is_empty());
}

#[tokio::test]
async fn test_perceive_explicit_porn_document_detection() {
    let app = setup_test_app();
    let temp_dir = tempfile::tempdir().unwrap();
    let file_path = temp_dir.path().join("explicit_story.txt");
    std::fs::write(
        &file_path,
        "这里是一段露骨性行为与交欢做爱的低俗描写，包含高潮内射等内容。",
    )
    .unwrap();

    let res = perceive_file(&app, &file_path, Some("zh")).await;

    assert_eq!(res.content_rating.as_deref(), Some("r18"));
    assert!(res.sensitive_types.contains(&"色情".to_string()));
    assert!(res.nsfw_tags.contains(&"色情".to_string()));
    assert!(res.nsfw_tags.contains(&"R-18".to_string()));
    assert!(!res.nsfw_tags.contains(&"全年龄".to_string()));
    assert!(!res.nsfw_tags.contains(&"安全".to_string()));
}

#[tokio::test]
async fn test_perceive_violence_terror_document_detection() {
    let app = setup_test_app();
    let temp_dir = tempfile::tempdir().unwrap();
    let file_path = temp_dir.path().join("terror_record.txt");
    std::fs::write(
        &file_path,
        "极端恐怖分子策划自杀式袭击并实施断头斩首与肢解碎尸的极端暴力行为。",
    )
    .unwrap();

    let res = perceive_file(&app, &file_path, Some("zh")).await;

    assert_eq!(res.content_rating.as_deref(), Some("r18g"));
    assert!(res.sensitive_types.contains(&"血腥".to_string()));
    assert!(res.nsfw_tags.contains(&"血腥暴力".to_string()));
    assert!(res.nsfw_tags.contains(&"R-18G".to_string()));
}

#[tokio::test]
async fn test_perceive_political_sensitive_document_detection() {
    let app = setup_test_app();
    let temp_dir = tempfile::tempdir().unwrap();
    let file_path = temp_dir.path().join("politics_manifesto.txt");
    std::fs::write(
        &file_path,
        "境外反动势力企图组织反党言论并煽动颠覆国家政权的分裂活动。",
    )
    .unwrap();

    let res = perceive_file(&app, &file_path, Some("zh")).await;

    assert_eq!(res.content_rating.as_deref(), Some("r18"));
    assert!(res.sensitive_types.contains(&"涉政".to_string()));
    assert!(res.nsfw_tags.contains(&"涉政违规".to_string()));
}

#[tokio::test]
async fn test_perceive_abuse_document_detection() {
    let app = setup_test_app();
    let temp_dir = tempfile::tempdir().unwrap();
    let file_path = temp_dir.path().join("abuse_rant.txt");
    std::fs::write(
        &file_path,
        "真是一个弱智傻逼，操你妈的赶紧去死吧，畜生不如！",
    )
    .unwrap();

    let res = perceive_file(&app, &file_path, Some("zh")).await;

    assert_eq!(res.content_rating.as_deref(), Some("r15"));
    assert!(res.sensitive_types.contains(&"辱骂".to_string()));
    assert!(res.nsfw_tags.contains(&"仇恨辱骂".to_string()));
}

#[tokio::test]
async fn test_perceive_medical_reference_anti_false_positive() {
    let app = setup_test_app();
    let temp_dir = tempfile::tempdir().unwrap();
    let file_path = temp_dir.path().join("anatomy_textbook.md");
    std::fs::write(
        &file_path,
        "# 高等医学院校教材《人体系统解剖学》\n\n第三章：男性生殖系统解剖生理学特征与生殖器官病理切片检查分析，属于临床医学教学标准。",
    )
    .unwrap();

    let res = perceive_file(&app, &file_path, Some("zh")).await;

    // 对照组防误杀保障：医学解剖文本绝不误判为色情 R-18
    assert_eq!(res.content_rating.as_deref(), Some("safe"));
    assert_eq!(res.nsfw_tags, vec!["全年龄".to_string()]);
    assert!(!res.sensitive_types.contains(&"色情".to_string()));
}

#[tokio::test]
async fn test_perceive_history_war_records_anti_false_positive() {
    let app = setup_test_app();
    let temp_dir = tempfile::tempdir().unwrap();
    let file_path = temp_dir.path().join("war_history.md");
    std::fs::write(
        &file_path,
        "# 台儿庄战役战况详报\n\n抗日战争历史文献：第五战区司令部关于前线伤亡统计、烈士牺牲名录与缴获敌军战报之纪实史料。",
    )
    .unwrap();

    let res = perceive_file(&app, &file_path, Some("zh")).await;

    // 对照组防误杀保障：历史战争纪实绝不误判为暴恐 R-18G
    assert_eq!(res.content_rating.as_deref(), Some("safe"));
    assert_eq!(res.nsfw_tags, vec!["全年龄".to_string()]);
    assert!(!res.sensitive_types.contains(&"血腥".to_string()));
}

#[tokio::test]
async fn test_perceive_legal_statutes_anti_false_positive() {
    let app = setup_test_app();
    let temp_dir = tempfile::tempdir().unwrap();
    let file_path = temp_dir.path().join("constitutional_study.md");
    std::fs::write(
        &file_path,
        "# 宪法学讲义\n\n全国人大常委会法制工作委员会关于《中华人民共和国宪法》国家机构与公民权利条款的法理学司法解释普及。",
    )
    .unwrap();

    let res = perceive_file(&app, &file_path, Some("zh")).await;

    // 对照组防误杀保障：宪法法律学术研究绝不误判为涉政违规
    assert_eq!(res.content_rating.as_deref(), Some("safe"));
    assert_eq!(res.nsfw_tags, vec!["全年龄".to_string()]);
    assert!(!res.sensitive_types.contains(&"涉政".to_string()));
}

#[tokio::test]
async fn test_perceive_whitelist_directory_exemption() {
    let app = setup_test_app();
    let temp_dir = tempfile::tempdir().unwrap();
    let whitelist_subfolder = temp_dir.path().join("medical_reference");
    std::fs::create_dir_all(&whitelist_subfolder).unwrap();

    let file_path = whitelist_subfolder.join("case_study.md");
    std::fs::write(
        &file_path,
        "分析涉及露骨性行为与涉政违规的特殊案例报告。",
    )
    .unwrap();

    let res = perceive_file(&app, &file_path, Some("zh")).await;

    // 白名单目录路径直接豁免
    assert_eq!(res.content_rating.as_deref(), Some("safe"));
    assert_eq!(res.nsfw_tags, vec!["全年龄".to_string()]);
    assert!(res.sensitive_types.is_empty());
}

#[tokio::test]
async fn test_perceive_config_reload_resilience() {
    // 验证配置重载机制与损坏降级
    assert!(omni_pro::text::FastTextNsfwClassifier::reload_config());
    let cfg = omni_pro::text::FastTextNsfwClassifier::get_config();
    assert_eq!(cfg.version, "1.0.0");
    assert!(cfg.get_threshold("pornography", Some("zh")) > 0.0);
}
