//! 受控标签别名的紧凑索引表示（PRD 07）
//!
//! 目标：把「多套 `String` 键容器 + 逐 key `Box::leak`」换成「单一 Arena 文本 + 定长键索引
//! + 进程级 code intern」，在不改变任何对外解析契约的前提下把常驻内存从 95.39 MB 降到
//! 约 30 MB 量级。
//!
//! ## 三张索引与现网三张缓冲逐条等价（PRD 07 决策 2 / 3 / 4）
//!
//! | 索引 | 语义等价于 | 服务入口 |
//! | :-- | :-- | :-- |
//! | [`TagAliasIndex::global_code`] | `DYNAMIC_ALIASES` | `resolve_controlled_tag_code` / `builtin_tag_code` |
//! | [`TagAliasIndex::lang_code`] | `DYNAMIC_ALIASES_BY_LANG` | `resolve_controlled_tag_two_stage` |
//! | [`TagAliasIndex::canonical_name`] | `DYNAMIC_CANONICAL_BY_LANG` | `tag_display` / 就地本地化 |
//!
//! ## 实现红线（PRD 07 决策 9）
//!
//! 索引内只存 `u32` / `u64`，**绝不存指向 Arena 的裸引用**，否则结构变成自引用、
//! `Arc` 替换时无法安全释放。唯一允许的 `&'static str` 是 `code_intern` 表内的 code 文本
//! （进程级 `'static`，不指向 Arena）。
//!
//! ## 为什么不做「语言域合并 + LID 直查」（Slice 0 实测结论）
//!
//! 装载按**表语言**、查询按 **LID**，二者对同一 lemma 会分歧：`$`、`-人` 只存在于
//! `tag_aliases_ja_JP`，而 `detect_tag_language` 分别返回 `en`、`zh`；`/` 只在
//! `tag_aliases_ko_KR`，LID 返回 `en`。实测按 LID 判域直查会丢失 **47,961** 条
//! （占现网全局表 12.4%），且拉丁五语合并后 22.85% 的 lemma 有 ≥2 个 code，产生
//! 3,431 条 `omw.* → omw.*` 漂移。因此本模块保留**全局索引**（与现网逐条等价）
//! 并按语言另建索引，**不引入任何仲裁覆盖表**。

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use crate::tag_identity::{is_controlled_code, normalize_language_code, normalize_lemma};

/// 10 个具体语种的稳定 id。
///
/// **不复用任何「语言域」概念**：域合并会让 de/fr/es/pt 抢同一 canonical 槽位，
/// 且 LID 分组与分表语言分组不一致时会丢词（见模块文档）。
pub fn language_id(lang: &str) -> u8 {
    match normalize_language_code(lang) {
        "ar" => 0,
        "de" => 1,
        "en" => 2,
        "es" => 3,
        "fr" => 4,
        "ja" => 5,
        "ko" => 6,
        "pt" => 7,
        "ru" => 8,
        "zh" => 9,
        // `normalize_language_code` 的兜底就是 "en"，此分支不可达，保留以防未来改契约
        _ => 2,
    }
}

/// 稳定哈希：`DefaultHasher::new()` 的 key 固定，因此跨进程可复现（不使用随机种子）。
fn hash_str(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// 受控前缀优先级：`omw.*`(3) > `hownet.*`(2) > `builtin.*`(1) > 其它(0)
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

// ───────────────────────────── code intern 层 ─────────────────────────────

struct CodeIntern {
    texts: Vec<&'static str>,
    map: HashMap<&'static str, u32>,
}

/// 进程级 code intern。
///
/// **永不清空**（包括 `clear_dynamic_aliases()` 之后）：已发给调用方的 `&'static str`
/// 一旦悬垂即为 UB。code 集合稳定（78,323 条），重载稳态零新增泄漏。
fn code_intern() -> &'static Mutex<CodeIntern> {
    static INTERN: OnceLock<Mutex<CodeIntern>> = OnceLock::new();
    INTERN.get_or_init(|| {
        Mutex::new(CodeIntern {
            texts: Vec::new(),
            map: HashMap::new(),
        })
    })
}

/// 取 code 的稳定下标；首次出现才 `Box::leak` 一次，重载只查表复用指针。
fn intern_code(code: &str) -> u32 {
    let mut guard = code_intern().lock().expect("code intern 锁中毒");
    if let Some(&idx) = guard.map.get(code) {
        return idx;
    }
    let leaked: &'static str = Box::leak(code.to_string().into_boxed_str());
    let idx = guard.texts.len() as u32;
    guard.texts.push(leaked);
    guard.map.insert(leaked, idx);
    idx
}

