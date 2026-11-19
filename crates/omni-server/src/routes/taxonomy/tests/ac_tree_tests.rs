#[cfg(test)]
mod cases {
    use super::super::{insert_ftr_rows_b, setup_test_master_and_pack_conns};
    use crate::routes::taxonomy::*;
    use rusqlite::Connection;

/// 断言 1【图 1 动作链扁平化断言 (AC-1)】
#[test]
fn test_taxonomy_tree_action_flattening_ac1() {
    let (pack_conn, master_conn) = setup_test_master_and_pack_conns();

    pack_conn
        .execute_batch(
            "INSERT INTO file_tags (code, pos, sememe_category) VALUES
                ('builtin.category.action', 'n', 1),
                ('action.replace', 'n', 1),
                ('action.change', 'n', 1),
                ('action.modify', 'n', 1),
                ('action.delete', 'v', 1),
                ('action.research', 'n', 1),
                ('action.deep_research', 'n', 1),
                ('action.discover', 'n', 1),
                ('action.search', 'v', 1);
            INSERT INTO tag_aliases_zh_CN (tag_code, lemma) VALUES
                ('builtin.category.action', '动作生产'),
                ('action.delete', '删除'),
                ('action.search', '搜索');",
        )
        .unwrap();

    master_conn
        .execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path) VALUES
                ('fp_1', 'D:\\files\\f1.txt'),
                ('fp_2', 'D:\\files\\f2.txt');",
        )
        .unwrap();
    insert_ftr_rows_b(
        &master_conn,
        &[
            ("fp_1", "action.delete", "/builtin.category.action/action.replace/action.change/action.modify/action.delete", "/动作生产/更换/改变/修改/删除"),
            ("fp_2", "action.search", "/builtin.category.action/action.research/action.deep_research/action.discover/action.search", "/动作生产/研究/深入研究/发掘/搜索"),
        ],
    );

    let resp =
        query_taxonomy_tree_fast(&pack_conn, "zh-CN", None, &master_conn, true, None, None)
            .unwrap();
    assert_eq!(resp.root_nodes.len(), 1);
    assert_eq!(resp.root_nodes[0].code, "builtin.content_tags");
    let action_root = &resp.root_nodes[0].children[0];
    assert_eq!(action_root.code, "builtin.category.action");
    assert_eq!(action_root.name, "动作生产");
    assert_eq!(action_root.file_count, 2);

    let child_codes: Vec<String> = action_root.children.iter().map(|c| c.code.clone()).collect();
    assert_eq!(child_codes, vec!["action.delete", "action.search"]);
    for child in &action_root.children {
        assert!(child.children.is_empty(), "二级平铺动作节点不应再有子节点嵌套");
        assert_eq!(child.parent_code.as_deref(), Some("builtin.category.action"));
    }
}

/// 断言 2【图 2 OMW 动词上位链扁平化断言 (AC-2)】
#[test]
fn test_taxonomy_tree_omw_verb_flattening_ac2() {
    let (pack_conn, master_conn) = setup_test_master_and_pack_conns();

    pack_conn
        .execute_batch(
            "INSERT INTO file_tags (code, pos) VALUES
                ('omw.00109535.v', 'v'),
                ('hownet.skeleton.anti', 'n'),
                ('action.debug_test', 'v');
            INSERT INTO tag_aliases_zh_CN (tag_code, lemma) VALUES
                ('omw.00109535.v', '改变'),
                ('hownet.skeleton.anti', '反'),
                ('action.debug_test', '调测');",
        )
        .unwrap();

    master_conn
        .execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path) VALUES
                ('fp_1', 'D:\\files\\code.rs');",
        )
        .unwrap();
    insert_ftr_rows_b(
        &master_conn,
        &[(
            "fp_1",
            "action.debug_test",
            "/omw.00109535.v/hownet.skeleton.anti/action.debug_test",
            "/改变/反/调测",
        )],
    );

    let resp =
        query_taxonomy_tree_fast(&pack_conn, "zh-CN", None, &master_conn, true, None, None)
            .unwrap();
    assert_eq!(resp.root_nodes.len(), 1);
    assert_eq!(resp.root_nodes[0].code, "builtin.content_tags");
    let root = &resp.root_nodes[0].children[0];
    assert_eq!(root.code, "omw.00109535.v");
    assert_eq!(root.children.len(), 1);
    assert_eq!(root.children[0].code, "action.debug_test");
    assert_eq!(root.children[0].name, "调测");
    assert_eq!(root.children[0].parent_code.as_deref(), Some("omw.00109535.v"));
}

