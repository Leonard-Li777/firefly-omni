//! 受控概念语义对立（互斥）关系的**泛化解析器**与通用仲裁
//!
//! ## 为什么需要泛化
//! 受控本体是**有限集合**，因此「哪些概念互斥」允许人工确权后硬编码；但互斥关系并不只
//! 存在于情绪维度——同一维度内凡处于同一条语义轴两端的概念，共现即自相矛盾：
//!
//! | 维度 | 互斥轴（同维共现即矛盾） |
//! | :--- | :--- |
//! | `builtin.emotion_tags`（情感） | 欢乐 ↔ 悲伤、治愈 ↔ 致郁、轻松 ↔ 紧张、兴奋 ↔ 压抑 |
//! | `builtin.image_subtype`（图片细分） | 有字图 ↔ 无字图 |
//! | 画幅 | 横屏 ↔ 竖屏 |
//! | 主色调 | 冷色调 ↔ 暖色调 |
//!
//! 除受控注册表外，**系统本体（`semantic.pack` 的 `omw_sense_relations.rel_type='antonym'`）
//! 是权威外部反义源**：凡是受控库中已有的 OMW/Hownet 概念，其反义对立应以本体图谱为准，
//! 而不是再抄一份硬编码表。故本模块把两者统一为「可组合的数据源」：
//!
//! ```text
//! OppositionResolver = [受控注册表（有限本体，人工确权）] + [OMW 反义图谱（运行时按候选现取）]
//! ```
//!
//! ## 权威优先级与实测边界（务必知悉）
//! 1. **受控注册表优先，且目前是情感对立对的唯一权威**：实测语义包 `omw_sense_relations`
//!    中 `omw.00918939.s`（兴奋）、`omw.02423762.v`（压抑）、`omw.01365785.s`（悲伤）等
//!    情感义位**零反义边**（`兴奋` 与 `压抑` 甚至零词义关系边，且二者词性不同：`.s` 形容词
//!    卫星义位 vs `.v` 动词义位），WordNet/OMW 结构上无法表达这对对立——这正是当初必须
//!    硬编码的原因，不是偷懒；
//! 2. **外部源是补充**：OMW 反义图谱可覆盖注册表未收录的受控概念（如「公开 ↔ 绝密」类
//!    词面对立），但其边含跨词性噪声，故只作为**追加判定**（任一源成立即判对立），
//!    不参与「撤销」注册表的既有判定。
//!
//! ## 裁决契约（与单选 argmax 的 `>` 语义保持一致，确定性且与语言无关）
//! - 对立两侧同现时**仅保留置信度更高者**；同分（含双 NaN）保留**先到者**（索引小者）；
//! - NaN 置信度视为最低分（该侧必落选）；
//! - 同一概念因跨注册表别名重复出现时，按其中最高分参与比较，落选时**全部同名实例一并落选**
//!   （否则落选侧会从另一个别名复活）；
//! - 非对立成员一律放行，多选契约不收窄（互补情绪「致郁 + 压抑 + 紧张」照旧并存）。
//!
//! 表成员一律使用 [`Concept`] 权威枚举，严禁在业务层写自然语言词串比对（PRD-0058）。

use crate::concepts::Concept;
use crate::TagChainItem;
use std::collections::{HashMap, HashSet};

/// 本体可供给的关系类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationKind {
    /// 反义对立：同一条语义轴的两端，共现即矛盾（本模块仲裁的唯一依据）
    Antonym,
    /// 近义/形近：易混同但并不互斥，供展示归并、检索去重与人工复核参考，**不参与互斥裁决**
    Similar,
}

/// 语义对立关系数据源（可组合：受控注册表 + 运行时本体图谱）
pub trait OppositeSource {
    /// 返回与 `concept_item` 在该关系下成对的概念（无则空集；实现必须是自反对称的）
    fn counterparts(&self, kind: RelationKind, concept_item: Concept) -> Vec<Concept>;
}

/// 受控本体对立对注册表（有限本体：人工确权后硬编码即权威）
#[derive(Debug, Clone, Copy)]
pub struct CuratedRegistry {
    pairs: &'static [(Concept, Concept)],
}

