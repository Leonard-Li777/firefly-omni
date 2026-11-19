use rusqlite::Connection;
use std::collections::HashMap;
use super::types::*;
use super::utils::*;

pub fn query_taxonomy_aliases(
    conn: &Connection,
    locale: &str,
    prefix_filter: Option<&str>,
) -> anyhow::Result<TaxonomyAliasesResponse> {
    let target_table = omni_pro::resolve_tag_aliases_table(locale);
    if !table_exists(conn, target_table) {
        return Ok(TaxonomyAliasesResponse {
            locale: locale.to_string(),
            aliases: HashMap::new(),
            canonical_names: HashMap::new(),
            total_tags: 0,
        });
    }

    let sql = format!(
        "SELECT tag_code, lemma, is_canonical FROM {} ORDER BY tag_code, is_canonical DESC",
        target_table
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([])?;

    let mut aliases: HashMap<String, Vec<String>> = HashMap::new();
    let mut canonical_names: HashMap<String, String> = HashMap::new();
    
    while let Some(row) = rows.next()? {
        let code: String = row.get(0)?;
        let lemma: String = row.get(1)?;
        let is_canonical: i64 = row.get(2)?;

        if let Some(prefix) = prefix_filter {
            if !code.starts_with(prefix) {
                continue;
            }
        }

        aliases.entry(code.clone()).or_default().push(lemma.clone());
        if is_canonical == 1 {
            canonical_names.insert(code, lemma);
        }
    }

    let total_tags = aliases.len();
    Ok(TaxonomyAliasesResponse {
        locale: locale.to_string(),
        aliases,
        canonical_names,
        total_tags,
    })
}

pub fn query_canonical_and_aliases(
    conn: &Connection,
    locale: &str,
) -> anyhow::Result<HashMap<String, (String, Vec<String>)>> {
    let aliases_resp = super::alias_query::query_taxonomy_aliases(conn, locale, None)?;
    let mut map = HashMap::new();
    for (code, list) in aliases_resp.aliases {
        let canonical = aliases_resp.canonical_names.get(&code).cloned().unwrap_or_else(|| {
            list.first().cloned().unwrap_or_default()
        });
        map.insert(code, (canonical, list));
    }
    Ok(map)
}

/// 别名对照端点核心查询：按 locale 语言分表拉取受控标签别名行（可按 source/codes/prefix 过滤）。
///
/// 工单08 拆分补全：原单文件 taxonomy.rs 的该函数在迁移时遗漏，导致 handlers.rs 的
/// taxonomy_aliases_handler 调用断链，此处补回真实实现。
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
        let tag_code: String = row.get(0)?;
        rows.push(TagAliasRow {
            tag_code,
            lemma: row.get(1)?,
            is_canonical: row.get(2)?,
            n: row.get(3)?,
            count: row.get(4)?,
        });
    }
    Ok(rows)
}
