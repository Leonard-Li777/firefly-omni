//! binary_file_perceive_test.rs — 二进制文件多模态感知契约与防误打标测试
//!
//! 验证：
//! 1. 含 NUL 字节的二进制文件（如 .lnk、.exe、.bin 等）在 POST /api/perceive 时，
//!    markdown_content 严格保持为空，严禁泄露 "[Binary File] NUL byte detected" 占位符；
//! 2. 严禁从调试占位符中抽取 "Binary"、"skipped"、"二进位组"、"哩哩啦啦" 等荒谬标签；
//! 3. 智能文件名 smart_name 绝不得命名为 "Binary与skipped" 等基于占位符的拼接名。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use omni_core::{OmniConfig, OmniPerceptionResult};
use omni_server::{create_app_router, AppState};
use std::io::Write;
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
        dimension_policies: Arc::new(std::sync::RwLock::new(omni_core::get_default_dimension_policies())),
        sherpa: Arc::new(omni_server::SherpaManager::new()),
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
async fn test_perceive_binary_file_no_fake_tags_or_smart_name() {
    let app = setup_test_app();
    let temp_file = std::env::temp_dir().join("Zoom_Workplace_test.lnk");
    {
        let mut f = std::fs::File::create(&temp_file).unwrap();
        // 模拟 Windows 快捷方式二进制头部结构 (包含多个 NUL 字节)
        f.write_all(b"\x4c\x00\x00\x00\x01\x14\x02\x00\x00\x00\x00\x00\xc0\x00\x00\x00\x00\x00\x00\x46").unwrap();
        f.write_all(b"Zoom Workplace Target Binary Information Here").unwrap();
    }

    let res = perceive_file(&app, &temp_file, Some("zh")).await;
    let _ = std::fs::remove_file(&temp_file);

    // 1. 正文不得包含占位符
    assert!(
        !res.markdown_content.contains("NUL byte detected"),
        "markdown_content 不得包含 NUL byte detected 占位提示，实际为: {}",
        res.markdown_content
    );
    assert!(
        res.markdown_content.trim().is_empty(),
        "二进制文件的 markdown_content 应为空，实际为: {}",
        res.markdown_content
    );

    // 2. 标签中严禁出现 "Binary", "skipped", "哩哩啦啦", "二进位组" 等基于错误占位语的伪标签
    let all_tag_names: Vec<String> = res
        .fused_tags
        .iter()
        .map(|t| t.name.clone())
        .collect();

    assert!(
        !all_tag_names.iter().any(|n| n == "Binary" || n == "skipped" || n == "哩哩啦啦" || n == "二进位组"),
        "fused_tags 严禁出现占位符衍生的伪标签，当前标签: {:?}",
        all_tag_names
    );

    // 3. 智能文件名严禁为 "Binary与skipped"
    if let Some(sn) = &res.smart_name {
        assert!(
            !sn.contains("Binary") && !sn.contains("skipped"),
            "smart_name 严禁包含占位符词串，当前为: {}",
            sn
        );
    }
}

#[tokio::test]
async fn test_perceive_real_pe_binary_metadata_and_dual_tags() {
    let notepad_path = std::path::Path::new(r"C:\Windows\System32\notepad.exe");
    if !notepad_path.exists() {
        eprintln!("notepad.exe not found on this system, skipping test");
        return;
    }

    let app = setup_test_app();
    let res = perceive_file(&app, notepad_path, Some("zh")).await;

    // 1. binary_info 结构化事实必须透出
    assert!(res.binary_info.is_some(), "binary_info 必须有值");
    let bin = res.binary_info.unwrap();
    assert_eq!(bin.format, "PE");

    // 2. author 统一透出 (微软签名或公司名)
    assert!(res.author.is_some(), "author 必须统一透出厂商或签名机构，当前为 None");
    let author = res.author.unwrap();
    assert!(
        author.contains("Microsoft"),
        "author 应包含 Microsoft，实际为: {}",
        author
    );

    // 3. smart_name 优先取 product_name
    assert!(res.smart_name.is_some(), "smart_name 应提取自产品名");
    let sn = res.smart_name.unwrap();
    assert!(
        sn.contains("Notepad") || sn.contains("记事本") || sn.contains("Windows"),
        "smart_name 实际为: {}",
        sn
    );

    // 4. fused_tags 必须包含 Windows 平台标签，以及正交派生标签
    let has_platform_win = res.fused_tags.iter().any(|t| t.code == omni_core::concepts::Concept::Windows.code());
    assert!(has_platform_win, "fused_tags 必须包含 Windows 受控代码 (builtin.windows)");

    // 5. 验证双路径（正交派生）：严格断言存在开发工具与工具应用，消除 if 条件旁路
    let has_dev_tools = res.fused_tags.iter().any(|t| t.code == omni_core::concepts::Concept::开发工具.code());
    assert!(has_dev_tools, "fused_tags 必须包含程序细分受控标签 (开发工具)");

    let has_tools_util = res.fused_tags.iter().any(|t| t.code == omni_core::concepts::Concept::工具应用.code());
    assert!(has_tools_util, "程序细分为开发工具时，必须正交派生文件用途中的 工具应用 标签");

    let dev_tag = res.fused_tags.iter().find(|t| t.code == omni_core::concepts::Concept::开发工具.code()).unwrap();
    assert!(
        dev_tag.parent_codes.contains(&omni_core::concepts::Concept::工具应用.code().to_string()),
        "开发工具的 parent_codes 必须包含 tagImplications 蕴涵的目标父级 (工具应用)"
    );
}