/// 断言 3【图 3 名词实体树形深度保全断言 (AC-3)】
#[test]
fn test_taxonomy_tree_noun_hierarchy_preservation_ac3() {
    let (pack_conn, master_conn) = setup_test_master_and_pack_conns();

    pack_conn
        .execute_batch(
            "INSERT INTO file_tags (code, pos) VALUES
                ('domain.study', 'n'),
                ('domain.discipline', 'n'),
                ('domain.biology', 'n'),
                ('domain.organism', 'n'),
                ('domain.person', 'n'),
                ('domain.villain', 'n');
            INSERT INTO tag_aliases_zh_CN (tag_code, lemma) VALUES
                ('domain.study', '学习资料'),
                ('domain.discipline', '学科领域'),
                ('domain.biology', '生物'),
                ('domain.organism', '有机体'),
                ('domain.person', '个人'),
                ('domain.villain', '坏人');",
        )
        .unwrap();

    master_conn
        .execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path) VALUES
                ('fp_1', 'D:\\files\\bio.pdf'),
                ('fp_l1', 'D:\\files\\l1.pdf'),
                ('fp_l2', 'D:\\files\\l2.pdf'),
                ('fp_l3', 'D:\\files\\l3.pdf'),
                ('fp_l4', 'D:\\files\\l4.pdf'),
                ('fp_l5', 'D:\\files\\l5.pdf');",
        )
        .unwrap();
    insert_ftr_rows_b(
        &master_conn,
        &[
            ("fp_l1", "domain.study", "/domain.study", "/学习资料"),
            ("fp_l2", "domain.discipline", "/domain.study/domain.discipline", "/学习资料/学科领域"),
            ("fp_l3", "domain.biology", "/domain.study/domain.discipline/domain.biology", "/学习资料/学科领域/生物"),
            ("fp_l4", "domain.organism", "/domain.study/domain.discipline/domain.biology/domain.organism", "/学习资料/学科领域/生物/有机体"),
            ("fp_l5", "domain.person", "/domain.study/domain.discipline/domain.biology/domain.organism/domain.person", "/学习资料/学科领域/生物/有机体/个人"),
            ("fp_1", "domain.villain", "/domain.study/domain.discipline/domain.biology/domain.organism/domain.person/domain.villain", "/学习资料/学科领域/生物/有机体/个人/坏人"),
        ],
    );

    let resp =
        query_taxonomy_tree_fast(&pack_conn, "zh-CN", None, &master_conn, true, None, None)
            .unwrap();
    assert_eq!(resp.root_nodes.len(), 1);
    assert_eq!(resp.root_nodes[0].code, "builtin.content_tags");

    let l1 = &resp.root_nodes[0].children[0];
    assert_eq!(l1.name, "学习资料");
    assert_eq!(l1.children.len(), 1);
    let l2 = &l1.children[0];
    assert_eq!(l2.name, "学科领域");
    assert_eq!(l2.children.len(), 1);
    let l3 = &l2.children[0];
    assert_eq!(l3.name, "生物");
    assert_eq!(l3.children.len(), 1);
    let l4 = &l3.children[0];
    assert_eq!(l4.name, "有机体");
    assert_eq!(l4.children.len(), 1);
    let l5 = &l4.children[0];
    assert_eq!(l5.name, "个人");
    assert_eq!(l5.children.len(), 1);
    let l6 = &l5.children[0];
    assert_eq!(l6.name, "坏人");
    assert_eq!(l6.children.len(), 0);
    assert_eq!(l6.file_count, 1);
}

