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
//! 索引内只存 `u32`，**绝不存指向 Arena 的裸引用**，否则结构变成自引用、
//! `Arc` 替换时无法安全释放。唯一允许的 `&'static str` 是 `code_intern` 表内的 code 文本
//! （进程级 `'static`，不指向 Arena）。
//!
//! ## 表示层账（Slice 5 实测驱动，勿凭直觉改回去）
//!
//! 首版把槽位写成 `{key: u64, off: u32, len: u32, code: u32}`（24 B），容量按
//! `expected * 2` 取 2 的幂 —— 三张表都会顶到 1,048,576 槽，光槽数组就 **72 MiB**，
//! 加上 Arena 后稳态 80 MB 量级，几乎吃光相对 95.39 MB 的全部收益。现在：
//!
//! - 槽位 `{key: u32, off: u32, code: u32}` = **12 B**（键用 32 位哈希，命中后一律比对
//!   Arena 文本，碰撞只多一次 memcmp，不损正确性）；
//! - Arena 文本 **NUL 结尾**，长度由终止符界定，槽位不再存 `len`；
//! - 容量用 Lemire fastrange（`mix32(key) * cap >> 32`）映射，**允许非 2 的幂**；
//! - 构建期几何 ×1.25 扩容（摊还 O(1)），`finish` 时一次性压实到负载因子 0.75；
//! - 构建期**不再用 `HashMap<String, _>` 做仲裁暂存**，直接往紧凑表里插 —— 否则热载峰值
//!   会是「构建期 70 MB + 已装索引 30 MB」，小内存机器上比优化前更糟。
//!
//! 实测（真实 `semantic.raw.db`，796,296 行，release）：**32,883,556 B = 31.36 MiB**，
//! 3 次重载字节恒定，`resolve_controlled_tag_code` 627.7 ns/次（基线 643 ns）。
//!
//! ## 两个踩过的坑（都有单测守着）
//!
//! 1. **fastrange 前必须 `mix32`**：`canon_key` 是稠密恒等键、`lang_key` 把语言 id 放最高位，
//!    而 fastrange 取的是高位 —— 不混合时 78,323 个 canonical 键只落 13 个起始槽，全量热载
//!    实测挂死 3 小时 18 分。守门用例：`dense_identity_keys_are_spread_by_fastrange`。
//! 2. **扩容不能定量到 `len*4/3`**：只留 8 个空槽余量，下次插入立刻再扩，退化成每插 2~3 条
//!    就整表重哈希 = O(n²)（实测挂死 3 分半）。省内存靠 `compact()` 在 `finish` 里做。
//!    守门用例：`rehash_count_is_logarithmic_not_quadratic`。
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
///
/// 只取低 32 位做槽位键：命中后一律比对 Arena 文本，截断只会多几次 memcmp，
/// 不会产生错命中（这是 12 B 槽位成立的前提）。
fn hash32(s: &str) -> u32 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish() as u32
}

/// 全局索引键：lemma 哈希（不含语言位）
fn global_key(lemma: &str) -> u32 {
    hash32(lemma)
}

/// 按语言索引键：`语言 id(4 bit) | lemma 哈希(28 bit)`
fn lang_key(lid: u8, lemma: &str) -> u32 {
    ((lid as u32) << 28) | (hash32(lemma) & 0x0FFF_FFFF)
}

