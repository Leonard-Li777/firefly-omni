#[cfg(test)]
mod cases {
    use crate::routes::taxonomy::inspect::evaluate_node_policy;
    use crate::routes::taxonomy::*;
    use rusqlite::Connection;

#[test]
fn test_inspect_controlled_tag_and_transitive_ancestor_closure() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE tag_aliases_zh_CN (
            tag_code TEXT NOT NULL,
            lemma TEXT NOT NULL,
            is_canonical INTEGER NOT NULL DEFAULT 1,
            source TEXT NOT NULL DEFAULT 'builtin',
            count INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE file_tags (
            code TEXT PRIMARY KEY,
            parent_codes TEXT,
            source TEXT NOT NULL DEFAULT 'builtin'
        );

        -- 1. 安全等级与公开 (直接匹配)
        INSERT INTO tag_aliases_zh_CN (tag_code, lemma, is_canonical) VALUES
            ('builtin.security_level', '安全等级', 1),
            ('omw.01704761.a', '公开', 1);
        INSERT INTO file_tags (code, parent_codes, source) VALUES
            ('builtin.security_level', '["builtin.domain"]', 'dimension'),
            ('omw.01704761.a', '["builtin.security_level"]', 'tag');

        -- 2. 发票 -> 财务票据 -> 文件用途 (多级祖先闭包)
        INSERT INTO tag_aliases_zh_CN (tag_code, lemma, is_canonical) VALUES
            ('builtin.file_purpose', '文件用途', 1),
            ('builtin.financial_type', '财务票据', 1),
            ('builtin.invoice', '发票', 1);
        INSERT INTO file_tags (code, parent_codes, source) VALUES
            ('builtin.file_purpose', '["builtin.domain"]', 'dimension'),
            ('builtin.financial_type', '["builtin.file_purpose"]', 'dimension'),
            ('builtin.invoice', '["builtin.financial_type"]', 'tag');

        -- 3. 塔防 -> 游戏类型
        INSERT INTO tag_aliases_zh_CN (tag_code, lemma, is_canonical) VALUES
            ('builtin.game_genre', '游戏类型', 1),
            ('builtin.tower_defense', '塔防', 1);
        INSERT INTO file_tags (code, parent_codes, source) VALUES
            ('builtin.game_genre', '["builtin.domain"]', 'dimension'),
            ('builtin.tower_defense', '["builtin.game_genre"]', 'tag');
        "#,
    )
    .unwrap();

    // Case 1: 直接受控匹配 (公开在安全等级下)
    let res_public = inspect_single_tag(&conn, "公开", "builtin.security_level", "zh-CN");
    assert!(res_public.is_controlled);
    assert!(res_public.matched_parent);
    assert_eq!(res_public.code.as_deref(), Some("omw.01704761.a"));
    assert_eq!(res_public.ancestry_matched_code.as_deref(), Some("builtin.security_level"));

    // Case 2: 祖先闭包多级匹配 (发票挂在顶层文件用途下)
    let res_invoice = inspect_single_tag(&conn, "发票", "builtin.file_purpose", "zh-CN");
    assert!(res_invoice.is_controlled);
    assert!(res_invoice.matched_parent);
    assert_eq!(res_invoice.code.as_deref(), Some("builtin.invoice"));
    assert_eq!(res_invoice.ancestry_matched_code.as_deref(), Some("builtin.file_purpose"));

    // Case 3: 受控但父级不匹配 (塔防挂在作者下)
    let author_code = omni_core::concepts::Concept::作者.code();
    let res_td = inspect_single_tag(&conn, "塔防", author_code, "zh-CN");
    assert!(res_td.is_controlled);
    assert!(!res_td.matched_parent);
    assert_eq!(res_td.code.as_deref(), Some("builtin.tower_defense"));
    assert!(res_td.ancestry_matched_code.is_none());

    // Case 4: 完全未收录的自定义标签 (张三)
    let res_custom = inspect_single_tag(&conn, "张三", author_code, "zh-CN");
    assert!(!res_custom.is_controlled);
    assert!(!res_custom.matched_parent);
    assert!(res_custom.code.is_none());
    assert!(res_custom.ancestry_matched_code.is_none());

    // Case 5: 批量批处理检验
    let batch_items = vec![
        InspectTagItem {
            tag_name: "公开".to_string(),
            target_parent_code: "builtin.security_level".to_string(),
        },
        InspectTagItem {
            tag_name: "发票".to_string(),
            target_parent_code: "builtin.file_purpose".to_string(),
        },
        InspectTagItem {
            tag_name: "塔防".to_string(),
            target_parent_code: author_code.to_string(),
        },
        InspectTagItem {
            tag_name: "张三".to_string(),
            target_parent_code: author_code.to_string(),
        },
    ];
    let batch_res = inspect_tags_batch(&conn, &batch_items, "zh-CN").unwrap();
    assert_eq!(batch_res.len(), 4);
    assert!(batch_res[0].matched_parent);
    assert!(batch_res[1].matched_parent);
    assert!(!batch_res[2].matched_parent);
    assert!(!batch_res[3].is_controlled);
}

