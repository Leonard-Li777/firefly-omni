use super::super::classifier::{classify_capabilities, detect_framework, SymbolProfile};
use super::super::BinaryAnalysisInfo;

/// 解析 ELF 与 Mach-O 二进制文件
pub fn parse_elf_macho(bytes: &[u8]) -> Result<BinaryAnalysisInfo, String> {
    if bytes.starts_with(b"\x7fELF") {
        return parse_elf(bytes);
    }

    parse_macho(bytes)
}

fn parse_elf(bytes: &[u8]) -> Result<BinaryAnalysisInfo, String> {
    let elf = goblin::elf::Elf::parse(bytes).map_err(|e| format!("ELF 解析失败: {:?}", e))?;

    let mut info = BinaryAnalysisInfo {
        format: "ELF".to_string(),
        architecture: match elf.header.e_machine {
            0x3E => "x86_64".to_string(),
            0xB7 => "arm64".to_string(),
            0x03 => "x86".to_string(),
            other => format!("machine_{}", other),
        },
        confidence_score: 0.90,
        ..Default::default()
    };

    let libraries: Vec<String> = elf.libraries.iter().map(|s| s.to_string()).collect();
    let mut imported_functions = Vec::new();
    for sym in elf.dynsyms.iter() {
        if sym.is_import() {
            if let Some(name) = elf.dynstrtab.get_at(sym.st_name) {
                imported_functions.push(name.to_string());
            }
        }
    }

    let profile = SymbolProfile {
        libraries: libraries.clone(),
        imported_functions,
        is_console_subsystem: true,
        is_gui_subsystem: false,
    };
    info.capability_codes = classify_capabilities(&profile);

    let section_names: Vec<String> = elf
        .section_headers
        .iter()
        .filter_map(|sh| elf.shdr_strtab.get_at(sh.sh_name).map(|s| s.to_string()))
        .collect();

    info.detected_framework_code = detect_framework(&libraries, &section_names, false);
    info.category_code = Some(crate::concepts::Concept::开发工具.code().to_string());

    Ok(info)
}

fn parse_macho(bytes: &[u8]) -> Result<BinaryAnalysisInfo, String> {
    let mach = goblin::mach::Mach::parse(bytes).map_err(|e| format!("Mach-O 解析失败: {:?}", e))?;

    let mut info = BinaryAnalysisInfo {
        format: "Mach-O".to_string(),
        confidence_score: 0.90,
        ..Default::default()
    };

    let mut libraries = Vec::new();
    match mach {
        goblin::mach::Mach::Binary(bin) => {
            info.architecture = match bin.header.cputype {
                0x01000007 => "x86_64".to_string(),
                0x0100000c => "arm64".to_string(),
                0x00000007 => "x86".to_string(),
                other => format!("cputype_{}", other),
            };
            libraries = bin.libs.iter().map(|s| s.to_string()).collect();
        }
        goblin::mach::Mach::Fat(_) => {
            info.architecture = "fat_universal".to_string();
        }
    }

    let profile = SymbolProfile {
        libraries: libraries.clone(),
        imported_functions: Vec::new(),
        is_console_subsystem: true,
        is_gui_subsystem: false,
    };
    info.capability_codes = classify_capabilities(&profile);
    info.detected_framework_code = detect_framework(&libraries, &[], false);
    info.category_code = Some(crate::concepts::Concept::开发工具.code().to_string());

    Ok(info)
}
