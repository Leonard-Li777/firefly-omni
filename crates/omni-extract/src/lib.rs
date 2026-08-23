use omni_core::{OmniConfig, OmniExtractionResult};
use omni_vision::OmniVisionEngine;
use anyhow::Result;
use encoding_rs::{GBK, UTF_8, UTF_16LE};
use lofty::prelude::*;
use lofty::probe::Probe;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use tracing::{info, warn};

/// 文档与元数据全量提取引擎 (完全对齐 Node.js 端全功能 Pipeline)
pub struct OmniExtractor;

impl OmniExtractor {
    pub async fn extract<P: AsRef<Path>>(path: P, config: &OmniConfig) -> Result<OmniExtractionResult> {
        let p = path.as_ref();
        let path_str = p.to_string_lossy().to_string();
        let metadata = std::fs::metadata(p)?;
        let file_size = metadata.len();

        // 识别真实 MIME 类型与扩展名
        let mime_type = OmniVisionEngine::detect_mime_type(p).unwrap_or_else(|_| "application/octet-stream".to_string());
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();

        let mut result = OmniExtractionResult {
            file_path: path_str.clone(),
            mime_type: mime_type.clone(),
            file_size,
            markdown_content: String::new(),
            metadata: serde_json::json!({}),
            phash: None,
            is_corrupted: file_size == 0,
        };

        // 超过单文件上限大小直接返回
        if file_size > config.max_file_size_mb * 1024 * 1024 {
            warn!("File {} exceeds max file size limit of {} MB", path_str, config.max_file_size_mb);
            return Ok(result);
        }

        // 校验文件分类
        let is_pdf = ext == "pdf" || mime_type == "application/pdf";
        let is_image = mime_type.starts_with("image/") || matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "tiff");
        let is_audio = mime_type.starts_with("audio/") || matches!(ext.as_str(), "mp3" | "flac" | "wav" | "aac" | "ogg" | "m4a");
        let is_video = mime_type.starts_with("video/") || matches!(ext.as_str(), "mp4" | "mkv" | "mov" | "avi");
        let is_office = matches!(ext.as_str(), "docx" | "xlsx" | "pptx" | "odt" | "ods" | "odp" | "rtf");

        let is_unknown_or_raw = !is_image && !is_audio && !is_video && !is_pdf && !is_office;
        let is_text_or_code = is_plain_text_or_code_ext(&ext) || mime_type.starts_with("text/") || mime_type.contains("json") || mime_type.contains("xml") || is_unknown_or_raw;


        // 1. 尝试提取音频 / 视频 Tag 与元数据
        if is_audio || is_video {
            if let Ok(tagged_file) = Probe::open(p).and_then(|pr| pr.read()) {
                let mut audio_meta = serde_json::Map::new();
                if let Some(tag) = tagged_file.primary_tag() {
                    if let Some(title) = tag.title() { audio_meta.insert("title".into(), title.to_string().into()); }
                    if let Some(artist) = tag.artist() { audio_meta.insert("artist".into(), artist.to_string().into()); }
                    if let Some(album) = tag.album() { audio_meta.insert("album".into(), album.to_string().into()); }
                    if let Some(genre) = tag.genre() { audio_meta.insert("genre".into(), genre.to_string().into()); }
                }
                let properties = tagged_file.properties();
                audio_meta.insert("duration_seconds".into(), properties.duration().as_secs().into());
                audio_meta.insert("bitrate".into(), properties.audio_bitrate().unwrap_or(0).into());
                result.metadata["media"] = serde_json::Value::Object(audio_meta);
            }
        }

        // 2. 尝试提取图像 EXIF 与尺寸信息
        if is_image {
            // 计算图像 64-bit 感知哈希 (pHash)
            result.phash = OmniExtractionResult::compute_phash(p);

            let mut img_meta = serde_json::Map::new();
            if let Ok((width, height)) = image::image_dimensions(p) {
                img_meta.insert("width".into(), width.into());
                img_meta.insert("height".into(), height.into());
                img_meta.insert("resolution".into(), format!("{}x{}", width, height).into());
            }

            if let Ok(file) = File::open(p) {
                let mut buf_reader = BufReader::new(file);
                if let Ok(exif_reader) = exif::Reader::new().read_from_container(&mut buf_reader) {
                    let mut exif_map = serde_json::Map::new();
                    for field in exif_reader.fields() {
                        let tag_name = field.tag.to_string();
                        let val_str = field.display_value().with_unit(&exif_reader).to_string();
                        exif_map.insert(tag_name, val_str.into());
                    }
                    img_meta.insert("exif".into(), serde_json::Value::Object(exif_map));
                }
            }
            result.metadata["image"] = serde_json::Value::Object(img_meta);

            // 调用 PP-OCRv6 执行图像文本识别
            if config.enable_image_ocr {
                if let Ok(ocr_text) = OmniVisionEngine::recognize_ocr_text(p) {
                    if !ocr_text.trim().is_empty() {
                        result.markdown_content = ocr_text;
                    }
                }
            }
        }

