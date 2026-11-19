use rusqlite::Connection;

mod ac_tree_tests;
mod policy_tests;
mod trie_healing_tests;

/// 辅助构建模型 B 业务主库连接（files.id / tag_paths / ftr(path_id, WITHOUT ROWID) / file_tags_private）。
pub(super) fn setup_model_b_master_conn() -> Connection {
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
            -- 模型 B（对齐 Spec §3.1.B）：files.id 代理主键、tag_paths 路径字典、ftr 仅存 path_id + WITHOUT ROWID
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
                tag_group TEXT NOT NULL DEFAULT '',
                confidence REAL NOT NULL DEFAULT 1.0,
                source TEXT NOT NULL DEFAULT 'ai',
                sync_status INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                meta TEXT NOT NULL DEFAULT '{}',
                PRIMARY KEY (file_fingerprint, tag_code, via_parent_code)
            ) WITHOUT ROWID;
            -- 模型 B 无 ftr.name_path：_ext.*/user.* 等展示名由 file_tags_private 实时回译
            CREATE TABLE file_tags_private (
                code TEXT PRIMARY KEY,
                name TEXT NOT NULL
            );",
        )
        .unwrap();
    master_conn
}

/// 辅助构建模拟主库与语义库连接
pub(super) fn setup_test_master_and_pack_conns() -> (Connection, Connection) {
    let pack_conn = Connection::open_in_memory().unwrap();
    pack_conn
        .execute_batch(
            "CREATE TABLE file_tags (
                code TEXT PRIMARY KEY,
                sort_order INTEGER NOT NULL DEFAULT 0,
                pos TEXT,
                lexfile TEXT,
                sememe_category INTEGER
            );
            CREATE TABLE tag_aliases_zh_CN (
                tag_code TEXT NOT NULL,
                lemma TEXT NOT NULL,
                is_canonical INTEGER NOT NULL DEFAULT 1,
                n INTEGER DEFAULT 0,
                count INTEGER DEFAULT 0,
                PRIMARY KEY (tag_code, lemma)
            );",
        )
        .unwrap();

    (pack_conn, setup_model_b_master_conn())
}

/// 测试夹具：按模型 B 写入 file_tag_relations 关联行。
/// 每条关系先插/复用 tag_paths 拿到 path_id，再写 ftr（WITHOUT ROWID，无 code_path/name_path/depth 列）；
/// name_path 的每段译名落入 file_tags_private，供建树时按 code_path 段序实时回译展示名，保持测试语义不变。
pub(super) fn insert_ftr_rows_b(
    conn: &Connection,
    rows: &[(&str, &str, &str, &str)], // (file_fingerprint, tag_code, code_path, name_path)
) {
    let tx = conn.unchecked_transaction().unwrap();
    for &(fp, tag_code, code_path, name_path) in rows {
        tx.execute(
            "INSERT OR IGNORE INTO files (file_fingerprint) VALUES (?1)",
            [fp],
        )
        .unwrap();

        // 模型 B 的 tag_paths.code_path 不允许空串；空路径沿用建树侧的悲观兜底 /{tag_code}
        let effective_code_path = if code_path.trim().is_empty() {
            format!("/{}", tag_code)
        } else {
            code_path.to_string()
        };
        let depth = effective_code_path.split('/').filter(|s| !s.is_empty()).count() as i64;
        tx.execute(
            "INSERT OR IGNORE INTO tag_paths (leaf_code, via_parent_code, code_path, depth) VALUES (?1, '', ?2, ?3)",
            rusqlite::params![tag_code, effective_code_path, depth],
        )
        .unwrap();
        let path_id: i64 = tx
            .query_row(
                "SELECT path_id FROM tag_paths WHERE code_path = ?1",
                [&effective_code_path],
                |r| r.get(0),
            )
            .unwrap();

        // code_path 段序对齐 name_path 段序，写入展示名（不依赖已删除的 ftr.name_path）
        let code_segs: Vec<&str> = effective_code_path.split('/').filter(|s| !s.is_empty()).collect();
        let name_segs: Vec<&str> = name_path.split('/').filter(|s| !s.is_empty()).collect();
        for (i, seg) in code_segs.iter().enumerate() {
            if let Some(name) = name_segs.get(i) {
                if !name.is_empty() && *name != *seg {
                    tx.execute(
                        "INSERT OR REPLACE INTO file_tags_private (code, name) VALUES (?1, ?2)",
                        rusqlite::params![seg, name],
                    )
                    .unwrap();
                }
            }
        }

        tx.execute(
            "INSERT OR REPLACE INTO file_tag_relations \
             (file_fingerprint, tag_code, via_parent_code, path_id, created_at, updated_at) \
             VALUES (?1, ?2, '', ?3, 0, 0)",
            rusqlite::params![fp, tag_code, path_id],
        )
        .unwrap();
    }
    tx.commit().unwrap();
}
