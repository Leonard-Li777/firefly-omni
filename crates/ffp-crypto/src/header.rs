use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;
use crate::error::FfpError;

pub const MAGIC_FFPC: [u8; 4] = *b"FFPC";
pub const MAX_AAD_LEN: usize = 1024;
pub const MAX_CT_LEN: u64 = 4 * 1024 * 1024 * 1024; // 4GiB 单包上限
pub const MAX_MODEL_BYTES: u64 = 4 * 1024 * 1024 * 1024; // 4GiB 硬上限
pub const TAG_LEN: usize = 16;
pub const FIXED_PREFIX_LEN: usize = 24; // Magic(4) + Version(2) + KeyEpoch(2) + Nonce(12) + AAD_Len(4)

pub const MIN_V2_HEADER_LEN: usize = 116; // 114 + 1 + 1
pub const MAX_V2_HEADER_LEN: usize = 178; // 114 + 32 + 32

/// .ffpc v1 头部元数据结构体
#[derive(Debug, Clone)]
pub struct FfpcHeader {
    pub version: u16,
    pub key_epoch: u16,
    pub nonce: [u8; 12],
    pub aad_len: u32,
    pub ct_len: u64,
}

/// AAD JSON 明文结构体（附加业务认证数据）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AadMetadata {
    pub id_hash: String,
    pub slot: String,
    pub version: String,
    pub key_epoch: u16,
    pub onnx_len: usize,
    #[serde(default)]
    pub tok_len: usize,
}

/// v2 冻结小端二进制 Header 解析结果 (零拷贝引用原始字节流)
#[derive(Debug, Clone)]
pub struct FfpcV2Header<'a> {
    pub format_version: u8,
    pub cipher_suite: u8,
    pub header_len: usize,
    pub resource_id_hash: [u8; 16],
    pub slot: &'a str,
    pub role: &'a str,
    pub key_epoch: u32,
    pub plaintext_len: u64,
    pub plaintext_sha256: [u8; 32],
    pub siblings_digest: [u8; 32],
    pub nonce: [u8; 12],
    pub aad: &'a [u8],
}

/// 伴随件条目结构体 (RFC 8785 JCS 规范化)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SiblingEntry {
    pub file: String,
    pub sha256: String,
    pub size: u64,
}

