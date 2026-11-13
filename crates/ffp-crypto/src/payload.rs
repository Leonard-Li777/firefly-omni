use zeroize::Zeroizing;

/// 解密载荷：单缓冲持有完整明文，对外暴露不可变切片视图（W-01 / M-07 零二次堆分配）
pub struct DecryptedPayload {
    /// 完整明文连续内存 (onnx_bytes ‖ tok_bytes)，Drop 时自动 zeroize
    pub(crate) storage: Zeroizing<Vec<u8>>,
    pub(crate) onnx_len: usize,
    pub(crate) tok_len: usize,
}

impl std::fmt::Debug for DecryptedPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecryptedPayload")
            .field("total_len", &self.storage.len())
            .field("onnx_len", &self.onnx_len)
            .field("tok_len", &self.tok_len)
            .finish()
    }
}

impl DecryptedPayload {
    pub fn new(storage: Zeroizing<Vec<u8>>, onnx_len: usize, tok_len: usize) -> Self {
        assert_eq!(
            storage.len(),
            onnx_len + tok_len,
            "载荷总大小必须严格等于 onnx_len + tok_len"
        );
        Self {
            storage,
            onnx_len,
            tok_len,
        }
    }

    /// 借出 ONNX 模型字节切片视图
    pub fn onnx_slice(&self) -> &[u8] {
        &self.storage[..self.onnx_len]
    }

    /// 借出可选分词器切片视图
    pub fn tok_slice(&self) -> Option<&[u8]> {
        if self.tok_len > 0 {
            Some(&self.storage[self.onnx_len..self.onnx_len + self.tok_len])
        } else {
            None
        }
    }

    /// 获取 ONNX 字节数
    pub fn onnx_len(&self) -> usize {
        self.onnx_len
    }

    /// 获取 Tokenizer 字节数
    pub fn tok_len(&self) -> usize {
        self.tok_len
    }

    /// 获取完整载荷总大小
    pub fn total_len(&self) -> usize {
        self.storage.len()
    }
}