impl CuratedRegistry {
    /// 以静态表构造注册表（每对只声明一次，判定时对称生效）
    pub const fn new(pairs: &'static [(Concept, Concept)]) -> Self {
        Self { pairs }
    }

    /// 注册表的静态对表（供护栏测试与文档生成读取）
    pub const fn pairs(&self) -> &'static [(Concept, Concept)] {
        self.pairs
    }
}

impl OppositeSource for CuratedRegistry {
    fn counterparts(&self, kind: RelationKind, concept_item: Concept) -> Vec<Concept> {
        // 注册表当前只声明反义对立；形近/近义不作为互斥依据
        if kind != RelationKind::Antonym {
            return Vec::new();
        }
        self.pairs
            .iter()
            .filter_map(|(lhs, rhs)| {
                if *lhs == concept_item {
                    Some(*rhs)
                } else if *rhs == concept_item {
                    Some(*lhs)
                } else {
                    None
                }
            })
            .collect()
    }
}

/// 运行期动态对立对集合
///
/// 用途：把 `semantic.pack` 的 OMW 反义图谱中**与本次候选相关**的边现取成对表
/// （受控概念 4 万量级，全量装载无必要；按候选 code 查询 `get_antonyms` 即可）。
#[derive(Debug, Clone, Default)]
pub struct PairSet {
    pairs: Vec<(Concept, Concept)>,
}

impl PairSet {
    /// 由概念对构造（自动去重与自反对称化：`(a, b)` 与 `(b, a)` 等价，`(a, a)` 丢弃）
    pub fn from_pairs(pairs: impl IntoIterator<Item = (Concept, Concept)>) -> Self {
        let mut seen: HashSet<(Concept, Concept)> = HashSet::new();
        let mut unique = Vec::new();
        for (left, right) in pairs {
            if left == right {
                continue;
            }
            let key = if (left as u32) <= (right as u32) {
                (left, right)
            } else {
                (right, left)
            };
            if seen.insert(key) {
                unique.push(key);
            }
        }
        Self { pairs: unique }
    }

    /// 已收录的对数（供日志与测试观测）
    pub fn len(&self) -> usize {
        self.pairs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }
}

impl OppositeSource for PairSet {
    fn counterparts(&self, kind: RelationKind, concept_item: Concept) -> Vec<Concept> {
        if kind != RelationKind::Antonym {
            return Vec::new();
        }
        self.pairs
            .iter()
            .filter_map(|(lhs, rhs)| {
                if *lhs == concept_item {
                    Some(*rhs)
                } else if *rhs == concept_item {
                    Some(*lhs)
                } else {
                    None
                }
            })
            .collect()
    }
}

/// 泛化对立解析器：按源顺序询问，**任一源判定成对即视为对立**
pub struct OppositionResolver<'a> {
    sources: Vec<&'a dyn OppositeSource>,
}

impl<'a> OppositionResolver<'a> {
    /// 显式给定数据源（受控注册表在前，外部本体源在后）
    pub fn new(sources: Vec<&'a dyn OppositeSource>) -> Self {
        Self { sources }
    }

    /// 仅受控注册表（omni-core 内无数据库连接时的落点：单选/多选维度仲裁）
    pub fn controlled_only() -> Self {
        Self::new(vec![
            &crate::emotion_opposition::EMOTION_REGISTRY,
            &CONTROLLED_VISUAL_REGISTRY,
        ])
    }

    /// 仅情感对立对（`emotion_opposition` 兼容入口与既有护栏测试使用）
    pub fn emotion_only() -> Self {
        Self::new(vec![&crate::emotion_opposition::EMOTION_REGISTRY])
    }

    /// 某概念在全部数据源下的对立面并集（去重，保持源顺序）
    pub fn counterparts(&self, kind: RelationKind, concept_item: Concept) -> Vec<Concept> {
        let mut result: Vec<Concept> = Vec::new();
        for source in &self.sources {
            for item in source.counterparts(kind, concept_item) {
                if item != concept_item && !result.contains(&item) {
                    result.push(item);
                }
            }
        }
        result
    }

