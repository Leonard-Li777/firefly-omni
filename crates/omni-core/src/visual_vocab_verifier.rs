//! 盲化同源视觉矩阵资产 (`vocab_v.{f16,meta.json}`) 只读同源校验器。
//!
//! 对齐 `visual-tagger-model-provisioning-spec.md` v2.11 §3.1（A/B 段核心规则）与
//! `multi-product-resource-placement-and-assembly-spec.md` §2.4：
//! 1. 严禁 `mmap`（一次性读入堆缓冲后立刻关句柄，确保 Windows 不锁盘）；
//! 2. A 段先读 `vocab_v.meta.json` 校验 `schema_version == 2`（!=2 返 409 incompatible）；
//! 3. B 段校验 `vocab_v.f16` 字节长度、实算 `sha256(f16_buf) == matrix_sha256`、`sibling_nms`、
//!    `entries[0].code == "builtin.landscape_photos"`、`entries[].code` 唯一性、无 NaN/Inf、
//!    逐行 L2 模长 ∈ [0.99, 1.01] 及居中中心化 `image_center` 约束；
//! 4. 旁路残留 `vocab_v.bin` 不视作损坏。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::path::Path;

pub const VISUAL_VOCAB_SCHEMA_VERSION: u32 = 2;
pub const EXPECTED_FIRST_ENTRY_CODE: &str = "builtin.landscape_photos";
pub const EXPECTED_EMBED_DIM: usize = 512;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VisualVocabVerifyReport {
    pub ok: bool,
    pub http_status: u16,
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embed_dim: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matrix_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta_sha256: Option<String>,
}

impl VisualVocabVerifyReport {
    fn missing(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            http_status: 500,
            code: "ASSET_MISSING".to_string(),
            reason: None,
            message: msg.into(),
            entry_count: None,
            embed_dim: None,
            matrix_sha256: None,
            meta_sha256: None,
        }
    }

    fn corrupt(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            http_status: 500,
            code: "ASSET_CORRUPT".to_string(),
            reason: None,
            message: msg.into(),
            entry_count: None,
            embed_dim: None,
            matrix_sha256: None,
            meta_sha256: None,
        }
    }

    fn incompatible(reason: &'static str, msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            http_status: 409,
            code: "ENGINE_NOT_READY".to_string(),
            reason: Some(reason.to_string()),
            message: msg.into(),
            entry_count: None,
            embed_dim: None,
            matrix_sha256: None,
            meta_sha256: None,
        }
    }
}

/// 纯整数位运算将 IEEE 754 binary16 解码为 `f32`（零第三方 crate 依赖）
#[inline]
pub fn f16_to_f32(bits: u16) -> f32 {
    let sign = ((bits as u32) & 0x8000) << 16;
    let exp = ((bits as u32) >> 10) & 0x1F;
    let mant = (bits as u32) & 0x03FF;

    let f32_bits = if exp == 0 {
        if mant == 0 {
            sign
        } else {
            // 次正规数归一化
            let mut m = mant;
            let mut e = 127 - 15 + 1;
            while (m & 0x0400) == 0 {
                m <<= 1;
                e -= 1;
            }
            m &= 0x03FF;
            sign | ((e as u32) << 23) | (m << 13)
        }
    } else if exp == 31 {
        sign | 0x7F80_0000 | (mant << 13)
    } else {
        let new_exp = exp + (127 - 15);
        sign | (new_exp << 23) | (mant << 13)
    };
    f32::from_bits(f32_bits)
}

#[derive(Deserialize)]
struct VocabEntryRaw {
    code: String,
    threshold: f64,
    source_kind: String,
    #[serde(default)]
    #[allow(dead_code)]
    mutual_group: Option<String>,
    #[allow(dead_code)]
    allowed_domains_mask: u16,
}

#[derive(Deserialize)]
struct SiblingNmsRaw {
    tau_conf: f64,
    max_siblings_k: serde_json::Value,
    delta_sib: f64,
}

#[derive(Deserialize)]
struct CalibrateRaw {
    floor: f64,
    cap: f64,
    abs_floor: f64,
    slope: f64,
}

#[derive(Deserialize)]
struct VocabMetaV2Raw {
    schema_version: u32,
    #[allow(dead_code)]
    version: String,
    bake_revision: u64,
    encoder_id: u8,
    encoder_digest: String,
    embed_dim: usize,
    pooling_contract: String,
    domain_order_version: u32,
    domain_matrix_sha256: String,
    matrix_file: String,
    matrix_sha256: String,
    #[serde(default)]
    image_center: Option<Vec<f64>>,
    #[serde(default)]
    image_center_contract: Option<String>,
    calibrate: CalibrateRaw,
    sibling_nms: SiblingNmsRaw,
    entries: Vec<VocabEntryRaw>,
}

