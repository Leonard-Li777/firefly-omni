//! semantic_pack_and_vector_tests.rs — 只读语义包零落地挂载与 zvec 向量引擎集成测试
//!
//! 依据 ADR-0038 与 PRD #679 验收契约：
//! 1. 只读包打包、AES-256-GCM 认证解密、Zstd 流式解压、sqlite3_deserialize 内存挂载；
//! 2. 单点点查 < 0.5ms，超义词递归查询 < 2ms；
//! 3. 篡改密文触发 AEAD 认证阻断；
//! 4. 向量引擎 RaBitQ + INT8 量化落盘、亚毫秒级 Top-K 检索、持久化与重启一致性；
//! 5. Axum HTTP 路由端到端验收 (/api/v1/taxonomy/* 与 /api/v1/vector/*)。

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use omni_core::OmniConfig;
use omni_server::{
    create_app_router, routes::taxonomy::*, AppState, OmwDb,
    SemanticPackLoader, VectorEngine, VECTOR_DIM,
};
use rusqlite::Connection;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tower::ServiceExt;

/// 构造用于只读包打包的最小 SQLite 语义库二进制
fn create_test_sqlite_bytes() -> Vec<u8> {
    let conn = Connection::open_in_memory().expect("创建内存数据库失败");
    conn.execute_batch(
        "CREATE TABLE file_tags (
            code TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            parent_codes TEXT NOT NULL DEFAULT '[]'
        );
        CREATE TABLE tag_aliases_zh_CN (
            tag_code TEXT NOT NULL,
            lemma TEXT NOT NULL,
            is_canonical INTEGER NOT NULL DEFAULT 0,
            n INTEGER NOT NULL DEFAULT 1,
            count INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (tag_code, lemma)
        ) WITHOUT ROWID;

        CREATE TABLE tag_aliases_en_US (
            tag_code TEXT NOT NULL,
            lemma TEXT NOT NULL,
            is_canonical INTEGER NOT NULL DEFAULT 0,
            n INTEGER NOT NULL DEFAULT 1,
            count INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (tag_code, lemma)
        ) WITHOUT ROWID;

        CREATE TABLE omw_relations (
            source_id TEXT NOT NULL,
            target_id TEXT NOT NULL,
            rel_type TEXT NOT NULL,
            PRIMARY KEY (source_id, target_id, rel_type)
        );

        INSERT INTO file_tags (code, name, parent_codes) VALUES
            ('builtin.document', '文档', '[]'),
            ('builtin.finance', '财务', '[\"builtin.document\"]'),
            ('builtin.invoice', '发票', '[\"builtin.finance\"]'),
            ('builtin.receipt', '收据', '[\"builtin.finance\"]');

        INSERT INTO tag_aliases_zh_CN (tag_code, lemma, is_canonical, n, count) VALUES
            ('builtin.document', '文档', 1, 1, 100),
            ('builtin.finance', '财务', 1, 1, 90),
            ('builtin.finance', '金融', 0, 1, 50),
            ('builtin.invoice', '发票', 1, 1, 80),
            ('builtin.invoice', '发票单据', 0, 1, 40),
            ('builtin.receipt', '收据', 1, 1, 70);

        INSERT INTO tag_aliases_en_US (tag_code, lemma, is_canonical, n, count) VALUES
            ('builtin.document', 'Document', 1, 1, 100),
            ('builtin.finance', 'Finance', 1, 1, 90),
            ('builtin.invoice', 'Invoice', 1, 1, 80),
            ('builtin.receipt', 'Receipt', 1, 1, 70);

        INSERT INTO omw_relations (source_id, target_id, rel_type) VALUES
            ('omw.00000001.n', 'omw.00000002.n', 'hypernym'),
            ('omw.00000002.n', 'omw.00000003.n', 'hypernym');"
    )
    .expect("初始化测试 SQLite 数据失败");

    // 序列化导出
    let data = conn.serialize(rusqlite::DatabaseName::Main).expect("序列化失败");
    data.to_vec()
}

