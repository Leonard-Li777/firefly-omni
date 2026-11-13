pub mod error;
pub mod header;
pub mod key;
pub mod payload;
pub mod decrypt;

pub use decrypt::{decrypt_ffpc_bytes, decrypt_ffpc_payload};
pub use error::FfpError;
pub use header::{AadMetadata, FfpcHeader};
pub use key::{derive_app_key, derive_sub_key, LOCAL_KEY_EPOCH};
pub use payload::DecryptedPayload;

use chacha20poly1305::{
    aead::{AeadInPlace, KeyInit},
    ChaCha20Poly1305, Nonce,
};

/// 辅助测试与跨 Crate 验证：按规范打包构造一个合法 .ffpc 内存切片
pub fn build_test_ffpc(
    master_key: &[u8; 32],
    key_epoch: u16,
    id_hash: &str,
    slot: &str,
    version: &str,
    onnx_bytes: &[u8],
    tok_bytes: &[u8],
    nonce: &[u8; 12],
) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(onnx_bytes);
    payload.extend_from_slice(tok_bytes);
    let ct_len = payload.len() as u64;

    let aad_meta = AadMetadata {
        id_hash: id_hash.to_string(),
        slot: slot.to_string(),
        version: version.to_string(),
        key_epoch,
        onnx_len: onnx_bytes.len(),
        tok_len: tok_bytes.len(),
    };
    let aad_json_bytes = serde_json::to_vec(&aad_meta).unwrap();
    let aad_len = aad_json_bytes.len() as u32;

    let mut header_bytes = Vec::new();
    header_bytes.extend_from_slice(b"FFPC");
    header_bytes.extend_from_slice(&1u16.to_le_bytes());
    header_bytes.extend_from_slice(&key_epoch.to_le_bytes());
    header_bytes.extend_from_slice(nonce);
    header_bytes.extend_from_slice(&aad_len.to_le_bytes());
    header_bytes.extend_from_slice(&aad_json_bytes);
    header_bytes.extend_from_slice(&ct_len.to_le_bytes());

    let sub_key = derive_sub_key(master_key, id_hash, slot);
    let cipher = ChaCha20Poly1305::new_from_slice(&sub_key).unwrap();
    let mut ct_buffer = payload.clone();
    let tag = cipher
        .encrypt_in_place_detached(Nonce::from_slice(nonce), &header_bytes, &mut ct_buffer)
        .unwrap();

    let mut final_file = header_bytes;
    final_file.extend_from_slice(&ct_buffer);
    final_file.extend_from_slice(&tag);
    final_file
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip_decrypt_success() {
        let master_key = derive_app_key();
        let onnx_data = b"FAKE_ONNX_MODEL_TENSORS_HERE";
        let tok_data = b"{\"model\": \"tokenizer\"}";
        let nonce = [0x55u8; 12];

        let file_bytes = build_test_ffpc(
            &master_key,
            LOCAL_KEY_EPOCH,
            "485c04b7",
            "ai_engine_cpu",
            "1.0.0",
            onnx_data,
            tok_data,
            &nonce,
        );

        let payload = decrypt_ffpc_bytes(
            &file_bytes,
            "485c04b7",
            "ai_engine_cpu",
            "1.0.0",
        )
        .expect("解密合法容器必须成功");

        assert_eq!(payload.onnx_slice(), onnx_data);
        assert_eq!(payload.tok_slice().unwrap(), tok_data);
    }

    #[test]
    fn test_tamper_header_fails_aead() {
        let master_key = derive_app_key();
        let onnx_data = b"FAKE_ONNX_MODEL_TENSORS";
        let nonce = [0x55u8; 12];

        let mut file_bytes = build_test_ffpc(
            &master_key,
            LOCAL_KEY_EPOCH,
            "485c04b7",
            "ai_engine_cpu",
            "1.0.0",
            onnx_data,
            b"",
            &nonce,
        );

        // 篡改 Nonce 任意 1 字节
        file_bytes[10] ^= 0xFF;

        let err = decrypt_ffpc_bytes(
            &file_bytes,
            "485c04b7",
            "ai_engine_cpu",
            "1.0.0",
        )
        .expect_err("篡改头部切片必须失败");

        match err {
            FfpError::AeadDecryptFailed => {}
            _ => panic!("预期 AeadDecryptFailed，实际 {:?}", err),
        }
    }

    #[test]
    fn test_epoch_mismatch() {
        let master_key = derive_app_key();
        let onnx_data = b"FAKE_ONNX_MODEL_TENSORS";
        let nonce = [0x55u8; 12];

        let file_bytes = build_test_ffpc(
            &master_key,
            LOCAL_KEY_EPOCH + 1, // 代次不一致
            "485c04b7",
            "ai_engine_cpu",
            "1.0.0",
            onnx_data,
            b"",
            &nonce,
        );

        let err = decrypt_ffpc_bytes(
            &file_bytes,
            "485c04b7",
            "ai_engine_cpu",
            "1.0.0",
        )
        .expect_err("代次不一致必须硬失败");

        match err {
            FfpError::KeyEpochMismatch(f, l) => {
                assert_eq!(f, LOCAL_KEY_EPOCH + 1);
                assert_eq!(l, LOCAL_KEY_EPOCH);
            }
            _ => panic!("预期 KeyEpochMismatch，实际 {:?}", err),
        }
    }
}
