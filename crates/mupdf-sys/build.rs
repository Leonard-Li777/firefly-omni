use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let mut target_dir: Option<PathBuf> = None;

    if let Ok(lib_dir) = env::var("MUPDF_LIB") {
        let p = PathBuf::from(lib_dir);
        if p.exists() {
            target_dir = Some(p);
        }
    }

    if target_dir.is_none() {
        let mut cur = env::current_dir().unwrap_or_default();
        for _ in 0..6 {
            let preset = cur.join("apps/omni/build/presetResources/libmupdf");
            if preset.exists() {
                target_dir = Some(preset);
                break;
            }
            let preset_rel = cur.join("build/presetResources/libmupdf");
            if preset_rel.exists() {
                target_dir = Some(preset_rel);
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

    // 如果存在合并库 mupdf_all.lib / libmupdf_all.a，直接全量链接；否则遍历链接全部 lib / a
    let has_all_lib = lib_dir.join("mupdf_all.lib").exists() || lib_dir.join("libmupdf_all.a").exists();
    if has_all_lib {
        println!("cargo:rustc-link-lib=static=mupdf_all");
    } else if let Ok(entries) = fs::read_dir(&lib_dir) {
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

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

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