/// 下标 → code 文本（`&'static`，可安全返回给调用方）
pub fn code_text(idx: u32) -> Option<&'static str> {
    let guard = code_intern().lock().ok()?;
    guard.texts.get(idx as usize).copied()
}

/// code 文本 → 下标（canonical 索引键需要）
pub fn code_id(code: &str) -> Option<u32> {
    let guard = code_intern().lock().ok()?;
    guard.map.get(code).copied()
}

/// 已 intern 的 code 条数。用于断言「二次构建不新增泄漏」。
pub fn code_intern_len() -> usize {
    code_intern().lock().map(|g| g.texts.len()).unwrap_or(0)
}

fn priority_of_idx(idx: u32) -> u8 {
    code_text(idx).map(code_priority).unwrap_or(0)
}

// ───────────────────────────── 开放寻址表 ─────────────────────────────

#[derive(Clone, Copy)]
struct Slot {
    key: u64,
    off: u32,
    len: u32,
    code: u32,
}

/// 空槽哨兵。真实 code 下标远小于该值（实际 78,323）。
const EMPTY_CODE: u32 = u32::MAX;

struct SlotTable {
    slots: Vec<Slot>,
    mask: usize,
    len: usize,
}

impl SlotTable {
    fn with_capacity(expected: usize) -> Self {
        // 负载因子 ≤ 0.5，保证线性探测链短且不会写满
        let cap = expected.saturating_mul(2).next_power_of_two().max(16);
        Self {
            slots: vec![
                Slot {
                    key: 0,
                    off: 0,
                    len: 0,
                    code: EMPTY_CODE,
                };
                cap
            ],
            mask: cap - 1,
            len: 0,
        }
    }

    /// 插入别名条目。同 key 且**文本相同**视为同一 lemma（覆盖）；文本不同是哈希碰撞，
    /// 继续探测 —— 这正是「碰撞校验比对 lemma 而非 code」的落点。
    fn insert(&mut self, key: u64, off: u32, len: u32, code: u32, arena: &[u8]) {
        let mut idx = key as usize & self.mask;
        loop {
            let slot = self.slots[idx];
            if slot.code == EMPTY_CODE {
                self.slots[idx] = Slot {
                    key,
                    off,
                    len,
                    code,
                };
                self.len += 1;
                return;
            }
            if slot.key == key {
                let existing = &arena[slot.off as usize..(slot.off + slot.len) as usize];
                let incoming = &arena[off as usize..(off + len) as usize];
                if existing == incoming {
                    self.slots[idx].code = code;
                    return;
                }
            }
            idx = (idx + 1) & self.mask;
        }
    }

    /// 插入唯一键（canonical：`(语言 id, code 下标)` 本身即唯一身份，无需文本比对）
    fn insert_unique(&mut self, key: u64, off: u32, len: u32) {
        let mut idx = key as usize & self.mask;
        loop {
            let slot = self.slots[idx];
            if slot.code == EMPTY_CODE {
                self.slots[idx] = Slot {
                    key,
                    off,
                    len,
                    code: 0,
                };
                self.len += 1;
                return;
            }
            if slot.key == key {
                self.slots[idx].off = off;
                self.slots[idx].len = len;
                return;
            }
            idx = (idx + 1) & self.mask;
        }
    }

    /// 查别名：key 相同且文本相同才算命中（哈希碰撞时继续探测）
    fn get(&self, key: u64, probe: &[u8], arena: &[u8]) -> Option<u32> {
        let mut idx = key as usize & self.mask;
        loop {
            let slot = self.slots[idx];
            if slot.code == EMPTY_CODE {
                return None;
            }
            if slot.key == key {
                let existing = &arena[slot.off as usize..(slot.off + slot.len) as usize];
                if existing == probe {
                    return Some(slot.code);
                }
            }
            idx = (idx + 1) & self.mask;
        }
    }

    /// 查唯一键（canonical）
    fn get_value(&self, key: u64) -> Option<(u32, u32)> {
        let mut idx = key as usize & self.mask;
        loop {
            let slot = self.slots[idx];
            if slot.code == EMPTY_CODE {
                return None;
            }
            if slot.key == key {
                return Some((slot.off, slot.len));
            }
            idx = (idx + 1) & self.mask;
        }
    }

}

// ───────────────────────────── 构建器 ─────────────────────────────

