//! 标签准入闸门 · G1 词形层（取码前，零本体依赖）
//!
//! Spec: `docs/specs/tag-admissibility-filter-spec.md` §3.1 / §5.1 / §6.9
//! ADR:  `docs/adr/0052-tag-admissibility-gate-and-controlled-pos-enum.md`
//!
//! 设计要点：
//! - **单一数据源**：`taxonomy/tag-admissibility.json`（与 `vocab_layers.json`、`domain_tag_matrix.json` 同构），
//!   资产缺失时回退**内置同源默认**；两侧一致性由单测 `builtin_default_matches_asset` 守护，
//!   **严禁在别处再写一份硬编码副本**。
//! - **裁决空间封闭三值**：`Allow` / `Reject { rule, reason }` / `Exempt`，**不引入「降权」**。
//! - **拒绝可追溯**：拒绝原因落 `tracing` + 按规则**聚合计数**（不建审计表，spec §7 D8）。
//! - **零本体依赖**：不查语义包、不取码，故可放在铸造入口最省算力的位置。
//! - **本模块不做语言分支**：同一套字符类规则对 zh/en 同等生效（P3 来源对称）。
//!
//! 覆盖规则：R-G1-01（非白名单文种）、R-G1-03（零文种字符子类：纯数字/纯标点/纯符号，
//! 2026-10-02 用户裁决收敛进本模块单点拦截）、R-G1-10（不可入列码位，一票否决）、
//! R-G1-11（单串多脚本混杂）。
//!
//! **严格性声明（与 TS 参考实现对齐，勿擅改）**：R-G1-01 是**全串锚定**判定——串内任一字符既非
//! 白名单文种、又非数字/空白/中性字符（`-` `_` `·`）即拒。因此 `C++`、`C#`、`川菜（辣）`
//! 这类「合法文种 + 非中性标点」的组合**会被拒**。这与 `isWhitelisted10LanguageText`
//! 的全串锚定字符类行为一致（其字符类同样不含 `+` `#` `（`），是有意为之而非疏漏。
//! 若将来要放宽，必须**两侧同时**改（Rust 资源 + TS 正则），否则 L1/L2 判定漂移。
//!
//! 本模块**只**覆盖 R-G1-01 / R-G1-03（零文种字符子类）/ R-G1-10 / R-G1-11；R-G1-02 与
//! R-G1-04…R-G1-09（超长、hex 形态、停用词、句型框架、CJK 单字、括号截断、保留字）及
//! R-G1-03 的其余范畴承担者（hex/流水号等形态判定由各自既有实现负责）**不在本模块范围**。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

/// R-G1-01 非白名单文种
pub const R_G1_01: &str = "R-G1-01";
/// R-G1-03 纯数字 / 纯标点 / 纯符号（零文种字符子类，2026-10-02 用户裁决收敛进本模块单点拦截）
pub const R_G1_03: &str = "R-G1-03";
/// R-G1-10 含不可入列码位（一票否决）
pub const R_G1_10: &str = "R-G1-10";
/// R-G1-11 单串多脚本混杂
pub const R_G1_11: &str = "R-G1-11";

/// 拒绝哨兵 code：**空串**。
///
/// 约定：任何铸造入口在 G1 拒绝时必须返回该哨兵，而**不是**一个 `_ext.*` code。
/// 消费端（标签链汇聚、落库前）看到空 code 即视为「未通过准入」，**必须丢弃**，
/// 不得写入 `file_tags` / 别名表 / `materialized_paths` 标签树。
///
/// 选择空串而非 `Option` 的理由：`derive_ext_tag_code` 是横跨 omni-core / omni-vision /
/// omni-text 三处的低层工具，改签名会波及 20+ 调用点；而空串在合法 code 空间内**不可能出现**
/// （`_ext.*` / `builtin.*` / `omw.*` / `hownet.*` 皆非空），故作为哨兵无歧义。
pub const REJECTED_CODE: &str = "";

/// 判定某 code 是否为 G1 拒绝哨兵
#[inline]
pub fn is_rejected_code(code: &str) -> bool {
    code.is_empty()
}

