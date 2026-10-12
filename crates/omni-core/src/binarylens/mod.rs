use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use thiserror::Error;

pub mod classifier;
pub mod parser;

pub use parser::{parse_elf_macho, parse_pe, PE_RESOURCE_MAX_DEPTH};

/// 单个二进制文件最大读入探测边界 (128 MB)，防内存放大与巨型安装包 OOM
pub const MAX_BINARY_PROBE_BYTES: usize = 128 * 1024 * 1024;

/// 二进制静态分析与分类事实结构
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct BinaryAnalysisInfo {
    pub format: String,
    pub architecture: String,
    pub product_name: Option<String>,
    pub company_name: Option<String>,
    pub file_description: Option<String>,
    pub file_version: Option<String>,
    pub legal_copyright: Option<String>,
    pub signer_organization: Option<String>,
    pub detected_framework_code: Option<String>,
    pub capability_codes: Vec<String>,
    pub category_code: Option<String>,
    pub confidence_score: f32,
}

#[derive(Error, Debug)]
pub enum BinaryLensError {
    #[error("文件 I/O 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("不支持的二进制格式: {0}")]
    UnsupportedFormat(String),
    #[error("解析失败: {0}")]
    ParseFailure(String),
}

/// 对外单一入口：毫秒级静态解析二进制文件
pub fn analyze_binary(path: &Path) -> Result<BinaryAnalysisInfo, BinaryLensError> {
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    let file_len = metadata.len() as usize;

    let probe_size = file_len.min(MAX_BINARY_PROBE_BYTES);
    let mut buffer = Vec::with_capacity(probe_size);
    file.by_ref().take(probe_size as u64).read_to_end(&mut buffer)?;

    if buffer.len() < 4 {
        return Err(BinaryLensError::UnsupportedFormat("文件过小无法探测魔数".to_string()));
    }

    // 1. Windows PE 魔数: MZ (4D 5A)
    if buffer.starts_with(b"MZ") {
        return parse_pe(&buffer).map_err(BinaryLensError::ParseFailure);
    }

    // 2. Linux ELF 魔数: 7F 45 4C 46 (\x7fELF)
    if buffer.starts_with(b"\x7fELF") {
        return parse_elf_macho(&buffer).map_err(BinaryLensError::ParseFailure);
    }

    // 3. macOS Mach-O 魔数: FEEDFACE (32位), FEEDFACF (64位), CAFEBABE (Universal Fat)
    let magic = &buffer[..4];
    if magic == [0xfe, 0xed, 0xfa, 0xce]
        || magic == [0xce, 0xfa, 0xed, 0xfe]
        || magic == [0xfe, 0xed, 0xfa, 0xcf]
        || magic == [0xcf, 0xfa, 0xed, 0xfe]
        || magic == [0xca, 0xfe, 0xba, 0xbe]
    {
        return parse_elf_macho(&buffer).map_err(BinaryLensError::ParseFailure);
    }

    // 兜底按文件后缀判断
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    match ext.as_str() {
        "exe" | "dll" | "sys" => parse_pe(&buffer).map_err(BinaryLensError::ParseFailure),
        "elf" | "so" | "dylib" => parse_elf_macho(&buffer).map_err(BinaryLensError::ParseFailure),
        _ => Err(BinaryLensError::UnsupportedFormat(format!("未识别的二进制格式: {}", path.display()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::classifier::{classify_capabilities, detect_framework, SymbolProfile};
    use std::io::Write;

    #[test]
    fn test_corrupted_or_empty_binary_defense() {
        let temp_dir = std::env::temp_dir();
        let empty_path = temp_dir.join("test_empty.exe");
        {
            let mut f = File::create(&empty_path).unwrap();
            f.write_all(b"").unwrap();
        }
        let res = analyze_binary(&empty_path);
        assert!(res.is_err(), "0字节文件必须返回错误而非 panic");
        let _ = std::fs::remove_file(empty_path);

        let junk_path = temp_dir.join("test_junk.exe");
        {
            let mut f = File::create(&junk_path).unwrap();
            f.write_all(b"NOT_A_PE_HEADER").unwrap();
        }
        let res = analyze_binary(&junk_path);
        assert!(res.is_err(), "非标准头格式必须返回错误而非 panic");
        let _ = std::fs::remove_file(junk_path);
    }

    #[test]
    fn test_capability_classification() {
        let profile = SymbolProfile {
            libraries: vec!["ws2_32.dll".to_string(), "d3d11.dll".to_string(), "crypt32.dll".to_string()],
            imported_functions: vec!["connect".to_string(), "CreateFileW".to_string()],
            is_console_subsystem: false,
            is_gui_subsystem: true,
        };
        let caps = classify_capabilities(&profile);
        assert!(caps.contains(&crate::concepts::Concept::具备网络访问能力.code().to_string()));
        assert!(caps.contains(&crate::concepts::Concept::硬件加速与3D图形.code().to_string()));
        assert!(caps.contains(&crate::concepts::Concept::数据加解密与安全运算.code().to_string()));
        assert!(caps.contains(&crate::concepts::Concept::图形用户界面.code().to_string()));
        assert!(caps.contains(&crate::concepts::Concept::本地文件存储读写.code().to_string()));
    }

    #[test]
    fn test_framework_detection() {
        let electron_fw = detect_framework(
            &["node.dll".to_string(), "ffmpeg.dll".to_string()],
            &[".pak".to_string()],
            false,
        );
        assert_eq!(electron_fw, Some(crate::concepts::Concept::Electron.code().to_string()));

        let dotnet_fw = detect_framework(&["mscoree.dll".to_string()], &[], true);
        assert_eq!(dotnet_fw, Some(crate::concepts::Concept::NET.code().to_string()));

        let qt_fw = detect_framework(&["Qt6Core.dll".to_string()], &[], false);
        assert_eq!(qt_fw, Some(crate::concepts::Concept::Qt.code().to_string()));
    }

    #[test]
    fn test_real_system_binary_parse_speed() {
        // 在 Windows 上测试真实的系统二进制 (如 notepad.exe 或 cmd.exe)
        let candidates = [
            "C:\\Windows\\System32\\notepad.exe",
            "C:\\Windows\\System32\\cmd.exe",
            "C:\\Windows\\explorer.exe",
        ];

        for &bin_path in &candidates {
            let p = Path::new(bin_path);
            if p.exists() {
                let start = std::time::Instant::now();
                let info = analyze_binary(p).expect("真实系统二进制解析应成功");
                let elapsed = start.elapsed();

                println!("解析 {} 耗时: {:?}, 事实: {:?}", bin_path, elapsed, info);
                assert_eq!(info.format, "PE");
                assert!(info.confidence_score >= 0.90);
                assert!(elapsed.as_millis() <= 30, "静态解析耗时必须满足毫秒级标准");
                break;
            }
        }
    }
}
