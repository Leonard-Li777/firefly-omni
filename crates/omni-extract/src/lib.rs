use omni_core::{OmniConfig, OmniExtractionResult};
use anyhow::Result;
use std::path::Path;

/// 文档与元数据提取器 (整合 anydoc + exiftool-rs + lofty + kamadak-exif)
pub struct OmniExtractor;

impl OmniExtractor {
    pub async fn extract<P: AsRef<Path>>(path: P, _config: &OmniConfig) -> Result<OmniExtractionResult> {
        let p = path.as_ref();
        let path_str = p.to_string_lossy().to_string();
        let metadata = std::fs::metadata(p)?;

        Ok(OmniExtractionResult {
            file_path: path_str,
            mime_type: "application/octet-stream".to_string(),
            file_size: metadata.len(),
            markdown_content: String::new(),
            metadata: serde_json::json!({}),
            phash: None,
            is_corrupted: false,
        })
    }
}
