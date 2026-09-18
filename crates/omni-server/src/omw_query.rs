//! OMW 核心查询端点（只读直连共享 SQLite，ADR-0035 §8.4）
//!
//! 所有查询均以 `&Connection` 为唯一输入，便于单测直接注入临时库；
//! 除 `mapping` 外，SQL 与桌面端 `DatabaseService.omw*` 完全同源，
//! 保证 Omni 与 Desktop 两个消费者对同一份只读库得到一致结果。
//!
//! 字段命名严格对齐桌面端已发布的 TS 接口（`OmwSynsetResult` / `OmwSynsetNode`
//! / `OmwAntonymResult` / `OmwTagResult`），故存在 snake_case 与 camelCase 混用。
//!
//! `mapping` 是 #649 的语义改造点：桌面端旧实现消费已废除的 `tag_omw_mapping`
//! 桥表（实测精确命中率仅 18.6%），此处改为纯 `file_tags.parent_codes` 反查，
//! 零桥表依赖。

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// 单次查询返回的最大 synset 条数（与桌面端 LIMIT 50 对齐）
const LOOKUP_LIMIT: i64 = 50;
/// 层级链递归保护上限（防脏数据成环导致无限上溯）
const HIERARCHY_MAX_DEPTH: usize = 64;

/// `POST /api/v1/omw/lookup` 请求体
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OmwLookupRequest {
    pub word: String,
    /// OMW 语言代码（如 en / cmn），缺省 en
    #[serde(default)]
    pub language: Option<String>,
}

/// `POST /api/v1/omw/hierarchy` 请求体
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OmwHierarchyRequest {
    pub synset_id: String,
}

/// `POST /api/v1/omw/antonyms` 请求体
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OmwAntonymsRequest {
    pub word: String,
    /// OMW 语言代码，缺省 cmn（与桌面端 omwAntonyms 默认值一致）
    #[serde(default)]
    pub language: Option<String>,
}

/// `POST /api/v1/omw/describe` 请求体
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OmwDescribeRequest {
    pub word: String,
    #[serde(default)]
    pub language: Option<String>,
}

/// `POST /api/v1/omw/mapping` 请求体
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OmwMappingRequest {
    pub tag_name: String,
}

/// `GET /api/v1/omw/tree` 查询参数
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OmwTreeRequest {
    pub root: String,
    /// 仅支持 1（单层懒加载），保留参数以便将来扩展
    #[serde(default = "default_tree_depth")]
    pub depth: i64,
}

fn default_tree_depth() -> i64 {
    1
}

/// `GET /api/v1/omw/unmapped-stats` 无请求体

/// 统一标签树节点（对齐前端 `TreeNode` 期望）
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeNode {
    pub code: String,
    pub name: String,
    pub parent_codes: Vec<String>,
    /// 由 code 前缀推断：builtin / omw / _ext
    pub source: String,
    pub depth: i64,
}

/// 未映射概念统计（对齐前端 `UnmappedStats` 期望）
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnmappedStats {
    pub by_lexfile: Vec<UnmappedGroup>,
    pub by_top_ancestor: Vec<UnmappedGroup>,
}

/// 分组统计项
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnmappedGroup {
    pub key: String,
    pub count: i64,
}

/// 词汇查询结果（对齐桌面端 `OmwSynsetResult`）
/// 已裁 ili/definition/dc_identifier（wayfinder 字段治理）
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OmwSynsetResult {
    pub id: String,
    pub pos: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lexfile: Option<String>,
    pub meta: Value,
    pub lemmas: Vec<String>,
}

/// 层级链节点（对齐桌面端 `OmwSynsetNode`）
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OmwSynsetNode {
    pub synset_id: String,
    pub rel_type: String,
    pub pos: String,
    pub lemmas: Vec<String>,
}

/// 反义词结果（对齐桌面端 `OmwAntonymResult`）
/// source 为固定枚举：omw_sense_relations | antonym_pairs
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OmwAntonymResult {
    pub word: String,
    pub antonym: String,
    pub source: String,
}