#[test]
fn test_semantic_pack_pack_and_zero_disk_mount() {
    let sqlite_bytes = create_test_sqlite_bytes();
    let _key = SemanticPackLoader::resolve_derived_key();

    // 1. 打包生成 semantic.pack 二进制
    let pack_bytes = SemanticPackLoader::create_semantic_pack(&sqlite_bytes)
        .expect("创建 semantic.pack 失败");

    // 2. 校验包头与魔数
    assert!(pack_bytes.len() > 64);
    assert_eq!(&pack_bytes[0..4], b"FFSP");

    // 3. 零磁盘纯内存挂载
    let conn = SemanticPackLoader::load_from_bytes(&pack_bytes)
        .expect("零磁盘挂载 semantic.pack 失败");

    // 4. 单点点查性能契约 (< 0.5ms)
    let t_start = Instant::now();
    let tag_name: String = conn
        .query_row(
            "SELECT name FROM file_tags WHERE code = 'builtin.invoice'",
            [],
            |r| r.get(0),
        )
        .expect("点查发票标签失败");
    let point_lookup_cost = t_start.elapsed();
    assert_eq!(tag_name, "发票");
    println!("只读包单点查询耗时: {:?}", point_lookup_cost);
    assert!(
        point_lookup_cost.as_micros() < 500,
        "单点查询延迟超标: {:?}",
        point_lookup_cost
    );

    // 5. 递归 CTE 查询性能契约 (< 2ms)
    let t_cte = Instant::now();
    let mut stmt = conn
        .prepare(
            "WITH RECURSIVE hypernyms(id, level) AS (
                SELECT target_id, 1 FROM omw_relations WHERE source_id = 'omw.00000001.n' AND rel_type = 'hypernym'
                UNION ALL
                SELECT r.target_id, h.level + 1 FROM omw_relations r
                JOIN hypernyms h ON r.source_id = h.id
                WHERE r.rel_type = 'hypernym' AND h.level < 64
            )
            SELECT id FROM hypernyms",
        )
        .expect("准备递归 CTE 失败");

    let ancestors: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .expect("执行 CTE 映射失败")
        .filter_map(|r| r.ok())
        .collect();
    let cte_cost = t_cte.elapsed();
    assert_eq!(ancestors, vec!["omw.00000002.n", "omw.00000003.n"]);
    println!("递归 CTE 查询耗时: {:?}", cte_cost);
    assert!(
        cte_cost.as_millis() < 2,
        "递归 CTE 查询延迟超标: {:?}",
        cte_cost
    );

    // 6. 验证强制只读护栏
    let write_res = conn.execute(
        "INSERT INTO file_tags (code, name) VALUES ('test.hack', '篡改')",
        [],
    );
    assert!(
        write_res.is_err(),
        "只读数据库应拒绝写入操作"
    );
}

#[test]
fn test_semantic_pack_tamper_proofing() {
    let sqlite_bytes = create_test_sqlite_bytes();
    let _key = SemanticPackLoader::resolve_derived_key();
    let mut pack_bytes = SemanticPackLoader::create_semantic_pack(&sqlite_bytes)
        .expect("创建 semantic.pack 失败");

    // 篡改密文负载的一个字节
    let last_idx = pack_bytes.len() - 1;
    pack_bytes[last_idx] ^= 0xFF;

    let res = SemanticPackLoader::load_from_bytes(&pack_bytes);
    assert!(
        res.is_err(),
        "被篡改密文的包必须被 AEAD 认证阻断拦截"
    );
}