/// 同源只读校验目录内的 `vocab_v.{f16,meta.json}`
pub fn verify_visual_vocab_dir(dir: &Path) -> VisualVocabVerifyReport {
    let meta_path = dir.join("vocab_v.meta.json");
    if !meta_path.is_file() {
        return VisualVocabVerifyReport::missing(format!(
            "缺失视觉矩阵元数据文件: {}",
            meta_path.display()
        ));
    }

    // 单次读入内存堆缓冲后立即关闭句柄，禁止 mmap
    let meta_bytes = match fs::read(&meta_path) {
        Ok(b) => b,
        Err(err) => {
            return VisualVocabVerifyReport::corrupt(format!(
                "读取 vocab_v.meta.json 失败: {err}"
            ))
        }
    };
    let meta_sha256 = hex::encode(Sha256::digest(&meta_bytes));

    // A 段：`schema_version` 快速闸门（仅解析 schema_version，!=2 立即短路返 409）
    let probe_val: serde_json::Value = match serde_json::from_slice(&meta_bytes) {
        Ok(v) => v,
        Err(err) => {
            return VisualVocabVerifyReport::corrupt(format!(
                "vocab_v.meta.json JSON 语法损坏: {err}"
            ))
        }
    };
    let schema_ver = match probe_val.get("schema_version").and_then(|v| v.as_u64()) {
        Some(v) => v,
        None => {
            return VisualVocabVerifyReport::corrupt(
                "vocab_v.meta.json 缺少整数型 schema_version",
            )
        }
    };
    if schema_ver < VISUAL_VOCAB_SCHEMA_VERSION as u64 {
        return VisualVocabVerifyReport::incompatible(
            "visual_vocab_stale",
            format!("vocab_v schema_version={schema_ver} 陈旧，需要升级"),
        );
    }
    if schema_ver > VISUAL_VOCAB_SCHEMA_VERSION as u64 {
        return VisualVocabVerifyReport::incompatible(
            "app_update_needed",
            format!("vocab_v schema_version={schema_ver} 高于当前支持版本"),
        );
    }

    // B 段：v2 结构与数值自洽校验
    let meta: VocabMetaV2Raw = match serde_json::from_value(probe_val) {
        Ok(m) => m,
        Err(err) => {
            return VisualVocabVerifyReport::corrupt(format!(
                "vocab_v.meta.json v2 必填字段缺失或类型非法: {err}"
            ))
        }
    };
    let _ = (meta.schema_version, meta.bake_revision, meta.encoder_id);

    if meta.embed_dim != EXPECTED_EMBED_DIM {
        return VisualVocabVerifyReport::corrupt(format!(
            "embed_dim 非法: 期望 {}, 实际 {}",
            EXPECTED_EMBED_DIM, meta.embed_dim
        ));
    }
    if meta.matrix_file != "vocab_v.f16" {
        return VisualVocabVerifyReport::corrupt(format!(
            "matrix_file 必须为 vocab_v.f16，实际 {}",
            meta.matrix_file
        ));
    }
    if meta.encoder_digest.trim().is_empty() || meta.domain_matrix_sha256.trim().is_empty() {
        return VisualVocabVerifyReport::corrupt("encoder_digest 或 domain_matrix_sha256 为空");
    }
    if meta.domain_order_version != 2 {
        return VisualVocabVerifyReport::corrupt(format!(
            "domain_order_version 必须为 2，实际 {}",
            meta.domain_order_version
        ));
    }

    // calibrate & sibling_nms 数值护栏
    if ![
        meta.calibrate.floor,
        meta.calibrate.cap,
        meta.calibrate.abs_floor,
        meta.calibrate.slope,
    ]
    .into_iter()
    .all(f64::is_finite)
        || meta.calibrate.floor > meta.calibrate.cap
    {
        return VisualVocabVerifyReport::corrupt("calibrate 校准参数包含非有限值或上下界倒挂");
    }

    let tau_conf = meta.sibling_nms.tau_conf;
    let delta_sib = meta.sibling_nms.delta_sib;
    let k_valid = meta
        .sibling_nms
        .max_siblings_k
        .as_u64()
        .is_some_and(|k| k >= 1);
    if !tau_conf.is_finite()
        || tau_conf <= 0.0
        || tau_conf > 1.0
        || !k_valid
        || !delta_sib.is_finite()
        || delta_sib < 0.0
    {
        return VisualVocabVerifyReport::corrupt(
            "sibling_nms 参数越界（要求 tau_conf∈(0,1], max_siblings_k>=1 整数, delta_sib>=0）",
        );
    }

    // 中心化约束检查
    if meta.pooling_contract.contains("_centered_") {
        if meta.image_center_contract.as_deref() != Some("l2_after_slice_512") {
            return VisualVocabVerifyReport::corrupt(
                "中心化 pooling_contract 缺少有效的 image_center_contract=l2_after_slice_512",
            );
        }
        match &meta.image_center {
            Some(vec) if vec.len() == EXPECTED_EMBED_DIM && vec.iter().all(|v| v.is_finite()) => {}
            _ => {
                return VisualVocabVerifyReport::corrupt(
                    "中心化 image_center 必须为 512 个有限浮点",
                )
            }
        }
    }

    // entries 基本结构与唯一性检查
    if meta.entries.is_empty() {
        return VisualVocabVerifyReport::corrupt("entries 列表不能为空");
    }
    if meta.entries[0].code != EXPECTED_FIRST_ENTRY_CODE {
        return VisualVocabVerifyReport::corrupt(format!(
            "entries[0].code 必须为 {}, 实际 {}",
            EXPECTED_FIRST_ENTRY_CODE, meta.entries[0].code
        ));
    }
    let mut seen_codes = HashSet::with_capacity(meta.entries.len());
    for (idx, entry) in meta.entries.iter().enumerate() {
        if entry.code.trim().is_empty() || !seen_codes.insert(entry.code.as_str()) {
            return VisualVocabVerifyReport::corrupt(format!(
                "entries[{idx}] code 为空或重复: {}",
                entry.code
            ));
        }
        if !entry.threshold.is_finite() {
            return VisualVocabVerifyReport::corrupt(format!(
                "entries[{idx}] ({}) threshold 非有限值",
                entry.code
            ));
        }
        if entry.source_kind != "system_dimension" && entry.source_kind != "fine_grained_entity" {
            return VisualVocabVerifyReport::corrupt(format!(
                "entries[{idx}] source_kind 非法: {}",
                entry.source_kind
            ));
        }
    }

    // 校验 vocab_v.f16（禁止 mmap，整块读入堆缓冲并关闭句柄）
    let f16_path = dir.join("vocab_v.f16");
    if !f16_path.is_file() {
        return VisualVocabVerifyReport::missing(format!(
            "缺失视觉矩阵权重文件: {}",
            f16_path.display()
        ));
    }
    let f16_bytes = match fs::read(&f16_path) {
        Ok(b) => b,
        Err(err) => {
            return VisualVocabVerifyReport::corrupt(format!("读取 vocab_v.f16 失败: {err}"))
        }
    };
    let expected_byte_len = meta.entries.len() * meta.embed_dim * 2;
    if f16_bytes.len() != expected_byte_len {
        return VisualVocabVerifyReport::corrupt(format!(
            "vocab_v.f16 字节长度不匹配: 期望 {expected_byte_len}, 实际 {}",
            f16_bytes.len()
        ));
    }
    let actual_matrix_sha256 = hex::encode(Sha256::digest(&f16_bytes));
    if !actual_matrix_sha256.eq_ignore_ascii_case(&meta.matrix_sha256) {
        return VisualVocabVerifyReport::corrupt(format!(
            "vocab_v.f16 SHA-256 失配: 期望 {}, 实际 {actual_matrix_sha256}",
            meta.matrix_sha256
        ));
    }

    // 逐行反量化与 L2 模长检查
    let row_bytes = meta.embed_dim * 2;
    for (row_idx, chunk) in f16_bytes.chunks_exact(row_bytes).enumerate() {
        let mut norm_sq: f64 = 0.0;
        for pair in chunk.chunks_exact(2) {
            let v = f16_to_f32(u16::from_le_bytes([pair[0], pair[1]]));
            if !v.is_finite() {
                return VisualVocabVerifyReport::corrupt(format!(
                    "vocab_v.f16 第 {row_idx} 行包含 NaN/Inf"
                ));
            }
            norm_sq += (v as f64) * (v as f64);
        }
        let norm = norm_sq.sqrt();
        if !(0.99..=1.01).contains(&norm) {
            return VisualVocabVerifyReport::corrupt(format!(
                "vocab_v.f16 第 {row_idx} 行 L2 模长越界: {norm:.4}（要求 [0.99, 1.01]）"
            ));
        }
    }

    VisualVocabVerifyReport {
        ok: true,
        http_status: 200,
        code: "OK".to_string(),
        reason: None,
        message: "vocab_v.{f16,meta.json} 同源校验通过".to_string(),
        entry_count: Some(meta.entries.len()),
        embed_dim: Some(meta.embed_dim),
        matrix_sha256: Some(actual_matrix_sha256),
        meta_sha256: Some(meta_sha256),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn make_tmp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("omni_vv_{nanos}_{tag}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn f32_one_hot_512() -> Vec<u8> {
        // f16 1.0 == 0x3C00, 0.0 == 0x0000
        let mut bytes = Vec::with_capacity(512 * 2);
        bytes.extend_from_slice(&0x3C00u16.to_le_bytes());
        for _ in 1..512 {
            bytes.extend_from_slice(&0x0000u16.to_le_bytes());
        }
        bytes
    }

    fn write_valid_fixture(dir: &Path) {
        let f16_bytes = f32_one_hot_512();
        let matrix_sha = hex::encode(Sha256::digest(&f16_bytes));
        fs::write(dir.join("vocab_v.f16"), &f16_bytes).unwrap();
        let meta = serde_json::json!({
            "version": "2.0.0",
            "schema_version": 2,
            "matrix_kind": "text_proto",
            "bake_revision": 1,
            "encoder_id": 4,
            "encoder_digest": "0123456789abcdef",
            "embed_dim": 512,
            "pooling_contract": "multimodal_proj_head_mrl512_l2_v1",
            "domain_order_version": 2,
            "domain_matrix_sha256": "a".repeat(64),
            "matrix_file": "vocab_v.f16",
            "matrix_sha256": matrix_sha,
            "image_center": null,
            "image_center_contract": null,
            "calibrate": {
                "floor": 0.60,
                "cap": 0.95,
                "abs_floor": 0.25,
                "slope": 2.0,
                "top_margin": 0.065
            },
            "sibling_nms": {
                "tau_conf": 0.85,
                "max_siblings_k": 2,
                "delta_sib": 0.05
            },
            "entries": [{
                "code": "builtin.landscape_photos",
                "threshold": 0.29,
                "source_kind": "system_dimension",
                "mutual_group": "photo_genre",
                "allowed_domains_mask": 4097
            }]
        });
        fs::write(dir.join("vocab_v.meta.json"), serde_json::to_string_pretty(&meta).unwrap()).unwrap();
    }

    #[test]
    fn verify_visual_vocab_passes_and_ignores_legacy_bin() {
        let dir = make_tmp_dir("ok");
        write_valid_fixture(&dir);
        fs::write(dir.join("vocab_v.bin"), b"stale-bin-residue").unwrap();
        let res = verify_visual_vocab_dir(&dir);
        assert!(res.ok, "{}", res.message);
        assert_eq!(res.http_status, 200);
        assert_eq!(res.entry_count, Some(1));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_visual_vocab_returns_409_on_old_schema_and_500_on_corrupt_or_missing() {
        let dir = make_tmp_dir("errors");
        let missing = verify_visual_vocab_dir(&dir);
        assert!(!missing.ok);
        assert_eq!(missing.http_status, 500);
        assert_eq!(missing.code, "ASSET_MISSING");

        // schema_version=1 短路返回 409
        fs::write(dir.join("vocab_v.meta.json"), r#"{"schema_version": 1}"#).unwrap();
        let stale = verify_visual_vocab_dir(&dir);
        assert!(!stale.ok);
        assert_eq!(stale.http_status, 409);
        assert_eq!(stale.reason.as_deref(), Some("visual_vocab_stale"));

        // 篡改 f16 触发 500 ASSET_CORRUPT
        write_valid_fixture(&dir);
        let mut bad_f16 = fs::read(dir.join("vocab_v.f16")).unwrap();
        bad_f16[0] ^= 0xFF;
        fs::write(dir.join("vocab_v.f16"), bad_f16).unwrap();
        let corrupt = verify_visual_vocab_dir(&dir);
        assert!(!corrupt.ok);
        assert_eq!(corrupt.http_status, 500);
        assert_eq!(corrupt.code, "ASSET_CORRUPT");

        let _ = fs::remove_dir_all(&dir);
    }
}
