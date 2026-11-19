#[cfg(test)]
mod cases {
    use super::super::{insert_ftr_rows_b, setup_model_b_master_conn};
    use crate::routes::taxonomy::*;
    use rusqlite::Connection;

#[test]
fn test_inverted_trie_direct_and_logical_parent_file_count_axiom() {
    let pack_conn = Connection::open_in_memory().unwrap();
    pack_conn
        .execute_batch(
            "CREATE TABLE tag_aliases_zh_CN (
            tag_code TEXT PRIMARY KEY,
            lemma TEXT NOT NULL,
            is_canonical INTEGER NOT NULL DEFAULT 1,
            n INTEGER DEFAULT 0,
            count INTEGER DEFAULT 0
        );
        INSERT INTO tag_aliases_zh_CN (tag_code, lemma, is_canonical) VALUES
        ('builtin.file_type', '文件类型', 1),
        ('builtin.image', '图片', 1),
        ('builtin.censorship', '打码程度', 1),
        ('builtin.uncensored', '无码', 1);",
        )
        .unwrap();

    let master_conn = setup_model_b_master_conn();
    master_conn
        .execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path, workspace_id, status) VALUES
        ('fp_1', 'D:\\files\\img1.jpg', 1, 1),
        ('fp_2', 'D:\\files\\img2.jpg', 1, 1),
        ('fp_3', 'D:\\files\\img3.png', 1, 1),
        ('fp_4', 'D:\\files\\story.txt', 1, 1);",
        )
        .unwrap();
    insert_ftr_rows_b(
        &master_conn,
        &[
            ("fp_1", "builtin.uncensored", "/builtin.file_type/builtin.image/builtin.censorship/builtin.uncensored", "/文件类型/图片/打码程度/无码"),
            ("fp_2", "builtin.uncensored", "/builtin.file_type/builtin.image/builtin.censorship/builtin.uncensored", "/文件类型/图片/打码程度/无码"),
            ("fp_3", "builtin.image", "/builtin.file_type/builtin.image", "/文件类型/图片"),
            ("fp_4", "builtin.novel", "", ""),
        ],
    );

    let resp = query_taxonomy_tree_fast(
        &pack_conn,
        "zh-CN",
        None,
        &master_conn,
        true,
        Some(1),
        None,
    )
    .expect("query_taxonomy_tree_fast failed");

    assert_eq!(resp.locale, "zh-CN");
    let file_type_root = resp
        .root_nodes
        .iter()
        .find(|n| n.code == "builtin.file_type")
        .expect("missing builtin.file_type");
    assert_eq!(file_type_root.name, "文件类型");
    assert_eq!(file_type_root.file_count, 3);
    assert_eq!(file_type_root.files, vec!["fp_1", "fp_2", "fp_3"]);

    let image_node = file_type_root
        .children
        .iter()
        .find(|n| n.code == "builtin.image")
        .expect("missing builtin.image");
    assert_eq!(image_node.file_count, 3);

    let censorship_node = image_node
        .children
        .iter()
        .find(|n| n.code == "builtin.censorship")
        .expect("missing builtin.censorship");
    assert_eq!(censorship_node.file_count, 2);

    let uncensored_node = censorship_node
        .children
        .iter()
        .find(|n| n.code == "builtin.uncensored")
        .expect("missing builtin.uncensored");
    assert_eq!(uncensored_node.file_count, 2);
    assert_eq!(uncensored_node.files, vec!["fp_1", "fp_2"]);

    let content_tags_root = resp
        .root_nodes
        .iter()
        .find(|n| n.code == "builtin.content_tags")
        .expect("missing builtin.content_tags");
    assert_eq!(content_tags_root.file_count, 1);
    assert_eq!(content_tags_root.files, vec!["fp_4"]);
    let novel_child = content_tags_root
        .children
        .iter()
        .find(|n| n.code == "builtin.novel")
        .expect("missing builtin.novel");
    assert_eq!(novel_child.file_count, 1);
    assert_eq!(novel_child.files, vec!["fp_4"]);
}

