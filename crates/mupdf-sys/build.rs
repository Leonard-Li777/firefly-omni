use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// 判断候选目录是否含 MuPDF 头文件（bindgen/cc 编译 wrapper 的硬前提）。
/// 摊平布局 fitz.h 位于 include/mupdf/fitz.h 或 include/fitz.h；按平台布局同理。
fn has_mupdf_headers(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    dir.join("include/mupdf/fitz.h").is_file()
        || dir.join("mupdf/fitz.h").is_file()
        || dir.join("include/fitz.h").is_file()
        || dir.join("fitz.h").is_file()
}

/// 候选目录是否**真的**含 libclang 动态库文件。
///
/// 仅判目录存在（`dir.exists()`）会把「空壳目录」当成可用路径：历史事故中
/// `presetResources/libclang/bin` 目录在、`libclang.dll` 已被清掉，于是 preset 路径
/// 覆盖掉系统 LLVM 的 `LIBCLANG_PATH`，bindgen 一加载就失败，表现为「资源被删了」。
/// 因此必须以动态库文件（含 Linux 版本化命名 `libclang.so.N`）为唯一判据，
/// 空壳目录一律跳过、优雅回退系统 LLVM。
fn has_libclang_library(dir: &Path, target_os: &str) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let exact = match target_os {
        "windows" => "libclang.dll",
        "macos" => "libclang.dylib",
        _ => "libclang.so",
    };
    if dir.join(exact).is_file() {
        return true;
    }
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            // Linux 常见版本化命名：libclang.so.1 / libclang.so.14.0.6
            if name.starts_with("libclang.") && entry.path().is_file() {
                return true;
            }
        }
    }
    false
}

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();

    // 自动探测并设置预置 libclang 路径，保证 Windows MSVC 与 bindgen 兼容
    let mut clang_cur = env::current_dir().unwrap_or_default();
    for _ in 0..6 {
        let preset_libclang = clang_cur.join("apps/omni/build/presetResources/libclang/bin");
        if has_libclang_library(&preset_libclang, &target_os) {
            env::set_var("LIBCLANG_PATH", &preset_libclang);
            println!("cargo:rustc-env=LIBCLANG_PATH={}", preset_libclang.display());
            break;
        }
        let preset_rel = clang_cur.join("build/presetResources/libclang/bin");
        if has_libclang_library(&preset_rel, &target_os) {
            env::set_var("LIBCLANG_PATH", &preset_rel);
            println!("cargo:rustc-env=LIBCLANG_PATH={}", preset_rel.display());
            break;
        }
        if !clang_cur.pop() {
            break;
        }
    }

    let mut target_dir: Option<PathBuf> = None;

    if let Ok(lib_dir) = env::var("MUPDF_LIB") {
        let p = PathBuf::from(lib_dir);
        if p.exists() {
            target_dir = Some(p);
        }
    }

    if target_dir.is_none() {
        // 本地 WSL/Linux 开发布局：download-libmupdf.js 按平台产出 build/libmupdf/<os>-<arch>/，
        // 头文件在 include/mupdf/ 下、静态库为 *.a；发布/CI 则摊平到 presetResources/libmupdf。
        // 旧逻辑只认 presetResources，导致本地直接 cargo 构建报 mupdf/fitz.h: No such file。
        let platform_dir = match (target_os.as_str(), target_arch.as_str()) {
            ("linux", "x86_64") => Some("linux-x86_64"),
            ("linux", "aarch64") => Some("linux-aarch64"),
            ("windows", "x86_64") => Some("windows-x86_64"),
            ("windows", "aarch64") => Some("windows-aarch64"),
            ("macos", "x86_64") => Some("macos-x86_64"),
            ("macos", "aarch64") => Some("macos-aarch64"),
            _ => None,
        };
        let mut cur = env::current_dir().unwrap_or_default();
        for _ in 0..6 {
            let preset = cur.join("apps/omni/build/presetResources/libmupdf");
            if has_mupdf_headers(&preset) {
                target_dir = Some(preset);
                break;
            }
            let preset_rel = cur.join("build/presetResources/libmupdf");
            if has_mupdf_headers(&preset_rel) {
                target_dir = Some(preset_rel);
                break;
            }
            if let Some(pd) = platform_dir {
                for lib_root in [
                    cur.join("apps/omni/build/libmupdf"),
                    cur.join("build/libmupdf"),
                ] {
                    let candidate = lib_root.join(pd);
                    if has_mupdf_headers(&candidate) {
                        target_dir = Some(candidate);
                        break;
                    }
                }
            }
            if target_dir.is_some() {
                break;
            }
            if !cur.pop() {
                break;
            }
        }
    }

    let lib_dir = target_dir.unwrap_or_else(|| {
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../build/presetResources/libmupdf")
    });

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rerun-if-env-changed=MUPDF_LIB");
    println!("cargo:rerun-if-changed=build.rs");

    if let Ok(entries) = fs::read_dir(&lib_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
                // 过滤掉不需要的 Tesseract OCR 和 Leptonica 静态库
                let lower_stem = stem.to_lowercase();
                if lower_stem.contains("tesseract") || lower_stem.contains("leptonica") || lower_stem.contains("_all") || lower_stem.contains("_combined") {
                    continue;
                }

                if ext == "lib" {
                    println!("cargo:rustc-link-lib=static={}", stem);
                } else if ext == "a" {
                    let name = stem.strip_prefix("lib").unwrap_or(stem);
                    println!("cargo:rustc-link-lib=static={}", name);
                }
            }
        }
    }

    if target_env == "msvc" {
        println!("cargo:rustc-link-lib=dylib=advapi32");
        println!("cargo:rustc-link-lib=dylib=user32");
        println!("cargo:rustc-link-lib=dylib=gdi32");
        println!("cargo:rustc-link-lib=dylib=shell32");
        println!("cargo:rustc-link-lib=dylib=windowscodecs");
    }

    if target_os == "macos" {
        println!("cargo:rustc-link-lib=dylib=z");
        println!("cargo:rustc-link-lib=dylib=c++");
        println!("cargo:rustc-link-lib=framework=CoreFoundation");
        println!("cargo:rustc-link-lib=framework=CoreGraphics");
        println!("cargo:rustc-link-lib=framework=CoreText");
    }

    if target_os == "linux" {
        println!("cargo:rustc-link-lib=dylib=z");
        println!("cargo:rustc-link-lib=dylib=m");
        println!("cargo:rustc-link-lib=dylib=pthread");
        println!("cargo:rustc-link-lib=dylib=dl");
    }

    let include_dir = if lib_dir.join("include").exists() {
        lib_dir.join("include")
    } else {
        lib_dir.clone()
    };
    let inc_str = include_dir.display().to_string();

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());

    let mut build = cc::Build::new();
    build.file(manifest_dir.join("wrapper.c")).include(&inc_str);
    build.compile("libmupdf-wrapper.a");

    let mut builder = bindgen::Builder::default()
        .clang_arg(format!("-I{}", inc_str))
        .header(manifest_dir.join("wrapper.h").to_str().unwrap())
        .header(manifest_dir.join("wrapper.c").to_str().unwrap())
        .allowlist_function("fz_.*")
        .allowlist_function("pdf_.*")
        .allowlist_function("ucdn_.*")
        .allowlist_function("Memento_.*")
        .allowlist_function("mupdf_.*")
        .allowlist_type("fz_.*")
        .allowlist_type("pdf_.*")
        .allowlist_var("fz_.*")
        .allowlist_var("FZ_.*")
        .allowlist_var("pdf_.*")
        .allowlist_var("PDF_.*")
        .allowlist_var("UCDN_.*")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .size_t_is_usize(true);

    if let Ok(target) = env::var("TARGET") {
        builder = builder.clang_arg(format!("--target={}", target));
    }

    if target_os == "macos" || env::var("TARGET").map(|t| t.contains("apple-darwin")).unwrap_or(false) {
        if let Ok(sdk_root) = env::var("SDKROOT") {
            if !sdk_root.is_empty() {
                builder = builder.clang_arg("-isysroot").clang_arg(sdk_root);
            }
        } else if let Ok(output) = std::process::Command::new("xcrun").args(["--show-sdk-path"]).output() {
            if output.status.success() {
                let sdk_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !sdk_path.is_empty() {
                    builder = builder.clang_arg("-isysroot").clang_arg(sdk_path);
                }
            }
        }
    }

    let bindings = builder
        .generate()
        .expect("Unable to generate bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings!");

    println!("cargo:rerun-if-env-changed=MUPDF_LIB");
}