/// 热载构建器：`push_lang` 反复累积，`finish` 一次性产出不可变索引。
///
/// 构建期内部使用 `HashMap<String, _>` 做仲裁 —— 这是**刻意**的：仲裁规则必须与现网
/// `load_aliases_for_lang` 逐字一致，用既有容器最不容易写错。这些 `String` 键只在构建
/// 期间存在，`finish` 后随 builder 释放，不进入稳态常驻（用户故事 34：不留第二事实源）。
pub struct TagAliasBuilder {
    arena: Vec<u8>,
    text_off: HashMap<String, (u32, u32)>,
    /// lemma（normalize 后）→ (code 下标, Arena 偏移, 长度)
    global: HashMap<String, (u32, u32, u32)>,
    /// (语言 id, lemma（normalize 后）) → (code 下标, Arena 偏移, 长度)
    by_lang: HashMap<(u8, String), (u32, u32, u32)>,
    canonical: HashMap<u64, (u32, u32)>,
    rows: usize,
}

impl Default for TagAliasBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TagAliasBuilder {
    pub fn new() -> Self {
        Self {
            arena: Vec::new(),
            text_off: HashMap::new(),
            global: HashMap::new(),
            by_lang: HashMap::new(),
            canonical: HashMap::new(),
            rows: 0,
        }
    }

    /// 追加文本到 Arena 并返回 `(偏移, 长度)`；同一文本只存一份（用户故事 40）。
    fn push_text(&mut self, text: &str) -> (u32, u32) {
        if let Some(&pos) = self.text_off.get(text) {
            return pos;
        }
        let off = self.arena.len() as u32;
        let len = text.len() as u32;
        self.arena.extend_from_slice(text.as_bytes());
        self.text_off.insert(text.to_string(), (off, len));
        (off, len)
    }

    /// 灌入一张语言分表。仲裁规则逐字复刻 `tag_identity::load_aliases_for_lang`：
    ///
    /// - 语言分表：`is_canonical && prefer_incoming` 才覆盖，否则先入为主；
    /// - 全局表：仅当该语言分表发生插入时才参与，同样受 `is_canonical` 与优先级约束；
    /// - canonical：`is_canonical || 该 code 尚无展示名`，**独立于**语言分表是否插入。
    pub fn push_lang<I, S1, S2>(&mut self, lang: &str, entries: I)
    where
        I: IntoIterator<Item = (S1, S2, bool)>,
        S1: AsRef<str>,
        S2: AsRef<str>,
    {
        let lid = language_id(lang);
        for (lemma, code, is_canonical) in entries {
            self.rows += 1;
            let norm = normalize_lemma(lemma.as_ref());
            if norm.is_empty() {
                continue;
            }
            let code_ref = code.as_ref();
            // 门禁：受控别名表严禁载入 _ext.* 非受控扩展代码
            if !is_controlled_code(code_ref) {
                continue;
            }
            let code_idx = intern_code(code_ref);

            let lang_key = (lid, norm.clone());
            let should_insert = match self.by_lang.get(&lang_key) {
                Some(&existing) => {
                    is_canonical && priority_of_idx(existing.0) < priority_of_idx(code_idx)
                }
                None => true,
            };

            if should_insert {
                // 全局索引与按语言索引共享同一份 normalize 文本（用户故事 40）
                let (off, len) = self.push_text(&norm);
                self.by_lang.insert(lang_key, (code_idx, off, len));
                match self.global.get(&norm) {
                    Some(&existing) if existing.0 == code_idx => {}
                    Some(&existing)
                        if !is_canonical
                            || priority_of_idx(existing.0) >= priority_of_idx(code_idx) => {}
                    _ => {
                        self.global.insert(norm.clone(), (code_idx, off, len));
                    }
                }
            }

            // 规范展示名：按 **code** 在**具体语言**内登记，与 lemma 映射是否插入无关
            let canon_key = ((lid as u64) << 32) | code_idx as u64;
            if is_canonical || !self.canonical.contains_key(&canon_key) {
                // 生产侧存的是原始 lemma（未 normalize 的词形），保持同源
                let (off, len) = self.push_text(lemma.as_ref());
                self.canonical.insert(canon_key, (off, len));
            }
        }
    }

    /// 已消费的原始行数（含被过滤的行），用于与热载日志对账
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// 产出不可变索引并释放构建期容器（消费式；`seal_alias_builder` 走这条路径）。
    pub fn finish(self) -> TagAliasIndex {
        let TagAliasBuilder {
            arena,
            global,
            by_lang,
            canonical,
            ..
        } = self;
        assemble(arena, &global, &by_lang, &canonical)
    }

