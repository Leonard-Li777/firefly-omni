// extension_topology.rs — 纯静态确定性扩展名与媒体大类受控拓扑解析器
// 依据 PRD-0058 受控三层架构与 AGENTS.md 权威契约设计，零语义包依赖，Open-Core 社区版与 Pro 版 100% 同构可用。

use crate::concepts::Concept;
use crate::MediaDomain;

/// 扩展名解析结果（自带 4 级完整物化路径）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionResolution {
    pub tag_code: String,
    pub tag_name: String,
    pub ext_dim_concept: Concept,
    pub category_concept: Concept,
    pub code_path: String,
    pub name_path: String,
    pub depth: u32,
}

/// 媒体大类解析结果（自带 2 级完整物化路径）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryResolution {
    pub category_concept: Concept,
    pub code_path: String,
    pub name_path: String,
    pub depth: u32,
}

pub struct ExtensionTopologyResolver;

impl ExtensionTopologyResolver {
    /// 14 个扩展名维度到媒体大类的对应关系
    pub fn get_category_for_ext_dim(dim: Concept) -> Concept {
        match dim {
            Concept::音频扩展名 => Concept::音频,
            Concept::图片扩展名 => Concept::图片,
            Concept::视频扩展名 => Concept::视频,
            Concept::文档扩展名 => Concept::文档,
            Concept::源代码扩展名 => Concept::源代码,
            Concept::压缩包扩展名 => Concept::压缩包,
            Concept::电子书扩展名 => Concept::电子书,
            Concept::字体扩展名 => Concept::字体,
            Concept::程序扩展名 => Concept::程序,
            Concept::系统文件扩展名 => Concept::系统文件,
            Concept::数据库扩展名 => Concept::数据库,
            Concept::文本扩展名 => Concept::文本,
            Concept::应用数据扩展名 => Concept::应用数据,
            Concept::磁盘映像扩展名 => Concept::磁盘映像,
            _ => Concept::应用数据,
        }
    }

    /// 根据 MediaDomain 获取默认的扩展名维度
    pub fn get_ext_dim_by_domain(domain: MediaDomain) -> Option<Concept> {
        match domain {
            MediaDomain::Audio => Some(Concept::音频扩展名),
            MediaDomain::Image => Some(Concept::图片扩展名),
            MediaDomain::Video => Some(Concept::视频扩展名),
            MediaDomain::Document => Some(Concept::文档扩展名),
            MediaDomain::Code => Some(Concept::源代码扩展名),
            MediaDomain::Archive => Some(Concept::压缩包扩展名),
            MediaDomain::Ebook => Some(Concept::电子书扩展名),
            _ => None,
        }
    }

    /// 根据 MediaDomain 获取大类解析结果（2 级物化路径）
    pub fn resolve_category_by_domain(domain: MediaDomain) -> Option<CategoryResolution> {
        let cat_concept = match domain {
            MediaDomain::Audio => Concept::音频,
            MediaDomain::Image => Concept::图片,
            MediaDomain::Video => Concept::视频,
            MediaDomain::Document => Concept::文档,
            MediaDomain::Code => Concept::源代码,
            MediaDomain::Archive => Concept::压缩包,
            MediaDomain::Ebook => Concept::电子书,
            _ => return None,
        };
        let code_path = format!("/{}/{}", Concept::文件类型.code(), cat_concept.code());
        let name_path = format!("/{}/{}", Concept::文件类型.zh_name(), cat_concept.zh_name());
        Some(CategoryResolution {
            category_concept: cat_concept,
            code_path,
            name_path,
            depth: 2,
        })
    }

