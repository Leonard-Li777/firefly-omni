//! 情绪维度对立对互斥（多选维度内的语义对立裁决）
//!
//! ## 背景与不变量
//! `builtin.emotion_tags`（维度 121）在 DEC-02 多选白名单内：单张画面/单段文本确实可以
//! 同时具备多种**互补**情绪（例如「致郁 + 压抑 + 紧张」同现是合理的），因此该维度不能
//! 退化为单选 argmax。但一对**语义对立**的情绪（兴奋 ↔ 压抑）描述的是同一条情感轴的
//! 两端，同维共现即自相矛盾（真实事故：`DSC01875.jpg` 同时挂载「兴奋」「压抑」）。
//!
//! ## 裁决契约
//! - 对立对两侧同现时，**仅保留置信度更高者**；置信度相等时保留先到者（索引小者），
//!   与单选互斥 argmax 的 `>` 语义一致，保证裁决结果确定性、与语言无关；
//! - NaN 置信度视为最低分（该侧必落选），不参与胜出；
//! - 非对立对成员（含其余 11 个情绪概念）一律放行，不做任何改写。
//!
//! 表成员一律使用 [`Concept`] 权威枚举，严禁在业务层写自然语言词串比对（PRD-0058）。

use crate::concepts::Concept;
use crate::concept_opposition::{CuratedRegistry, OppositionResolver, RelationKind};
use std::collections::HashSet;

/// 情绪维度（`builtin.emotion_tags`）内的语义对立对（每对仅声明一次，判定时对称生效）。
///
/// 说明：`支配 / 羞耻 / 罪恶感 / 背德感 / 感动 / 怀旧 / 欲望` 之间不存在同一情感轴上的
/// 严格对立关系，故不入表，保持多选并存。
pub const OPPOSING_EMOTION_PAIRS: &[(Concept, Concept)] = &[
    (Concept::欢乐, Concept::悲伤),
    (Concept::治愈, Concept::致郁),
    (Concept::轻松, Concept::紧张),
    (Concept::兴奋, Concept::压抑),
];

/// 情感维度对立对注册表：作为泛化解析器 [`OppositionResolver`] 的受控源之一
///
/// 为何必须保留这张硬编码表：实测语义包 `omw_sense_relations` 中「兴奋（omw.00918939.s）」
/// 「压抑（omw.02423762.v）」「悲伤（omw.01365785.s）」等义位**零反义边**（兴奋与压抑甚至
/// 零词义关系边，且二者词性一为形容词卫星义位、一为动词义位），OWM 结构上无法表达这对对立，
/// 因此情感对立对的权威源只能是受控注册表；外部本体图谱仅作追加补充。
pub const EMOTION_REGISTRY: CuratedRegistry = CuratedRegistry::new(OPPOSING_EMOTION_PAIRS);

/// 返回该概念的情绪对立面；非对立对成员返回 `None`。
///
/// 兼容入口：内部委托给泛化解析器（仅情感注册表源），与 [`crate::concept_opposition`]
/// 的通用判定同源，避免「情感专用判定」与「通用判定」两套事实源漂移。
pub fn opposing_emotion(concept_item: Concept) -> Option<Concept> {
    OppositionResolver::emotion_only()
        .counterparts(RelationKind::Antonym, concept_item)
        .into_iter()
        .next()
}

/// 在候选标签中裁决情绪对立冲突，返回**必须落选的候选索引集合**。
///
/// - 入参按候选原顺序给出 `(概念, 置信度)`，`None` 表示该候选不参与裁决
///   （已淘汰 / 概念未解析 / 非情绪概念）；
/// - 同一概念可能因跨注册表别名而重复出现（如「兴奋」同时映射到两个 `omw.*` code）：
///   按其中**最高分那次**参与分数比较，但一侧落选时其**全部同名实例**一并落选，
///   否则落选侧会从另一个别名"复活"，矛盾对依旧共现；
/// - 返回集合仅供调用方剔除，不改变其它候选的相对顺序。
pub fn resolve_opposing_emotion_conflicts(candidates: &[Option<(Concept, f32)>]) -> HashSet<usize> {
    OppositionResolver::emotion_only().resolve_conflicts(candidates)
}