/// 断言 4【HowNet 义原骨架 'n' 占位符防逃逸断言 (AC-4, ISS-08)】
#[test]
fn test_taxonomy_tree_hownet_skeleton_noun_action_flattening_ac4() {
    let (pack_conn, master_conn) = setup_test_master_and_pack_conns();

    pack_conn
        .execute_batch(
            "INSERT INTO file_tags (code, pos, sememe_category) VALUES
                ('builtin.category.action', 'n', 1),
                ('hownet.act', 'n', 1),
                ('hownet.do', 'n', 1),
                ('action.recreate', 'n', 1),
                ('action.render', 'v', 1);
            INSERT INTO tag_aliases_zh_CN (tag_code, lemma) VALUES
                ('builtin.category.action', '动作生产'),
                ('hownet.act', '行动'),
                ('hownet.do', '作'),
                ('action.recreate', '再造'),
                ('action.render', '渲染');",
        )
        .unwrap();

    master_conn
        .execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path) VALUES
                ('fp_1', 'D:\\files\\3d.blend');",
        )
        .unwrap();
    insert_ftr_rows_b(
        &master_conn,
        &[(
            "fp_1",
            "action.render",
            "/builtin.category.action/hownet.act/hownet.do/action.recreate/action.render",
            "/动作生产/行动/作/再造/渲染",
        )],
    );

    let resp =
        query_taxonomy_tree_fast(&pack_conn, "zh-CN", None, &master_conn, true, None, None)
            .unwrap();
    assert_eq!(resp.root_nodes[0].code, "builtin.content_tags");
    let root = &resp.root_nodes[0].children[0];
    assert_eq!(root.children.len(), 1);
    assert_eq!(root.children[0].code, "action.render");
    assert_eq!(root.children[0].name, "渲染");
    assert!(root.children[0].children.is_empty());
}

/// 断言 5【动词自身直接命中文件保全断言 (AC-5, ISS-03)】
#[test]
fn test_taxonomy_tree_verb_direct_file_preservation_ac5() {
    let (pack_conn, master_conn) = setup_test_master_and_pack_conns();

    pack_conn
        .execute_batch(
            "INSERT INTO file_tags (code, pos, sememe_category) VALUES
                ('builtin.category.action', 'n', 1),
                ('action.modify', 'v', 1),
                ('action.delete', 'v', 1);
            INSERT INTO tag_aliases_zh_CN (tag_code, lemma) VALUES
                ('builtin.category.action', '动作生产'),
                ('action.modify', '修改'),
                ('action.delete', '删除');",
        )
        .unwrap();

    master_conn
        .execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path) VALUES
                ('fp_1', 'D:\\files\\f1.txt'),
                ('fp_2', 'D:\\files\\f2.txt');",
        )
        .unwrap();
    insert_ftr_rows_b(
        &master_conn,
        &[
            ("fp_1", "action.delete", "/builtin.category.action/action.modify/action.delete", "/动作生产/修改/删除"),
            ("fp_2", "action.modify", "/builtin.category.action/action.modify", "/动作生产/修改"),
        ],
    );

    let resp =
        query_taxonomy_tree_fast(&pack_conn, "zh-CN", None, &master_conn, true, None, None)
            .unwrap();
    assert_eq!(resp.root_nodes[0].code, "builtin.content_tags");
    let root = &resp.root_nodes[0].children[0];
    assert_eq!(root.children.len(), 2);
    let names: Vec<String> = root.children.iter().map(|c| c.name.clone()).collect();
    assert_eq!(names, vec!["删除", "修改"]);
    assert_eq!(root.children[0].file_count, 1);
    assert_eq!(root.children[1].file_count, 1);
    assert!(root.children[0].children.is_empty());
    assert!(root.children[1].children.is_empty());
}

/// 断言 6【多分支同 Code 碰撞文件计数公理断言 (AC-6, ISS-01)】
#[test]
fn test_taxonomy_tree_multi_branch_collision_file_count_axiom_ac6() {
    let (pack_conn, master_conn) = setup_test_master_and_pack_conns();

    pack_conn
        .execute_batch(
            "INSERT INTO file_tags (code, pos, sememe_category) VALUES
                ('builtin.category.action', 'n', 1),
                ('action.branch_a', 'n', 1),
                ('action.branch_b', 'n', 1),
                ('action.sync', 'v', 1);
            INSERT INTO tag_aliases_zh_CN (tag_code, lemma) VALUES
                ('builtin.category.action', '动作生产'),
                ('action.sync', '同步');",
        )
        .unwrap();

    master_conn
        .execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path) VALUES
                ('fp_1', 'D:\\files\\f1.txt'),
                ('fp_2', 'D:\\files\\f2.txt');",
        )
        .unwrap();
    insert_ftr_rows_b(
        &master_conn,
        &[
            ("fp_1", "action.sync", "/builtin.category.action/action.branch_a/action.sync", "/动作生产/分支A/同步"),
            ("fp_2", "action.sync", "/builtin.category.action/action.branch_b/action.sync", "/动作生产/分支B/同步"),
        ],
    );

    let resp =
        query_taxonomy_tree_fast(&pack_conn, "zh-CN", None, &master_conn, true, None, None)
            .unwrap();
    assert_eq!(resp.root_nodes[0].code, "builtin.content_tags");
    let root = &resp.root_nodes[0].children[0];
    assert_eq!(root.children.len(), 1);
    let sync_node = &root.children[0];
    assert_eq!(sync_node.code, "action.sync");
    assert_eq!(sync_node.name, "同步");
    assert_eq!(sync_node.file_count, 2);
    assert_eq!(sync_node.files, vec!["fp_1", "fp_2"]);
}

