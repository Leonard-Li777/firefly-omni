use serde::{Deserialize, Serialize};

/// 基础分析与配置规范 (对齐 Desktop ConfigKey)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OmniConfig {
    pub enable_document_ocr: bool,
    pub enable_image_ocr: bool,
    pub max_document_ocr_file_size_mb: u64,
    pub max_content_size_kb: usize,
    pub max_file_size_mb: u64,
}

impl Default for OmniConfig {
    fn default() -> Self {
        Self {
            enable_document_ocr: true,
            enable_image_ocr: true,
            max_document_ocr_file_size_mb: 10,
            max_content_size_kb: 30,
            max_file_size_mb: 100,
        }
    }
}

/// 全量文件提取结果元数据
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OmniExtractionResult {
    pub file_path: String,
    pub mime_type: String,
    pub file_size: u64,
    pub markdown_content: String,
    pub metadata: serde_json::Value,
    pub phash: Option<String>,
    pub is_corrupted: bool,
}