#[test]
fn test_semantic_pack_load_raw_from_file_and_discovery() {
    let sqlite_bytes = create_test_sqlite_bytes();
    let pack_bytes = SemanticPackLoader::create_semantic_pack(&sqlite_bytes)
        .expect("创建 semantic.pack 失败");

    let temp_dir = tempfile::tempdir().expect("创建临时目录失败");
    let pack_path = temp_dir.path().join("semantic.pack");
    std::fs::write(&pack_path, &pack_bytes).expect("写入临时 semantic.pack 失败");

    // 验证 load_pack_raw_from_file 能够成功解密解压
    let raw_bytes = SemanticPackLoader::load_pack_raw_from_file(&pack_path)
        .expect("从文件解密加载原始 SQLite 失败");
    assert!(!raw_bytes.is_empty());
    assert_eq!(&raw_bytes[0..16], b"SQLite format 3\0");

    // 验证真实构建产物（若存在）能够被候选密钥链顺利解密
    if let Some(real_pack_path) = SemanticPackLoader::discover_pack_path() {
        println!("发现本地只读语义包: {}", real_pack_path.display());
        let real_raw_bytes = SemanticPackLoader::load_pack_raw_from_file(&real_pack_path)
            .expect("解密真实 semantic.pack 失败");
        assert!(!real_raw_bytes.is_empty());
        assert_eq!(&real_raw_bytes[0..16], b"SQLite format 3\0");
    }
}

#[test]
fn test_vector_engine_rabitq_and_int8_ann() {
    let temp_dir = tempfile::tempdir().expect("创建临时目录失败");
    let engine = VectorEngine::open(temp_dir.path()).expect("打开向量引擎失败");

    // 构造测试向量 (384 维)
    let mut base_vec = vec![0.0f32; VECTOR_DIM];
    for i in 0..VECTOR_DIM {
        base_vec[i] = (i as f32).sin();
    }

    // 相似向量 (微小扰动)
    let mut similar_vec = base_vec.clone();
    similar_vec[0] += 0.05;
    similar_vec[1] -= 0.05;

    // 正交/无关向量
    let mut diff_vec = vec![0.0f32; VECTOR_DIM];
    for i in 0..VECTOR_DIM {
        diff_vec[i] = (i as f32).cos();
    }

    // 1. 维度契约校验
    assert!(engine.upsert("fp_invalid", &[0.1, 0.2]).is_err());
    assert!(engine.upsert("fp_nan", &vec![f32::NAN; VECTOR_DIM]).is_err());

    // 2. 写入特征向量
    engine.upsert("fp_base", &base_vec).expect("写入 base 失败");
    engine.upsert("fp_similar", &similar_vec).expect("写入 similar 失败");
    engine.upsert("fp_diff", &diff_vec).expect("写入 diff 失败");
    assert_eq!(engine.count(), 3);

    // 3. 极速 Top-K 检索性能与精度
    // 预热热身 (消除 debug 模式冷启动页缺失与首次内存分配抖动)
    let _ = engine.search(&base_vec, 1, None);

    let mut min_search_cost = std::time::Duration::from_secs(10);
    let mut last_matches = Vec::new();
    // 运行多次测量以平滑抖动
    for _ in 0..5 {
        let t_search = Instant::now();
        let matches = engine.search(&base_vec, 2, None).expect("检索失败");
        let cost = t_search.elapsed();
        if cost < min_search_cost {
            min_search_cost = cost;
        }
        last_matches = matches;
    }
    println!("向量引擎检索最优耗时: {:?}", min_search_cost);

    let matches = last_matches;
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0].file_fingerprint, "fp_base");
    assert!((matches[0].score - 1.0).abs() < 0.05); // 余弦相似度接近 1.0
    assert_eq!(matches[1].file_fingerprint, "fp_similar");
    assert!(matches[1].score > 0.85);

    // 性能契约: release 优化模式下应 < 1ms (通常 < 100µs)；Debug 模式下放宽至 < 5ms 以容忍无内联开销
    let max_allowed_us = if cfg!(debug_assertions) { 5000 } else { 1000 };
    assert!(
        min_search_cost.as_micros() < max_allowed_us,
        "向量检索耗时超过契约限制 (当前: {:?}, 允许: {}µs)",
        min_search_cost,
        max_allowed_us
    );

    // 4. 批量删除
    let deleted = engine.delete(&["fp_diff".to_string()]).expect("删除失败");
    assert_eq!(deleted, 1);
    assert_eq!(engine.count(), 2);

    // 5. 持久化与重启恢复测试
    drop(engine);
    let reloaded = VectorEngine::open(temp_dir.path()).expect("重新打开向量引擎失败");
    assert_eq!(reloaded.count(), 2);
    let reloaded_matches = reloaded.search(&base_vec, 2, None).expect("重启后检索失败");
    assert_eq!(reloaded_matches[0].file_fingerprint, "fp_base");
    assert_eq!(reloaded_matches[1].file_fingerprint, "fp_similar");
}

