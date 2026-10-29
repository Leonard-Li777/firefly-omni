//! taxonomy.rs — 分类树与多语言别名批量检索端点
//!
//! 依据 ADR-0038 与 PRD #679 架构决议：
//! - 承接零磁盘落地只读语义包 (semantic.pack) 内存只读挂载成果；
//! - GET /api/v1/taxonomy/tree: 返回聚合后的多语言受控标签分类树 (TreeNode 多叉树)；
//! - GET /api/v1/taxonomy/aliases: 批量拉取多语言别名对照映射表。

use axum::{
    extract::{Query, State},
    Json,
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::AppState;

/// 分类树查询参数
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaxonomyTreeQuery {
    /// 语言区域标识符，如 zh-CN, zh, en-US, en。缺省 zh-CN
    pub locale: Option<String>,
    /// 可选指定根节点 code (例如 builtin.domain)；缺省返回全部顶层根节点
    pub root: Option<String>,
    /// 可选指定桌面端 SQLite 业务主库路径（只读访问 file_tag_relations 等主库表）
    #[serde(alias = "db_path")]
    pub db_path: Option<String>,
    /// 是否在响应中包含各节点的完整文件指纹列表（默认 false，节省带宽与序列化耗时）
    #[serde(alias = "include_files")]
    pub include_files: Option<bool>,
    /// 可选指定所属工作区ID过滤（作用域感知建树）
    #[serde(alias = "workspace_id")]
    pub workspace_id: Option<i64>,
    /// 可选指定目录物理路径前缀过滤（作用域感知建树）
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
/// 依据 tag-aliases-lang-tables 设计文档（C2=(c) 双参并存）：
/// - `source`：初值镜像，逗号分隔的受控 source 列表（如 `tag,dimension`），仅返回 file_tags.source 命中的受控 code 行；
/// - `codes`：迁移补漏，逗号分隔的精确 tag_code 列表，未命中 code 不出行；
/// - `prefix`：兼容保留的旧前缀过滤（`builtin` / `omw`）。
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaxonomyAliasesQuery {
    /// 语言区域代码，如 zh-CN, zh, en 等。若缺省则回退到 Omni 配置语言或默认 zh-CN
    pub locale: Option<String>,
    /// 初值镜像：逗号分隔的受控 source 列表（如 `tag,dimension`），仅返回 file_tags.source 命中的行
    pub source: Option<String>,
    /// 迁移补漏：逗号分隔的精确 tag_code 列表，未命中 code 不出行
    pub codes: Option<String>,
    /// 标签前缀过滤（如 "builtin"），若指定则仅返回符合前缀的标签别名
    pub prefix: Option<String>,
}

/// 批量别名响应结构体（map 形式）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TaxonomyAliasesResponse {
    pub locale: String,
    /// tag_code -> 别名候选列表 (按规范名优先排序)
    pub aliases: HashMap<String, Vec<String>>,
    /// tag_code -> 唯一规范展示名 (Canonical Lemma)
    pub canonical_names: HashMap<String, String>,
    pub total_tags: usize,
}

/// 别名行数组（对应主库 tag_aliases_{lang} 分表逐列，snake_case 与语义包一致）
/// 设计文档：响应直接返回 `[{tag_code, lemma, is_canonical, n, count}]`，无需 map 转化。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct TagAliasRow {
    /// 受控 code（builtin.* / omw.*）
    pub tag_code: String,
    /// 该语言词形
    pub lemma: String,
    /// 是否规范名
    pub is_canonical: i64,
    /// 语义包词频（镜像字段）
    pub n: i64,
    /// 语义包文档数（镜像字段）
    pub count: i64,
}

/// 解析逗号分隔参数为去空格非空列表
fn parse_csv_param(raw: Option<&str>) -> Vec<String> {
    raw.map(|s| {
        s.split(',')
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

/// 获取分类树：GET /api/v1/taxonomy/tree?locale={locale}&root={root}
pub async fn taxonomy_tree_handler(
    State(state): State<AppState>,
    Query(query): Query<TaxonomyTreeQuery>,
) -> Json<TaxonomyTreeResponse> {
    let locale = query.locale.unwrap_or_else(|| "zh-CN".to_string());
    let root_filter = query.root;
    let explicit_db_path = query.db_path.map(std::path::PathBuf::from);
    let master_db_path = explicit_db_path.or_else(|| {
        state.master_db_path.lock().ok().and_then(|opt| opt.clone())
    });
    let include_files = query.include_files.unwrap_or(false);
    let workspace_id = query.workspace_id;
    let directory_prefix = query.directory_prefix;

    let res = state.omw.with_conn(|conn| {
        query_taxonomy_tree(
            conn,
            &locale,
            root_filter.as_deref(),
            master_db_path.as_deref(),
            include_files,
            workspace_id,
            directory_prefix.as_deref(),
        )
    });

    match res {
        Ok(tree) => Json(tree),
        Err(err) => {
            tracing::warn!("[taxonomy_tree_handler] 查询分类树异常或未就绪: {err}");
            Json(TaxonomyTreeResponse {
                locale,
                root_nodes: Vec::new(),
                total_nodes: 0,
            })
        }
    }
}

/// 获取多语言别名字典：GET /api/v1/taxonomy/aliases?locale={locale}&source={source}&codes={codes}
/// 依据 tag-aliases-lang-tables 设计文档：返回 tag_aliases_{lang} 行数组（与主库分表逐列一致）。
/// - `source`（如 tag,dimension）初值镜像受控全集，仅返回 file_tags.source 命中的 code；
/// - `codes`（逗号分隔精确 code 列表）迁移补漏，未命中 code 不出行。
pub async fn taxonomy_aliases_handler(
    State(state): State<AppState>,
    Query(query): Query<TaxonomyAliasesQuery>,
) -> Json<Vec<TagAliasRow>> {
    let locale = query
        .locale
        .or_else(|| state.config.lock().ok().and_then(|c| c.language.clone()))
        .unwrap_or_else(|| "zh-CN".to_string());
    let source = query.source.clone();
    let codes = query.codes.clone();
    let prefix = query.prefix.clone();

    let res = state.omw.with_conn(|conn| {
        query_tag_alias_rows(
            conn,
            &locale,
            source.as_deref(),
            codes.as_deref(),
            prefix.as_deref(),
        )
    });

    match res {
        Ok(rows) => Json(rows),
        Err(err) => {
            tracing::warn!("[taxonomy_aliases_handler] 查询别名映射异常或未就绪: {err}");
            Json(Vec::new())
        }
    }
}

/// 单项待鉴别标签请求
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InspectTagItem {
    pub tag_name: String,
    pub target_parent_code: String,
}

/// 批量鉴别请求载荷: POST /api/v1/taxonomy/inspect-tag
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

/// 受控标签鉴别与祖先闭包判定端点: POST /api/v1/taxonomy/inspect-tag
pub async fn taxonomy_inspect_tag_handler(
    State(state): State<AppState>,
    Json(req): Json<InspectTagBatchRequest>,
) -> Json<InspectTagBatchResponse> {
    let locale = req
        .language
        .or_else(|| state.config.lock().ok().and_then(|c| c.language.clone()))
        .unwrap_or_else(|| "zh-CN".to_string());
    let items = req.items;

    let res = state.omw.with_conn(move |conn| {
        inspect_tags_batch(conn, &items, &locale)
    });

    match res {
        Ok(results) => Json(InspectTagBatchResponse {
            success: true,
            results,
        }),
        Err(err) => {
            tracing::warn!("[taxonomy_inspect_tag_handler] 鉴别标签异常: {err}");
            Json(InspectTagBatchResponse {
                success: false,
                results: Vec::new(),
            })
        }
    }
}

/// 对单项标签在只读语义包 (semantic.pack) 中进行受控性与祖先继承链匹配检验
pub fn inspect_single_tag(
    conn: &Connection,
    tag_name: &str,
    target_parent_code: &str,
    locale: &str,
) -> InspectTagItemResult {
    let target_table = omni_pro::resolve_tag_aliases_table(locale);
    let has_alias_table = table_exists(conn, target_table);
    let has_file_tags = table_exists(conn, "file_tags");
    inspect_single_tag_internal(conn, tag_name, target_parent_code, target_table, has_alias_table, has_file_tags)
}

fn inspect_single_tag_internal(
    conn: &Connection,
    tag_name: &str,
    target_parent_code: &str,
    target_table: &str,
    has_alias_table: bool,
    has_file_tags: bool,
) -> InspectTagItemResult {
    let clean_name = tag_name.trim();
    let clean_parent = target_parent_code.trim();

    if clean_name.is_empty() {
        return InspectTagItemResult {
            tag_name: tag_name.to_string(),
            is_controlled: false,
            matched_parent: false,
            code: None,
            canonical_name: None,
            parent_codes: Vec::new(),
            ancestry_matched_code: None,
        };
    }

    if !has_file_tags {
        return InspectTagItemResult {
            tag_name: tag_name.to_string(),
            is_controlled: false,
            matched_parent: false,
            code: None,
            canonical_name: None,
            parent_codes: Vec::new(),
            ancestry_matched_code: None,
        };
    }

    // 1. 寻找候选受控 tag_code
    struct Candidate {
        code: String,
        canonical_name: String,
    }

    let mut candidates: Vec<Candidate> = Vec::new();

    // 1.1 从别名表中精确匹配 lemma（严格遵循 omw > hownet > builtin 权威优先级排序）
    if has_alias_table {
        let sql = format!(
            "SELECT a.tag_code, a.lemma, \
             (SELECT a2.lemma FROM {target_table} a2 WHERE a2.tag_code = a.tag_code AND a2.is_canonical = 1 LIMIT 1) as canonical_lemma \
             FROM {target_table} a \
             WHERE a.lemma = ?1 COLLATE NOCASE \
             ORDER BY \
               CASE \
                 WHEN a.tag_code LIKE 'omw.%' THEN 3 \
                 WHEN a.tag_code LIKE 'hownet.%' THEN 2 \
                 WHEN a.tag_code LIKE 'builtin.%' THEN 1 \
                 ELSE 0 \
               END DESC, \
               a.is_canonical DESC, a.count DESC"
        );
        if let Ok(mut stmt) = conn.prepare(&sql) {
            if let Ok(rows) = stmt.query_map([clean_name], |row| {
                let code: String = row.get(0)?;
                let lemma: String = row.get(1)?;
                let canonical_lemma: Option<String> = row.get(2)?;
                Ok(Candidate {
                    code,
                    canonical_name: canonical_lemma.unwrap_or(lemma),
                })
            }) {
                for c in rows.flatten() {
                    candidates.push(c);
                }
            }
        }
    }

    // 1.2 若别名表未命中，尝试直接以 code 精确匹配
    if candidates.is_empty() {
        let sql = "SELECT code FROM file_tags WHERE code = ?1";
        if let Ok(code) = conn.query_row(sql, [clean_name], |row| row.get::<_, String>(0)) {
            candidates.push(Candidate {
                code,
                canonical_name: clean_name.to_string(),
            });
        }
    }

    if candidates.is_empty() {
        return InspectTagItemResult {
            tag_name: tag_name.to_string(),
            is_controlled: false,
            matched_parent: false,
            code: None,
            canonical_name: None,
            parent_codes: Vec::new(),
            ancestry_matched_code: None,
        };
    }

    // 2. 针对候选条目拉取直接 parent_codes 并运行祖先闭包检测
    let fetch_parent_codes = |c: &str| -> Vec<String> {
        let sql = "SELECT parent_codes FROM file_tags WHERE code = ?1";
        if let Ok(raw) = conn.query_row(sql, [c], |row| row.get::<_, Option<String>>(0)) {
            if let Some(json_str) = raw {
                if let Ok(parsed) = serde_json::from_str::<Vec<String>>(&json_str) {
                    return parsed;
                }
            }
        }
        Vec::new()
    };

    // 祖先传递闭包算法 (Transitive Ancestor Closure Check)
    let check_ancestry = |start_code: &str, target: &str| -> Option<String> {
        if target.is_empty() {
            return None;
        }
        let mut queue: std::collections::VecDeque<(String, usize)> = std::collections::VecDeque::new();
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();

        let initial_parents = fetch_parent_codes(start_code);
        for p in initial_parents {
            if p == target {
                return Some(target.to_string());
            }
            queue.push_back((p.clone(), 1));
            visited.insert(p);
        }

        // BFS 向上追溯，最大深度限制 5 级防环
        while let Some((curr, depth)) = queue.pop_front() {
            if curr == target {
                return Some(target.to_string());
            }
            if depth >= 5 {
                continue;
            }
            let parents = fetch_parent_codes(&curr);
            for p in parents {
                if p == target {
                    return Some(target.to_string());
                }
                if visited.insert(p.clone()) {
                    queue.push_back((p, depth + 1));
                }
            }
        }
        None
    };

    // 优先选择祖先匹配成功的候选概念
    let mut matched_candidate: Option<(Candidate, Vec<String>, String)> = None;
    let mut fallback_candidate: Option<(Candidate, Vec<String>)> = None;

    for cand in candidates {
        let parents = fetch_parent_codes(&cand.code);
        if let Some(ancestry_hit) = check_ancestry(&cand.code, clean_parent) {
            matched_candidate = Some((cand, parents, ancestry_hit));
            break;
        } else if fallback_candidate.is_none() {
            fallback_candidate = Some((cand, parents));
        }
    }

    if let Some((cand, parents, ancestry_code)) = matched_candidate {
        InspectTagItemResult {
            tag_name: tag_name.to_string(),
            is_controlled: true,
            matched_parent: true,
            code: Some(cand.code),
            canonical_name: Some(cand.canonical_name),
            parent_codes: parents,
            ancestry_matched_code: Some(ancestry_code),
        }
    } else if let Some((cand, parents)) = fallback_candidate {
        InspectTagItemResult {
            tag_name: tag_name.to_string(),
            is_controlled: true,
            matched_parent: false,
            code: Some(cand.code),
            canonical_name: Some(cand.canonical_name),
            parent_codes: parents,
            ancestry_matched_code: None,
        }
    } else {
        InspectTagItemResult {
            tag_name: tag_name.to_string(),
            is_controlled: false,
            matched_parent: false,
            code: None,
            canonical_name: None,
            parent_codes: Vec::new(),
            ancestry_matched_code: None,
        }
    }
}

/// 批量鉴别受控标签（批次层单次检测系统表，杜绝 N+1 次 table_exists 查询）
pub fn inspect_tags_batch(
    conn: &Connection,
    items: &[InspectTagItem],
    locale: &str,
) -> anyhow::Result<Vec<InspectTagItemResult>> {
    let target_table = omni_pro::resolve_tag_aliases_table(locale);
    let has_alias_table = table_exists(conn, target_table);
    let has_file_tags = table_exists(conn, "file_tags");

    Ok(items
        .iter()
        .map(|item| {
            inspect_single_tag_internal(
                conn,
                &item.tag_name,
                &item.target_parent_code,
                target_table,
                has_alias_table,
                has_file_tags,
            )
        })
        .collect())
}

/// 树节点折叠与过滤策略 (Spec §3.1)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaxonomyFoldPolicy {
    /// 正常保留当前层级（名词实体、合法文件管理维度）
    Keep,
    /// 抽象壳词/过渡虚层：抹除自身目录抽屉，子节点穿透上提挂父
    BypassAndHoist,
    /// 动词体系扁平化：抹平中间学术链，有效命中标签直接提升至动作根
    FlattenToActionRoot,
    /// 语法虚词/相对时序：一票否决，整枝丢弃
    RejectSubtree,
}

/// 语义节点轻量判定元数据 (Spec §3.1)
#[derive(Debug, Clone)]
pub struct NodeEvaluationContext<'a> {
    pub code: &'a str,
    pub pos: Option<&'a str>,
    pub lexfile: Option<&'a str>,
    pub sememe_category: Option<u8>,
    pub is_root: bool,
    pub in_action_branch: bool,
}

/// 受控抽象空洞壳名词 Codes 静态白名单 (Spec §2.3 / ISS-02)
pub static SHELL_NOUN_CODES: &[&str] = &[
    "omw.00024720.n", // 状态 (state, status)
    "omw.00026367.n", // 过程 (process)
    "omw.00027167.n", // 方式 (manner, way)
    "omw.00028119.n", // 原因 (reason, cause)
    "omw.00029378.n", // 方法 (method)
    "omw.00024073.n", // 条件 (condition)
    "omw.13941407.n", // 情况 (situation)
    "omw.00026788.n", // 问题 (problem)
    "omw.05834839.n", // 问题
    "omw.05835747.n", // 类型 (type)
    "omw.13939634.n", // 阶段 (phase, stage)
    "omw.05836885.n", // 形式 (form)
    "omw.05837265.n", // 模式 (mode, pattern)
    "builtin.category.abstract_entity",
    "builtin.status",
    "builtin.state",
    "builtin.process",
    "builtin.way",
    "builtin.reason",
    "builtin.method",
    "builtin.condition",
    "builtin.situation",
    "builtin.problem",
    "builtin.type",
    "builtin.stage",
    "builtin.pattern",
];

/// 判定是否为抽象壳词代码
pub fn is_shell_noun_code(code: &str) -> bool {
    SHELL_NOUN_CODES.contains(&code)
}

/// 判定是否为动词或修饰词 (动词分支向下传染核心依据)
pub fn is_real_verb_or_modifier(pos: Option<&str>, lexfile: Option<&str>, code: &str) -> bool {
    if let Some(p) = pos {
        let p_lower = p.trim().to_lowercase();
        if p_lower == "v" || p_lower == "a" || p_lower == "s" || p_lower == "r" {
            return true;
        }
    }
    if let Some(lex) = lexfile {
        if lex.trim().starts_with("verb.") {
            return true;
        }
    }
    if code.starts_with("omw.")
        && (code.ends_with(".v") || code.ends_with(".a") || code.ends_with(".r") || code.ends_with(".s"))
    {
        return true;
    }
    false
}

/// 核心后置树策略评估器 (Spec §3.2 四维规则短路判定)
pub fn evaluate_node_policy(ctx: &NodeEvaluationContext) -> TaxonomyFoldPolicy {
    // 虚词/相对时序根代码
    let is_function_word_root = ctx.code == "builtin.category.function_word" || ctx.code == "builtin.category.relational_time";

    // P0: 根维度豁免（非虚词根；若根节点自身为动作大类或动词，触发 FlattenToActionRoot）
    if ctx.is_root && !is_function_word_root {
        if ctx.code == "builtin.category.action" || is_real_verb_or_modifier(ctx.pos, ctx.lexfile, ctx.code) {
            return TaxonomyFoldPolicy::FlattenToActionRoot;
        }
        return TaxonomyFoldPolicy::Keep;
    }

    // P1: 一票否决 (RejectSubtree)
    if is_function_word_root {
        return TaxonomyFoldPolicy::RejectSubtree;
    }
    if let Some(p) = ctx.pos {
        let p_trimmed = p.trim();
        if matches!(p_trimmed, "m" | "q" | "p" | "c" | "u" | "y" | "o" | "h" | "w" | "b" | "x")
            || omni_core::tag_admissibility::is_tag_pos_rejected(p_trimmed)
        {
            return TaxonomyFoldPolicy::RejectSubtree;
        }
    }
    if let Some(lex) = ctx.lexfile {
        if omni_core::tag_admissibility::is_banned_lexfile(lex) {
            return TaxonomyFoldPolicy::RejectSubtree;
        }
    }

    // P2: 动作分支向下传染 (FlattenToActionRoot)
    if ctx.in_action_branch
        || ctx.code == "builtin.category.action"
        || is_real_verb_or_modifier(ctx.pos, ctx.lexfile, ctx.code)
    {
        return TaxonomyFoldPolicy::FlattenToActionRoot;
    }

    // P3: 抽象壳词穿透 (BypassAndHoist) - 包含受控抽象代码、抽象实体大类与 WordNet 哲学顶层壳 (noun.Tops)
    if is_shell_noun_code(ctx.code)
        || ctx.code == "builtin.category.abstract_entity"
        || ctx.lexfile == Some("noun.Tops")
    {
        return TaxonomyFoldPolicy::BypassAndHoist;
    }

    // P4: 名词实体默认展开 (Keep)
    TaxonomyFoldPolicy::Keep
}

/// 节点语义元数据
#[derive(Debug, Clone, Default)]
pub struct NodeSemanticMeta {
    pub sort_order: i64,
    pub pos: Option<String>,
    pub lexfile: Option<String>,
    pub sememe_category: Option<u8>,
}

/// 批量查取语义包中节点的元数据（sort_order, pos, lexfile, sememe_category）
pub fn load_pack_semantic_meta_for_codes(conn: &Connection, codes: &[String]) -> HashMap<String, NodeSemanticMeta> {
    if codes.is_empty() || !table_exists(conn, "file_tags") {
        return HashMap::new();
    }

    // 动态探测列，确保兼容轻量测试夹具或只读 pack
    let mut has_sort_order = false;
    let mut has_pos = false;
    let mut has_lexfile = false;
    let mut has_sememe_category = false;

    if let Ok(mut info_stmt) = conn.prepare("PRAGMA table_info(file_tags)") {
        if let Ok(rows) = info_stmt.query_map([], |r| r.get::<_, String>(1)) {
            for col in rows.flatten() {
                match col.as_str() {
                    "sort_order" => has_sort_order = true,
                    "pos" => has_pos = true,
                    "lexfile" => has_lexfile = true,
                    "sememe_category" => has_sememe_category = true,
                    _ => {}
                }
            }
        }
    }

    let sort_expr = if has_sort_order { "sort_order" } else { "0 AS sort_order" };
    let pos_expr = if has_pos { "pos" } else { "NULL AS pos" };
    let lex_expr = if has_lexfile { "lexfile" } else { "NULL AS lexfile" };
    let sem_expr = if has_sememe_category { "sememe_category" } else { "NULL AS sememe_category" };

    let mut map = HashMap::new();
    for chunk in codes.chunks(200) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let sql = format!(
            "SELECT code, {sort_expr}, {pos_expr}, {lex_expr}, {sem_expr} FROM file_tags WHERE code IN ({placeholders})"
        );
        if let Ok(mut stmt) = conn.prepare(&sql) {
            let params: Vec<&dyn rusqlite::ToSql> = chunk.iter().map(|c| c as &dyn rusqlite::ToSql).collect();
            if let Ok(rows) = stmt.query_map(params.as_slice(), |row| {
                let code: String = row.get(0)?;
                let sort_order: i64 = row.get(1).unwrap_or(0);
                let pos: Option<String> = row.get(2).ok();
                let lexfile: Option<String> = row.get(3).ok();
                let sememe_category: Option<u8> = row.get(4).ok();
                Ok((code, NodeSemanticMeta {
                    sort_order,
                    pos,
                    lexfile,
                    sememe_category,
                }))
            }) {
                for (code, meta) in rows.flatten() {
                    map.insert(code, meta);
                }
            }
        }
    }
    map
}

