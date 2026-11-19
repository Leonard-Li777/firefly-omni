//! taxonomy 模块（工单08 · 四缝合一）：分类树与多语言别名检索端点。
//!
//! 原 `taxonomy.rs`（4352 行单文件）已按职责拆分为 8 个子模块并退位删除。
//! 本 mod.rs 通过 re-export 保持调用方 `crate::routes::taxonomy::*` / `routes::taxonomy::*` 路径不变（零改动）。
//!
//! 子模块职责：
//! - `connection`：Seam A — 只读连接唯一入口（busy_timeout(5000) + PRAGMA 收口）。
//! - `tree_fast`：Seam B — 倒排 Trie 极速建树（RoaringBitmap / tag_paths 改造落点）。
//! - `tree_query`：传统分类树查询。
//! - `alias_query`：多语言别名批量检索。
//! - `inspect`：标签检视 + 维度策略加载。
//! - `handlers`：axum 端点处理函数。
//! - `types`：请求/响应/内部数据结构。
//! - `utils`：通用工具函数。

pub mod alias_query;
pub mod connection;
pub mod handlers;
pub mod inspect;
pub mod tree_fast;
pub mod tree_query;
pub mod types;
pub mod utils;

// —— 路径兼容 re-export（保持旧 `taxonomy::X` 调用方零改动）——

// 端点处理函数
pub use handlers::{
    fast_recognize_handler, taxonomy_aliases_handler, taxonomy_inspect_tag_handler,
    taxonomy_tree_handler,
};
pub use inspect::{load_dimension_policies_from_db, reload_policies_handler};

// 连接治理（Seam A）
pub use connection::{apply_readonly_pragmas, open_omni_readonly_connection};

// 树查询（传统 + fast）
pub use tree_fast::query_taxonomy_tree_fast;
pub use tree_query::{query_canonical_and_aliases, query_taxonomy_tree};
pub use alias_query::query_taxonomy_aliases;
pub use inspect::{inspect_single_tag, inspect_tags_batch};

// 类型（请求/响应/内部）
pub use types::{
    InspectTagBatchRequest, InspectTagBatchResponse, InspectTagItem, InspectTagItemResult,
    NodeEvaluationContext, NodeSemanticMeta, TagAliasRow, TaxonomyAliasesQuery,
    TaxonomyAliasesResponse, TaxonomyFoldPolicy, TaxonomyNode, TaxonomyTreeQuery,
    TaxonomyTreeResponse,
};
