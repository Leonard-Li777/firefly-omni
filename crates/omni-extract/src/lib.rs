use omni_core::{OmniConfig, OmniExtractionResult};
use anyhow::Result;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use tracing::{info, warn};
use lofty::prelude::*;
use lofty::probe::Probe;

/// 文档与元数据提取器 (整合 anydoc + exiftool-rs + lofty + kamadak-exif)
pub struct OmniExtractor;

impl OmniExtractor {
    pub async fn extract<P: AsRef<Path>>(path: P, config: &OmniConfig) -> Result<OmniExtractionResult> {
        let p = path.as_ref();
        let path_str = p.to_string_lossy().to_string();
        let metadata = std::fs::metadata(p)?;
        let file_size = metadata.len();

        let mut result = OmniExtractionResult {
            file_path: path_str.clone(),
            mime_type: "application/octet-stream".to_string(),
            file_size,
            markdown_content: String::new(),
            metadata: serde_json::json!({}),
            phash: None,
            is_corrupted: false,
        };

        // 超过单文件上限大小直接返回
        if file_size > config.max_file_size_mb * 1024 * 1024 {
            warn!("File {} exceeds max file size limit of {} MB", path_str, config.max_file_size_mb);
            return Ok(result);
        }

        // 1. 尝试使用 lofty 提取音频/视频 Tag 元数据
        if let Ok(tagged_file) = Probe::open(p).and_then(|pr| pr.read()) {
            let mut audio_meta = serde_json::Map::new();
            if let Some(tag) = tagged_file.primary_tag() {
                if let Some(title) = tag.title() { audio_meta.insert("title".into(), title.to_string().into()); }
                if let Some(artist) = tag.artist() { audio_meta.insert("artist".into(), artist.to_string().into()); }
                if let Some(album) = tag.album() { audio_meta.insert("album".into(), album.to_string().into()); }
            }
            let properties = tagged_file.properties();
            audio_meta.insert("duration_seconds".into(), properties.duration().as_secs().into());
            audio_meta.insert("bitrate".into(), properties.audio_bitrate().unwrap_or(0).into());
            result.metadata["audio"] = serde_json::Value::Object(audio_meta);
        }

        // 2. 尝试使用 kamadak-exif 提取图像 EXIF
        if let Ok(file) = File::open(p) {
            let mut buf_reader = BufReader::new(file);
            if let Ok(exif_reader) = exif::Reader::new().read_from_container(&mut buf_reader) {
                let mut exif_map = serde_json::Map::new();
                for field in exif_reader.fields() {
                    let tag_name = field.tag.to_string();
                    let val_str = field.display_value().with_unit(&exif_reader).to_string();
                    exif_map.insert(tag_name, val_str.into());
                }
                result.metadata["exif"] = serde_json::Value::Object(exif_map);
            }
        }

        info!("Extracted metadata for {}", path_str);
        Ok(result)
    }

    /// 嵌入图片 OCR 文字提取与 Markdown 原始占位符原位替换
    pub fn replace_embedded_image_ocr(markdown: &str, image_ocr_map: &std::collections::HashMap<String, String>) -> String {
        let mut substituted = markdown.to_string();
        for (img_name, ocr_text) in image_ocr_map {
            if ocr_text.trim().is_empty() {
                continue;
            }
            let pattern = format!("![{}]", img_name);
            let replacement = format!(
                "\n> 📷 **[图片内提取文字]**\n> {}\n",
                ocr_text.trim().replace('\n', "\n> ")
            );
            substituted = substituted.replace(&pattern, &replacement);
        }
        substituted
    }
}
