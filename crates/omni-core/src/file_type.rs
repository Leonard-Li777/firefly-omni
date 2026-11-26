//! 跨版本（CE / Pro）统一文件类型探测契约与确定性魔数头快判（Ticket 08）
//!
//! - `detect_magic_header`: 零依赖快速魔数头识别，覆盖常见图片/视频/音频/文档/压缩包/可执行体；
//! - `detect_file_type_deterministic`: CE 版唯一探测实现与 Pro 版快路径（`source = "magic_header"` 或 `"extension_policy"`），
//!   严禁伪造神经网络分数（如 `0.995`）或 `"Magika Neural Network Classification..."` 欺诈文案。

use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileTypeDetection {
    pub label: String,
    pub mime_type: String,
    pub group: String,
    pub description: String,
    pub score: f32,
    pub extensions: Vec<String>,
    /// 识别来源：`"magika_onnx"` | `"magic_header"` | `"extension_policy"`
    pub source: String,
}

#[derive(Debug, Clone, Copy)]
pub struct MagicMatch {
    pub label: &'static str,
    pub mime_type: &'static str,
    pub group: &'static str,
    pub description: &'static str,
    pub extensions: &'static [&'static str],
}

/// 确定性文件魔数头识别（覆盖标准媒体容器、PDF、压缩包、可执行文件等）
pub fn detect_magic_header(bytes: &[u8]) -> Option<MagicMatch> {
    if bytes.len() >= 8 && &bytes[0..8] == b"\x89PNG\r\n\x1a\n" {
        return Some(MagicMatch {
            label: "png",
            mime_type: "image/png",
            group: "image",
            description: "PNG image data",
            extensions: &["png"],
        });
    }
    if bytes.len() >= 3 && &bytes[0..3] == b"\xFF\xD8\xFF" {
        return Some(MagicMatch {
            label: "jpeg",
            mime_type: "image/jpeg",
            group: "image",
            description: "JPEG image data",
            extensions: &["jpg", "jpeg"],
        });
    }
    if bytes.len() >= 6 && (&bytes[0..6] == b"GIF87a" || &bytes[0..6] == b"GIF89a") {
        return Some(MagicMatch {
            label: "gif",
            mime_type: "image/gif",
            group: "image",
            description: "GIF image data",
            extensions: &["gif"],
        });
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some(MagicMatch {
            label: "webp",
            mime_type: "image/webp",
            group: "image",
            description: "WebP image data",
            extensions: &["webp"],
        });
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
        return Some(MagicMatch {
            label: "wav",
            mime_type: "audio/wav",
            group: "audio",
            description: "WAVE audio",
            extensions: &["wav"],
        });
    }
    if bytes.len() >= 2 && &bytes[0..2] == b"BM" {
        return Some(MagicMatch {
            label: "bmp",
            mime_type: "image/bmp",
            group: "image",
            description: "BMP image data",
            extensions: &["bmp"],
        });
    }
    if bytes.len() >= 5 && &bytes[0..5] == b"%PDF-" {
        return Some(MagicMatch {
            label: "pdf",
            mime_type: "application/pdf",
            group: "document",
            description: "PDF document",
            extensions: &["pdf"],
        });
    }
    if bytes.len() >= 4 && &bytes[0..4] == b"fLaC" {
        return Some(MagicMatch {
            label: "flac",
            mime_type: "audio/flac",
            group: "audio",
            description: "FLAC audio",
            extensions: &["flac"],
        });
    }
    if bytes.len() >= 4 && &bytes[0..4] == b"OggS" {
        return Some(MagicMatch {
            label: "ogg",
            mime_type: "audio/ogg",
            group: "audio",
            description: "Ogg media container",
            extensions: &["ogg", "oga", "ogv"],
        });
    }
    if bytes.len() >= 3 && (&bytes[0..3] == b"ID3" || (bytes[0] == 0xFF && (bytes[1] & 0xE0) == 0xE0)) {
        return Some(MagicMatch {
            label: "mp3",
            mime_type: "audio/mpeg",
            group: "audio",
            description: "MP3 audio",
            extensions: &["mp3"],
        });
    }
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        return Some(MagicMatch {
            label: "mp4",
            mime_type: "video/mp4",
            group: "video",
            description: "MP4/ISO Base Media container",
            extensions: &["mp4", "m4v", "mov"],
        });
    }
    if bytes.len() >= 4 && &bytes[0..4] == b"\x1A\x45\xDF\xA3" {
        return Some(MagicMatch {
            label: "mkv",
            mime_type: "video/x-matroska",
            group: "video",
            description: "Matroska / WebM media container",
            extensions: &["mkv", "webm"],
        });
    }
    if bytes.len() >= 6 && &bytes[0..6] == b"7z\xBC\xAF\x27\x1C" {
        return Some(MagicMatch {
            label: "sevenzip",
            mime_type: "application/x-7z-compressed",
            group: "archive",
            description: "7-zip archive",
            extensions: &["7z"],
        });
    }
    if bytes.len() >= 7 && &bytes[0..7] == b"Rar!\x1A\x07\x00"
        || bytes.len() >= 8 && &bytes[0..8] == b"Rar!\x1A\x07\x01\x00"
    {
        return Some(MagicMatch {
            label: "rar",
            mime_type: "application/vnd.rar",
            group: "archive",
            description: "RAR archive",
            extensions: &["rar"],
        });
    }
    if bytes.len() >= 2 && &bytes[0..2] == b"\x1F\x8B" {
        return Some(MagicMatch {
            label: "gzip",
            mime_type: "application/gzip",
            group: "archive",
            description: "GZIP compressed data",
            extensions: &["gz", "tgz"],
        });
    }
    if bytes.len() >= 16 && &bytes[0..16] == b"SQLite format 3\0" {
        return Some(MagicMatch {
            label: "sqlite",
            mime_type: "application/vnd.sqlite3",
            group: "application",
            description: "SQLite 3 database",
            extensions: &["db", "sqlite", "sqlite3"],
        });
    }
    if bytes.len() >= 4 && &bytes[0..4] == b"\x7FELF" {
        return Some(MagicMatch {
            label: "elf",
            mime_type: "application/x-executable",
            group: "executable",
            description: "ELF executable",
            extensions: &["elf", "so"],
        });
    }
    if bytes.len() >= 4 && &bytes[0..4] == b"\x00asm" {
        return Some(MagicMatch {
            label: "wasm",
            mime_type: "application/wasm",
            group: "executable",
            description: "WebAssembly binary",
            extensions: &["wasm"],
        });
    }
    if bytes.len() >= 4 && &bytes[0..4] == b"PK\x03\x04" {
        return Some(MagicMatch {
            label: "zip",
            mime_type: "application/zip",
            group: "archive",
            description: "ZIP archive",
            extensions: &["zip", "docx", "xlsx", "pptx", "epub", "jar"],
        });
    }
    None
}