/// 断言 7【抽象壳名词穿透断言 (AC-7, ISS-02)】
#[test]
fn test_taxonomy_tree_shell_noun_bypass_ac7() {
    let (pack_conn, master_conn) = setup_test_master_and_pack_conns();

    pack_conn
        .execute_batch(
            "INSERT INTO file_tags (code, pos) VALUES
                ('doc.work', 'n'),
                ('omw.00024720.n', 'n'),
                ('status.done', 'n');
            INSERT INTO tag_aliases_zh_CN (tag_code, lemma) VALUES
                ('doc.work', '工作文档'),
                ('omw.00024720.n', '状态'),
                ('status.done', '处理完毕');",
        )
        .unwrap();

    master_conn
        .execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path) VALUES
                ('fp_doc', 'D:\\files\\work.docx'),
                ('fp_1', 'D:\\files\\doc.docx');",
        )
        .unwrap();
    insert_ftr_rows_b(
        &master_conn,
        &[
            ("fp_doc", "doc.work", "/doc.work", "/工作文档"),
            ("fp_1", "status.done", "/doc.work/omw.00024720.n/status.done", "/工作文档/状态/处理完毕"),
        ],
    );

    let resp =
        query_taxonomy_tree_fast(&pack_conn, "zh-CN", None, &master_conn, true, None, None)
            .unwrap();
    assert_eq!(resp.root_nodes.len(), 1);
    assert_eq!(resp.root_nodes[0].code, "builtin.content_tags");
    let work_doc = &resp.root_nodes[0].children[0];
    assert_eq!(work_doc.name, "工作文档");
    assert_eq!(work_doc.children.len(), 1);
    assert_eq!(work_doc.children[0].code, "status.done");
    assert_eq!(work_doc.children[0].name, "处理完毕");
    assert_eq!(work_doc.children[0].parent_code.as_deref(), Some("doc.work"));
}

/// 断言 8【虚词与时序大类一票否决断言 (AC-8, ISS-11)】
#[test]
fn test_taxonomy_tree_function_word_rejection_ac8() {
    let (pack_conn, master_conn) = setup_test_master_and_pack_conns();

    pack_conn
        .execute_batch(
            "INSERT INTO file_tags (code, pos) VALUES
                ('builtin.category.function_word', 'u'),
                ('func.some_particle', 'u');
            INSERT INTO tag_aliases_zh_CN (tag_code, lemma) VALUES
                ('builtin.category.function_word', '虚词'),
                ('func.some_particle', '的');",
        )
        .unwrap();

    master_conn
        .execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path) VALUES
                ('fp_1', 'D:\\files\\f1.txt');",
        )
        .unwrap();
    insert_ftr_rows_b(
        &master_conn,
        &[(
            "fp_1",
            "func.some_particle",
            "/builtin.category.function_word/func.some_particle",
            "/虚词/的",
        )],
    );

    let resp =
        query_taxonomy_tree_fast(&pack_conn, "zh-CN", None, &master_conn, true, None, None)
            .unwrap();
    assert_eq!(resp.root_nodes.len(), 0);
}