#[test]
fn test_inverted_trie_scope_filter() {
    let pack_conn = Connection::open_in_memory().unwrap();
    let master_conn = setup_model_b_master_conn();
    master_conn
        .execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path, workspace_id, status) VALUES
        ('fp_a', 'D:\\ws1\\sub\\a.jpg', 1, 1),
        ('fp_b', 'D:\\ws1\\other\\b.jpg', 1, 1),
        ('fp_c', 'D:\\ws2\\sub\\c.jpg', 2, 1);",
        )
        .unwrap();
    insert_ftr_rows_b(
        &master_conn,
        &[
            ("fp_a", "builtin.image", "/builtin.file_type/builtin.image", "/文件类型/图片"),
            ("fp_b", "builtin.image", "/builtin.file_type/builtin.image", "/文件类型/图片"),
            ("fp_c", "builtin.image", "/builtin.file_type/builtin.image", "/文件类型/图片"),
        ],
    );

    let resp = query_taxonomy_tree_fast(
        &pack_conn,
        "zh-CN",
        None,
        &master_conn,
        true,
        Some(1),
        Some("D:\\ws1\\sub"),
    )
    .unwrap();

    let ft_root = resp
        .root_nodes
        .iter()
        .find(|n| n.code == "builtin.file_type")
        .unwrap();
    assert_eq!(ft_root.file_count, 1);
    assert_eq!(ft_root.files, vec!["fp_a"]);
}

#[test]
fn test_inverted_trie_cross_branch_isolation_and_paths() {
    let pack_conn = Connection::open_in_memory().unwrap();
    let master_conn = setup_model_b_master_conn();
    master_conn
        .execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path, workspace_id, status) VALUES
        ('fp_img', 'D:\\ws1\\a.jpg', 1, 1),
        ('fp_vid', 'D:\\ws1\\b.mp4', 1, 1);",
        )
        .unwrap();
    insert_ftr_rows_b(
        &master_conn,
        &[
            ("fp_img", "builtin.uncensored", "/builtin.file_type/builtin.image/builtin.censorship/builtin.uncensored", "/文件类型/图片/打码程度/无码"),
            ("fp_vid", "builtin.uncensored", "/builtin.file_type/builtin.video/builtin.censorship/builtin.uncensored", "/文件类型/视频/打码程度/无码"),
        ],
    );

    let resp = query_taxonomy_tree_fast(
        &pack_conn,
        "zh-CN",
        None,
        &master_conn,
        true,
        Some(1),
        None,
    )
    .unwrap();

    let ft_root = resp
        .root_nodes
        .iter()
        .find(|n| n.code == "builtin.file_type")
        .expect("missing builtin.file_type");
    assert_eq!(ft_root.file_count, 2);
    assert_eq!(ft_root.code_path.as_deref(), Some("/builtin.file_type"));

    let img_node = ft_root
        .children
        .iter()
        .find(|n| n.code == "builtin.image")
        .expect("missing image branch");
    assert_eq!(img_node.file_count, 1);
    assert_eq!(img_node.files, vec!["fp_img"]);
    assert_eq!(img_node.code_path.as_deref(), Some("/builtin.file_type/builtin.image"));

    let vid_node = ft_root
        .children
        .iter()
        .find(|n| n.code == "builtin.video")
        .expect("missing video branch");
    assert_eq!(vid_node.file_count, 1);
    assert_eq!(vid_node.files, vec!["fp_vid"]);
    assert_eq!(vid_node.code_path.as_deref(), Some("/builtin.file_type/builtin.video"));

    let img_censor = img_node.children.iter().find(|n| n.code == "builtin.censorship").unwrap();
    let img_uncensored = img_censor.children.iter().find(|n| n.code == "builtin.uncensored").unwrap();
    assert_eq!(img_uncensored.file_count, 1);
    assert_eq!(img_uncensored.files, vec!["fp_img"]);
    assert_eq!(
        img_uncensored.code_path.as_deref(),
        Some("/builtin.file_type/builtin.image/builtin.censorship/builtin.uncensored")
    );
    assert_eq!(
        img_uncensored.name_path.as_deref(),
        Some("/文件类型/图片/打码程度/无码")
    );

    let vid_censor = vid_node.children.iter().find(|n| n.code == "builtin.censorship").unwrap();
    let vid_uncensored = vid_censor.children.iter().find(|n| n.code == "builtin.uncensored").unwrap();
    assert_eq!(vid_uncensored.file_count, 1);
    assert_eq!(vid_uncensored.files, vec!["fp_vid"]);
    assert_eq!(
        vid_uncensored.code_path.as_deref(),
        Some("/builtin.file_type/builtin.video/builtin.censorship/builtin.uncensored")
    );
    assert_eq!(
        vid_uncensored.name_path.as_deref(),
        Some("/文件类型/视频/打码程度/无码")
    );
}

