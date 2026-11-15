use ffp_crypto::{
    build_v2_header_bytes, compute_canonical_siblings_digest, parse_v2_header,
    validate_sibling_keys, verify_siblings_digest, FfpError, FfpcHeader, SiblingEntry,
    MAX_MODEL_BYTES, TAG_LEN,
};
use sha2::{Digest, Sha256};

fn dummy_id_hash() -> [u8; 16] {
    let mut h = [0u8; 16];
    h.copy_from_slice(&Sha256::digest(b"chinese-clip")[0..16]);
    h
}

fn dummy_sha256(seed: &[u8]) -> [u8; 32] {
    Sha256::digest(seed).into()
}

#[test]
fn test_v2_header_roundtrip_golden_case() {
    let id_hash = dummy_id_hash();
    let slot = "base_embedding";
    let role = "visual";
    let key_epoch = 42u32;
    let plaintext_len = 100u64;
    let pt_sha = dummy_sha256(b"fake-plaintext-weights");
    let sib_digest = dummy_sha256(b"fake-siblings-digest");
    let nonce = [7u8; 12];

    let header_bytes = build_v2_header_bytes(
        &id_hash,
        slot,
        role,
        key_epoch,
        plaintext_len,
        &pt_sha,
        &sib_digest,
        &nonce,
    )
    .expect("构建合法 Header 成功");

    let expected_header_len = 114 + slot.len() + role.len();
    assert_eq!(header_bytes.len(), expected_header_len);

    // 组装完整的 .ffpc 容器切片: Header + Ciphertext(100B) + Tag(16B)
    let mut container = header_bytes.clone();
    container.extend_from_slice(&vec![0xAA; plaintext_len as usize]); // 密文
    container.extend_from_slice(&[0xBB; TAG_LEN]); // Poly1305 Tag

    let parsed = parse_v2_header(&container).expect("解析 v2 头部成功");
    assert_eq!(parsed.format_version, 2);
    assert_eq!(parsed.cipher_suite, 1);
    assert_eq!(parsed.header_len, expected_header_len);
    assert_eq!(parsed.resource_id_hash, id_hash);
    assert_eq!(parsed.slot, slot);
    assert_eq!(parsed.role, role);
    assert_eq!(parsed.key_epoch, key_epoch);
    assert_eq!(parsed.plaintext_len, plaintext_len);
    assert_eq!(parsed.plaintext_sha256, pt_sha);
    assert_eq!(parsed.siblings_digest, sib_digest);
    assert_eq!(parsed.nonce, nonce);

    // AAD 物理切片铁律: AAD 严格等于 [0..headerLen] 且无多余字节
    assert_eq!(parsed.aad, &header_bytes[..]);
    assert_eq!(parsed.aad.len(), expected_header_len);
}

#[test]
fn test_v2_header_truncation_fine_grained() {
    // 1. < 4 字节
    let err = parse_v2_header(&[b'F', b'F', b'P']).unwrap_err();
    assert!(matches!(err, FfpError::TruncatedHeader));

    // 2. 4 字节但 Magic 错误
    let err = parse_v2_header(b"XXXX").unwrap_err();
    assert!(matches!(err, FfpError::InvalidMagic));

    // 3. Magic 正确但小于 116 字节
    let mut short_buf = b"FFPC".to_vec();
    short_buf.resize(115, 0);
    let err = parse_v2_header(&short_buf).unwrap_err();
    assert!(matches!(err, FfpError::TruncatedHeader));
}

#[test]
fn test_v2_header_version_and_suite_rejection() {
    let id_hash = dummy_id_hash();
    let slot = "s";
    let role = "r";
    let pt_sha = dummy_sha256(b"p");
    let sib_sha = dummy_sha256(b"s");
    let nonce = [0u8; 12];

    let header = build_v2_header_bytes(&id_hash, slot, role, 1, 10, &pt_sha, &sib_sha, &nonce)
        .unwrap();
    let mut container = header.clone();
    container.extend_from_slice(&vec![0; 10]); // ct
    container.extend_from_slice(&[0; 16]); // tag

    // 篡改 formatVersion != 2
    container[4] = 1;
    let err = parse_v2_header(&container).unwrap_err();
    assert!(matches!(err, FfpError::UnsupportedVersion(1)));

    container[4] = 3;
    let err = parse_v2_header(&container).unwrap_err();
    assert!(matches!(err, FfpError::UnsupportedVersion(3)));

    // 篡改 cipherSuite != 1
    container[4] = 2; // 恢复版本 2
    container[5] = 2; // 非法套件
    let err = parse_v2_header(&container).unwrap_err();
    assert!(matches!(err, FfpError::UnsupportedCipherSuite(2)));
}