    /// 两概念是否构成对立（对称）
    pub fn is_opposed(&self, left: Concept, right: Concept) -> bool {
        left != right && self.counterparts(RelationKind::Antonym, left).contains(&right)
    }

    /// 在候选标签中裁决语义对立冲突，返回**必须落选的候选索引集合**
    ///
    /// - 入参按候选原顺序给出 `(概念, 置信度)`，`None` 表示该候选不参与裁决
    ///   （已淘汰 / 概念未解析 / 非对立对成员）；
    /// - 返回集合仅供调用方剔除，不改变其它候选的相对顺序。
    pub fn resolve_conflicts(&self, candidates: &[Option<(Concept, f32)>]) -> HashSet<usize> {
        // 概念 → (代表索引, 最高置信度)
        let mut present: HashMap<Concept, (usize, f32)> = HashMap::new();
        // 概念 → 全部出现索引（跨别名重复时需整组剔除）
        let mut occurrences: HashMap<Concept, Vec<usize>> = HashMap::new();
        for (idx, candidate) in candidates.iter().enumerate() {
            let Some((concept_item, confidence)) = candidate else {
                continue;
            };
            occurrences.entry(*concept_item).or_default().push(idx);
            present
                .entry(*concept_item)
                .and_modify(|(best_idx, best_conf)| {
                    let better = match (best_conf.is_nan(), confidence.is_nan()) {
                        (true, false) => true,
                        (false, true) => false,
                        (true, true) => idx < *best_idx,
                        (false, false) => *confidence > *best_conf,
                    };
                    if better {
                        *best_idx = idx;
                        *best_conf = *confidence;
                    }
                })
                .or_insert((idx, *confidence));
        }

        let mut dropped: HashSet<usize> = HashSet::new();
        // 无序对去重：同一对只裁决一次（双向查询结果一致，故结果与遍历顺序无关）
        let mut adjudicated: HashSet<(usize, usize)> = HashSet::new();
        for (concept_item, (idx, confidence)) in present.iter() {
            for counterpart in self.counterparts(RelationKind::Antonym, *concept_item) {
                let Some(&(other_idx, other_conf)) = present.get(&counterpart) else {
                    continue;
                };
                if other_idx == *idx {
                    continue;
                }
                let key = if *idx < other_idx {
                    (*idx, other_idx)
                } else {
                    (other_idx, *idx)
                };
                if !adjudicated.insert(key) {
                    continue;
                }
                // 同分（含双 NaN）时保留先到者：索引小者胜出，保证确定性
                let lhs_wins = match (confidence.is_nan(), other_conf.is_nan()) {
                    (true, true) => *idx < other_idx,
                    (true, false) => false,
                    (false, true) => true,
                    (false, false) if *confidence > other_conf => true,
                    (false, false) if other_conf > *confidence => false,
                    (false, false) => *idx < other_idx,
                };
                let loser = if lhs_wins { counterpart } else { *concept_item };
                if let Some(idxs) = occurrences.get(&loser) {
                    dropped.extend(idxs.iter().copied());
                }
            }
        }
        dropped
    }

