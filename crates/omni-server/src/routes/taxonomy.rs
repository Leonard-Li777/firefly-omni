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
            let escaped_prefix = base_with_sep
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            let prefix_pattern = format!("{}%", escaped_prefix);

            where_clauses.push("(wf.path LIKE ? ESCAPE '\\' OR wf.path = ?)".to_string());
            params.push(rusqlite::types::Value::Text(prefix_pattern));
            params.push(rusqlite::types::Value::Text(base.to_string()));
        }
    }

    let col_name_expr = if has_name_path { "ftr.name_path" } else { "''" };
    let col_code_expr = if has_code_path { "ftr.code_path" } else { "''" };

    let select_sql = if has_wf && !where_clauses.is_empty() {
        format!(
            "SELECT DISTINCT ftr.file_fingerprint, ftr.tag_code, {}, {} \
             FROM file_tag_relations ftr \
             JOIN workspace_files wf ON wf.file_fingerprint = ftr.file_fingerprint \
             WHERE {}",
            col_code_expr,
            col_name_expr,
            where_clauses.join(" AND ")
        )
    } else {
        let col_name_raw = if has_name_path { "name_path" } else { "''" };
        let col_code_raw = if has_code_path { "code_path" } else { "''" };
        format!(
            "SELECT file_fingerprint, tag_code, {}, {} FROM file_tag_relations",
            col_code_raw, col_name_raw
        )
    };

    let mut stmt = master_conn.prepare(&select_sql)?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p as &dyn rusqlite::ToSql).collect();
    let rows = stmt.query_map(param_refs.as_slice(), |row| {
        let fp: String = row.get(0)?;
        let tag_code: String = row.get(1)?;
        let code_path: String = row.get(2).unwrap_or_default();
        let name_path: String = row.get(3).unwrap_or_default();
        Ok((fp, tag_code, code_path, name_path))
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
        children_paths: Vec<String>,
    }

    let mut node_map: HashMap<String, TrieNode> = HashMap::new();

    // 2. 遍历活跃记录，自顶向下构建前缀树 Trie
    for r in rows.flatten() {
        let (fp, tag_code, raw_code_path, raw_name_path) = r;
        if fp.trim().is_empty() || tag_code.trim().is_empty() {
            continue;
        }

        // 悲观数据兜底 (M-3)：若 code_path 为空，回退虚拟挂载至 /builtin.content_tags/{tag_code}
        let (eff_code_path, eff_name_path) = if raw_code_path.trim().is_empty() {
            (
                format!("/builtin.content_tags/{}", tag_code),
                format!("/内容标签/{}", tag_code),
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
            code_segs.push("builtin.content_tags".to_string());
            code_segs.push(tag_code.clone());
            name_segs.push("内容标签".to_string());
            name_segs.push(tag_code.clone());
        }

        // 历史驼峰归一化兼容
        if code_segs[0] == "builtin.fileType" {
            code_segs[0] = "builtin.file_type".to_string();
        }

        // 历史存量数据自愈：若前缀误被包装为 /builtin.content_tags/<root_dim>/...，剥除顶层伪 content_tags 前缀
        if code_segs.len() > 1 && code_segs[0] == "builtin.content_tags" && omni_core::is_root_dimension(&code_segs[1]) {
            code_segs.remove(0);
            if !name_segs.is_empty() {
                name_segs.remove(0);
            }
        }

        // 受控根维度封闭性：若顶层非受控根维度，强制前置挂载到 builtin.content_tags 下
        if !omni_core::is_root_dimension(&code_segs[0]) && code_segs[0] != "builtin.content_tags" {
            code_segs.insert(0, "builtin.content_tags".to_string());
            name_segs.insert(0, "内容标签".to_string());
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

            let sort_order = if seg_code == "builtin.content_tags" {
                9999
            } else if seg_code == "builtin.file_type" {
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
                        children_paths: Vec::new(),
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
                    target_node.direct_files.insert(fp.clone());
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

    // 3. 按需查词 (P95 <= 5ms)：仅对 Trie 树涉及的活跃节点批量向 tag_aliases_{lang} 查词
    let mut resolved_names: HashMap<String, String> = HashMap::new();
    let target_table = omni_pro::resolve_tag_aliases_table(locale);
    let has_target_table = table_exists(conn, target_table);

    let all_codes: Vec<String> = {
        let mut s = HashSet::new();
        for n in node_map.values() {
            s.insert(n.code.clone());
        }
        s.into_iter().collect()
    };
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

    // 4. 后序后代文件汇聚 (真实父与逻辑父一视同仁)
    fn build_trie_subtree(
        path: &str,
        node_map: &HashMap<String, TrieNode>,
        resolved_names: &HashMap<String, String>,
        lang: &str,
        include_files: bool,
        visited: &mut HashSet<String>,
    ) -> Option<(TaxonomyNode, HashSet<String>)> {
        if visited.contains(path) {
            return None; // 防环保护
        }
        visited.insert(path.to_string());

        let node = node_map.get(path)?;
        let mut aggregate_files = node.direct_files.clone();
        let mut children_nodes = Vec::new();

        for child_path in &node.children_paths {
            if let Some((child_node, child_files)) = build_trie_subtree(
                child_path,
                node_map,
                resolved_names,
                lang,
                include_files,
                visited,
            ) {
                aggregate_files.extend(child_files);
                children_nodes.push(child_node);
            }
        }

        // 子节点按 sort_order 升序优先 (sort_order > 0)，相同或未标注时按 code 稳定排序
        children_nodes.sort_by(|a, b| {
            let order_a = if a.sort_order > 0 { a.sort_order } else { i64::MAX };
            let order_b = if b.sort_order > 0 { b.sort_order } else { i64::MAX };
            if order_a != order_b {
                order_a.cmp(&order_b)
            } else {
                a.code.cmp(&b.code)
            }
        });

        visited.remove(path);

        let file_count = aggregate_files.len();
        let files = if include_files {
            let mut list: Vec<String> = aggregate_files.iter().cloned().collect();
            list.sort();
            list
        } else {
            Vec::new()
        };

        let display_name = if let Some(n) = resolved_names.get(&node.code) {
            n.clone()
        } else if omni_core::is_root_dimension(&node.code) || node.code.starts_with("builtin.") {
            omni_core::tag_identity::tag_display(&node.code, lang)
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

        let tax_node = TaxonomyNode {
            code: node.code.clone(),
            name: display_name,
            parent_code,
            parent_codes,
            source: node.source.clone(),
            sort_order: node.sort_order,
            file_count,
            files,
            children: children_nodes,
            code_path: Some(node.code_path.clone()),
            name_path: Some(node.name_path.clone()),
        };

        Some((tax_node, aggregate_files))
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
                    if let Some((root_node, _)) = build_trie_subtree(
                        r_path,
                        &node_map,
                        &resolved_names,
                        lang,
                        include_files,
                        &mut visited,
                    ) {
                        if !root_node.children.is_empty() || root_node.file_count > 0 {
                            roots.push(root_node);
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
            if let Some((root_node, _)) = build_trie_subtree(
                &r_path,
                &node_map,
                &resolved_names,
                lang,
                include_files,
                &mut visited,
            ) {
                if !root_node.children.is_empty() || root_node.file_count > 0 {
                    roots.push(root_node);
                }
            }
        }

        roots.sort_by(|a, b| {
            let root_rank = |code: &str| -> usize {
                omni_core::ROOT_DIMENSION_CONCEPTS
                    .iter()
                    .position(|concept_item| concept_item.code() == code)
                    .unwrap_or(usize::MAX)
            };
            let rank_a = root_rank(&a.code);
            let rank_b = root_rank(&b.code);
            if rank_a != rank_b {
                rank_a.cmp(&rank_b)
            } else if a.sort_order != b.sort_order {
                a.sort_order.cmp(&b.sort_order)
            } else {
                a.code.cmp(&b.code)
            }
        });
    }

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
            if let Ok(master_conn) = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) {
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
        node_map.insert(
            raw.code.clone(),
            TaxonomyNode {
                code: raw.code,
                name: display_name,
                parent_code: primary_parent,
                parent_codes: raw.parent_codes,
                source: raw.source,
                sort_order: raw.sort_order,
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

    // 5.1 确保 6 大受控根维度节点完整存在于 node_map，并具备准确的当前母语规范展示名
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
                    sort_order: if code == "builtin.content_tags" { 9999 } else { 0 },
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
                                            .filter(|s| !s.is_empty() && s != "内容标签" && s != "builtin.content_tags")
                                            .collect()
                                    } else {
                                        Vec::new()
                                    };
                                    let code_segs: Vec<String> = if has_code_chain {
                                        code_chain
                                            .split('/')
                                            .map(|s| s.trim().to_string())
                                            .filter(|s| !s.is_empty() && s != "内容标签" && s != "builtin.content_tags")
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
                                            first_seg_code = "builtin.file_type".to_string();
                                        }

                                        // 原则 2：只有 6 大受控根维度允许作为独立顶层树根
                                        let is_root_dim = omni_core::is_root_dimension(&first_seg_code);

                                        let (mut prev_code, start_idx) = if is_root_dim {
                                            (first_seg_code, 1)
                                        } else {
                                            // 原则 1：非 6 大根维度链路，统一安全收敛挂入 builtin.content_tags
                                            ("builtin.content_tags".to_string(), 0)
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
                if is_root && !root_candidates.contains(code) {
                    root_candidates.push(code.clone());
                } else if !is_root && code != "builtin.content_tags" {
                    // 原则 1：未挂在已知父级且非 6 大根维度的节点，统一收拢归入 builtin.content_tags
                    let children = parent_to_children.entry("builtin.content_tags".to_string()).or_default();
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
            if is_root && !root_candidates.contains(code) {
                root_candidates.push(code.clone());
            } else if !is_root && code != "builtin.content_tags" {
                // 原则 1：未挂在已知父级且非 6 大根维度的节点，统一收拢归入 builtin.content_tags
                let children = parent_to_children.entry("builtin.content_tags".to_string()).or_default();
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
pub fn load_dimension_policies_from_db(
    master_db_path: Option<&std::path::Path>,
) -> HashMap<String, omni_core::DimensionExecutionPolicy> {
    let mut policies = omni_core::get_default_dimension_policies();
    let Some(path) = master_db_path else {
        return policies;
    };
    if !path.exists() {
        return policies;
    }

    let Ok(conn) = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return policies;
    };

    let has_table: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'system_config'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);

    if !has_table {
        return policies;
    }

    let val_str: Result<String, _> = conn.query_row(
        "SELECT value FROM system_config WHERE key = 'DIMENSION_POLICIES' LIMIT 1",
        [],
        |row| row.get(0),
    );

    if let Ok(raw) = val_str {
        if let Ok(db_policies) = serde_json::from_str::<HashMap<String, omni_core::DimensionExecutionPolicy>>(&raw) {
            let count = db_policies.len();
            for (code, policy) in db_policies {
                policies.insert(code, policy);
            }
            tracing::info!(
                "[load_dimension_policies_from_db] 成功从主库 system_config 读取并覆盖 {} 条维度策略",
                count
            );
        } else {
            tracing::warn!("[load_dimension_policies_from_db] system_config.DIMENSION_POLICIES JSON 解析失败，回退物化默认策略");
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

        // 1. 验证安全等级独立建根，自愈剥离顶层伪 content_tags
        let sec_root = resp.root_nodes.iter().find(|n| n.code == "builtin.security_level");
        assert!(sec_root.is_some(), "安全等级必须独立建根，不得丢失");
        let sec_root = sec_root.unwrap();
        assert_eq!(sec_root.code_path.as_deref(), Some("/builtin.security_level"));
        assert_eq!(sec_root.name, "安全等级");
        assert_eq!(sec_root.children.len(), 1);
        assert_eq!(sec_root.children[0].code, "omw.01704761.a");
        assert_eq!(sec_root.children[0].code_path.as_deref(), Some("/builtin.security_level/omw.01704761.a"));

        // 2. 验证语言细分独立建根
        let lang_root = resp.root_nodes.iter().find(|n| n.code == "builtin.language_segmentation");
        assert!(lang_root.is_some(), "语言细分必须独立建根，不得丢失");
        let lang_root = lang_root.unwrap();
        assert_eq!(lang_root.code_path.as_deref(), Some("/builtin.language_segmentation"));
        assert_eq!(lang_root.name, "语言细分");
        assert_eq!(lang_root.children.len(), 1);
        assert_eq!(lang_root.children[0].code, "builtin.chinese");
        assert_eq!(lang_root.children[0].code_path.as_deref(), Some("/builtin.language_segmentation/builtin.chinese"));

        // 3. 验证内容标签下仅有纯业务扩展标签，绝无安全等级或语言细分
        let content_root = resp.root_nodes.iter().find(|n| n.code == "builtin.content_tags");
        assert!(content_root.is_some());
        let content_root = content_root.unwrap();
        assert!(content_root.children.iter().all(|c| c.code != "builtin.security_level" && c.code != "builtin.language_segmentation"));
        assert!(content_root.children.iter().any(|c| c.code == "_ext.he.9ec86979"));
    }
}