/// 从语义包 `file_tags` 按需读取指定受控 codes 的 `sort_order > 0` 映射（受控域排序值唯一事实源）。
///
/// - 利用 `PRIMARY KEY (code)` B-Tree 索引批量点查，单批次 <= 200 个，耗时仅 ~0.05ms，彻底杜绝 40,000+ 行全表扫描；
/// - 表不存在或测试夹具未提供 `file_tags` 时安全返回空映射，节点保留兜底序值。
#[allow(dead_code)]
fn load_pack_sort_orders_for_codes(conn: &Connection, codes: &[String]) -> HashMap<String, i64> {
    let meta_map = load_pack_semantic_meta_for_codes(conn, codes);
    meta_map.into_iter().filter(|(_, m)| m.sort_order > 0).map(|(c, m)| (c, m.sort_order)).collect()
}

/// 以绝对只读 + WAL 防阻塞 + 内存临时表安全模式打开桌面端业务主库 (Spec §3.1.1)
pub fn open_omni_readonly_connection<P: AsRef<std::path::Path>>(db_path: P) -> anyhow::Result<Connection> {
    let conn = Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    // 1. 设置 5 秒 busy 超时，防止 Node 主进程写入时直接抛出 SQLITE_BUSY
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    // 2. 强制只读，杜绝任何意外写操作
    conn.pragma_update(None, "query_only", "ON")?;
    // 3. 分配专用内存缓存页 (64MB)
    conn.pragma_update(None, "cache_size", -65536)?;
    // 4. 将临时表强制存放在内存中，禁止生成磁盘临时文件
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    Ok(conn)
}