    /// 在多个标签集合之间做对立联合裁决（就地剔除落选项），返回剔除条数
    ///
    /// 使用场景：视觉标签与融合标签各自独立仲裁后，同一对立对可能**各留一侧**
    /// （如视觉组留「兴奋」、融合组留「压抑」）；桌面端按组合并落库，故必须在响应出口
    /// 再做一次跨集合联合裁决，否则矛盾对仍会同时出现在文件标签面板
    /// （真实事故：`DSC01875.jpg` 同时挂载「兴奋」「压抑」）。
    ///
    /// 折中口径：按各集合内的原始顺序编号，等分时「先到者」胜出（视觉组先于融合组），
    /// 与 [`OppositionResolver::resolve_conflicts`] 的确定性语义保持一致。
    pub fn arbitrate_across_sets(&self, sets: &mut [&mut Vec<TagChainItem>]) -> usize {
        // 全局索引 → (集合下标, 集合内下标)
        let mut slots: Vec<(usize, usize)> = Vec::new();
        // 全局索引 → 对立面（仅对立对成员入池）
        let mut faces: Vec<Option<(Concept, f32)>> = Vec::new();

        for (set_idx, set) in sets.iter().enumerate() {
            for (tag_idx, tag) in set.iter().enumerate() {
                let face = crate::tag_identity::concept_from_code_or_alias(&tag.code)
                    .filter(|concept_item| {
                        !self
                            .counterparts(RelationKind::Antonym, *concept_item)
                            .is_empty()
                    })
                    .map(|concept_item| (concept_item, tag.confidence));
                faces.push(face);
                slots.push((set_idx, tag_idx));
            }
        }

        let dropped = self.resolve_conflicts(&faces);
        if dropped.is_empty() {
            return 0;
        }

        let mut per_set: Vec<Vec<usize>> = vec![Vec::new(); sets.len()];
        for global_idx in &dropped {
            let (set_idx, tag_idx) = slots[*global_idx];
            let tag = &sets[set_idx][tag_idx];
            tracing::debug!(
                "[语义对立互斥:跨集合联合裁决] set={} tag={} code={} conf={:.3}",
                set_idx,
                tag.name,
                tag.code,
                tag.confidence
            );
            per_set[set_idx].push(tag_idx);
        }

        let mut removed = 0usize;
        for (set_idx, set) in sets.iter_mut().enumerate() {
            let mut idxs = per_set[set_idx].clone();
            // 逆序删除，避免下标漂移
            idxs.sort_unstable_by(|lhs, rhs| rhs.cmp(lhs));
            for tag_idx in idxs {
                set.remove(tag_idx);
                removed += 1;
            }
        }
        removed
    }
}

/// 视觉与画幅类受控维度的互斥轴（情绪维度见 [`crate::emotion_opposition::OPPOSING_EMOTION_PAIRS`]）
///
/// 收录准则（新增维度必须同时满足）：
/// 1. 两侧均为受控 `builtin.*`/`hownet.*` 概念，且同属一个受控维度或同一语义轴；
/// 2. 「同维共现」在物理上不可能或无意义（有字图与无字图、横屏与竖屏、冷色与暖色调）；
/// 3. 仅在 OMW 反义图谱**无对应边**时才必须入表（有边的概念交给本体图谱，避免双份事实源）。
pub const VISUAL_OPPOSITION_PAIRS: &[(Concept, Concept)] = &[
    (Concept::有字图, Concept::无字图),
    (Concept::横屏, Concept::竖屏),
    (Concept::冷色调, Concept::暖色调),
];

/// 视觉互斥轴注册表
pub const CONTROLLED_VISUAL_REGISTRY: CuratedRegistry =
    CuratedRegistry::new(VISUAL_OPPOSITION_PAIRS);

#[cfg(test)]
mod tests {
    use super::*;

    fn chain_item(code: &str, name: &str, confidence: f32) -> TagChainItem {
        TagChainItem {
            code: code.to_string(),
            name: name.to_string(),
            confidence,
            ..Default::default()
        }
    }

    #[test]
    fn test_curated_registry_is_symmetric_and_scoped() {
        let registry = &CONTROLLED_VISUAL_REGISTRY;
        for (left, right) in registry.pairs() {
            assert_eq!(
                registry.counterparts(RelationKind::Antonym, *left),
                vec![*right],
                "对立对 {:?} 必须对称",
                left
            );
            assert_eq!(
                registry.counterparts(RelationKind::Antonym, *right),
                vec![*left],
                "对立对 {:?} 必须对称",
                right
            );
        }
        // 形近/近义不作为互斥依据
        assert!(registry
            .counterparts(RelationKind::Similar, Concept::有字图)
            .is_empty());
        // 非对立成员不入表
        assert!(registry
            .counterparts(RelationKind::Antonym, Concept::风景照)
            .is_empty());
    }

