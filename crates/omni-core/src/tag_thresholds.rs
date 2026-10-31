//! (WP5) 图片标签链路阈值集中定义：单点可调，便于后续校准表（score→prob 温度缩放）接入。
//!
//! 涵盖 CLIP 开放词表、RAM 投影/降级、互斥组胜出 margin、出口 conf 门限与分层回退置信。
//! 消费方：`omni-vision`（提取层）、`omni-server`（汇聚层出口）。

// ============ CLIP 开放词表（omni-vision extract_clip_visual_tags*） ============

/// CLIP 绝对候选阈值（点积/余弦，工业标准 0.18~0.25 取 0.20）
pub const CLIP_ABS_THRESHOLD: f32 = 0.20;

/// CLIP Top-1 侧向抑制 margin：与第一名差距超过该值的低分标签被滤除
pub const CLIP_TOP_MARGIN: f32 = 0.058;

/// CLIP 标定线性起步分（sim = CLIP_ABS_THRESHOLD 映射到 conf 下限）
pub const CLIP_CALIBRATE_FLOOR: f32 = 0.55;

/// CLIP 标定线性斜率
pub const CLIP_CALIBRATE_SLOPE: f32 = 1.5;

/// CLIP 标定 conf 上限
pub const CLIP_CALIBRATE_CAP: f32 = 0.97;

// ============ EmbeddingGemma 视觉零样本（omni-vision gemma.rs） ============
//
// 本组常量是 `scripts/bake-gemma-embeddings.py` 的**跨语言单一事实源**：
// 烘焙脚本用 `load_rust_threshold()` 正则解析本文件，禁止在 Python 侧另写一份。
// 改动任一数值必须同步重烤 150 词视觉词表与 OMW 消歧表，否则烘焙侧门禁护栏
// （assert_visual_vocab_gate_width）会按新 margin 判出与实际闸门不一致的结论。

/// Gemma 视觉零样本绝对候选下限（余弦）
///
/// 该值按**旧坍缩空间**（词表取 `sentence_embedding`，偏置项把 raw 整体抬到 0.85 量级）标定。
/// 烘焙侧改 content_mask 池化后 raw 分整体下移，但 2026-10-30 用金标图端到端复测
/// （`omni-server/tests/gemma_runtime_tests.rs::test_ac3_2_and_3_3_gold_landscape_image_inference`）
/// 确认 Top-1 仍为风景照、raw ≥ 0.305，本值仍有约 0.025 余量，暂不下调。
/// 扩到多图夹具后（TASK-02）需随 `GEMMA_MIN_PROMINENCE` 一起重新标定。
pub const GEMMA_ABS_FLOOR: f32 = 0.28;

/// Gemma 视觉零样本 Top-1 侧向抑制 margin：与第一名差距超过该值的候选被滤除
pub const GEMMA_TOP_MARGIN: f32 = 0.065;

/// Gemma 峰值-中位落差门限（峰值相对 150 词背景噪声的最小落差，见
/// docs/specs/gemma-visual-vocab-anisotropy-and-hub-domination-fix-spec.md §4.2 / §14.1）
///
/// 严禁写为 top1-top2，必须是 peak-to-median：允许非互斥标签同时过线，
/// 拦截平坦噪声背景上的假阳性。
pub const GEMMA_MIN_PROMINENCE: f32 = 0.08;

/// Gemma 标定线性起步分（sim = GEMMA_ABS_FLOOR 映射到 conf 下限）
pub const GEMMA_CALIBRATE_FLOOR: f32 = 0.60;

/// Gemma 标定线性斜率（TASK-02 去饱和修复：从 2.0 降至 1.2，使置信度在 [0.60, 0.95] 呈单调连续分布）
pub const GEMMA_CALIBRATE_SLOPE: f32 = 1.2;

/// Gemma 标定 conf 上限（饱和值，详见事故规格 §4.4：全库 visual 组该通道恒等于此值）
pub const GEMMA_CALIBRATE_CAP: f32 = 0.95;

// ============ RAM（omni-vision extract_ram_tags*） ============

/// RAM ONNX 主路径概率阈值
pub const RAM_ONNX_THRESHOLD: f32 = 0.5;

/// RAM 降级路径（embeddings 点积）绝对阈值
pub const RAM_FALLBACK_THRESHOLD: f32 = 0.22;

/// RAM 降级路径 Top-1 侧向抑制 margin
pub const RAM_FALLBACK_MARGIN: f32 = 0.05;

// ============ 互斥组（classify_mutual_exclusive_groups） ============

/// 互斥组宽松最低置信阈值（多数内容域组）
pub const MUTUAL_GROUP_THRESHOLD_LOOSE: f32 = 0.18;

/// 互斥组中等最低置信阈值（版式/受众/背景等二三值组）
pub const MUTUAL_GROUP_THRESHOLD_MEDIUM: f32 = 0.19;

/// 互斥组严格最低置信阈值（文字存在性/色彩模式等二元事实组）
pub const MUTUAL_GROUP_THRESHOLD_STRICT: f32 = 0.20;

/// 互斥组胜出所需次优差距（WP5 margin）：杜绝 0.18 勉强过线的噪声胜出
pub const MUTUAL_GROUP_MARGIN: f32 = 0.03;

// ============ 汇聚层出口（omni-server） ============

/// 出口置信度门限：低于该值的候选标签严禁透出到接口返回结果
pub const EXIT_CONFIDENCE_THRESHOLD: f32 = 0.60;

// ============ 文本源准入「分级放行」（omni-text `fusion.rs` 双锚点融合） ============