    /// 解析扩展名归属的扩展名维度
    pub fn resolve_ext_dim(ext: &str, domain_hint: Option<MediaDomain>) -> Option<Concept> {
        let clean = ext.trim().trim_start_matches(".").to_ascii_lowercase();
        if clean.is_empty() { return None; }
        
        match clean.as_str() {
            "3ga" => Some(Concept::音频扩展名),
            "3gp" => Some(Concept::视频扩展名),
            "7z" => Some(Concept::压缩包扩展名),
            "7zip" => Some(Concept::压缩包扩展名),
            "aac" => Some(Concept::音频扩展名),
            "ac3" => Some(Concept::音频扩展名),
            "accdb" => Some(Concept::数据库扩展名),
            "ace" => Some(Concept::压缩包扩展名),
            "ahk" => Some(Concept::源代码扩展名),
            "ai" => Some(Concept::文档扩展名),
            "aiff" => Some(Concept::音频扩展名),
            "amr" => Some(Concept::音频扩展名),
            "ape" => Some(Concept::音频扩展名),
            "apk" => Some(Concept::程序扩展名),
            "app" => Some(Concept::程序扩展名),
            "arw" => Some(Concept::图片扩展名),
            "asf" => Some(Concept::视频扩展名),
            "asm" => Some(Concept::源代码扩展名),
            "asp" => Some(Concept::源代码扩展名),
            "aspx" => Some(Concept::源代码扩展名),
            "au" => Some(Concept::音频扩展名),
            "au3" => Some(Concept::源代码扩展名),
            "avi" => Some(Concept::视频扩展名),
            "avif" => Some(Concept::图片扩展名),
            "awk" => Some(Concept::源代码扩展名),
            "azw3" => Some(Concept::电子书扩展名),
            "bash" => Some(Concept::源代码扩展名),
            "bat" => Some(Concept::源代码扩展名),
            "bdl" => Some(Concept::源代码扩展名),
            "bib" => Some(Concept::文本扩展名),
            "bin" => Some(Concept::程序扩展名),
            "bmp" => Some(Concept::图片扩展名),
            "bplist" => Some(Concept::应用数据扩展名),
            "bundle" => Some(Concept::源代码扩展名),
            "bz2" => match domain_hint {
                Some(MediaDomain::Archive) => Some(Concept::压缩包扩展名),
                Some(MediaDomain::Ebook) => Some(Concept::电子书扩展名),
                _ => Some(Concept::压缩包扩展名),
            },
            "bzl" => Some(Concept::源代码扩展名),
            "c" => Some(Concept::源代码扩展名),
            "caf" => Some(Concept::音频扩展名),
            "cat" => Some(Concept::应用数据扩展名),
            "cb7" => Some(Concept::电子书扩展名),
            "cbl" => Some(Concept::源代码扩展名),
            "cbr" => match domain_hint {
                Some(MediaDomain::Archive) => Some(Concept::压缩包扩展名),
                Some(MediaDomain::Ebook) => Some(Concept::电子书扩展名),
                _ => Some(Concept::压缩包扩展名),
            },
            "cbt" => Some(Concept::电子书扩展名),
            "cbz" => match domain_hint {
                Some(MediaDomain::Archive) => Some(Concept::压缩包扩展名),
                Some(MediaDomain::Ebook) => Some(Concept::电子书扩展名),
                _ => Some(Concept::压缩包扩展名),
            },
            "cc" => Some(Concept::源代码扩展名),
            "chm" => Some(Concept::应用数据扩展名),
            "cjs" => Some(Concept::源代码扩展名),
            "cl" => Some(Concept::源代码扩展名),
            "class" => Some(Concept::程序扩展名),
            "clj" => Some(Concept::源代码扩展名),
            "cljc" => Some(Concept::源代码扩展名),
            "cljr" => Some(Concept::源代码扩展名),
            "cljs" => Some(Concept::源代码扩展名),
            "cmake" => Some(Concept::源代码扩展名),
            "cmd" => Some(Concept::源代码扩展名),
            "cob" => Some(Concept::源代码扩展名),
            "coffee" => Some(Concept::源代码扩展名),
            "cpp" => Some(Concept::源代码扩展名),
            "cppm" => Some(Concept::源代码扩展名),
            "cpy" => Some(Concept::源代码扩展名),
            "cr2" => Some(Concept::图片扩展名),
            "crx" => Some(Concept::程序扩展名),
            "cs" => Some(Concept::源代码扩展名),
            "csproj" => Some(Concept::源代码扩展名),
            "css" => Some(Concept::源代码扩展名),
            "csv" => Some(Concept::文档扩展名),
            "csx" => Some(Concept::源代码扩展名),
            "cts" => Some(Concept::源代码扩展名),
            "cxx" => Some(Concept::源代码扩展名),
            "dart" => Some(Concept::源代码扩展名),
            "db" => Some(Concept::数据库扩展名),
            "db3" => Some(Concept::数据库扩展名),
            "dbf" => Some(Concept::数据库扩展名),
            "dcm" => Some(Concept::图片扩展名),
            "deb" => Some(Concept::压缩包扩展名),
            "dex" => Some(Concept::程序扩展名),
            "dff" => Some(Concept::音频扩展名),
            "diff" => Some(Concept::文本扩展名),
            "djvu" => Some(Concept::电子书扩展名),
            "dll" => match domain_hint {
                _ => Some(Concept::系统文件扩展名),
            },
            "dm" => Some(Concept::源代码扩展名),
            "dmg" => Some(Concept::压缩包扩展名),
            "dng" => Some(Concept::图片扩展名),
            "doc" => Some(Concept::文档扩展名),
            "docm" => Some(Concept::文档扩展名),
            "docx" => Some(Concept::文档扩展名),
            "drv" => Some(Concept::系统文件扩展名),
            "dsd" => Some(Concept::音频扩展名),
            "dsf" => Some(Concept::音频扩展名),
            "dts" => Some(Concept::音频扩展名),
            "dv" => Some(Concept::视频扩展名),
            "dwg" => Some(Concept::图片扩展名),
            "dxf" => Some(Concept::图片扩展名),
            "ear" => Some(Concept::压缩包扩展名),
            "elf" => Some(Concept::程序扩展名),
            "emf" => Some(Concept::应用数据扩展名),
            "epub" => match domain_hint {
                Some(MediaDomain::Document) => Some(Concept::文档扩展名),
                Some(MediaDomain::Ebook) => Some(Concept::电子书扩展名),
                _ => Some(Concept::文档扩展名),
            },
            "erb" => Some(Concept::源代码扩展名),
            "erl" => Some(Concept::源代码扩展名),
            "exe" => Some(Concept::程序扩展名),
            "f03" => Some(Concept::文档扩展名),
            "f4v" => Some(Concept::视频扩展名),
            "f90" => Some(Concept::文档扩展名),
            "f95" => Some(Concept::文档扩展名),
            "fb2" => Some(Concept::电子书扩展名),
            "fdb" => Some(Concept::数据库扩展名),
            "flac" => Some(Concept::音频扩展名),
            "flv" => Some(Concept::视频扩展名),
            "gemspec" => Some(Concept::源代码扩展名),
            "gif" => Some(Concept::图片扩展名),
            "go" => Some(Concept::源代码扩展名),
            "gpg" => Some(Concept::应用数据扩展名),
            "gradle" => Some(Concept::源代码扩展名),
            "groovy" => Some(Concept::源代码扩展名),
            "gz" => Some(Concept::压缩包扩展名),
            "gzip" => Some(Concept::压缩包扩展名),
            "h" => Some(Concept::源代码扩展名),
            "h5" => Some(Concept::压缩包扩展名),
            "handlebars" => Some(Concept::源代码扩展名),
            "hbs" => Some(Concept::源代码扩展名),
            "hdf5" => Some(Concept::压缩包扩展名),
            "heic" => Some(Concept::图片扩展名),
            "heif" => Some(Concept::图片扩展名),
            "hlp" => Some(Concept::应用数据扩展名),
            "hpp" => Some(Concept::源代码扩展名),
            "hrl" => Some(Concept::源代码扩展名),
            "hs" => Some(Concept::源代码扩展名),
            "icns" => Some(Concept::图片扩展名),
            "ico" => Some(Concept::图片扩展名),
            "ics" => Some(Concept::应用数据扩展名),
            "img" => Some(Concept::磁盘映像扩展名),
            "ipynb" => Some(Concept::源代码扩展名),
            "iso" => Some(Concept::压缩包扩展名),
            "ixx" => Some(Concept::源代码扩展名),
            "j2" => Some(Concept::源代码扩展名),
            "j2k" => Some(Concept::图片扩展名),
            "jar" => Some(Concept::压缩包扩展名),
            "java" => Some(Concept::源代码扩展名),
            "jinja" => Some(Concept::源代码扩展名),
            "jinja2" => Some(Concept::源代码扩展名),
            "jl" => Some(Concept::源代码扩展名),
            "jp2" => Some(Concept::图片扩展名),
            "jpeg" => Some(Concept::图片扩展名),
            "jpg" => Some(Concept::图片扩展名),
            "jpx" => Some(Concept::图片扩展名),
            "js" => Some(Concept::源代码扩展名),
            "json" => Some(Concept::源代码扩展名),
            "jsx" => Some(Concept::源代码扩展名),
            "jxl" => Some(Concept::图片扩展名),
            "klib" => Some(Concept::压缩包扩展名),
            "kt" => Some(Concept::源代码扩展名),
            "kts" => Some(Concept::源代码扩展名),
            "l" => Some(Concept::源代码扩展名),
            "lha" => Some(Concept::压缩包扩展名),
            "lhs" => Some(Concept::源代码扩展名),
            "lisp" => Some(Concept::源代码扩展名),
            "lnk" => Some(Concept::应用数据扩展名),
            "lock" => Some(Concept::系统文件扩展名),
            "lrc" => Some(Concept::文本扩展名),
            "lsp" => Some(Concept::源代码扩展名),
            "lua" => Some(Concept::源代码扩展名),
            "lzh" => Some(Concept::压缩包扩展名),
            "m2ts" => Some(Concept::视频扩展名),
            "m3u" => Some(Concept::应用数据扩展名),
            "m3u8" => Some(Concept::应用数据扩展名),
            "m4" => Some(Concept::源代码扩展名),
            "m4a" => Some(Concept::音频扩展名),
            "m4v" => Some(Concept::视频扩展名),
            "matlab" => Some(Concept::源代码扩展名),
            "md" => Some(Concept::文档扩展名),
            "mdb" => Some(Concept::数据库扩展名),
            "mdf" => Some(Concept::磁盘映像扩展名),
            "mds" => Some(Concept::磁盘映像扩展名),
            "mid" => Some(Concept::音频扩展名),
            "midi" => Some(Concept::音频扩展名),
            "mjs" => Some(Concept::源代码扩展名),
            "mkv" => Some(Concept::视频扩展名),
            "mobi" => Some(Concept::电子书扩展名),
            "mov" => Some(Concept::视频扩展名),
            "mp3" => Some(Concept::音频扩展名),
            "mp4" => Some(Concept::视频扩展名),
            "mpeg" => Some(Concept::视频扩展名),
            "mpg" => Some(Concept::视频扩展名),
            "mqa" => Some(Concept::音频扩展名),
            "msi" => Some(Concept::压缩包扩展名),
            "mts" => Some(Concept::源代码扩展名),
            "mum" => Some(Concept::应用数据扩展名),
            "mxf" => Some(Concept::视频扩展名),
            "nef" => Some(Concept::图片扩展名),
            "npy" => Some(Concept::压缩包扩展名),
            "npz" => Some(Concept::压缩包扩展名),
            "nrg" => Some(Concept::磁盘映像扩展名),
            "numbers" => Some(Concept::文档扩展名),
            "o" => Some(Concept::程序扩展名),
            "obj" => Some(Concept::程序扩展名),
            "odp" => Some(Concept::文档扩展名),
            "ods" => Some(Concept::文档扩展名),
            "odt" => Some(Concept::文档扩展名),
            "oga" => Some(Concept::音频扩展名),
            "ogg" => Some(Concept::音频扩展名),
            "ogv" => Some(Concept::视频扩展名),
            "one" => Some(Concept::文档扩展名),
            "onnx" => Some(Concept::压缩包扩展名),
            "opus" => Some(Concept::音频扩展名),
            "otf" => Some(Concept::字体扩展名),
            "ova" => Some(Concept::磁盘映像扩展名),
            "patch" => Some(Concept::文本扩展名),
            "pcap" => Some(Concept::应用数据扩展名),
            "pcapng" => Some(Concept::应用数据扩展名),
            "pcx" => Some(Concept::图片扩展名),
            "pdb" => Some(Concept::应用数据扩展名),
            "pdf" => Some(Concept::文档扩展名),
            "pem" => Some(Concept::应用数据扩展名),
            "php" => Some(Concept::源代码扩展名),
            "pickle" => Some(Concept::应用数据扩展名),
            "pid" => Some(Concept::系统文件扩展名),
            "pkg" => Some(Concept::压缩包扩展名),
            "pkl" => Some(Concept::应用数据扩展名),
            "pl" => Some(Concept::源代码扩展名),
            "plist" => Some(Concept::应用数据扩展名),
            "pm" => Some(Concept::源代码扩展名),
            "png" => Some(Concept::图片扩展名),
            "po" => Some(Concept::应用数据扩展名),
            "ppt" => Some(Concept::文档扩展名),
            "pptm" => Some(Concept::文档扩展名),
            "pptx" => Some(Concept::文档扩展名),
            "ps" => Some(Concept::文档扩展名),
            "ps1" => Some(Concept::源代码扩展名),
            "psd" => Some(Concept::图片扩展名),
            "pt" => Some(Concept::应用数据扩展名),
            "pth" => Some(Concept::应用数据扩展名),
            "pub" => Some(Concept::应用数据扩展名),
            "py" => Some(Concept::源代码扩展名),
            "pyc" => Some(Concept::程序扩展名),
            "pyo" => Some(Concept::程序扩展名),
            "qcow2" => Some(Concept::磁盘映像扩展名),
            "r" => Some(Concept::源代码扩展名),
            "ra" => Some(Concept::音频扩展名),
            "rar" => Some(Concept::压缩包扩展名),
            "raw" => match domain_hint {
                Some(MediaDomain::Image) => Some(Concept::图片扩展名),
                _ => Some(Concept::磁盘映像扩展名),
            },
            "rb" => Some(Concept::源代码扩展名),
            "rdf" => Some(Concept::文本扩展名),
            "reg" => Some(Concept::应用数据扩展名),
            "rm" => Some(Concept::视频扩展名),
            "rmvb" => Some(Concept::视频扩展名),
            "rpm" => Some(Concept::压缩包扩展名),
            "rs" => Some(Concept::源代码扩展名),
            "rst" => Some(Concept::文本扩展名),
            "rtf" => Some(Concept::文档扩展名),
            "scala" => Some(Concept::源代码扩展名),
            "sed" => Some(Concept::源代码扩展名),
            "service" => Some(Concept::系统文件扩展名),
            "sgml" => Some(Concept::文本扩展名),
            "sh" => Some(Concept::源代码扩展名),
            "sln" => Some(Concept::源代码扩展名),
            "smali" => Some(Concept::源代码扩展名),
            "snap" => Some(Concept::压缩包扩展名),
            "snd" => Some(Concept::音频扩展名),
            "socket" => Some(Concept::系统文件扩展名),
            "sol" => Some(Concept::源代码扩展名),
            "sql" => Some(Concept::源代码扩展名),
            "sqlite" => Some(Concept::应用数据扩展名),
            "sqlite3" => Some(Concept::应用数据扩展名),
            "sqlitedb" => Some(Concept::数据库扩展名),
            "srt" => Some(Concept::应用数据扩展名),
            "stl" => Some(Concept::图片扩展名),
            "sty" => Some(Concept::文本扩展名),
            "sum" => Some(Concept::应用数据扩展名),
            "svg" => Some(Concept::图片扩展名),
            "swf" => Some(Concept::程序扩展名),
            "swift" => Some(Concept::源代码扩展名),
            "sys" => Some(Concept::系统文件扩展名),
            "tar" => Some(Concept::压缩包扩展名),
            "tbz2" => Some(Concept::压缩包扩展名),
            "tex" => Some(Concept::文本扩展名),
            "tga" => Some(Concept::图片扩展名),
            "tgz" => Some(Concept::压缩包扩展名),
            "tif" => Some(Concept::图片扩展名),
            "tiff" => Some(Concept::图片扩展名),
            "toml" => Some(Concept::源代码扩展名),
            "ts" => match domain_hint {
                Some(MediaDomain::Video) => Some(Concept::视频扩展名),
                _ => Some(Concept::源代码扩展名),
            },
            "ttf" => Some(Concept::字体扩展名),
            "twig" => Some(Concept::源代码扩展名),
            "txt" => Some(Concept::文本扩展名),
            "umd" => Some(Concept::电子书扩展名),
            "url" => Some(Concept::应用数据扩展名),
            "v" => Some(Concept::源代码扩展名),
            "vb" => Some(Concept::源代码扩展名),
            "vba" => Some(Concept::源代码扩展名),
            "vbs" => Some(Concept::源代码扩展名),
            "vcxproj" => Some(Concept::源代码扩展名),
            "vdi" => Some(Concept::磁盘映像扩展名),
            "verilog" => Some(Concept::源代码扩展名),
            "vh" => Some(Concept::源代码扩展名),
            "vhd" => match domain_hint {
                Some(MediaDomain::Code) => Some(Concept::源代码扩展名),
                _ => Some(Concept::磁盘映像扩展名),
            },
            "vhdx" => Some(Concept::磁盘映像扩展名),
            "vlg" => Some(Concept::源代码扩展名),
            "vmdk" => Some(Concept::磁盘映像扩展名),
            "vob" => Some(Concept::视频扩展名),
            "vtt" => Some(Concept::文本扩展名),
            "vue" => Some(Concept::源代码扩展名),
            "war" => Some(Concept::压缩包扩展名),
            "wasm" => Some(Concept::程序扩展名),
            "wav" => Some(Concept::音频扩展名),
            "weba" => Some(Concept::音频扩展名),
            "webm" => Some(Concept::视频扩展名),
            "webp" => Some(Concept::图片扩展名),
            "webvtt" => Some(Concept::文本扩展名),
            "wma" => Some(Concept::音频扩展名),
            "wmf" => Some(Concept::图片扩展名),
            "wmv" => Some(Concept::视频扩展名),
            "woff" => Some(Concept::字体扩展名),
            "woff2" => Some(Concept::字体扩展名),
            "xar" => Some(Concept::压缩包扩展名),
            "xcf" => Some(Concept::图片扩展名),
            "xls" => Some(Concept::文档扩展名),
            "xlsb" => Some(Concept::文档扩展名),
            "xlsm" => Some(Concept::文档扩展名),
            "xlsx" => Some(Concept::文档扩展名),
            "xml" => Some(Concept::源代码扩展名),
            "xpi" => Some(Concept::压缩包扩展名),
            "xz" => Some(Concept::压缩包扩展名),
            "yaml" => Some(Concept::源代码扩展名),
            "yar" => Some(Concept::源代码扩展名),
            "yara" => Some(Concept::源代码扩展名),
            "yml" => Some(Concept::源代码扩展名),
            "zig" => Some(Concept::源代码扩展名),
            "zip" => Some(Concept::压缩包扩展名),
            _ => None,
        }
    }