    #[test]
    fn test_resolver_combines_curated_and_external_sources() {
        // 外部源（模拟 OMW 反义图谱现取的对表）补充注册表之外的受控对立
        let external = PairSet::from_pairs(vec![(Concept::公开, Concept::绝密)]);
        let resolver = OppositionResolver::new(vec![
            &crate::emotion_opposition::EMOTION_REGISTRY,
            &CONTROLLED_VISUAL_REGISTRY,
            &external,
        ]);

        // 受控注册表命中
        assert!(resolver.is_opposed(Concept::有字图, Concept::无字图));
        assert!(resolver.is_opposed(Concept::兴奋, Concept::压抑));
        // 外部本体命中
        assert!(resolver.is_opposed(Concept::公开, Concept::绝密));
        // 非对立不误判，且自身永不对立
        assert!(!resolver.is_opposed(Concept::风景照, Concept::摄影照片));
        assert!(!resolver.is_opposed(Concept::有字图, Concept::有字图));
        // 受控注册表判定不因外部源缺失而丢失
        let curated_only = OppositionResolver::controlled_only();
        assert!(curated_only.is_opposed(Concept::冷色调, Concept::暖色调));
        assert!(!curated_only.is_opposed(Concept::公开, Concept::绝密));
    }

    #[test]
    fn test_pair_set_dedupes_and_symmetrizes() {
        let set = PairSet::from_pairs(vec![
            (Concept::公开, Concept::绝密),
            (Concept::绝密, Concept::公开),
            (Concept::公开, Concept::公开),
        ]);
        assert_eq!(set.len(), 1);
        assert_eq!(set.counterparts(RelationKind::Antonym, Concept::公开), vec![Concept::绝密]);
        assert_eq!(set.counterparts(RelationKind::Antonym, Concept::绝密), vec![Concept::公开]);
    }

    #[test]
    fn test_resolver_prunes_visual_axis_conflict() {
        let resolver = OppositionResolver::controlled_only();
        let candidates = vec![
            Some((Concept::有字图, 0.61)),
            Some((Concept::无字图, 0.58)),
            Some((Concept::风景照, 0.90)),
        ];
        let dropped = resolver.resolve_conflicts(&candidates);
        assert_eq!(dropped.len(), 1);
        assert!(dropped.contains(&1), "低分侧必须落选: {:?}", dropped);

        // 非对立成员一律放行
        let compatible = vec![
            Some((Concept::横屏, 0.80)),
            Some((Concept::冷色调, 0.70)),
            Some((Concept::暖色调, 0.66)),
        ];
        let dropped = resolver.resolve_conflicts(&compatible);
        assert_eq!(dropped, HashSet::from([2]));
    }

    #[test]
    fn test_arbitrate_across_sets_prunes_split_visual_axis() {
        let resolver = OppositionResolver::controlled_only();
        let mut visual = vec![chain_item(Concept::横屏.code(), "横屏", 0.66)];
        let mut fused = vec![chain_item(Concept::竖屏.code(), "竖屏", 0.72)];
        let removed = resolver.arbitrate_across_sets(&mut [&mut visual, &mut fused]);

        assert_eq!(removed, 1);
        assert!(visual.is_empty(), "低分侧必须被剔除");
        assert!(fused.iter().any(|t| t.code == Concept::竖屏.code()));
    }

    #[test]
    fn test_arbitrate_across_sets_merges_external_source_pairs() {
        // 跨集合矛盾由外部本体源（OMW 反义图谱）判定，注册表未收录时同样必须裁决
        let external = PairSet::from_pairs(vec![(Concept::公开, Concept::绝密)]);
        let resolver =
            OppositionResolver::new(vec![&CONTROLLED_VISUAL_REGISTRY, &external]);
        let mut visual = vec![chain_item(Concept::公开.code(), "公开", 0.81)];
        let mut fused = vec![chain_item(Concept::绝密.code(), "绝密", 0.77)];
        let removed = resolver.arbitrate_across_sets(&mut [&mut visual, &mut fused]);

        assert_eq!(removed, 1);
        assert!(visual.iter().any(|t| t.code == Concept::公开.code()));
        assert!(fused.is_empty());
    }
}
