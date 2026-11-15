#[derive(Debug, thiserror::Error)]
pub enum FfpError {
    #[error("头部数据被截断")]
    TruncatedHeader,

    #[error("无效的 .ffpc 文件魔数")]
    InvalidMagic,

    #[error("不支持的容器版本: {0}")]
    UnsupportedVersion(u16),

    #[error("不支持的密码套件: {0}")]
    UnsupportedCipherSuite(u8),

    #[error("非法头部长度: {0}")]
    InvalidHeaderLength(usize),

    #[error("非法 Slot 长度")]
    InvalidSlotLength,

    #[error("非法 Role 长度")]
    InvalidRoleLength,

    #[error("Header 长度自洽性校验失败: declared={declared}, expected={expected}")]
    HeaderLengthMismatch { declared: usize, expected: usize },

    #[error("密文长度与明文声明不匹配: declared={declared}, actual={actual}")]
    CiphertextLengthMismatch { declared: u64, actual: u64 },

    #[error("模型超出硬上限 4GiB: {0}")]
    ModelTooLarge(u64),

    #[error(".ffpc 文件结构损坏、整型溢出或字段不一致: {0}")]
    InvalidFormat(String),

    #[error("伴随件 JSON 键名包含非 ASCII 字符: {0}")]
    NonAsciiSiblingKey(String),

    #[error("伴随件摘要不匹配: declared={declared}, actual={actual}")]
    SiblingsDigestMismatch { declared: String, actual: String },

    #[error("密钥代次不匹配：file_epoch={0}, local_epoch={1}（请重新导入最新离线总包）")]
    KeyEpochMismatch(u16, u16),

    #[error("AEAD 解密失败（文件可能被篡改或密钥版本不匹配）")]
    AeadDecryptFailed,

    #[error("AAD 不匹配：expected idHash={expected_id_hash} slot={expected_slot} version={expected_version}, actual idHash={actual_id_hash} slot={actual_slot} version={actual_version}")]
    AadMismatch {
        expected_id_hash: String,
        expected_slot: String,
        expected_version: String,
        actual_id_hash: String,
        actual_slot: String,
        actual_version: String,
    },

    #[error("Tokenizer 解析失败: {0}")]
    TokenizerLoadFailed(String),

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
}