/// 倒排 Trie 极速分类树构建引擎 (ADR-0038 / ADR-0053 / P95 <= 5ms)
pub fn query_taxonomy_tree_fast(
    conn: &Connection,
    locale: &str,
    root_filter: Option<&str>,
    master_conn: &Connection,
    include_files: bool,
    workspace_id: Option<i64>,
    directory_prefix: Option<&str>,
) -> anyhow::Result<TaxonomyTreeResponse> {
    let t_start = std::time::Instant::now();
    let lang = locale.split('-').next().unwrap_or("zh");

    // 1. 检查主库中表与列的存在性
    let has_ftr: bool = master_conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'file_tag_relations'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);

    if !has_ftr {
        return Ok(TaxonomyTreeResponse {
            locale: locale.to_string(),
            root_nodes: Vec::new(),
            total_nodes: 0,
        });
    }

    let mut has_code_path = false;
    let mut has_name_path = false;
    if let Ok(mut col_stmt) = master_conn.prepare("PRAGMA table_info(file_tag_relations)") {
        if let Ok(rows) = col_stmt.query_map([], |r| r.get::<_, String>(1)) {
            for col in rows.flatten() {
                if col == "code_path" {
                    has_code_path = true;
                } else if col == "name_path" {
                    has_name_path = true;
                }
            }
        }
    }

    let has_wf: bool = master_conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'workspace_files'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);

    let mut where_clauses = Vec::new();
    let mut params: Vec<rusqlite::types::Value> = Vec::new();

    if has_wf {
        where_clauses.push("wf.status = 1".to_string());
    }

    if let Some(ws_id) = workspace_id {
        if has_wf {
            where_clauses.push("wf.workspace_id = ?".to_string());
            params.push(rusqlite::types::Value::Integer(ws_id));
        }
    }

    if let Some(prefix) = directory_prefix {
        let clean_prefix = prefix.trim();
        if !clean_prefix.is_empty() && has_wf {
            #[cfg(windows)]
            let norm_prefix = clean_prefix.replace('/', "\\");
            #[cfg(not(windows))]
            let norm_prefix = clean_prefix.replace('\\', "/");

            let sep = if norm_prefix.contains('\\') { "\\" } else { "/" };
            let base = norm_prefix.trim_end_matches(['/', '\\']);
            let base_with_sep = format!("{}{}", base, sep);
            // 安全转义：使用 | 作为转义符，原样保留原生反斜杠 \ (Spec §3.1.1)
            let escaped_prefix = base_with_sep
                .replace('|', "||")
                .replace('%', "|%")
                .replace('_', "|_");
            let prefix_pattern = format!("{}%", escaped_prefix);

            where_clauses.push("(wf.path = ? OR wf.path LIKE ? ESCAPE '|')".to_string());
            params.push(rusqlite::types::Value::Text(base.to_string()));
            params.push(rusqlite::types::Value::Text(prefix_pattern));
        }
    }

    let col_name_expr = if has_name_path { "ftr.name_path" } else { "''" };
    let col_code_expr = if has_code_path { "ftr.code_path" } else { "''" };

    let select_sql = if has_wf && !where_clauses.is_empty() {
        format!(
            "SELECT DISTINCT wf.rowid, ftr.file_fingerprint, ftr.tag_code, {}, {} \
             FROM file_tag_relations ftr \
             JOIN workspace_files wf ON wf.file_fingerprint = ftr.file_fingerprint \
             WHERE {}",
            col_code_expr,
            col_name_expr,
            where_clauses.join(" AND ")
        )
    } else if has_wf {
        format!(
            "SELECT DISTINCT wf.rowid, ftr.file_fingerprint, ftr.tag_code, {}, {} \
             FROM file_tag_relations ftr \
             JOIN workspace_files wf ON wf.file_fingerprint = ftr.file_fingerprint",
            col_code_expr,
            col_name_expr
        )
    } else {
        let col_name_raw = if has_name_path { "name_path" } else { "''" };
        let col_code_raw = if has_code_path { "code_path" } else { "''" };
        format!(
            "SELECT ftr.rowid, ftr.file_fingerprint, ftr.tag_code, {}, {} FROM file_tag_relations ftr",
            col_code_raw, col_name_raw
        )
    };

    let mut stmt = master_conn.prepare(&select_sql)?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p as &dyn rusqlite::ToSql).collect();
    let rows = stmt.query_map(param_refs.as_slice(), |row| {
        let raw_id: i64 = row.get(0)?;
        let fp: String = row.get(1)?;
        let tag_code: String = row.get(2)?;
        let code_path: String = row.get(3).unwrap_or_default();
        let name_path: String = row.get(4).unwrap_or_default();
        Ok((raw_id, fp, tag_code, code_path, name_path))
    })?;

    // Trie 节点结构：唯一键为从根到该节点的物化完整路径 (current_code_path)，捍卫前缀树分支隔离性
    struct TrieNode {
        code: String,
        name: String,
        code_path: String,
        name_path: String,
        parent_path: Option<String>,
        source: String,
        sort_order: i64,
        direct_files: HashSet<String>,
        direct_bitmap: roaring::RoaringBitmap,
        children_paths: Vec<String>,
        pos: Option<String>,
        lexfile: Option<String>,
        sememe_category: Option<u8>,
    }

    let mut node_map: HashMap<String, TrieNode> = HashMap::new();

    // 2. 遍历活跃记录，自顶向下构建前缀树 Trie
    for r in rows.flatten() {
        let (raw_id, fp, tag_code, raw_code_path, raw_name_path) = r;
        if fp.trim().is_empty() || tag_code.trim().is_empty() {
            continue;
        }

        let code_content_tags = omni_core::concepts::Concept::内容标签.code();
        let code_file_type = omni_core::concepts::Concept::文件类型.code();
        let code_lang_seg = omni_core::concepts::Concept::语言细分.code();
        let code_sec_level = omni_core::concepts::Concept::安全等级.code();
        let code_proc_status = omni_core::concepts::Concept::处理状态.code();
        let code_photo_cat = omni_core::concepts::Concept::摄影照片细分.code();
        let code_text_density = omni_core::concepts::Concept::文字密度.code();
        let code_geo_location = omni_core::concepts::Concept::地理位置.code();

        let name_content_tags = omni_core::concepts::Concept::内容标签.zh_name();
        let name_geo_location = omni_core::concepts::Concept::地理位置.zh_name();
        let name_file_type = omni_core::concepts::Concept::文件类型.zh_name();
        let name_document = omni_core::concepts::Concept::文档.zh_name();

        // 悲观数据兜底 (M-3)：若 code_path 为空，回退虚拟挂载至 /{code_content_tags}/{tag_code}
        let (eff_code_path, eff_name_path) = if raw_code_path.trim().is_empty() {
            (
                format!("/{}/{}", code_content_tags, tag_code),
                format!("/{}/{}", name_content_tags, tag_code),
            )
        } else {
            (raw_code_path, raw_name_path)
        };

        let mut code_segs: Vec<String> = eff_code_path
            .split('/')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let mut name_segs: Vec<String> = eff_name_path
            .split('/')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        if code_segs.is_empty() {
            code_segs.push(code_content_tags.to_string());
            code_segs.push(tag_code.clone());
            name_segs.push(name_content_tags.to_string());
            name_segs.push(tag_code.clone());
        }

        // 历史驼峰归一化兼容
        if code_segs[0] == "builtin.fileType" {
            code_segs[0] = code_file_type.to_string();
        }

        // 历史兼并副码自愈归一（ISS-01）：将历史 omw.06367879.n 直接归入正统受控码 omw.03833065.n
        for seg in &mut code_segs {
            if seg == "omw.06367879.n" {
                *seg = "omw.03833065.n".to_string();
            }
        }

        // 拓扑防环（ISS-01）：若路径中出现重复节点（如 A/.../A 隔代同码回环），截除回环冗余段
        let mut seen_codes = HashSet::new();
        let mut dedup_code_segs = Vec::new();
        let mut dedup_name_segs = Vec::new();
        for (idx, seg) in code_segs.into_iter().enumerate() {
            if seen_codes.insert(seg.clone()) {
                dedup_code_segs.push(seg);
                if idx < name_segs.len() {
                    dedup_name_segs.push(name_segs[idx].clone());
                }
            }
        }
        code_segs = dedup_code_segs;
        name_segs = dedup_name_segs;

        // 历史存量数据自愈：若前缀误被包装为 /{code_content_tags}/<subdim>/... 或 /{code_content_tags}/<root_dim>/...，
        // 门禁：只要第二段为受控维度代码（如摄影照片细分、文字密度、语言细分等）或一级根维度代码，绝对严禁作为 content_tags 的子节点，直接剥除伪 content_tags 前缀！
        if code_segs.len() > 1 && code_segs[0] == code_content_tags {
            let is_subdim_or_root = omni_core::is_root_dimension(&code_segs[1])
                || omni_core::get_dimension_policy(&code_segs[1]).is_some()
                || omni_core::tag_identity::concept_from_code(&code_segs[1])
                    .map_or(false, |c_concept| omni_core::get_dimension_policy(c_concept.code()).is_some())
                || code_segs[1] == code_lang_seg
                || code_segs[1] == "builtin.language_segmentation"
                || code_segs[1] == code_sec_level
                || code_segs[1] == "builtin.security_level"
                || code_segs[1] == code_proc_status
                || code_segs[1] == "builtin.processing_status"
                || code_segs[1] == code_photo_cat
                || code_segs[1] == code_text_density;
            if is_subdim_or_root {
                code_segs.remove(0);
                if !name_segs.is_empty() {
                    name_segs.remove(0);
                }
            }
        }

        // 历史错误拓扑自愈：地理位置为一级独立根维度，若历史脏数据中被误挂在其他维度（如文件类型->图片->地理位置），
        // 自动切除前缀，将地理位置提升为根节点
        if let Some(pos) = code_segs.iter().position(|s| s == code_geo_location) {
            if pos > 0 {
                code_segs.drain(0..pos);
                if let Some(name_pos) = name_segs.iter().position(|s| s == name_geo_location) {
                    name_segs.drain(0..name_pos);
                } else if name_segs.len() > pos {
                    name_segs.drain(0..pos);
                }
            }
        }

        // 历史截断短路径自愈：子维度若缺失顶层祖先，自动根据本体拓扑补齐真实父级路径
        if code_segs[0] == code_lang_seg || code_segs[0] == "builtin.language_segmentation" {
            // 语言细分属于 文件类型 -> 文档
            code_segs.insert(0, "builtin.document".to_string());
            code_segs.insert(0, code_file_type.to_string());
            if !name_segs.is_empty() {
                name_segs.insert(0, name_document.to_string());
                name_segs.insert(0, name_file_type.to_string());
            }
        } else if code_segs[0] == code_sec_level
            || code_segs[0] == "builtin.security_level"
            || code_segs[0] == code_proc_status
            || code_segs[0] == "builtin.processing_status"
        {
            // 安全等级 / 处理状态属于 文件用途 -> 工作文档
            code_segs.insert(0, omni_core::concepts::Concept::工作文档.code().to_string());
            code_segs.insert(0, omni_core::concepts::Concept::文件用途.code().to_string());
            if !name_segs.is_empty() {
                name_segs.insert(0, "工作文档".to_string());
                name_segs.insert(0, "文件用途".to_string());
            }
        }

        // 受控根维度封闭性与排他门禁 (Spec v2.0 不变式 1 & 不变式 2)
        if !omni_core::is_root_dimension(&code_segs[0]) && code_segs[0] != code_content_tags {
            let is_subdim = omni_core::get_dimension_policy(&code_segs[0]).is_some()
                || omni_core::tag_identity::concept_from_code(&code_segs[0])
                    .map_or(false, |c_concept| omni_core::get_dimension_policy(c_concept.code()).is_some())
                || code_segs[0] == code_photo_cat
                || code_segs[0] == code_text_density;
            if is_subdim {
                // 已归入受控细分体系，排他门禁生效：严禁强插内容标签！
                // 前置补齐所属文件类型一级根
                let root_code = code_file_type.to_string();
                let root_name = "文件类型".to_string();
                code_segs.insert(0, root_code);
                name_segs.insert(0, root_name);
            } else {
                // 仅对非受控的开放自由标签或未声明父级的受控泛标签，收敛兜底至内容标签
                code_segs.insert(0, code_content_tags.to_string());
                name_segs.insert(0, "内容标签".to_string());
            }
        }

        // 自顶向下装配路径节点
        let seg_len = code_segs.len();
        for i in 0..seg_len {
            let seg_code = code_segs[i].clone();
            let seg_name = if i < name_segs.len() && !name_segs[i].is_empty() {
                name_segs[i].clone()
            } else {
                seg_code.clone()
            };
            let curr_code_path = format!("/{}", code_segs[0..=i].join("/"));
            let curr_name_path = if i < name_segs.len() {
                format!("/{}", name_segs[0..=i].join("/"))
            } else {
                curr_code_path.clone()
            };
            let parent_code_path = if i > 0 {
                Some(format!("/{}", code_segs[0..i].join("/")))
            } else {
                None
            };

            let source = if seg_code.starts_with("builtin.") {
                "builtin".to_string()
            } else if seg_code.starts_with("omw.") {
                "omw".to_string()
            } else if seg_code.starts_with("_ext.") {
                "_ext".to_string()
            } else {
                "user".to_string()
            };

            let sort_order = if seg_code == omni_core::concepts::Concept::内容标签.code() {
                9999
            } else if seg_code == omni_core::concepts::Concept::文件类型.code() {
                1
            } else {
                0
            };

            // 检查或插入当前路径节点 (以物化 code_path 为键保证前缀树各分支绝对独立)
            if !node_map.contains_key(&curr_code_path) {
                node_map.insert(
                    curr_code_path.clone(),
                    TrieNode {
                        code: seg_code.clone(),
                        name: seg_name,
                        code_path: curr_code_path.clone(),
                        name_path: curr_name_path,
                        parent_path: parent_code_path.clone(),
                        source,
                        sort_order,
                        direct_files: HashSet::new(),
                        direct_bitmap: roaring::RoaringBitmap::new(),
                        children_paths: Vec::new(),
                        pos: None,
                        lexfile: None,
                        sememe_category: None,
                    },
                );
            }

            // 父节点维护子节点路径列表
            if let Some(ref p_path) = parent_code_path {
                if let Some(p_node) = node_map.get_mut(p_path) {
                    if !p_node.children_paths.contains(&curr_code_path) {
                        p_node.children_paths.push(curr_code_path.clone());
                    }
                }
            }

            // 【捍卫真实父与逻辑父文件计数公理 (M-1)】
            // 中间祖先节点 (逻辑父) direct_files 初始化为 ∅
            // 唯有路径末级命中节点才累加 direct_files
            if i == seg_len - 1 {
                if let Some(target_node) = node_map.get_mut(&curr_code_path) {
                    if raw_id >= 0 && raw_id <= (u32::MAX as i64) {
                        target_node.direct_bitmap.insert(raw_id as u32);
                    } else {
                        tracing::error!("[Omni] 检测到非法文件整型 ID: {}", raw_id);
                    }
                    if include_files {
                        target_node.direct_files.insert(fp.clone());
                    }
                }
            }
        }
    }

    if node_map.is_empty() {
        return Ok(TaxonomyTreeResponse {
            locale: locale.to_string(),
            root_nodes: Vec::new(),
            total_nodes: 0,
        });
    }

    let all_codes: Vec<String> = {
        let mut s = HashSet::new();
        for n in node_map.values() {
            s.insert(n.code.clone());
        }
        s.into_iter().collect()
    };

    // 2.5 按需回填语义包 file_tags 元数据（sort_order, pos, lexfile, sememe_category）：
    // 性能优化：仅对当前树出现的 all_codes 通过 PRIMARY KEY B-Tree 索引批量点查，耗时 < 0.1ms；
    // 特例：builtin.content_tags 维持硬编码 9999 恒定垫底，不被 pack 值覆盖。
    let pack_semantic_meta = load_pack_semantic_meta_for_codes(conn, &all_codes);
    for node in node_map.values_mut() {
        if let Some(meta) = pack_semantic_meta.get(&node.code) {
            if node.code != omni_core::concepts::Concept::内容标签.code() && meta.sort_order > 0 {
                node.sort_order = meta.sort_order;
            }
            node.pos = meta.pos.clone();
            node.lexfile = meta.lexfile.clone();
            node.sememe_category = meta.sememe_category;
        }
        // 若数据库未直接提供 pos，从 code 后缀做权威词性推导
        if node.pos.is_none() {
            if node.code.starts_with("omw.") {
                if node.code.ends_with(".v") {
                    node.pos = Some("v".to_string());
                } else if node.code.ends_with(".n") {
                    node.pos = Some("n".to_string());
                } else if node.code.ends_with(".a") || node.code.ends_with(".s") {
                    node.pos = Some("a".to_string());
                } else if node.code.ends_with(".r") {
                    node.pos = Some("r".to_string());
                }
            } else if node.code.starts_with("hownet.") && node.code.ends_with(".n") {
                node.pos = Some("n".to_string());
            }
        }
    }

    // 3. 按需查词 (P95 <= 5ms)：仅对 Trie 树涉及的活跃节点批量向 tag_aliases_{lang} 查词
    let mut resolved_names: HashMap<String, String> = HashMap::new();
    let target_table = omni_pro::resolve_tag_aliases_table(locale);
    let has_target_table = table_exists(conn, target_table);

    if has_target_table && !all_codes.is_empty() {
        for chunk in all_codes.chunks(200) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!(
                "SELECT tag_code, lemma FROM {target_table} WHERE is_canonical = 1 AND tag_code IN ({placeholders})"
            );
            if let Ok(mut stmt) = conn.prepare(&sql) {
                let params: Vec<&dyn rusqlite::ToSql> = chunk.iter().map(|c| c as &dyn rusqlite::ToSql).collect();
                if let Ok(rows) = stmt.query_map(params.as_slice(), |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                }) {
                    for (c, l) in rows.flatten() {
                        resolved_names.insert(c, l);
                    }
                }
            }
        }
    }

    // 4.1 节点结构转换辅助函数
    fn convert_trie_node_to_taxonomy_node(
        node: &TrieNode,
        node_map: &HashMap<String, TrieNode>,
        resolved_names: &HashMap<String, String>,
        lang: &str,
        include_files: bool,
        file_count: usize,
        files_set: &HashSet<String>,
    ) -> TaxonomyNode {
        let files = if include_files {
            let mut list: Vec<String> = files_set.iter().cloned().collect();
            list.sort();
            list
        } else {
            Vec::new()
        };

        let display_name = if let Some(n) = resolved_names.get(&node.code) {
            n.clone()
        } else if omni_core::is_root_dimension(&node.code) || node.code.starts_with("builtin.") {
            let td = omni_core::tag_identity::tag_display(&node.code, lang);
            if lang.starts_with("zh") && td.is_ascii() && !node.name.is_empty() && !node.name.is_ascii() && node.name != node.code {
                node.name.clone()
            } else {
                td
            }
        } else if !node.name.is_empty() && node.name != node.code {
            node.name.clone()
        } else {
            omni_core::tag_identity::tag_display(&node.code, lang)
        };

        let parent_code = node
            .parent_path
            .as_ref()
            .and_then(|p| node_map.get(p).map(|pn| pn.code.clone()));
        let parent_codes = parent_code.as_ref().map(|p| vec![p.clone()]).unwrap_or_default();

        TaxonomyNode {
            code: node.code.clone(),
            name: display_name,
            parent_code,
            parent_codes,
            source: node.source.clone(),
            sort_order: node.sort_order,
            file_count,
            files,
            children: Vec::new(),
            code_path: Some(node.code_path.clone()),
            name_path: Some(node.name_path.clone()),
        }
    }

    // 4.2 动词分支扁平化子孙收集器 (Spec §3.3)
    fn collect_action_flattened_children(
        node_path: &str,
        action_root_code: &str,
        node_map: &HashMap<String, TrieNode>,
        resolved_names: &HashMap<String, String>,
        lang: &str,
        include_files: bool,
        visited: &mut HashSet<String>,
    ) -> (Vec<(TaxonomyNode, roaring::RoaringBitmap)>, roaring::RoaringBitmap, HashSet<String>) {
        let mut result_nodes = Vec::new();
        let mut aggregate_bitmap = roaring::RoaringBitmap::new();
        let mut aggregate_files = HashSet::new();

        // 细节 1：防死循环防护
        if !visited.insert(node_path.to_string()) {
            return (result_nodes, aggregate_bitmap, aggregate_files);
        }

        let node = match node_map.get(node_path) {
            Some(n) => n,
            None => return (result_nodes, aggregate_bitmap, aggregate_files),
        };

        // 细节 2：即使在动作分支内部，遇到 P1 级语法虚词/禁用 lexfile 仍优先整枝截断
        let ctx = NodeEvaluationContext {
            code: &node.code,
            pos: node.pos.as_deref(),
            lexfile: node.lexfile.as_deref(),
            sememe_category: node.sememe_category,
            is_root: false,
            in_action_branch: true,
        };
        if evaluate_node_policy(&ctx) == TaxonomyFoldPolicy::RejectSubtree {
            return (result_nodes, aggregate_bitmap, aggregate_files);
        }

        aggregate_bitmap |= &node.direct_bitmap;
        if include_files {
            aggregate_files.extend(node.direct_files.clone());
        }

        // 递归下钻收集子孙
        for child_path in &node.children_paths {
            let (sub_nodes, sub_bitmap, sub_files) = collect_action_flattened_children(
                child_path,
                action_root_code,
                node_map,
                resolved_names,
                lang,
                include_files,
                visited,
            );
            aggregate_bitmap |= &sub_bitmap;
            if include_files {
                aggregate_files.extend(sub_files);
            }
            result_nodes.extend(sub_nodes);
        }

        // 公理 2：若当前动词节点自身有直接文件命中 (direct_bitmap > 0)，
        // 自身作为一个独立的平铺叶节点被返回（抽离原有 children）
        if !node.direct_bitmap.is_empty() {
            let mut cloned = convert_trie_node_to_taxonomy_node(
                node,
                node_map,
                resolved_names,
                lang,
                include_files,
                node.direct_bitmap.len() as usize,
                &node.direct_files,
            );
            cloned.children.clear(); // 抹平子级嵌套
            cloned.file_count = node.direct_bitmap.len() as usize;
            // 细节 3：显式将其父级指针修正为当前动作根，避免指向已被抹除的悬空中间父级
            cloned.parent_code = Some(action_root_code.to_string());
            cloned.parent_codes = vec![action_root_code.to_string()];
            result_nodes.push((cloned, node.direct_bitmap.clone()));
        }

        (result_nodes, aggregate_bitmap, aggregate_files)
    }

    // 4.3 动作分支平铺子节点按同 code 归并与稳定排序 (ISS-01 & ISS-03)
    fn merge_and_sort_flattened_nodes(
        nodes: Vec<(TaxonomyNode, roaring::RoaringBitmap)>,
        action_root_code: &str,
        include_files: bool,
    ) -> Vec<TaxonomyNode> {
        let mut map: BTreeMap<String, (TaxonomyNode, roaring::RoaringBitmap)> = BTreeMap::new();
        for (mut node, bm) in nodes {
            node.parent_code = Some(action_root_code.to_string());
            node.parent_codes = vec![action_root_code.to_string()];
            if let Some(existing) = map.get_mut(&node.code) {
                existing.1 |= &bm;
                existing.0.file_count = existing.1.len() as usize;
                if include_files {
                    let mut file_set: HashSet<String> = existing.0.files.iter().cloned().collect();
                    file_set.extend(node.files);
                    let mut sorted_files: Vec<String> = file_set.into_iter().collect();
                    sorted_files.sort();
                    existing.0.files = sorted_files;
                } else {
                    existing.0.files.clear();
                }
            } else {
                map.insert(node.code.clone(), (node, bm));
            }
        }
        let mut result: Vec<TaxonomyNode> = map.into_values().map(|(n, _)| n).collect();
        result.sort_by(|a, b| {
            let order_a = if a.sort_order > 0 { a.sort_order } else { i64::MAX };
            let order_b = if b.sort_order > 0 { b.sort_order } else { i64::MAX };
            if order_a != order_b {
                order_a.cmp(&order_b)
            } else {
                a.code.cmp(&b.code)
            }
        });
        result
    }

    // 4.4 递归装配 Trie 子树 (四维策略分流)
    fn build_trie_subtree(
        path: &str,
        node_map: &HashMap<String, TrieNode>,
        resolved_names: &HashMap<String, String>,
        lang: &str,
        include_files: bool,
        in_action_branch: bool,
        is_root: bool,
        visited: &mut HashSet<String>,
    ) -> Option<(Vec<(TaxonomyNode, roaring::RoaringBitmap)>, roaring::RoaringBitmap, HashSet<String>)> {
        if !visited.insert(path.to_string()) {
            return None; // 防环保护
        }

        let node = match node_map.get(path) {
            Some(n) => n,
            None => {
                visited.remove(path);
                return None;
            }
        };

        // 策略评估
        let ctx = NodeEvaluationContext {
            code: &node.code,
            pos: node.pos.as_deref(),
            lexfile: node.lexfile.as_deref(),
            sememe_category: node.sememe_category,
            is_root,
            in_action_branch,
        };
        let policy = evaluate_node_policy(&ctx);

        match policy {
            TaxonomyFoldPolicy::RejectSubtree => {
                visited.remove(path);
                None
            }
            TaxonomyFoldPolicy::FlattenToActionRoot => {
                // 当前节点作为动作根节点，执行二级扁平化
                let mut aggregate_bitmap = node.direct_bitmap.clone();
                let mut aggregate_files = if include_files { node.direct_files.clone() } else { HashSet::new() };
                let mut unmerged_children = Vec::new();
                let mut action_visited = HashSet::new();
                action_visited.insert(path.to_string());

                for child_path in &node.children_paths {
                    let (child_nodes, child_bm, child_files) = collect_action_flattened_children(
                        child_path,
                        &node.code,
                        node_map,
                        resolved_names,
                        lang,
                        include_files,
                        &mut action_visited,
                    );
                    aggregate_bitmap |= &child_bm;
                    if include_files {
                        aggregate_files.extend(child_files);
                    }
                    unmerged_children.extend(child_nodes);
                }

                let flattened_children = merge_and_sort_flattened_nodes(unmerged_children, &node.code, include_files);
                let mut tax_node = convert_trie_node_to_taxonomy_node(
                    node,
                    node_map,
                    resolved_names,
                    lang,
                    include_files,
                    aggregate_bitmap.len() as usize,
                    &aggregate_files,
                );
                tax_node.children = flattened_children;
                visited.remove(path);
                Some((vec![(tax_node, aggregate_bitmap.clone())], aggregate_bitmap, aggregate_files))
            }
            TaxonomyFoldPolicy::BypassAndHoist => {
                // 抽象壳词：抹除自身抽屉，子节点穿透上提
                let mut aggregate_bitmap = node.direct_bitmap.clone();
                let mut aggregate_files = if include_files { node.direct_files.clone() } else { HashSet::new() };
                let mut hoisted_nodes = Vec::new();

                for child_path in &node.children_paths {
                    if let Some((child_nodes, child_bm, child_files)) = build_trie_subtree(
                        child_path,
                        node_map,
                        resolved_names,
                        lang,
                        include_files,
                        in_action_branch,
                        false,
                        visited,
                    ) {
                        aggregate_bitmap |= &child_bm;
                        if include_files {
                            aggregate_files.extend(child_files);
                        }
                        hoisted_nodes.extend(child_nodes);
                    }
                }
                visited.remove(path);
                Some((hoisted_nodes, aggregate_bitmap, aggregate_files))
            }
            TaxonomyFoldPolicy::Keep => {
                // 正常保留当前层级（名词实体、根维度等）
                let mut aggregate_bitmap = node.direct_bitmap.clone();
                let mut aggregate_files = if include_files { node.direct_files.clone() } else { HashSet::new() };
                let mut children_nodes: Vec<(TaxonomyNode, roaring::RoaringBitmap)> = Vec::new();

                // 向下传染 action 分支状态（用户微调提醒：包含真实动词/修饰词）
                let next_in_action_branch = in_action_branch
                    || node.code == "builtin.category.action"
                    || is_real_verb_or_modifier(node.pos.as_deref(), node.lexfile.as_deref(), &node.code);

                for child_path in &node.children_paths {
                    if let Some((child_nodes, child_bm, child_files)) = build_trie_subtree(
                        child_path,
                        node_map,
                        resolved_names,
                        lang,
                        include_files,
                        next_in_action_branch,
                        false,
                        visited,
                    ) {
                        aggregate_bitmap |= &child_bm;
                        if include_files {
                            aggregate_files.extend(child_files);
                        }
                        for mut cn in child_nodes {
                            // 若子节点来自 BypassAndHoist 上提，其 parent_code 修正为当前节点
                            cn.0.parent_code = Some(node.code.clone());
                            cn.0.parent_codes = vec![node.code.clone()];
                            children_nodes.push(cn);
                        }
                    }
                }

                let mut tax_node = convert_trie_node_to_taxonomy_node(
                    node,
                    node_map,
                    resolved_names,
                    lang,
                    include_files,
                    aggregate_bitmap.len() as usize,
                    &aggregate_files,
                );

                // 【加固点 2：父子同名自愈折叠 (Deduplication Axiom & Child Hoisting)】
                // 若子节点的展示名与当前父节点展示名完全相同（如跨语种撞译导致的“资本 -> 资本”），
                // 抹平同名子抽屉，将子节点的直接文件合并至父节点，并将孙辈节点提升为当前父节点的直接子节点。
                // 采用 loop 迭代展开，天然免疫 A -> A -> A 连续多重撞名嵌套。
                let mut current_children = children_nodes;
                if !tax_node.name.trim().is_empty() {
                    loop {
                        let mut has_duplicate = false;
                        let mut next_children = Vec::new();
                        for (child, child_bm) in current_children {
                            if child.name.trim() == tax_node.name.trim() {
                                has_duplicate = true;
                                if include_files {
                                    let mut merged_files: HashSet<String> = tax_node.files.into_iter().collect();
                                    merged_files.extend(child.files);
                                    let mut sorted_files: Vec<String> = merged_files.into_iter().collect();
                                    sorted_files.sort();
                                    tax_node.files = sorted_files;
                                }
                                for mut grand_child in child.children {
                                    grand_child.parent_code = Some(node.code.clone());
                                    grand_child.parent_codes = vec![node.code.clone()];
                                    let g_bm = roaring::RoaringBitmap::new();
                                    next_children.push((grand_child, g_bm));
                                }
                            } else {
                                next_children.push((child, child_bm));
                            }
                        }
                        current_children = next_children;
                        if !has_duplicate {
                            break;
                        }
                    }
                }

                // 【加固点 3：同父同名兄弟节点自愈合并 (Sibling Deduplication Axiom / ISS-01)】
                // 采用 RoaringBitmap 并在合并时执行按位或 (|=)，在 !include_files 模式下依然保持亚毫秒精确计数
                let mut sibling_map: BTreeMap<String, (TaxonomyNode, roaring::RoaringBitmap)> = BTreeMap::new();
                for (child, child_bm) in current_children {
                    let key = if !child.name.trim().is_empty() {
                        child.name.trim().to_string()
                    } else {
                        child.code.clone()
                    };
                    if let Some(existing) = sibling_map.get_mut(&key) {
                        existing.1 |= &child_bm;
                        existing.0.file_count = existing.1.len() as usize;
                        if include_files {
                            let mut merged_files: HashSet<String> = existing.0.files.iter().cloned().collect();
                            merged_files.extend(child.files);
                            existing.0.file_count = merged_files.len();
                            let mut sorted_files: Vec<String> = merged_files.into_iter().collect();
                            sorted_files.sort();
                            existing.0.files = sorted_files;
                        } else {
                            existing.0.files.clear();
                        }
                        existing.0.children.extend(child.children);
                    } else {
                        sibling_map.insert(key, (child, child_bm));
                    }
                }
                let mut current_children_clean: Vec<TaxonomyNode> = sibling_map.into_values().map(|(n, _)| n).collect();

                // 子节点排序：sort_order > 0 升序优先，相同按 code 字典序
                current_children_clean.sort_by(|a, b| {
                    let order_a = if a.sort_order > 0 { a.sort_order } else { i64::MAX };
                    let order_b = if b.sort_order > 0 { b.sort_order } else { i64::MAX };
                    if order_a != order_b {
                        order_a.cmp(&order_b)
                    } else {
                        a.code.cmp(&b.code)
                    }
                });

                tax_node.children = current_children_clean;
                visited.remove(path);
                Some((vec![(tax_node, aggregate_bitmap.clone())], aggregate_bitmap, aggregate_files))
            }
        }
    }

    // 5. 顶层根节点过滤与排序输出
    let mut roots = Vec::new();
    let mut visited = HashSet::new();

    let root_paths: Vec<String> = node_map
        .values()
        .filter(|n| n.parent_path.is_none())
        .map(|n| n.code_path.clone())
        .collect();

    if let Some(target_root) = root_filter {
        let target_norm = if target_root.starts_with('/') {
            target_root.to_string()
        } else {
            format!("/{}", target_root)
        };
        for r_path in &root_paths {
            if let Some(r_node) = node_map.get(r_path) {
                if r_node.code == target_root || r_node.code_path == target_norm {
                    if let Some((root_nodes, _, _)) = build_trie_subtree(
                        r_path,
                        &node_map,
                        &resolved_names,
                        lang,
                        include_files,
                        false,
                        true, // is_root = true
                        &mut visited,
                    ) {
                        for (root_node, _) in root_nodes {
                            if !root_node.children.is_empty() || root_node.file_count > 0 {
                                roots.push(root_node);
                            }
                        }
                    }
                }
            }
        }
    } else {
        let mut sorted_root_paths = root_paths;
        sorted_root_paths.sort_by(|a, b| {
            let node_a = node_map.get(a);
            let node_b = node_map.get(b);
            let order_a = node_a.map(|n| if n.sort_order > 0 { n.sort_order } else { i64::MAX }).unwrap_or(i64::MAX);
            let order_b = node_b.map(|n| if n.sort_order > 0 { n.sort_order } else { i64::MAX }).unwrap_or(i64::MAX);
            if order_a != order_b {
                order_a.cmp(&order_b)
            } else {
                a.cmp(b)
            }
        });

        for r_path in sorted_root_paths {
            if let Some((root_nodes, _, _)) = build_trie_subtree(
                &r_path,
                &node_map,
                &resolved_names,
                lang,
                include_files,
                false,
                true, // is_root = true
                &mut visited,
            ) {
                for (root_node, _) in root_nodes {
                    if !root_node.children.is_empty() || root_node.file_count > 0 {
                        roots.push(root_node);
                    }
                }
            }
        }
    }

        // 顶层根节点排序：sort_order (>0 升序) 为主判据，其次 ROOT_DIMENSION_CONCEPTS 规范概念序，再次 code 字典序。
        // 主判据与回退路径 (L1300 一带) 及 desktop 服务端 (TagTreeQuery) 的 `order > 0` 语义保持一致。
        roots.sort_by(|a, b| {
            let root_rank = |code: &str| -> usize {
                omni_core::ROOT_DIMENSION_CONCEPTS
                    .iter()
                    .position(|concept_item| concept_item.code() == code)
                    .unwrap_or(usize::MAX)
            };
            let order_a = if a.sort_order > 0 { a.sort_order } else { i64::MAX };
            let order_b = if b.sort_order > 0 { b.sort_order } else { i64::MAX };
            if order_a != order_b {
                order_a.cmp(&order_b)
            } else {
                let rank_a = root_rank(&a.code);
                let rank_b = root_rank(&b.code);
                if rank_a != rank_b {
                    rank_a.cmp(&rank_b)
                } else {
                    a.code.cmp(&b.code)
                }
            }
        });

    fn count_nodes(nodes: &[TaxonomyNode]) -> usize {
        let mut count = nodes.len();
        for node in nodes {
            count += count_nodes(&node.children);
        }
        count
    }

    let total_nodes = count_nodes(&roots);
    let elapsed = t_start.elapsed();
    tracing::info!(
        "[TaxonomyTreeFast] 倒排 Trie 树装配完成: roots={}, total_nodes={}, 耗时={:?}",
        roots.len(),
        total_nodes,
        elapsed
    );

    Ok(TaxonomyTreeResponse {
        locale: locale.to_string(),
        root_nodes: roots,
        total_nodes,
    })
}