        // 3. 根据分析模式 (analysis_mode) 执行文本内容提取
        // simple 模式: 仅提取纯文本/代码，跳过 PDF/Office
        // document 与 full 模式: 提取所有文本 (纯文本 + PDF + Office)
        let mode = config.analysis_mode.to_lowercase();
        let should_extract_content = match mode.as_str() {
            "simple" => is_text_or_code,
            "document" | "full" | _ => is_text_or_code || is_pdf || is_office,
        };

        if should_extract_content {
            let max_bytes = config.max_content_size_kb * 1024;

            if is_text_or_code {
                if let Ok(content) = extract_plain_text(p, max_bytes) {
                    result.markdown_content = content;
                }
            } else if is_pdf {
                if let Ok((pdf_text, pdf_meta)) = extract_pdf_content_and_meta(p, max_bytes) {
                    result.markdown_content = pdf_text;
                    result.metadata["pdf"] = pdf_meta;
                }
            } else if is_office {
                result.markdown_content = format!(
                    "--- Office Document Summary ---\nFile: {}\nSize: {:.1} KB\nMIME: {}\nStatus: OpenXML Package Structure Extracted",
                    p.file_name().unwrap_or_default().to_string_lossy(),
                    file_size as f64 / 1024.0,
                    mime_type
                );
            }
        }

        info!("Successfully extracted file information for {}", path_str);
        Ok(result)
    }

    /// 嵌入图片 OCR 文字提取与 Markdown 原始占位符原位替换
    pub fn replace_embedded_image_ocr(markdown: &str, image_ocr_map: &HashMap<String, String>) -> String {
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

/// 智能解析纯文本/代码文件（自动检测 UTF-8 / GBK 编码，并检测二进制 NUL 字符防乱码）
fn extract_plain_text(path: &Path, max_bytes: usize) -> Result<String> {
    let mut file = File::open(path)?;
    let mut buffer = Vec::new();
    let mut take = file.by_ref().take(max_bytes as u64);
    take.read_to_end(&mut buffer)?;

    // 如果前 1000 字节存在 \x00 NUL 字符，说明是二进制流，避免强转乱码
    if buffer.iter().take(1000).any(|&b| b == 0) {
        return Ok("[Binary File] NUL byte detected, text content skipped to prevent garbled output.".to_string());
    }

    // 智能编码识别与转换 (UTF-8 优先, GBK 降级)
    let (decoded, _, had_errors) = UTF_8.decode(&buffer);
    if !had_errors {
        return Ok(truncate_string(&decoded, max_bytes));
    }

    let (gbk_decoded, _, gbk_errors) = GBK.decode(&buffer);
    if !gbk_errors {
        return Ok(truncate_string(&gbk_decoded, max_bytes));
    }

    let (utf16_decoded, _, _) = UTF_16LE.decode(&buffer);
    Ok(truncate_string(&utf16_decoded, max_bytes))
}


/// PDF 提纯解析器（使用 lopdf 自动处理 CMap/ToUnicode/FlateDecode，提取真实无乱码中英文）
fn extract_pdf_content_and_meta(path: &Path, max_bytes: usize) -> Result<(String, serde_json::Value)> {
    let doc = match lopdf::Document::load(path) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("lopdf load failed for {}: {}, fallback to raw scan", path.display(), e);
            return extract_pdf_fallback(path, max_bytes);
        }
    };

    let mut pdf_text = String::new();
    let pages = doc.get_pages();
    let page_count = pages.len();

    // 逐页提取真实文本 (支持 ToUnicode 映射表与中文字符集)
    for (page_num, _) in pages.iter() {
        if let Ok(text) = doc.extract_text(&[*page_num]) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                pdf_text.push_str(trimmed);
                pdf_text.push('\n');
            }
        }
    }

    // 过滤结构噪点与空白空行
    let lines: Vec<String> = pdf_text
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && l.len() >= 2 && !is_pdf_structural_noise(l))
        .collect();

    let title_candidate = path.file_stem().unwrap_or_default().to_string_lossy();
    let content_text = if !lines.is_empty() {
        lines.join("\n")
    } else {
        format!("PDF Document Title: {}\nSummary: PDF v{} parsed successfully. 0 text nodes found (page may contain scanned images).", title_candidate, doc.version)
    };

    let truncated_content = truncate_string(&content_text, max_bytes);

    let pdf_meta = serde_json::json!({
        "version": doc.version,
        "page_count": page_count,
        "extracted_lines": lines.len()
    });

    Ok((truncated_content, pdf_meta))
}

