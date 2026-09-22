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
    pub children: Vec<TaxonomyNode>,
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

    let res = state.omw.with_conn(|conn| {
        query_taxonomy_tree(conn, &locale, root_filter.as_deref())
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

/// 内部逻辑：从 SQLite 连接中提取并组装分类树
pub fn query_taxonomy_tree(
    conn: &Connection,
    locale: &str,
    root_filter: Option<&str>,
) -> anyhow::Result<TaxonomyTreeResponse> {
    // 1. 检查 file_tags 表是否存在
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

    // 3. 查询 file_tags：读取 pack source/sort_order（创世字段治理，不兼容 category）
    // TreeNode.source 仍为 code 前缀分区（builtin/omw/_ext/user），供 UI 分区消费
    let mut stmt = conn.prepare(
        "SELECT code, name, parent_codes, source, sort_order FROM file_tags ORDER BY code"
    )?;

    struct RawTag {
        code: String,
        default_name: String,
        parent_codes: Vec<String>,
        source: String,
        sort_order: i64,
    }

    let mut raw_tags: Vec<RawTag> = Vec::new();
    let rows = stmt.query_map([], |row| {
        let code: String = row.get(0)?;
        let name: String = row.get(1)?;
        let parent_codes_raw: String = row.get(2).unwrap_or_else(|_| "[]".to_string());
        let source: Option<String> = row.get(3)?;
        let sort_order: Option<i64> = row.get(4)?;
        Ok((code, name, parent_codes_raw, source, sort_order))
    })?;

    for row in rows {
        let (code, default_name, parent_codes_raw, pack_source, sort_order) = row?;
        let parent_codes: Vec<String> = serde_json::from_str(&parent_codes_raw).unwrap_or_default();
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
        let _ = pack_source; // pack source 已在 unmapped_stats / 义原查询面消费；树分区不混用
        raw_tags.push(RawTag {
            code,
            default_name,
            parent_codes,
            source: source.to_string(),
            sort_order,
        });
    }

    let total_nodes = raw_tags.len();

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

        let primary_parent = raw.parent_codes.first().cloned();
        node_map.insert(
            raw.code.clone(),
            TaxonomyNode {
                code: raw.code,
                name: display_name,
                parent_code: primary_parent,
                parent_codes: raw.parent_codes,
                source: raw.source,
                sort_order: raw.sort_order,
                children: Vec::new(),
            },
        );
    }

    // 5. 组装多叉树森林
    // 找出所有节点及其父子关系
    let mut parent_to_children: HashMap<String, Vec<String>> = HashMap::new();
    let mut root_candidates: Vec<String> = Vec::new();

    for (code, node) in &node_map {
        if let Some(ref p) = node.parent_code {
            if node_map.contains_key(p) && p != code {
                parent_to_children.entry(p.clone()).or_default().push(code.clone());
            } else {
                root_candidates.push(code.clone());
            }
        } else {
            root_candidates.push(code.clone());
        }
    }

    // 递归组装树节点
    fn build_subtree(
        code: &str,
        node_map: &BTreeMap<String, TaxonomyNode>,
        parent_to_children: &HashMap<String, Vec<String>>,
        visited: &mut HashSet<String>,
    ) -> Option<TaxonomyNode> {
        if visited.contains(code) {
            return None; // 防环保护
        }
        visited.insert(code.to_string());

        let base_node = node_map.get(code)?;
        let mut children = Vec::new();
        if let Some(child_codes) = parent_to_children.get(code) {
            for child_code in child_codes {
                if let Some(child_node) = build_subtree(child_code, node_map, parent_to_children, visited) {
                    children.push(child_node);
                }
            }
        }
        // 子节点按 code 稳定排序
        children.sort_by(|a, b| a.code.cmp(&b.code));

        visited.remove(code);

        Some(TaxonomyNode {
            code: base_node.code.clone(),
            name: base_node.name.clone(),
            parent_code: base_node.parent_code.clone(),
            parent_codes: base_node.parent_codes.clone(),
            source: base_node.source.clone(),
            sort_order: base_node.sort_order,
            children,
        })
    }

    let mut roots: Vec<TaxonomyNode> = Vec::new();
    let mut visited = HashSet::new();

    if let Some(target_root) = root_filter {
        if let Some(root_node) = build_subtree(target_root, &node_map, &parent_to_children, &mut visited) {
            roots.push(root_node);
        }
    } else {
        root_candidates.sort();
        for root_code in root_candidates {
            if let Some(root_node) = build_subtree(&root_code, &node_map, &parent_to_children, &mut visited) {
                roots.push(root_node);
            }
        }
    }

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

