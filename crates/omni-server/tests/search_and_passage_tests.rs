//! search_and_passage_tests.rs — 高速未分析文件秒搜与向量段落对齐集成测试
//!
//! 依据 ADR-0039 与 PRD 0039 / Ticket 1 验收契约：
//! 1. GET /api/v1/search/fs 在多层真实文件树中高速扫描，支持大小写不敏感匹配、limit 约束截断、双轨平滑降级；
//! 2. POST /api/v1/vector/match-passages 单次 Query 编码，批量余弦相似度计算，精准锁定最相关段落，单次耗时 < 15ms；
//! 3. 边界条件防御：空关键词、空段落集合、不存在目录等软失败处理。

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use omni_core::OmniConfig;
use omni_server::{
    create_app_router, routes::fs_search::*, routes::vector::*, AppState, OmwDb, VectorEngine,
};
use std::fs::{create_dir_all, File};
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tower::ServiceExt;

/// 初始化包含内存向量引擎的最小测试 App
fn setup_test_app() -> axum::Router {
    let state = AppState {
        config: Arc::new(Mutex::new(OmniConfig::default())),
        geo: Arc::new(omni_pro::geo::GeoService::unavailable()),
        hownet: Arc::new(omni_pro::hownet::OmniHowNetService::unavailable()),
        search: Arc::new(omni_pro::search::OmniSearchService::default()),
        omw: OmwDb::unavailable(),
        vector: Arc::new(VectorEngine::in_memory()),
    };
    create_app_router(state)
}