#[test]
fn test_v2_header_coarse_length_bounds() {
    let id_hash = dummy_id_hash();
    let slot = "s";
    let role = "r";
    let pt_sha = dummy_sha256(b"p");
    let sib_sha = dummy_sha256(b"s");
    let nonce = [0u8; 12];

    let header = build_v2_header_bytes(&id_hash, slot, role, 1, 10, &pt_sha, &sib_sha, &nonce)
        .unwrap();
    let mut container = header.clone();
    container.extend_from_slice(&vec![0; 10]);
    container.extend_from_slice(&[0; 16]);

    // 粗筛下界 < 116 (例如 115)
    let original_len_bytes = [container[6], container[7]];
    container[6..8].copy_from_slice(&115u16.to_le_bytes());
    let err = parse_v2_header(&container).unwrap_err();
    assert!(matches!(err, FfpError::InvalidHeaderLength(115)));

    // 粗筛上界 > 178 (例如 179)
    container[6..8].copy_from_slice(&179u16.to_le_bytes());
    let err = parse_v2_header(&container).unwrap_err();
    assert!(matches!(err, FfpError::InvalidHeaderLength(179)));

    // 容器总长度小于 headerLen + 16 (截断文件，连 Tag 都不完整)
    container[6..8].copy_from_slice(&original_len_bytes);
    let truncated_tag_container = &container[..header.len() + TAG_LEN - 1]; // 仅有 15B 尾部
    let err = parse_v2_header(truncated_tag_container).unwrap_err();
    assert!(matches!(err, FfpError::TruncatedHeader));

    // 容器被削掉 1 字节密文 (触发密文等长校验失败)
    let truncated_ct_container = &container[..container.len() - 1];
    let err = parse_v2_header(truncated_ct_container).unwrap_err();
    assert!(matches!(err, FfpError::CiphertextLengthMismatch { .. }));
}

#[test]
fn test_v2_header_length_mismatch_assertion() {
    let id_hash = dummy_id_hash();
    let slot = "s"; // Ls = 1
    let role = "r"; // Lr = 1
    let pt_sha = dummy_sha256(b"p");
    let sib_sha = dummy_sha256(b"s");
    let nonce = [0u8; 12];

    // 构造合法包 (header_len = 114 + 1 + 1 = 116)
    let header = build_v2_header_bytes(&id_hash, slot, role, 1, 10, &pt_sha, &sib_sha, &nonce)
        .unwrap();
    let mut container = header.clone();
    container.extend_from_slice(&vec![0; 10]);
    container.extend_from_slice(&[0; 16]);

    // 自相矛盾攻击 (TC-10 / R3-01):
    // 伪造 headerLen = 178 (符合粗筛 116..=178), 但内部 slotLen=1, roleLen=1 (预期 116)
    // 扩展 container 长度以满足 container.len() >= 178 + 16
    container.resize(200, 0);
    container[6..8].copy_from_slice(&178u16.to_le_bytes());

    let err = parse_v2_header(&container).unwrap_err();
    match err {
        FfpError::HeaderLengthMismatch { declared, expected } => {
            assert_eq!(declared, 178);
            assert_eq!(expected, 116);
        }
        _ => panic!("预期 HeaderLengthMismatch, 实际 {:?}", err),
    }
}

#[test]
fn test_v2_header_slot_and_role_length_bounds() {
    let id_hash = dummy_id_hash();
    let pt_sha = dummy_sha256(b"p");
    let sib_sha = dummy_sha256(b"s");
    let nonce = [0u8; 12];

    // slot 为空
    let err = build_v2_header_bytes(&id_hash, "", "role", 1, 10, &pt_sha, &sib_sha, &nonce)
        .unwrap_err();
    assert!(matches!(err, FfpError::InvalidSlotLength));

    // slot 超出 32 字节
    let long_slot = "a".repeat(33);
    let err = build_v2_header_bytes(&id_hash, &long_slot, "role", 1, 10, &pt_sha, &sib_sha, &nonce)
        .unwrap_err();
    assert!(matches!(err, FfpError::InvalidSlotLength));

    // role 为空
    let err = build_v2_header_bytes(&id_hash, "slot", "", 1, 10, &pt_sha, &sib_sha, &nonce)
        .unwrap_err();
    assert!(matches!(err, FfpError::InvalidRoleLength));

    // role 超出 32 字节
    let long_role = "b".repeat(33);
    let err = build_v2_header_bytes(&id_hash, "slot", &long_role, 1, 10, &pt_sha, &sib_sha, &nonce)
        .unwrap_err();
    assert!(matches!(err, FfpError::InvalidRoleLength));
}

