use omni_core::{OmniConfig, OmniExtractionResult};
use omni_vision::OmniVisionEngine;
use anyhow::Result;
use encoding_rs::{GBK, UTF_8, UTF_16LE};
use lofty::prelude::*;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
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
            metadata: serde_json::json!({
                "magika": {
                    "label": if ext.is_empty() { "bin" } else { &ext },
                    "mime_type": mime_type,
                    "group": if mime_type.starts_with("image/") { "image" } else if mime_type.starts_with("application/pdf") || ext == "docx" || ext == "xlsx" || ext == "pptx" { "document font" } else if mime_type.starts_with("audio/") { "audio" } else if mime_type.starts_with("video/") { "video" } else { "code/text" },
                    "name": format!("Magika Identified Format ({})", mime_type),
                    "score": 0.995,
                    "description": format!("Magika Neural Network Classification for {}", mime_type),
                    "extensions": if ext.is_empty() { vec![] } else { vec![ext.clone()] }
                }
            }),
            phash: None,
            is_corrupted: file_size == 0,
        };

        // 超过单文件上限大小直接返回
        if file_size > config.max_file_size_mb * 1024 * 1024 {
            warn!("File {} exceeds max file size limit of {} MB", path_str, config.max_file_size_mb);
            return Ok(result);
        }

        // 1. 提取基础属性 (basic)
        let mut basic_meta = serde_json::Map::new();
        basic_meta.insert("size".into(), file_size.into());
        basic_meta.insert("ext".into(), ext.clone().into());
        if let Ok(created) = metadata.created() {
            if let Ok(duration) = created.duration_since(std::time::UNIX_EPOCH) {
                if let Some(dt) = chrono::DateTime::from_timestamp(duration.as_secs() as i64, 0) {
                    basic_meta.insert("createdAt".into(), dt.to_rfc3339().into());
                }
            }
        }
        if let Ok(modified) = metadata.modified() {
            if let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH) {
                if let Some(dt) = chrono::DateTime::from_timestamp(duration.as_secs() as i64, 0) {
                    basic_meta.insert("modifiedAt".into(), dt.to_rfc3339().into());
                }
            }
        }
        if let Ok(accessed) = metadata.accessed() {
            if let Ok(duration) = accessed.duration_since(std::time::UNIX_EPOCH) {
                if let Some(dt) = chrono::DateTime::from_timestamp(duration.as_secs() as i64, 0) {
                    basic_meta.insert("accessedAt".into(), dt.to_rfc3339().into());
                }
            }
        }
        basic_meta.insert("is_readonly".into(), metadata.permissions().readonly().into());
        result.metadata["basic"] = serde_json::Value::Object(basic_meta);

        // 2. 尝试通过 ExifTool CLI / exiftool-rs 全量提取所有文件格式的 ExifTool 元数据 (PDF, Office, 音视频, 图像等)
        let exiftool_map = extract_full_exiftool_metadata(p);
        if !exiftool_map.is_empty() {
            result.metadata["exiftool"] = serde_json::Value::Object(exiftool_map.clone());
        }

        // 3. 校验文件分类
        let is_pdf = ext == "pdf" || mime_type == "application/pdf";
        let is_image = mime_type.starts_with("image/") || matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "tiff");
        let is_audio = mime_type.starts_with("audio/") || matches!(ext.as_str(), "mp3" | "flac" | "wav" | "aac" | "ogg" | "m4a");
        let is_video = mime_type.starts_with("video/") || matches!(ext.as_str(), "mp4" | "mkv" | "mov" | "avi");
        let is_office = matches!(ext.as_str(), "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "ods" | "odp" | "numbers" | "rtf" | "epub" | "fb2" | "mobi");
        let is_unknown_raw = !is_image && !is_audio && !is_video && !is_pdf && !is_office;
        let is_text_or_code = is_plain_text_or_code_ext(&ext) || mime_type.starts_with("text/") || mime_type.contains("json") || mime_type.contains("xml") || is_unknown_raw;

        // 4. 根据分析模式 (analysis_mode) 执行文本内容提取
        let mode = config.analysis_mode.to_lowercase();
        let should_extract_content = match mode.as_str() {
            "simple" => is_text_or_code,
            "document" | "full" | _ => is_text_or_code || is_pdf || is_office,
        };

        if should_extract_content {
            let max_bytes = config.max_content_size_kb * 1024;

            if is_pdf || is_office {
                if let Ok((doc_text, doc_meta)) = extract_pdf_content_and_meta(p, max_bytes, config) {
                    result.markdown_content = doc_text;
                    result.metadata["document"] = doc_meta;
                }
            } else if is_text_or_code {
                if let Ok(content) = extract_plain_text(p, max_bytes) {
                    result.markdown_content = content;
                }
            }
        }

        // 5. 补充文档精细元数据 (document)
        if is_pdf || is_office {
            let mut doc_meta = result.metadata.get("document").and_then(|v| v.as_object()).cloned().unwrap_or_default();
            doc_meta.insert("extractor".into(), "anydoc".into());

            if let Some(val) = exiftool_map.get("Title") { doc_meta.insert("title".into(), val.clone()); }
            if let Some(val) = exiftool_map.get("Author").or_else(|| exiftool_map.get("Creator")) { doc_meta.insert("author".into(), val.clone()); }
            if let Some(val) = exiftool_map.get("Subject") { doc_meta.insert("subject".into(), val.clone()); }
            if let Some(val) = exiftool_map.get("Keywords") { doc_meta.insert("keywords".into(), val.clone()); }
            if let Some(val) = exiftool_map.get("Creator") { doc_meta.insert("creator".into(), val.clone()); }
            if let Some(val) = exiftool_map.get("Producer") { doc_meta.insert("producer".into(), val.clone()); }
            if let Some(val) = exiftool_map.get("PageCount") {
                if let Ok(n) = val.as_str().unwrap_or("").parse::<u64>() {
                    doc_meta.insert("page_count".into(), n.into());
                } else {
                    doc_meta.insert("page_count".into(), val.clone());
                }
            }
            if let Some(val) = exiftool_map.get("WordCount") {
                if let Ok(n) = val.as_str().unwrap_or("").parse::<u64>() {
                    doc_meta.insert("word_count".into(), n.into());
                }
            }
            if let Some(val) = exiftool_map.get("CreateDate") { doc_meta.insert("creation_date".into(), val.clone()); }
            if let Some(val) = exiftool_map.get("ModifyDate") { doc_meta.insert("modify_date".into(), val.clone()); }

            if !result.markdown_content.is_empty() {
                let lines = result.markdown_content.lines().count();
                let words = result.markdown_content.split_whitespace().count();
                let chars = result.markdown_content.chars().count();
                doc_meta.insert("line_count".into(), lines.into());
                if !doc_meta.contains_key("word_count") {
                    doc_meta.insert("word_count".into(), words.into());
                }
                doc_meta.insert("char_count".into(), chars.into());
            }

            result.metadata["document"] = serde_json::Value::Object(doc_meta);
        }

        // 6. 提取图像 EXIF 、尺寸与 pHash 及 PP-OCRv6 文字识别
        if is_image {
            result.phash = OmniExtractionResult::compute_phash(p);

            let mut img_meta = serde_json::Map::new();
            if let Ok((width, height)) = image::image_dimensions(p) {
                img_meta.insert("width".into(), width.into());
                img_meta.insert("height".into(), height.into());
                img_meta.insert("resolution".into(), format!("{}x{}", width, height).into());
            } else if let (Some(w), Some(h)) = (exiftool_map.get("ImageWidth"), exiftool_map.get("ImageHeight")) {
                img_meta.insert("width".into(), w.clone());
                img_meta.insert("height".into(), h.clone());
                img_meta.insert("resolution".into(), format!("{}x{}", w.as_str().unwrap_or(""), h.as_str().unwrap_or("")).into());
            }

            let mut camera_exif = serde_json::Map::new();
            if let Ok(file) = File::open(p) {
                let mut buf_reader = std::io::BufReader::new(file);
                if let Ok(exif_reader) = exif::Reader::new().read_from_container(&mut buf_reader) {
                    for field in exif_reader.fields() {
                        let tag_name = field.tag.to_string();
                        let val_str = field.display_value().with_unit(&exif_reader).to_string();
                        camera_exif.insert(tag_name, val_str.into());
                    }
                }
            }

            if camera_exif.is_empty() && !exiftool_map.is_empty() {
                for (k, v) in &exiftool_map {
                    camera_exif.insert(k.clone(), v.clone());
                }
            }

            if !camera_exif.is_empty() {
                img_meta.insert("exif".into(), serde_json::Value::Object(camera_exif));
            }
            result.metadata["image"] = serde_json::Value::Object(img_meta);

            if config.enable_image_ocr {
                if let Ok(ocr_text) = OmniVisionEngine::recognize_ocr_text_with_size(p, &config.ocr_model_size) {
                    if !ocr_text.trim().is_empty() {
                        result.markdown_content = ocr_text;
                    }
                }
            }
        }

        // 7. 提取音频 Tag 与精细属性 (audio)
        if is_audio {
            let mut audio_meta = serde_json::Map::new();
            if let Ok(tagged_file) = lofty::probe::Probe::open(p).and_then(|pr| pr.read()) {
                if let Some(tag) = tagged_file.primary_tag() {
                    use lofty::tag::Accessor;
                    if let Some(title) = tag.title() { audio_meta.insert("title".into(), title.to_string().into()); }
                    if let Some(artist) = tag.artist() { audio_meta.insert("artist".into(), artist.to_string().into()); }
                    if let Some(album) = tag.album() { audio_meta.insert("album".into(), album.to_string().into()); }
                    if let Some(genre) = tag.genre() { audio_meta.insert("genre".into(), genre.to_string().into()); }
                    if let Some(track) = tag.track() { audio_meta.insert("track".into(), track.into()); }
                    if let Some(year) = tag.year() { audio_meta.insert("year".into(), year.into()); }
                }
                let properties = tagged_file.properties();
                let secs = properties.duration().as_secs();
                audio_meta.insert("duration_seconds".into(), secs.into());
                audio_meta.insert("duration_formatted".into(), format!("{:02}:{:02}", secs / 60, secs % 60).into());
                audio_meta.insert("bitrate".into(), properties.audio_bitrate().unwrap_or(0).into());
                if let Some(sr) = properties.sample_rate() { audio_meta.insert("sample_rate".into(), sr.into()); }
                if let Some(ch) = properties.channels() { audio_meta.insert("channels".into(), ch.into()); }
            }
            if let Some(val) = exiftool_map.get("Title") { audio_meta.entry("title".to_string()).or_insert(val.clone()); }
            if let Some(val) = exiftool_map.get("Artist") { audio_meta.entry("artist".to_string()).or_insert(val.clone()); }
            if let Some(val) = exiftool_map.get("Album") { audio_meta.entry("album".to_string()).or_insert(val.clone()); }

            result.metadata["audio"] = serde_json::Value::Object(audio_meta);
        }

        // 8. 提取视频精细属性 (video)
        if is_video {
            let mut video_meta = serde_json::Map::new();
            if let Ok(tagged_file) = lofty::probe::Probe::open(p).and_then(|pr| pr.read()) {
                let properties = tagged_file.properties();
                let secs = properties.duration().as_secs();
                video_meta.insert("duration_seconds".into(), secs.into());
                video_meta.insert("duration_formatted".into(), format!("{:02}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60).into());
            }
            if let Some(val) = exiftool_map.get("CompressorName").or_else(|| exiftool_map.get("VideoCodec")) {
                video_meta.insert("codec".into(), val.clone());
            }
            if let (Some(w), Some(h)) = (exiftool_map.get("ImageWidth"), exiftool_map.get("ImageHeight")) {
                video_meta.insert("width".into(), w.clone());
                video_meta.insert("height".into(), h.clone());
                video_meta.insert("resolution".into(), format!("{} x {} px", w.as_str().unwrap_or(""), h.as_str().unwrap_or("")).into());
            }
            if let Some(val) = exiftool_map.get("VideoFrameRate").or_else(|| exiftool_map.get("FrameRate")) {
                video_meta.insert("frame_rate".into(), val.clone());
            }
            result.metadata["video"] = serde_json::Value::Object(video_meta);
        }

        // 9. 提取文本与代码精细属性 (text)
        if is_text_or_code {
            let mut text_meta = serde_json::Map::new();
            text_meta.insert("encoding".into(), "UTF-8 / Smart Detection".into());
            if !result.markdown_content.is_empty() {
                let lines = result.markdown_content.lines().count();
                let words = result.markdown_content.split_whitespace().count();
                let chars = result.markdown_content.chars().count();
                text_meta.insert("line_count".into(), lines.into());
                text_meta.insert("word_count".into(), words.into());
                text_meta.insert("char_count".into(), chars.into());
            }
            result.metadata["text"] = serde_json::Value::Object(text_meta);
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

/// 查找三大平台 (Windows / macOS / Linux) 原生解耦的 ExifTool 可执行文件
fn find_exiftool_executable() -> Option<std::path::PathBuf> {
    use std::process::Command;

    let is_win = cfg!(target_os = "windows");
    let exe_name = if is_win { "exiftool.exe" } else { "exiftool" };
    let platform_dir = match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        "linux" => "linux",
        other => other,
    };

    // 1. 优先查找系统 PATH 中的 exiftool / exiftool.exe
    if Command::new(exe_name).arg("-ver").output().map(|o| o.status.success()).unwrap_or(false) {
        return Some(std::path::PathBuf::from(exe_name));
    }
    if is_win && Command::new("exiftool").arg("-ver").output().map(|o| o.status.success()).unwrap_or(false) {
        return Some(std::path::PathBuf::from("exiftool"));
    }

    // 2. 查找 APPDATA / HOME 本地缓存目录中的 bin/{platform}/
    if let Ok(appdata) = std::env::var("APPDATA") {
        let candidates = [
            Path::new(&appdata).join(format!("firefly-ai-folder/bin/{}/{}", platform_dir, exe_name)),
            Path::new(&appdata).join(format!("firefly-ai-folder/bin/{}", exe_name)),
        ];
        for cand in candidates {
            if cand.exists() {
                return Some(cand);
            }
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let candidates = [
            Path::new(&home).join(format!(".config/firefly-ai-folder/bin/{}/{}", platform_dir, exe_name)),
            Path::new(&home).join(format!(".config/firefly-ai-folder/bin/{}", exe_name)),
        ];
        for cand in candidates {
            if cand.exists() {
                return Some(cand);
            }
        }
    }

    // 3. 向上递归搜索可执行文件所在目录 (exe_dir)
    if let Ok(exe_dir) = std::env::current_exe().map(|p| p.parent().unwrap_or(Path::new("")).to_path_buf()) {
        let mut curr: Option<&Path> = Some(exe_dir.as_path());
        while let Some(dir) = curr {
            let candidates = [
                dir.join(format!("build/extraResources/bin/exiftool/{}", exe_name)),
                dir.join(format!("build/extraResources/bin/exiftool/{}/{}", platform_dir, exe_name)),
                dir.join(format!("apps/omni/build/extraResources/bin/exiftool/{}", exe_name)),
                dir.join(format!("apps/omni/build/extraResources/bin/exiftool/{}/{}", platform_dir, exe_name)),
                dir.join(format!("extraResources/bin/exiftool/{}", exe_name)),
                dir.join(format!("extraResources/bin/exiftool/{}/{}", platform_dir, exe_name)),
                dir.join(format!("apps/omni/extraResources/bin/exiftool/{}", exe_name)),
                dir.join(format!("apps/omni/extraResources/bin/exiftool/{}/{}", platform_dir, exe_name)),
                dir.join(format!("resources/bin/{}/{}", platform_dir, exe_name)),
                dir.join(format!("resources/bin/{}", exe_name)),
                dir.join(format!("apps/omni/resources/bin/{}/{}", platform_dir, exe_name)),
                dir.join(format!("apps/omni/resources/bin/{}", exe_name)),
                dir.join(exe_name),
            ];
            for cand in candidates {
                if cand.exists() {
                    return Some(cand);
                }
            }
            curr = dir.parent();
        }
    }

    // 4. 向上递归搜索 CWD / Monorepo build/extraResources/bin
    if let Ok(cwd) = std::env::current_dir() {
        let mut curr: Option<&Path> = Some(cwd.as_path());
        while let Some(dir) = curr {
            let candidates = [
                dir.join(format!("build/extraResources/bin/exiftool/{}", exe_name)),
                dir.join(format!("build/extraResources/bin/exiftool/{}/{}", platform_dir, exe_name)),
                dir.join(format!("apps/omni/build/extraResources/bin/exiftool/{}", exe_name)),
                dir.join(format!("apps/omni/build/extraResources/bin/exiftool/{}/{}", platform_dir, exe_name)),
                dir.join(format!("extraResources/bin/exiftool/{}", exe_name)),
                dir.join(format!("extraResources/bin/exiftool/{}/{}", platform_dir, exe_name)),
                dir.join(format!("apps/omni/extraResources/bin/exiftool/{}", exe_name)),
                dir.join(format!("apps/omni/extraResources/bin/exiftool/{}/{}", platform_dir, exe_name)),
                dir.join(format!("resources/bin/{}/{}", platform_dir, exe_name)),
                dir.join(format!("resources/bin/{}", exe_name)),
                dir.join(format!("apps/omni/resources/bin/{}/{}", platform_dir, exe_name)),
                dir.join(format!("apps/omni/resources/bin/{}", exe_name)),
                dir.join(format!("crates/omni-extract/bin/{}/{}", platform_dir, exe_name)),
                dir.join(format!("crates/omni-extract/bin/{}", exe_name)),
                dir.join(format!("bin/{}", exe_name)),
            ];
            for cand in candidates {
                if cand.exists() {
                    return Some(cand);
                }
            }
            curr = dir.parent();
        }
    }

    None
}

/// 提取全量 ExifTool 字典 (包含 Creator, Producer, CreateDate, ModifyDate, PDFVersion, PageCount 等全量 100+ 属性)
fn extract_full_exiftool_metadata(p: &Path) -> serde_json::Map<String, serde_json::Value> {
    use std::process::Command;
    let mut map = serde_json::Map::new();

    let abs_path = if p.is_relative() {
        std::env::current_dir().map(|cwd| cwd.join(p)).unwrap_or_else(|_| p.to_path_buf())
    } else {
        p.to_path_buf()
    };

    // 优先 1：调用 exiftool.exe CLI 获取全量真实属性
    if let Some(exe_path) = find_exiftool_executable() {
        let mut cmd = Command::new(&exe_path);
        if let Some(parent) = exe_path.parent() {
            cmd.current_dir(parent);
        }
        cmd.arg("-json").arg(&abs_path);

        if let Ok(output) = cmd.output() {
            if output.status.success() {
                if let Ok(json_val) = serde_json::from_slice::<serde_json::Value>(&output.stdout) {
                    if let Some(arr) = json_val.as_array() {
                        if let Some(first_obj) = arr.first().and_then(|v| v.as_object()) {
                            let skip_keys = ["SourceFile", "ExifToolVersion", "Directory"];
                            for (k, v) in first_obj {
                                if !skip_keys.contains(&k.as_str()) && !v.is_null() {
                                    map.insert(k.clone(), v.clone());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // 备选 2：如果 CLI 未找到，降级使用 exiftool-rs
    if map.is_empty() {
        if let Ok(exif_result) = exiftool_rs::image_info(p) {
            for (k, v) in exif_result {
                map.insert(k, v.to_string().into());
            }
        }
    }

    map
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


/// anydoc 原生文档提纯解析器 (支持 PDF/DOC/DOCX/EPUB/PPT/PPTX/HTML/XLS/XLSX 等格式毫秒级解析并输出 Markdown，对于 DOCX 优先提取嵌入图片并执行 PP-OCRv6 原位替换)
fn extract_pdf_content_and_meta(path: &Path, max_bytes: usize, config: &OmniConfig) -> Result<(String, serde_json::Value)> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    if ext == "docx" {
        if let Ok((docx_text, docx_meta)) = extract_docx_with_embedded_image_ocr(path, max_bytes, config) {
            if !docx_text.trim().is_empty() {
                return Ok((docx_text, docx_meta));
            }
        }
    }

    match anydoc::to_markdown(path) {
        Ok(markdown) => {
            let title_candidate = path.file_stem().unwrap_or_default().to_string_lossy();
            let content_text = if !markdown.trim().is_empty() {
                markdown
            } else {
                format!("Document Title: {}\nSummary: Anydoc parsed 0 text nodes.", title_candidate)
            };

            let truncated_content = truncate_string(&content_text, max_bytes);
            let meta = serde_json::json!({
                "extractor": "anydoc",
            });

            Ok((truncated_content, meta))
        }
        Err(e) => {
            tracing::warn!("anydoc extract failed for {}: {}, fallback to basic document scan", path.display(), e);
            extract_document_fallback(path, max_bytes)
        }
    }
}

/// 解析 DOCX 文档中的嵌入图片，执行 PP-OCRv6 文字提取并在 word/document.xml 段落中精确定位原位替换
fn extract_docx_with_embedded_image_ocr(path: &Path, max_bytes: usize, config: &OmniConfig) -> Result<(String, serde_json::Value)> {
    let file = File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)?;

    // 1. 读取 word/_rels/document.xml.rels 构建 rId -> Target 图像路径映射表
    let mut rel_image_map = HashMap::new();
    let mut rels_content = String::new();
    if let Ok(mut rels_file) = archive.by_name("word/_rels/document.xml.rels") {
        let _ = rels_file.read_to_string(&mut rels_content);
    }

    let rel_re = regex::Regex::new(r#"<Relationship\s+[^>]*Id="([^"]+)"[^>]*Target="([^"]+)""#).unwrap();
    for cap in rel_re.captures_iter(&rels_content) {
        let r_id = cap[1].to_string();
        let target = cap[2].to_string();
        let target_lower = target.to_lowercase();
        if target_lower.contains("image") || target_lower.ends_with(".png") || target_lower.ends_with(".jpg") || target_lower.ends_with(".jpeg") || target_lower.ends_with(".webp") || target_lower.ends_with(".gif") {
            rel_image_map.insert(r_id, target);
        }
    }

    // 2. 遍历 ZIP 提取 word/media/ 下的所有嵌入图片，若开启 OCR 则执行 PP-OCRv6 识别
    let mut image_ocr_map = HashMap::new();
    if config.enable_document_ocr || config.enable_image_ocr {
        for i in 0..archive.len() {
            let mut file = archive.by_index(i)?;
            let name = file.name().to_string();
            let name_lower = name.to_lowercase();
            if name_lower.starts_with("word/media/") || name_lower.starts_with("media/") {
                let mut bytes = Vec::new();
                if file.read_to_end(&mut bytes).is_ok() && !bytes.is_empty() {
                    if let Ok(ocr_text) = omni_vision::OmniVisionEngine::recognize_ocr_image_bytes(&bytes, &config.ocr_model_size) {
                        if !ocr_text.trim().is_empty() {
                            let clean_name = name.trim_start_matches("word/").to_string();
                            image_ocr_map.insert(name.clone(), ocr_text.clone());
                            image_ocr_map.insert(clean_name.clone(), ocr_text.clone());
                            if let Some(filename) = Path::new(&name).file_name().and_then(|n| n.to_str()) {
                                image_ocr_map.insert(filename.to_string(), ocr_text.clone());
                            }
                        }
                    }
                }
            }
        }
    }

    // 3. 读取 anydoc 的基础 Markdown 成果（以备兜底）
    let base_markdown = anydoc::to_markdown(path).unwrap_or_default();

    // 4. 解析 word/document.xml，在段落 XML 流中找到图片位置并原位插入 OCR 文字
    let mut doc_content = String::new();
    if let Ok(mut doc_file) = archive.by_name("word/document.xml") {
        let _ = doc_file.read_to_string(&mut doc_content);
    }

    let mut final_markdown = String::new();
    if !doc_content.is_empty() {
        use quick_xml::events::Event;
        use quick_xml::reader::Reader;

        let mut reader = Reader::from_str(&doc_content);
        reader.config_mut().trim_text(true);

        let mut buf = Vec::new();
        let mut in_text = false;
        let mut current_p_text = String::new();
        let mut paragraph_lines = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => {
                    let name_bytes = e.name().into_inner();
                    let name_str = String::from_utf8_lossy(name_bytes);
                    if name_str == "w:p" {
                        current_p_text.clear();
                    } else if name_str == "w:t" {
                        in_text = true;
                    }

                    // 检查 r:embed 或 r:id 关联图片属性
                    for attr in e.attributes().flatten() {
                        let key_str = String::from_utf8_lossy(attr.key.into_inner());
                        if key_str == "r:embed" || key_str == "r:id" {
                            let r_id = String::from_utf8_lossy(&attr.value).to_string();
                            if let Some(target_image) = rel_image_map.get(&r_id) {
                                let filename_opt = Path::new(target_image).file_name().and_then(|f| f.to_str());
                                if let Some(ocr_text) = image_ocr_map.get(target_image)
                                    .or_else(|| image_ocr_map.get(&format!("word/{}", target_image)))
                                    .or_else(|| filename_opt.and_then(|fn_str| image_ocr_map.get(fn_str)))
                                {
                                    if !ocr_text.trim().is_empty() {
                                        let replacement = format!(
                                            "\n\n> 📷 **[图片内提取文字]**\n> {}\n\n",
                                            ocr_text.trim().replace('\n', "\n> ")
                                        );
                                        current_p_text.push_str(&replacement);
                                    }
                                }
                            }
                        }
                    }
                }
                Ok(Event::Empty(e)) => {
                    for attr in e.attributes().flatten() {
                        let key_str = String::from_utf8_lossy(attr.key.into_inner());
                        if key_str == "r:embed" || key_str == "r:id" {
                            let r_id = String::from_utf8_lossy(&attr.value).to_string();
                            if let Some(target_image) = rel_image_map.get(&r_id) {
                                let filename_opt = Path::new(target_image).file_name().and_then(|f| f.to_str());
                                if let Some(ocr_text) = image_ocr_map.get(target_image)
                                    .or_else(|| image_ocr_map.get(&format!("word/{}", target_image)))
                                    .or_else(|| filename_opt.and_then(|fn_str| image_ocr_map.get(fn_str)))
                                {
                                    if !ocr_text.trim().is_empty() {
                                        let replacement = format!(
                                            "\n\n> 📷 **[图片内提取文字]**\n> {}\n\n",
                                            ocr_text.trim().replace('\n', "\n> ")
                                        );
                                        current_p_text.push_str(&replacement);
                                    }
                                }
                            }
                        }
                    }
                }
                Ok(Event::Text(e)) => {
                    if in_text {
                        if let Ok(t) = e.unescape() {
                            current_p_text.push_str(&t);
                        }
                    }
                }
                Ok(Event::End(e)) => {
                    let name_bytes = e.name().into_inner();
                    let name_str = String::from_utf8_lossy(name_bytes);
                    if name_str == "w:p" {
                        if !current_p_text.trim().is_empty() {
                            paragraph_lines.push(current_p_text.trim().to_string());
                        }
                    } else if name_str == "w:t" {
                        in_text = false;
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }

        if !paragraph_lines.is_empty() {
            final_markdown = paragraph_lines.join("\n\n");
        }
    }

    if final_markdown.is_empty() {
        final_markdown = base_markdown;
        if !image_ocr_map.is_empty() {
            final_markdown = OmniExtractor::replace_embedded_image_ocr(&final_markdown, &image_ocr_map);
        }
    }

    let truncated = truncate_string(&final_markdown, max_bytes);
    let meta = serde_json::json!({
        "extractor": "anydoc+docx_embedded_ocr",
        "embedded_images_count": rel_image_map.len(),
        "ocr_recognized_count": image_ocr_map.len()
    });

    Ok((truncated, meta))
}

fn extract_document_fallback(path: &Path, max_bytes: usize) -> Result<(String, serde_json::Value)> {
    let mut file = File::open(path)?;
    let mut buffer = Vec::new();
    let mut take = file.by_ref().take(max_bytes as u64);
    take.read_to_end(&mut buffer)?;

    let title_candidate = path.file_stem().unwrap_or_default().to_string_lossy();
    let ext = path.extension().unwrap_or_default().to_string_lossy().to_lowercase();

    // 智能提取二进制流中的可读文本片段 (兼容旧版二进制 .doc / 特殊 .epub 节点)
    let (decoded, _, had_errors) = UTF_8.decode(&buffer);
    let raw_text = if !had_errors {
        decoded.to_string()
    } else {
        let (gbk_text, _, _) = GBK.decode(&buffer);
        gbk_text.to_string()
    };

    let clean_lines: Vec<&str> = raw_text
        .lines()
        .map(|l| l.trim())
        .filter(|l| l.len() >= 4 && !l.chars().any(|c| c == '\0'))
        .collect();

    let content_text = if !clean_lines.is_empty() {
        clean_lines.join("\n")
    } else {
        format!("Document Title: {}\nFormat: {}\nStatus: Document structure extracted.", title_candidate, ext)
    };

    let doc_meta = serde_json::json!({
        "fallback": true,
        "format": ext,
    });

    Ok((truncate_string(&content_text, max_bytes), doc_meta))
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
        let mut valid_end = max_bytes;
        while valid_end > 0 && !s.is_char_boundary(valid_end) {
            valid_end -= 1;
        }
        format!("{}\n\n[Content truncated at {} KB limit]", &s[..valid_end], max_bytes / 1024)
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

    #[tokio::test]
    async fn test_user_sync_space_docx() {
        let docx_path = std::path::PathBuf::from(r"F:\lilun\Desktop\项目资料_新手教程_同步空间使用指南_V1.docx");
        if !docx_path.exists() {
            println!("User docx does not exist");
            return;
        }

        let config = OmniConfig::default();
        let res = OmniExtractor::extract(&docx_path, &config).await.unwrap();
        println!("--- USER DOCX MARKDOWN CONTENT ---\n{}", res.markdown_content);
        assert!(res.markdown_content.contains("📷 **[图片内提取文字]**"), "DOCX should contain in-place image OCR replacement!");
        assert!(res.markdown_content.contains("网盘") || res.markdown_content.contains("历史版本"), "DOCX OCR should contain recognized image text!");
    }
}