#[test]
fn test_inverted_trie_healing_and_fact_dimension_independence() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE tag_aliases_zh_CN (
            tag_code TEXT PRIMARY KEY,
            lemma TEXT NOT NULL,
            is_canonical INTEGER NOT NULL DEFAULT 1,
            source TEXT NOT NULL DEFAULT 'builtin',
            count INTEGER NOT NULL DEFAULT 0
        );
        INSERT INTO tag_aliases_zh_CN (tag_code, lemma, is_canonical, source, count) VALUES
            ('builtin.security_level', '安全等级', 1, 'builtin', 10),
            ('omw.01704761.a', '公开', 1, 'omw', 10),
            ('builtin.language_segmentation', '语言细分', 1, 'builtin', 10),
            ('builtin.chinese', '中文', 1, 'builtin', 10),
            ('builtin.content_tags', '内容标签', 1, 'builtin', 10);
        "#,
    )
    .unwrap();

    let master_conn = setup_model_b_master_conn();
    master_conn
        .execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path, workspace_id, status) VALUES
            ('fp_1', 'D:\\files\\fact.txt', 1, 1);",
        )
        .unwrap();
    insert_ftr_rows_b(
        &master_conn,
        &[
            ("fp_1", "omw.01704761.a", "/builtin.content_tags/builtin.security_level/omw.01704761.a", "/内容标签/安全等级/公开"),
            ("fp_1", "builtin.chinese", "/builtin.content_tags/builtin.language_segmentation/builtin.chinese", "/内容标签/语言细分/中文"),
            ("fp_1", "_ext.he.9ec86979", "/builtin.content_tags/_ext.he.9ec86979", "/内容标签/合歆"),
        ],
    );

    let resp = query_taxonomy_tree_fast(&conn, "zh-CN", None, &master_conn, false, None, None).unwrap();

    assert!(!resp.root_nodes.iter().any(|n| n.code == "builtin.security_level"), "安全等级严禁独立建根");
    let purpose_root = resp.root_nodes.iter().find(|n| n.code == "builtin.file_purpose");
    assert!(purpose_root.is_some(), "文件用途必须存在");
    let purpose_root = purpose_root.unwrap();
    let work_doc_node = purpose_root.children.iter().find(|c| c.code == "builtin.work_documents");
    assert!(work_doc_node.is_some(), "工作文档必须存在于文件用途下");
    let work_doc_node = work_doc_node.unwrap();
    let sec_node = work_doc_node.children.iter().find(|c| c.code == "builtin.security_level");
    assert!(sec_node.is_some(), "安全等级必须挂载在工作文档下");
    let sec_node = sec_node.unwrap();
    assert_eq!(
        sec_node.code_path.as_deref(),
        Some("/builtin.file_purpose/builtin.work_documents/builtin.security_level")
    );
    assert_eq!(sec_node.children.len(), 1);
    assert_eq!(sec_node.children[0].code, "omw.01704761.a");
    assert_eq!(
        sec_node.children[0].code_path.as_deref(),
        Some("/builtin.file_purpose/builtin.work_documents/builtin.security_level/omw.01704761.a")
    );

    assert!(
        !resp.root_nodes.iter().any(|n| n.code == "builtin.language_segmentation"),
        "语言细分严禁独立建根"
    );
    let type_root = resp.root_nodes.iter().find(|n| n.code == "builtin.file_type");
    assert!(type_root.is_some(), "文件类型必须存在");
    let type_root = type_root.unwrap();
    let doc_node = type_root.children.iter().find(|c| c.code == "builtin.document");
    assert!(doc_node.is_some(), "文档必须存在于文件类型下");
    let doc_node = doc_node.unwrap();
    let lang_node = doc_node.children.iter().find(|c| c.code == "builtin.language_segmentation");
    assert!(lang_node.is_some(), "语言细分必须挂载在文档下");
    let lang_node = lang_node.unwrap();
    assert_eq!(
        lang_node.code_path.as_deref(),
        Some("/builtin.file_type/builtin.document/builtin.language_segmentation")
    );
    assert_eq!(lang_node.children.len(), 1);
    assert_eq!(lang_node.children[0].code, "builtin.chinese");
    assert_eq!(
        lang_node.children[0].code_path.as_deref(),
        Some("/builtin.file_type/builtin.document/builtin.language_segmentation/builtin.chinese")
    );

    let content_root = resp.root_nodes.iter().find(|n| n.code == "builtin.content_tags");
    assert!(content_root.is_some());
    let content_root = content_root.unwrap();
    assert!(content_root
        .children
        .iter()
        .all(|c| c.code != "builtin.security_level" && c.code != "builtin.language_segmentation"));
    assert!(content_root.children.iter().any(|c| c.code == "_ext.he.9ec86979"));
}