/// 校验伴随件原始 JSON 字符串或字节流的所有键名必须严格为 ASCII 且仅限合法键
pub fn validate_sibling_keys(json_bytes: &[u8]) -> Result<(), FfpError> {
    let value: serde_json::Value = serde_json::from_slice(json_bytes)
        .map_err(|e| FfpError::InvalidFormat(format!("伴随件不是合法 JSON: {}", e)))?;

    fn check_value(v: &serde_json::Value) -> Result<(), FfpError> {
        match v {
            serde_json::Value::Object(map) => {
                for (k, val) in map {
                    if !k.is_ascii() {
                        return Err(FfpError::NonAsciiSiblingKey(k.clone()));
                    }
                    if k != "file" && k != "sha256" && k != "size" {
                        return Err(FfpError::InvalidFormat(format!(
                            "伴随件包含非预期字段: {}",
                            k
                        )));
                    }
                    check_value(val)?;
                }
            }
            serde_json::Value::Array(arr) => {
                for item in arr {
                    check_value(item)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    check_value(&value)
}

/// 按照 RFC 8785 (JCS) 规则生成规范化 JSON 字节切片并计算 SHA-256 摘要
pub fn compute_canonical_siblings_digest(siblings: &[SiblingEntry]) -> Result<[u8; 32], FfpError> {
    if siblings.is_empty() {
        let mut hasher = Sha256::new();
        hasher.update(b"[]");
        return Ok(hasher.finalize().into());
    }

    // 1. NFC 归一化 file，sha256 转小写
    let mut normalized: Vec<SiblingEntry> = siblings
        .iter()
        .map(|s| SiblingEntry {
            file: s.file.nfc().collect::<String>(),
            sha256: s.sha256.to_ascii_lowercase(),
            size: s.size,
        })
        .collect();

    // 2. 数组内元素严格按照 file 字段的 Unicode 码点 (UTF-8 字节序) 升序排序
    normalized.sort_by(|a, b| a.file.cmp(&b.file));

    // 3. 构建规范 JCS 紧凑格式：无空格，键名固定顺序 "file" -> "sha256" -> "size"
    let mut items = Vec::new();
    for entry in &normalized {
        let file_escaped = serde_json::to_string(&entry.file)
            .map_err(|e| FfpError::InvalidFormat(format!("序列化 file 失败: {}", e)))?;
        let sha256_escaped = serde_json::to_string(&entry.sha256)
            .map_err(|e| FfpError::InvalidFormat(format!("序列化 sha256 失败: {}", e)))?;
        items.push(format!(
            "{{\"file\":{},\"sha256\":{},\"size\":{}}}",
            file_escaped, sha256_escaped, entry.size
        ));
    }

    let jcs_json = format!("[{}]", items.join(","));
    let mut hasher = Sha256::new();
    hasher.update(jcs_json.as_bytes());
    Ok(hasher.finalize().into())
}

/// 校验伴随件摘要是否与预期吻合
pub fn verify_siblings_digest(
    siblings: &[SiblingEntry],
    expected_digest: &[u8; 32],
) -> Result<(), FfpError> {
    let actual_digest = compute_canonical_siblings_digest(siblings)?;
    if actual_digest != *expected_digest {
        return Err(FfpError::SiblingsDigestMismatch {
            declared: hex::encode(expected_digest),
            actual: hex::encode(actual_digest),
        });
    }
    Ok(())
}

/// 构建合法的 v2 二进制 Header 字节流 (供测试与向量验证使用)
pub fn build_v2_header_bytes(
    resource_id_hash: &[u8; 16],
    slot: &str,
    role: &str,
    key_epoch: u32,
    plaintext_len: u64,
    plaintext_sha256: &[u8; 32],
    siblings_digest: &[u8; 32],
    nonce: &[u8; 12],
) -> Result<Vec<u8>, FfpError> {
    let slot_bytes = slot.as_bytes();
    if slot_bytes.is_empty() || slot_bytes.len() > 32 {
        return Err(FfpError::InvalidSlotLength);
    }
    let role_bytes = role.as_bytes();
    if role_bytes.is_empty() || role_bytes.len() > 32 {
        return Err(FfpError::InvalidRoleLength);
    }

    let expected_header_len = 114 + slot_bytes.len() + role_bytes.len();
    if expected_header_len < MIN_V2_HEADER_LEN || expected_header_len > MAX_V2_HEADER_LEN {
        return Err(FfpError::InvalidHeaderLength(expected_header_len));
    }

    let mut buf = Vec::with_capacity(expected_header_len);
    buf.extend_from_slice(&MAGIC_FFPC); // 0..4
    buf.push(2u8); // 4: formatVersion = 2
    buf.push(1u8); // 5: cipherSuite = 1
    buf.extend_from_slice(&(expected_header_len as u16).to_le_bytes()); // 6..8: headerLen
    buf.extend_from_slice(resource_id_hash); // 8..24: resourceIdHash (16B)
    buf.push(slot_bytes.len() as u8); // 24: slotLen
    buf.extend_from_slice(slot_bytes); // 25..25+Ls: slot
    buf.push(role_bytes.len() as u8); // 25+Ls: roleLen
    buf.extend_from_slice(role_bytes); // 26+Ls..26+Ls+Lr: role
    buf.extend_from_slice(&key_epoch.to_le_bytes()); // keyEpoch (4B)
    buf.extend_from_slice(&plaintext_len.to_le_bytes()); // plaintextLen (8B)
    buf.extend_from_slice(plaintext_sha256); // plaintextSha256 (32B)
    buf.extend_from_slice(siblings_digest); // siblingsDigest (32B)
    buf.extend_from_slice(nonce); // nonce (12B)

    debug_assert_eq!(buf.len(), expected_header_len);
    Ok(buf)
}

/// 解析 v2 冻结小端二进制 Header 并在解密前执行全部严格前置边界门禁
pub fn parse_v2_header<'a>(bytes: &'a [u8]) -> Result<FfpcV2Header<'a>, FfpError> {
    // 1. [N-01] 细粒度长度与 Magic 精确分类断言 (诊断友好型)
    if bytes.len() < 4 {
        return Err(FfpError::TruncatedHeader);
    }
    if bytes[0..4] != MAGIC_FFPC {
        return Err(FfpError::InvalidMagic);
    }
    if bytes.len() < MIN_V2_HEADER_LEN {
        return Err(FfpError::TruncatedHeader);
    }

    let format_version = bytes[4];
    if format_version != 2 {
        return Err(FfpError::UnsupportedVersion(format_version as u16));
    }
    let cipher_suite = bytes[5];
    if cipher_suite != 1 {
        return Err(FfpError::UnsupportedCipherSuite(cipher_suite));
    }

    // 2. [Q-01 / R3-02] headerLen 粗筛边界校验 (下界精确为 116, 上界 178)
    let header_len = u16::from_le_bytes([bytes[6], bytes[7]]) as usize;
    if header_len < MIN_V2_HEADER_LEN || header_len > MAX_V2_HEADER_LEN {
        return Err(FfpError::InvalidHeaderLength(header_len));
    }
    if bytes.len() < header_len + TAG_LEN {
        return Err(FfpError::TruncatedHeader);
    }

    // 3. [R3-01] slotLen 与 roleLen 提取及 headerLen 强自洽断言 (必须先于后续字段定位)
    let slot_len = bytes[24] as usize;
    if slot_len < 1 || slot_len > 32 {
        return Err(FfpError::InvalidSlotLength);
    }
    let role_len_offset = 25 + slot_len;
    // slot_len <= 32 时，role_len_offset <= 57 < 116 <= header_len <= bytes.len()，访问必定安全
    let role_len = bytes[role_len_offset] as usize;
    if role_len < 1 || role_len > 32 {
        return Err(FfpError::InvalidRoleLength);
    }

    // 强自洽等式校验：headerLen 必须严格等于 114 + slotLen + roleLen
    let expected_header_len = 114 + slot_len + role_len;
    if header_len != expected_header_len {
        return Err(FfpError::HeaderLengthMismatch {
            declared: header_len,
            expected: expected_header_len,
        });
    }

    // slot 与 role 字符串 UTF-8 / ASCII 提取
    let slot_bytes = &bytes[25..25 + slot_len];
    let slot = std::str::from_utf8(slot_bytes)
        .map_err(|_| FfpError::InvalidFormat("slot 不是有效的 UTF-8 字符串".into()))?;

    let role_start = role_len_offset + 1;
    let role_end = role_start + role_len;
    let role_bytes = &bytes[role_start..role_end];
    let role = std::str::from_utf8(role_bytes)
        .map_err(|_| FfpError::InvalidFormat("role 不是有效的 UTF-8 字符串".into()))?;

    // 4. [Q-02 / R3-03 / N-02] 解析并校验明文长度 (严格等长，彻底杜绝尾部垃圾注入)
    let pt_len_offset = 30 + slot_len + role_len;
    let plaintext_len = u64::from_le_bytes(
        bytes[pt_len_offset..pt_len_offset + 8]
            .try_into()
            .unwrap(),
    );
    let ct_len = (bytes.len() - header_len - TAG_LEN) as u64;

    if plaintext_len == 0 || plaintext_len != ct_len {
        return Err(FfpError::CiphertextLengthMismatch {
            declared: plaintext_len,
            actual: ct_len,
        });
    }
    if plaintext_len > MAX_MODEL_BYTES {
        return Err(FfpError::ModelTooLarge(plaintext_len));
    }

    let mut resource_id_hash = [0u8; 16];
    resource_id_hash.copy_from_slice(&bytes[8..24]);

    let key_epoch_offset = 26 + slot_len + role_len;
    let key_epoch = u32::from_le_bytes(
        bytes[key_epoch_offset..key_epoch_offset + 4]
            .try_into()
            .unwrap(),
    );

    let pt_sha_offset = 38 + slot_len + role_len;
    let mut plaintext_sha256 = [0u8; 32];
    plaintext_sha256.copy_from_slice(&bytes[pt_sha_offset..pt_sha_offset + 32]);

    let sib_digest_offset = 70 + slot_len + role_len;
    let mut siblings_digest = [0u8; 32];
    siblings_digest.copy_from_slice(&bytes[sib_digest_offset..sib_digest_offset + 32]);

    let nonce_offset = 102 + slot_len + role_len;
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&bytes[nonce_offset..nonce_offset + 12]);

    // 5. 通过全部前置门禁，直接零拷贝切出 AAD
    let aad = &bytes[0..header_len];

    Ok(FfpcV2Header {
        format_version,
        cipher_suite,
        header_len,
        resource_id_hash,
        slot,
        role,
        key_epoch,
        plaintext_len,
        plaintext_sha256,
        siblings_digest,
        nonce,
        aad,
    })
}

impl FfpcHeader {
    /// 校验定长头部并解析基础参数
    pub fn parse_prefix(bytes: &[u8]) -> Result<(Self, usize), FfpError> {
        if bytes.len() < FIXED_PREFIX_LEN {
            return Err(FfpError::InvalidFormat("文件长度小于最小头部长度 24 字节".into()));
        }

        if bytes[0..4] != MAGIC_FFPC {
            return Err(FfpError::InvalidMagic);
        }

        let version = u16::from_le_bytes([bytes[4], bytes[5]]);
        if version != 1 {
            return Err(FfpError::UnsupportedVersion(version));
        }

        let key_epoch = u16::from_le_bytes([bytes[6], bytes[7]]);
        let mut nonce = [0u8; 12];
        nonce.copy_from_slice(&bytes[8..20]);

        let aad_len = u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        if (aad_len as usize) > MAX_AAD_LEN {
            return Err(FfpError::InvalidFormat(format!(
                "AAD 长度超标: {} > {}",
                aad_len, MAX_AAD_LEN
            )));
        }

        let aad_len_usize = aad_len as usize;
        let expected_min_len = FIXED_PREFIX_LEN + aad_len_usize + 8 + TAG_LEN;
        if bytes.len() < expected_min_len {
            return Err(FfpError::InvalidFormat(format!(
                "文件总长度不足以包含头部与Tag: {} < {}",
                bytes.len(),
                expected_min_len
            )));
        }

        // 读取 ct_len (偏移 24 + aad_len)
        let ct_offset = FIXED_PREFIX_LEN + aad_len_usize;
        let ct_len = u64::from_le_bytes([
            bytes[ct_offset],
            bytes[ct_offset + 1],
            bytes[ct_offset + 2],
            bytes[ct_offset + 3],
            bytes[ct_offset + 4],
            bytes[ct_offset + 5],
            bytes[ct_offset + 6],
            bytes[ct_offset + 7],
        ]);

        if ct_len > MAX_CT_LEN {
            return Err(FfpError::InvalidFormat(format!(
                "密文长度超过 4GiB 上限: {}",
                ct_len
            )));
        }

        // 全文件总长度精确等于: 32 + aad_len + ct_len + 16
        let expected_total_len = (FIXED_PREFIX_LEN + 8 + aad_len_usize) as u64 + ct_len + (TAG_LEN as u64);
        if (bytes.len() as u64) != expected_total_len {
            return Err(FfpError::InvalidFormat(format!(
                "文件长度与头部声明不匹配: 实际 {}, 预期 {} (禁止尾随垃圾数据)",
                bytes.len(),
                expected_total_len
            )));
        }

        let header = FfpcHeader {
            version,
            key_epoch,
            nonce,
            aad_len,
            ct_len,
        };

        Ok((header, ct_offset + 8))
    }
}