/// 标签映射结果（对齐桌面端 `OmwTagResult`）
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OmwTagResult {
    pub tag_code: String,
    pub tag_name: String,
    pub match_level: i64,
    pub confidence: f64,
}

/// 解析 `meta` TEXT 列：非法/缺失 JSON 一律回退为空对象（与桌面端 try/catch 语义一致）
fn parse_meta(raw: Option<String>) -> Value {
    raw.and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| Value::Object(Map::new()))
}

/// 查询某 synset 的全部 lemma（去重，保留库内顺序）
fn lemmas_of(conn: &Connection, synset_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT DISTINCT lemma FROM omw_lexical_entries WHERE synset_id = ?1")?;
    let rows = stmt.query_map(params![synset_id], |row| row.get::<_, String>(0))?;
    let mut lemmas = Vec::new();
    for row in rows {
        let lemma = row?;
        if !lemmas.contains(&lemma) {
            lemmas.push(lemma);
        }
    }
    Ok(lemmas)
}

/// 语言代码归一化（BCP-47 / locale -> OMW 词典原生语言代码）
pub fn normalize_omw_language(lang: &str) -> &'static str {
    let clean = lang.trim().to_lowercase();
    if clean.starts_with("zh") || clean == "cmn" {
        "cmn"
    } else if clean.starts_with("en") || clean == "eng" {
        "en"
    } else if clean.starts_with("ja") || clean == "jpn" {
        "jpn"
    } else if clean.starts_with("fr") || clean == "fra" {
        "fra"
    } else if clean.starts_with("de") || clean == "deu" {
        "deu"
    } else if clean.starts_with("es") || clean == "spa" {
        "spa"
    } else if clean.starts_with("pt") || clean == "por" {
        "por"
    } else if clean.starts_with("ru") || clean == "rus" {
        "rus"
    } else if clean.starts_with("it") || clean == "ita" {
        "ita"
    } else {
        "en"
    }
}

