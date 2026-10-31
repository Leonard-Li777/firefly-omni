//! 现网（PRD 07 改造前）仲裁的**独立复刻** —— 等价性对拍的 oracle。
//!
//! ## 为什么 oracle 必须独立
//!
//! 若 oracle 复用 `TagAliasBuilder` 的仲裁代码，两边同错会被判成「0 差异」，测试变成
//! 自证。Slice 0 的教训更直接：脚本模拟现网规则给出的差异是 4,942，真实实现给出 51,875，
//! 偏差 10.5 倍。因此这里用 `String` 键的朴素容器逐字复刻改造前
//! `tag_identity::load_aliases_for_lang` 的三张缓冲（存储形态与被测实现完全无关），
//! 再与 Arena 双索引对拍。
//!
//! ## 数据源
//!
//! 读真实 `semantic.raw.db`（生产 `semantic.pack` 解密后的同一份库，别名分表内容一致）。
//! 路径优先级：`OMNI_SEMANTIC_RAW_DB` 环境变量 → 仓库内构建产物。
//! 两者都不存在时由调用方**显式跳过并打印原因**（该文件是构建产物、不入库）。

use std::collections::HashMap;
use std::path::PathBuf;

use crate::tag_identity::{
    en_builtin_code, english_lemma_candidates, is_controlled_code, normalize_language_code,
    normalize_lemma, tag_display, BUILTIN_ALIASES,
};

/// 定位真实语义库（未加密 raw 镜像）。
pub(super) fn raw_db_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("OMNI_SEMANTIC_RAW_DB") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
        panic!(
            "OMNI_SEMANTIC_RAW_DB 指向的文件不存在: {}（不接受静默跳过）",
            p.display()
        );
    }
    // CARGO_MANIFEST_DIR = apps/omni/crates/omni-core
    let default = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../apps/desktop/build/presetResources/semantic/semantic.raw.db");
    if default.is_file() {
        Some(default)
    } else {
        None
    }
}

/// 枚举 `tag_aliases_*` 分表并**按表名字母序**排序。
///
/// 逐字对齐 `omw_db::list_tag_alias_tables`：GLOB 里 `_` 是字面量（LIKE 的 `_` 是单字符
/// 通配），白名单过滤异常表名，最后 `sort()` —— 全局仲裁结果依赖这个顺序，基线必须同序。
fn list_tag_alias_tables(conn: &rusqlite::Connection) -> Vec<String> {
    let mut tables = Vec::new();
    let sql = "SELECT name FROM sqlite_master WHERE type = 'table' AND name GLOB 'tag_aliases_*'";
    let Ok(mut stmt) = conn.prepare(sql) else {
        return tables;
    };
    let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) else {
        return tables;
    };
    for name in rows.flatten() {
        if name.starts_with("tag_aliases_")
            && !name.chars().any(|c| c == '\'' || c == '"' || c == ';' || c == ' ')
        {
            tables.push(name);
        }
    }
    tables.sort();
    tables
}

/// 读一张分表，返回 `(lemma, tag_code, is_canonical)`。
///
/// SQL 与 `omw_db::hotload_aliases_from_conn` 逐字相同：`ORDER BY count DESC,
/// is_canonical DESC` 是「同优先级下保留最先入表的高频词义」这一仲裁前提，顺序变了结论就变。
/// `is_canonical = is_canonical != 0 || count >= 100` 同样照抄生产。
fn read_table(conn: &rusqlite::Connection, table: &str) -> Vec<(String, String, bool)> {
    let sql = format!(
        "SELECT lemma, tag_code, COALESCE(is_canonical, 0), COALESCE(count, 0) FROM {table} \
         ORDER BY count DESC, is_canonical DESC"
    );
    if let Ok(mut stmt) = conn.prepare(&sql) {
        if let Ok(rows) = stmt.query_map([], |row| {
            let is_can: i32 = row.get(2).unwrap_or(0);
            let count: i64 = row.get(3).unwrap_or(0);
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                is_can != 0 || count >= 100,
            ))
        }) {
            return rows.flatten().collect();
        }
    }
    // 生产侧同款降级：缺 is_canonical 列的旧版分表按两列热载（规范展示名轨不可用）
    let fallback_sql = format!("SELECT lemma, tag_code FROM {table}");
    if let Ok(mut stmt) = conn.prepare(&fallback_sql) {
        if let Ok(rows) = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, false))
        }) {
            return rows.flatten().collect();
        }
    }
    Vec::new()
}

