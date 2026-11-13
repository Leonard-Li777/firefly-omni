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