#[test]
fn test_evaluate_node_policy_precedence() {
    // P0: 根维度豁免
    let ctx_root_keep = NodeEvaluationContext {
        code: "builtin.domain",
        pos: Some("n"),
        lexfile: None,
        sememe_category: None,
        is_root: true,
        in_action_branch: false,
    };
    assert_eq!(evaluate_node_policy(&ctx_root_keep), TaxonomyFoldPolicy::Keep);

    let ctx_root_action = NodeEvaluationContext {
        code: "builtin.category.action",
        pos: Some("n"),
        lexfile: None,
        sememe_category: Some(1),
        is_root: true,
        in_action_branch: false,
    };
    assert_eq!(
        evaluate_node_policy(&ctx_root_action),
        TaxonomyFoldPolicy::FlattenToActionRoot
    );

    // P1: 一票否决
    let ctx_p1_function_word = NodeEvaluationContext {
        code: "builtin.category.function_word",
        pos: None,
        lexfile: None,
        sememe_category: Some(7),
        is_root: true,
        in_action_branch: false,
    };
    assert_eq!(
        evaluate_node_policy(&ctx_p1_function_word),
        TaxonomyFoldPolicy::RejectSubtree
    );

    let ctx_p1_pos = NodeEvaluationContext {
        code: "some.word",
        pos: Some("u"),
        lexfile: None,
        sememe_category: None,
        is_root: false,
        in_action_branch: false,
    };
    assert_eq!(evaluate_node_policy(&ctx_p1_pos), TaxonomyFoldPolicy::RejectSubtree);

    let ctx_p1_lexfile = NodeEvaluationContext {
        code: "some.adv",
        pos: None,
        lexfile: Some("adv.all"),
        sememe_category: None,
        is_root: false,
        in_action_branch: false,
    };
    assert_eq!(evaluate_node_policy(&ctx_p1_lexfile), TaxonomyFoldPolicy::RejectSubtree);

    // P2: 动作分支向下传染
    let ctx_p2_in_action = NodeEvaluationContext {
        code: "hownet.step.n",
        pos: Some("n"),
        lexfile: None,
        sememe_category: None,
        is_root: false,
        in_action_branch: true,
    };
    assert_eq!(
        evaluate_node_policy(&ctx_p2_in_action),
        TaxonomyFoldPolicy::FlattenToActionRoot
    );

    let ctx_p2_verb = NodeEvaluationContext {
        code: "omw.00109535.v",
        pos: Some("v"),
        lexfile: Some("verb.change"),
        sememe_category: None,
        is_root: false,
        in_action_branch: false,
    };
    assert_eq!(
        evaluate_node_policy(&ctx_p2_verb),
        TaxonomyFoldPolicy::FlattenToActionRoot
    );

    // P3: 抽象壳词穿透
    let ctx_p3_shell = NodeEvaluationContext {
        code: "omw.00024720.n", // 状态
        pos: Some("n"),
        lexfile: None,
        sememe_category: None,
        is_root: false,
        in_action_branch: false,
    };
    assert_eq!(evaluate_node_policy(&ctx_p3_shell), TaxonomyFoldPolicy::BypassAndHoist);

    // WordNet 顶层哲学壳 (noun.Tops) 穿透
    let ctx_p3_tops = NodeEvaluationContext {
        code: "omw.00032613.n", // possession (所有)
        pos: Some("n"),
        lexfile: Some("noun.Tops"),
        sememe_category: None,
        is_root: false,
        in_action_branch: false,
    };
    assert_eq!(evaluate_node_policy(&ctx_p3_tops), TaxonomyFoldPolicy::BypassAndHoist);

    // P4: 名词实体默认展开
    let ctx_p4_noun = NodeEvaluationContext {
        code: "omw.00000001.n",
        pos: Some("n"),
        lexfile: Some("noun.animal"),
        sememe_category: Some(3),
        is_root: false,
        in_action_branch: false,
    };
    assert_eq!(evaluate_node_policy(&ctx_p4_noun), TaxonomyFoldPolicy::Keep);
}

/// 断言 12【AC-1: RoaringBitmap 10万文件子树去重计数耗时 < 5ms】
#[test]
fn test_roaring_bitmap_massive_files_dedup_perf() {
    use roaring::RoaringBitmap;
    use std::time::Instant;

    let mut child1 = RoaringBitmap::new();
    let mut child2 = RoaringBitmap::new();
    for id in 1..=70_000u32 {
        child1.insert(id);
    }
    for id in 30_001..=100_000u32 {
        child2.insert(id);
    }

    let start = Instant::now();
    let mut parent = RoaringBitmap::new();
    parent |= &child1;
    parent |= &child2;
    let count = parent.len();
    let elapsed = start.elapsed();

    assert_eq!(count, 100_000);
    assert!(elapsed.as_millis() < 5, "RoaringBitmap 10万去重耗时超标: {:?}", elapsed);
}

/// 断言 13【只读连接安全配置验证 (Spec §3.1.1)】
#[test]
fn test_open_omni_readonly_connection_pragmas() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test_readonly.db");
    {
        let init_conn = Connection::open(&db_path).unwrap();
        init_conn
            .execute("CREATE TABLE test (id INTEGER PRIMARY KEY)", [])
            .unwrap();
    }

    let ro_conn =
        open_omni_readonly_connection(&db_path).expect("open_omni_readonly_connection failed");
    let query_only: i64 = ro_conn.query_row("PRAGMA query_only", [], |r| r.get(0)).unwrap();
    assert_eq!(query_only, 1, "query_only 必须开启");

    let temp_store: i64 = ro_conn.query_row("PRAGMA temp_store", [], |r| r.get(0)).unwrap();
    assert_eq!(temp_store, 2, "temp_store 必须为 MEMORY (2)");

    let write_res = ro_conn.execute("INSERT INTO test (id) VALUES (1)", []);
    assert!(write_res.is_err(), "只读连接严禁写入");
}
}
