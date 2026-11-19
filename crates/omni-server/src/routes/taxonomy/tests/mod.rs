use rusqlite::Connection;

mod ac_tree_tests;
mod policy_tests;
mod trie_healing_tests;

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

    let master_conn = Connection::open_in_memory().unwrap();
    master_conn
        .execute_batch(
            "CREATE TABLE workspace_files (
                file_fingerprint TEXT PRIMARY KEY,
                path TEXT NOT NULL,
                workspace_id INTEGER NOT NULL DEFAULT 1,
                status INTEGER NOT NULL DEFAULT 1
            );
            CREATE TABLE file_tag_relations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file_fingerprint TEXT NOT NULL,
                tag_code TEXT NOT NULL,
                code_path TEXT,
                name_path TEXT
            );",
        )
        .unwrap();

    (pack_conn, master_conn)
}