/// G1 裁决（封闭三值）。
///
/// `Exempt` 不在本模块产生（来源信息属上层），保留该态是为了让上层的裁决空间
/// 保持**封闭三值**而不是四态——G1 只管「词形该不该存在」，不管「谁写的」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// 放行
    Allow,
    /// 拒绝：不铸造扩展概念、不建立标注关系
    Reject {
        rule: &'static str,
        reason: &'static str,
    },
    /// 豁免（`user` / `fact` 来源，由上层判定）
    Exempt { reason: &'static str },
}

impl Verdict {
    /// 是否拒绝
    pub fn is_reject(&self) -> bool {
        matches!(self, Verdict::Reject { .. })
    }

    /// 是否可放行（`Allow` 与 `Exempt` 皆可写入）
    pub fn is_admitted(&self) -> bool {
        !self.is_reject()
    }

    /// 命中规则 ID（仅 `Reject` 有值）
    pub fn rule(&self) -> Option<&'static str> {
        match self {
            Verdict::Reject { rule, .. } => Some(rule),
            _ => None,
        }
    }

    /// 人类可读原因（仅 `Reject` / `Exempt` 有值）
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            Verdict::Reject { reason, .. } => Some(reason),
            Verdict::Exempt { reason } => Some(reason),
            Verdict::Allow => None,
        }
    }
}

/// G1 白名单文种（与 TS 侧 `isWhitelisted10LanguageText` 的脚本集合对齐）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Script {
    Han,
    Hiragana,
    Katakana,
    Hangul,
    Latin,
    Cyrillic,
    Arabic,
}

impl Script {
    fn from_name(name: &str) -> Option<Script> {
        match name {
            "Han" => Some(Script::Han),
            "Hiragana" => Some(Script::Hiragana),
            "Katakana" => Some(Script::Katakana),
            "Hangul" => Some(Script::Hangul),
            "Latin" => Some(Script::Latin),
            "Cyrillic" => Some(Script::Cyrillic),
            "Arabic" => Some(Script::Arabic),
            _ => None,
        }
    }

    /// 码位 → 文种。
    ///
    /// **与 TS 参考实现的已知差异（有意登记，勿静默扩散）**：TS 侧用 Unicode Script 属性类
    /// (`\p{Script=Han}` 等)，覆盖全部已指派码位；本函数按**显式区段枚举**，未列出的码位一律判
    /// 「非白名单文种」。已按审查（S1 #705 第 1 轮 P2-1）补齐主干扩展区段（CJK Ext B–I、
    /// 拉丁 Ext C/D/E、谚文 Jamo 扩展、西里尔扩展）；若 TS 侧将来再扩白名单，两侧必须同步。
    pub fn of(c: char) -> Option<Script> {
        let u = c as u32;
        match u {
            // Han：基本区 / 扩展 A / 兼容表意 / **扩展 B–I / 兼容补充**
            // （Ext B 起含 U+20000+，真实汉字如 `𠮷`(U+20BB7) 在此——缺了会把合法人名用字判成乱码）
            0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2A6DF
            | 0x2A700..=0x2B739 | 0x2B740..=0x2B81F | 0x2B820..=0x2CEAF | 0x2CEB0..=0x2EBEF
            | 0x2EBF0..=0x2EE5F | 0x2F800..=0x2FA1F | 0x30000..=0x3134A | 0x31350..=0x323AF => {
                Some(Script::Han)
            }
            // 日文假名
            0x3040..=0x309F => Some(Script::Hiragana),
            0x30A0..=0x30FF | 0x31F0..=0x31FF => Some(Script::Katakana),
            // 谚文（含 Jamo 扩展 A/B）
            0x1100..=0x11FF | 0x3130..=0x318F | 0xA960..=0xA97C | 0xAC00..=0xD7AF
            | 0xD7B0..=0xD7FF => Some(Script::Hangul),
            // 西里尔（含 Cyrillic Extended-A）
            0x0400..=0x04FF | 0x0500..=0x052F | 0x2DE0..=0x2DFF | 0xA640..=0xA69F => {
                Some(Script::Cyrillic)
            }
            // 阿拉伯
            0x0600..=0x06FF | 0x0750..=0x077F | 0x08A0..=0x08FF | 0xFB50..=0xFDFF
            | 0xFE70..=0xFEFF => Some(Script::Arabic),
            // 拉丁：ASCII 字母 + 拉丁补充 / 扩展 A / 扩展 B / **扩展 C / D / E**
            _ if c.is_ascii_alphabetic() => Some(Script::Latin),
            0x00C0..=0x024F | 0x1E00..=0x1EFF | 0x2C60..=0x2C7F | 0xA720..=0xA7FF
            | 0xAB30..=0xAB6F => Some(Script::Latin),
            _ => None,
        }
    }
}