fn setup_test_app_with_pack_and_vector() -> axum::Router {
    let sqlite_bytes = create_test_sqlite_bytes();
    let pack_bytes = SemanticPackLoader::create_semantic_pack(&sqlite_bytes).expect("打包测试库失败");
    let omw = OmwDb::unavailable();
    omw.load_pack_bytes(&pack_bytes).expect("挂载测试库失败");

    let vector = Arc::new(VectorEngine::in_memory());

    let state = AppState {
        config: Arc::new(Mutex::new(OmniConfig::default())),
        geo: Arc::new(omni_pro::geo::GeoService::unavailable()),
        hownet: Arc::new(omni_pro::hownet::OmniHowNetService::unavailable()),
        search: Arc::new(omni_pro::search::OmniSearchService::default()),
        omw,
        vector,
    };
    create_app_router(state)
}

#[test]
fn test_taxonomy_aliases_display_cascade_fallback_to_en_us() {
    // 展示级联：目标语言（zh-CN）无 canonical 时，Omni 返回 name 应回退 en-US，而非露出 code
    let conn = Connection::open_in_memory().expect("创建内存库失败");
    conn.execute_batch(
        "CREATE TABLE file_tags (
            code TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            parent_codes TEXT NOT NULL DEFAULT '[]'
        );
        CREATE TABLE tag_aliases_zh_CN (
            tag_code TEXT NOT NULL, lemma TEXT NOT NULL, is_canonical INTEGER NOT NULL DEFAULT 0,
            n INTEGER NOT NULL DEFAULT 1, count INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (tag_code, lemma)
        ) WITHOUT ROWID;
        CREATE TABLE tag_aliases_en_US (
            tag_code TEXT NOT NULL, lemma TEXT NOT NULL, is_canonical INTEGER NOT NULL DEFAULT 0,
            n INTEGER NOT NULL DEFAULT 1, count INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (tag_code, lemma)
        ) WITHOUT ROWID;
        INSERT INTO file_tags (code, name) VALUES
            ('builtin.invoice', '发票'),
            ('omw.02084071.n', 'omw.02084071.n');
        INSERT INTO tag_aliases_zh_CN (tag_code, lemma, is_canonical, count) VALUES
            ('builtin.invoice', '发票', 1, 10);
        INSERT INTO tag_aliases_en_US (tag_code, lemma, is_canonical, count) VALUES
            ('builtin.invoice', 'Invoice', 1, 10),
            ('omw.02084071.n', 'bill', 1, 8);",
    )
    .expect("初始化别名夹具失败");

    let zh = query_taxonomy_aliases(&conn, "zh-CN", None).expect("zh 别名查询失败");
    // 中文有词形：仍优先中文
    assert_eq!(zh.canonical_names.get("builtin.invoice").unwrap(), "发票");
    // 中文缺失的 omw.*：回退英文 canonical，而不是 code/slug
    assert_eq!(zh.canonical_names.get("omw.02084071.n").unwrap(), "bill");

    let en = query_taxonomy_aliases(&conn, "en-US", None).expect("en 别名查询失败");
    assert_eq!(en.canonical_names.get("builtin.invoice").unwrap(), "Invoice");
    assert_eq!(en.canonical_names.get("omw.02084071.n").unwrap(), "bill");
}