/// canonical 索引键：`语言 id(4 bit) | code 下标(24 bit)`
///
/// 该键**本身就是唯一身份**（不靠哈希），因此 `code` 下标必须真的装得进 24 位；
/// `intern_code` 在越界时会直接崩溃，不会静默产生键碰撞。
fn canon_key(lid: u8, code_idx: u32) -> u32 {
    ((lid as u32) << 24) | code_idx
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

/// canonical 键里 code 下标只有 24 位
const MAX_INTERNED_CODES: u32 = 1 << 24;

/// 取 code 的稳定下标；首次出现才 `Box::leak` 一次，重载只查表复用指针。
fn intern_code(code: &str) -> u32 {
    let mut guard = code_intern().lock().expect("code intern 锁中毒");
    if let Some(&idx) = guard.map.get(code) {
        return idx;
    }
    let idx = guard.texts.len() as u32;
    assert!(
        idx < MAX_INTERNED_CODES,
        "唯一受控 code 数超过 canonical 键空间（2^24 = {MAX_INTERNED_CODES}），\
         继续 intern 会让 canon_key 发生静默碰撞"
    );
    let leaked: &'static str = Box::leak(code.to_string().into_boxed_str());
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

// ───────────────────────────── Arena ─────────────────────────────

/// 连续文本缓冲：每条文本 **NUL 结尾**，槽位只存起始偏移。
///
/// NUL 是 UTF-8 合法序列里永不出现的字节，因此可安全作终止符；这样槽位不必存长度，
/// 单槽从 16 B 降到 12 B（1,153,656 条文本合计省下 4.4 MiB）。
struct Arena {
    buf: Vec<u8>,
}

impl Arena {
    fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// 追加一条文本，返回起始偏移
    fn push(&mut self, text: &str) -> u32 {
        let off = self.buf.len() as u32;
        self.buf.extend_from_slice(text.as_bytes());
        self.buf.push(0);
        off
    }

    /// 判断 `off` 处的文本是否等于 `probe`（先比长度与内容，再确认终止符）
    #[inline]
    fn matches(&self, off: u32, probe: &[u8]) -> bool {
        let start = off as usize;
        let end = start + probe.len();
        end < self.buf.len() && &self.buf[start..end] == probe && self.buf[end] == 0
    }

    /// 取 `off` 处的文本（到 NUL 为止）
    fn get(&self, off: u32) -> Option<&str> {
        let rest = self.buf.get(off as usize..)?;
        let end = rest.iter().position(|&b| b == 0)?;
        std::str::from_utf8(&rest[..end]).ok()
    }

    fn bytes(&self) -> usize {
        self.buf.capacity()
    }
}

// ───────────────────────────── 开放寻址表 ─────────────────────────────

#[derive(Clone, Copy)]
struct Slot {
    /// 32 位哈希键（canonical 表为 `语言 id | code 下标` 的精确键）
    key: u32,
    /// Arena 文本起始偏移
    off: u32,
    /// alias 表：code intern 下标；canonical 表：恒 0
    code: u32,
}

impl Slot {
    const EMPTY: u32 = u32::MAX;

    const fn empty() -> Self {
        Self {
            key: 0,
            off: 0,
            code: Self::EMPTY,
        }
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.code == Self::EMPTY
    }
}

/// 开放寻址（线性探测）定长键表，单槽 12 B。
///
/// 容量用 Lemire fastrange 映射，**允许非 2 的幂**：构建期几何 ×1.25 增长保证摊还 O(1)，
/// `finish` 时一次性压到负载因子 0.75（见 [`SlotTable::compact`]）。
struct SlotTable {
    slots: Vec<Slot>,
    len: usize,
}

/// 重哈希次数计数器（仅测试用）。用于断言扩容是几何级而非线性级 —— 这是
/// 「扩到 `len*4/3`」那个 bug 的确定性判据，不依赖计时。
#[cfg(test)]
static REHASH_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
fn rehash_count() -> usize {
    REHASH_COUNT.load(std::sync::atomic::Ordering::Relaxed)
}

impl SlotTable {
    fn new() -> Self {
        Self {
            slots: vec![Slot::empty(); 64],
            len: 0,
        }
    }

    /// 32 位 finalizer（murmur3 `fmix32`）。
    ///
    /// **这一步不是可选项**：fastrange 取的是键的**高位**，而本模块的键有两类结构性偏差：
    ///
    /// - `canon_key` 是**稠密恒等键** `语言id<<24 | code 下标`（下标连续），高位几乎恒定；
    /// - `lang_key` 把 4 bit 语言 id 放在最高位，等于把键空间切成 10 段。
    ///
    /// 直接 fastrange 会让 78,323 个 canonical 键只落到 **13 个**不同起始槽，`by_lang`
    /// 的 home 也只落在表的前 62% —— 线性探测随即退化成 O(n²)，全量热载从秒级变成小时级
    /// （实测挂死 3 小时 18 分）。先混合再映射才是「均匀映射到 [0, cap)」。
    #[inline]
    fn mix32(mut h: u32) -> u32 {
        h ^= h >> 16;
        h = h.wrapping_mul(0x85EB_CA6B);
        h ^= h >> 13;
        h = h.wrapping_mul(0xC2B2_AE35);
        h ^= h >> 16;
        h
    }

    #[inline]
    fn home(key: u32, cap: usize) -> usize {
        // Lemire fastrange：把混合后的 32 位键均匀映射到 [0, cap)
        ((Self::mix32(key) as u64 * cap as u64) >> 32) as usize
    }

    #[inline]
    fn next(i: usize, cap: usize) -> usize {
        if i + 1 == cap {
            0
        } else {
            i + 1
        }
    }

    /// 查别名：键相同**且**文本相同才算命中（32 位键碰撞时继续探测）
    fn find(&self, key: u32, probe: &[u8], arena: &Arena) -> Option<usize> {
        let cap = self.slots.len();
        let mut i = Self::home(key, cap);
        loop {
            let slot = self.slots[i];
            if slot.is_empty() {
                return None;
            }
            if slot.key == key && arena.matches(slot.off, probe) {
                return Some(i);
            }
            i = Self::next(i, cap);
        }
    }

    /// 查唯一键（canonical：键本身即身份，无需文本比对）
    fn find_key(&self, key: u32) -> Option<usize> {
        let cap = self.slots.len();
        let mut i = Self::home(key, cap);
        loop {
            let slot = self.slots[i];
            if slot.is_empty() {
                return None;
            }
            if slot.key == key {
                return Some(i);
            }
            i = Self::next(i, cap);
        }
    }

    /// 插入新条目（调用方保证 `find` 已确认不存在同键同文本的槽位）
    fn insert(&mut self, key: u32, off: u32, code: u32) {
        if (self.len + 1) * 4 > self.slots.len() * 3 {
            self.grow();
        }
        let cap = self.slots.len();
        let mut i = Self::home(key, cap);
        loop {
            if self.slots[i].is_empty() {
                self.slots[i] = Slot { key, off, code };
                self.len += 1;
                return;
            }
            i = Self::next(i, cap);
        }
    }

    /// 构建期扩容：×1.25（几何增长，保证摊还 O(1)）。
    ///
    /// **不要改成「扩到 `len*4/3`」**：那样只留 8 个空槽余量，下一次插入立刻又触发扩容，
    /// 退化成「每插 2~3 条就整表重哈希」= O(n²)（实测挂死 3 分半仍未出结果）。
    /// 想省掉「末次扩容欠填」的浪费，走 [`SlotTable::compact`]，在 `finish` 时一次性压实。
    fn grow(&mut self) {
        let cap = self.slots.len();
        self.rehash_to(((cap * 5) / 4).max(64));
    }

    /// 把槽数组重建为恰好容纳 `len` 条、负载因子 0.75 的容量。
    ///
    /// 几何扩容在最后一步必然欠填（实测 global 448,932 条占 742,515 槽，负载仅 0.605，
    /// 三张表合计多占 3.7 MiB）。构建结束后已知准确条数，此时一次性压到 0.75 才是正解：
    /// 一次 O(n) 重哈希，构建期行为不变（风险隔离在 `finish` 这一个出口）。
    fn compact(&mut self) {
        let target = ((self.len * 4) / 3 + 8).max(64);
        if target < self.slots.len() {
            self.rehash_to(target);
        }
    }

    /// 重建为 `new_cap` 个槽并重新放置全部条目
    fn rehash_to(&mut self, new_cap: usize) {
        #[cfg(test)]
        REHASH_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut slots = vec![Slot::empty(); new_cap];
        for slot in &self.slots {
            if slot.is_empty() {
                continue;
            }
            let mut i = Self::home(slot.key, new_cap);
            while !slots[i].is_empty() {
                i = Self::next(i, new_cap);
            }
            slots[i] = *slot;
        }
        self.slots = slots;
    }

    #[inline]
    fn code_at(&self, i: usize) -> u32 {
        self.slots[i].code
    }

    #[inline]
    fn off_at(&self, i: usize) -> u32 {
        self.slots[i].off
    }

    #[inline]
    fn set_code(&mut self, i: usize, code: u32) {
        self.slots[i].code = code;
    }

    #[inline]
    fn set_off(&mut self, i: usize, off: u32) {
        self.slots[i].off = off;
    }

    fn bytes(&self) -> usize {
        self.slots.len() * std::mem::size_of::<Slot>()
    }
}

// ───────────────────────────── 构建器 ─────────────────────────────

/// 热载构建器：`push_lang` 反复累积，`finish` 一次性产出不可变索引。
///
/// 构建期**直接往紧凑表里插**，不设 `HashMap<String, _>` 暂存：仲裁规则需要「同 lemma 已
/// 入表的 code」，这在开放寻址表里查一次即可。用 String 键暂存会让热载峰值变成
/// 「构建期 70 MB + 已装索引 30 MB」，在小内存机器上比优化前更糟。
///
/// 仲裁规则与改造前 `load_aliases_for_lang` 逐字一致；等价性由
/// `crate::alias_equivalence` 对 386,698 条真实 lemma 全量对拍保证（差异必须 0）。
pub struct TagAliasBuilder {
    arena: Arena,
    global: SlotTable,
    by_lang: SlotTable,
    canonical: SlotTable,
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
            arena: Arena::new(),
            global: SlotTable::new(),
            by_lang: SlotTable::new(),
            canonical: SlotTable::new(),
            rows: 0,
        }
    }

    /// 灌入一张语言分表。仲裁规则逐字复刻 `tag_identity::load_aliases_for_lang`：
    ///
    /// - 语言分表：`is_canonical && 优先级更高` 才覆盖，否则先入为主；
    /// - 全局表：**仅当该语言分表发生插入时**才参与，同样受 `is_canonical` 与优先级约束；
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
            let raw = lemma.as_ref();
            let norm = normalize_lemma(raw);
            if norm.is_empty() {
                continue;
            }
            let code_ref = code.as_ref();
            // 门禁：受控别名表严禁载入 _ext.* 非受控扩展代码
            if !is_controlled_code(code_ref) {
                continue;
            }
            let code_idx = intern_code(code_ref);

            // ── 语言分表仲裁 ──
            let lkey = lang_key(lid, &norm);
            let existing = self.by_lang.find(lkey, norm.as_bytes(), &self.arena);
            let should_insert = match existing {
                Some(si) => {
                    is_canonical && priority_of_idx(self.by_lang.code_at(si)) < priority_of_idx(code_idx)
                }
                None => true,
            };

            if should_insert {
                // 覆盖时复用原文本偏移；新插入才追加 Arena（文本已按 (语言, lemma) 去重）
                let off = match existing {
                    Some(si) => self.by_lang.off_at(si),
                    None => self.arena.push(&norm),
                };
                match existing {
                    Some(si) => self.by_lang.set_code(si, code_idx),
                    None => self.by_lang.insert(lkey, off, code_idx),
                }

                // ── 全局表独立仲裁（同名 lemma 跨语言分表可能映射不同 code）──
                let gkey = global_key(&norm);
                match self.global.find(gkey, norm.as_bytes(), &self.arena) {
                    Some(si) => {
                        let current = self.global.code_at(si);
                        if current != code_idx
                            && is_canonical
                            && priority_of_idx(current) < priority_of_idx(code_idx)
                        {
                            self.global.set_code(si, code_idx);
                        }
                    }
                    None => self.global.insert(gkey, off, code_idx),
                }
            }

            // ── 规范展示名：按 **code** 在**具体语言**内登记，与 lemma 映射是否插入无关 ──
            let ckey = canon_key(lid, code_idx);
            match self.canonical.find_key(ckey) {
                Some(si) => {
                    // 仅在 is_canonical 时覆盖；同词形不重复追加 Arena
                    if is_canonical && !self.arena.matches(self.canonical.off_at(si), raw.as_bytes())
                    {
                        let off = self.arena.push(raw);
                        self.canonical.set_off(si, off);
                    }
                }
                None => {
                    // 生产侧存的是原始 lemma（未 normalize 的词形），保持同源
                    let off = self.arena.push(raw);
                    self.canonical.insert(ckey, off, 0);
                }
            }
        }
    }

    /// 已消费的原始行数（含被过滤的行），用于与热载日志对账
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// 产出不可变索引并释放构建期容器（消费式；`seal_alias_builder` 走这条路径）。
    ///
    /// 消费式 `finish` 让生产热载路径**零拷贝**：10 张分表灌完后直接把表移交给 `Arc`，
    /// 不会出现「构建器 + 索引」两份并存的峰值。
    pub fn finish(mut self) -> TagAliasIndex {
        self.arena.buf.shrink_to_fit();
        self.global.compact();
        self.by_lang.compact();
        self.canonical.compact();
        TagAliasIndex {
            arena: self.arena,
            global: self.global,
            by_lang: self.by_lang,
            canonical: self.canonical,
        }
    }

    /// 产出不可变索引但**保留** builder，供后续增量继续叠加。
    ///
    /// 为什么必须保留：热载是**追加式**的（同一进程可能分多批 `push_lang`，批次之间还会有
    /// 读取）。若提交时取走 builder，下一批增量会从空 builder 重建索引并整体覆盖安装槽，
    /// 把先前批次灌入的别名全部丢掉 —— 实测症状是 `Connected` 退化成 `None`（`connect`
    /// 来自更早一批）。正确性优先于构建期内存，稳态紧凑由显式 `seal_alias_builder` 收口。
    pub fn snapshot(&self) -> TagAliasIndex {
        let mut global = self.global.clone();
        let mut by_lang = self.by_lang.clone();
        let mut canonical = self.canonical.clone();
        global.compact();
        by_lang.compact();
        canonical.compact();
        TagAliasIndex {
            arena: Arena {
                buf: self.arena.buf.clone(),
            },
            global,
            by_lang,
            canonical,
        }
    }
}