/// 断言 9【WordNet 顶层哲学壳 noun.Tops 批量穿透断言 (AC-9)】
#[test]
fn test_taxonomy_tree_noun_tops_bypass_ac9() {
    let (pack_conn, master_conn) = setup_test_master_and_pack_conns();

    pack_conn
        .execute_batch(
            "INSERT INTO file_tags (code, pos, lexfile) VALUES
                ('omw.00032613.n', 'n', 'noun.Tops'),
                ('omw.13329641.n', 'n', 'noun.possession'),
                ('tag.contract', 'n', 'noun.communication');
            INSERT INTO tag_aliases_zh_CN (tag_code, lemma) VALUES
                ('omw.00032613.n', '所有'),
                ('omw.13329641.n', '财产'),
                ('tag.contract', '转让合同');",
        )
        .unwrap();

    master_conn
        .execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path) VALUES
                ('fp_prop', 'D:\\files\\prop.pdf'),
                ('fp_1', 'D:\\files\\contract.pdf');",
        )
        .unwrap();
    insert_ftr_rows_b(
        &master_conn,
        &[
            ("fp_prop", "omw.13329641.n", "/omw.00032613.n/omw.13329641.n", "/所有/财产"),
            ("fp_1", "tag.contract", "/omw.00032613.n/omw.13329641.n/tag.contract", "/所有/财产/转让合同"),
        ],
    );

    let resp =
        query_taxonomy_tree_fast(&pack_conn, "zh-CN", None, &master_conn, true, None, None)
            .unwrap();
    assert_eq!(resp.root_nodes.len(), 1);
    assert_eq!(resp.root_nodes[0].code, "builtin.content_tags");
    let content_tags = &resp.root_nodes[0];

    assert!(!content_tags
        .children
        .iter()
        .any(|c| c.code == "omw.00032613.n" || c.name == "所有"));
    let property_node = content_tags.children.iter().find(|c| c.code == "omw.13329641.n");
    assert!(property_node.is_some(), "财产必须作为合法分类抽屉直接挂在内容标签下");
    let property_node = property_node.unwrap();
    assert_eq!(property_node.name, "财产");
    assert_eq!(property_node.children.len(), 1);
    assert_eq!(property_node.children[0].code, "tag.contract");
    assert_eq!(property_node.children[0].name, "转让合同");
    assert_eq!(
        property_node.children[0].parent_code.as_deref(),
        Some("omw.13329641.n")
    );
}

/// 断言 10【父子同名自愈折叠与孙辈提升断言 (AC-10)】
#[test]
fn test_taxonomy_tree_parent_child_duplicate_name_folding_ac10() {
    let (pack_conn, master_conn) = setup_test_master_and_pack_conns();

    pack_conn
        .execute_batch(
            "INSERT INTO file_tags (code, pos, lexfile) VALUES
                ('omw.13353607.n', 'n', 'noun.possession'),
                ('omw.13353858.n', 'n', 'noun.possession'),
                ('omw.13354154.n', 'n', 'noun.possession');
            INSERT INTO tag_aliases_zh_CN (tag_code, lemma) VALUES
                ('omw.13353607.n', '资本'),
                ('omw.13353858.n', '资本'),
                ('omw.13354154.n', '钱财');",
        )
        .unwrap();

    master_conn
        .execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path) VALUES
                ('fp_cap_direct', 'D:\\files\\capital.txt'),
                ('fp_cap_b', 'D:\\files\\capital_b.txt'),
                ('fp_money', 'D:\\files\\money.txt');",
        )
        .unwrap();
    insert_ftr_rows_b(
        &master_conn,
        &[
            ("fp_cap_direct", "omw.13353607.n", "/omw.13353607.n", "/资本"),
            ("fp_cap_b", "omw.13353858.n", "/omw.13353607.n/omw.13353858.n", "/资本/资本"),
            ("fp_money", "omw.13354154.n", "/omw.13353607.n/omw.13353858.n/omw.13354154.n", "/资本/资本/钱财"),
        ],
    );

    let resp =
        query_taxonomy_tree_fast(&pack_conn, "zh-CN", None, &master_conn, true, None, None)
            .unwrap();
    assert_eq!(resp.root_nodes.len(), 1);
    let content_tags = &resp.root_nodes[0];

    let capital_nodes: Vec<_> = content_tags
        .children
        .iter()
        .filter(|c| c.name == "资本")
        .collect();
    assert_eq!(capital_nodes.len(), 1);
    let capital = capital_nodes[0];

    assert_eq!(capital.file_count, 3);
    assert_eq!(capital.files, vec!["fp_cap_b", "fp_cap_direct", "fp_money"]);

    assert_eq!(capital.children.len(), 1);
    let money = &capital.children[0];
    assert_eq!(money.code, "omw.13354154.n");
    assert_eq!(money.name, "钱财");
    assert_eq!(money.file_count, 1);
    assert_eq!(money.files, vec!["fp_money"]);
    assert_eq!(
        money.parent_code.as_deref(),
        Some("omw.13353607.n"),
        "孙节点的父级指针必须对齐提升后的父节点"
    );
}