/// 不可入列码位类目（R-G1-10）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForbiddenClass {
    ReplacementChar,
    ControlC0,
    ControlC1,
    PrivateUse,
    /// Rust `&str` 由类型保证不含孤立代理项，故该态在 Rust 侧**不可观测**；
    /// 保留它是为了让资源与 TS/JS 侧（JS 字符串可含孤立代理项）保持同一份类目表。
    UnpairedSurrogate,
}

impl ForbiddenClass {
    fn describe(self) -> &'static str {
        match self {
            ForbiddenClass::ReplacementChar => "U+FFFD 替换字符（乱码签名）",
            ForbiddenClass::ControlC0 => "C0 控制字符（含 NUL）",
            ForbiddenClass::ControlC1 => "C1 控制字符",
            ForbiddenClass::PrivateUse => "私用区 PUA（图标字体残留）",
            ForbiddenClass::UnpairedSurrogate => "孤立代理项",
        }
    }

    fn of(c: char) -> Option<ForbiddenClass> {
        let u = c as u32;
        match u {
            0xFFFD => Some(ForbiddenClass::ReplacementChar),
            0x0000..=0x001F | 0x007F => Some(ForbiddenClass::ControlC0),
            0x0080..=0x009F => Some(ForbiddenClass::ControlC1),
            0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD => {
                Some(ForbiddenClass::PrivateUse)
            }
            // 代理项不是合法 `char`，此分支在 Rust 侧恒不可达；显式写出以对齐资源类目。
            0xD800..=0xDFFF => Some(ForbiddenClass::UnpairedSurrogate),
            _ => None,
        }
    }
}

/// G1 规则配置（唯一数据源的解析结果）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagAdmissibility {
    allowed_scripts: Vec<Script>,
    /// 中性字符：既不算文种也不算违规（`-` / `_` / `·`）
    neutral_chars: Vec<char>,
    /// 互斥文种对：同串内同时出现且**无分隔** → 拒（R-G1-11）
    exclusive_pairs: Vec<(Script, Script)>,
    /// 配置来源是否为内置默认（false = 命中外部资产）。供诊断/单测使用。
    builtin_default: bool,
}

impl TagAdmissibility {
    /// 是否正在使用内置默认配置（外部资产缺失）
    pub fn is_builtin_default(&self) -> bool {
        self.builtin_default
    }

    /// 白名单文种（只读）
    pub fn allowed_scripts(&self) -> &[Script] {
        &self.allowed_scripts
    }

    /// 互斥文种对（只读）
    pub fn exclusive_pairs(&self) -> &[(Script, Script)] {
        &self.exclusive_pairs
    }

    fn default_config() -> TagAdmissibility {
        use Script::*;
        TagAdmissibility {
            allowed_scripts: vec![Han, Hiragana, Katakana, Hangul, Latin, Cyrillic, Arabic],
            neutral_chars: vec!['-', '_', '·'],
            // 刻意**不**含 Latin↔Cyrillic / Latin↔Arabic：品牌与外来词常合法混写（如 `МВидео`），
            // 误杀代价高于收益。互斥对只取现实中不会合法共现的组合。
            exclusive_pairs: vec![
                (Cyrillic, Arabic),
                (Han, Cyrillic),
                (Han, Arabic),
                (Hiragana, Cyrillic),
                (Hiragana, Arabic),
                (Katakana, Cyrillic),
                (Katakana, Arabic),
                (Hangul, Cyrillic),
                (Hangul, Arabic),
            ],
            builtin_default: true,
        }
    }

