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

/// 基于 HKDF-SHA256 派生资源专属子密钥（M-05 对齐）
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_app_key_reconstitution() {
        let key = derive_app_key();
        assert_eq!(key.len(), 32);
        // debug 模式下应当还原出 Dev Seed
        let dev_seed = hex::decode("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef").unwrap();
        assert_eq!(key.as_slice(), dev_seed.as_slice());
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