    /// 产出不可变索引但**保留** builder，供后续增量继续叠加。
    ///
    /// 为什么必须保留：热载是**追加式**的（同一进程可能分多批 `push_lang`，批次之间还会有
    /// 读取）。若提交时取走 builder，下一批增量会从空 builder 重建索引并整体覆盖安装槽，
    /// 把先前批次灌入的别名全部丢掉 —— 实测症状是 `Connected` 退化成 `None`（`connect`
    /// 来自更早一批）。正确性优先于构建期内存，稳态紧凑由显式 `seal_alias_builder` 收口。
    pub fn snapshot(&self) -> TagAliasIndex {
        assemble(
            self.arena.clone(),
            &self.global,
            &self.by_lang,
            &self.canonical,
        )
    }
}

/// 把构建期容器装配成不可变索引（`finish` 与 `snapshot` 共用同一装配口径）
fn assemble(
    arena: Vec<u8>,
    global: &HashMap<String, (u32, u32, u32)>,
    by_lang: &HashMap<(u8, String), (u32, u32, u32)>,
    canonical: &HashMap<u64, (u32, u32)>,
) -> TagAliasIndex {
    let mut g = SlotTable::with_capacity(global.len());
    let mut b = SlotTable::with_capacity(by_lang.len());
    let mut c = SlotTable::with_capacity(canonical.len());

    for (lemma, (code_idx, off, len)) in global {
        g.insert(hash_str(lemma), *off, *len, *code_idx, &arena);
    }
    for ((lid, lemma), (code_idx, off, len)) in by_lang {
        let key = ((*lid as u64) << 60) | (hash_str(lemma) & 0x0FFF_FFFF_FFFF_FFFF);
        b.insert(key, *off, *len, *code_idx, &arena);
    }
    for (key, (off, len)) in canonical {
        c.insert_unique(*key, *off, *len);
    }

    TagAliasIndex {
        arena: arena.into_boxed_slice(),
        global: g,
        by_lang: b,
        canonical: c,
    }
}

// ───────────────────────────── 不可变索引 ─────────────────────────────

/// 别名热载的稳态表示。只读、可 `Send + Sync`、可整体 `Arc` 替换。
pub struct TagAliasIndex {
    arena: Box<[u8]>,
    global: SlotTable,
    by_lang: SlotTable,
    canonical: SlotTable,
}

impl std::fmt::Debug for TagAliasIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TagAliasIndex")
            .field("arena_bytes", &self.arena.len())
            .field("global_len", &self.global.len)
            .field("by_lang_len", &self.by_lang.len)
            .field("canonical_len", &self.canonical.len)
            .finish()
    }
}

impl TagAliasIndex {
    /// 全局语义直查（等价 `DYNAMIC_ALIASES`）
    pub fn global_code(&self, lemma: &str) -> Option<&'static str> {
        let idx = self
            .global
            .get(hash_str(lemma), lemma.as_bytes(), &self.arena)?;
        code_text(idx)
    }

    /// 按具体语言直查（等价 `DYNAMIC_ALIASES_BY_LANG`）
    pub fn lang_code(&self, lang: &str, lemma: &str) -> Option<&'static str> {
        let key = ((language_id(lang) as u64) << 60) | (hash_str(lemma) & 0x0FFF_FFFF_FFFF_FFFF);
        let idx = self.lang_code_idx(key, lemma.as_bytes())?;
        code_text(idx)
    }

    fn lang_code_idx(&self, key: u64, probe: &[u8]) -> Option<u32> {
        // by_lang 的碰撞探测与 global 同构：key 相同再比文本
        let mut idx = key as usize & self.by_lang.mask;
        loop {
            let slot = self.by_lang.slots[idx];
            if slot.code == EMPTY_CODE {
                return None;
            }
            if slot.key == key {
                let existing = &self.arena[slot.off as usize..(slot.off + slot.len) as usize];
                if existing == probe {
                    return Some(slot.code);
                }
            }
            idx = (idx + 1) & self.by_lang.mask;
        }
    }

    /// 按具体语言取 code 的规范展示名（等价 `DYNAMIC_CANONICAL_BY_LANG`）
    pub fn canonical_name(&self, lang: &str, code: &str) -> Option<&str> {
        let code_idx = code_id(code)?;
        let key = ((language_id(lang) as u64) << 32) | code_idx as u64;
        let (off, len) = self.canonical.get_value(key)?;
        std::str::from_utf8(&self.arena[off as usize..(off + len) as usize]).ok()
    }

    /// Arena 常驻字节（S5 断言「多次重载不递增」的口径）
    pub fn arena_bytes(&self) -> usize {
        self.arena.len()
    }

    /// `(全局, 按语言, canonical)` 条目数
    pub fn counts(&self) -> (usize, usize, usize) {
        (self.global.len, self.by_lang.len, self.canonical.len)
    }
}