#[test]
fn test_fast_path_backfills_pack_sort_order_for_roots_and_children() {
    let pack_conn = Connection::open_in_memory().unwrap();
    pack_conn
        .execute_batch(
            "CREATE TABLE file_tags (
                code TEXT PRIMARY KEY,
                parent_codes TEXT NOT NULL DEFAULT '[]',
                source TEXT NOT NULL DEFAULT 'dimension',
                sort_order INTEGER NOT NULL DEFAULT 0
            );
            INSERT INTO file_tags (code, sort_order) VALUES
                ('builtin.file_type', 1),
                ('hownet.000000043396.n', 4),
                ('builtin.content_tags', 28),
                ('builtin.novel', 1),
                ('builtin.image', 5),
                ('builtin.zhangsan', 3);",
        )
        .unwrap();

    let master_conn = setup_model_b_master_conn();
    master_conn
        .execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path, workspace_id, status) VALUES
            ('fp_1', 'D:\\\\files\\\\a.txt', 1, 1),
            ('fp_2', 'D:\\\\files\\\\b.txt', 1, 1),
            ('fp_3', 'D:\\\\files\\\\c.txt', 1, 1),
            ('fp_4', 'D:\\\\files\\\\d.txt', 1, 1),
            ('fp_5', 'D:\\\\files\\\\e.txt', 1, 1);",
        )
        .unwrap();
    insert_ftr_rows_b(
        &master_conn,
        &[
            ("fp_1", "builtin.image", "/builtin.file_type/builtin.image", "/文件类型/图片"),
            ("fp_2", "builtin.novel", "/builtin.file_type/builtin.novel", "/文件类型/小说"),
            ("fp_3", "builtin.article", "/builtin.file_type/builtin.article", "/文件类型/文章"),
            ("fp_4", "builtin.zhangsan", "/hownet.000000043396.n/builtin.zhangsan", "/作者/张三"),
            ("fp_5", "builtin.foo", "/builtin.content_tags/builtin.foo", "/内容标签/未知"),
        ],
    );

    let resp = query_taxonomy_tree_fast(
        &pack_conn,
        "zh-CN",
        None,
        &master_conn,
        false,
        Some(1),
        None,
    )
    .expect("query_taxonomy_tree_fast failed");

    let root_codes: Vec<&str> = resp.root_nodes.iter().map(|n| n.code.as_str()).collect();
    assert_eq!(
        root_codes,
        vec!["builtin.file_type", "hownet.000000043396.n", "builtin.content_tags"]
    );

    assert_eq!(resp.root_nodes[0].sort_order, 1);
    assert_eq!(resp.root_nodes[1].sort_order, 4);
    assert_eq!(resp.root_nodes[2].sort_order, 9999);

    let file_type_root = resp
        .root_nodes
        .iter()
        .find(|n| n.code == "builtin.file_type")
        .expect("missing builtin.file_type");
    let child_codes: Vec<&str> = file_type_root.children.iter().map(|n| n.code.as_str()).collect();
    assert_eq!(child_codes, vec!["builtin.novel", "builtin.image", "builtin.article"]);
    assert_eq!(file_type_root.children[0].sort_order, 1);
    assert_eq!(file_type_root.children[1].sort_order, 5);
    assert_eq!(file_type_root.children[2].sort_order, 0);
}

