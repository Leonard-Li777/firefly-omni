//! 受控「敏感内容」子树封闭清单（维度 130 / 123 / 134 的叶子全集）
//!
//! ## 为什么需要这个模块
//!
//! 视觉零样本通道（CLIP 词表 / EmbeddingGemma 预烘焙词表）与「全年龄内容纯净闸」
//! （`omni_vision::is_nsfw_or_restricted_tag`）此前各自维护一份**手写词串黑名单**，
//! 两份清单都与创世维度事实源持续漂移，酿成真实事故：
//!
//! > 一张公司拓展训练合影被判「全年龄」(`builtin.all_ages`)，同时落库
//! > `builtin.teacher_student_erotica`（师生情色）与
//! > `builtin.diverse_sexual_orientations`（多元性取向）。
//! >
//! > 成因是双闸同时漏判：CLIP 三级黑名单把「多元性取向」误写成「多元取向」，
//! > 且 `师生情色 / 职场情色 / 制服诱惑 / 情色文学` 从未登记；
//! > 全年龄纯净闸的 `RESTRICTED_SENSITIVE` 也只覆盖 12 个色情细分叶子中的 7 个。
//! > 而 `gemma_unified` 档位的视觉零样本**根本没有黑名单过滤**，14 个敏感叶子
//! > 直接参与 150 词全表 top-K 竞争。
//!
//! 本模块把「敏感内容子树」收敛为**唯一判据**，供三处消费：
//!
//! 1. `omni_vision::gemma` 视觉零样本候选闸（Gemma 档位唯一拦截面）；
//! 2. `omni_vision::is_vocab_layer_blocked`（CLIP 候选闸，词表级封闭）；
//! 3. `omni_vision::is_nsfw_or_restricted_tag`（全年龄内容纯净闸，出口级清洗）。
//!
//! ## 事实源与维护纪律
//!
//! 清单 = 创世维度 `fileDimension_zh-CN.json` 中三个合规审查维度的**全部叶子**：
//!
//! | 维度 | 受控代码 | 叶子数 |
//! | :--- | :--- | :--- |
//! | 130 敏感内容 | `builtin.sensitive_content` | 6 |
//! | 123 色情分级 | `builtin.erotic_scale` | 5 |
//! | 134 色情细分 | `builtin.pornography_subgenres` | 12 |
//!
//! 表成员一律用 [`Concept`] 权威枚举表达（PRD-0058：严禁业务层自然语言词串比对），
//! 由此天然获得中英别名与受控 code 的跨语言幂等匹配。
//!
//! **纪律**：上述维度新增/改名叶子时必须同步在此登记——漏登记的标签会穿透视觉
//! 零样本直达全年龄文件。单测 `sensitive_subtree_covers_all_controlled_leaves`
//! 锁死叶子数量与 code 归属，改动即报警。

use crate::concepts::Concept;
use crate::tag_identity::{concept_from_code, concept_from_code_or_alias};

/// 受控「敏感内容」子树叶子全集（130 敏感内容 + 123 色情分级 + 134 色情细分）
///
/// ⚠ 该常量是合规审查标签的**唯一封闭判据**，新增维度叶子必须同步登记。
pub const SENSITIVE_SUBTREE_CONCEPTS: &[Concept] = &[
    // ── 维度 130「敏感内容」(`builtin.sensitive_content`) 6 叶子 ──────────────
    Concept::暴恐,       // builtin.violence_terrorism
    Concept::涉政,       // builtin.political_issues
    Concept::违规,       // omw.00746116.n
    Concept::色情,       // omw.00747215.n
    Concept::辱骂,       // omw.00419908.n
    Concept::血腥,       // omw.00247439.a
    // ── 维度 123「色情分级」(`builtin.erotic_scale`) 5 叶子 ───────────────────
    Concept::软色情,     // builtin.soft_porn
    Concept::半肉,       // builtin.semi_explicit
    Concept::纯肉,       // builtin.fully_explicit
    Concept::特殊XP,     // builtin.special_xp
    Concept::重度猎奇,   // builtin.extreme_grotesque
    // ── 维度 134「色情细分」(`builtin.pornography_subgenres`) 12 叶子 ─────────
    Concept::家庭乱伦,       // builtin.family_incest
    Concept::婚外情欲,       // builtin.extramarital_lust
    Concept::师生情色,       // builtin.teacher_student_erotica
    Concept::职场情色,       // builtin.workplace_erotica
    Concept::多人群交,       // builtin.group_sex
    Concept::性爱调教,       // builtin.sex_training
    Concept::窥视暴露,       // builtin.peeping_wardrobe_malfunction
    Concept::制服诱惑,       // builtin.uniform_seduction
    Concept::情色文学,       // builtin.erotic_literature
    Concept::情欲生理,       // builtin.erotic_physiology
    Concept::非自愿侵犯,     // builtin.involuntary_assault
    Concept::多元性取向,     // builtin.diverse_sexual_orientations
];

