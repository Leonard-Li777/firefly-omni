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
        let search_roots = [
            std::env::current_dir().unwrap_or_default(),
            PathBuf::from(r"D:\workspace\firefly-ai-folder"),
        ];

        for root in &search_roots {
            let mut cur = root.clone();
            for _ in 0..6 {
                let candidate = cur.join("apps/omni/build/presetResources/libmupdf");
                if candidate.exists() {
                    target_dir = Some(candidate);
                    break;
                }
                if !cur.pop() {
                    break;
                }
            }
            if target_dir.is_some() {
                break;
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
        println!("cargo:rerun-if-env-changed=MUPDF_LIB");
    }
}
