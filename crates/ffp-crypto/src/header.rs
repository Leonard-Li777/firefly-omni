use serde::{Deserialize, Serialize};
use crate::error::FfpError;

pub const MAGIC_FFPC: [u8; 4] = *b"FFPC";
pub const MAX_AAD_LEN: usize = 1024;
pub const MAX_CT_LEN: u64 = 4 * 1024 * 1024 * 1024; // 4GiB 单包上限
pub const TAG_LEN: usize = 16;
pub const FIXED_PREFIX_LEN: usize = 24; // Magic(4) + Version(2) + KeyEpoch(2) + Nonce(12) + AAD_Len(4)

/// .ffpc 头部元数据结构体
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