/// 该受控概念是否属于「敏感内容」子树（合规审查标签）
#[inline]
pub fn is_sensitive_subtree_concept(concept: Concept) -> bool {
    SENSITIVE_SUBTREE_CONCEPTS.contains(&concept)
}

/// 受控 code 是否属于「敏感内容」子树（视觉零样本候选闸的主判据）
///
/// 非受控 code（`_ext.*` / `dim.*` / 裸词面）一律返回 `false`——开放集无合规语义。
pub fn is_sensitive_subtree_code(code: &str) -> bool {
    let clean = code.trim();
    if clean.is_empty() {
        return false;
    }
    concept_from_code(clean).is_some_and(is_sensitive_subtree_concept)
}

/// 任意词面（受控 code / 中文规范名 / 英文名 / 别名）是否属于「敏感内容」子树
///
/// 出口级清洗（`is_nsfw_or_restricted_tag`）使用本函数：上游可能送来
/// 中文展示名、英文名或受控 code，三种形态必须同判。
///
/// ## 为什么是「一次解析」而不是「逐叶子比对」
///
/// 早期实现为 `SENSITIVE_SUBTREE_CONCEPTS.iter().any(|cpt| tag_matches_concept(..))`，
/// 单次判定最多触发 46 次 `builtin_tag_code`（每次含 `dynamic_aliases()` RwLock 读）。
/// 该函数处于热路径：CLIP 全词表循环与 omni-server 六通道 `retain` 会把它放大到
/// 每图数千~数万次锁读。改为 `concept_from_code_or_alias` 一次收口（GH #729 D4 唯一入口）
/// 后，锁读次数降为常数 1~2 次，判定语义等价。
pub fn is_sensitive_subtree_tag(word: &str) -> bool {
    let clean = word.trim();
    if clean.is_empty() {
        return false;
    }
    concept_from_code_or_alias(clean).is_some_and(is_sensitive_subtree_concept)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 叶子数量锁：130(6) + 123(5) + 134(12) = 23
    /// 维度叶子增删必须同步本表，否则此断言立即失败（防再次静默漂移）
    #[test]
    fn sensitive_subtree_covers_all_controlled_leaves() {
        assert_eq!(
            SENSITIVE_SUBTREE_CONCEPTS.len(),
            23,
            "敏感内容子树叶子数量与创世维度事实源不符：130(6) + 123(5) + 134(12) = 23"
        );
        let mut codes: Vec<&str> = SENSITIVE_SUBTREE_CONCEPTS.iter().map(|cpt| cpt.code()).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), 23, "子树清单存在重复 code");
    }

    /// 真实事故回归：这两个叶子此前双闸皆漏判
    #[test]
    fn user_reported_leaks_are_covered() {
        for (zh, code) in [
            ("师生情色", "builtin.teacher_student_erotica"),
            ("多元性取向", "builtin.diverse_sexual_orientations"),
            ("职场情色", "builtin.workplace_erotica"),
            ("制服诱惑", "builtin.uniform_seduction"),
            ("情色文学", "builtin.erotic_literature"),
            ("软色情", "builtin.soft_porn"),
            ("特殊XP", "builtin.special_xp"),
            ("涉政", "builtin.political_issues"),
        ] {
            assert!(is_sensitive_subtree_tag(zh), "中文名未命中: {zh}");
            assert!(is_sensitive_subtree_code(code), "受控 code 未命中: {code}");
            assert!(
                is_sensitive_subtree_tag(code),
                "code 形态经 tag 入口未命中: {code}"
            );
        }
    }

    /// 英文名同判（跨语言幂等）
    #[test]
    fn english_names_match_too() {
        assert!(is_sensitive_subtree_tag("Teacher-Student Erotica"));
        assert!(is_sensitive_subtree_tag("Diverse Sexual Orientations"));
        assert!(is_sensitive_subtree_tag("Workplace Erotica"));
    }

    /// 中性生活工作词汇严禁误杀（历史白名单护栏）
    #[test]
    fn neutral_life_work_words_are_not_sensitive() {
        for w in ["校园师生", "职场社交", "职业制服", "文学题材", "大学", "摄影照片", "户外活动"] {
            assert!(!is_sensitive_subtree_tag(w), "中性词被误判为敏感: {w}");
        }
    }

    /// 开放集与非受控词面不得命中
    #[test]
    fn open_set_codes_are_not_sensitive() {
        for w in ["_ext.abc123", "dim.134.XXX", "builtin.content_tags", ""] {
            assert!(!is_sensitive_subtree_code(w), "开放集/空串被误判: {w}");
        }
    }
}