impl Clone for SlotTable {
    fn clone(&self) -> Self {
        Self {
            slots: self.slots.clone(),
            len: self.len,
        }
    }
}

// ───────────────────────────── 不可变索引 ─────────────────────────────

/// 别名热载的稳态表示。只读、可 `Send + Sync`、可整体 `Arc` 替换。
pub struct TagAliasIndex {
    arena: Arena,
    global: SlotTable,
    by_lang: SlotTable,
    canonical: SlotTable,
}

impl std::fmt::Debug for TagAliasIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TagAliasIndex")
            .field("arena_bytes", &self.arena.buf.len())
            .field("global_len", &self.global.len)
            .field("by_lang_len", &self.by_lang.len)
            .field("canonical_len", &self.canonical.len)
            .finish()
    }
}

impl TagAliasIndex {
    /// 全局语义直查（等价 `DYNAMIC_ALIASES`）
    pub fn global_code(&self, lemma: &str) -> Option<&'static str> {
        let si = self
            .global
            .find(global_key(lemma), lemma.as_bytes(), &self.arena)?;
        code_text(self.global.code_at(si))
    }

    /// 按具体语言直查（等价 `DYNAMIC_ALIASES_BY_LANG`）
    pub fn lang_code(&self, lang: &str, lemma: &str) -> Option<&'static str> {
        let key = lang_key(language_id(lang), lemma);
        let si = self.by_lang.find(key, lemma.as_bytes(), &self.arena)?;
        code_text(self.by_lang.code_at(si))
    }

    /// 按具体语言取 code 的规范展示名（等价 `DYNAMIC_CANONICAL_BY_LANG`）
    pub fn canonical_name(&self, lang: &str, code: &str) -> Option<&str> {
        let code_idx = code_id(code)?;
        let si = self.canonical.find_key(canon_key(language_id(lang), code_idx))?;
        self.arena.get(self.canonical.off_at(si))
    }

    /// Arena 常驻字节（S5 断言「多次重载不递增」的口径）
    pub fn arena_bytes(&self) -> usize {
        self.arena.buf.len()
    }

    /// 稳态常驻字节（S5 口径）：Arena 文本 + 三张索引的槽数组。
    ///
    /// **不含**进程级 `code_intern`（跨重载复用、属一次性常驻，见决策 1.2），也**不含**
    /// 进程 RSS —— RSS 含 33 MB pack 镜像与 SQLite 页缓存，会把结论污染成噪声。
    pub fn resident_bytes(&self) -> usize {
        let (a, g, b, c) = self.component_bytes();
        a + g + b + c
    }

    /// 各组成部分字节数 `(arena, global, by_lang, canonical)`，用于实测报告拆账
    pub fn component_bytes(&self) -> (usize, usize, usize, usize) {
        (
            self.arena.bytes(),
            self.global.bytes(),
            self.by_lang.bytes(),
            self.canonical.bytes(),
        )
    }

    /// 三张索引的槽容量 `(global, by_lang, canonical)`，用于核算负载因子
    pub fn slot_capacities(&self) -> (usize, usize, usize) {
        (
            self.global.slots.len(),
            self.by_lang.slots.len(),
            self.canonical.slots.len(),
        )
    }

    /// 单槽字节数（`Slot` 结构体尺寸），用于核算表示层放大倍数
    pub fn slot_size() -> usize {
        std::mem::size_of::<Slot>()
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
        let id_before = code_id("omw.12345678.n").expect("首次构建必须 intern 该 code");
        let text_before = code_text(id_before).expect("intern 下标必须可取回文本");

        let mut b2 = TagAliasBuilder::new();
        b2.push_lang("en", entries(&[("zzz-probe-word", "omw.12345678.n", true)]));
        let idx2 = b2.finish();

        // 断言的是「同一 code 的下标与文本指针不漂移」，**不是**全局条数不变 ——
        // intern 是进程级静态，cargo 并行跑同模块其它用例时条数会被别人推高，
        // 用条数断言会假红（本轮实测踩过：期望 6 实得 8，其实是并行用例 intern 了新 code）。
        let id_after = code_id("omw.12345678.n").expect("重载后该 code 必须仍在 intern 表内");
        assert_eq!(id_after, id_before, "重载重新 intern 了同一 code（下标漂移）");
        let text_after = code_text(id_after).expect("intern 下标必须可取回文本");
        assert!(
            std::ptr::eq(text_before, text_after),
            "重载后 code 文本指针漂移 ⇒ 已发给调用方的 &'static str 可能悬垂"
        );
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

    #[test]
    fn dense_identity_keys_are_spread_by_fastrange() {
        // 反向可证伪：不加 mix32 时，2000 个连续 canonical 键的 home 全部落在 0（只有 1 个起始槽），
        // 线性探测退化成 O(n²)（实测挂死 3 小时 18 分）。这里直接量分布，不靠计时。
        //
        // 容量取 65536 是为了让「哈希自身碰撞」可忽略：2000 条落 65536 槽，期望不同起始位置
        // ≈ 65536×(1−e^(−2000/65536)) ≈ 1970，故 1900 的门槛只筛分布退化，不筛正常碰撞。
        let cap = 65_536;
        let mut homes = std::collections::HashSet::new();
        for code_idx in 0..2000u32 {
            homes.insert(SlotTable::home(canon_key(0, code_idx), cap));
        }
        assert!(
            homes.len() > 1900,
            "canonical 稠密键在 {} 个槽里只散开 {} 个起始位置 ⇒ fastrange 未做混合，\
             热载会退化成 O(n²)",
            cap,
            homes.len()
        );

        // by_lang 的 4 bit 语言 id 在最高位，同样必须被打散（否则 home 只落在表的前段）
        let mut lang_homes = std::collections::HashSet::new();
        for (lid, lang) in LANGS_TEST.iter().enumerate() {
            for i in 0..200u32 {
                lang_homes.insert(SlotTable::home(
                    lang_key(lid as u8, &format!("{lang}-word-{i}")),
                    cap,
                ));
            }
        }
        assert!(
            lang_homes.len() > 1900,
            "按语言键只散开 {} 个起始位置 ⇒ 语言 id 占高位导致分段聚集",
            lang_homes.len()
        );
    }

    const LANGS_TEST: [&str; 10] = ["ar", "de", "en", "es", "fr", "ja", "ko", "pt", "ru", "zh"];

    #[test]
    fn rehash_count_is_logarithmic_not_quadratic() {
        // 反向可证伪：把 grow 改成「扩到 len*4/3」时只留 8 个空槽余量，下一次插入立刻再扩，
        // 2 万条会触发 ~24,000 次整表重哈希（生产上表现为挂死 3 分半不出结果）。
        //
        // 门槛 500 而不是精确值：计数器是**进程级**静态，cargo 并行跑同模块其它用例时也会累加。
        // 正常几何增长本用例自身约 87 次（3 张表 × ~28 次 + 3 次压实），退化版本是 24,000+，
        // 两者相差 48 倍以上，门槛留 5 倍余量仍能可靠判红。
        let before = rehash_count();
        let items: Vec<(String, String, bool)> = (0..20_000)
            .map(|i| (format!("w{i}"), format!("omw.{i:08}.n"), true))
            .collect();
        let mut b = TagAliasBuilder::new();
        b.push_lang("en", items.iter().map(|(l, c, k)| (l.as_str(), c.as_str(), *k)));
        let idx = b.finish();
        let rehashes = rehash_count() - before;
        assert_eq!(idx.counts().0, 20_000);
        assert!(
            rehashes <= 500,
            "2 万条触发 {rehashes} 次重哈希 ⇒ 扩容策略退化成线性（正常约 87 次）"
        );
    }

    #[test]
    fn finish_compacts_to_load_factor() {
        // 几何扩容末次必然欠填；finish 必须把负载压回 ≤ 0.75
        let items: Vec<(String, String, bool)> = (0..5_000)
            .map(|i| (format!("w{i}"), format!("omw.{i:08}.n"), true))
            .collect();
        let mut b = TagAliasBuilder::new();
        b.push_lang("en", items.iter().map(|(l, c, k)| (l.as_str(), c.as_str(), *k)));
        let idx = b.finish();
        let (g, bl, c) = idx.slot_capacities();
        assert!(5_000 * 4 <= g * 3, "global 负载因子 {} > 0.75", 5_000.0 / g as f64);
        assert!(5_000 * 4 <= bl * 3, "by_lang 负载因子 {}", 5_000.0 / bl as f64);
        assert!(5_000 * 4 <= c * 3, "canonical 负载因子 {}", 5_000.0 / c as f64);
        // 压实后仍须全部可查（压实不得丢条目）
        for i in [0usize, 1, 2_499, 4_998, 4_999] {
            assert_eq!(
                idx.global_code(&format!("w{i}")).map(str::to_string),
                Some(format!("omw.{i:08}.n"))
            );
        }
    }

    #[test]
    fn hash_key_truncation_still_compares_text() {
        // 32 位键碰撞时必须以 Arena 文本为准：两个不同 lemma 无论是否撞键都要各归各位
        let mut b = TagAliasBuilder::new();
        b.push_lang(
            "en",
            entries(&[
                ("alpha", "omw.00000003.n", true),
                ("beta", "omw.00000004.n", true),
                ("gamma", "builtin.gamma", true),
            ]),
        );
        let idx = b.finish();
        assert_eq!(idx.global_code("alpha"), Some("omw.00000003.n"));
        assert_eq!(idx.global_code("beta"), Some("omw.00000004.n"));
        assert_eq!(idx.global_code("gamma"), Some("builtin.gamma"));
        // 前缀相同但更长的词不得误命中（NUL 终止符判定）
        assert_eq!(idx.global_code("alph"), None);
        assert_eq!(idx.global_code("alphax"), None);
    }

    #[test]
    fn table_grows_without_losing_entries() {
        // 覆盖 1.25 倍扩容路径：插入量远超初始 64 槽
        let mut b = TagAliasBuilder::new();
        let items: Vec<(String, String, bool)> = (0..2000)
            .map(|i| (format!("word{i}"), format!("omw.{i:08}.n"), true))
            .collect();
        b.push_lang("en", items.iter().map(|(l, c, k)| (l.as_str(), c.as_str(), *k)));
        let idx = b.finish();
        assert_eq!(idx.counts().0, 2000);
        for i in [0usize, 1, 999, 1998, 1999] {
            let expected = format!("omw.{i:08}.n");
            assert_eq!(
                idx.global_code(&format!("word{i}")).map(str::to_string),
                Some(expected)
            );
        }
    }
}