    fn from_json(v: &serde_json::Value, fallback: &TagAdmissibility) -> TagAdmissibility {
        let g1 = v.get("g1");
        let pick_scripts = |key: &str, dflt: &[Script]| -> Vec<Script> {
            let parsed: Vec<Script> = g1
                .and_then(|g| g.get(key))
                .and_then(|x| x.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|s| s.as_str())
                        .filter_map(Script::from_name)
                        .collect()
                })
                .unwrap_or_default();
            if parsed.is_empty() {
                dflt.to_vec()
            } else {
                parsed
            }
        };
        let allowed_scripts = pick_scripts("allowed_scripts", &fallback.allowed_scripts);

        let neutral_chars: Vec<char> = g1
            .and_then(|g| g.get("neutral_chars"))
            .and_then(|x| x.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| s.as_str())
                    .filter_map(|s| s.chars().next())
                    .collect()
            })
            .filter(|v: &Vec<char>| !v.is_empty())
            .unwrap_or_else(|| fallback.neutral_chars.clone());

        let exclusive_pairs: Vec<(Script, Script)> = g1
            .and_then(|g| g.get("exclusive_script_pairs"))
            .and_then(|x| x.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|pair| pair.as_array())
                    .filter(|p| p.len() >= 2)
                    .filter_map(|p| {
                        let a = p[0].as_str().and_then(Script::from_name)?;
                        let b = p[1].as_str().and_then(Script::from_name)?;
                        Some((a, b))
                    })
                    .collect()
            })
            .unwrap_or_else(|| fallback.exclusive_pairs.clone());

        TagAdmissibility {
            allowed_scripts,
            neutral_chars,
            exclusive_pairs,
            builtin_default: false,
        }
    }
}

/// 规则资源所在目录候选。
///
/// **镜像自 `omni-vision::OmniVisionEngine::resolve_taxonomy_dir`，但已登记 3 处差异**
/// （S1 #705 第 1 轮审查 P2-3；完整收敛——下沉为 omni-core 单一函数、omni-vision 改为调用——
/// 属超纲建议，另行开票）：
/// ① 后者另有 `%APPDATA%/firefly-ai-folder/taxonomy` 与 `%USERPROFILE%/.firefly/taxonomy` 两个
///    宿主侧候选，本函数**有意不收**（引擎不应读宿主用户数据，见
///    `docs/features/ai/engine-resource-lookup-scope.md`，尽管该文档只管辖引擎二进制）；
/// ② 命中判据不同：本函数要求目录含 `tag-admissibility.json`，后者要求含 `entity_ontology.json`——
///    若某目录只有其一，两者可能选中**不同目录**（规则与词表漂移的现实风险）；
/// ③ CWD 向上 walk 已补齐 `build/extraResources/taxonomy`（原镜像源漏了该项）。
/// 重复的是**路径查找**而非**规则数据**——规则数据仍只有 `tag-admissibility.json` 一份。
fn taxonomy_dir_candidates() -> Vec<PathBuf> {
    let mut candidates = vec![
        PathBuf::from("apps/desktop/build/extraResources/taxonomy"),
        PathBuf::from("apps/desktop/pro/build/extraResources/taxonomy"),
        PathBuf::from("build/extraResources/taxonomy"),
        PathBuf::from("../desktop/build/extraResources/taxonomy"),
        PathBuf::from("../../desktop/build/extraResources/taxonomy"),
        PathBuf::from("../../../desktop/build/extraResources/taxonomy"),
        PathBuf::from("../../../apps/desktop/build/extraResources/taxonomy"),
        PathBuf::from("taxonomy"),
    ];
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(mut current) = exe_path.parent() {
            for _ in 0..8 {
                candidates.push(current.join("taxonomy"));
                candidates.push(current.join("build/extraResources/taxonomy"));
                candidates.push(current.join("apps/desktop/build/extraResources/taxonomy"));
                match current.parent() {
                    Some(p) => current = p,
                    None => break,
                }
            }
        }
    }
    if let Ok(mut current) = std::env::current_dir() {
        for _ in 0..8 {
            candidates.push(current.join("taxonomy"));
            candidates.push(current.join("build/extraResources/taxonomy"));
            candidates.push(current.join("apps/desktop/build/extraResources/taxonomy"));
            match current.parent() {
                Some(p) => current = p.to_path_buf(),
                None => break,
            }
        }
    }
    candidates
}

/// 加载 G1 规则配置：优先 `taxonomy/tag-admissibility.json`，缺失/损坏时回退内置同源默认。
pub fn config() -> &'static TagAdmissibility {
    static CFG: OnceLock<TagAdmissibility> = OnceLock::new();
    CFG.get_or_init(|| {
        let fallback = TagAdmissibility::default_config();
        for dir in taxonomy_dir_candidates() {
            let path = dir.join("tag-admissibility.json");
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                tracing::warn!(path = %path.display(), "tag-admissibility.json 解析失败，回退内置默认");
                continue;
            };
            return TagAdmissibility::from_json(&value, &fallback);
        }
        fallback
    })
}