#[tokio::test]
async fn test_http_taxonomy_and_vector_endpoints() {
    let app = setup_test_app_with_pack_and_vector();

    // 1. GET /api/v1/taxonomy/tree?locale=zh-CN
    let req = Request::builder()
        .uri("/api/v1/taxonomy/tree?locale=zh-CN")
        .method("GET")
        .body(Body::empty())
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let tree_resp: TaxonomyTreeResponse = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(tree_resp.locale, "zh-CN");
    assert_eq!(tree_resp.root_nodes.len(), 1); // builtin.document 为顶层根
    assert_eq!(tree_resp.root_nodes[0].code, "builtin.document");
    assert_eq!(tree_resp.root_nodes[0].name, "文档");
    assert_eq!(tree_resp.root_nodes[0].children.len(), 1); // builtin.finance
    assert_eq!(tree_resp.root_nodes[0].children[0].code, "builtin.finance");
    assert_eq!(tree_resp.root_nodes[0].children[0].children.len(), 2); // invoice, receipt

    // 2. GET /api/v1/taxonomy/aliases?locale=zh-CN
    let req = Request::builder()
        .uri("/api/v1/taxonomy/aliases?locale=zh-CN")
        .method("GET")
        .body(Body::empty())
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let aliases_resp: TaxonomyAliasesResponse = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(aliases_resp.canonical_names.get("builtin.invoice").unwrap(), "发票");
    let invoice_aliases = aliases_resp.aliases.get("builtin.invoice").unwrap();
    assert!(invoice_aliases.contains(&"发票".to_string()));
    assert!(invoice_aliases.contains(&"发票单据".to_string()));

    // 2.1 GET /api/v1/taxonomy/aliases?locale=zh-CN&prefix=builtin.finance
    let req_prefix = Request::builder()
        .uri("/api/v1/taxonomy/aliases?locale=zh-CN&prefix=builtin.finance")
        .method("GET")
        .body(Body::empty())
        .unwrap();
    let resp_prefix = app.clone().oneshot(req_prefix).await.unwrap();
    assert_eq!(resp_prefix.status(), StatusCode::OK);
    let body_bytes_prefix = axum::body::to_bytes(resp_prefix.into_body(), usize::MAX)
        .await
        .unwrap();
    let prefix_resp: TaxonomyAliasesResponse = serde_json::from_slice(&body_bytes_prefix).unwrap();
    assert!(prefix_resp.aliases.contains_key("builtin.finance"));
    assert!(!prefix_resp.aliases.contains_key("builtin.document"));

    // 3. POST /api/v1/vector/upsert
    let mut test_vec = vec![0.0f32; VECTOR_DIM];
    for i in 0..VECTOR_DIM {
        test_vec[i] = ((i as f32) * 0.1).sin();
    }
    let upsert_payload = serde_json::json!({
        "fileFingerprint": "fp-unit-test-1",
        "vector": test_vec
    });

    let req = Request::builder()
        .uri("/api/v1/vector/upsert")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(upsert_payload.to_string()))
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let upsert_resp: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(upsert_resp["success"], true);
    assert_eq!(upsert_resp["count"], 1);

    // 4. POST /api/v1/vector/search
    let search_payload = serde_json::json!({
        "vector": test_vec,
        "topK": 5
    });

    let req = Request::builder()
        .uri("/api/v1/vector/search")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(search_payload.to_string()))
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let search_resp: Value = serde_json::from_slice(&body_bytes).unwrap();
    let matches = search_resp["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0]["fileFingerprint"], "fp-unit-test-1");
    assert_eq!(matches[0]["rank"], 1);
    assert!(matches[0]["score"].as_f64().unwrap() > 0.95);
    println!("HTTP 向量检索返回耗时: {} 微秒", search_resp["durationUs"]);

    // 5. DELETE /api/v1/vector/delete
    let delete_payload = serde_json::json!({
        "fileFingerprints": ["fp-unit-test-1"]
    });

    let req = Request::builder()
        .uri("/api/v1/vector/delete")
        .method("DELETE")
        .header("content-type", "application/json")
        .body(Body::from(delete_payload.to_string()))
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let delete_resp: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(delete_resp["success"], true);
    assert_eq!(delete_resp["deletedCount"], 1);

    // 6. 验证删除后搜索不再召回
    let req = Request::builder()
        .uri("/api/v1/vector/search")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(search_payload.to_string()))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let search_resp: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(search_resp["matches"].as_array().unwrap().len(), 0);
}