fn code_priority(code: &str) -> u8 {
    if code.starts_with("omw.") {
        3
    } else if code.starts_with("hownet.") {
        2
    } else if code.starts_with("builtin.") {
        1
    } else {
        0
    }
}

/// 改造前 `load_aliases_for_lang` 三张缓冲的独立复刻。
#[derive(Default)]
pub(super) struct LegacyAliasOracle {
    /// lemma → code（全局，无语言键；等价 `DYNAMIC_ALIASES`）
    pub(super) global: HashMap<String, String>,
    /// 语言 → lemma → code（等价 `DYNAMIC_ALIASES_BY_LANG`）
    pub(super) by_lang: HashMap<String, HashMap<String, String>>,
    /// 语言 → code → 原始词形（等价 `DYNAMIC_CANONICAL_BY_LANG`，值未 normalize）
    pub(super) canonical: HashMap<String, HashMap<String, String>>,
}

impl LegacyAliasOracle {
    pub(super) fn push_lang<I, S1, S2>(&mut self, lang: &str, entries: I)
    where
        I: IntoIterator<Item = (S1, S2, bool)>,
        S1: AsRef<str>,
        S2: AsRef<str>,
    {
        let norm_lang = normalize_language_code(lang).to_string();
        for (lemma, code, is_canonical) in entries {
            let norm = normalize_lemma(lemma.as_ref());
            if norm.is_empty() {
                continue;
            }
            let code = code.as_ref();
            if !is_controlled_code(code) {
                continue;
            }
            let code = code.to_string();

            let lang_map = self.by_lang.entry(norm_lang.clone()).or_default();
            // `prefer_incoming` = 严格更高优先级；同优先级保留先入表者
            let should_insert = match lang_map.get(&norm) {
                Some(existing) => is_canonical && code_priority(existing) < code_priority(&code),
                None => true,
            };

            if should_insert {
                lang_map.insert(norm.clone(), code.clone());
                match self.global.get(&norm) {
                    Some(existing) if existing == &code => {}
                    Some(existing)
                        if !is_canonical || code_priority(existing) >= code_priority(&code) => {}
                    _ => {
                        self.global.insert(norm.clone(), code.clone());
                    }
                }
            }

            let can_map = self.canonical.entry(norm_lang.clone()).or_default();
            if is_canonical || !can_map.contains_key(&code) {
                can_map.insert(code, lemma.as_ref().to_string());
            }
        }
    }

    pub(super) fn by_lang_len(&self) -> usize {
        self.by_lang.values().map(|m| m.len()).sum()
    }

    pub(super) fn canonical_len(&self) -> usize {
        self.canonical.values().map(|m| m.len()).sum()
    }

    /// 旧实现的单语言分表反查（含英文形态学候选）
    pub(super) fn lang_lookup(&self, lang: &str, key: &str) -> Option<String> {
        let map = self.by_lang.get(lang)?;
        if let Some(code) = map.get(key) {
            if is_controlled_code(code) {
                return Some(code.clone());
            }
        }
        for cand in english_lemma_candidates(key) {
            if let Some(code) = map.get(&cand) {
                if is_controlled_code(code) {
                    return Some(code.clone());
                }
            }
        }
        None
    }
}

/// 静态 builtin 底座（`tag_identity` 内 `alias_map` / `en_to_code` 的独立复刻）。
///
/// 两阶段入口的「同语种未命中 → 查静态底座 → 仍未命中即判未登录词」必须一并复刻，
/// 否则 oracle 会把静态命中误判成差异。
pub(super) struct StaticBase {
    pub(super) alias_map: HashMap<String, String>,
    pub(super) en_to_code: HashMap<String, String>,
}

impl StaticBase {
    pub(super) fn new() -> Self {
        let mut alias_map = HashMap::new();
        let mut en_to_code = HashMap::new();
        for (alias, en) in BUILTIN_ALIASES {
            alias_map.insert(normalize_lemma(alias), (*en).to_string());
            alias_map.insert(normalize_lemma(en), (*en).to_string());
            en_to_code.insert((*en).to_string(), en_builtin_code(en));
        }
        Self {
            alias_map,
            en_to_code,
        }
    }

    pub(super) fn lookup_with_en_lemmas(&self, key: &str) -> Option<String> {
        if let Some(en) = self.alias_map.get(key) {
            if let Some(code) = self.en_to_code.get(en.as_str()) {
                return Some(code.clone());
            }
        }
        if let Some(code) = self.en_to_code.get(key) {
            return Some(code.clone());
        }
        for cand in english_lemma_candidates(key) {
            if let Some(en) = self.alias_map.get(&cand) {
                if let Some(code) = self.en_to_code.get(en.as_str()) {
                    return Some(code.clone());
                }
            }
            if let Some(code) = self.en_to_code.get(&cand) {
                return Some(code.clone());
            }
        }
        None
    }
}