/// 文本源 ASCII 候选放行所需的**全局锚点原始余弦**阈值 `cos(e_term, e_global)`。
///
/// 受众：非受控（`!is_controlled_code`）+ 展示名纯 ASCII + 命中文本源
/// （`Ocr` / `Document` / `Asr` / `Lrc`）的候选；与「跨源共现（`sources` 规模 ≥ 2）」
/// 构成**逻辑或**放行判据。
///
/// **口径**：作用于归一化映射之前的原始余弦，**非** `(sim + 1) * 0.5`。
/// 单一事实源见 `docs/specs/text-source-tag-recall-recovery-spec.md` §2.1。
pub const ANCHOR_COSINE_PASS_THRESHOLD: f32 = 0.55;

// ============ 分层回退置信（tag_engine_map 缺分时按引擎回退） ============

/// OCR 事实层回退分
pub const LAYER_FALLBACK_OCR: f32 = 0.99;
/// 物理/互斥/NSFW/画质/RAM 层回退分
pub const LAYER_FALLBACK_PHYSICAL: f32 = 0.90;
/// CLIP 开放集长尾层回退分（= 标定起步分）
pub const LAYER_FALLBACK_CLIP: f32 = 0.55;
/// 合规/画质评级事实层回退分（R-18/全年龄等敏感度槽位）
pub const LAYER_FALLBACK_RATING: f32 = 0.95;

// ============ (WP2b 补遗) 级联 bound_tags 规则层默认分 ============

/// 敏感度槽位规则分（R-18/全年龄/色情）——rating 事实层
pub const BOUND_SENS_CONF: f32 = 0.95;
/// 主体/形态 bound_tags 规则分——physical 层默认
pub const BOUND_SUBJECT_CONF: f32 = 0.92;
/// logicPan 次级 bound 规则分——physical 层次级
pub const BOUND_LOGIC_PAN_CONF: f32 = 0.85;
/// 形态矩阵主档 bound 分（同人本/插画/绘画/截图等 primary form）
pub const BOUND_FORM_PRIMARY_CONF: f32 = 0.95;
/// 形态矩阵次档 bound 分（漫画/写实/手绘/水彩水墨等 secondary form）
pub const BOUND_FORM_SECONDARY_CONF: f32 = 0.90;
/// 写实摄影附属形态分（人像写真/静物照）
pub const BOUND_FORM_PHOTO_SECONDARY_CONF: f32 = 0.88;

/// (WP5) 互斥组胜出判定纯函数。
///
/// 同时满足才胜出：
/// 1. `best_score` 有限且 `>= min_conf`（绝对阈值过线）
/// 2. `best_score - second_score >= margin`（与次优差距足够，杜绝临界噪声）
///
/// 当组内仅有一个有效候选时，`second_score` 传 `f32::NEG_INFINITY`，margin 恒满足。
pub fn resolve_mutual_winner(
    best_score: f32,
    second_score: f32,
    min_conf: f32,
    margin: f32,
) -> bool {
    if !best_score.is_finite() || best_score < min_conf {
        return false;
    }
    if second_score.is_nan() {
        // 次优不存在视为 -∞，仅要求 best 过绝对阈值
        return true;
    }
    if !second_score.is_finite() {
        return true;
    }
    best_score - second_score >= margin
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn winner_requires_absolute_threshold() {
        assert!(!resolve_mutual_winner(
            0.17,
            f32::NEG_INFINITY,
            MUTUAL_GROUP_THRESHOLD_LOOSE,
            MUTUAL_GROUP_MARGIN
        ));
        assert!(resolve_mutual_winner(
            0.18,
            f32::NEG_INFINITY,
            MUTUAL_GROUP_THRESHOLD_LOOSE,
            MUTUAL_GROUP_MARGIN
        ));
    }

    #[test]
    fn winner_requires_margin_against_second() {
        // 第二名接近：0.19 - 0.17 = 0.02 < 0.03 → 不胜出（WP5 核心场景）
        assert!(!resolve_mutual_winner(
            0.19,
            0.17,
            MUTUAL_GROUP_THRESHOLD_LOOSE,
            MUTUAL_GROUP_MARGIN
        ));
        // 差距足够：0.25 - 0.17 = 0.08 >= 0.03 → 胜出
        assert!(resolve_mutual_winner(
            0.25,
            0.17,
            MUTUAL_GROUP_THRESHOLD_LOOSE,
            MUTUAL_GROUP_MARGIN
        ));
    }

    #[test]
    fn winner_with_single_candidate_passes_margin() {
        assert!(resolve_mutual_winner(
            0.30,
            f32::NEG_INFINITY,
            MUTUAL_GROUP_THRESHOLD_LOOSE,
            MUTUAL_GROUP_MARGIN
        ));
        assert!(resolve_mutual_winner(
            0.30,
            f32::NAN,
            MUTUAL_GROUP_THRESHOLD_LOOSE,
            MUTUAL_GROUP_MARGIN
        ));
    }

    #[test]
    fn threshold_constants_are_consistent() {
        assert!(CLIP_ABS_THRESHOLD > 0.0);
        assert!(CLIP_TOP_MARGIN > 0.0);
        assert!(RAM_ONNX_THRESHOLD > RAM_FALLBACK_THRESHOLD);
        assert!(MUTUAL_GROUP_MARGIN > 0.0);
        assert!(EXIT_CONFIDENCE_THRESHOLD > LAYER_FALLBACK_CLIP);
        assert!(LAYER_FALLBACK_OCR > LAYER_FALLBACK_PHYSICAL);
        assert!(LAYER_FALLBACK_PHYSICAL > LAYER_FALLBACK_CLIP);
        assert_eq!(LAYER_FALLBACK_CLIP, CLIP_CALIBRATE_FLOOR);
    }
}