// ───────────────────────────── 全局安装槽 ─────────────────────────────

fn installed_slot() -> &'static RwLock<Option<Arc<TagAliasIndex>>> {
    static SLOT: OnceLock<RwLock<Option<Arc<TagAliasIndex>>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(None))
}

/// 原子替换当前索引（构建在锁外完成，这里只换 `Arc`）
pub fn install(index: Arc<TagAliasIndex>) {
    if let Ok(mut guard) = installed_slot().write() {
        *guard = Some(index);
    }
}

/// 取当前索引的 `Arc` 克隆（读锁内克隆后即脱离锁）
pub fn installed() -> Option<Arc<TagAliasIndex>> {
    installed_slot().read().ok().and_then(|g| g.clone())
}

/// 卸载当前索引（断开态）；**code intern 层不清空**
pub fn uninstall() {
    if let Ok(mut guard) = installed_slot().write() {
        *guard = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(items: &[(&str, &str, bool)]) -> Vec<(String, String, bool)> {
        items
            .iter()
            .map(|(l, c, k)| (l.to_string(), c.to_string(), *k))
            .collect()
    }

    #[test]
    fn global_arbitration_prefers_omw_over_builtin() {
        let mut b = TagAliasBuilder::new();
        b.push_lang("en", entries(&[("invoice", "builtin.invoice", true)]));
        b.push_lang("zh", entries(&[("invoice", "omw.99999999.n", true)]));
        let idx = b.finish();
        assert_eq!(idx.global_code("invoice"), Some("omw.99999999.n"));
    }

    #[test]
    fn canonical_is_per_language_not_shared() {
        let mut b = TagAliasBuilder::new();
        b.push_lang("de", entries(&[("Kunst", "omw.00000001.n", true)]));
        b.push_lang("fr", entries(&[("art", "omw.00000001.n", true)]));
        let idx = b.finish();
        assert_eq!(idx.canonical_name("de", "omw.00000001.n"), Some("Kunst"));
        assert_eq!(idx.canonical_name("fr", "omw.00000001.n"), Some("art"));
    }

    #[test]
    fn lang_index_is_isolated() {
        let mut b = TagAliasBuilder::new();
        b.push_lang("ja", entries(&[("$", "omw.06821581.n", true)]));
        let idx = b.finish();
        assert_eq!(idx.lang_code("ja", "$"), Some("omw.06821581.n"));
        assert_eq!(idx.lang_code("en", "$"), None);
        // 全局索引不看语言，这正是 Slice 0 证伪「LID 判域直查」后保留的通道
        assert_eq!(idx.global_code("$"), Some("omw.06821581.n"));
    }

    #[test]
    fn code_intern_reuse_does_not_leak_twice() {
        let mut b1 = TagAliasBuilder::new();
        b1.push_lang("en", entries(&[("zzz-probe-word", "omw.12345678.n", true)]));
        let _ = b1.finish();
        let after_first = code_intern_len();

        let mut b2 = TagAliasBuilder::new();
        b2.push_lang("en", entries(&[("zzz-probe-word", "omw.12345678.n", true)]));
        let idx2 = b2.finish();

        assert_eq!(code_intern_len(), after_first, "重载不得新增 intern");
        assert_eq!(idx2.global_code("zzz-probe-word"), Some("omw.12345678.n"));
    }

    #[test]
    fn non_controlled_codes_are_rejected() {
        let mut b = TagAliasBuilder::new();
        b.push_lang("en", entries(&[("foo-ext", "_ext.foo.1234", true)]));
        let idx = b.finish();
        assert_eq!(idx.global_code("foo-ext"), None);
        assert_eq!(idx.counts(), (0, 0, 0));
    }

    #[test]
    fn canonical_requires_exact_language_id() {
        // de 与 fr 不得互串（防「分组相等即同语种」误判）
        let mut b = TagAliasBuilder::new();
        b.push_lang("de", entries(&[("Tisch", "omw.00000002.n", true)]));
        let idx = b.finish();
        assert_eq!(idx.canonical_name("de", "omw.00000002.n"), Some("Tisch"));
        assert_eq!(idx.canonical_name("fr", "omw.00000002.n"), None);
    }
}