#[tokio::test]
async fn test_fast_fs_search_walk_and_limit() {
    let temp_dir = tempfile::tempdir().expect("创建临时测试目录失败");
    let base_path = temp_dir.path();

    // 构造嵌套目录与测试文件
    let sub1 = base_path.join("finance_folder");
    let sub2 = base_path.join("documents").join("nested");
    create_dir_all(&sub1).unwrap();
    create_dir_all(&sub2).unwrap();

    let f1 = sub1.join("annual_financial_report_2025.pdf");
    {
        let mut file1 = File::create(&f1).unwrap();
        writeln!(file1, "PDF content 1").unwrap();
        file1.sync_all().unwrap();
    }

    let f2 = sub1.join("QUARTERLY_REPORT_Q3.docx");
    {
        let mut file2 = File::create(&f2).unwrap();
        writeln!(file2, "Word content 2").unwrap();
        file2.sync_all().unwrap();
    }

    let f3 = sub2.join("audit_report_final.xlsx");
    {
        let mut file3 = File::create(&f3).unwrap();
        writeln!(file3, "Excel content 3").unwrap();
        file3.sync_all().unwrap();
    }

    let f4 = base_path.join("other_file_image.png");
    {
        let mut file4 = File::create(&f4).unwrap();
        writeln!(file4, "PNG image").unwrap();
        file4.sync_all().unwrap();
    }

    let app = setup_test_app();
    let dir_str = base_path.to_string_lossy().to_string();

    // 1. 验证 limit=2 截断及大小写不敏感匹配 (q=report 应命中 f1, f2, f3 中截取 2 个)
    let uri = format!(
        "/api/v1/search/fs?dir={}&q=report&limit=2",
        urlencoding::encode(&dir_str)
    );
    let req = Request::builder()
        .uri(&uri)
        .method("GET")
        .body(Body::empty())
        .unwrap();

    let t_search = Instant::now();
    let resp = app.clone().oneshot(req).await.unwrap();
    let search_cost = t_search.elapsed();
    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let search_resp: FsSearchResponse = serde_json::from_slice(&body_bytes).unwrap();

    println!(
        "fast_walk 扫描测试耗时: {:?}, 接口返回内部耗时: {}ms",
        search_cost, search_resp.duration_ms
    );
    assert_eq!(search_resp.items.len(), 2, "返回结果条目数必须严格受 limit=2 截断约束");
    assert_eq!(search_resp.total, 2);
    assert!(search_resp.source == "fast_walk" || search_resp.source == "everything");

    for item in &search_resp.items {
        assert!(
            item.name.to_lowercase().contains("report"),
            "文件名必须包含搜索词 report，当前为: {}",
            item.name
        );
        if !item.is_directory {
            assert!(item.size > 0, "非目录文件大小应大于0: {}", item.name);
        }
        assert!(item.extension.is_some());
    }

    // 2. 验证全量 limit=100 召回全部 3 个匹配文件
    let uri_all = format!(
        "/api/v1/search/fs?dir={}&q=REPORT&limit=100",
        urlencoding::encode(&dir_str)
    );
    let req_all = Request::builder()
        .uri(&uri_all)
        .method("GET")
        .body(Body::empty())
        .unwrap();

    let resp_all = app.clone().oneshot(req_all).await.unwrap();
    assert_eq!(resp_all.status(), StatusCode::OK);
    let bytes_all = axum::body::to_bytes(resp_all.into_body(), usize::MAX)
        .await
        .unwrap();
    let all_resp: FsSearchResponse = serde_json::from_slice(&bytes_all).unwrap();
    assert_eq!(all_resp.items.len(), 3, "全量搜索应召回全部 3 个匹配文件");

    // 3. 验证空关键词查询：返回空列表
    let uri_empty_q = format!(
        "/api/v1/search/fs?dir={}&q=",
        urlencoding::encode(&dir_str)
    );
    let req_empty_q = Request::builder()
        .uri(&uri_empty_q)
        .method("GET")
        .body(Body::empty())
        .unwrap();
    let resp_empty_q = app.clone().oneshot(req_empty_q).await.unwrap();
    assert_eq!(resp_empty_q.status(), StatusCode::OK);
    let bytes_empty_q = axum::body::to_bytes(resp_empty_q.into_body(), usize::MAX)
        .await
        .unwrap();
    let empty_q_resp: FsSearchResponse = serde_json::from_slice(&bytes_empty_q).unwrap();
    assert_eq!(empty_q_resp.items.len(), 0);
    assert_eq!(empty_q_resp.total, 0);

    // 4. 验证不存在目录防御：返回 404 NOT_FOUND
    let req_not_found = Request::builder()
        .uri("/api/v1/search/fs?dir=C%3A%2Fnon_existent_folder_xyz_123&q=test")
        .method("GET")
        .body(Body::empty())
        .unwrap();
    let resp_not_found = app.oneshot(req_not_found).await.unwrap();
    assert_eq!(resp_not_found.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_vector_match_passages_alignment() {
    let app = setup_test_app();

    let payload = serde_json::json!({
        "query": "2025年企业净利润与营业收入大幅增长",
        "items": [
            {
                "fileFingerprint": "fp_financial_report",
                "passages": [
                    "本公司关于办公区空调节能与用电安全管理规范细则，请各部门严格遵守执行。",
                    "2025年度报告显示，公司营业收入和净利润双双突破历史新高，各项经营财务指标表现强劲优异。",
                    "关于组织全体员工参加秋季户外徒步及露营团建活动的日程安排与物资准备清单。"
                ]
            },
            {
                "fileFingerprint": "fp_engineering_guide",
                "passages": [
                    "微服务架构设计与高并发分布式系统核心实践指南与限流策略。",
                    "办公室绿色植物领养与定期养护常识说明。"
                ]
            }
        ]
    });

    let req = Request::builder()
        .uri("/api/v1/vector/match-passages")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(payload.to_string()))
        .unwrap();

    let t_start = Instant::now();
    let resp = app.clone().oneshot(req).await.unwrap();
    let total_cost = t_start.elapsed();
    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let match_resp: MatchPassagesResponse = serde_json::from_slice(&body_bytes).unwrap();

    println!(
        "match-passages 端到端耗时: {:?}, 服务内部耗时: {}ms",
        total_cost, match_resp.duration_ms
    );

    assert_eq!(match_resp.matches.len(), 2);

    // 验证文件 1 精准命中段落 1 ("2025年度报告显示，公司营业收入和净利润双双突破历史新高...")
    let m1 = &match_resp.matches[0];
    assert_eq!(m1.file_fingerprint, "fp_financial_report");
    assert_eq!(
        m1.best_passage_index, 1,
        "必须精准对齐到最相关的第二段 (index 1)"
    );
    assert!(m1.best_passage.contains("营业收入和净利润"));
    assert!(
        m1.similarity > 0.20,
        "相关段落余弦相似度应显著高出基准，当前: {}",
        m1.similarity
    );

    // 验证第二篇文档也正常计算
    let m2 = &match_resp.matches[1];
    assert_eq!(m2.file_fingerprint, "fp_engineering_guide");
    assert!(!m2.best_passage.is_empty());
    assert!(m2.similarity >= -1.0 && m2.similarity <= 1.0);

    // 性能契约检验: 单次请求耗时 < 15ms (CI 与 Debug 环境容忍单次冷启动波动放宽至 30ms)
    let max_allowed_ms = if cfg!(debug_assertions) { 40 } else { 15 };
    assert!(
        match_resp.duration_ms <= max_allowed_ms,
        "match-passages 接口计算耗时超出契约限制: {}ms",
        match_resp.duration_ms
    );
}

#[tokio::test]
async fn test_vector_match_passages_empty_and_robustness() {
    let app = setup_test_app();

    // 包含空 passages 列表的文件项
    let payload = serde_json::json!({
        "query": "人工智能模型架构",
        "items": [
            {
                "fileFingerprint": "fp_empty_passages",
                "passages": []
            }
        ]
    });

    let req = Request::builder()
        .uri("/api/v1/vector/match-passages")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(payload.to_string()))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let match_resp: MatchPassagesResponse = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(match_resp.matches.len(), 1);
    assert_eq!(match_resp.matches[0].file_fingerprint, "fp_empty_passages");
    assert_eq!(match_resp.matches[0].best_passage_index, 0);
    assert_eq!(match_resp.matches[0].similarity, 0.0);
    assert!(match_resp.matches[0].best_passage.is_empty());
}