#[test]
fn test_fast_path_without_pack_file_tags_keeps_hardcoded_sort_order() {
    let pack_conn = Connection::open_in_memory().unwrap();
    let master_conn = setup_model_b_master_conn();
    master_conn
        .execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path, workspace_id, status) VALUES
            ('fp_1', 'D:\\\\files\\\\a.txt', 1, 1),
            ('fp_2', 'D:\\\\files\\\\b.txt', 1, 1);",
        )
        .unwrap();
    insert_ftr_rows_b(
        &master_conn,
        &[
            ("fp_1", "builtin.image", "/builtin.file_type/builtin.image", "/文件类型/图片"),
            ("fp_2", "builtin.foo", "/builtin.content_tags/builtin.foo", "/内容标签/未知"),
        ],
    );

    let resp = query_taxonomy_tree_fast(
        &pack_conn,
        "zh-CN",
        None,
        &master_conn,
        false,
        Some(1),
        None,
    )
    .expect("query_taxonomy_tree_fast failed");

    let file_type_root = resp
        .root_nodes
        .iter()
        .find(|n| n.code == "builtin.file_type")
        .expect("missing builtin.file_type");
    assert_eq!(file_type_root.sort_order, 1);

    let content_root = resp
        .root_nodes
        .iter()
        .find(|n| n.code == "builtin.content_tags")
        .expect("missing builtin.content_tags");
    assert_eq!(content_root.sort_order, 9999);

    assert_eq!(file_type_root.children.len(), 1);
    assert_eq!(file_type_root.children[0].sort_order, 0);
}

#[test]
fn test_inverted_trie_healing_pseudo_content_tags() {
    let pack_conn = Connection::open_in_memory().unwrap();
    let master_conn = setup_model_b_master_conn();
    master_conn
        .execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path, workspace_id, status) VALUES
            ('fp_cam', 'D:\\\\files\\\\cam.jpg', 1, 1),
            ('fp_txt', 'D:\\\\files\\\\txt.png', 1, 1);",
        )
        .unwrap();
    insert_ftr_rows_b(
        &master_conn,
        &[
            ("fp_cam", "_ext.camera_canon", "/builtin.content_tags/builtin.photography_categories/_ext.camera_canon", "/内容标签/摄影照片细分/Canon EOS 5D Mark II"),
            ("fp_txt", "builtin.tiny_text", "/builtin.content_tags/builtin.text_density/builtin.tiny_text", "/内容标签/文字密度/微量文本"),
        ],
    );

    let resp = query_taxonomy_tree_fast(
        &pack_conn,
        "zh-CN",
        None,
        &master_conn,
        false,
        Some(1),
        None,
    )
    .expect("query_taxonomy_tree_fast failed");

    if let Some(content_root) = resp.root_nodes.iter().find(|n| n.code == "builtin.content_tags") {
        assert!(!content_root
            .children
            .iter()
            .any(|c| c.code == "builtin.photo_categories" || c.code == "builtin.photography_categories"));
        assert!(!content_root.children.iter().any(|c| c.code == "builtin.text_density"));
    }

    let file_type_root = resp
        .root_nodes
        .iter()
        .find(|n| n.code == "builtin.file_type")
        .expect("missing builtin.file_type");

    let has_healed_subdim = file_type_root.children.iter().any(|c| {
        c.code == "builtin.photo_categories"
            || c.code == "builtin.photography_categories"
            || c.code == "builtin.text_density"
    });
    assert!(has_healed_subdim, "摄影照片细分或文字密度必须成功自愈至文件类型根节点下");
}
}
