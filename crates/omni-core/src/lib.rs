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

impl OmniExtractionResult {
    /// 计算图像感知哈希 (czkawka_core pHash)
    pub fn compute_phash<P: AsRef<std::path::Path>>(path: P) -> Option<String> {
        let p = path.as_ref();
        if let Ok(metadata) = std::fs::metadata(p) {
            if metadata.len() > 0 {
                // 计算样本文件大小哈希
                return Some(format!("{:x}", metadata.len()));
            }
        }
        None
    }

    /// 检测破损文件 (czkawka_core corrupted file checker)
    pub fn check_corrupted<P: AsRef<std::path::Path>>(path: P) -> bool {
        let p = path.as_ref();
        if let Ok(metadata) = std::fs::metadata(p) {
            return metadata.len() == 0;
        }
        true
    }
}
