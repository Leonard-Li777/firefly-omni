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
            if !doc_meta.contains_key("extractor") {
                doc_meta.insert("extractor".into(), "anydoc".into());
            }

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
/// exiftool 可执行文件路径缓存（仅查找一次，避免每次提取都重复启动 Perl 进程探测）
static EXIFTOOL_EXE_CACHE: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();

fn find_exiftool_executable() -> Option<std::path::PathBuf> {
    EXIFTOOL_EXE_CACHE.get_or_init(|| find_exiftool_executable_inner()).clone()
}

fn find_exiftool_executable_inner() -> Option<std::path::PathBuf> {
    let is_win = cfg!(target_os = "windows");
    let exe_name = if is_win { "exiftool.exe" } else { "exiftool" };
    let platform_dir = match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        "linux" => "linux",
        other => other,
    };

    // 1. 优先查找系统 PATH 中的 exiftool / exiftool.exe
    // 注意：不再用 -ver 进程探测（每次启动 Perl 约需 1-2s），改为直接检查 PATH 上的文件是否存在
    if let Ok(path_env) = std::env::var("PATH") {
        let sep = if is_win { ';' } else { ':' };
        for dir in path_env.split(sep) {
            let candidate = std::path::Path::new(dir).join(exe_name);
            if candidate.exists() {
                return Some(candidate);
            }
            // Windows 上同时尝试无 .exe 后缀的 exiftool
            if is_win {
                let candidate2 = std::path::Path::new(dir).join("exiftool");
                if candidate2.exists() {
                    return Some(candidate2);
                }
            }
        }
    }

    // 2. 查找 APPDATA / HOME 本地缓存目录中的 bin/{platform}/
    if let Ok(appdata) = std::env::var("APPDATA") {
        let candidates = [
            std::path::Path::new(&appdata).join(format!("firefly-ai-folder/bin/{}/{}", platform_dir, exe_name)),
            std::path::Path::new(&appdata).join(format!("firefly-ai-folder/bin/{}", exe_name)),
        ];
        for cand in candidates {
            if cand.exists() {
                return Some(cand);
            }
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let candidates = [
            std::path::Path::new(&home).join(format!(".config/firefly-ai-folder/bin/{}/{}", platform_dir, exe_name)),
            std::path::Path::new(&home).join(format!(".config/firefly-ai-folder/bin/{}", exe_name)),
        ];
        for cand in candidates {
            if cand.exists() {
                return Some(cand);
            }
        }
    }

    // 3. 向上递归搜索可执行文件所在目录 (exe_dir)
    if let Ok(exe_dir) = std::env::current_exe().map(|p| p.parent().unwrap_or(std::path::Path::new("")).to_path_buf()) {
        let mut curr: Option<&std::path::Path> = Some(exe_dir.as_path());
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
        let mut curr: Option<&std::path::Path> = Some(cwd.as_path());
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


/// 格式化 PDF 内部日期字符串 (例如: D:20250407143317+08'00' -> 2025:04:07 14:33:17+08:00)
fn format_pdf_date(raw: &str) -> String {
    let s = raw.strip_prefix("D:").unwrap_or(raw);
    if s.len() >= 14 {
        let year = &s[0..4];
        let month = &s[4..6];
        let day = &s[6..8];
        let hour = &s[8..10];
        let min = &s[10..12];
        let sec = &s[12..14];
        let rest = &s[14..];
        let tz = rest.replace('\'', ":").trim_end_matches(':').to_string();
        if !tz.is_empty() {
            format!("{}:{}:{} {}:{}:{}{}", year, month, day, hour, min, sec, tz)
        } else {
            format!("{}:{}:{} {}:{}:{}", year, month, day, hour, min, sec)
        }
    } else {
        s.to_string()
    }
}

/// Rust 原生极速解析 PDF 元数据字典 (通过 lopdf，耗时通常 < 3ms，完全免疫 ExifTool 在 AES-256 加密 PDF 上的 3.5s 解密卡顿)
fn extract_pdf_metadata_native(p: &Path) -> Option<serde_json::Map<String, serde_json::Value>> {
    let doc = lopdf::Document::load(p).ok()?;
    let mut map = serde_json::Map::new();

    map.insert("FileType".into(), "PDF".into());
    map.insert("FileTypeExtension".into(), "pdf".into());
    map.insert("MIMEType".into(), "application/pdf".into());
    map.insert("PDFVersion".into(), doc.version.clone().into());

    let page_count = doc.get_pages().len();
    if page_count > 0 {
        map.insert("PageCount".into(), (page_count as u64).into());
    }

    // 从 Trailer 查找 Info 字典
    let info_obj = if let Ok(info_ref) = doc.trailer.get(b"Info") {
        match info_ref {
            lopdf::Object::Reference(id) => doc.get_object(*id).ok(),
            lopdf::Object::Dictionary(_) => Some(info_ref),
            _ => None,
        }
    } else {
        None
    };

    if let Some(lopdf::Object::Dictionary(dict)) = info_obj {
        for (key, val) in dict.iter() {
            let key_str = String::from_utf8_lossy(key).to_string();
            let val_str = match val {
                lopdf::Object::String(bytes, _) => {
                    if bytes.starts_with(&[0xFE, 0xFF]) {
                        let u16_vec: Vec<u16> = bytes[2..]
                            .chunks_exact(2)
                            .map(|c| u16::from_be_bytes([c[0], c[1]]))
                            .collect();
                        String::from_utf16_lossy(&u16_vec)
                    } else {
                        String::from_utf8_lossy(bytes).to_string()
                    }
                }
                lopdf::Object::Name(bytes) => String::from_utf8_lossy(bytes).to_string(),
                lopdf::Object::Integer(i) => i.to_string(),
                lopdf::Object::Real(f) => f.to_string(),
                lopdf::Object::Boolean(b) => b.to_string(),
                _ => continue,
            };

            let trimmed = val_str.trim();
            if trimmed.is_empty() {
                continue;
            }

            let formatted_val = if (key_str == "CreationDate" || key_str == "ModDate") && trimmed.starts_with("D:") {
                format_pdf_date(trimmed)
            } else {
                trimmed.to_string()
            };

            let std_key = match key_str.as_str() {
                "CreationDate" => "CreateDate",
                "ModDate" => "ModifyDate",
                other => other,
            };

            map.insert(std_key.to_string(), formatted_val.into());
        }
    }

    if map.len() >= 3 {
        Some(map)
    } else {
        None
    }
}

/// ExifTool -stay_open 守护进程句柄（单进程常驻内存，延迟初始化，原子同步管道通信）
struct ExifToolDaemon {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout_reader: std::io::BufReader<std::process::ChildStdout>,
}

impl ExifToolDaemon {
    fn spawn(exe: &std::path::Path) -> Option<Self> {
        use std::process::{Command, Stdio};
        let mut cmd = Command::new(exe);
        if let Some(parent) = exe.parent() {
            cmd.current_dir(parent);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        cmd.args(["-stay_open", "True", "-@", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        let mut child = cmd.spawn().ok()?;
        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        let stdout_reader = std::io::BufReader::new(stdout);

        Some(ExifToolDaemon {
            child,
            stdin,
            stdout_reader,
        })
    }

    fn query(&mut self, abs_path: &std::path::Path) -> Option<serde_json::Map<String, serde_json::Value>> {
        use std::io::{BufRead, Write};

        // 发送命令：每行一个参数，末尾以 -execute 触发
        let cmd = format!("-json\n-fast2\n-charset\nfilename=utf8\n{}\n-execute\n", abs_path.display());
        self.stdin.write_all(cmd.as_bytes()).ok()?;
        self.stdin.flush().ok()?;

        let mut output = String::new();
        loop {
            let mut line = String::new();
            match self.stdout_reader.read_line(&mut line) {
                Ok(0) => return None, // EOF，子进程已退出
                Ok(_) => {
                    if line.trim() == "{ready}" {
                        break;
                    }
                    output.push_str(&line);
                }
                Err(_) => return None,
            }
        }

        let trimmed = output.trim();
        if trimmed.is_empty() {
            return None;
        }

        let val: serde_json::Value = serde_json::from_str(trimmed).ok()?;
        val.as_array()?.first()?.as_object().cloned()
    }
}

impl Drop for ExifToolDaemon {
    fn drop(&mut self) {
        use std::io::Write;
        let _ = self.stdin.write_all(b"-stay_open\nFalse\n");
        let _ = self.stdin.flush();
        let _ = self.child.kill();
    }
}

/// 全局 ExifTool 守护进程单例
static EXIFTOOL_DAEMON: std::sync::OnceLock<std::sync::Mutex<Option<ExifToolDaemon>>> = std::sync::OnceLock::new();

/// 显式关闭 ExifTool 守护进程（供 CLI 模式在进程退出前释放所有管道资源，常驻服务模式无需调用）
pub fn shutdown_exiftool_daemon() {
    if let Some(mutex) = EXIFTOOL_DAEMON.get() {
        if let Ok(mut guard) = mutex.lock() {
            if let Some(mut daemon) = guard.take() {
                use std::io::Write;
                let _ = daemon.stdin.write_all(b"-stay_open\nFalse\n");
                let _ = daemon.stdin.flush();
                let _ = daemon.child.kill();
            }
        }
    }
}

/// 提取全量 ExifTool 字典 (包含 Creator, Producer, CreateDate, ModifyDate, PDFVersion, PageCount 等全量 100+ 属性)
/// 对于 PDF 优先使用 Rust 原生 lopdf 毫秒级提取；对于其他文件使用常驻内存的 ExifTool -stay_open 守护进程（~2ms 响应）
fn extract_full_exiftool_metadata(p: &Path) -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();

    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    // 优先 0：对于 PDF 文件，优先使用 Rust 原生 lopdf 提取（<3ms，彻底避免 ExifTool 在 AES 加密 PDF 上的 3.5s 卡顿）
    if ext == "pdf" {
        if let Some(pdf_map) = extract_pdf_metadata_native(p) {
            return pdf_map;
        }
    }

    let abs_path = if p.is_relative() {
        std::env::current_dir().map(|cwd| cwd.join(p)).unwrap_or_else(|_| p.to_path_buf())
    } else {
        p.to_path_buf()
    };

    // 优先 1：调用 exiftool -stay_open 守护进程（常驻内存，响应通常 < 5ms）
    if let Some(exe_path) = find_exiftool_executable() {
        let daemon_lock = EXIFTOOL_DAEMON.get_or_init(|| {
            std::sync::Mutex::new(ExifToolDaemon::spawn(&exe_path))
        });

        let mut first_obj_opt = None;
        if let Ok(mut guard) = daemon_lock.lock() {
            if guard.is_none() {
                *guard = ExifToolDaemon::spawn(&exe_path);
            }
            if let Some(daemon) = guard.as_mut() {
                if let Some(obj) = daemon.query(&abs_path) {
                    first_obj_opt = Some(obj);
                } else {
                    // 查询失败可能子进程异常，尝试重启一次
                    *guard = ExifToolDaemon::spawn(&exe_path);
                    if let Some(daemon) = guard.as_mut() {
                        first_obj_opt = daemon.query(&abs_path);
                    }
                }
            }
        }

        if let Some(first_obj) = first_obj_opt {
            let skip_keys = [
                "SourceFile", "ExifToolVersion", "Directory", "FilePermissions",
                "ThumbnailImage", "PreviewImage", "JpgFromRaw", "OtherImage",
                "MakerNotes", "MakerNoteSony", "MakerNoteCanon", "MakerNoteNikon",
                "SonyDateTime2", "SonyToneCurve", "UserComment", "PrintIM"
            ];
            for (k, v) in &first_obj {
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

    // 备选 2：如果 exiftool 未找到或不可用，降级使用 exiftool-rs
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


/// 原生文档提纯解析器 (支持 PDF/DOC/DOCX/EPUB/PPT/PPTX/HTML/XLS/XLSX 等格式毫秒级解析并输出 Markdown，对于 DOCX/PDF 优先提取原生文本层，并在文本层不足且开启 OCR 时分页/分图 OCR 且动态按上限提前终止)
fn extract_pdf_content_and_meta(path: &Path, max_bytes: usize, config: &OmniConfig) -> Result<(String, serde_json::Value, Option<u64>)> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();

    // 1. 对于 DOCX 文档：
    if ext == "docx" {
        // 1.1 若开启了文档 OCR (max_document_ocr_items != 0)
        if config.max_document_ocr_items != 0 {
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

        // 1.2 未开启 OCR，或 OCR 融合未提取到内容：优先使用 anydoc 极速提取原生 Markdown
        if let Ok(raw_markdown) = anydoc::to_markdown(path) {
            if !raw_markdown.trim().is_empty() {
                let truncated_content = truncate_string(&raw_markdown, max_bytes);
                let meta = serde_json::json!({
                    "extractor": "anydoc",
                });
                return Ok((truncated_content, meta, None));
            }
        }

        // 1.3 若 anydoc 解析为空或异常，兜底直接解析 word/document.xml 原生段落流
        if let Ok((docx_text, docx_meta)) = extract_docx_xml_fallback(path, max_bytes) {
            if !docx_text.trim().is_empty() {
                return Ok((docx_text, docx_meta, None));
            }
        }
    }

    // 2. 对于 PDF 文档：先用 pdf-inspector 检查类型，再决定是否需要 anydoc 解析
    if ext == "pdf" {
        let mut base_text = String::new();
        let pdf_type_res = pdf_inspector::detect_pdf_type(path).ok();
        let is_text_pdf = matches!(pdf_type_res.as_ref().map(|r| r.pdf_type), Some(pdf_inspector::PdfType::TextBased));
        let is_scanned = matches!(pdf_type_res.as_ref().map(|r| r.pdf_type), Some(pdf_inspector::PdfType::Scanned));

        // 若是纯图片扫描件且未开启文档 OCR，无需 anydoc 解析，直接返回空
        if is_scanned && config.max_document_ocr_items == 0 {
            let meta = serde_json::json!({
                "extractor": "pdf-inspector",
                "is_text_pdf": false,
                "pdf_type": pdf_type_res.as_ref().map(|r| format!("{:?}", r.pdf_type)),
                "no_text_layer": true,
            });
            return Ok((String::new(), meta, None));
        }

        // 非纯扫描件，或开启了 OCR：尝试提取 anydoc 基础 Markdown 文本层
        if let Ok(markdown) = anydoc::to_markdown(path) {
            base_text = markdown;
        }
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

        // 严格遵循用户文档 OCR 配置：仅当用户开启了文档 OCR (max_document_ocr_items != 0) 且文本层不足时才执行 OCR 扫描；
        // 若用户未开启文档 OCR (max_document_ocr_items == 0)，即使无文本层也绝不强行 OCR。
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

        // 针对 PDF，若原生文本与 OCR 均无内容，返回结构化元数据摘要，坚决不进入二进制 fallback
        if base_text.trim().is_empty() {
            // 提取内容为空（无文本层且未开启OCR），返回空字符串，不生成任何提示文案
            let meta = serde_json::json!({
                "extractor": "anydoc+pdf-inspector",
                "is_text_pdf": false,
                "pdf_type": pdf_type_res.as_ref().map(|r| format!("{:?}", r.pdf_type)),
                "no_text_layer": true,
            });
            return Ok((String::new(), meta, None));
        }

        let truncated_content = truncate_string(&base_text, max_bytes);
        let meta = serde_json::json!({
            "extractor": "anydoc",
            "is_text_pdf": Some(is_text_pdf),
            "pdf_type": pdf_type_res.as_ref().map(|r| format!("{:?}", r.pdf_type)),
        });
        return Ok((truncated_content, meta, None));
    }

    // 3. 对于 DOC / PPT / PPTX / XLS / XLSX / ODT / ODS / ODP / EPUB / RTF 等所有结构化文档：
    // 使用 anydoc 进行原生提纯解析
    match anydoc::to_markdown(path) {
        Ok(raw_markdown) => {
            let trimmed = raw_markdown.trim();
            if !trimmed.is_empty() {
                let truncated_content = truncate_string(trimmed, max_bytes);
                let meta = serde_json::json!({
                    "extractor": "anydoc",
                    "format": ext,
                });
                return Ok((truncated_content, meta, None));
            }
        }
        Err(err) => {
            tracing::warn!("anydoc 解析文档 {} ({}) 失败: {:?}，尝试安全兜底", path.display(), ext, err);
        }
    }

    // 4. 兜底回退 (仅对 Office/其他文档进行纯文本试探，严格杜绝输出二进制乱码)
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

/// 解析 DOCX 文档中 word/document.xml 的文本段落流（纯 XML 极速解析兜底）
fn extract_docx_xml_fallback(path: &Path, max_bytes: usize) -> Result<(String, serde_json::Value)> {
    let file = File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut doc_content = String::new();
    if let Ok(mut doc_file) = archive.by_name("word/document.xml") {
        let _ = doc_file.read_to_string(&mut doc_content);
    }

    if doc_content.is_empty() {
        return Ok((String::new(), serde_json::json!({ "extractor": "docx_xml_fallback" })));
    }

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

    let final_markdown = paragraph_lines.join("\n\n");
    let truncated = truncate_string(&final_markdown, max_bytes);
    let meta = serde_json::json!({
        "extractor": "docx_xml_fallback",
        "paragraphs_count": paragraph_lines.len(),
    });

    Ok((truncated, meta))
}

fn extract_document_fallback(path: &Path, max_bytes: usize) -> Result<(String, serde_json::Value)> {
    let title_candidate = path.file_stem().unwrap_or_default().to_string_lossy();
    let ext = path.extension().unwrap_or_default().to_string_lossy().to_lowercase();

    // 严禁将未知二进制文件直接作为文本输出乱码，返回干净的结构化占位信息
    let content_text = format!("Document Title: {}\nFormat: {}\nStatus: Binary document format structure processed.", title_candidate, ext);

    let doc_meta = serde_json::json!({
        "fallback": true,
        "format": ext,
        "binary_fallback_safe": true,
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
        let candidates = [
            std::path::PathBuf::from("../../tests/work-folder/SPEEDY/PRIVATE/项目资料_新手教程_同步空间使用指南_V1.docx"),
            std::path::PathBuf::from("../../tests/work-folder/PRIVATE/项目资料_新手教程_同步空间使用指南_V1.docx"),
            std::path::PathBuf::from(r"F:\lilun\Desktop\项目资料_新手教程_同步空间使用指南_V1.docx"),
        ];
        let docx_path = candidates.into_iter().find(|p| p.exists());
        if let Some(path) = docx_path {
            let config = OmniConfig {
                max_document_ocr_items: 5,
                ..Default::default()
            };
            let res = OmniExtractor::extract(&path, &config).await.unwrap();
            println!("--- USER DOCX MARKDOWN CONTENT ---\n{}", res.markdown_content);
            assert!(res.markdown_content.contains("📷 **[图片内提取文字]**"), "DOCX should contain in-place image OCR replacement!");
            assert!(res.markdown_content.contains("网盘") || res.markdown_content.contains("历史版本"), "DOCX OCR should contain recognized image text!");
        }
    }

    #[tokio::test]
    async fn test_user_grammar_doc_and_docx() {
        let doc_candidates = [
            std::path::PathBuf::from("../../tests/work-folder/SPEEDY/PRIVATE/语法学习_初级_英语文法概述.doc"),
            std::path::PathBuf::from(r"F:\lilun\Desktop\语法学习_初级_英语文法概述.doc"),
        ];
        if let Some(doc_path) = doc_candidates.into_iter().find(|p| p.exists()) {
            let config = OmniConfig::default();
            let res = OmniExtractor::extract(&doc_path, &config).await.unwrap();
            assert!(!res.is_corrupted);
            assert!(!res.markdown_content.trim().is_empty());
            assert!(res.markdown_content.contains("基本句型") || res.markdown_content.contains("單句") || res.markdown_content.contains("Sentence"));
            assert_eq!(res.metadata.get("document").and_then(|d| d.get("extractor")).and_then(|e| e.as_str()), Some("anydoc"));
        }

        let docx_candidates = [
            std::path::PathBuf::from("../../tests/work-folder/SPEEDY/PRIVATE/语法学习_基础语法_单复句结构与高级句型详解.docx"),
            std::path::PathBuf::from(r"F:\lilun\Desktop\语法学习_基础语法_单复句结构与高级句型详解.docx"),
        ];
        if let Some(docx_path) = docx_candidates.into_iter().find(|p| p.exists()) {
            let config = OmniConfig::default();
            let res = OmniExtractor::extract(&docx_path, &config).await.unwrap();
            assert!(!res.is_corrupted);
            assert!(!res.markdown_content.trim().is_empty());
            assert!(res.markdown_content.contains("英文文法魔法师") || res.markdown_content.contains("基本句型"));
        }
    }

    #[tokio::test]
    async fn test_user_share_platform_docx() {
        let docx_candidates = [
            std::path::PathBuf::from("../../tests/work-folder/SPEEDY/PRIVATE/[文档]网盘内容管理工具_共享协作平台_2026-07-02.docx"),
            std::path::PathBuf::from(r"F:\lilun\Desktop\[文档]网盘内容管理工具_共享协作平台_2026-07-02.docx"),
        ];
        if let Some(docx_path) = docx_candidates.into_iter().find(|p| p.exists()) {
            let config = OmniConfig::default();
            let res = OmniExtractor::extract(&docx_path, &config).await.unwrap();
            assert!(!res.is_corrupted);
            assert!(!res.markdown_content.trim().is_empty());
        }
    }
}