/// 内部逻辑：从 SQLite 连接中提取并组装分类树
pub fn query_taxonomy_tree(
    conn: &Connection,
    locale: &str,
    root_filter: Option<&str>,
    master_db_path: Option<&std::path::Path>,
    include_files: bool,
    workspace_id: Option<i64>,
    directory_prefix: Option<&str>,
) -> anyhow::Result<TaxonomyTreeResponse> {
    // 若提供业务主库路径且有效，优先且强制走倒排 Trie 极速分类树引擎 (ADR-0038 / ADR-0053 / P95 <= 5ms)
    if let Some(path) = master_db_path {
        if path.exists() {
            if let Ok(master_conn) = open_omni_readonly_connection(path) {
                return query_taxonomy_tree_fast(
                    conn,
                    locale,
                    root_filter,
                    &master_conn,
                    include_files,
                    workspace_id,
                    directory_prefix,
                );
            }
        }
    }

    // 1. 检查 file_tags 表是否存在 (用于离线/只读无主库轻量回退)
    let has_file_tags: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'file_tags'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);

    if !has_file_tags {
        return Ok(TaxonomyTreeResponse {
            locale: locale.to_string(),
            root_nodes: Vec::new(),
            total_nodes: 0,
        });
    }

    // 2. 预载入别名与规范名映射表 (优化展示名注入)
    let alias_map = query_canonical_and_aliases(conn, locale).unwrap_or_default();

    // 3. 动态探测 file_tags 表列，兼容只读语义包 (含 sort_order / _debug_name_zh, 无 name) 与本地主库 (含 name, 无 sort_order)
    let mut has_name_col = false;
    let mut has_sort_order_col = false;
    let mut has_debug_name_col = false;
    if let Ok(mut info_stmt) = conn.prepare("PRAGMA table_info(file_tags)") {
        if let Ok(rows) = info_stmt.query_map([], |r| r.get::<_, String>(1)) {
            for col_name in rows.flatten() {
                if col_name == "name" {
                    has_name_col = true;
                } else if col_name == "sort_order" {
                    has_sort_order_col = true;
                } else if col_name == "_debug_name_zh" {
                    has_debug_name_col = true;
                }
            }
        }
    }

    let name_expr = if has_name_col {
        "name"
    } else if has_debug_name_col {
        "_debug_name_zh"
    } else {
        "'' AS name"
    };

    let sort_order_expr = if has_sort_order_col {
        "sort_order"
    } else {
        "0 AS sort_order"
    };

    let select_sql = format!(
        "SELECT code, {}, parent_codes, source, {} FROM file_tags WHERE source IN ('dimension', 'tag', 'builtin') ORDER BY code",
        name_expr, sort_order_expr
    );
    let mut stmt = conn.prepare(&select_sql)?;

    struct RawTag {
        code: String,
        default_name: String,
        parent_codes: Vec<String>,
        source: String,
        _pack_source: String,
        sort_order: i64,
    }

    let mut raw_tags: Vec<RawTag> = Vec::new();
    let rows = stmt.query_map([], |row| {
        let code: String = row.get(0)?;
        let name: Option<String> = row.get(1)?;
        let parent_codes_raw: String = row.get(2).unwrap_or_else(|_| "[]".to_string());
        let source: Option<String> = row.get(3)?;
        let sort_order: Option<i64> = row.get(4)?;
        Ok((code, name, parent_codes_raw, source, sort_order))
    })?;

    let mut pack_source_map: HashMap<String, String> = HashMap::new();
    for row in rows {
        let (code, default_name_opt, parent_codes_raw, pack_source, sort_order) = row?;
        let default_name = default_name_opt.unwrap_or_default();
        let parent_codes: Vec<String> = serde_json::from_str(&parent_codes_raw).unwrap_or_default();
        let pack_source_str = pack_source.unwrap_or_default();
        pack_source_map.insert(code.clone(), pack_source_str.clone());

        // TreeNode.source = code 前缀分区（与 pack source 语义正交：前者 code 身份，后者 taxonomy 来源）
        let source = if code.starts_with("builtin.") {
            "builtin"
        } else if code.starts_with("omw.") {
            "omw"
        } else if code.starts_with("_ext.") {
            "_ext"
        } else {
            "user"
        };
        // builtin 中文命中 omw 时 pack 已回写 sort_order；未写则 0
        let sort_order = sort_order.unwrap_or(0);
        raw_tags.push(RawTag {
            code,
            default_name,
            parent_codes,
            source: source.to_string(),
            _pack_source: pack_source_str,
            sort_order,
        });
    }

    // 4. 为每个标签确定注入的本地化展示名
    let mut node_map: BTreeMap<String, TaxonomyNode> = BTreeMap::new();
    for raw in raw_tags {
        // 展示名优先级：alias_map 匹配的 canonical_name -> default_name -> code
        let display_name = if let Some((canonical, _)) = alias_map.get(&raw.code) {
            canonical.clone()
        } else if !raw.default_name.trim().is_empty() {
            raw.default_name.clone()
        } else {
            raw.code.clone()
        };

        let primary_parent = raw
            .parent_codes
            .iter()
            .find(|p| pack_source_map.contains_key(*p) && *p != &raw.code)
            .cloned()
            .or_else(|| raw.parent_codes.first().cloned());
        // 特例：builtin.content_tags 恒定垫底，与 fast path 的 9999 硬编码口径一致（pack 值 28 不参与根级排序）
        let sort_order = if raw.code == omni_core::concepts::Concept::内容标签.code() {
            9999
        } else {
            raw.sort_order
        };
        node_map.insert(
            raw.code.clone(),
            TaxonomyNode {
                code: raw.code,
                name: display_name,
                parent_code: primary_parent,
                parent_codes: raw.parent_codes,
                source: raw.source,
                sort_order,
                file_count: 0,
                files: Vec::new(),
                children: Vec::new(),
                code_path: None,
                name_path: None,
            },
        );
    }

    // 5. 组装多叉树森林与文件关联 (支持直读桌面主库 file_tag_relations)
    let mut direct_files: HashMap<String, HashSet<String>> = HashMap::new();
    let mut parent_to_children: HashMap<String, Vec<String>> = HashMap::new();
    let mut root_candidates: Vec<String> = Vec::new();

    // 5.1 确保 7 大受控根维度节点完整存在于 node_map，并具备准确的当前母语规范展示名
    let lang = locale.split('-').next().unwrap_or("zh");
    for root_concept in omni_core::ROOT_DIMENSION_CONCEPTS {
        let code = root_concept.code().to_string();
        let display_name = alias_map
            .get(&code)
            .map(|(c, _)| c.clone())
            .unwrap_or_else(|| omni_core::tag_identity::tag_display(&code, lang));

        if let Some(existing_node) = node_map.get_mut(&code) {
            // 若已有节点的展示名为裸 code 或空，使用当前母语规范名修复
            if existing_node.name == code || existing_node.name.is_empty() {
                existing_node.name = display_name;
            }
        } else {
            node_map.insert(
                code.clone(),
                TaxonomyNode {
                    code: code.clone(),
                    name: display_name,
                    parent_code: None,
                    parent_codes: Vec::new(),
                    source: "builtin".to_string(),
                    sort_order: if code == omni_core::concepts::Concept::内容标签.code() { 9999 } else { 0 },
                    file_count: 0,
                    files: Vec::new(),
                    children: Vec::new(),
                    code_path: None,
                    name_path: None,
                },
            );
        }
        pack_source_map.entry(code).or_insert_with(|| "dimension".to_string());
    }

    // 5.2 若提供业务主库路径，只读加载 file_tag_relations 中的文件归属与父级链
    if let Some(path) = master_db_path {
        if path.exists() {
            if let Ok(master_conn) = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) {
                let has_ftr: bool = master_conn
                    .query_row(
                        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'file_tag_relations'",
                        [],
                        |_| Ok(true),
                    )
                    .unwrap_or(false);

                if has_ftr {
                    let mut has_code_path = false;
                    let mut has_name_path = false;
                    let mut _has_depth = false;
                    if let Ok(mut col_stmt) = master_conn.prepare("PRAGMA table_info(file_tag_relations)") {
                        if let Ok(rows) = col_stmt.query_map([], |r| r.get::<_, String>(1)) {
                            for col in rows.flatten() {
                                if col == "code_path" {
                                    has_code_path = true;
                                } else if col == "name_path" {
                                    has_name_path = true;
                                } else if col == "depth" {
                                    _has_depth = true;
                                }
                            }
                        }
                    }

                    let has_wf: bool = master_conn
                        .query_row(
                            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'workspace_files'",
                            [],
                            |_| Ok(true),
                        )
                        .unwrap_or(false);

                    let mut where_clauses = vec!["wf.status = 1".to_string()];
                    let mut params: Vec<rusqlite::types::Value> = Vec::new();

                    if let Some(ws_id) = workspace_id {
                        where_clauses.push("wf.workspace_id = ?".to_string());
                        params.push(rusqlite::types::Value::Integer(ws_id));
                    }

                    if let Some(prefix) = directory_prefix {
                        let clean_prefix = prefix.trim();
                        if !clean_prefix.is_empty() {
                            let sep = if clean_prefix.contains('\\') { '\\' } else { '/' };
                            let prefix_pattern = if clean_prefix.ends_with('/') || clean_prefix.ends_with('\\') {
                                format!("{}%", clean_prefix)
                            } else {
                                format!("{}{}%", clean_prefix, sep)
                            };
                            where_clauses.push("(wf.path LIKE ? OR wf.path = ?)".to_string());
                            params.push(rusqlite::types::Value::Text(prefix_pattern));
                            params.push(rusqlite::types::Value::Text(clean_prefix.to_string()));
                        }
                    }

                    let col_name_expr = if has_name_path {
                        "ftr.name_path"
                    } else {
                        "''"
                    };
                    let col_code_expr = if has_code_path {
                        "ftr.code_path"
                    } else {
                        "''"
                    };

                    let select_sql = if has_wf && !params.is_empty() {
                        format!(
                            "SELECT DISTINCT ftr.file_fingerprint, ftr.tag_code, ftr.via_parent_code, {}, {} \
                             FROM file_tag_relations ftr \
                             JOIN workspace_files wf ON wf.file_fingerprint = ftr.file_fingerprint \
                             WHERE {}",
                            col_name_expr,
                            col_code_expr,
                            where_clauses.join(" AND ")
                        )
                    } else {
                        let col_name_raw = if has_name_path {
                            "name_path"
                        } else {
                            "''"
                        };
                        let col_code_raw = if has_code_path {
                            "code_path"
                        } else {
                            "''"
                        };
                        format!(
                            "SELECT file_fingerprint, tag_code, via_parent_code, {}, {} FROM file_tag_relations",
                            col_name_raw,
                            col_code_raw
                        )
                    };

                    if let Ok(mut stmt) = master_conn.prepare(&select_sql) {
                        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p as &dyn rusqlite::ToSql).collect();
                        let rows = stmt.query_map(param_refs.as_slice(), |row| {
                            let fp: String = row.get(0)?;
                            let tag_code: String = row.get(1)?;
                            let via: String = row.get(2).unwrap_or_default();
                            let name_chain: String = row.get(3).unwrap_or_default();
                            let code_chain: String = row.get(4).unwrap_or_default();
                            Ok((fp, tag_code, via, name_chain, code_chain))
                        });

                        if let Ok(rows) = rows {
                            for r in rows.flatten() {
                                let (fp, tag_code, via, name_chain, code_chain) = r;
                                direct_files.entry(tag_code.clone()).or_default().insert(fp.clone());

                                // 处理物化路径 (例如 "/文件类型/图片/主体类型/鸭子" 或 "/builtin.file_type/builtin.image/builtin.subject_type/omw.01846331.n")
                                let has_name_chain = !name_chain.trim().is_empty();
                                let has_code_chain = !code_chain.trim().is_empty();
                                if has_name_chain || has_code_chain {
                                    let name_segs: Vec<String> = if has_name_chain {
                                        name_chain
                                            .split('/')
                                            .map(|s| s.trim().to_string())
                                            .filter(|s| !s.is_empty() && s != omni_core::concepts::Concept::内容标签.zh_name() && s != omni_core::concepts::Concept::内容标签.code())
                                            .collect()
                                    } else {
                                        Vec::new()
                                    };
                                    let code_segs: Vec<String> = if has_code_chain {
                                        code_chain
                                            .split('/')
                                            .map(|s| s.trim().to_string())
                                            .filter(|s| !s.is_empty() && s != omni_core::concepts::Concept::内容标签.zh_name() && s != omni_core::concepts::Concept::内容标签.code())
                                            .collect()
                                    } else {
                                        Vec::new()
                                    };

                                    let num_segs = code_segs.len().max(name_segs.len());
                                    if num_segs > 0 {
                                        let mut first_seg_code = if !code_segs.is_empty() && !code_segs[0].is_empty() {
                                            code_segs[0].clone()
                                        } else if num_segs == 1 {
                                            tag_code.clone()
                                        } else {
                                            format!("builtin.cat.{}", name_segs[0])
                                        };

                                        // 归一化兼容历史驼峰 builtin.fileType
                                        if first_seg_code == "builtin.fileType" {
                                            first_seg_code = omni_core::concepts::Concept::文件类型.code().to_string();
                                        }

                                        // 原则 2：只有 7 大受控根维度允许作为独立顶层树根
                                        let is_root_dim = omni_core::is_root_dimension(&first_seg_code);

                                        let (mut prev_code, start_idx) = if is_root_dim {
                                            (first_seg_code, 1)
                                        } else {
                                            // 原则 1：非 7 大根维度链路，统一安全收敛挂入 builtin.content_tags
                                            (omni_core::concepts::Concept::内容标签.code().to_string(), 0)
                                        };

                                        for i in start_idx..num_segs {
                                            let seg_code = if i < code_segs.len() && !code_segs[i].is_empty() {
                                                code_segs[i].clone()
                                            } else if i == num_segs - 1 {
                                                tag_code.clone()
                                            } else if i < name_segs.len() {
                                                format!("builtin.cat.{}", name_segs[i])
                                            } else {
                                                tag_code.clone()
                                            };

                                            let seg_name = if i < name_segs.len() && !name_segs[i].is_empty() {
                                                name_segs[i].clone()
                                            } else if let Some(n) = node_map.get(&seg_code) {
                                                n.name.clone()
                                            } else if let Some((canonical, _)) = alias_map.get(&seg_code) {
                                                canonical.clone()
                                            } else {
                                                let lang = locale.split('-').next().unwrap_or("zh");
                                                omni_core::tag_identity::tag_display(&seg_code, lang)
                                            };

                                            if !node_map.contains_key(&seg_code) {
                                                node_map.insert(
                                                    seg_code.clone(),
                                                    TaxonomyNode {
                                                        code: seg_code.clone(),
                                                        name: seg_name.clone(),
                                                        parent_code: Some(prev_code.clone()),
                                                        parent_codes: vec![prev_code.clone()],
                                                        source: "chain".to_string(),
                                                        sort_order: 9999,
                                                        file_count: 0,
                                                        files: Vec::new(),
                                                        children: Vec::new(),
                                                        code_path: None,
                                                        name_path: None,
                                                    },
                                                );
                                            }

                                            if prev_code != seg_code {
                                                let children = parent_to_children.entry(prev_code.clone()).or_default();
                                                if !children.contains(&seg_code) {
                                                    children.push(seg_code.clone());
                                                }
                                            }
                                            prev_code = seg_code;
                                        }
                                    }
                                } else if !via.trim().is_empty() && via != tag_code {
                                    if !node_map.contains_key(&tag_code) {
                                        let lang = locale.split('-').next().unwrap_or("zh");
                                        let name = alias_map
                                            .get(&tag_code)
                                            .map(|(c, _)| c.clone())
                                            .unwrap_or_else(|| omni_core::tag_identity::tag_display(&tag_code, lang));
                                        node_map.insert(
                                            tag_code.clone(),
                                            TaxonomyNode {
                                                code: tag_code.clone(),
                                                name,
                                                parent_code: Some(via.clone()),
                                                parent_codes: vec![via.clone()],
                                                source: "rel".to_string(),
                                                sort_order: 9999,
                                                file_count: 0,
                                                files: Vec::new(),
                                                children: Vec::new(),
                                                code_path: None,
                                                name_path: None,
                                            },
                                        );
                                    }
                                    let children = parent_to_children.entry(via.clone()).or_default();
                                    if !children.contains(&tag_code) {
                                        children.push(tag_code.clone());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // 5.3 补齐受控语义库中所有已有节点的父子邻接映射
    for (code, node) in &node_map {
        if let Some(ref p) = node.parent_code {
            if node_map.contains_key(p) && p != code {
                let children = parent_to_children.entry(p.clone()).or_default();
                if !children.contains(code) {
                    children.push(code.clone());
                }
            } else {
                // 原则 2 双保险：拓扑无父级维度 + 强类型 ROOT_DIMENSION_CONCEPTS 校验
                let is_root = omni_core::is_root_dimension(code)
                    || (pack_source_map.get(code).map(|s| s == "dimension").unwrap_or(false)
                        && node.parent_codes.is_empty());
                let code_content_tags = omni_core::concepts::Concept::内容标签.code();
                if is_root && !root_candidates.contains(code) {
                    root_candidates.push(code.clone());
                } else if !is_root && code != code_content_tags {
                    // 原则 1：未挂在已知父级且非 7 大根维度的节点，统一收拢归入 builtin.content_tags
                    let children = parent_to_children.entry(code_content_tags.to_string()).or_default();
                    if !children.contains(code) {
                        children.push(code.clone());
                    }
                }
            }
        } else {
            // 原则 2 双保险：拓扑无父级维度 + 强类型 ROOT_DIMENSION_CONCEPTS 校验
            let is_root = omni_core::is_root_dimension(code)
                || (pack_source_map.get(code).map(|s| s == "dimension").unwrap_or(false)
                    && node.parent_codes.is_empty());
            let code_content_tags = omni_core::concepts::Concept::内容标签.code();
            if is_root && !root_candidates.contains(code) {
                root_candidates.push(code.clone());
            } else if !is_root && code != code_content_tags {
                // 原则 1：未挂在已知父级且非 7 大根维度的节点，统一收拢归入 builtin.content_tags
                let children = parent_to_children.entry(code_content_tags.to_string()).or_default();
                if !children.contains(code) {
                    children.push(code.clone());
                }
            }
        }
    }

    // 递归组装树节点并自底向上后序遍历计算唯一文件合计 (数学公理：真实父级与逻辑父级一视同仁)
    fn build_subtree(
        code: &str,
        node_map: &BTreeMap<String, TaxonomyNode>,
        parent_to_children: &HashMap<String, Vec<String>>,
        visited: &mut HashSet<String>,
        direct_files: &HashMap<String, HashSet<String>>,
        include_files: bool,
    ) -> Option<(TaxonomyNode, HashSet<String>)> {
        if visited.contains(code) {
            return None; // 防环保护
        }
        visited.insert(code.to_string());

        let base_node = node_map.get(code)?;
        let mut children = Vec::new();
        let mut aggregate_files = HashSet::new();

        if let Some(dfs) = direct_files.get(code) {
            aggregate_files.extend(dfs.iter().cloned());
        }
        if let Some(dfs) = direct_files.get(&base_node.name) {
            aggregate_files.extend(dfs.iter().cloned());
        }

        if let Some(child_codes) = parent_to_children.get(code) {
            for child_code in child_codes {
                if let Some((child_node, child_files)) = build_subtree(child_code, node_map, parent_to_children, visited, direct_files, include_files) {
                    aggregate_files.extend(child_files);
                    children.push(child_node);
                }
            }
        }
        // 子节点按 sort_order 升序优先 (sort_order > 0)，相同或未标注时按 code 稳定排序
        children.sort_by(|a, b| {
            let order_a = if a.sort_order > 0 { a.sort_order } else { i64::MAX };
            let order_b = if b.sort_order > 0 { b.sort_order } else { i64::MAX };
            if order_a != order_b {
                order_a.cmp(&order_b)
            } else {
                a.code.cmp(&b.code)
            }
        });

        visited.remove(code);

        let file_count = aggregate_files.len();
        let files = if include_files {
            let mut sorted_files: Vec<String> = aggregate_files.iter().cloned().collect();
            sorted_files.sort();
            sorted_files
        } else {
            Vec::new()
        };

        let node = TaxonomyNode {
            code: base_node.code.clone(),
            name: base_node.name.clone(),
            parent_code: base_node.parent_code.clone(),
            parent_codes: base_node.parent_codes.clone(),
            source: base_node.source.clone(),
            sort_order: base_node.sort_order,
            file_count,
            files,
            children,
            code_path: base_node.code_path.clone(),
            name_path: base_node.name_path.clone(),
        };

        Some((node, aggregate_files))
    }

    let mut roots: Vec<TaxonomyNode> = Vec::new();
    let mut visited = HashSet::new();

    if let Some(target_root) = root_filter {
        if let Some((root_node, _)) = build_subtree(target_root, &node_map, &parent_to_children, &mut visited, &direct_files, include_files) {
            // 对于根级，如果其下没有子标签且自身无文件，则根级数据不应输出
            if !root_node.children.is_empty() || root_node.file_count > 0 {
                roots.push(root_node);
            }
        }
    } else {
        root_candidates.retain(|c| omni_core::is_root_dimension(c));
        root_candidates.sort();
        let mut candidate_nodes: Vec<TaxonomyNode> = Vec::new();
        for root_code in root_candidates {
            if let Some((root_node, _)) = build_subtree(&root_code, &node_map, &parent_to_children, &mut visited, &direct_files, include_files) {
                // 对于根级，如果其下没有子标签且自身无文件，则根级数据不应输出
                if !root_node.children.is_empty() || root_node.file_count > 0 {
                    candidate_nodes.push(root_node);
                }
            }
        }

        // 顶层主干根节点排序：按 sort_order 升序优先 (sort_order > 0)，次之 builtin.* 优先，再按 code 字典序稳定排序
        candidate_nodes.sort_by(|a, b| {
            let order_a = if a.sort_order > 0 { a.sort_order } else { i64::MAX };
            let order_b = if b.sort_order > 0 { b.sort_order } else { i64::MAX };
            if order_a != order_b {
                order_a.cmp(&order_b)
            } else {
                let is_builtin_a = a.code.starts_with("builtin.");
                let is_builtin_b = b.code.starts_with("builtin.");
                if is_builtin_a != is_builtin_b {
                    is_builtin_b.cmp(&is_builtin_a)
                } else {
                    a.code.cmp(&b.code)
                }
            }
        });

        // 根级展示名去重：相同名称只保留第一个规范主干维度，彻底杜绝出现重复根级（如多个“背景”）
        let mut seen_root_names = HashSet::new();
        for node in candidate_nodes {
            if !seen_root_names.contains(&node.name) {
                seen_root_names.insert(node.name.clone());
                roots.push(node);
            }
        }
    }

    fn count_nodes(nodes: &[TaxonomyNode]) -> usize {
        let mut count = nodes.len();
        for node in nodes {
            count += count_nodes(&node.children);
        }
        count
    }

    let total_nodes = count_nodes(&roots);

    Ok(TaxonomyTreeResponse {
        locale: locale.to_string(),
        root_nodes: roots,
        total_nodes,
    })
}

/// 表是否存在
fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |_| Ok(true),
    )
    .unwrap_or(false)
}

/// 从指定语言分表装载 alias/canonical（支持 prefix 过滤）
fn load_alias_rows_from_table(
    conn: &Connection,
    table: &str,
    prefix_patterns: Option<&(String, String)>,
) -> anyhow::Result<(HashMap<String, Vec<String>>, HashMap<String, String>)> {
    let mut aliases_by_code: HashMap<String, Vec<String>> = HashMap::new();
    let mut canonical_by_code: HashMap<String, String> = HashMap::new();

    let sql = if prefix_patterns.is_some() {
        format!(
            "SELECT tag_code, lemma, is_canonical FROM {table} WHERE (tag_code = ?1 OR tag_code LIKE ?2) ORDER BY is_canonical DESC, count DESC, lemma ASC"
        )
    } else {
        format!(
            "SELECT tag_code, lemma, is_canonical FROM {table} ORDER BY is_canonical DESC, count DESC, lemma ASC"
        )
    };

    let mut stmt = conn.prepare(&sql)?;
    let mut raw_items: Vec<(String, String, bool)> = Vec::new();
    if let Some((exact, pat)) = prefix_patterns {
        let mut rows = stmt.query(rusqlite::params![exact, pat])?;
        while let Some(row) = rows.next()? {
            let tag_code: String = row.get(0)?;
            let lemma: String = row.get(1)?;
            let is_canonical: i64 = row.get(2).unwrap_or(0);
            raw_items.push((tag_code, lemma, is_canonical == 1));
        }
    } else {
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let tag_code: String = row.get(0)?;
            let lemma: String = row.get(1)?;
            let is_canonical: i64 = row.get(2).unwrap_or(0);
            raw_items.push((tag_code, lemma, is_canonical == 1));
        }
    }

    for (tag_code, lemma, is_canonical) in raw_items {
        let entry = aliases_by_code.entry(tag_code.clone()).or_default();
        if !entry.contains(&lemma) {
            if is_canonical {
                entry.insert(0, lemma.clone());
            } else {
                entry.push(lemma.clone());
            }
        }
        if is_canonical && !canonical_by_code.contains_key(&tag_code) {
            canonical_by_code.insert(tag_code, lemma);
        }
    }

    Ok((aliases_by_code, canonical_by_code))
}

/// 展示级联：目标语言 canonical 缺失时回退 `tag_aliases_en_US`（不改识别路径）
fn fill_canonical_from_en_fallback(
    conn: &Connection,
    prefix_patterns: Option<&(String, String)>,
    aliases_by_code: &mut HashMap<String, Vec<String>>,
    canonical_by_code: &mut HashMap<String, String>,
) {
    const EN_TABLE: &str = "tag_aliases_en_US";
    if !table_exists(conn, EN_TABLE) {
        return;
    }
    let Ok((en_aliases, en_canonical)) = load_alias_rows_from_table(conn, EN_TABLE, prefix_patterns)
    else {
        return;
    };
    for (code, lemma) in en_canonical {
        if canonical_by_code.contains_key(&code) {
            continue;
        }
        canonical_by_code.insert(code.clone(), lemma.clone());
        if !aliases_by_code.contains_key(&code) {
            aliases_by_code.insert(code, vec![lemma]);
        }
    }
    // 有英文词形但尚无 canonical 的 code：用英文列表首位兜底
    for (code, list) in en_aliases {
        if !canonical_by_code.contains_key(&code) {
            if let Some(first) = list.first() {
                canonical_by_code.insert(code.clone(), first.clone());
            }
        }
    }
}

/// 内部逻辑：提取指定 locale 下所有标签的别名和规范展示名 (Issue #684)
/// 优先使用纯净语言分表 tag_aliases_{lang}，回退旧表 tag_aliases，支持 prefix 过滤
/// 展示级联（slice 03）：目标语言 canonical 缺失时回退 en-US 母本，避免 UI 露出 code/slug
pub fn query_taxonomy_aliases(
    conn: &Connection,
    locale: &str,
    prefix_filter: Option<&str>,
) -> anyhow::Result<TaxonomyAliasesResponse> {
    let target_table = omni_pro::resolve_tag_aliases_table(locale);
    let has_target_table = table_exists(conn, target_table);

    let has_legacy_table: bool = if !has_target_table {
        table_exists(conn, "tag_aliases")
    } else {
        false
    };

    if !has_target_table && !has_legacy_table {
        return Ok(TaxonomyAliasesResponse {
            locale: locale.to_string(),
            aliases: HashMap::new(),
            canonical_names: HashMap::new(),
            total_tags: 0,
        });
    }

    let mut aliases_by_code: HashMap<String, Vec<String>> = HashMap::new();
    let mut canonical_by_code: HashMap<String, String> = HashMap::new();

    let prefix_patterns = prefix_filter.and_then(|p| {
        let trimmed = p.trim().trim_end_matches('.').trim_end_matches('%');
        if trimmed.is_empty() {
            None
        } else {
            Some((trimmed.to_string(), format!("{}.%", trimmed)))
        }
    });

    if has_target_table {
        let (a, c) = load_alias_rows_from_table(conn, target_table, prefix_patterns.as_ref())?;
        aliases_by_code.extend(a);
        canonical_by_code.extend(c);
    } else {
        let sql = if prefix_patterns.is_some() {
            "SELECT tag_code, lemma, is_canonical, locale FROM tag_aliases WHERE (tag_code = ?1 OR tag_code LIKE ?2) ORDER BY is_canonical DESC, lemma ASC"
        } else {
            "SELECT tag_code, lemma, is_canonical, locale FROM tag_aliases ORDER BY is_canonical DESC, lemma ASC"
        };
        let mut stmt = conn.prepare(sql)?;
        let lang_prefix = locale.split(['-', '_']).next().unwrap_or(locale);

        let mut raw_legacy: Vec<(String, String, bool, String)> = Vec::new();
        if let Some((ref exact, ref pat)) = prefix_patterns {
            let mut rows = stmt.query(rusqlite::params![exact, pat])?;
            while let Some(row) = rows.next()? {
                let tag_code: String = row.get(0)?;
                let lemma: String = row.get(1)?;
                let is_canonical: i64 = row.get(2).unwrap_or(0);
                let loc: String = row.get(3).unwrap_or_default();
                raw_legacy.push((tag_code, lemma, is_canonical == 1, loc));
            }
        } else {
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let tag_code: String = row.get(0)?;
                let lemma: String = row.get(1)?;
                let is_canonical: i64 = row.get(2).unwrap_or(0);
                let loc: String = row.get(3).unwrap_or_default();
                raw_legacy.push((tag_code, lemma, is_canonical == 1, loc));
            }
        }

        for (tag_code, lemma, is_canonical, loc) in raw_legacy {
            let is_match = loc.eq_ignore_ascii_case(locale)
                || loc.split(['-', '_']).next().unwrap_or(&loc).eq_ignore_ascii_case(lang_prefix);

            if !is_match && !locale.is_empty() {
                continue;
            }

            let entry = aliases_by_code.entry(tag_code.clone()).or_default();
            if !entry.contains(&lemma) {
                if is_canonical {
                    entry.insert(0, lemma.clone());
                } else {
                    entry.push(lemma.clone());
                }
            }

            if is_canonical && !canonical_by_code.contains_key(&tag_code) {
                canonical_by_code.insert(tag_code, lemma);
            }
        }
    }

    // 若某个 tag_code 有别名但没有显式 canonical，以第一个别名作为 canonical
    for (code, list) in &aliases_by_code {
        if !canonical_by_code.contains_key(code) {
            if let Some(first) = list.first() {
                canonical_by_code.insert(code.clone(), first.clone());
            }
        }
    }

    // 展示级联：目标语言缺 canonical 时回退 en-US（识别路径不受影响）
    if target_table != "tag_aliases_en_US" {
        fill_canonical_from_en_fallback(
            conn,
            prefix_patterns.as_ref(),
            &mut aliases_by_code,
            &mut canonical_by_code,
        );
    }

    let total_tags = aliases_by_code.len();

    Ok(TaxonomyAliasesResponse {
        locale: locale.to_string(),
        aliases: aliases_by_code,
        canonical_names: canonical_by_code,
        total_tags,
    })
}

/// 内部辅助：查询 (canonical_name, all_aliases)
fn query_canonical_and_aliases(
    conn: &Connection,
    locale: &str,
) -> anyhow::Result<HashMap<String, (String, Vec<String>)>> {
    let aliases_resp = query_taxonomy_aliases(conn, locale, None)?;
    let mut map = HashMap::new();
    for (code, list) in aliases_resp.aliases {
        let canonical = aliases_resp.canonical_names.get(&code).cloned().unwrap_or_else(|| {
            list.first().cloned().unwrap_or_default()
        });
        map.insert(code, (canonical, list));
    }
    Ok(map)
}

/// 内部逻辑：按 locale + source/codes 双参提取 tag_aliases_{lang} 行数组（主库分表镜像契约）
/// - `source`：逗号分隔受控 source 列表（如 tag,dimension），仅返回 file_tags.source 命中的受控 code 行；
/// - `codes`：逗号分隔精确 tag_code 列表，未命中 code 不出行；
/// - `prefix`：兼容保留的旧前缀过滤；
/// - source/codes 均未指定：返回该语言分表全量行（供旧消费方全量拉取）。
pub fn query_tag_alias_rows(
    conn: &Connection,
    locale: &str,
    source_filter: Option<&str>,
    codes_filter: Option<&str>,
    prefix_filter: Option<&str>,
) -> anyhow::Result<Vec<TagAliasRow>> {
    let target_table = omni_pro::resolve_tag_aliases_table(locale);
    if !table_exists(conn, target_table) {
        return Ok(Vec::new());
    }

    let base_columns = "tag_code, lemma, is_canonical, n, count";
    let mut where_parts: Vec<String> = Vec::new();
    let mut params: Vec<String> = Vec::new();

    let codes = parse_csv_param(codes_filter);
    if !codes.is_empty() {
        // codes 精确匹配：未命中 code 不出行
        let placeholders = vec!["?"; codes.len()].join(",");
        where_parts.push(format!("tag_code IN ({placeholders})"));
        params.extend(codes);
    } else {
        let sources = parse_csv_param(source_filter);
        if !sources.is_empty() {
            // source 初值镜像：仅返回 file_tags.source 命中的受控 code 行（生成语义包治理字段）
            let placeholders = vec!["?"; sources.len()].join(",");
            where_parts.push(format!(
                "tag_code IN (SELECT code FROM file_tags WHERE source IN ({placeholders}))"
            ));
            params.extend(sources);
        }
    }

    if let Some(p) = prefix_filter {
        let trimmed = p.trim().trim_end_matches('.').trim_end_matches('%');
        if !trimmed.is_empty() {
            where_parts.push("(tag_code = ? OR tag_code LIKE ?)".to_string());
            params.push(trimmed.to_string());
            params.push(format!("{}.%", trimmed));
        }
    }

    let sql = if where_parts.is_empty() {
        format!(
            "SELECT {base_columns} FROM {target_table} ORDER BY tag_code, is_canonical DESC, count DESC, lemma ASC"
        )
    } else {
        format!(
            "SELECT {base_columns} FROM {target_table} WHERE {} ORDER BY is_canonical DESC, count DESC, lemma ASC",
            where_parts.join(" AND ")
        )
    };

    let mut stmt = conn.prepare(&sql)?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p as &dyn rusqlite::ToSql).collect();
    let mut rows_iter = stmt.query(param_refs.as_slice())?;

    let mut rows: Vec<TagAliasRow> = Vec::new();
    while let Some(row) = rows_iter.next()? {
        rows.push(TagAliasRow {
            tag_code: row.get(0)?,
            lemma: row.get(1)?,
            is_canonical: row.get(2)?,
            n: row.get(3)?,
            count: row.get(4)?,
        });
    }
    Ok(rows)
}

/// POST /api/taxonomy/fast-recognize 与 POST /api/v1/taxonomy/fast-recognize
/// 端侧纯 CPU 两阶段快速语义标签识别端点 (Slice 3, Issue #680)
pub async fn fast_recognize_handler(
    State(state): State<AppState>,
    Json(ctx): Json<omni_pro::text::FastRecognizeContext>,
) -> Json<omni_pro::text::FastRecognizeResponse> {
    let embedder = omni_pro::text::BekkoEmbedder::new();

    // 尝试获取常驻预固化向量表（带全局缓存）
    static GLOBAL_VECTOR_TABLE: std::sync::OnceLock<Option<omni_pro::text::PrecomputedVectorTable>> =
        std::sync::OnceLock::new();
    let vector_table_ref = GLOBAL_VECTOR_TABLE.get_or_init(|| {
        omni_pro::semantic_loader::SemanticPackLoader::load_dense_embeddings().ok()
    });

    let res = state.omw.with_conn(|conn| {
        Ok(omni_pro::text::FastTagRecognizer::recognize(
            &ctx,
            Some(conn),
            vector_table_ref.as_ref(),
            &embedder,
        ))
    });

    match res {
        Ok(outcome) => Json(outcome),
        Err(err) => {
            tracing::warn!("[fast_recognize_handler] 数据库离线或任务异常，执行降级纯文本识别: {err}");
            let outcome = omni_pro::text::FastTagRecognizer::recognize(
                &ctx,
                None,
                vector_table_ref.as_ref(),
                &embedder,
            );
            Json(outcome)
        }
    }
}

/// 从 SQLite 桌面主库只读加载 DIMENSION_POLICIES 策略配置（DEC-03 / DEC-04）
///
/// **零降级（2026-10-14 用户裁定）**：主库路径已显式给出时，「打开失败 / 无 `system_config`
/// 表 / JSON 解析失败 / 读取报错」一律属"数据库错"，直接 panic 崩溃 —— 静默回退物化默认策略
/// 会让云端下发的策略（当前 95 条）与运行时实际生效策略静默失真。
///
/// 仅两条软路径被允许，且都必须显式上报：① 路径尚不存在（首次启动建库前的合法瞬态）；
/// ② `system_config` 存在但尚无 `DIMENSION_POLICIES` 行（云端配置尚未落库）。
pub fn load_dimension_policies_from_db(
    master_db_path: Option<&std::path::Path>,
) -> HashMap<String, omni_core::DimensionExecutionPolicy> {
    let mut policies = omni_core::get_default_dimension_policies();
    let Some(path) = master_db_path else {
        return policies;
    };
    if !path.exists() {
        tracing::warn!(
            "[load_dimension_policies_from_db] 主库路径尚不存在（{}），暂用物化默认策略；建库后 reconnect 会重载",
            path.display()
        );
        return policies;
    }

    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap_or_else(|err| {
            tracing::error!("[load_dimension_policies_from_db] 主库打开失败（{}）: {err:#}", path.display());
            panic!(
                "[load_dimension_policies_from_db] 主库只读打开失败，拒绝以默认策略降级运行（{}）：{err:#}",
                path.display()
            )
        });

    let has_table: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'system_config'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);

    if !has_table {
        tracing::error!("[load_dimension_policies_from_db] 主库缺少 system_config 表（{}）", path.display());
        panic!(
            "[load_dimension_policies_from_db] 库 {} 缺少 system_config 表 ⇒ 不是桌面业务主库，拒绝以降级默认策略继续运行",
            path.display()
        );
    }

    let val_str: Result<String, _> = conn.query_row(
        "SELECT value FROM system_config WHERE key = 'DIMENSION_POLICIES' LIMIT 1",
        [],
        |row| row.get(0),
    );

    match val_str {
        Ok(raw) => {
            let db_policies = serde_json::from_str::<HashMap<String, omni_core::DimensionExecutionPolicy>>(&raw)
                .unwrap_or_else(|err| {
                    tracing::error!("[load_dimension_policies_from_db] DIMENSION_POLICIES JSON 解析失败: {err}");
                    panic!(
                        "[load_dimension_policies_from_db] system_config.DIMENSION_POLICIES JSON 解析失败（{}），拒绝以默认策略降级运行：{err}",
                        path.display()
                    )
                });
            let count = db_policies.len();
            for (code, policy) in db_policies {
                policies.insert(code, policy);
            }
            tracing::info!(
                "[load_dimension_policies_from_db] 成功从主库 system_config 读取并覆盖 {} 条维度策略",
                count
            );
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            tracing::warn!(
                "[load_dimension_policies_from_db] 主库 system_config 尚无 DIMENSION_POLICIES 行（云端配置未落库），暂用物化默认策略"
            );
        }
        Err(err) => {
            tracing::error!("[load_dimension_policies_from_db] 读取 DIMENSION_POLICIES 失败: {err:#}");
            panic!(
                "[load_dimension_policies_from_db] 读取主库 {} 的 DIMENSION_POLICIES 失败，拒绝以降级默认策略继续运行：{err:#}",
                path.display()
            );
        }
    }

    policies
}