// ============ 聚合计数（spec §7 D8：拒绝原因落日志 + 聚合计数，不建审计表） ============

static REJECT_R_G1_01: AtomicU64 = AtomicU64::new(0);
static REJECT_R_G1_03: AtomicU64 = AtomicU64::new(0);
static REJECT_R_G1_10: AtomicU64 = AtomicU64::new(0);
static REJECT_R_G1_11: AtomicU64 = AtomicU64::new(0);

fn counter_for(rule: &str) -> Option<&'static AtomicU64> {
    match rule {
        R_G1_01 => Some(&REJECT_R_G1_01),
        R_G1_03 => Some(&REJECT_R_G1_03),
        R_G1_10 => Some(&REJECT_R_G1_10),
        R_G1_11 => Some(&REJECT_R_G1_11),
        _ => None,
    }
}

/// 单条规则的累计拒绝次数
pub fn rejection_count(rule: &str) -> u64 {
    counter_for(rule)
        .map(|c| c.load(Ordering::Relaxed))
        .unwrap_or(0)
}

/// 全部规则的累计拒绝次数（规则 ID → 次数）
pub fn rejection_counts() -> [(&'static str, u64); 4] {
    [
        (R_G1_01, rejection_count(R_G1_01)),
        (R_G1_03, rejection_count(R_G1_03)),
        (R_G1_10, rejection_count(R_G1_10)),
        (R_G1_11, rejection_count(R_G1_11)),
    ]
}

/// 清零计数（仅供测试与诊断，不影响生产语义）
pub fn reset_rejection_counts() {
    for c in [&REJECT_R_G1_01, &REJECT_R_G1_10, &REJECT_R_G1_11] {
        c.store(0, Ordering::Relaxed);
    }
}

// ============ 判定 ============

/// 纯判定：不做日志、不计数。用于单测与需要无副作用的场景。
///
/// **修剪语义**：判定前先 `trim()`。由于 `str::trim()` 按 Unicode `White_Space` 剥离，
/// U+0085(NEL)、U+000B(VT)、U+000C(FF) 等**空白类控制字符**出现在首尾时会被剥掉而非判拒；
/// 因铸造入口的 code 与 name 同源取自修剪后的串，这不会铸造出垃圾概念。
/// 夹在中间的同名控制字符仍由 R-G1-10 拦下（真实乱码即此形态）。
///
/// 判定顺序：**R-G1-10 一票否决** → R-G1-01 文种白名单 → R-G1-03 零文种字符 → R-G1-11 多脚本混杂。
///
/// > 规范 §2 的通用顺序是「按规则 ID 升序，首个命中即裁决」，但 §3.1 F25 / §5.1 明确把
/// > R-G1-10 标为**一票否决**。若严格按 ID 升序，R-G1-01 会先命中（U+FFFD / NUL / PUA
/// > 均不在任何白名单文种内），R-G1-10 **永不触发**，其存在意义（给出精确拒绝原因）落空。
/// > 故此处让一票否决规则优先，并在资源 `notes.evaluation_order` 中显式记录该偏离。
pub fn g1_verdict(tag: &str) -> Verdict {
    let cfg = config();
    let trimmed = tag.trim();
    if trimmed.is_empty() {
        return Verdict::Reject {
            rule: R_G1_01,
            reason: "空串",
        };
    }

    // ① R-G1-10：不可入列码位，一票否决
    for ch in trimmed.chars() {
        if let Some(class) = ForbiddenClass::of(ch) {
            return Verdict::Reject {
                rule: R_G1_10,
                reason: class.describe(),
            };
        }
    }

    // ② R-G1-01：逐码位文种白名单
    let mut present: Vec<Script> = Vec::new();
    for ch in trimmed.chars() {
        if ch.is_whitespace() || ch.is_ascii_digit() || cfg.neutral_chars.contains(&ch) {
            continue;
        }
        match Script::of(ch) {
            Some(s) if cfg.allowed_scripts.contains(&s) => {
                if !present.contains(&s) {
                    present.push(s);
                }
            }
            _ => {
                return Verdict::Reject {
                    rule: R_G1_01,
                    reason: "非白名单文种",
                }
            }
        }
    }

    // ②' R-G1-03（零文种字符子类，2026-10-02 用户裁决收敛进 G1 单点拦截）：
    // R-G1-01 循环对数字与中性字符一律 continue，纯此类串（`---` / `___` / `123` / `· ·`）
    // 走完后 `present` 为空，此前会静默放行并在铸造点产出 `_ext.{hash8}` 垃圾 code。
    // spec §5.1 表格本就标注 R-G1-03 装在铸造路径，本分支使其在 Rust 侧落地。
    // 收敛语义：此处是**权威契约兜底**——上游预筛（fusion `is_noise_entity` 等候选早筛，
    // 其范畴更宽：短 ASCII 碎片 / 序列号 / CJK 单字，与 G1 不重叠）漏拦也铸不出垃圾；
    // 零额外扫描成本（复用 ② 的 present 结果）。受控反查在闸前，合法受控别名不受影响。
    if present.is_empty() {
        return Verdict::Reject {
            rule: R_G1_03,
            reason: "零文种字符（纯数字/纯标点/纯符号）",
        };
    }

    // ③ R-G1-11：单串多脚本混杂（有分隔则不判）
    if present.len() >= 2 {
        // 分隔符判定与 R-G1-01 的中性字符集**同源**（cfg.neutral_chars + 空白），
        // 避免两条规则对「什么算分隔」分叉（S1 #705 第 1 轮 P2-4）。
        // 注意规范 N24/F26 的文本只写「空格/连字符」，本实现刻意取超集（含 `_` `·`）——
        // 与 TS 参考实现的字符类 `\d\-_·\s` 对齐；该差异已在资源 notes 中登记。
        let has_separator = trimmed
            .chars()
            .any(|c| c.is_whitespace() || cfg.neutral_chars.contains(&c));
        if !has_separator {
            for (a, b) in &cfg.exclusive_pairs {
                if present.contains(a) && present.contains(b) {
                    return Verdict::Reject {
                        rule: R_G1_11,
                        reason: "单串多脚本混杂（无分隔）",
                    };
                }
            }
        }
    }

    Verdict::Allow
}

/// 带副作用（日志 + 聚合计数）的判定，供铸造路径调用。
pub fn g1_gate(tag: &str) -> Verdict {
    let verdict = g1_verdict(tag);
    if let Verdict::Reject { rule, reason } = &verdict {
        if let Some(counter) = counter_for(rule) {
            counter.fetch_add(1, Ordering::Relaxed);
        }
        tracing::debug!(
            target: "tag_admissibility",
            rule = *rule,
            reason = *reason,
            tag = tag,
            "G1 词形闸拒绝"
        );
    }
    verdict
}

/// 便捷布尔：是否被 G1 拒绝（带计数与日志）
pub fn is_g1_rejected(tag: &str) -> bool {
    g1_gate(tag).is_reject()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_replacement_char() {
        let v = g1_verdict("��Ϊ�ֻ�����");
        assert!(v.is_reject(), "U+FFFD 乱码必须被拒");
        assert_eq!(v.rule(), Some(R_G1_10));
    }

    #[test]
    fn rejects_nul_control_char() {
        let v = g1_verdict("袁浩, 李晓红编著\u{0}");
        assert!(v.is_reject(), "含 NUL 必须被拒");
        assert_eq!(v.rule(), Some(R_G1_10));
    }

    #[test]
    fn rejects_private_use_area() {
        let v = g1_verdict("Exclude\u{f023}T,");
        assert!(v.is_reject(), "私用区 PUA 必须被拒");
        assert_eq!(v.rule(), Some(R_G1_10));
    }

    #[test]
    fn rejects_c1_control_char() {
        let v = g1_verdict("abc\u{85}def");
        assert!(v.is_reject(), "C1 控制字符必须被拒");
        assert_eq!(v.rule(), Some(R_G1_10));
    }

    #[test]
    fn trims_whitespace_class_control_chars_before_scan() {
        // U+0085 (NEL) 虽落在 C1 区段，但它同时是 Unicode `White_Space`，
        // 会被 `str::trim()` 先剥掉 → 前导/尾随 NEL **不会**触发 R-G1-10。
        // 这是可接受的：`g1_verdict` 与铸造入口同源修剪，code 与 name 都取自修剪后的串，
        // 不会因此铸造出垃圾概念（`"\u{85}abc"` → 视作 `"abc"`）。
        assert!(g1_verdict("\u{85}abc").is_admitted());
        // 但夹在中间的 NEL 不会被 trim，仍由 R-G1-10 拦下（这才是真实乱码的形态）。
        assert_eq!(g1_verdict("abc\u{85}def").rule(), Some(R_G1_10));
    }

    #[test]
    fn allows_non_exclusive_script_mixes() {
        // Latin↔Cyrillic / Latin↔Arabic **刻意不在互斥对内**：品牌与外来词合法混写
        // （`МВидео`），误杀代价高于收益。此为「互斥对之外多脚本混写应放行」的否定用例
        // （S1 #705 第 1 轮审查：测试缺口）。
        assert!(g1_verdict("Ӊabc").is_admitted(), "Cyr+Latin 混写应放行");
        assert!(g1_verdict("МВидео").is_admitted(), "品牌混写应放行");
        assert!(g1_verdict("Helloمرحبا").is_admitted(), "Latin+Arabic 混写应放行");
        // 对照：互斥对（Han↔Cyrillic）无分隔仍拒
        assert!(g1_verdict("ӉӉӉ汉").is_reject());
        // 对照：互斥对**有**分隔则放行
        assert!(g1_verdict("ӉӉӉ 汉").is_admitted());
    }

    #[test]
    fn from_json_partial_corruption_falls_back_per_field() {
        // 部分字段损坏 → 该字段回退内置默认；合法字段照常采纳（逐字段降级，不整体失败）
        let dflt = TagAdmissibility::default_config();
        let cfg = serde_json::json!({
            "g1": {
                "allowed_scripts": "not-an-array",
                "neutral_chars": ["-", "_"],
                "exclusive_script_pairs": [["Han", "Cyrillic"]]
            }
        });
        let parsed = TagAdmissibility::from_json(&cfg, &dflt);
        // 损坏字段 → 回退默认（7 个白名单文种）
        assert_eq!(parsed.allowed_scripts(), dflt.allowed_scripts());
        // 合法字段 → 采纳（不含 `·`，与默认不同）
        assert_eq!(parsed.neutral_chars, vec!['-', '_']);
        // 合法字段 → 采纳
        use Script::*;
        assert_eq!(parsed.exclusive_pairs(), &[(Han, Cyrillic)]);
    }

    #[test]
    fn rejects_non_whitelisted_script() {
        // 希腊字母不在白名单语族内
        let v = g1_verdict("Ελληνικά");
        assert!(v.is_reject());
        assert_eq!(v.rule(), Some(R_G1_01));
    }

    #[test]
    fn rejects_empty() {
        assert!(g1_verdict("   ").is_reject());
    }

    #[test]
    fn rejects_mixed_exclusive_scripts() {
        // 西里尔 + 汉，无分隔 → R-G1-11
        let v = g1_verdict("ӉӉӉ汉");
        assert!(v.is_reject(), "西里尔+汉混杂必须被拒");
        assert_eq!(v.rule(), Some(R_G1_11));
    }

    #[test]
    fn allows_mixed_scripts_with_separator() {
        // 有分隔则不判 R-G1-11
        assert!(!g1_verdict("ӉӉӉ 汉").is_reject());
    }

    #[test]
    fn allows_positive_cases() {
        // 规范 §8 V12 的正例：零误杀
        for ok in [
            "手机照片",
            "macOS截图",
            "2024年度报告",
            "Документы",
            "مشاريع",
            "abc-def",
        ] {
            let v = g1_verdict(ok);
            assert!(v.is_admitted(), "正例不应被拒: {ok} → {v:?}");
        }
    }

    #[test]
    fn rejects_symbol_bearing_mixed_forms() {
        // `C++` / `C#`：符号不在白名单文种与中性字符内，按 R-G1-01 拒绝。
        // 与 TS 参考实现 `isWhitelisted10LanguageText`（全串锚定 `[Han|Kana|Hangul|Latin|Cyrillic|Arabic|\d\-_·\s]`）
        // 行为一致——`+` / `#` 同样不在其字符集内。刻意保持两侧一致，不引入「符号豁免」。
        for bad in ["C++", "C#", "§123"] {
            let v = g1_verdict(bad);
            assert!(v.is_reject(), "含非中性符号应被拒: {bad} → {v:?}");
            assert_eq!(v.rule(), Some(R_G1_01), "{bad}");
        }
    }

    #[test]
    fn rejects_zero_script_char_strings() {
        // R-G1-03 零文种字符子类（2026-10-02 用户裁决收敛进 G1 单点拦截，spec §6.9.6 项 3）：
        // 纯数字 / 纯中性标点 / 纯空白串无任何白名单文种字符，此前走完 R-G1-01 循环后
        // `present` 为空被静默放行，会在铸造点产出 `_ext.{hash8}` 垃圾 code。
        for bad in ["---", "___", "·-·", "123", "9 9", "-_-", "···"] {
            let v = g1_verdict(bad);
            assert!(v.is_reject(), "零文种字符串应被拒: {bad:?} → {v:?}");
            assert_eq!(v.rule(), Some(R_G1_03), "{bad:?}");
        }
        // 混入白名单文种字符的含数字/中性字符形态不受影响
        for ok in ["2024年度报告", "abc-def", "v2", "MP3 Player"] {
            let v = g1_verdict(ok);
            assert!(v.is_admitted(), "含文种字符的混合形态不应被拒: {ok} → {v:?}");
        }
        // 铸造入口收敛：上游预筛（fusion is_noise_entity 等）漏拦时，此处兜底也铸不出垃圾
        assert!(crate::tag_identity::normalize_tag_to_code("---").is_empty());
        assert!(crate::tag_identity::normalize_tag_to_code("123").is_empty());
        // 受控别名优先级不受影响（反查在闸前；「高质量」为静态 BUILTIN_ALIASES 映射，
        // 与 tag_identity 测试 :1469 的既有断言同源）
        assert_eq!(
            crate::tag_identity::normalize_tag_to_code("高质量"),
            "builtin.high_quality"
        );
    }

    #[test]
    fn verdict_space_is_closed_three_valued() {
        // 封闭三值：Allow / Reject / Exempt，无第四态
        let variants = [
            Verdict::Allow,
            Verdict::Reject {
                rule: R_G1_01,
                reason: "x",
            },
            Verdict::Exempt { reason: "user" },
        ];
        assert!(variants.iter().all(|v| v.is_admitted() == !v.is_reject()));
        assert!(Verdict::Exempt { reason: "user" }.is_admitted());
    }

    #[test]
    fn gate_counts_rejections_by_rule() {
        reset_rejection_counts();
        let _ = g1_gate("��Ϊ�ֻ�����");
        let _ = g1_gate("ӉӉӉ汉");
        assert_eq!(rejection_count(R_G1_10), 1);
        assert_eq!(rejection_count(R_G1_11), 1);
        assert_eq!(rejection_count(R_G1_01), 0);
        reset_rejection_counts();
    }

    #[test]
    fn rejected_code_sentinel_is_unambiguous() {
        // 哨兵契约：空串在合法 code 空间内不可能出现
        assert!(is_rejected_code(REJECTED_CODE));
        for legit in [
            "_ext.abc.0123abcd",
            "builtin.screenshot",
            "omw.00001234",
            "hownet.1234",
        ] {
            assert!(!is_rejected_code(legit), "合法 code 不应被判为哨兵: {legit}");
        }
    }

    #[test]
    fn builtin_default_matches_asset() {
        // 单一数据源守护：内置默认必须与 taxonomy/tag-admissibility.json 同源。
        // 若资产不存在（omni 子模块被单独检出）则跳过，不制造假失败。
        let asset = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../apps/desktop/build/extraResources/taxonomy/tag-admissibility.json");
        if !asset.exists() {
            eprintln!("skip: 资产不存在 {}", asset.display());
            return;
        }
        let text = std::fs::read_to_string(&asset).expect("读取 tag-admissibility.json");
        let value: serde_json::Value = serde_json::from_str(&text).expect("解析 JSON");
        let builtin = TagAdmissibility::default_config();
        let parsed = TagAdmissibility::from_json(&value, &builtin);
        assert_eq!(
            parsed.allowed_scripts, builtin.allowed_scripts,
            "allowed_scripts 与内置默认漂移"
        );
        assert_eq!(
            parsed.exclusive_pairs, builtin.exclusive_pairs,
            "exclusive_script_pairs 与内置默认漂移"
        );
        assert_eq!(
            parsed.neutral_chars, builtin.neutral_chars,
            "neutral_chars 与内置默认漂移"
        );
    }
}