/// 在多个标签集合之间做情绪对立联合裁决（就地剔除落选项），返回剔除条数。
///
/// 使用场景：视觉标签与融合标签各自独立仲裁后，同一情绪对立对可能**各留一侧**
/// （如视觉组留「兴奋」、融合组留「压抑」）；桌面端按组合并落库，故必须在响应出口
/// 再做一次跨集合联合裁决，否则矛盾对仍会同时出现在文件标签面板。
///
/// 折中口径：按各集合内的原始顺序编号，等分时“先到者”胜出（视觉组先于融合组），
/// 与 [`resolve_opposing_emotion_conflicts`] 的确定性语义保持一致。
pub fn arbitrate_opposing_emotions_across_sets(
    sets: &mut [&mut Vec<crate::TagChainItem>],
) -> usize {
    OppositionResolver::emotion_only().arbitrate_across_sets(sets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_opposing_emotion_pairs_are_symmetric() {
        for (lhs, rhs) in OPPOSING_EMOTION_PAIRS {
            assert_eq!(
                opposing_emotion(*lhs),
                Some(*rhs),
                "对立对 {:?} 必须对称",
                lhs
            );
            assert_eq!(
                opposing_emotion(*rhs),
                Some(*lhs),
                "对立对 {:?} 必须对称",
                rhs
            );
        }
        // 非对立成员不入表
        assert_eq!(opposing_emotion(Concept::感动), None);
        assert_eq!(opposing_emotion(Concept::怀旧), None);
        assert_eq!(opposing_emotion(Concept::支配), None);
    }

    #[test]
    fn test_resolve_opposing_emotion_conflicts_keeps_higher_confidence() {
        let candidates = vec![
            Some((Concept::兴奋, 0.66)),
            Some((Concept::压抑, 0.84)),
            // 互补情绪：与胜出者并存，不参与对立裁决
            Some((Concept::紧张, 0.71)),
            Some((Concept::致郁, 0.75)),
            None,
        ];
        let dropped = resolve_opposing_emotion_conflicts(&candidates);
        assert_eq!(dropped.len(), 1);
        assert!(
            dropped.contains(&0),
            "低分侧「兴奋」必须落选: {:?}",
            dropped
        );
    }

    #[test]
    fn test_resolve_opposing_emotion_conflicts_keeps_earlier_on_tie() {
        let candidates = vec![Some((Concept::压抑, 0.80)), Some((Concept::兴奋, 0.80))];
        let dropped = resolve_opposing_emotion_conflicts(&candidates);
        assert_eq!(dropped.len(), 1);
        assert!(dropped.contains(&1), "同分时保留先到者: {:?}", dropped);
    }

    fn chain_item(code: &str, name: &str, confidence: f32) -> crate::TagChainItem {
        crate::TagChainItem {
            code: code.to_string(),
            name: name.to_string(),
            confidence,
            ..Default::default()
        }
    }

    #[test]
    fn test_arbitrate_opposing_emotions_across_sets_prunes_split_collections() {
        // 视觉组留「兴奋」、融合组留「压抑」：单集合内部仲裁各自合法，跨集合必须再裁一次
        let mut visual = vec![
            chain_item(Concept::兴奋.code(), "兴奋", 0.66),
            chain_item(Concept::紧张.code(), "紧张", 0.71),
        ];
        let mut fused = vec![
            chain_item(Concept::压抑.code(), "压抑", 0.84),
            chain_item(Concept::致郁.code(), "致郁", 0.75),
        ];
        let removed = arbitrate_opposing_emotions_across_sets(&mut [&mut visual, &mut fused]);

        assert_eq!(removed, 1);
        assert!(
            !visual.iter().any(|t| t.code == Concept::兴奋.code()),
            "低分侧必须被剔除"
        );
        assert!(visual.iter().any(|t| t.code == Concept::紧张.code()));
        assert!(fused.iter().any(|t| t.code == Concept::压抑.code()));
        assert!(fused.iter().any(|t| t.code == Concept::致郁.code()));
    }

    #[test]
    fn test_arbitrate_opposing_emotions_across_sets_noop_without_conflict() {
        let mut visual = vec![chain_item(Concept::兴奋.code(), "兴奋", 0.66)];
        let mut fused = vec![
            chain_item(Concept::治愈.code(), "治愈", 0.80),
            chain_item(Concept::致郁.code(), "致郁", 0.70),
        ];
        // 治愈/致郁 属同一对立对：跨集合仍应按分数裁剪
        let removed = arbitrate_opposing_emotions_across_sets(&mut [&mut visual, &mut fused]);
        assert_eq!(removed, 1);
        assert!(fused.iter().any(|t| t.code == Concept::治愈.code()));
        assert!(!fused.iter().any(|t| t.code == Concept::致郁.code()));
        assert!(visual.iter().any(|t| t.code == Concept::兴奋.code()));
    }

    #[test]
    fn test_resolve_opposing_emotion_conflicts_drops_all_alias_duplicates() {
        // 跨注册表别名：同一「兴奋」概念出现两次（不同 code，经 code 解析后同概念）
        let candidates = vec![
            Some((Concept::兴奋, 0.62)),
            Some((Concept::兴奋, 0.71)),
            Some((Concept::压抑, 0.58)),
        ];
        let dropped = resolve_opposing_emotion_conflicts(&candidates);
        assert_eq!(dropped.len(), 1, "落选侧整组剔除: {:?}", dropped);
        assert!(
            dropped.contains(&2),
            "低分侧「压抑」必须落选: {:?}",
            dropped
        );

        let candidates = vec![
            Some((Concept::兴奋, 0.61)),
            Some((Concept::兴奋, 0.63)),
            Some((Concept::压抑, 0.86)),
        ];
        let dropped = resolve_opposing_emotion_conflicts(&candidates);
        assert_eq!(
            dropped.len(),
            2,
            "「兴奋」两个别名实例须一并剔除: {:?}",
            dropped
        );
        assert!(dropped.contains(&0) && dropped.contains(&1));
    }

    #[test]
    fn test_resolve_opposing_emotion_conflicts_nan_loses() {
        let candidates = vec![Some((Concept::欢乐, f32::NAN)), Some((Concept::悲伤, 0.61))];
        let dropped = resolve_opposing_emotion_conflicts(&candidates);
        assert!(
            dropped.contains(&0),
            "NaN 置信度一侧必须落选: {:?}",
            dropped
        );

        // 不同对立对互不干扰：两组对立同时存在时各淘汰一个
        let candidates = vec![
            Some((Concept::欢乐, 0.70)),
            Some((Concept::悲伤, 0.62)),
            Some((Concept::轻松, 0.61)),
            Some((Concept::紧张, 0.74)),
        ];
        let dropped = resolve_opposing_emotion_conflicts(&candidates);
        assert_eq!(dropped.len(), 2);
        assert!(dropped.contains(&1));
        assert!(dropped.contains(&2));
    }
}
