/// omni-cli build.rs
/// 确保在最终生成 firefly-omni.exe 可执行文件时，将预置目录下的全量静态库传给链接器

use std::path::PathBuf;

fn main() {
    let mut target_dir: Option<PathBuf> = None;

    if let Ok(lib_dir) = std::env::var("MUPDF_LIB") {
        let p = PathBuf::from(lib_dir);
        if p.exists() {
            target_dir = Some(p);
        }
    }

    if target_dir.is_none() {
        // 构建期缓存唯一落点（多产品资源放置方案 §4.4）：
        //   apps/omni/build/presetResources/libmupdf/<platform-arch>/
        // 摊平根目录与 build/libmupdf 旧布局均已废止，此处不保留兼容候选：
        // 多留一个候选只会让「缓存放错位置」静默通过，把失败推迟到链接阶段。
        let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
        let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
        let platform_dir = match (target_os.as_str(), target_arch.as_str()) {
            ("linux", "x86_64") => Some("linux-x86_64"),
            ("linux", "aarch64") => Some("linux-aarch64"),
            ("windows", "x86_64") => Some("windows-x86_64"),
            ("windows", "aarch64") => Some("windows-aarch64"),
            ("macos", "x86_64") => Some("macos-x86_64"),
            ("macos", "aarch64") => Some("macos-aarch64"),
            _ => None,
        };

        if let Some(pd) = platform_dir {
            let roots = [
                std::env::current_dir().unwrap_or_default(),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            ];
            'outer: for root in roots {
                let mut cur = root;
                for _ in 0..8 {
                    for candidate in [
                        cur.join("apps/omni/build/presetResources/libmupdf").join(pd),
                        cur.join("build/presetResources/libmupdf").join(pd),
                    ] {
                        if candidate.join("include/mupdf/fitz.h").exists()
                            || candidate.join("mupdf/fitz.h").exists()
                        {
                            target_dir = Some(candidate);
                            break 'outer;
                        }
                    }
                    if !cur.pop() {
                        break;
                    }
                }
            }
        }
    }

    if let Some(dir) = target_dir {
        println!("cargo:rustc-link-search=native={}", dir.display());

        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
                    if ext == "lib" {
                    let lower = stem.to_lowercase();
                    if lower.contains("_all") || lower.contains("_combined") {
                        continue;
                    }
                        println!("cargo:rustc-link-lib=static={}", stem);
                    } else if ext == "a" {
                        let name = stem.strip_prefix("lib").unwrap_or(stem);
                        println!("cargo:rustc-link-lib=static={}", name);
                    }
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            println!("cargo:rustc-link-lib=dylib=advapi32");
            println!("cargo:rustc-link-lib=dylib=user32");
            println!("cargo:rustc-link-lib=dylib=gdi32");
            println!("cargo:rustc-link-lib=dylib=shell32");
            println!("cargo:rustc-link-lib=dylib=windowscodecs");
        }

        #[cfg(target_os = "macos")]
        {
            println!("cargo:rustc-link-lib=dylib=z");
            println!("cargo:rustc-link-lib=dylib=c++");
            println!("cargo:rustc-link-lib=framework=CoreFoundation");
            println!("cargo:rustc-link-lib=framework=CoreGraphics");
            println!("cargo:rustc-link-lib=framework=CoreText");
        }

        #[cfg(target_os = "linux")]
        {
            println!("cargo:rustc-link-lib=dylib=z");
            println!("cargo:rustc-link-lib=dylib=m");
            println!("cargo:rustc-link-lib=dylib=pthread");
            println!("cargo:rustc-link-lib=dylib=dl");
        }
        println!("cargo:rerun-if-env-changed=MUPDF_LIB");
    }
}