/// 断言 11【小说多码兼并自愈与防环回归测试 (Task 4 验收标准)】
#[test]
fn test_taxonomy_tree_single_novel_deduplication_and_loop_prevention() {
    let (pack_conn, master_conn) = setup_test_master_and_pack_conns();

    pack_conn
        .execute_batch(
            "INSERT INTO file_tags (code, pos, lexfile) VALUES
                ('builtin.e_book', 'n', 'noun.communication'),
                ('builtin.e_book_segmentation', 'n', 'noun.communication'),
                ('builtin.theme', 'n', 'noun.cognition'),
                ('omw.03833065.n', 'n', 'noun.artifact'),
                ('omw.06367879.n', 'n', 'noun.communication');
            INSERT INTO tag_aliases_zh_CN (tag_code, lemma, is_canonical, count) VALUES
                ('builtin.e_book', '电子书', 1, 100),
                ('builtin.e_book_segmentation', '电子书细分', 1, 100),
                ('builtin.theme', '题材', 1, 100),
                ('omw.03833065.n', '小说', 1, 10000),
                ('omw.06367879.n', '小说', 0, 0);",
        )
        .unwrap();

    master_conn
        .execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path) VALUES
                ('fp_novel_legacy', 'D:\\files\\novel1.txt'),
                ('fp_novel_canonical', 'D:\\files\\novel2.txt'),
                ('fp_novel_loop', 'D:\\files\\novel3.txt');",
        )
        .unwrap();
    insert_ftr_rows_b(
        &master_conn,
        &[
            ("fp_novel_legacy", "omw.06367879.n", "/builtin.e_book/builtin.e_book_segmentation/omw.06367879.n", "/电子书/电子书细分/小说"),
            ("fp_novel_canonical", "omw.03833065.n", "/builtin.e_book/builtin.e_book_segmentation/omw.03833065.n", "/电子书/电子书细分/小说"),
            ("fp_novel_loop", "omw.03833065.n", "/builtin.e_book/builtin.e_book_segmentation/omw.03833065.n/builtin.theme/omw.03833065.n", "/电子书/电子书细分/小说/题材/小说"),
        ],
    );

    let resp =
        query_taxonomy_tree_fast(&pack_conn, "zh-CN", None, &master_conn, true, None, None)
            .unwrap();

    fn collect_novel_nodes<'a>(node: &'a TaxonomyNode, acc: &mut Vec<&'a TaxonomyNode>) {
        if node.name == "小说" {
            acc.push(node);
        }
        for child in &node.children {
            collect_novel_nodes(child, acc);
        }
    }

    let mut all_novel_nodes = Vec::new();
    for root in &resp.root_nodes {
        collect_novel_nodes(root, &mut all_novel_nodes);
    }

    assert_eq!(
        all_novel_nodes.len(),
        1,
        "整棵分类树上必须仅存在 1 个小说节点，实际找到了 {} 个",
        all_novel_nodes.len()
    );

    let novel_node = all_novel_nodes[0];
    assert_eq!(novel_node.code, "omw.03833065.n");
    assert_eq!(novel_node.file_count, 3);
    assert_eq!(
        novel_node.files,
        vec!["fp_novel_canonical", "fp_novel_legacy", "fp_novel_loop"]
    );

    assert!(!novel_node.children.iter().any(|c| c.name == "小说"));
}

