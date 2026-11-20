//! 工单 09（V4 Slice 5 收尾）：10 万文件建树压测自动化基准（融合方案 §8 验收）。
//!
//! 真实数据规模（10 万 workspace_files + 模型 B file_tag_relations），
//! 全部走真实 `query_taxonomy_tree_fast` 建树链路，严禁桩代码/伪造数据面。
//! 墙钟耗时随 println! 输出（`-- --nocapture` 可见），并断言 generous 预算防 CI 抖动。
//!
//! 运行（禁裸跑 cargo，必须走 cargo-runner）：
//!   node scripts/cargo-runner.js --task=test-dev test -p omni-server --lib bench_100k -- --nocapture

#[cfg(test)]
mod cases {
    use super::super::{insert_ftr_rows_b, setup_test_master_and_pack_conns};
    use crate::routes::taxonomy::*;
    use std::time::Instant;

    /// 10 万文件 × 500 条物化路径（每路径 200 文件，索引不重叠全覆盖）。
    /// SQL 绑定用 i64，建树计数断言用 usize（TaxonomyTreeNode.file_count 为 usize）。
    const TOTAL_FILES: i64 = 100_000;
    const TOTAL_FILES_USIZE: usize = 100_000;
    const DISTINCT_PATHS: i64 = 500;
    /// 建树预算（ms）：generous 防抖，防 CI 机器抖动误杀；真实耗时经 println! 留档。
    const TREE_BUILD_BUDGET_MS: u128 = 8_000;

    #[test]
    fn bench_taxonomy_tree_fast_100k_files_model_b() {
        let (pack_conn, master_conn) = setup_test_master_and_pack_conns();

        // pack 侧语义包标签 + 中文别名（真实建树展示名回译链路）
        pack_conn
            .execute_batch(
                "INSERT INTO file_tags (code, pos, sememe_category) VALUES
                    ('builtin.category.bench', 'n', 1);",
            )
            .unwrap();
        pack_conn
            .execute_batch(
                "INSERT INTO tag_aliases_zh_CN (tag_code, lemma) VALUES ('builtin.category.bench', '基准分类');",
            )
            .unwrap();

        // 主库：10 万 workspace_files 单事务灌入
        let t_seed = Instant::now();
        {
            let tx = master_conn.unchecked_transaction().unwrap();
            for i in 0..TOTAL_FILES {
                tx.execute(
                    "INSERT INTO workspace_files (file_fingerprint, path) VALUES (?1, ?2)",
                    rusqlite::params![format!("fp_bench_{}", i), format!("D:\\bench\\file-{}.txt", i)],
                )
                .unwrap();
            }
            tx.commit().unwrap();
        }

        // 500 条物化路径 × 每路径 200 文件 = 10 万关联（单事务，真实 ensureTagPath 夹具链路）
        {
            let per_path = (TOTAL_FILES / DISTINCT_PATHS) as usize;
            for p in 0..DISTINCT_PATHS {
                let tag_code = format!("bench.tag.{}", p);
                let code_path = format!("/builtin.category.bench/bench.tag.{}", p);
                let rows: Vec<(String, &str, &str, &str)> = (0..per_path)
                    .map(|k| {
                        (
                            format!("fp_bench_{}", (p as usize) * per_path + k),
                            tag_code.as_str(),
                            code_path.as_str(),
                            "/基准分类",
                        )
                    })
                    .collect();
                let ref_rows: Vec<(&str, &str, &str, &str)> =
                    rows.iter().map(|(a, b, c, d)| (a.as_str(), *b, *c, *d)).collect();
                insert_ftr_rows_b(&master_conn, &ref_rows);
            }
        }
        let seed_ms = t_seed.elapsed().as_millis();

        // 关联完整性自检：10 万文件全部有标签关联
        let tagged: i64 = master_conn
            .query_row(
                "SELECT COUNT(DISTINCT file_fingerprint) FROM file_tag_relations",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tagged, TOTAL_FILES, "10 万文件必须全部有标签关联");

        // 真实建树压测（RoaringBitmap 快路径）
        let t_tree = Instant::now();
        let resp = query_taxonomy_tree_fast(&pack_conn, "zh-CN", None, &master_conn, true, None, None)
            .expect("10 万文件建树必须成功（模型 B 唯一路径）");
        let tree_ms = t_tree.elapsed().as_millis();

        // 正确性断言：根节点唯一 + 总文件计数 10 万
        assert_eq!(resp.root_nodes.len(), 1, "根节点应收敛为 builtin.content_tags 单根");
        assert_eq!(resp.root_nodes[0].code, "builtin.content_tags");
        assert_eq!(
            resp.root_nodes[0].file_count, TOTAL_FILES_USIZE,
            "建树文件计数必须等于 10 万（RoaringBitmap 去重后）"
        );

        println!(
            "[bench-100k] seed={}ms tree={}ms budget={}ms files={} paths={}",
            seed_ms, tree_ms, TREE_BUILD_BUDGET_MS, TOTAL_FILES, DISTINCT_PATHS
        );
        assert!(
            tree_ms < TREE_BUILD_BUDGET_MS,
            "10 万文件建树耗时 {}ms 超出预算 {}ms",
            tree_ms,
            TREE_BUILD_BUDGET_MS
        );
    }

    /// 连接治理回归：10 万行规模库上应用 Rust 侧只读 PRAGMA 套装（Seam A 权威实现）后查询路径仍可用。
    #[test]
    fn bench_readonly_pragmas_survive_100k_query() {
        let (pack_conn, master_conn) = setup_test_master_and_pack_conns();
        {
            let tx = master_conn.unchecked_transaction().unwrap();
            for i in 0..TOTAL_FILES {
                tx.execute(
                    "INSERT INTO workspace_files (file_fingerprint, path) VALUES (?1, ?2)",
                    rusqlite::params![format!("fp_ro_{}", i), format!("D:\\ro\\file-{}.txt", i)],
                )
                .unwrap();
            }
            tx.commit().unwrap();
        }
        pack_conn
            .execute_batch("INSERT INTO file_tags (code, pos) VALUES ('ro.tag.0', 'n');")
            .unwrap();

        // 全部文件挂同一路径（step_by 均匀抽样索引 0,200,400,...）
        let per_path = (TOTAL_FILES / DISTINCT_PATHS) as usize;
        let rows: Vec<(String, &str, &str, &str)> = (0..TOTAL_FILES)
            .step_by(per_path)
            .map(|i| (format!("fp_ro_{}", i), "ro.tag.0", "/ro.tag.0", "/只读"))
            .collect();
        let ref_rows: Vec<(&str, &str, &str, &str)> =
            rows.iter().map(|(a, b, c, d)| (a.as_str(), *b, *c, *d)).collect();
        insert_ftr_rows_b(&master_conn, &ref_rows);

        // 只读 PRAGMA 套装（omni-core db_sqlite.rs 权威实现）应用后查询仍可用
        omni_core::db_sqlite::apply_readonly_pragmas(&master_conn).unwrap();
        let resp = query_taxonomy_tree_fast(&pack_conn, "zh-CN", None, &master_conn, true, None, None)
            .expect("只读 PRAGMA 套装下建树必须可用（Seam A）");
        assert!(resp.root_nodes[0].file_count > 0);
    }
}