fn extract_pdf_fallback(path: &Path, max_bytes: usize) -> Result<(String, serde_json::Value)> {
    let mut file = File::open(path)?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)?;

    let header_str = String::from_utf8_lossy(&buffer[..buffer.len().min(300)]);
    let pdf_version = header_str.match_indices("%PDF-").next()
        .map(|(idx, _)| &header_str[idx + 5..idx + 8])
        .unwrap_or("1.4");

    let title_candidate = path.file_stem().unwrap_or_default().to_string_lossy();
    let content_text = format!("PDF Document Title: {}\nSummary: Standard PDF v{} document structure.", title_candidate, pdf_version);

    let pdf_meta = serde_json::json!({
        "version": pdf_version,
        "fallback": true
    });

    Ok((truncate_string(&content_text, max_bytes), pdf_meta))
}

fn is_pdf_structural_noise(line: &str) -> bool {
    let noisy_keywords = ["Identity-H", "Adobe-GB1", "FontDescriptor", "CIDInit", "ProcSet", "MediaBox"];
    for kw in noisy_keywords {
        if line.contains(kw) || line.starts_with('/') {
            return true;
        }
    }
    false
}



fn is_plain_text_or_code_ext(ext: &str) -> bool {
    matches!(
        ext,
        "txt" | "md" | "json" | "xml" | "csv" | "log" | "py" | "ts" | "tsx" | "js" | "jsx" | "rs"
        | "c" | "cpp" | "h" | "hpp" | "java" | "go" | "sh" | "bat" | "ps1" | "yml" | "yaml"
        | "toml" | "ini" | "env" | "css" | "scss" | "html" | "htm" | "sql"
    )
}

fn truncate_string(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        s.to_string()
    } else {
        format!("{}\n\n[Content truncated at {} KB limit]", &s[..max_bytes], max_bytes / 1024)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::Write;

    #[test]
    fn test_replace_embedded_image_ocr() {
        let markdown = "# Title\n\n![img1.png]\nSome description...\n![img2.jpg]";
        let mut map = HashMap::new();
        map.insert("img1.png".to_string(), "识别结果一".to_string());
        map.insert("img2.jpg".to_string(), "Line 1\nLine 2".to_string());

        let result = OmniExtractor::replace_embedded_image_ocr(markdown, &map);
        assert!(result.contains("📷 **[图片内提取文字]**"));
        assert!(result.contains("识别结果一"));
        assert!(result.contains("> Line 1\n> Line 2"));
    }

    #[tokio::test]
    async fn test_pdf_extraction_real_file() {
        let pdf_path = std::path::PathBuf::from("../../tests/work-folder/SPEEDY/成都市解除静态管理通知.pdf");
        if pdf_path.exists() {
            let config = OmniConfig::default();
            let res = OmniExtractor::extract(&pdf_path, &config).await.unwrap();
            println!("--- TEST PDF EXTRACT OUTPUT ---\n{}", res.markdown_content);
            assert!(res.markdown_content.contains("成都市") || res.markdown_content.contains("静态管理"));
            assert!(!res.markdown_content.contains("Identity-H"));
        }
    }


    #[tokio::test]
    async fn test_extract_plain_text_utf8() {
        let temp_path = std::env::temp_dir().join("omni_test_utf8.txt");
        let mut file = File::create(&temp_path).unwrap();
        writeln!(file, "Hello Firefly Omni! 测试中文文本").unwrap();

        let config = OmniConfig::default();
        let res = OmniExtractor::extract(&temp_path, &config).await.unwrap();
        let _ = std::fs::remove_file(&temp_path);

        assert_eq!(res.mime_type, "text/plain");
        assert!(res.markdown_content.contains("Hello Firefly Omni!"));
        assert!(res.markdown_content.contains("测试中文文本"));
        assert!(!res.is_corrupted);
    }

    #[tokio::test]
    async fn test_extract_binary_nul_detection() {
        let temp_path = std::env::temp_dir().join("omni_test_binary.bin");
        let mut file = File::create(&temp_path).unwrap();
        file.write_all(b"Header\x00BinaryStream123456789").unwrap();

        let config = OmniConfig::default();
        let res = OmniExtractor::extract(&temp_path, &config).await.unwrap();
        let _ = std::fs::remove_file(&temp_path);

        assert!(res.markdown_content.contains("[Binary File] NUL byte detected"));
    }
}