/// 断言 14【内容标签单传空目录紧凑坍缩 (Zero-Entropy Node Collapse) 与默认模式安全】
#[test]
fn test_content_tags_single_child_collapse_and_default_mode_safety() {
    let pack_conn = Connection::open_in_memory().unwrap();
    pack_conn
        .execute_batch(
            "CREATE TABLE file_tags (
                code TEXT PRIMARY KEY,
                parent_codes TEXT NOT NULL DEFAULT '[]',
                source TEXT NOT NULL DEFAULT 'omw',
                sort_order INTEGER NOT NULL DEFAULT 0
            );
            INSERT INTO file_tags (code, sort_order) VALUES
                ('builtin.file_type', 1),
                ('builtin.content_tags', 28),
                ('omw.furniture.n', 10),
                ('omw.seat.n', 20),
                ('omw.throne.n', 30),
                ('omw.sofa.n', 40);",
        )
        .unwrap();

    let master_conn = Connection::open_in_memory().unwrap();
    master_conn
        .execute_batch(
            "CREATE TABLE workspace_files (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file_fingerprint TEXT NOT NULL UNIQUE,
                path TEXT NOT NULL,
                workspace_id INTEGER NOT NULL DEFAULT 1,
                status INTEGER NOT NULL DEFAULT 1
            );
            -- 模型 B：files.id / tag_paths / ftr(path_id, WITHOUT ROWID) / file_tags_private
            CREATE TABLE files (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file_fingerprint TEXT NOT NULL UNIQUE
            );
            CREATE TABLE tag_paths (
                path_id INTEGER PRIMARY KEY AUTOINCREMENT,
                leaf_code TEXT NOT NULL,
                via_parent_code TEXT NOT NULL DEFAULT '',
                code_path TEXT NOT NULL UNIQUE,
                depth INTEGER NOT NULL DEFAULT 1
            );
            CREATE TABLE file_tag_relations (
                file_fingerprint TEXT NOT NULL,
                tag_code TEXT NOT NULL,
                via_parent_code TEXT NOT NULL DEFAULT '',
                path_id INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (file_fingerprint, tag_code, via_parent_code)
            ) WITHOUT ROWID;
            CREATE TABLE file_tags_private (
                code TEXT PRIMARY KEY,
                name TEXT NOT NULL
            );
            INSERT INTO workspace_files (file_fingerprint, path, workspace_id, status) VALUES
                ('fp_1', 'D:\\files\\throne.jpg', 1, 1);",
        )
        .unwrap();
    insert_ftr_rows_b(
        &master_conn,
        &[(
            "fp_1",
            "omw.throne.n",
            "/builtin.content_tags/omw.furniture.n/omw.seat.n/omw.throne.n",
            "/内容标签/家具/座位/宝座",
        )],
    );

    let resp1 = query_taxonomy_tree_fast(
        &pack_conn,
        "zh-CN",
        None,
        &master_conn,
        false,
        Some(1),
        None,
    )
    .expect("query_taxonomy_tree_fast failed");

    let content_tags_root = resp1
        .root_nodes
        .iter()
        .find(|n| n.code == "builtin.content_tags")
        .expect("内容标签根节点必须存在且保持受控代码");

    assert_eq!(content_tags_root.code, "builtin.content_tags");
    assert_eq!(
        content_tags_root.children.len(),
        1,
        "单文件单传路径必须坍缩为 1 个直接子节点"
    );
    let single_child = &content_tags_root.children[0];
    assert_eq!(single_child.code, "omw.throne.n");
    assert_eq!(single_child.name, "宝座");
    assert_eq!(single_child.file_count, 1, "文件计数必须准确继承");

    master_conn
        .execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path, workspace_id, status) VALUES
                ('fp_2', 'D:\\files\\sofa.jpg', 1, 1);",
        )
        .unwrap();
    insert_ftr_rows_b(
        &master_conn,
        &[(
            "fp_2",
            "omw.sofa.n",
            "/builtin.content_tags/omw.furniture.n/omw.seat.n/omw.sofa.n",
            "/内容标签/家具/座位/沙发",
        )],
    );

    let resp2 = query_taxonomy_tree_fast(
        &pack_conn,
        "zh-CN",
        None,
        &master_conn,
        false,
        Some(1),
        None,
    )
    .expect("query_taxonomy_tree_fast 2nd failed");

    let content_tags_root2 = resp2
        .root_nodes
        .iter()
        .find(|n| n.code == "builtin.content_tags")
        .expect("内容标签根节点必须存在");

    assert_eq!(content_tags_root2.file_count, 2);
    let seat_or_furniture = content_tags_root2
        .children
        .iter()
        .find(|c| c.code == "omw.seat.n" || c.code == "omw.furniture.n")
        .expect("内容标签下必须自动撑开中观聚拢目录（座位或家具）");
    assert_eq!(seat_or_furniture.file_count, 2, "聚拢目录必须包含 2 份文件");
    let has_throne = seat_or_furniture.children.iter().any(|c| {
        c.code == "omw.throne.n" || c.children.iter().any(|gc| gc.code == "omw.throne.n")
    });
    let has_sofa = seat_or_furniture.children.iter().any(|c| {
        c.code == "omw.sofa.n" || c.children.iter().any(|gc| gc.code == "omw.sofa.n")
    });
    assert!(has_throne, "聚拢目录下必须包含宝座分支");
    assert!(has_sofa, "聚拢目录下必须包含沙发分支");
}
}