    /// 全链路求解扩展名完整 4 级物化路径
    pub fn resolve(ext: &str, domain_hint: Option<MediaDomain>) -> Option<ExtensionResolution> {
        let clean = ext.trim().trim_start_matches(".").to_ascii_lowercase();
        if clean.is_empty() { return None; }
        let ext_dim = Self::resolve_ext_dim(&clean, domain_hint)?;
        let category = Self::get_category_for_ext_dim(ext_dim);

        let tag_name = clean.clone();
        let tag_code = format!("builtin.{}", clean);

        let code_path = format!(
            "/{}/{}/{}/{}",
            Concept::文件类型.code(),
            category.code(),
            ext_dim.code(),
            tag_code
        );
        let name_path = format!(
            "/{}/{}/{}/{}",
            Concept::文件类型.zh_name(),
            category.zh_name(),
            ext_dim.zh_name(),
            tag_name
        );

        Some(ExtensionResolution {
            tag_code,
            tag_name,
            ext_dim_concept: ext_dim,
            category_concept: category,
            code_path,
            name_path,
            depth: 4,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_mp3_path() {
        let res = ExtensionTopologyResolver::resolve("mp3", None).expect("mp3 should resolve");
        assert_eq!(res.tag_code, "builtin.mp3");
        assert_eq!(res.ext_dim_concept, Concept::音频扩展名);
        assert_eq!(res.category_concept, Concept::音频);
        assert_eq!(res.code_path, "/builtin.file_type/builtin.audio/builtin.audio_extensions/builtin.mp3");
        assert_eq!(res.name_path, "/文件类型/音频/音频扩展名/mp3");
        assert_eq!(res.depth, 4);
    }

    #[test]
    fn test_resolve_disambiguate_ts() {
        let res_code = ExtensionTopologyResolver::resolve("ts", Some(MediaDomain::Code)).unwrap();
        assert_eq!(res_code.ext_dim_concept, Concept::源代码扩展名);
        assert_eq!(res_code.category_concept, Concept::源代码);
        assert_eq!(res_code.code_path, "/builtin.file_type/hownet.000000194528.n/builtin.source_code_extensions/builtin.ts");

        let res_video = ExtensionTopologyResolver::resolve("ts", Some(MediaDomain::Video)).unwrap();
        assert_eq!(res_video.ext_dim_concept, Concept::视频扩展名);
        assert_eq!(res_video.category_concept, Concept::视频);
        assert_eq!(res_video.code_path, "/builtin.file_type/omw.06277803.n/builtin.video_extensions/builtin.ts");
    }

    #[test]
    fn test_resolve_jpg() {
        let res = ExtensionTopologyResolver::resolve("jpg", None).unwrap();
        assert_eq!(res.category_concept, Concept::图片);
        assert_eq!(res.code_path, "/builtin.file_type/omw.03234306.n/builtin.image_extensions/builtin.jpg");
    }

    #[test]
    fn test_unknown_extension_returns_none() {
        assert!(ExtensionTopologyResolver::resolve("xyz", Some(MediaDomain::Audio)).is_none());
        assert!(ExtensionTopologyResolver::resolve("crdownload", None).is_none());
    }
}