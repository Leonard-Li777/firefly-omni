use std::path::Path;
use chacha20poly1305::{
    aead::{AeadInPlace, KeyInit},
    ChaCha20Poly1305, Nonce, Tag,
};
use zeroize::Zeroizing;

use crate::{
    error::FfpError,
    header::{AadMetadata, FfpcHeader, FIXED_PREFIX_LEN, TAG_LEN},
    key::{derive_app_key, derive_sub_key, LOCAL_KEY_EPOCH},
    payload::DecryptedPayload,
};

/// 从加密的 .ffpc 复合流容器中解密载荷，并严格校验全头部与 AAD。
pub fn decrypt_ffpc_payload(
    ffpc_path: &Path,
    expected_id_hash: &str,
    expected_slot: &str,
    expected_version: &str,
) -> Result<DecryptedPayload, FfpError> {
    let file_bytes = std::fs::read(ffpc_path)?;
    decrypt_ffpc_bytes(&file_bytes, expected_id_hash, expected_slot, expected_version)
}

/// 核心内存字节解密实现（支持单测与流式复用）
pub fn decrypt_ffpc_bytes(
    file_bytes: &[u8],
    expected_id_hash: &str,
    expected_slot: &str,
    expected_version: &str,
) -> Result<DecryptedPayload, FfpError> {
    // 1. 解析定长头部与长度
    let (header, ct_start) = FfpcHeader::parse_prefix(file_bytes)?;

    // 2. 严格核验代次（ALT-P1 立即硬失败）
    if header.key_epoch != LOCAL_KEY_EPOCH {
        return Err(FfpError::KeyEpochMismatch(header.key_epoch, LOCAL_KEY_EPOCH));
    }

    // 3. 构造全头部切片作为 AEAD 的 AAD: file_bytes[0 .. 32+aad_len]
    let aad_full_header = &file_bytes[..ct_start];

    // 4. 派生专属子密钥
    let master_key = derive_app_key();
    let sub_key = derive_sub_key(&master_key, expected_id_hash, expected_slot);

    // 5. 准备密文单缓冲与 Tag
    let ct_len = header.ct_len as usize;
    let ct_end = ct_start + ct_len;
    let auth_tag_bytes = &file_bytes[ct_end..ct_end + TAG_LEN];

    let mut decrypted_storage = Zeroizing::new(file_bytes[ct_start..ct_end].to_vec());

    // 6. 执行 ChaCha20-Poly1305 原地解密与认证
    let cipher = ChaCha20Poly1305::new_from_slice(&sub_key)
        .map_err(|_| FfpError::InvalidFormat("初始化 ChaCha20Poly1305 失败".into()))?;
    let nonce = Nonce::from_slice(&header.nonce);
    let tag = Tag::from_slice(auth_tag_bytes);

    cipher
        .decrypt_in_place_detached(nonce, aad_full_header, &mut decrypted_storage, tag)
        .map_err(|_| FfpError::AeadDecryptFailed)?;

    // 7. 解密成功后，解析明文 AAD JSON 并核对业务契约
    let aad_json_offset = FIXED_PREFIX_LEN;
    let aad_json_bytes = &file_bytes[aad_json_offset..aad_json_offset + (header.aad_len as usize)];
    let aad: AadMetadata = serde_json::from_slice(aad_json_bytes).map_err(|e| {
        FfpError::InvalidFormat(format!("解析 AAD JSON 失败: {}", e))
    })?;

    if aad.key_epoch != header.key_epoch {
        return Err(FfpError::InvalidFormat(format!(
            "AAD keyEpoch {} 与头部代次 {} 不一致",
            aad.key_epoch, header.key_epoch
        )));
    }

    if aad.id_hash != expected_id_hash
        || aad.slot != expected_slot
        || aad.version != expected_version
    {
        return Err(FfpError::AadMismatch {
            expected_id_hash: expected_id_hash.to_string(),
            expected_slot: expected_slot.to_string(),
            expected_version: expected_version.to_string(),
            actual_id_hash: aad.id_hash,
            actual_slot: aad.slot,
            actual_version: aad.version,
        });
    }

    if aad.onnx_len + aad.tok_len != ct_len {
        return Err(FfpError::InvalidFormat(format!(
            "AAD onnxLen({}) + tokLen({}) != ctLen({})",
            aad.onnx_len, aad.tok_len, ct_len
        )));
    }

    Ok(DecryptedPayload::new(
        decrypted_storage,
        aad.onnx_len,
        aad.tok_len,
    ))
}

/// 从 v2 密文容器流中安全解密单个角色流明文 (规格书 §3.2 / §3.3)
///
/// 校验步骤：
/// 1. 解析 v2 Header 并执行全部前置边界门禁 (Magic, 版本, 长度粗筛, slotLen/roleLen 强自洽断言)；
/// 2. 校验文件总长度是否严格等于 headerLen + plaintextLen + 16 (禁止尾随垃圾追加, N-02)；
/// 3. 校验密文载荷长度严格等于 header.plaintext_len；
/// 4. 派生专属 v2 AEAD 子密钥；
/// 5. 原地解密并认证 Tag，使用 Header 原始字节切片作为 AAD；
/// 6. 解密后计算明文 SHA-256，强断言与 header.plaintext_sha256 100% 吻合；
/// 7. 返回解密明文字节 (被 Zeroizing 保护)。
pub fn decrypt_v2_stream(
    file_bytes: &[u8],
    master_key: &[u8; 32],
) -> Result<Zeroizing<Vec<u8>>, FfpError> {
    use sha2::{Digest, Sha256};
    use crate::header::parse_v2_header;
    use crate::key::derive_v2_aead_sub_key;

    let header = parse_v2_header(file_bytes)?;

    let header_len = header.header_len;
    let pt_len = header.plaintext_len as usize;
    let expected_total_len = header_len + pt_len + TAG_LEN;

    if file_bytes.len() != expected_total_len {
        return Err(FfpError::CiphertextLengthMismatch {
            declared: header.plaintext_len,
            actual: (file_bytes.len().saturating_sub(header_len + TAG_LEN)) as u64,
        });
    }

    let ct_start = header_len;
    let ct_end = ct_start + pt_len;
    let tag_bytes = &file_bytes[ct_end..ct_end + TAG_LEN];

    let sub_key = derive_v2_aead_sub_key(
        master_key,
        &header.resource_id_hash,
        header.slot,
        header.role,
        header.key_epoch,
    );

    let cipher = ChaCha20Poly1305::new_from_slice(&sub_key)
        .map_err(|_| FfpError::InvalidFormat("初始化 ChaCha20Poly1305 失败".into()))?;

    let nonce = Nonce::from_slice(&header.nonce);
    let tag = Tag::from_slice(tag_bytes);

    let mut decrypted_storage = Zeroizing::new(file_bytes[ct_start..ct_end].to_vec());

    cipher
        .decrypt_in_place_detached(nonce, header.aad, &mut decrypted_storage, tag)
        .map_err(|_| FfpError::AeadDecryptFailed)?;

    // 解密后明文 SHA-256 完整性强断言
    let mut hasher = Sha256::new();
    hasher.update(&decrypted_storage);
    let actual_sha: [u8; 32] = hasher.finalize().into();

    if actual_sha != header.plaintext_sha256 {
        return Err(FfpError::InvalidFormat("明文 SHA-256 摘要不匹配".into()));
    }

    Ok(decrypted_storage)
}
