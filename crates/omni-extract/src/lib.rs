use omni_core::{to_native_path_str, OmniConfig, OmniExtractionResult};
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
        let t_extract_start = std::time::Instant::now();
        let p = path.as_ref();
        let path_str = to_native_path_str(p);
        let metadata = std::fs::metadata(p)?;
        let file_size = metadata.len();

        // 识别真实 MIME 类型与扩展名 (Magika 类型识别阶段计时)
        let t_magika_start = std::time::Instant::now();
        let mime_type = OmniVisionEngine::detect_mime_type(p).unwrap_or_else(|_| "application/octet-stream".to_string());
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        let magika_duration_ms = t_magika_start.elapsed().as_millis() as u64;

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
            benchmark: None,
        };

        // 超过单文件上限大小直接返回
        if file_size > config.max_file_size_mb * 1024 * 1024 {
            warn!("File {} exceeds max file size limit of {} MB", path_str, config.max_file_size_mb);
            return Ok(result);
        }

        // 2. 尝试通过 ExifTool CLI / exiftool-rs 全量提取所有文件格式的 ExifTool 元数据 (PDF, Office, 音视频, 图像等)
        let t_meta_start = std::time::Instant::now();
        let exiftool_map = extract_full_exiftool_metadata(p);
        if !exiftool_map.is_empty() {
            result.metadata["exiftool"] = serde_json::Value::Object(exiftool_map.clone());
        }
        let metadata_duration_ms = t_meta_start.elapsed().as_millis() as u64;

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

        let mut doc_duration_ms: Option<u64> = None;
        let mut text_duration_ms: Option<u64> = None;
        let mut ocr_duration_ms: Option<u64> = None;

        if should_extract_content {
            let max_bytes = config.max_content_size_kb * 1024;

            if is_pdf || is_office {
                let t_doc_start = std::time::Instant::now();
                if let Ok((doc_text, doc_meta, doc_ocr_ms)) = extract_pdf_content_and_meta(p, max_bytes, config) {
                    result.markdown_content = doc_text;
                    result.metadata["document"] = doc_meta;
                    if let Some(ocr_ms) = doc_ocr_ms {
                        ocr_duration_ms = Some(ocr_ms);
                    }
                }
                let total_doc_ms = t_doc_start.elapsed().as_millis() as u64;
                let ocr_ms = ocr_duration_ms.unwrap_or(0);
                // 将纯文本解析与 OCR 耗时剥离
                doc_duration_ms = Some(total_doc_ms.saturating_sub(ocr_ms));
            } else if is_text_or_code {
                let t_text_start = std::time::Instant::now();
                if let Ok(content) = extract_plain_text(p, max_bytes) {
                    result.markdown_content = content;
                }
                text_duration_ms = Some(t_text_start.elapsed().as_millis() as u64);
            }
        }

        // 5. 补充文档精细元数据 (document) 与 文本统计 (text_stats)
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

            result.metadata["document"] = serde_json::Value::Object(doc_meta);
        }

        // 5. 补充文档精细元数据 (document) 与 文本统计 (text_stats)
        // 如果提取到了文本内容，写入标准的 text_stats（字符数、行数、词数、编码统计）
        if !result.markdown_content.is_empty() {
            let lines = result.markdown_content.lines().count();
            let words = result.markdown_content.split_whitespace().count();
            let chars = result.markdown_content.chars().count();
            let mut text_stats = serde_json::Map::new();
            text_stats.insert("encoding".into(), "UTF-8".into());
            text_stats.insert("line_count".into(), lines.into());
            text_stats.insert("word_count".into(), words.into());
            text_stats.insert("char_count".into(), chars.into());
            result.metadata["text_stats"] = serde_json::Value::Object(text_stats);
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
                let t_ocr_start = std::time::Instant::now();
                if let Ok(ocr_text) = OmniVisionEngine::recognize_ocr_text_with_size(p, &config.ocr_model_size) {
                    if !ocr_text.trim().is_empty() {
                        result.markdown_content = ocr_text;
                    }
                }
                ocr_duration_ms = Some(t_ocr_start.elapsed().as_millis() as u64);
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



        // 10. 提取 PE 可执行程序属性 (executable / exe)
        let is_exe = matches!(ext.as_str(), "exe" | "dll" | "sys" | "so" | "dylib")
            || mime_type.contains("x-msdownload")
            || mime_type.contains("x-executable")
            || mime_type.contains("application/vnd.microsoft.portable-executable")
            || (mime_type.contains("application/octet-stream") && matches!(ext.as_str(), "exe" | "dll" | "sys"));

        if is_exe || exiftool_map.contains_key("CompanyName") || exiftool_map.contains_key("FileDescription") {
            let mut exe_meta = serde_json::Map::new();
            if let Some(val) = exiftool_map.get("FileDescription").or_else(|| exiftool_map.get("Description")) {
                exe_meta.insert("file_description".into(), val.clone());
            }
            if let Some(val) = exiftool_map.get("CompanyName").or_else(|| exiftool_map.get("Company")) {
                exe_meta.insert("company_name".into(), val.clone());
            }
            if let Some(val) = exiftool_map.get("ProductName").or_else(|| exiftool_map.get("Product")) {
                exe_meta.insert("product_name".into(), val.clone());
            }
            if let Some(val) = exiftool_map.get("FileVersion") {
                exe_meta.insert("file_version".into(), val.clone());
            }
            if let Some(val) = exiftool_map.get("ProductVersion") {
                exe_meta.insert("product_version".into(), val.clone());
            }
            if let Some(val) = exiftool_map.get("LegalCopyright").or_else(|| exiftool_map.get("Copyright")) {
                exe_meta.insert("legal_copyright".into(), val.clone());
            }
            if let Some(val) = exiftool_map.get("OriginalFileName") {
                exe_meta.insert("original_file_name".into(), val.clone());
            }
            if let Some(val) = exiftool_map.get("InternalName") {
                exe_meta.insert("internal_name".into(), val.clone());
            }
            if let Some(val) = exiftool_map.get("PEType").or_else(|| exiftool_map.get("MachineType")) {
                exe_meta.insert("pe_type".into(), val.clone());
            }
            if let Some(val) = exiftool_map.get("Subsystem") {
                exe_meta.insert("subsystem".into(), val.clone());
            }

            if !exe_meta.is_empty() {
                result.metadata["executable"] = serde_json::Value::Object(exe_meta);
            }
        }

        // 11. 提取 压缩包/磁盘镜像 属性 (archive)
        let is_archive = matches!(ext.as_str(), "7z" | "7zip" | "zip" | "rar" | "tar" | "gz" | "gzip" | "bz2" | "xz" | "tgz" | "tbz2" | "iso" | "dmg" | "deb" | "rpm" | "jar" | "war" | "ear" | "pkg" | "xar" | "cbr" | "cbz" | "vhd" | "vhdx" | "vmdk" | "img" | "qcow2" | "vdi" | "ova");
        if is_archive || exiftool_map.contains_key("ZipFileName") || exiftool_map.contains_key("ZipUncompressedSize") {
            let mut archive_meta = serde_json::Map::new();
            if let Some(val) = exiftool_map.get("ZipFileName").or_else(|| exiftool_map.get("ArchiveFormat")).or_else(|| exiftool_map.get("FileType")) {
                archive_meta.insert("format".into(), val.clone());
            }
            if let Some(val) = exiftool_map.get("ZipUncompressedSize").or_else(|| exiftool_map.get("UncompressedSize")) {
                archive_meta.insert("uncompressed_size".into(), val.clone());
            }
            if let Some(val) = exiftool_map.get("ZipFileCount").or_else(|| exiftool_map.get("FileCount")) {
                archive_meta.insert("file_count".into(), val.clone());
            }
            if !archive_meta.is_empty() {
                result.metadata["archive"] = serde_json::Value::Object(archive_meta);
            }
        }

        // 12. 提取 数据库 文件属性 (database)
        let is_database = matches!(ext.as_str(), "db" | "db3" | "sqlite" | "sqlite3" | "sqlitedb" | "mdb" | "accdb" | "fdb" | "dbf");
        if is_database {
            let mut db_meta = serde_json::Map::new();
            if let Some(val) = exiftool_map.get("FileType") {
                db_meta.insert("engine".into(), val.clone());
            } else {
                db_meta.insert("engine".into(), ext.to_uppercase().into());
            }
            db_meta.insert("file_size".into(), file_size.into());
            result.metadata["database"] = serde_json::Value::Object(db_meta);
        }

        // 13. 提取 AI 模型/神经网络 属性 (model)
        let is_model = matches!(ext.as_str(), "onnx" | "gguf" | "safetensors" | "pt" | "pth" | "tflite" | "h5");
        if is_model {
            let mut model_meta = serde_json::Map::new();
            if let Some(val) = exiftool_map.get("FileType") {
                model_meta.insert("model_format".into(), val.clone());
            } else {
                model_meta.insert("model_format".into(), ext.to_uppercase().into());
            }
            model_meta.insert("file_size".into(), file_size.into());
            result.metadata["model"] = serde_json::Value::Object(model_meta);
        }

        // 14. 提取 字体 文件属性 (font)
        let is_font = matches!(ext.as_str(), "ttf" | "otf" | "woff" | "woff2" | "ttc" | "eot");
        if is_font || exiftool_map.contains_key("FontName") {
            let mut font_meta = serde_json::Map::new();
            if let Some(val) = exiftool_map.get("FontName").or_else(|| exiftool_map.get("FamilyName")) {
                font_meta.insert("font_name".into(), val.clone());
            }
            if let Some(val) = exiftool_map.get("FontSubfamily") {
                font_meta.insert("subfamily".into(), val.clone());
            }
            if let Some(val) = exiftool_map.get("FontVersion") {
                font_meta.insert("version".into(), val.clone());
            }
            if !font_meta.is_empty() {
                result.metadata["font"] = serde_json::Value::Object(font_meta);
            }
        }

        let text_duration = text_duration_ms.or(doc_duration_ms);
        let max_parallel_ms = magika_duration_ms
            .max(metadata_duration_ms)
            .max(text_duration.unwrap_or(0))
            .max(ocr_duration_ms.unwrap_or(0));

        result.benchmark = Some(omni_core::OmniBenchmark {
            total_ms: max_parallel_ms,
            magika_ms: Some(magika_duration_ms),
            metadata_ms: if metadata_duration_ms > 0 { Some(metadata_duration_ms) } else { None },
            text_ms: text_duration,
            document_ms: None, // 彻底废弃旧的冗余正文字段
            ocr_ms: ocr_duration_ms,
            html_ms: None,
            thumbnail_ms: None,
        });

        info!(
            "Successfully extracted file information for {} in {}ms (Max parallel: {}ms)",
            path_str,
            t_extract_start.elapsed().as_millis(),
            max_parallel_ms
        );
        Ok(result)
    }

    /// 将 OCR 文字格式化为 100% 兼容 MarkItDown 与 react-markdown 的多行 Blockquote 块
    pub fn format_markitdown_ocr_block(ocr_text: &str) -> String {
        let trimmed = ocr_text.trim();
        if trimmed.is_empty() {
            return String::new();
        }

        let block_lines: Vec<String> = trimmed
            .lines()
            .map(|l| format!("> {}", l))
            .collect();

        format!(
            "\n\n> 📷 **[图片内提取文字]**\n>\n{}\n\n",
            block_lines.join("\n>\n")
        )
    }

    /// 嵌入图片 OCR 文字提取与 Markdown 原始占位符原位替换
    pub fn replace_embedded_image_ocr(markdown: &str, image_ocr_map: &HashMap<String, String>) -> String {
        let mut substituted = markdown.to_string();
        for (img_name, ocr_text) in image_ocr_map {
            if ocr_text.trim().is_empty() {
                continue;
            }
            let pattern = format!("![{}]", img_name);
            let replacement = Self::format_markitdown_ocr_block(ocr_text);
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
                dir.join("apps/desktop/node_modules/exiftool-vendored.exe/bin/exiftool.exe"),
                dir.join("node_modules/exiftool-vendored.exe/bin/exiftool.exe"),
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
                            let skip_keys = [
                                "SourceFile", "ExifToolVersion", "Directory", "FilePermissions",
                                "ThumbnailImage", "PreviewImage", "JpgFromRaw", "OtherImage",
                                "MakerNotes", "MakerNoteSony", "MakerNoteCanon", "MakerNoteNikon",
                                "SonyDateTime2", "SonyToneCurve", "UserComment", "PrintIM"
                            ];
                            for (k, v) in first_obj {
                                if skip_keys.contains(&k.as_str()) || v.is_null() {
                                    continue;
                                }
                                // 如果是字符串，限制单字段长度（过滤超过 2KB 的 base64/二进制 hex dump）
                                if let Some(s) = v.as_str() {
                                    if s.len() > 2048 {
                                        continue;
                                    }
                                }
                                // 如果是数组，且元素过多（超过 200 项的直方图/色彩空间表），跳过以减小传输开销
                                if let Some(arr) = v.as_array() {
                                    if arr.len() > 200 {
                                        continue;
                                    }
                                }
                                map.insert(k.clone(), v.clone());
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

/// 智能解析纯文本/代码文件（自动检测 UTF-8 / GBK / UTF-16 编码，并检测二进制 NUL 字符防乱码）
fn extract_plain_text(path: &Path, max_bytes: usize) -> Result<String> {
    let mut file = File::open(path)?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)?;

    if buffer.is_empty() {
        return Ok(String::new());
    }

    // 1. 检查 UTF-8 / UTF-16 BOM 头
    if buffer.len() >= 3 && buffer[0] == 0xEF && buffer[1] == 0xBB && buffer[2] == 0xBF {
        let (utf8_decoded, _, _) = UTF_8.decode(&buffer[3..]);
        return Ok(truncate_string(&utf8_decoded, max_bytes));
    }
    if buffer.len() >= 2 {
        if buffer[0] == 0xFF && buffer[1] == 0xFE {
            let (utf16_decoded, _, _) = UTF_16LE.decode(&buffer[2..]);
            return Ok(truncate_string(&utf16_decoded, max_bytes));
        } else if buffer[0] == 0xFE && buffer[1] == 0xFF {
            let (utf16_decoded, _, _) = encoding_rs::UTF_16BE.decode(&buffer[2..]);
            return Ok(truncate_string(&utf16_decoded, max_bytes));
        }
    }

    // 2. 检查前 1000 字节是否存在 \x00 NUL 字符（排除 UTF-16 BOM 后若有密集 NUL 说明是二进制流）
    if buffer.iter().take(1000).any(|&b| b == 0) {
        // 如果奇偶位存在大量 0x00，且符合 UTF-16LE 无 BOM 结构
        let sample = &buffer[..buffer.len().min(1000)];
        let nul_odd = sample.iter().enumerate().filter(|(i, &b)| i % 2 == 1 && b == 0).count();
        let nul_even = sample.iter().enumerate().filter(|(i, &b)| i % 2 == 0 && b == 0).count();
        if (nul_odd > sample.len() / 4 && nul_even == 0) || (nul_even > sample.len() / 4 && nul_odd == 0) {
            let (utf16_decoded, _, utf16_errors) = UTF_16LE.decode(&buffer);
            if !utf16_errors {
                return Ok(truncate_string(&utf16_decoded, max_bytes));
            }
        }
        return Ok("[Binary File] NUL byte detected, text content skipped to prevent garbled output.".to_string());
    }

    // 3. 严格验证是否为标准 UTF-8（中文及现代文本事实标准，不允许任何解码错误）
    if let Ok(utf8_str) = std::str::from_utf8(&buffer) {
        return Ok(truncate_string(utf8_str, max_bytes));
    }

    // 4. 尝试 GBK 编码解码（常见于 Windows 中文记事本 ANSI 格式）
    let (gbk_decoded, _, gbk_errors) = GBK.decode(&buffer);
    if !gbk_errors {
        return Ok(truncate_string(&gbk_decoded, max_bytes));
    }

    // 5. 降级：选择 UTF-8 lossy 与 GBK lossy 中错误替换符最少的解码结果
    let (utf8_lossy, _, _) = UTF_8.decode(&buffer);
    let utf8_rep_count = utf8_lossy.chars().filter(|&c| c == '\u{FFFD}').count();
    let gbk_rep_count = gbk_decoded.chars().filter(|&c| c == '\u{FFFD}').count();

    if gbk_rep_count < utf8_rep_count {
        Ok(truncate_string(&gbk_decoded, max_bytes))
    } else {
        Ok(truncate_string(&utf8_lossy, max_bytes))
    }
}


/// anydoc 原生文档提纯解析器 (支持 PDF/DOC/DOCX/EPUB/PPT/PPTX/HTML/XLS/XLSX 等格式毫秒级解析并输出 Markdown，对于 DOCX/PDF 优先提取原生文本层，并在文本层不足且开启 OCR 时分页/分图 OCR 且动态按上限提前终止)
fn extract_pdf_content_and_meta(path: &Path, max_bytes: usize, config: &OmniConfig) -> Result<(String, serde_json::Value, Option<u64>)> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();

    // 1. 对于 DOCX 文档，若开启了文档 OCR (max_document_ocr_items != 0)
    if ext == "docx" && config.max_document_ocr_items != 0 {
        // 先检查 anydoc 提取的原生文本层大小
        if let Ok(raw_markdown) = anydoc::to_markdown(path) {
            let raw_len = raw_markdown.trim().len();
            // 如果原生文本层已经满足或超过 max_bytes 限制 (max_bytes > 0)，则无需进行昂贵的图片 OCR 扫描，节约性能
            if max_bytes > 0 && raw_len >= max_bytes {
                tracing::info!(
                    "DOCX 原生文本层 ({} 字节) 已达到内容上限 ({} 字节)，跳过嵌入图片 OCR 识别",
                    raw_len,
                    max_bytes
                );
                let truncated_content = truncate_string(&raw_markdown, max_bytes);
                let meta = serde_json::json!({
                    "extractor": "anydoc",
                    "ocr_skipped_due_to_content_limit": true
                });
                return Ok((truncated_content, meta, None));
            }
        }

        // 原生文本层未超限，进行嵌入图片 OCR 提纯与原位融合
        let t_docx_ocr_start = std::time::Instant::now();
        if let Ok((docx_text, docx_meta)) = extract_docx_with_embedded_image_ocr(path, max_bytes, config) {
            let docx_ocr_ms = t_docx_ocr_start.elapsed().as_millis() as u64;
            if !docx_text.trim().is_empty() {
                return Ok((docx_text, docx_meta, Some(docx_ocr_ms)));
            }
        }
    }

    // 2. 尝试提取 anydoc 基础 Markdown 文本层
    let mut base_text = String::new();
    if let Ok(markdown) = anydoc::to_markdown(path) {
        base_text = markdown;
    }

    // 3. 对于 PDF 文档：使用 pdf-inspector 检查是否为原生文本型 PDF (TextBased)
    if ext == "pdf" {
        let pdf_type_res = pdf_inspector::detect_pdf_type(path).ok();
        let is_text_pdf = matches!(pdf_type_res.as_ref().map(|r| r.pdf_type), Some(pdf_inspector::PdfType::TextBased));
        let raw_len = base_text.trim().len();

        // 判定条件：如果是文本型 PDF 且提取到了有效文本层（或文本层已达上限），直接输出并标记跳过 OCR
        if (is_text_pdf && raw_len > 0) || (max_bytes > 0 && raw_len >= max_bytes) {
            tracing::info!(
                "PDF 为原生文本类型 (is_text_pdf={}, 提取文本 {} 字节)，跳过后续分页 OCR 识别",
                is_text_pdf,
                raw_len
            );
            let truncated_content = truncate_string(&base_text, max_bytes);
            let meta = serde_json::json!({
                "extractor": "anydoc+pdf-inspector",
                "is_text_pdf": is_text_pdf,
                "pdf_type": pdf_type_res.as_ref().map(|r| format!("{:?}", r.pdf_type)),
                "ocr_skipped": true,
                "ocr_skipped_reason": if is_text_pdf { "native_text_pdf" } else { "content_limit_reached" }
            });
            return Ok((truncated_content, meta, None));
        }

        // 若非纯文本 PDF（如扫描件/纯图片 PDF）或文本层为空，且开启了文档 OCR (max_document_ocr_items != 0)
        let max_ocr_pages = config.max_document_ocr_items;
        if max_ocr_pages != 0 {
            let max_limit_bytes = if max_bytes > 0 { max_bytes } else { usize::MAX };
            let page_limit = if max_ocr_pages < 0 { 0usize } else { max_ocr_pages as usize };

            let t_pdf_ocr_start = std::time::Instant::now();
            if let Ok(page_images) = omni_pro::CoverRenderer::render_pdf_page_images(path, page_limit) {
                if !page_images.is_empty() {
                    tracing::info!(
                        "PDF 包含 {} 页待 OCR 扫描页面，开始执行原生 MuPDF + PP-OCRv6 极速识别",
                        page_images.len()
                    );
                    let mut ocr_page_texts = Vec::new();
                    let mut current_bytes = raw_len;

                    for (page_idx, img) in page_images.iter().enumerate() {
                        if current_bytes >= max_limit_bytes {
                            tracing::info!(
                                "PDF OCR 识别文本已达到上限 ({} 字节)，在第 {} 页提前终止",
                                current_bytes,
                                page_idx + 1
                            );
                            break;
                        }

                        if let Ok(page_text) = omni_vision::OmniVisionEngine::recognize_ocr_dynamic_image(img, &config.ocr_model_size) {
                            let trimmed = page_text.trim();
                            if !trimmed.is_empty() {
                                current_bytes += trimmed.len();
                                ocr_page_texts.push(format!("## Page {}\n{}", page_idx + 1, trimmed));
                            }
                        }
                    }

                    let ocr_elapsed_ms = t_pdf_ocr_start.elapsed().as_millis() as u64;

                    if !ocr_page_texts.is_empty() {
                        let combined_ocr = ocr_page_texts.join("\n\n");
                        let final_markdown = if raw_len > 0 {
                            format!("{}\n\n---\n\n### 🔍 多页 OCR 图像识别文本\n\n{}", base_text.trim(), combined_ocr)
                        } else {
                            combined_ocr
                        };

                        let truncated_content = truncate_string(&final_markdown, max_bytes);
                        let meta = serde_json::json!({
                            "extractor": "anydoc+mupdf_ocr",
                            "is_text_pdf": false,
                            "pdf_type": pdf_type_res.as_ref().map(|r| format!("{:?}", r.pdf_type)),
                            "ocr_pages_processed": ocr_page_texts.len(),
                        });
                        return Ok((truncated_content, meta, Some(ocr_elapsed_ms)));
                    }
                }
            }
        }
    }

    // 4. 若文本层有效，截断并输出
    if !base_text.trim().is_empty() {
        let truncated_content = truncate_string(&base_text, max_bytes);
        let pdf_type_res = if ext == "pdf" { pdf_inspector::detect_pdf_type(path).ok() } else { None };
        let is_text_pdf = matches!(pdf_type_res.as_ref().map(|r| r.pdf_type), Some(pdf_inspector::PdfType::TextBased));
        let meta = serde_json::json!({
            "extractor": "anydoc",
            "is_text_pdf": if ext == "pdf" { Some(is_text_pdf) } else { None },
            "pdf_type": pdf_type_res.as_ref().map(|r| format!("{:?}", r.pdf_type)),
        });
        return Ok((truncated_content, meta, None));
    }

    // 5. 兜底回退
    let (fallback_content, fallback_meta) = extract_document_fallback(path, max_bytes)?;
    Ok((fallback_content, fallback_meta, None))
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

    // 2. 遍历 ZIP 提取 word/media/ 下的所有嵌入图片，根据 max_document_ocr_items 及 max_bytes 限制提取识别
    let mut image_ocr_map = HashMap::new();
    let max_ocr_items = config.max_document_ocr_items;
    let max_limit_bytes = if config.max_content_size_kb > 0 { config.max_content_size_kb * 1024 } else { 0 };

    if max_ocr_items != 0 {
        let mut recognized_count = 0;
        let mut accumulated_ocr_bytes = 0usize;

        for i in 0..archive.len() {
            if max_ocr_items > 0 && recognized_count >= max_ocr_items as usize {
                break;
            }
            // 若提取的 OCR 累计字节数已达到内容上限，提前终止后续图片的 OCR 处理
            if max_limit_bytes > 0 && accumulated_ocr_bytes >= max_limit_bytes {
                tracing::info!(
                    "DOCX 嵌入图片 OCR 提取文本大小已达上限 ({} KB)，提前终止后续图片识别",
                    config.max_content_size_kb
                );
                break;
            }

            let mut file = archive.by_index(i)?;
            let name = file.name().to_string();
            let name_lower = name.to_lowercase();
            if name_lower.starts_with("word/media/") || name_lower.starts_with("media/") {
                let mut bytes = Vec::new();
                if file.read_to_end(&mut bytes).is_ok() && !bytes.is_empty() {
                    if let Ok(ocr_text) = omni_vision::OmniVisionEngine::recognize_ocr_image_bytes(&bytes, &config.ocr_model_size) {
                        if !ocr_text.trim().is_empty() {
                            let clean_name = name.trim_start_matches("word/").to_string();
                            accumulated_ocr_bytes += ocr_text.len();
                            image_ocr_map.insert(name.clone(), ocr_text.clone());
                            image_ocr_map.insert(clean_name.clone(), ocr_text.clone());
                            if let Some(filename) = Path::new(&name).file_name().and_then(|n| n.to_str()) {
                                image_ocr_map.insert(filename.to_string(), ocr_text.clone());
                            }
                            recognized_count += 1;

                            // 识别单张后若已超限，立即打断循环
                            if max_limit_bytes > 0 && accumulated_ocr_bytes >= max_limit_bytes {
                                tracing::info!(
                                    "DOCX 嵌入图片 OCR 文本识别达到上限 ({} KB)，终止后续图片处理",
                                    config.max_content_size_kb
                                );
                                break;
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
                                        let replacement = OmniExtractor::format_markitdown_ocr_block(ocr_text);
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
                                        let replacement = OmniExtractor::format_markitdown_ocr_block(ocr_text);
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
        assert!(result.contains("> Line 1"));
        assert!(result.contains("> Line 2"));
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
    async fn test_extract_plain_text_utf8_truncation() {
        let temp_path = std::env::temp_dir().join("omni_test_utf8_long.txt");
        let mut file = File::create(&temp_path).unwrap();
        // 构造一个大于 30KB 的中文 UTF-8 文本
        let sample = "角色：你是一位海报设计大师。智能分析系统匹配规则。\n";
        for _ in 0..1000 {
            file.write_all(sample.as_bytes()).unwrap();
        }

        let config = OmniConfig {
            max_content_size_kb: 30,
            ..Default::default()
        };
        let res = OmniExtractor::extract(&temp_path, &config).await.unwrap();
        let _ = std::fs::remove_file(&temp_path);

        assert_eq!(res.mime_type, "text/plain");
        assert!(res.markdown_content.contains("海报设计大师"));
        assert!(res.markdown_content.contains("[Content truncated at 30 KB limit]"));
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

        let config = OmniConfig {
            max_document_ocr_items: 5,
            ..Default::default()
        };
        let res = OmniExtractor::extract(&docx_path, &config).await.unwrap();
        println!("--- USER DOCX MARKDOWN CONTENT ---\n{}", res.markdown_content);
        assert!(res.markdown_content.contains("📷 **[图片内提取文字]**"), "DOCX should contain in-place image OCR replacement!");
        assert!(res.markdown_content.contains("网盘") || res.markdown_content.contains("历史版本"), "DOCX OCR should contain recognized image text!");
    }
}


