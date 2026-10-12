use pelite::pe32::{Pe as Pe32, PeFile as PeFile32};
use pelite::pe64::{Pe as Pe64, PeFile as PeFile64};
use pelite::resources::version_info::VersionInfo;
use pelite::resources::{Directory, Resources};
use pelite::Wrap;

use super::super::classifier::{classify_capabilities, detect_framework, SymbolProfile};
use super::super::BinaryAnalysisInfo;

/// 微软 PE 规范正规资源目录严格只有 3 层 (Type -> ID -> Language)。
/// 硬上限设为 4，用于防加壳保护（VMProtect/Themida）或恶意样本构造的循环指针导致爆栈。
pub const PE_RESOURCE_MAX_DEPTH: usize = 4;

/// 解析 PE 文件的全部静态事实与特征
pub fn parse_pe(bytes: &[u8]) -> Result<BinaryAnalysisInfo, String> {
    let wrap = match pelite::PeFile::from_bytes(bytes) {
        Ok(w) => w,
        Err(e) => return Err(format!("PE 解析失败: {:?}", e)),
    };

    match wrap {
        Wrap::T32(pe) => extract_pe32_facts(&pe, bytes),
        Wrap::T64(pe) => extract_pe64_facts(&pe, bytes),
    }
}

macro_rules! impl_extract_pe_facts {
    ($fn_name:ident, $pe_type:ty, $pe_trait:ident, $arch:expr) => {
        fn $fn_name(pe: &$pe_type, raw_bytes: &[u8]) -> Result<BinaryAnalysisInfo, String> {
            let mut info = BinaryAnalysisInfo {
                format: "PE".to_string(),
                architecture: $arch.to_string(),
                confidence_score: 0.95,
                ..Default::default()
            };

            // 1. 节区名称提取
            let mut section_names = Vec::new();
            for s in pe.section_headers().iter() {
                if let Ok(name) = s.name() {
                    section_names.push(name.to_string());
                }
            }

            // 2. 子系统类型判断 (GUI vs Console)
            let mut is_gui = false;
            let mut is_console = false;
            let optional_header = pe.optional_header();
            let subsystem = optional_header.Subsystem;
            if subsystem == 2 {
                is_gui = true;
            } else if subsystem == 3 {
                is_console = true;
            }

            // 3. 防加壳指针回环与资源树深度校验 (max_depth = 4)
            if let Ok(resources) = pe.resources() {
                let is_safe = check_resource_depth_safe(&resources, 1, PE_RESOURCE_MAX_DEPTH);
                if !is_safe {
                    tracing::warn!("[BinaryLens] PE 资源目录层级超过上限 (max_depth=4)，触发回环截断保护");
                } else if let Ok(version_info) = resources.version_info() {
                    extract_version_info(&version_info, &mut info);
                }
            }

            // 4. IAT 导入表解析与符号聚类
            let mut libraries = Vec::new();
            let mut imported_functions = Vec::new();
            if let Ok(imports) = pe.imports() {
                for desc in imports {
                    if let Ok(dll_name) = desc.dll_name() {
                        let dll_str = dll_name.to_string();
                        libraries.push(dll_str.clone());

                        if let Ok(int) = desc.int() {
                            for import in int {
                                if let Ok(imp) = import {
                                    match imp {
                                        pelite::pe32::imports::Import::ByName { name, .. } => {
                                            imported_functions.push(name.to_string());
                                        }
                                        pelite::pe32::imports::Import::ByOrdinal { .. } => {}
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // 5. Authenticode 数字签名组织抽取
            let signer = pe.data_directory().get(4).and_then(|sec_dir| {
                extract_authenticode_signer(sec_dir.VirtualAddress as usize, sec_dir.Size as usize, raw_bytes)
            });
            if signer.is_some() {
                info.signer_organization = signer;
            }

            // 6. 符号聚类与能力特征识别
            let profile = SymbolProfile {
                libraries: libraries.clone(),
                imported_functions,
                is_console_subsystem: is_console,
                is_gui_subsystem: is_gui,
            };
            info.capability_codes = classify_capabilities(&profile);

            // 7. 运行时框架探测
            let has_clr = pe.data_directory().get(14).map(|d| d.VirtualAddress > 0).unwrap_or(false);
            info.detected_framework_code = detect_framework(&libraries, &section_names, has_clr);

            // 8. 默认分类为开发工具
            info.category_code = Some(crate::concepts::Concept::开发工具.code().to_string());

            Ok(info)
        }
    };
}

impl_extract_pe_facts!(extract_pe32_facts, PeFile32, Pe32, "x86");
impl_extract_pe_facts!(extract_pe64_facts, PeFile64, Pe64, "x86_64");

/// 微软 PE 规范正规资源目录严格只有 3 层。
/// 使用带深度的递归遍历，硬上限设为 4，防止指针回环。
fn check_resource_depth_safe(resources: &Resources, current_depth: usize, max_depth: usize) -> bool {
    if current_depth > max_depth {
        return false;
    }
    let root = match resources.root() {
        Ok(r) => r,
        Err(_) => return true,
    };
    for entry in root.entries() {
        if let Ok(e) = entry.entry() {
            if let Some(sub_dir) = e.dir() {
                if !check_dir_depth_safe(&sub_dir, current_depth + 1, max_depth) {
                    return false;
                }
            }
        }
    }
    true
}

fn check_dir_depth_safe(dir: &Directory, current_depth: usize, max_depth: usize) -> bool {
    if current_depth > max_depth {
        return false;
    }
    for entry in dir.entries() {
        if let Ok(e) = entry.entry() {
            if let Some(sub_dir) = e.dir() {
                if !check_dir_depth_safe(&sub_dir, current_depth + 1, max_depth) {
                    return false;
                }
            }
        }
    }
    true
}

/// 从 VersionInfo 中安全提取产品名、厂商名、版本号等
fn extract_version_info(ver: &VersionInfo, info: &mut BinaryAnalysisInfo) {
    let file_info = ver.file_info();
    for (_lang, map) in file_info.strings {
        if info.product_name.is_none() {
            if let Some(val) = map.get("ProductName") {
                let clean = val.trim().to_string();
                if !clean.is_empty() {
                    info.product_name = Some(clean);
                }
            }
        }
        if info.company_name.is_none() {
            if let Some(val) = map.get("CompanyName") {
                let clean = val.trim().to_string();
                if !clean.is_empty() {
                    info.company_name = Some(clean);
                }
            }
        }
        if info.file_description.is_none() {
            if let Some(val) = map.get("FileDescription") {
                let clean = val.trim().to_string();
                if !clean.is_empty() {
                    info.file_description = Some(clean);
                }
            }
        }
        if info.file_version.is_none() {
            if let Some(val) = map.get("FileVersion") {
                let clean = val.trim().to_string();
                if !clean.is_empty() {
                    info.file_version = Some(clean);
                }
            }
        }
        if info.legal_copyright.is_none() {
            if let Some(val) = map.get("LegalCopyright") {
                let clean = val.trim().to_string();
                if !clean.is_empty() {
                    info.legal_copyright = Some(clean);
                }
            }
        }
    }

    if info.file_version.is_none() {
        if let Some(fixed) = ver.fixed() {
            info.file_version = Some(format!(
                "{}.{}.{}.{}",
                fixed.dwFileVersion.Major,
                fixed.dwFileVersion.Minor,
                fixed.dwFileVersion.Patch,
                fixed.dwFileVersion.Build
            ));
        }
    }
}

/// 从 Authenticode 安全目录中提取证书签名者组织
fn extract_authenticode_signer(offset: usize, size: usize, raw_bytes: &[u8]) -> Option<String> {
    if offset == 0 || size < 8 || offset + size > raw_bytes.len() {
        return None;
    }

    let cert_data = &raw_bytes[offset..offset + size];
    if cert_data.len() < 8 {
        return None;
    }
    let cert_type = u16::from_le_bytes([cert_data[6], cert_data[7]]);
    if cert_type != 0x0002 {
        return None;
    }

    let pkcs_bytes = &cert_data[8..];
    find_asn1_string(pkcs_bytes, &[0x55, 0x04, 0x0a])
        .or_else(|| find_asn1_string(pkcs_bytes, &[0x55, 0x04, 0x03]))
}

/// 从 DER/ASN.1 字节流中启发式提取指定 OID 后的字符串
fn find_asn1_string(bytes: &[u8], oid: &[u8]) -> Option<String> {
    let mut pos = 0;
    while pos + oid.len() + 2 < bytes.len() {
        if &bytes[pos..pos + oid.len()] == oid {
            let next_pos = pos + oid.len();
            let tag = bytes[next_pos];
            let len = bytes[next_pos + 1] as usize;
            let str_start = next_pos + 2;
            let str_end = str_start + len;

            if str_end <= bytes.len() {
                let str_bytes = &bytes[str_start..str_end];
                match tag {
                    0x0c | 0x13 | 0x16 => {
                        if let Ok(s) = std::str::from_utf8(str_bytes) {
                            let clean = s.trim().to_string();
                            if !clean.is_empty() {
                                return Some(clean);
                            }
                        }
                    }
                    0x1e => {
                        if len % 2 == 0 {
                            let u16_vec: Vec<u16> = str_bytes
                                .chunks_exact(2)
                                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                                .collect();
                            if let Ok(s) = String::from_utf16(&u16_vec) {
                                let clean = s.trim().to_string();
                                if !clean.is_empty() {
                                    return Some(clean);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        pos += 1;
    }
    None
}
