use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 分类树查询参数
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaxonomyTreeQuery {
    pub locale: Option<String>,
    pub root: Option<String>,
    #[serde(alias = "db_path")]
    pub db_path: Option<String>,
    #[serde(alias = "include_files")]
    pub include_files: Option<bool>,
    #[serde(alias = "workspace_id")]
    pub workspace_id: Option<i64>,
    #[serde(alias = "directory_prefix")]
    pub directory_prefix: Option<String>,
}

/// 分类树多叉树节点
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TaxonomyNode {
    pub code: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_code: Option<String>,
    pub parent_codes: Vec<String>,
    pub source: String,
    pub sort_order: i64,
    #[serde(default)]
    pub file_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    pub children: Vec<TaxonomyNode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_path: Option<String>,
}

/// 分类树响应结构体
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TaxonomyTreeResponse {
    pub locale: String,
    pub root_nodes: Vec<TaxonomyNode>,
    pub total_nodes: usize,
}

/// 多语言别名查询参数
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaxonomyAliasesQuery {
    pub locale: Option<String>,
    pub source: Option<String>,
    pub codes: Option<String>,
    pub prefix: Option<String>,
}

/// 批量别名响应结构体（map 形式）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TaxonomyAliasesResponse {
    pub locale: String,
    pub aliases: HashMap<String, Vec<String>>,
    pub canonical_names: HashMap<String, String>,
    pub total_tags: usize,
}

/// 别名行数组
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct TagAliasRow {
    pub tag_code: String,
    pub lemma: String,
    pub is_canonical: i64,
    pub n: i64,
    pub count: i64,
}

/// 单项待鉴别标签请求
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InspectTagItem {
    pub tag_name: String,
    pub target_parent_code: String,
}

/// 批量鉴别请求载荷
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InspectTagBatchRequest {
    pub items: Vec<InspectTagItem>,
    pub language: Option<String>,
}

/// 单项鉴别输出结果
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InspectTagItemResult {
    pub tag_name: String,
    pub is_controlled: bool,
    pub matched_parent: bool,
    pub code: Option<String>,
    pub canonical_name: Option<String>,
    pub parent_codes: Vec<String>,
    pub ancestry_matched_code: Option<String>,
}

/// 批量鉴别响应载荷
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InspectTagBatchResponse {
    pub success: bool,
    pub results: Vec<InspectTagItemResult>,
}

/// 树节点折叠与过滤策略
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaxonomyFoldPolicy {
    Keep,
    BypassAndHoist,
    FlattenToActionRoot,
    RejectSubtree,
}

/// 语义节点轻量判定元数据
#[derive(Debug, Clone)]
pub struct NodeEvaluationContext<'a> {
    pub code: &'a str,
    pub pos: Option<&'a str>,
    pub lexfile: Option<&'a str>,
    pub sememe_category: Option<u8>,
    pub is_root: bool,
    pub in_action_branch: bool,
}

/// 节点语义元数据
#[derive(Debug, Clone, Default)]
pub struct NodeSemanticMeta {
    pub sort_order: i64,
    pub pos: Option<String>,
    pub lexfile: Option<String>,
    pub sememe_category: Option<u8>,
}

/// POST /api/taxonomy/reload-policies 端点响应体
/// （工单08 拆分补全：原单文件 taxonomy.rs 的该类型随 reload_policies_handler 一并迁入 inspect.rs，
/// 但类型本体缺失导致编译断链，此处补回。）
#[derive(Debug, Serialize)]
pub struct ReloadPoliciesResponse {
    pub success: bool,
    pub count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