/// 策略重载响应体
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReloadPoliciesResponse {
    pub success: bool,
    pub count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// POST /api/taxonomy/reload-policies 与 POST /api/v1/taxonomy/reload-policies
/// 动态重载维度执行策略端点 (DEC-04)
pub async fn reload_policies_handler(
    State(state): State<AppState>,
) -> Json<ReloadPoliciesResponse> {
    let master_path = state.master_db_path.lock().ok().and_then(|p| p.clone());
    let new_policies = load_dimension_policies_from_db(master_path.as_deref());
    let count = new_policies.len();
    if let Ok(mut lock) = state.dimension_policies.write() {
        *lock = new_policies;
        tracing::info!("[reload_policies_handler] 策略重载成功，当前激活策略数: {}", count);
        Json(ReloadPoliciesResponse {
            success: true,
            count,
            error: None,
        })
    } else {
        Json(ReloadPoliciesResponse {
            success: false,
            count: 0,
            error: Some("RwLock poisoned".to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

        master_conn
            .execute_batch(
                "INSERT INTO workspace_files (file_fingerprint, path, workspace_id, status) VALUES
            ('fp_1', 'D:\\files\\img1.jpg', 1, 1),
            ('fp_2', 'D:\\files\\img2.jpg', 1, 1),
            ('fp_3', 'D:\\files\\img3.png', 1, 1),
            ('fp_4', 'D:\\files\\story.txt', 1, 1);

            INSERT INTO file_tag_relations (file_fingerprint, tag_code, code_path, name_path) VALUES
            ('fp_1', 'builtin.uncensored', '/builtin.file_type/builtin.image/builtin.censorship/builtin.uncensored', '/文件类型/图片/打码程度/无码'),
            ('fp_2', 'builtin.uncensored', '/builtin.file_type/builtin.image/builtin.censorship/builtin.uncensored', '/文件类型/图片/打码程度/无码'),
            ('fp_3', 'builtin.image', '/builtin.file_type/builtin.image', '/文件类型/图片'),
            ('fp_4', 'builtin.novel', '', '');",
            )
            .unwrap();

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
            );
            INSERT INTO workspace_files (file_fingerprint, path, workspace_id, status) VALUES
            ('fp_a', 'D:\\ws1\\sub\\a.jpg', 1, 1),
            ('fp_b', 'D:\\ws1\\other\\b.jpg', 1, 1),
            ('fp_c', 'D:\\ws2\\sub\\c.jpg', 2, 1);

            INSERT INTO file_tag_relations (file_fingerprint, tag_code, code_path, name_path) VALUES
            ('fp_a', 'builtin.image', '/builtin.file_type/builtin.image', '/文件类型/图片'),
            ('fp_b', 'builtin.image', '/builtin.file_type/builtin.image', '/文件类型/图片'),
            ('fp_c', 'builtin.image', '/builtin.file_type/builtin.image', '/文件类型/图片');",
            )
            .unwrap();

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
            );
            INSERT INTO workspace_files (file_fingerprint, path, workspace_id, status) VALUES
            ('fp_img', 'D:\\ws1\\a.jpg', 1, 1),
            ('fp_vid', 'D:\\ws1\\b.mp4', 1, 1);

            -- 同一个 tag_code 'builtin.uncensored'，分别属于图片与视频分支
            INSERT INTO file_tag_relations (file_fingerprint, tag_code, code_path, name_path) VALUES
            ('fp_img', 'builtin.uncensored', '/builtin.file_type/builtin.image/builtin.censorship/builtin.uncensored', '/文件类型/图片/打码程度/无码'),
            ('fp_vid', 'builtin.uncensored', '/builtin.file_type/builtin.video/builtin.censorship/builtin.uncensored', '/文件类型/视频/打码程度/无码');",
            )
            .unwrap();

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

        // 验证图片分支下的 uncensored
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

        // 验证视频分支下的 uncensored (跨分支独立，未受污染)
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
        ).unwrap();

        let master_conn = rusqlite::Connection::open_in_memory().unwrap();
        master_conn.execute_batch(
            r#"
            CREATE TABLE file_tag_relations (
                file_fingerprint TEXT NOT NULL,
                tag_code TEXT NOT NULL,
                via_parent_code TEXT,
                code_path TEXT NOT NULL DEFAULT '',
                name_path TEXT NOT NULL DEFAULT '',
                depth INTEGER NOT NULL DEFAULT 1
            );
            -- 模拟存量脏数据：被错误包裹了 /builtin.content_tags/ 前缀
            INSERT INTO file_tag_relations (file_fingerprint, tag_code, via_parent_code, code_path, name_path, depth) VALUES
                ('fp_1', 'omw.01704761.a', 'builtin.security_level', '/builtin.content_tags/builtin.security_level/omw.01704761.a', '/内容标签/安全等级/公开', 3),
                ('fp_1', 'builtin.chinese', 'builtin.language_segmentation', '/builtin.content_tags/builtin.language_segmentation/builtin.chinese', '/内容标签/语言细分/中文', 3),
                ('fp_1', '_ext.he.9ec86979', 'builtin.content_tags', '/builtin.content_tags/_ext.he.9ec86979', '/内容标签/合歆', 2);
            "#,
        ).unwrap();

        let resp = query_taxonomy_tree_fast(
            &conn,
            "zh-CN",
            None,
            &master_conn,
            false,
            None,
            None,
        ).unwrap();

        // 1. 验证安全等级自愈归位于「文件用途」->「工作文档」下，严禁独立建根
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
        assert_eq!(sec_node.code_path.as_deref(), Some("/builtin.file_purpose/builtin.work_documents/builtin.security_level"));
        assert_eq!(sec_node.children.len(), 1);
        assert_eq!(sec_node.children[0].code, "omw.01704761.a");
        assert_eq!(sec_node.children[0].code_path.as_deref(), Some("/builtin.file_purpose/builtin.work_documents/builtin.security_level/omw.01704761.a"));

        // 2. 验证语言细分自愈归位于「文件类型」->「文档」下，严禁独立建根
        assert!(!resp.root_nodes.iter().any(|n| n.code == "builtin.language_segmentation"), "语言细分严禁独立建根");
        let type_root = resp.root_nodes.iter().find(|n| n.code == "builtin.file_type");
        assert!(type_root.is_some(), "文件类型必须存在");
        let type_root = type_root.unwrap();
        let doc_node = type_root.children.iter().find(|c| c.code == "builtin.document");
        assert!(doc_node.is_some(), "文档必须存在于文件类型下");
        let doc_node = doc_node.unwrap();
        let lang_node = doc_node.children.iter().find(|c| c.code == "builtin.language_segmentation");
        assert!(lang_node.is_some(), "语言细分必须挂载在文档下");
        let lang_node = lang_node.unwrap();
        assert_eq!(lang_node.code_path.as_deref(), Some("/builtin.file_type/builtin.document/builtin.language_segmentation"));
        assert_eq!(lang_node.children.len(), 1);
        assert_eq!(lang_node.children[0].code, "builtin.chinese");
        assert_eq!(lang_node.children[0].code_path.as_deref(), Some("/builtin.file_type/builtin.document/builtin.language_segmentation/builtin.chinese"));

        // 3. 验证内容标签下仅有纯业务扩展标签，绝无安全等级或语言细分
        let content_root = resp.root_nodes.iter().find(|n| n.code == "builtin.content_tags");
        assert!(content_root.is_some());
        let content_root = content_root.unwrap();
        assert!(content_root.children.iter().all(|c| c.code != "builtin.security_level" && c.code != "builtin.language_segmentation"));
        assert!(content_root.children.iter().any(|c| c.code == "_ext.he.9ec86979"));
    }

    /// fast path 必须按 code 回填语义包 file_tags.sort_order：
    /// 根节点与子节点均取 pack 值，根数组按 sort_order (>0) 升序；
    /// builtin.content_tags 保留 9999 垫底特例（不被 pack 值 28 覆盖）。
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
            );
            INSERT INTO workspace_files (file_fingerprint, path, workspace_id, status) VALUES
                ('fp_1', 'D:\\\\files\\\\a.txt', 1, 1),
                ('fp_2', 'D:\\\\files\\\\b.txt', 1, 1),
                ('fp_3', 'D:\\\\files\\\\c.txt', 1, 1),
                ('fp_4', 'D:\\\\files\\\\d.txt', 1, 1),
                ('fp_5', 'D:\\\\files\\\\e.txt', 1, 1);

            INSERT INTO file_tag_relations (file_fingerprint, tag_code, code_path, name_path) VALUES
                ('fp_1', 'builtin.image', '/builtin.file_type/builtin.image', '/文件类型/图片'),
                ('fp_2', 'builtin.novel', '/builtin.file_type/builtin.novel', '/文件类型/小说'),
                ('fp_3', 'builtin.article', '/builtin.file_type/builtin.article', '/文件类型/文章'),
                ('fp_4', 'builtin.zhangsan', '/hownet.000000043396.n/builtin.zhangsan', '/作者/张三'),
                ('fp_5', 'builtin.foo', '/builtin.content_tags/builtin.foo', '/内容标签/未知');",
            )
            .unwrap();

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

        // 1. 根数组按 sort_order (>0) 升序：file_type(1) -> author(4) -> content_tags(9999)
        let root_codes: Vec<&str> = resp.root_nodes.iter().map(|n| n.code.as_str()).collect();
        assert_eq!(root_codes, vec!["builtin.file_type", "hownet.000000043396.n", "builtin.content_tags"]);

        // 2. 根节点 sort_order 取自 pack；content_tags 特例仍为 9999（pack 值 28 不参与）
        assert_eq!(resp.root_nodes[0].sort_order, 1);
        assert_eq!(resp.root_nodes[1].sort_order, 4);
        assert_eq!(resp.root_nodes[2].sort_order, 9999);

        // 3. 子节点 sort_order 取自 pack，并按 (>0 升序, 0 视为未设置垫底, code 兜底) 排序
        let file_type_root = resp
            .root_nodes
            .iter()
            .find(|n| n.code == "builtin.file_type")
            .expect("missing builtin.file_type");
        let child_codes: Vec<&str> = file_type_root
            .children
            .iter()
            .map(|n| n.code.as_str())
            .collect();
        assert_eq!(child_codes, vec!["builtin.novel", "builtin.image", "builtin.article"]);
        assert_eq!(file_type_root.children[0].sort_order, 1);
        assert_eq!(file_type_root.children[1].sort_order, 5);
        // pack 无该行 -> 保留 fast path 硬编码兜底 0（= 未设置）
        assert_eq!(file_type_root.children[2].sort_order, 0);
    }

    /// pack 缺失 file_tags 表时（本地主库/测试夹具），回填映射为空，回退硬编码兜底不 panic。
    #[test]
    fn test_fast_path_without_pack_file_tags_keeps_hardcoded_sort_order() {
        let pack_conn = Connection::open_in_memory().unwrap();
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
            );
            INSERT INTO workspace_files (file_fingerprint, path, workspace_id, status) VALUES
                ('fp_1', 'D:\\\\files\\\\a.txt', 1, 1),
                ('fp_2', 'D:\\\\files\\\\b.txt', 1, 1);
            INSERT INTO file_tag_relations (file_fingerprint, tag_code, code_path, name_path) VALUES
                ('fp_1', 'builtin.image', '/builtin.file_type/builtin.image', '/文件类型/图片'),
                ('fp_2', 'builtin.foo', '/builtin.content_tags/builtin.foo', '/内容标签/未知');",
            )
            .unwrap();

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

        // 子节点在 pack 缺失时保持硬编码 0，按 code 字典序稳定排序
        assert_eq!(file_type_root.children.len(), 1);
        assert_eq!(file_type_root.children[0].sort_order, 0);
    }

    /// 历史存量数据自愈测试：若受控子维度被错误包裹在 /builtin.content_tags 下，倒排 Trie 树必须剥除伪前缀并自愈至文件类型根
    #[test]
    fn test_inverted_trie_healing_pseudo_content_tags() {
        let pack_conn = Connection::open_in_memory().unwrap();
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
            );
            INSERT INTO workspace_files (file_fingerprint, path, workspace_id, status) VALUES
                ('fp_cam', 'D:\\\\files\\\\cam.jpg', 1, 1),
                ('fp_txt', 'D:\\\\files\\\\txt.png', 1, 1);
            INSERT INTO file_tag_relations (file_fingerprint, tag_code, code_path, name_path) VALUES
                ('fp_cam', '_ext.camera_canon', '/builtin.content_tags/builtin.photography_categories/_ext.camera_canon', '/内容标签/摄影照片细分/Canon EOS 5D Mark II'),
                ('fp_txt', 'builtin.tiny_text', '/builtin.content_tags/builtin.text_density/builtin.tiny_text', '/内容标签/文字密度/微量文本');",
            )
            .unwrap();

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

        // 1. 内容标签根节点下绝不能包含摄影照片细分或文字密度
        if let Some(content_root) = resp.root_nodes.iter().find(|n| n.code == "builtin.content_tags") {
            assert!(!content_root.children.iter().any(|c| c.code == "builtin.photo_categories" || c.code == "builtin.photography_categories"));
            assert!(!content_root.children.iter().any(|c| c.code == "builtin.text_density"));
        }

        // 2. 文件类型根节点下必须存在自愈后的摄影照片细分或文字密度
        let file_type_root = resp
            .root_nodes
            .iter()
            .find(|n| n.code == "builtin.file_type")
            .expect("missing builtin.file_type");

        let has_healed_subdim = file_type_root.children.iter().any(|c| {
            c.code == "builtin.photo_categories" || c.code == "builtin.photography_categories" || c.code == "builtin.text_density"
        });
        assert!(has_healed_subdim, "摄影照片细分或文字密度必须成功自愈至文件类型根节点下");
    }

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
        ).unwrap();

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
        let res_td = inspect_single_tag(&conn, "塔防", "builtin.author", "zh-CN");
        assert!(res_td.is_controlled);
        assert!(!res_td.matched_parent);
        assert_eq!(res_td.code.as_deref(), Some("builtin.tower_defense"));
        assert!(res_td.ancestry_matched_code.is_none());

        // Case 4: 完全未收录的自定义标签 (张三)
        let res_custom = inspect_single_tag(&conn, "张三", "builtin.author", "zh-CN");
        assert!(!res_custom.is_controlled);
        assert!(!res_custom.matched_parent);
        assert!(res_custom.code.is_none());
        assert!(res_custom.ancestry_matched_code.is_none());

        // Case 5: 批量批处理检验
        let batch_items = vec![
            InspectTagItem { tag_name: "公开".to_string(), target_parent_code: "builtin.security_level".to_string() },
            InspectTagItem { tag_name: "发票".to_string(), target_parent_code: "builtin.file_purpose".to_string() },
            InspectTagItem { tag_name: "塔防".to_string(), target_parent_code: "builtin.author".to_string() },
            InspectTagItem { tag_name: "张三".to_string(), target_parent_code: "builtin.author".to_string() },
        ];
        let batch_res = inspect_tags_batch(&conn, &batch_items, "zh-CN").unwrap();
        assert_eq!(batch_res.len(), 4);
        assert!(batch_res[0].matched_parent);
        assert!(batch_res[1].matched_parent);
        assert!(!batch_res[2].matched_parent);
        assert!(!batch_res[3].is_controlled);
    }

    // =========================================================================
    // 八大核销断言 (Spec §4.2 / AC-1 ~ AC-8)
    // =========================================================================

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
        assert_eq!(evaluate_node_policy(&ctx_root_action), TaxonomyFoldPolicy::FlattenToActionRoot);

        // P1: 一票否决
        let ctx_p1_function_word = NodeEvaluationContext {
            code: "builtin.category.function_word",
            pos: None,
            lexfile: None,
            sememe_category: Some(7),
            is_root: true,
            in_action_branch: false,
        };
        assert_eq!(evaluate_node_policy(&ctx_p1_function_word), TaxonomyFoldPolicy::RejectSubtree);

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
            pos: Some("n"), // HowNet 义原骨架的 'n' 占位符
            lexfile: None,
            sememe_category: None,
            is_root: false,
            in_action_branch: true,
        };
        assert_eq!(evaluate_node_policy(&ctx_p2_in_action), TaxonomyFoldPolicy::FlattenToActionRoot);

        let ctx_p2_verb = NodeEvaluationContext {
            code: "omw.00109535.v",
            pos: Some("v"),
            lexfile: Some("verb.change"),
            sememe_category: None,
            is_root: false,
            in_action_branch: false,
        };
        assert_eq!(evaluate_node_policy(&ctx_p2_verb), TaxonomyFoldPolicy::FlattenToActionRoot);

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

    /// 辅助构建模拟主库与语义库连接
    fn setup_test_master_and_pack_conns() -> (Connection, Connection) {
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

    /// 断言 1【图 1 动作链扁平化断言 (AC-1)】
    #[test]
    fn test_taxonomy_tree_action_flattening_ac1() {
        let (pack_conn, master_conn) = setup_test_master_and_pack_conns();

        pack_conn.execute_batch(
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
                ('action.search', '搜索');"
        ).unwrap();

        master_conn.execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path) VALUES
                ('fp_1', 'D:\\files\\f1.txt'),
                ('fp_2', 'D:\\files\\f2.txt');
            INSERT INTO file_tag_relations (file_fingerprint, tag_code, code_path, name_path) VALUES
                ('fp_1', 'action.delete', '/builtin.category.action/action.replace/action.change/action.modify/action.delete', '/动作生产/更换/改变/修改/删除'),
                ('fp_2', 'action.search', '/builtin.category.action/action.research/action.deep_research/action.discover/action.search', '/动作生产/研究/深入研究/发掘/搜索');"
        ).unwrap();

        let resp = query_taxonomy_tree_fast(&pack_conn, "zh-CN", None, &master_conn, true, None, None).unwrap();
        assert_eq!(resp.root_nodes.len(), 1);
        assert_eq!(resp.root_nodes[0].code, "builtin.content_tags");
        let action_root = &resp.root_nodes[0].children[0];
        assert_eq!(action_root.code, "builtin.category.action");
        assert_eq!(action_root.name, "动作生产");
        assert_eq!(action_root.file_count, 2);

        // 断言：直接子节点严格只有“删除”与“搜索”，中间层被彻底抹平（深度为 2）
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

        pack_conn.execute_batch(
            "INSERT INTO file_tags (code, pos) VALUES
                ('omw.00109535.v', 'v'),
                ('hownet.skeleton.anti', 'n'),
                ('action.debug_test', 'v');
            INSERT INTO tag_aliases_zh_CN (tag_code, lemma) VALUES
                ('omw.00109535.v', '改变'),
                ('hownet.skeleton.anti', '反'),
                ('action.debug_test', '调测');"
        ).unwrap();

        master_conn.execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path) VALUES
                ('fp_1', 'D:\\files\\code.rs');
            INSERT INTO file_tag_relations (file_fingerprint, tag_code, code_path, name_path) VALUES
                ('fp_1', 'action.debug_test', '/omw.00109535.v/hownet.skeleton.anti/action.debug_test', '/改变/反/调测');"
        ).unwrap();

        let resp = query_taxonomy_tree_fast(&pack_conn, "zh-CN", None, &master_conn, true, None, None).unwrap();
        assert_eq!(resp.root_nodes.len(), 1);
        assert_eq!(resp.root_nodes[0].code, "builtin.content_tags");
        let root = &resp.root_nodes[0].children[0];
        assert_eq!(root.code, "omw.00109535.v");
        // 断言：“调测”直接平铺为顶级动词根的直接子节点，中间“反”被彻底抹除
        assert_eq!(root.children.len(), 1);
        assert_eq!(root.children[0].code, "action.debug_test");
        assert_eq!(root.children[0].name, "调测");
        assert_eq!(root.children[0].parent_code.as_deref(), Some("omw.00109535.v"));
    }

    /// 断言 3【图 3 名词实体树形深度保全断言 (AC-3)】
    #[test]
    fn test_taxonomy_tree_noun_hierarchy_preservation_ac3() {
        let (pack_conn, master_conn) = setup_test_master_and_pack_conns();

        pack_conn.execute_batch(
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
                ('domain.villain', '坏人');"
        ).unwrap();

        master_conn.execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path) VALUES
                ('fp_1', 'D:\\files\\bio.pdf');
            INSERT INTO file_tag_relations (file_fingerprint, tag_code, code_path, name_path) VALUES
                ('fp_1', 'domain.villain', '/domain.study/domain.discipline/domain.biology/domain.organism/domain.person/domain.villain', '/学习资料/学科领域/生物/有机体/个人/坏人');"
        ).unwrap();

        let resp = query_taxonomy_tree_fast(&pack_conn, "zh-CN", None, &master_conn, true, None, None).unwrap();
        assert_eq!(resp.root_nodes.len(), 1);
        assert_eq!(resp.root_nodes[0].code, "builtin.content_tags");

        // 验证完整 6 级层级树形结构保持展开
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

        pack_conn.execute_batch(
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
                ('action.render', '渲染');"
        ).unwrap();

        master_conn.execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path) VALUES
                ('fp_1', 'D:\\files\\3d.blend');
            INSERT INTO file_tag_relations (file_fingerprint, tag_code, code_path, name_path) VALUES
                ('fp_1', 'action.render', '/builtin.category.action/hownet.act/hownet.do/action.recreate/action.render', '/动作生产/行动/作/再造/渲染');"
        ).unwrap();

        let resp = query_taxonomy_tree_fast(&pack_conn, "zh-CN", None, &master_conn, true, None, None).unwrap();
        assert_eq!(resp.root_nodes[0].code, "builtin.content_tags");
        let root = &resp.root_nodes[0].children[0];
        assert_eq!(root.children.len(), 1);
        assert_eq!(root.children[0].code, "action.render");
        assert_eq!(root.children[0].name, "渲染");
        // 绝不因行动、作、再造标注为 pos='n' 而逃逸为名词实体多级树展开
        assert!(root.children[0].children.is_empty());
    }

    /// 断言 5【动词自身直接命中文件保全断言 (AC-5, ISS-03)】
    #[test]
    fn test_taxonomy_tree_verb_direct_file_preservation_ac5() {
        let (pack_conn, master_conn) = setup_test_master_and_pack_conns();

        pack_conn.execute_batch(
            "INSERT INTO file_tags (code, pos, sememe_category) VALUES
                ('builtin.category.action', 'n', 1),
                ('action.modify', 'v', 1),
                ('action.delete', 'v', 1);
            INSERT INTO tag_aliases_zh_CN (tag_code, lemma) VALUES
                ('builtin.category.action', '动作生产'),
                ('action.modify', '修改'),
                ('action.delete', '删除');"
        ).unwrap();

        master_conn.execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path) VALUES
                ('fp_1', 'D:\\files\\f1.txt'),
                ('fp_2', 'D:\\files\\f2.txt');
            INSERT INTO file_tag_relations (file_fingerprint, tag_code, code_path, name_path) VALUES
                ('fp_1', 'action.delete', '/builtin.category.action/action.modify/action.delete', '/动作生产/修改/删除'),
                ('fp_2', 'action.modify', '/builtin.category.action/action.modify', '/动作生产/修改');"
        ).unwrap();

        let resp = query_taxonomy_tree_fast(&pack_conn, "zh-CN", None, &master_conn, true, None, None).unwrap();
        assert_eq!(resp.root_nodes[0].code, "builtin.content_tags");
        let root = &resp.root_nodes[0].children[0];
        // 断言：“动作生产”下平级并列存在“修改 (1)”与“删除 (1)”，F2 独立入口完好无损，且总深度依然为 2
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

        pack_conn.execute_batch(
            "INSERT INTO file_tags (code, pos, sememe_category) VALUES
                ('builtin.category.action', 'n', 1),
                ('action.branch_a', 'n', 1),
                ('action.branch_b', 'n', 1),
                ('action.sync', 'v', 1);
            INSERT INTO tag_aliases_zh_CN (tag_code, lemma) VALUES
                ('builtin.category.action', '动作生产'),
                ('action.sync', '同步');"
        ).unwrap();

        master_conn.execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path) VALUES
                ('fp_1', 'D:\\files\\f1.txt'),
                ('fp_2', 'D:\\files\\f2.txt');
            INSERT INTO file_tag_relations (file_fingerprint, tag_code, code_path, name_path) VALUES
                ('fp_1', 'action.sync', '/builtin.category.action/action.branch_a/action.sync', '/动作生产/分支A/同步'),
                ('fp_2', 'action.sync', '/builtin.category.action/action.branch_b/action.sync', '/动作生产/分支B/同步');"
        ).unwrap();

        let resp = query_taxonomy_tree_fast(&pack_conn, "zh-CN", None, &master_conn, true, None, None).unwrap();
        assert_eq!(resp.root_nodes[0].code, "builtin.content_tags");
        let root = &resp.root_nodes[0].children[0];
        // 断言：提升至动作根下合并为一个“同步”节点，其 file_count 严格为 2，files 并集为 {fp_1, fp_2}
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

        pack_conn.execute_batch(
            "INSERT INTO file_tags (code, pos) VALUES
                ('doc.work', 'n'),
                ('omw.00024720.n', 'n'), -- 状态 (命中标量白名单 SHELL_NOUN_CODES)
                ('status.done', 'n');
            INSERT INTO tag_aliases_zh_CN (tag_code, lemma) VALUES
                ('doc.work', '工作文档'),
                ('omw.00024720.n', '状态'),
                ('status.done', '处理完毕');"
        ).unwrap();

        master_conn.execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path) VALUES
                ('fp_1', 'D:\\files\\doc.docx');
            INSERT INTO file_tag_relations (file_fingerprint, tag_code, code_path, name_path) VALUES
                ('fp_1', 'status.done', '/doc.work/omw.00024720.n/status.done', '/工作文档/状态/处理完毕');"
        ).unwrap();

        let resp = query_taxonomy_tree_fast(&pack_conn, "zh-CN", None, &master_conn, true, None, None).unwrap();
        assert_eq!(resp.root_nodes.len(), 1);
        assert_eq!(resp.root_nodes[0].code, "builtin.content_tags");
        let work_doc = &resp.root_nodes[0].children[0];
        assert_eq!(work_doc.name, "工作文档");
        // 断言：“状态”抽屉被抹除，“处理完毕”直接上提至“工作文档”下一级
        assert_eq!(work_doc.children.len(), 1);
        assert_eq!(work_doc.children[0].code, "status.done");
        assert_eq!(work_doc.children[0].name, "处理完毕");
        assert_eq!(work_doc.children[0].parent_code.as_deref(), Some("doc.work"));
    }

    /// 断言 8【虚词与时序大类一票否决断言 (AC-8, ISS-11)】
    #[test]
    fn test_taxonomy_tree_function_word_rejection_ac8() {
        let (pack_conn, master_conn) = setup_test_master_and_pack_conns();

        pack_conn.execute_batch(
            "INSERT INTO file_tags (code, pos) VALUES
                ('builtin.category.function_word', 'u'),
                ('func.some_particle', 'u');
            INSERT INTO tag_aliases_zh_CN (tag_code, lemma) VALUES
                ('builtin.category.function_word', '虚词'),
                ('func.some_particle', '的');"
        ).unwrap();

        master_conn.execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path) VALUES
                ('fp_1', 'D:\\files\\f1.txt');
            INSERT INTO file_tag_relations (file_fingerprint, tag_code, code_path, name_path) VALUES
                ('fp_1', 'func.some_particle', '/builtin.category.function_word/func.some_particle', '/虚词/的');"
        ).unwrap();

        let resp = query_taxonomy_tree_fast(&pack_conn, "zh-CN", None, &master_conn, true, None, None).unwrap();
        // 断言：返回的根节点列表中不含虚词分类，整枝被过滤
        assert_eq!(resp.root_nodes.len(), 0);
    }
    /// 断言 9【WordNet 顶层哲学壳 noun.Tops 批量穿透断言 (AC-9)】
    #[test]
    fn test_taxonomy_tree_noun_tops_bypass_ac9() {
        let (pack_conn, master_conn) = setup_test_master_and_pack_conns();

        pack_conn.execute_batch(
            "INSERT INTO file_tags (code, pos, lexfile) VALUES
                ('omw.00032613.n', 'n', 'noun.Tops'), -- 所有 (possession, 哲学顶层壳)
                ('omw.13329641.n', 'n', 'noun.possession'), -- 财产 (具象分类抽屉)
                ('tag.contract', 'n', 'noun.communication');
            INSERT INTO tag_aliases_zh_CN (tag_code, lemma) VALUES
                ('omw.00032613.n', '所有'),
                ('omw.13329641.n', '财产'),
                ('tag.contract', '转让合同');"
        ).unwrap();

        master_conn.execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path) VALUES
                ('fp_1', 'D:\\files\\contract.pdf');
            INSERT INTO file_tag_relations (file_fingerprint, tag_code, code_path, name_path) VALUES
                ('fp_1', 'tag.contract', '/omw.00032613.n/omw.13329641.n/tag.contract', '/所有/财产/转让合同');"
        ).unwrap();

        let resp = query_taxonomy_tree_fast(&pack_conn, "zh-CN", None, &master_conn, true, None, None).unwrap();
        assert_eq!(resp.root_nodes.len(), 1);
        assert_eq!(resp.root_nodes[0].code, "builtin.content_tags");
        let content_tags = &resp.root_nodes[0];
        
        // 断言：顶层哲学壳“所有 (omw.00032613.n)”被 BypassAndHoist 穿透抹除，
        // “财产 (omw.13329641.n)”直接提升为内容标签的第一级子节点！
        assert!(!content_tags.children.iter().any(|c| c.code == "omw.00032613.n" || c.name == "所有"));
        let property_node = content_tags.children.iter().find(|c| c.code == "omw.13329641.n");
        assert!(property_node.is_some(), "财产必须作为合法分类抽屉直接挂在内容标签下");
        let property_node = property_node.unwrap();
        assert_eq!(property_node.name, "财产");
        assert_eq!(property_node.children.len(), 1);
        assert_eq!(property_node.children[0].code, "tag.contract");
        assert_eq!(property_node.children[0].name, "转让合同");
        assert_eq!(property_node.children[0].parent_code.as_deref(), Some("omw.13329641.n"));
    }

    /// 断言 10【父子同名自愈折叠与孙辈提升断言 (AC-10)】
    #[test]
    fn test_taxonomy_tree_parent_child_duplicate_name_folding_ac10() {
        let (pack_conn, master_conn) = setup_test_master_and_pack_conns();

        // 模拟跨语种撞译场景：omw.13353607.n 与 omw.13353858.n 在中文库中均被映射为“资本”
        pack_conn.execute_batch(
            "INSERT INTO file_tags (code, pos, lexfile) VALUES
                ('omw.13353607.n', 'n', 'noun.possession'),
                ('omw.13353858.n', 'n', 'noun.possession'),
                ('omw.13354154.n', 'n', 'noun.possession');
            INSERT INTO tag_aliases_zh_CN (tag_code, lemma) VALUES
                ('omw.13353607.n', '资本'),
                ('omw.13353858.n', '资本'),
                ('omw.13354154.n', '钱财');"
        ).unwrap();

        master_conn.execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path) VALUES
                ('fp_cap_direct', 'D:\\files\\capital.txt'),
                ('fp_money', 'D:\\files\\money.txt');
            -- 路径：/资本(A)/资本(B)/钱财(C)，其中资本(A)有直接文件，钱财(C)有直接文件
            INSERT INTO file_tag_relations (file_fingerprint, tag_code, code_path, name_path) VALUES
                ('fp_cap_direct', 'omw.13353858.n', '/omw.13353607.n/omw.13353858.n', '/资本/资本'),
                ('fp_money', 'omw.13354154.n', '/omw.13353607.n/omw.13353858.n/omw.13354154.n', '/资本/资本/钱财');"
        ).unwrap();

        let resp = query_taxonomy_tree_fast(&pack_conn, "zh-CN", None, &master_conn, true, None, None).unwrap();
        assert_eq!(resp.root_nodes.len(), 1);
        let content_tags = &resp.root_nodes[0];
        
        // 断言 1：内容标签下只有 1 个“资本”节点（绝不能出现 2 个）
        let capital_nodes: Vec<_> = content_tags.children.iter().filter(|c| c.name == "资本").collect();
        assert_eq!(capital_nodes.len(), 1);
        let capital = capital_nodes[0];
        
        // 断言 2：顶层“资本”节点聚合了直接文件与子树文件，file_count 严格为 2
        assert_eq!(capital.file_count, 2);
        assert_eq!(capital.files, vec!["fp_cap_direct", "fp_money"]);
        
        // 断言 3：同名子节点被抹平，其下的孙节点“钱财”被直接提升为“资本”的直接子节点！
        assert_eq!(capital.children.len(), 1);
        let money = &capital.children[0];
        assert_eq!(money.code, "omw.13354154.n");
        assert_eq!(money.name, "钱财");
        assert_eq!(money.file_count, 1);
        assert_eq!(money.files, vec!["fp_money"]);
        assert_eq!(money.parent_code.as_deref(), Some("omw.13353607.n"), "孙节点的父级指针必须对齐提升后的父节点");
    }

    /// 断言 11【小说多码兼并自愈与防环回归测试 (Task 4 验收标准)】
    #[test]
    fn test_taxonomy_tree_single_novel_deduplication_and_loop_prevention() {
        let (pack_conn, master_conn) = setup_test_master_and_pack_conns();

        // 构造语义包词表与标签
        pack_conn.execute_batch(
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
                ('omw.06367879.n', '小说', 0, 0);"
        ).unwrap();

        // 模拟三个小说历史数据场景：
        // 1. fp_novel_legacy: 挂在历史副码 omw.06367879.n
        // 2. fp_novel_canonical: 挂在正统码 omw.03833065.n
        // 3. fp_novel_loop: 挂在回环路径 /电子书/电子书细分/小说/题材/小说
        master_conn.execute_batch(
            "INSERT INTO workspace_files (file_fingerprint, path) VALUES
                ('fp_novel_legacy', 'D:\\files\\novel1.txt'),
                ('fp_novel_canonical', 'D:\\files\\novel2.txt'),
                ('fp_novel_loop', 'D:\\files\\novel3.txt');
            INSERT INTO file_tag_relations (file_fingerprint, tag_code, code_path, name_path) VALUES
                ('fp_novel_legacy', 'omw.06367879.n', '/builtin.e_book/builtin.e_book_segmentation/omw.06367879.n', '/电子书/电子书细分/小说'),
                ('fp_novel_canonical', 'omw.03833065.n', '/builtin.e_book/builtin.e_book_segmentation/omw.03833065.n', '/电子书/电子书细分/小说'),
                ('fp_novel_loop', 'omw.03833065.n', '/builtin.e_book/builtin.e_book_segmentation/omw.03833065.n/builtin.theme/omw.03833065.n', '/电子书/电子书细分/小说/题材/小说');"
        ).unwrap();

        let resp = query_taxonomy_tree_fast(&pack_conn, "zh-CN", None, &master_conn, true, None, None).unwrap();
        
        // 辅助递归函数：收集整棵树中所有名为“小说”的节点
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

        // 核心验收断言：整棵分类树上全局【仅有 1 个】“小说”节点！
        assert_eq!(all_novel_nodes.len(), 1, "整棵分类树上必须仅存在 1 个小说节点，实际找到了 {} 个", all_novel_nodes.len());
        
        let novel_node = all_novel_nodes[0];
        // 断言：该节点为权威受控码 omw.03833065.n
        assert_eq!(novel_node.code, "omw.03833065.n");
        // 断言：聚合了全部 3 份文件（fp_novel_legacy, fp_novel_canonical, fp_novel_loop）
        assert_eq!(novel_node.file_count, 3);
        assert_eq!(novel_node.files, vec!["fp_novel_canonical", "fp_novel_legacy", "fp_novel_loop"]);
        
        // 断言：小说节点自身不应包含自身（无回环冗余子节点）
        assert!(!novel_node.children.iter().any(|c| c.name == "小说"));
    }

    /// 断言 12【AC-1: RoaringBitmap 10万文件子树去重计数耗时 < 5ms】
    #[test]
    fn test_roaring_bitmap_massive_files_dedup_perf() {
        use roaring::RoaringBitmap;
        use std::time::Instant;

        let mut child1 = RoaringBitmap::new();
        let mut child2 = RoaringBitmap::new();
        // 构造 100,000 个文件，其中 40,000 个文件同时命中 child1 与 child2
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
        // SIMD 位图聚合耗时必须 < 5ms (通常在 100微秒 以内)
        assert!(elapsed.as_millis() < 5, "RoaringBitmap 10万去重耗时超标: {:?}", elapsed);
    }

    /// 断言 13【只读连接安全配置验证 (Spec §3.1.1)】
    #[test]
    fn test_open_omni_readonly_connection_pragmas() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test_readonly.db");
        {
            let init_conn = Connection::open(&db_path).unwrap();
            init_conn.execute("CREATE TABLE test (id INTEGER PRIMARY KEY)", []).unwrap();
        }

        let ro_conn = open_omni_readonly_connection(&db_path).expect("open_omni_readonly_connection failed");
        let query_only: i64 = ro_conn.query_row("PRAGMA query_only", [], |r| r.get(0)).unwrap();
        assert_eq!(query_only, 1, "query_only 必须开启");

        let temp_store: i64 = ro_conn.query_row("PRAGMA temp_store", [], |r| r.get(0)).unwrap();
        assert_eq!(temp_store, 2, "temp_store 必须为 MEMORY (2)");

        // 尝试写入必须被拦截阻断
        let write_res = ro_conn.execute("INSERT INTO test (id) VALUES (1)", []);
        assert!(write_res.is_err(), "只读连接严禁写入");
    }
}
