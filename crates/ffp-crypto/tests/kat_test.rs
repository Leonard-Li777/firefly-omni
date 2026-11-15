use ffp_crypto::{
    compute_canonical_siblings_digest, decrypt_v2_stream, derive_v2_aead_sub_key,
    derive_v2_siv_nonce, parse_v2_header, FfpError, SiblingEntry,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Deserialize)]
struct KatVector {
    input: KatInput,
    expected: KatExpected,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct KatInput {
    master_key_hex: String,
    resource_id: String,
    slot: String,
    role: String,
    key_epoch: u32,
    plaintext_hex: String,
    siblings: Vec<SiblingEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct KatExpected {
    resource_id_hash16_hex: String,
    siblings_digest_hex: String,
    sub_key_hex: String,
    nonce_hex: String,
    header_hex: String,
    header_len: usize,
    plaintext_sha256_hex: String,
    ciphertext_with_tag_hex: String,
    ffpc_container_hex: String,
    container_sha256_hex: String,
    ffpc_file_name: String,
}

fn load_kat_vector() -> KatVector {
    const KAT_JSON: &str = include_str!("../../../../../scripts/tests/kat_vector.json");
    serde_json::from_str(KAT_JSON).expect("反序列化 kat_vector.json 成功")
}

#[test]
fn test_kat_cross_language_vector_verification() {
    let kat = load_kat_vector();

    // 1. 验证 master_key
    let master_key_bytes = hex::decode(&kat.input.master_key_hex).expect("解码 master_key");
    assert_eq!(master_key_bytes.len(), 32);
    let mut master_key = [0u8; 32];
    master_key.copy_from_slice(&master_key_bytes);

    // 2. 验证 resource_id_hash (16 字节切片)
    let mut expected_id_hash = [0u8; 16];
    expected_id_hash.copy_from_slice(
        &hex::decode(&kat.expected.resource_id_hash16_hex).expect("解码 resourceIdHash16"),
    );
    let actual_id_hash: [u8; 16] = Sha256::digest(kat.input.resource_id.as_bytes())[..16]
        .try_into()
        .unwrap();
    assert_eq!(actual_id_hash, expected_id_hash, "resourceIdHash 16B 吻合");

    // 3. 验证 siblingsDigest (RFC 8785 JCS + SHA256)
    let sib_digest =
        compute_canonical_siblings_digest(&kat.input.siblings).expect("计算伴随件规范化摘要");
    assert_eq!(
        hex::encode(sib_digest),
        kat.expected.siblings_digest_hex,
        "siblingsDigest 吻合"
    );

    // 4. 验证明文哈希
    let plaintext_bytes = hex::decode(&kat.input.plaintext_hex).expect("解码 plaintext");
    let actual_pt_sha: [u8; 32] = Sha256::digest(&plaintext_bytes).into();
    assert_eq!(
        hex::encode(actual_pt_sha),
        kat.expected.plaintext_sha256_hex,
        "明文 SHA256 吻合"
    );

    // 5. 验证 AEAD 子密钥派生 (HKDF-SHA256)
    let derived_sub_key = derive_v2_aead_sub_key(
        &master_key,
        &actual_id_hash,
        &kat.input.slot,
        &kat.input.role,
        kat.input.key_epoch,
    );
    assert_eq!(
        hex::encode(derived_sub_key),
        kat.expected.sub_key_hex,
        "AEAD 子密钥派生吻合"
    );

    // 6. 验证 SIV 确定性 Nonce 派生
    let derived_nonce = derive_v2_siv_nonce(
        &master_key,
        &actual_id_hash,
        &kat.input.role,
        &actual_pt_sha,
    );
    assert_eq!(
        hex::encode(derived_nonce),
        kat.expected.nonce_hex,
        "SIV 确定性 Nonce 派生吻合"
    );

    // 7. 验证 Header 字节流与容器 SHA-256
    let container_bytes =
        hex::decode(&kat.expected.ffpc_container_hex).expect("解码 ffpcContainerHex");
    let container_sha256 = Sha256::digest(&container_bytes);
    assert_eq!(
        hex::encode(container_sha256),
        kat.expected.container_sha256_hex,
        "容器文件 SHA256 吻合"
    );

    // 8. 解析 Header 并验证全部元数据
    let header = parse_v2_header(&container_bytes).expect("解析黄金向量 v2 Header 成功");
    assert_eq!(header.format_version, 2);
    assert_eq!(header.cipher_suite, 1);
    assert_eq!(header.header_len, kat.expected.header_len);
    assert_eq!(header.resource_id_hash, actual_id_hash);
    assert_eq!(header.slot, kat.input.slot);
    assert_eq!(header.role, kat.input.role);
    assert_eq!(header.key_epoch, kat.input.key_epoch);
    assert_eq!(header.plaintext_len, plaintext_bytes.len() as u64);
    assert_eq!(header.plaintext_sha256, actual_pt_sha);
    assert_eq!(header.siblings_digest, sib_digest);
    assert_eq!(header.nonce, derived_nonce);
    assert_eq!(
        hex::encode(header.aad),
        kat.expected.header_hex,
        "AAD 字节流与 Header 字节流完全恒等"
    );

    // 9. 执行解密并强断言明文 100% 逐字节吻合
    let decrypted =
        decrypt_v2_stream(&container_bytes, &master_key).expect("解密黄金向量容器成功");
    assert_eq!(
        decrypted.as_slice(),
        plaintext_bytes.as_slice(),
        "解密明文与原始明文 100% 逐字节恒等"
    );
}

#[test]
fn test_kat_anomaly_interception_security_invariants() {
    let kat = load_kat_vector();
    let master_key_bytes = hex::decode(&kat.input.master_key_hex).expect("解码 master_key");
    let mut master_key = [0u8; 32];
    master_key.copy_from_slice(&master_key_bytes);

    let container_bytes =
        hex::decode(&kat.expected.ffpc_container_hex).expect("解码 ffpcContainerHex");

    // N-02 尾部垃圾追加严格拦截：在合法文件末尾追加 1 字节
    {
        let mut tampered = container_bytes.clone();
        tampered.push(0xCC);
        let err = decrypt_v2_stream(&tampered, &master_key).unwrap_err();
        match err {
            FfpError::CiphertextLengthMismatch { declared, actual } => {
                assert_eq!(declared, 1024);
                assert_eq!(actual, 1025);
            }
            other => panic!("预期 CiphertextLengthMismatch，实际得到: {:?}", other),
        }
    }

    // 密文损坏：翻转密文区 1 字节
    {
        let mut tampered = container_bytes.clone();
        let flip_idx = kat.expected.header_len + 10;
        tampered[flip_idx] ^= 0x01;
        let err = decrypt_v2_stream(&tampered, &master_key).unwrap_err();
        match err {
            FfpError::AeadDecryptFailed => {}
            other => panic!("预期 AeadDecryptFailed，实际得到: {:?}", other),
        }
    }

    // AAD 篡改：翻转 Header 中 role 的某个字符
    {
        let mut tampered = container_bytes.clone();
        // role 在 26 + Ls 处，这里 slot 是 base_embedding (14B)，slotLen 在 24，slot在 25..39，roleLen在 39，role在 40..46
        tampered[40] ^= 0x01; // 篡改 role 首字节
        let err = decrypt_v2_stream(&tampered, &master_key).unwrap_err();
        // 由于 Header AAD 被篡改，AEAD 认证标签必不匹配
        match err {
            FfpError::AeadDecryptFailed => {}
            other => panic!("预期 AeadDecryptFailed，实际得到: {:?}", other),
        }
    }
}