/// 词汇查 synset：按 lemma 忽略大小写匹配，目标语言缺失时回退英文
pub fn lookup(conn: &Connection, word: &str, language: &str) -> Result<Vec<OmwSynsetResult>> {
    let lang = normalize_omw_language(language);
    let mut stmt = conn.prepare(
        "SELECT DISTINCT s.id, s.pos, s.lexfile, s.meta
         FROM omw_lexical_entries e
         JOIN omw_synsets s ON e.synset_id = s.id
         WHERE e.lemma = ?1 COLLATE NOCASE
           AND (e.language = ?2 OR e.language = 'en')
         LIMIT ?3",
    )?;

    let rows = stmt.query_map(params![word, lang, LOOKUP_LIMIT], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;

    let mut results = Vec::new();
    for row in rows {
        let (id, pos, lexfile, meta) = row?;
        let lemmas = lemmas_of(conn, &id)?;
        results.push(OmwSynsetResult {
            id,
            pos,
            lexfile,
            meta: parse_meta(meta),
            lemmas,
        });
    }
    Ok(results)
}

/// 层级链查询：沿 `omw_relations.hypernym` 向上递归至根，返回祖先链（不含自身）
///
/// 仅向上递归：下位词(subtree)展开会命中 11.7 万节点，违背 §8.4「严禁全量吐出」
/// 的硬约束，故按需单层下钻由 #650 的 `GET /api/v1/omw/tree` 承担。
pub fn hierarchy(conn: &Connection, synset_id: &str) -> Result<Vec<OmwSynsetNode>> {
    let mut stmt = conn.prepare(
        "SELECT r.rel_type, s.id, s.pos
         FROM omw_relations r
         JOIN omw_synsets s ON r.target_id = s.id
         WHERE r.source_id = ?1 AND r.rel_type = 'hypernym'
         LIMIT ?2",
    )?;

    let mut chain = Vec::new();
    let mut visited: Vec<String> = vec![synset_id.to_string()];
    let mut cursor = synset_id.to_string();

    for _ in 0..HIERARCHY_MAX_DEPTH {
        let parents = stmt.query_map(params![cursor, LOOKUP_LIMIT], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;

        let mut next: Option<String> = None;
        for parent in parents {
            let (rel_type, id, pos) = parent?;
            // 成环保护：已访问过的节点直接跳过，不重复进入链
            if visited.contains(&id) {
                continue;
            }
            let lemmas = lemmas_of(conn, &id)?;
            visited.push(id.clone());
            chain.push(OmwSynsetNode {
                synset_id: id.clone(),
                rel_type,
                pos,
                lemmas,
            });
            if next.is_none() {
                next = Some(id);
            }
        }

        match next {
            Some(id) => cursor = id,
            None => break,
        }
    }

    Ok(chain)
}

/// 反义词查询：词义层图谱优先，词面 `antonym_pairs` 兜底（无 language 列）
pub fn antonyms(conn: &Connection, word: &str, _language: &str) -> Result<Vec<OmwAntonymResult>> {
    let mut results = Vec::new();

    let mut sense_stmt = conn.prepare(
        "SELECT e2.lemma
         FROM omw_lexical_entries e1
         JOIN omw_sense_relations sr ON e1.id = sr.source_entry_id
         JOIN omw_lexical_entries e2 ON sr.target_entry_id = e2.id
         WHERE e1.lemma = ?1 COLLATE NOCASE AND sr.rel_type = 'antonym'
         LIMIT ?2",
    )?;
    let senses = sense_stmt.query_map(params![word, LOOKUP_LIMIT], |row| row.get::<_, String>(0))?;
    for sense in senses {
        results.push(OmwAntonymResult {
            word: word.to_string(),
            antonym: sense?,
            source: "omw_sense_relations".to_string(),
        });
    }

    if results.is_empty() {
        let mut pairs_stmt = conn.prepare(
            "SELECT word_a, word_b
             FROM antonym_pairs
             WHERE word_a = ?1 OR word_b = ?1",
        )?;
        let pairs = pairs_stmt.query_map(params![word], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for pair in pairs {
            let (word_a, word_b) = pair?;
            results.push(OmwAntonymResult {
                word: word.to_string(),
                antonym: if word_a == word { word_b } else { word_a },
                source: "antonym_pairs".to_string(),
            });
        }
    }

    Ok(results)
}

/// 生成概念自然语言描述句：取首个非空 OMW definition（HowNet 兜底由 handler 组合）
pub fn describe(conn: &Connection, word: &str, language: &str) -> Result<Option<String>> {
    for synset in lookup(conn, word, language)? {
        if let Some(definition) = synset.definition {
            if !definition.trim().is_empty() {
                return Ok(Some(definition));
            }
        }
    }
    Ok(None)
}

/// 标签映射反查（零桥表依赖，打通 tag_aliases 多语言别名）：
/// 按 `file_tags.parent_codes` 找到共享同一 OMW 父级概念的 `builtin.*` 标签。
///
/// 语义（#649 / Q4-A）：
/// 输入 `tagName`（可为当前语言名、任意语言别名或标准 code）
/// → 归一定位目标标签与其父级 `omw.*` 概念
/// → 反查所有以这些概念为父级的兄弟 `builtin.*` 标签。
pub fn mapping(conn: &Connection, tag_name: &str) -> Result<Vec<OmwTagResult>> {
    let clean_tag = tag_name.trim();
    let mut self_codes: Vec<String> = Vec::new();
    let mut parent_codes: Vec<String> = Vec::new();

    // 1) 优先按 name 或 code 直接查 file_tags
    let mut stmt = conn.prepare("SELECT code, parent_codes FROM file_tags WHERE name = ?1 COLLATE NOCASE OR code = ?1 COLLATE NOCASE")?;
    let rows = stmt.query_map(params![clean_tag], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    for row in rows {
        let (code, parents) = row?;
        if !self_codes.contains(&code) {
            self_codes.push(code);
        }
        if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&parents) {
            for item in items {
                if let Some(c) = item.as_str() {
                    if !parent_codes.contains(&c.to_string()) {
                        parent_codes.push(c.to_string());
                    }
                }
            }
        }
    }

    // 2) 如果在 file_tags 没有按名称命中，尝试查 tag_aliases 表（若表存在）
    if self_codes.is_empty() {
        let has_alias_table: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'tag_aliases'",
                [],
                |_| Ok(true),
            )
            .unwrap_or(false);

        if has_alias_table {
            let mut alias_stmt = conn.prepare(
                "SELECT DISTINCT tag_code FROM tag_aliases WHERE lemma = ?1 COLLATE NOCASE",
            )?;
            let alias_codes = alias_stmt
                .query_map(params![clean_tag], |row| row.get::<_, String>(0))?
                .filter_map(|r| r.ok())
                .collect::<Vec<String>>();

            for code in alias_codes {
                if !self_codes.contains(&code) {
                    self_codes.push(code.clone());
                }
                // 查出该 tag_code 在 file_tags 中的 parent_codes
                let mut p_stmt = conn.prepare("SELECT parent_codes FROM file_tags WHERE code = ?1")?;
                let p_rows = p_stmt.query_map(params![code], |row| row.get::<_, String>(0))?;
                for p in p_rows.flatten() {
                    if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&p) {
                        for item in items {
                            if let Some(c) = item.as_str() {
                                if !parent_codes.contains(&c.to_string()) {
                                    parent_codes.push(c.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // 3) 若依然未查到，尝试利用 omni_core::tag_identity 字典保底解析 code
    if self_codes.is_empty() {
        if let Some(code) = omni_core::tag_identity::builtin_tag_code(clean_tag) {
            self_codes.push(code.to_string());
            let mut p_stmt = conn.prepare("SELECT parent_codes FROM file_tags WHERE code = ?1")?;
            let p_rows = p_stmt.query_map(params![code], |row| row.get::<_, String>(0))?;
            for p in p_rows.flatten() {
                if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&p) {
                    for item in items {
                        if let Some(c) = item.as_str() {
                            if !parent_codes.contains(&c.to_string()) {
                                parent_codes.push(c.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    // 该标签没有任何 OMW 父级 → 无映射可反查
    if !parent_codes.iter().any(|c| c.starts_with("omw.")) {
        return Ok(Vec::new());
    }
    let parents_json = serde_json::to_string(&parent_codes).context("序列化 parent_codes 失败")?;

    // 4) 反向查询：parent_codes 与这些 OMW 概念相交的 builtin.* 标签
    let mut rev_stmt = conn.prepare(
        "SELECT DISTINCT t.code, t.name
         FROM file_tags t, json_each(t.parent_codes) AS j
         WHERE t.code LIKE 'builtin.%'
           AND j.value LIKE 'omw.%'
           AND j.value IN (SELECT value FROM json_each(?1))
         ORDER BY t.code",
    )?;
    let rev_rows = rev_stmt.query_map(params![parents_json], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    let mut results = Vec::new();
    for row in rev_rows {
        let (code, name) = row?;
        // 排除输入标签自身，其余为同一 OMW 概念下的兄弟标签
        if self_codes.contains(&code) {
            continue;
        }
        results.push(OmwTagResult {
            tag_code: code,
            tag_name: name,
            // 命中方式为 parent_codes 直接包含，属精确级
            match_level: 1,
            confidence: 1.0,
        });
    }
    Ok(results)
}

/// 统一标签树单层懒加载：查询 `file_tags` 中 `parent_codes` 直接包含 `root` 的子节点
///
/// 支持 `builtin.*` / `omw.*` / `_ext.*` 三分区编码混存的统一树（ADR-0035）。
/// 仅返回一层直接子节点（`depth=1` 固定），严禁全量吐出 11.7 万节点（§8.4 硬约束）。
pub fn tree(conn: &Connection, root: &str, _depth: i64) -> Result<Vec<TreeNode>> {
    let mut stmt = conn.prepare(
        "SELECT code, name, parent_codes
         FROM file_tags
         WHERE EXISTS (
             SELECT 1 FROM json_each(parent_codes) WHERE value = ?1
         )
         ORDER BY code",
    )?;

    let rows = stmt.query_map(params![root], |row| {
        let code: String = row.get(0)?;
        let name: String = row.get(1)?;
        let parent_codes_raw: String = row.get(2)?;
        let parent_codes: Vec<String> = serde_json::from_str(&parent_codes_raw).unwrap_or_default();
        let source = if code.starts_with("builtin.") {
            "builtin"
        } else if code.starts_with("omw.") {
            "omw"
        } else if code.starts_with("_ext.") {
            "_ext"
        } else {
            "unknown"
        };
        Ok(TreeNode {
            code,
            name,
            parent_codes,
            source: source.to_string(),
            depth: 0, // depth 可由前端根据 parent_codes 递归推导，此处固定 0
        })
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// 未映射概念聚合统计：统计 `omw_synsets` 中未出现在任何 `file_tags.parent_codes` 的概念
///
/// 两维度聚合：
/// 1. by_lexfile：按 `omw_synsets.lexfile` 分组（词类/领域维度）
/// 2. by_top_ancestor：按超义词链根节点的 lexfile 分组（顶层抽象分类）
///
/// 若 synset 无 lexfile 则归入 "unknown" 组。未被任何标签 parent_codes 引用的 synset 即为「未映射」。
/// 注意：file_tags 中的 OMW 概念 code 为 `omw.{synset_id}`（如 `omw.o-dog.n`），
/// 需补齐前缀进行匹配（ADR-0035 三分区编码契约）。
pub fn unmapped_stats(conn: &Connection) -> Result<UnmappedStats> {
    // 先取出所有被 parent_codes 引用的 omw.* 代码（去重）
    let mut ref_stmt = conn.prepare(
        "SELECT DISTINCT j.value
         FROM file_tags t, json_each(t.parent_codes) AS j
         WHERE j.value LIKE 'omw.%'",
    )?;
    let referenced: std::collections::HashSet<String> = ref_stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();

    // 按 lexfile 聚合
    let mut by_lexfile: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    // 按顶层祖先 lexfile 聚合：需先求每个 synset 的超义词根
    let mut by_top_ancestor: std::collections::HashMap<String, i64> = std::collections::HashMap::new();

    // 预加载所有 synset 的 lexfile（建索引，避免逐节点线性扫描导致 O(n²)）
    let mut lexfile_stmt = conn.prepare("SELECT id, lexfile FROM omw_synsets")?;
    let synset_lexfiles: std::collections::HashMap<String, Option<String>> = lexfile_stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    // 预加载 hypernym 关系用于向上追溯
    let mut rel_stmt = conn.prepare(
        "SELECT source_id, target_id FROM omw_relations WHERE rel_type = 'hypernym'",
    )?;
    let relations: Vec<(String, String)> = rel_stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();
    // 构建 source_id -> target_id 映射（hypernym 向上链）
    let mut hypernym_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (source, target) in relations {
        hypernym_map.insert(source, target);
    }

    // 定义函数：求 synset 的顶层祖先 lexfile
    // 注意：必须先沿 hypernym 链走到根，再取「根节点」的 lexfile，
    // 否则会退化成与 by_lexfile 相同的自身 lexfile（顶层抽象分类失去意义）。
    fn top_ancestor_lexfile(
        id: &str,
        hypernym_map: &std::collections::HashMap<String, String>,
        synset_lexfiles: &std::collections::HashMap<String, Option<String>>,
    ) -> String {
        let mut cursor = id.to_string();
        let mut visited = std::collections::HashSet::new();
        for _ in 0..64 {
            if !visited.insert(cursor.clone()) {
                break;
            }
            // 向上走，直到没有 hypernym 父级（或成环）
            match hypernym_map.get(&cursor) {
                Some(parent) => cursor = parent.clone(),
                None => break,
            }
        }
        // 取顶层根的 lexfile；根无 lexfile 时回退 unknown
        synset_lexfiles
            .get(&cursor)
            .and_then(|lf| lf.clone())
            .filter(|lf| !lf.is_empty())
            .unwrap_or_else(|| "unknown".to_string())
    }

    for (id, lexfile_opt) in &synset_lexfiles {
        // 检查带前缀的 omw.{id} 是否被引用
        let prefixed = format!("omw.{id}");
        if referenced.contains(&prefixed) {
            continue;
        }
        // by_lexfile
        let lf = lexfile_opt
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        *by_lexfile.entry(lf).or_insert(0) += 1;

        // by_top_ancestor
        let top_lf = top_ancestor_lexfile(id, &hypernym_map, &synset_lexfiles);
        *by_top_ancestor.entry(top_lf).or_insert(0) += 1;
    }

    let mut by_lexfile_vec: Vec<UnmappedGroup> = by_lexfile
        .into_iter()
        .map(|(key, count)| UnmappedGroup { key, count })
        .collect();
    by_lexfile_vec.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.key.cmp(&b.key)));

    let mut by_top_ancestor_vec: Vec<UnmappedGroup> = by_top_ancestor
        .into_iter()
        .map(|(key, count)| UnmappedGroup { key, count })
        .collect();
    by_top_ancestor_vec.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.key.cmp(&b.key)));

    Ok(UnmappedStats {
        by_lexfile: by_lexfile_vec,
        by_top_ancestor: by_top_ancestor_vec,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造覆盖全部 5 个查询的最小 OMW + file_tags 夹具
    fn fixture() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE omw_languages (code TEXT PRIMARY KEY, label TEXT NOT NULL, \
                has_hierarchy INTEGER NOT NULL DEFAULT 0, has_definitions INTEGER NOT NULL DEFAULT 0, \
                has_examples INTEGER NOT NULL DEFAULT 0, meta TEXT NOT NULL DEFAULT '{}');
             CREATE TABLE omw_synsets (id TEXT PRIMARY KEY, ili TEXT, pos TEXT NOT NULL, lexfile TEXT, \
                definition TEXT, dc_identifier TEXT, meta TEXT NOT NULL DEFAULT '{}');
             CREATE TABLE omw_lexical_entries (id TEXT PRIMARY KEY, synset_id TEXT NOT NULL, \
                language TEXT NOT NULL, lemma TEXT NOT NULL, pos TEXT NOT NULL, meta TEXT NOT NULL DEFAULT '{}');
             CREATE TABLE omw_relations (source_id TEXT NOT NULL, target_id TEXT NOT NULL, \
                rel_type TEXT NOT NULL, meta TEXT NOT NULL DEFAULT '{}', PRIMARY KEY (source_id, target_id, rel_type));
             CREATE TABLE omw_sense_relations (source_entry_id TEXT NOT NULL, target_entry_id TEXT NOT NULL, \
                rel_type TEXT NOT NULL, meta TEXT NOT NULL DEFAULT '{}', PRIMARY KEY (source_entry_id, target_entry_id, rel_type));
             CREATE TABLE antonym_pairs (id INTEGER PRIMARY KEY AUTOINCREMENT, word_a TEXT NOT NULL, \
                word_b TEXT NOT NULL, source TEXT NOT NULL, language TEXT NOT NULL DEFAULT 'cmn', \
                status TEXT NOT NULL DEFAULT 'auto', meta TEXT NOT NULL DEFAULT '{}', UNIQUE (word_a, word_b, language));
             CREATE TABLE file_tags (code TEXT PRIMARY KEY, name TEXT NOT NULL, parent_codes TEXT NOT NULL DEFAULT '[]');",
        )
        .unwrap();

        conn.execute_batch(
            "INSERT INTO omw_languages (code, label) VALUES ('en', 'English'), ('cmn', '中文');
             INSERT INTO omw_synsets (id, ili, pos, lexfile, definition, dc_identifier, meta) VALUES
                ('o-dog.n', 'i1', 'n', 'noun.animal', 'a domesticated canine', 'dc1', '{\"k\":1}'),
                ('o-animal.n', 'i2', 'n', 'noun.animal', 'a living organism', NULL, '{}'),
                ('o-organism.n', 'i3', 'n', 'noun.entity', NULL, NULL, '{}'),
                ('o-plant.n', 'i4', 'n', 'noun.plant', 'a photosynthetic organism', NULL, '{}');
             INSERT INTO omw_lexical_entries (id, synset_id, language, lemma, pos) VALUES
                ('e1', 'o-dog.n', 'en', 'dog', 'n'),
                ('e2', 'o-dog.n', 'en', 'domestic dog', 'n'),
                ('e3', 'o-animal.n', 'en', 'animal', 'n'),
                ('e4', 'o-plant.n', 'en', 'plant', 'n');
             INSERT INTO omw_relations (source_id, target_id, rel_type) VALUES
                ('o-dog.n', 'o-animal.n', 'hypernym'),
                ('o-animal.n', 'o-organism.n', 'hypernym'),
                ('o-dog.n', 'o-animal.n', 'hyponym');
             INSERT INTO omw_sense_relations (source_entry_id, target_entry_id, rel_type) VALUES
                ('e3', 'e4', 'antonym');
             INSERT INTO antonym_pairs (word_a, word_b, source, language) VALUES
                ('dog', 'cat', 'antonym.txt', 'cmn');
             INSERT INTO file_tags (code, name, parent_codes) VALUES
                ('builtin.dog', '狗', '[\"omw.o-dog.n\", \"builtin.pet\"]'),
                ('builtin.pet', '宠物', '[\"omw.o-dog.n\"]'),
                ('builtin.cat', '猫', '[\"omw.o-dog.n\"]'),
                ('builtin.unrelated', '无关', '[]'),
                ('omw.o-dog.n', 'dog', '[\"builtin.pet\"]');",
        )
        .unwrap();

        conn
    }

    #[test]
    fn lookup_returns_synset_with_deduped_lemmas() {
        let conn = fixture();
        let results = lookup(&conn, "dog", "en").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "o-dog.n");
        assert_eq!(results[0].pos, "n");
        assert_eq!(results[0].definition.as_deref(), Some("a domesticated canine"));
        assert_eq!(results[0].lemmas, vec!["dog", "domestic dog"]);
        assert_eq!(results[0].meta["k"], 1);
    }

    #[test]
    fn lookup_is_case_insensitive_and_falls_back_to_english() {
        let conn = fixture();
        // 大小写不敏感
        assert_eq!(lookup(&conn, "DOG", "en").unwrap().len(), 1);
        // 目标语言无该 lemma 时回退英文
        assert_eq!(lookup(&conn, "dog", "cmn").unwrap().len(), 1);
        assert!(lookup(&conn, "no-such-word", "en").unwrap().is_empty());
    }

    #[test]
    fn hierarchy_walks_hypernym_chain_up_to_root() {
        let conn = fixture();
        let chain = hierarchy(&conn, "o-dog.n").unwrap();
        let ids: Vec<&str> = chain.iter().map(|n| n.synset_id.as_str()).collect();
        assert_eq!(ids, vec!["o-animal.n", "o-organism.n"]);
        assert!(chain.iter().all(|n| n.rel_type == "hypernym"));
        assert_eq!(chain[0].lemmas, vec!["animal"]);
    }

    #[test]
    fn hierarchy_terminates_on_cycle() {
        let conn = fixture();
        // 人为制造环：o-organism.n → o-dog.n
        conn.execute(
            "INSERT INTO omw_relations (source_id, target_id, rel_type) VALUES ('o-organism.n', 'o-dog.n', 'hypernym')",
            [],
        )
        .unwrap();
        let chain = hierarchy(&conn, "o-dog.n").unwrap();
        // 自身不会被再次纳入，链长收敛而非无限递归
        assert!(chain.len() <= HIERARCHY_MAX_DEPTH);
        assert!(!chain.iter().any(|n| n.synset_id == "o-dog.n"));
    }

    #[test]
    fn antonyms_prefers_antonym_pairs() {
        let conn = fixture();
        let results = antonyms(&conn, "dog", "cmn").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].antonym, "cat");
        assert_eq!(results[0].source, "antonym.txt");
        assert_eq!(results[0].language, "cmn");
    }

    #[test]
    fn antonyms_falls_back_to_omw_sense_relations() {
        let conn = fixture();
        let results = antonyms(&conn, "animal", "en").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].antonym, "plant");
        assert_eq!(results[0].source, "omw_sense_relations");
    }

    #[test]
    fn describe_returns_first_non_empty_definition() {
        let conn = fixture();
        assert_eq!(
            describe(&conn, "dog", "en").unwrap().as_deref(),
            Some("a domesticated canine")
        );
        // 定义为空/不存在的词返回 None（HowNet 兜底由 handler 负责）
        assert_eq!(describe(&conn, "organism", "en").unwrap(), None);
        assert_eq!(describe(&conn, "no-such-word", "en").unwrap(), None);
    }

    #[test]
    fn mapping_reverses_builtin_tags_sharing_omw_parent() {
        let conn = fixture();
        let results = mapping(&conn, "狗").unwrap();
        let codes: Vec<&str> = results.iter().map(|r| r.tag_code.as_str()).collect();
        // 共享 omw.o-dog.n 的兄弟标签，排除输入标签自身
        assert_eq!(codes, vec!["builtin.cat", "builtin.pet"]);
        assert!(results.iter().all(|r| r.match_level == 1 && r.confidence == 1.0));
    }

    #[test]
    fn mapping_returns_empty_without_omw_parents() {
        let conn = fixture();
        assert!(mapping(&conn, "无关").unwrap().is_empty());
        assert!(mapping(&conn, "不存在的标签").unwrap().is_empty());
    }

    #[test]
    fn mapping_uses_no_tag_omw_mapping_table() {
        let conn = fixture();
        // 夹具中根本不存在 tag_omw_mapping 表：查询能成功即证明零桥表依赖
        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='tag_omw_mapping'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(exists, 0);
        assert!(!mapping(&conn, "狗").unwrap().is_empty());
    }

    #[test]
    fn tree_returns_one_layer_children_of_root() {
        let conn = fixture();
        // root = omw.o-dog.n，子节点为 parent_codes 含它的标签
        let started = std::time::Instant::now();
        let nodes = tree(&conn, "omw.o-dog.n", 1).unwrap();
        // #650 验收：单次单层树响应耗时 < 50ms
        assert!(
            started.elapsed() < std::time::Duration::from_millis(50),
            "单层树查询应在 50ms 内返回，实际 {:?}",
            started.elapsed()
        );
        let codes: Vec<&str> = nodes.iter().map(|n| n.code.as_str()).collect();
        // builtin.dog, builtin.pet, builtin.cat 都在 parent_codes 里包含 omw.o-dog.n
        // omw.o-dog.n 自身 parent_codes 是 ["builtin.pet"]，不含自己
        assert_eq!(codes, vec!["builtin.cat", "builtin.dog", "builtin.pet"]);
        assert!(nodes.iter().all(|n| n.source == "builtin"));
    }

    #[test]
    fn tree_returns_empty_for_unknown_root() {
        let conn = fixture();
        let nodes = tree(&conn, "no-such-code", 1).unwrap();
        assert!(nodes.is_empty());
    }

    #[test]
    fn unmapped_stats_counts_synsets_not_in_any_parent_codes() {
        let conn = fixture();
        // 夹具中：o-dog.n 被引用（在 parent_codes 中），
        // o-animal.n / o-organism.n / o-plant.n 未被引用
        let stats = unmapped_stats(&conn).unwrap();
        // by_lexfile: 3 未引用 synsets (noun.animal + noun.entity + noun.plant)
        assert_eq!(stats.by_lexfile.iter().map(|g| g.count).sum::<i64>(), 3);
        // by_top_ancestor: o-animal.n 沿 hypernym 链到根 o-organism.n，取根的 lexfile = noun.entity；
        // o-organism.n 自身即根 = noun.entity；o-plant.n 根为 noun.plant
        let top_entity = stats.by_top_ancestor.iter().find(|g| g.key == "noun.entity").unwrap();
        assert_eq!(top_entity.count, 2);
        let top_plant = stats.by_top_ancestor.iter().find(|g| g.key == "noun.plant").unwrap();
        assert_eq!(top_plant.count, 1);
        // 回归守卫：中间的 o-animal.n 的自身 lexfile (noun.animal) 不得作为顶层祖先分组，
        // 否则 by_top_ancestor 会退化为 by_lexfile
        assert!(!stats.by_top_ancestor.iter().any(|g| g.key == "noun.animal"));
    }
}