/// 旧 `resolve_controlled_tag_two_stage` 的独立复刻（ADR-0048 三段结构）。
///
/// 注意「同语种」判定必须是**具体语言相等**，不得退化成任何分组相等 —— 否则
/// `curr_lang="de"` 遇到英文词会被误判成直查命中，德语用户拿不到德语展示名。
pub(super) fn legacy_two_stage(
    oracle: &LegacyAliasOracle,
    base: &StaticBase,
    tag: &str,
    current_lang: Option<&str>,
    detect_lang: impl Fn(&str) -> &'static str,
) -> Option<(String, Option<String>)> {
    let key = normalize_lemma(tag);
    if key.is_empty() {
        return None;
    }
    let curr_lang = normalize_language_code(current_lang.unwrap_or("zh"));
    let detected_lang = detect_lang(tag);

    let resolve_canonical = |code: &str| -> Option<String> {
        if let Some(name) = oracle.canonical.get(curr_lang).and_then(|m| m.get(code)) {
            return Some(name.clone());
        }
        // 兜底链（静态 canonical + slug）两侧共用同一份代码，且 canonical 索引已由
        // 全量对拍断言与 oracle 逐条相同，故这里直接复用生产的展示名解析
        let fallback = tag_display(code, curr_lang);
        if fallback != code {
            Some(fallback)
        } else {
            None
        }
    };

    // 第一阶段：当前语言直查
    if detected_lang == curr_lang {
        if curr_lang == "en" {
            if let Some(code) = oracle.lang_lookup(curr_lang, &key) {
                return Some((code, None));
            }
        } else if let Some(code) = oracle.by_lang.get(curr_lang).and_then(|m| m.get(&key)) {
            if is_controlled_code(code) {
                return Some((code.clone(), None));
            }
        }
        // 当前语言静态底座
        if curr_lang == "zh" {
            if let Some(en) = base.alias_map.get(&key) {
                if let Some(code) = base.en_to_code.get(en.as_str()) {
                    return Some((code.clone(), None));
                }
            }
        } else if curr_lang == "en" {
            if let Some(code) = base.lookup_with_en_lemmas(&key) {
                return Some((code, None));
            }
        }
        // 同语种未查得 ⇒ 直接判未登录词（ADR-0048 契约，不加跨语兜底）
        return None;
    }

    // 异语阶段
    let mut matched: Option<String> = None;
    if detected_lang == "en" {
        matched = oracle.lang_lookup(detected_lang, &key);
    } else if let Some(code) = oracle.by_lang.get(detected_lang).and_then(|m| m.get(&key)) {
        if is_controlled_code(code) {
            matched = Some(code.clone());
        }
    }
    if matched.is_none() && detected_lang != "en" {
        matched = oracle.lang_lookup("en", &key);
    }
    if matched.is_none() {
        matched = base.lookup_with_en_lemmas(&key);
    }
    matched.map(|code| {
        let canon = resolve_canonical(&code);
        (code, canon)
    })
}

/// 一次性全量装载的真实输入（分表顺序、行序、is_canonical 判定全按生产口径）
pub(super) struct PackRows {
    /// (语言码, 该表全部行)
    pub(super) tables: Vec<(String, Vec<(String, String, bool)>)>,
}

impl PackRows {
    pub(super) fn load(conn: &rusqlite::Connection) -> Self {
        let mut tables = Vec::new();
        for table in list_tag_alias_tables(conn) {
            let lang = table.strip_prefix("tag_aliases_").unwrap_or("zh");
            let rows = read_table(conn, &table);
            assert!(
                !rows.is_empty(),
                "分表 {table} 读到 0 行 —— 数据源不是语义包或已损坏（零降级：不接受空壳）"
            );
            tables.push((lang.to_string(), rows));
        }
        assert!(!tables.is_empty(), "未发现任何 tag_aliases_{{lang}} 分表");
        Self { tables }
    }

    pub(super) fn total_rows(&self) -> usize {
        self.tables.iter().map(|(_, rows)| rows.len()).sum()
    }

    pub(super) fn build_oracle(&self) -> LegacyAliasOracle {
        let mut oracle = LegacyAliasOracle::default();
        for (lang, rows) in &self.tables {
            oracle.push_lang(
                lang,
                rows.iter().map(|(l, c, k)| (l.as_str(), c.as_str(), *k)),
            );
        }
        oracle
    }
}
