use rusqlite::Connection;
use std::collections::HashMap;
use super::types::*;
use super::utils::*;

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
pub fn query_canonical_and_aliases(
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

/// 别名对照端点核心查询：按 locale 语言分表拉取受控标签别名行（可按 source/codes/prefix 过滤）。
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