/// 读取文件前 64 字节执行魔数头探测
pub fn probe_file_magic_header(path: &Path) -> Option<MagicMatch> {
    let mut file = File::open(path).ok()?;
    let mut header = [0u8; 64];
    let n = file.read(&mut header).ok()?;
    detect_magic_header(&header[..n])
}

/// CE 版与 Pro 回退统一使用的确定性探测（`source = "magic_header"` 或 `"extension_policy"`）
pub fn detect_file_type_deterministic(path: &Path) -> FileTypeDetection {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    if let Some(m) = probe_file_magic_header(path) {
        // OOXML 等 ZIP 派生格式在扩展名与 ZIP 容器一致时保留精准 MIME
        if m.label == "zip" {
            if let Some((lbl, mime, grp, desc)) = match ext.as_str() {
                "docx" => Some(("docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document", "document", "Microsoft Word document")),
                "xlsx" => Some(("xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", "document", "Microsoft Excel spreadsheet")),
                "pptx" => Some(("pptx", "application/vnd.openxmlformats-officedocument.presentationml.presentation", "document", "Microsoft PowerPoint presentation")),
                "epub" => Some(("epub", "application/epub+zip", "document", "EPUB electronic book")),
                _ => None,
            } {
                return FileTypeDetection {
                    label: lbl.to_string(),
                    mime_type: mime.to_string(),
                    group: grp.to_string(),
                    description: desc.to_string(),
                    score: 1.0,
                    extensions: vec![ext],
                    source: "magic_header".to_string(),
                };
            }
        }
        return FileTypeDetection {
            label: m.label.to_string(),
            mime_type: m.mime_type.to_string(),
            group: m.group.to_string(),
            description: m.description.to_string(),
            score: 1.0,
            extensions: m.extensions.iter().map(|s| s.to_string()).collect(),
            source: "magic_header".to_string(),
        };
    }

    let (label, mime, group, desc) = match ext.as_str() {
        "txt" => ("txt", "text/plain", "text", "Plain text document"),
        "md" | "markdown" => ("markdown", "text/markdown", "text", "Markdown document"),
        "json" => ("json", "application/json", "code", "JSON data"),
        "xml" => ("xml", "application/xml", "code", "XML document"),
        "html" | "htm" => ("html", "text/html", "code", "HTML document"),
        "css" => ("css", "text/css", "code", "CSS stylesheet"),
        "js" | "mjs" | "cjs" => ("javascript", "text/javascript", "code", "JavaScript source"),
        "ts" | "tsx" => ("typescript", "text/typescript", "code", "TypeScript source"),
        "rs" => ("rust", "text/x-rust", "code", "Rust source"),
        "py" => ("python", "text/x-python", "code", "Python source"),
        "csv" => ("csv", "text/csv", "text", "CSV table"),
        "yaml" | "yml" => ("yaml", "application/yaml", "code", "YAML document"),
        "toml" => ("toml", "application/toml", "code", "TOML document"),
        "svg" => ("svg", "image/svg+xml", "image", "SVG vector image"),
        _ => ("unknown", "application/octet-stream", "unknown", "Unknown binary or text stream"),
    };

    FileTypeDetection {
        label: label.to_string(),
        mime_type: mime.to_string(),
        group: group.to_string(),
        description: desc.to_string(),
        score: if label == "unknown" { 0.0 } else { 0.8 },
        extensions: if ext.is_empty() { vec![] } else { vec![ext] },
        source: "extension_policy".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disguised_png_and_pdf_with_txt_extension_detected_by_magic_header() {
        let dir = std::env::temp_dir().join(format!("ff-magic-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);

        let fake_png = dir.join("disguised.txt");
        std::fs::write(&fake_png, b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR").unwrap();
        let res_png = detect_file_type_deterministic(&fake_png);
        assert_eq!(res_png.label, "png");
        assert_eq!(res_png.mime_type, "image/png");
        assert_eq!(res_png.source, "magic_header");

        let no_ext_pdf = dir.join("secret_doc");
        std::fs::write(&no_ext_pdf, b"%PDF-1.7\n1 0 obj\n").unwrap();
        let res_pdf = detect_file_type_deterministic(&no_ext_pdf);
        assert_eq!(res_pdf.label, "pdf");
        assert_eq!(res_pdf.mime_type, "application/pdf");
        assert_eq!(res_pdf.source, "magic_header");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
