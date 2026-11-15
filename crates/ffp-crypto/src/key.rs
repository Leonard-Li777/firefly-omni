use hkdf::Hkdf;
use sha2::Sha256;

include!(concat!(env!("OUT_DIR"), "/key_frags.rs"));

/// 从 4 个独立编译期分片还原运行时主密钥（32 字节）
pub fn derive_app_key() -> [u8; 32] {
    let mut key = [0u8; 32];
    key[..8].copy_from_slice(&KEY_FRAG_A);
    key[8..16].copy_from_slice(&KEY_FRAG_B);
    key[16..24].copy_from_slice(&KEY_FRAG_C);
    key[24..].copy_from_slice(&KEY_FRAG_D);
    key
}

/// 基于 HKDF-SHA256 派生资源专属子密钥（M-05 对齐，v1 兼容）
///
/// 参数：
/// - `master_key`: 32 字节主密钥
/// - `id_hash`: 8 位 hex 资源哈希 (例如 "485c04b7")
/// - `slot`: 完整能力槽位字符串 (例如 "ai_engine_cpu")
pub fn derive_sub_key(master_key: &[u8; 32], id_hash: &str, slot: &str) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(None, master_key);
    let mut okm = [0u8; 32];
    let info = format!("{}{}", id_hash, slot);
    hk.expand(info.as_bytes(), &mut okm)
        .expect("HKDF expand 32 字节恒成功");
    okm
}

/// 按规格书 §3.3 规范派生 v2 AEAD 子密钥 (ChaCha20-Poly1305)
///
/// infoAead = "ffpc-v2|" || idHash16B || "|" || slot || "|" || role || "|" || keyEpoch_le4B || "|chacha20-poly1305"
pub fn derive_v2_aead_sub_key(
    master_key: &[u8; 32],
    resource_id_hash_16b: &[u8; 16],
    slot: &str,
    role: &str,
    key_epoch: u32,
) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(b"ffpc-v2"), master_key);
    let mut okm = [0u8; 32];
    let mut info = Vec::with_capacity(32 + slot.len() + role.len());
    info.extend_from_slice(b"ffpc-v2|");
    info.extend_from_slice(resource_id_hash_16b);
    info.push(b'|');
    info.extend_from_slice(slot.as_bytes());
    info.push(b'|');
    info.extend_from_slice(role.as_bytes());
    info.push(b'|');
    info.extend_from_slice(&key_epoch.to_le_bytes());
    info.extend_from_slice(b"|chacha20-poly1305");
    hk.expand(&info, &mut okm).expect("HKDF expand 32 字节恒成功");
    okm
}

/// 按规格书 §3.3 规范采用 SIV 范式派生 12 字节确定性 Nonce
///
/// infoNonce = "ffpc-v2-nonce|" || idHash16B || "|" || role
/// nonce = HKDF-SHA256(ikm, salt=plaintextSha256, infoNonce, 12)
pub fn derive_v2_siv_nonce(
    master_key: &[u8; 32],
    resource_id_hash_16b: &[u8; 16],
    role: &str,
    plaintext_sha256: &[u8; 32],
) -> [u8; 12] {
    let hk = Hkdf::<Sha256>::new(Some(plaintext_sha256), master_key);
    let mut okm = [0u8; 12];
    let mut info = Vec::with_capacity(32 + role.len());
    info.extend_from_slice(b"ffpc-v2-nonce|");
    info.extend_from_slice(resource_id_hash_16b);
    info.push(b'|');
    info.extend_from_slice(role.as_bytes());
    hk.expand(&info, &mut okm).expect("HKDF expand 12 字节恒成功");
    okm
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_app_key_reconstitution() {
        let key = derive_app_key();
        assert_eq!(key.len(), 32);
        // 若构建环境指定了主密钥，则验证分片重组与环境变量解码 100% 字节一致；未指定时比对 Dev Seed
        let expected_seed = match std::env::var("FFP_MASTER_KEY_HEX") {
            Ok(v) if !v.trim().is_empty() => hex::decode(v.trim()).expect("合法 64 字符 hex"),
            _ => hex::decode("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef").unwrap(),
        };
        assert_eq!(key.as_slice(), expected_seed.as_slice());
    }

    #[test]
    fn test_derive_sub_key_determinism() {
        let master_key = [0x42u8; 32];
        let sub1 = derive_sub_key(&master_key, "485c04b7", "ai_engine_cpu");
        let sub2 = derive_sub_key(&master_key, "485c04b7", "ai_engine_cpu");
        assert_eq!(sub1, sub2, "相同参数子密钥必须完全确定");

        let sub3 = derive_sub_key(&master_key, "391b9a03", "base_embedding");
        assert_ne!(sub1, sub3, "不同槽位/ID 派生子密钥必须正交隔离");
    }
}