#[test]
fn test_v2_header_plaintext_length_and_tail_injection() {
    let id_hash = dummy_id_hash();
    let slot = "slot";
    let role = "role";
    let pt_sha = dummy_sha256(b"p");
    let sib_sha = dummy_sha256(b"s");
    let nonce = [0u8; 12];

    // 1. plaintext_len == 0 拦截
    let header = build_v2_header_bytes(&id_hash, slot, role, 1, 0, &pt_sha, &sib_sha, &nonce)
        .unwrap();
    let mut container = header.clone();
    container.extend_from_slice(&[0; 16]); // tag
    let err = parse_v2_header(&container).unwrap_err();
    assert!(matches!(err, FfpError::CiphertextLengthMismatch { .. }));

    // 2. 尾随垃圾注入拦截 (N-02 / Q-02): 声明 10 字节，实际密文 11 字节
    let header = build_v2_header_bytes(&id_hash, slot, role, 1, 10, &pt_sha, &sib_sha, &nonce)
        .unwrap();
    let mut container = header.clone();
    container.extend_from_slice(&vec![0xAA; 11]); // 多塞了 1 字节垃圾
    container.extend_from_slice(&[0; 16]); // tag
    let err = parse_v2_header(&container).unwrap_err();
    match err {
        FfpError::CiphertextLengthMismatch { declared, actual } => {
            assert_eq!(declared, 10);
            assert_eq!(actual, 11);
        }
        _ => panic!("预期 CiphertextLengthMismatch, 实际 {:?}", err),
    }

    // 3. 模型尺寸超过 4GiB 上限拦截错误类型格式化验证
    let err = FfpError::ModelTooLarge(MAX_MODEL_BYTES + 1);
    assert_eq!(
        err.to_string(),
        format!("模型超出硬上限 4GiB: {}", MAX_MODEL_BYTES + 1)
    );
}

#[test]
fn test_v2_rejection_by_legacy_v1_client() {
    // 验证破坏性代际升级 (V2-02 / Q-04):
    // 当旧版客户端读取 v2 容器时，必须且必然由于 formatVersion=2, cipherSuite=1 (合成 u16_le=258)
    // 返回 UnsupportedVersion(258) 快速失败
    let id_hash = dummy_id_hash();
    let slot = "visual";
    let role = "model";
    let pt_sha = dummy_sha256(b"weights");
    let sib_sha = dummy_sha256(b"sibs");
    let nonce = [0u8; 12];

    let header = build_v2_header_bytes(&id_hash, slot, role, 1, 20, &pt_sha, &sib_sha, &nonce)
        .unwrap();
    let mut v2_container = header.clone();
    v2_container.extend_from_slice(&vec![0; 20]);
    v2_container.extend_from_slice(&[0; 16]);

    // 旧版解析入口 FfpcHeader::parse_prefix
    let err = FfpcHeader::parse_prefix(&v2_container).expect_err("旧版解析器必须拒绝 v2 容器");
    match err {
        FfpError::UnsupportedVersion(v) => {
            assert_eq!(v, 258, "旧版按 u16_le 读取 bytes[4]=2, bytes[5]=1 必然得到 258");
        }
        _ => panic!("旧版必须返回 UnsupportedVersion(258), 实际 {:?}", err),
    }
}

#[test]
fn test_rfc8785_canonical_siblings_digest() {
    // 1. 空伴随件必须等于 SHA-256("[]")
    let empty_digest = compute_canonical_siblings_digest(&[]).unwrap();
    let mut hasher = Sha256::new();
    hasher.update(b"[]");
    let expected_empty: [u8; 32] = hasher.finalize().into();
    assert_eq!(empty_digest, expected_empty);

    // 2. 乱序输入必须确定性排序收敛 (按 file 升序)
    let s1 = SiblingEntry {
        file: "vocab.txt".into(),
        sha256: "AAAA".into(),
        size: 100,
    };
    let s2 = SiblingEntry {
        file: "config.json".into(),
        sha256: "BBBB".into(),
        size: 200,
    };

    let digest1 = compute_canonical_siblings_digest(&[s1.clone(), s2.clone()]).unwrap();
    let digest2 = compute_canonical_siblings_digest(&[s2.clone(), s1.clone()]).unwrap();
    assert_eq!(digest1, digest2, "乱序伴随件的 JCS 摘要必须完全一致");

    // 校验 verify_siblings_digest 辅助函数
    assert!(verify_siblings_digest(&[s1.clone(), s2.clone()], &digest1).is_ok());

    let tampered_digest = [0xFFu8; 32];
    assert!(verify_siblings_digest(&[s1, s2], &tampered_digest).is_err());
}

#[test]
fn test_siblings_ascii_keys_validation() {
    // 纯 ASCII 合法键名
    let valid_json = br#"[{"file":"vocab.txt","sha256":"abcd","size":123}]"#;
    assert!(validate_sibling_keys(valid_json).is_ok());

    // 非 ASCII 键名 (严禁)
    let non_ascii_key_json = "[{\"文件名\":\"vocab.txt\",\"sha256\":\"abcd\",\"size\":123}]".as_bytes();
    let err = validate_sibling_keys(non_ascii_key_json).unwrap_err();
    assert!(matches!(err, FfpError::NonAsciiSiblingKey(_)));

    // 非预期字段拦截
    let extra_field_json = br#"[{"file":"vocab.txt","sha256":"abcd","size":123,"extra":"bad"}]"#;
    let err = validate_sibling_keys(extra_field_json).unwrap_err();
    assert!(matches!(err, FfpError::InvalidFormat(_)));
}
