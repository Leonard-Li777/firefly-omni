use std::error::Error;
use std::path::{Path, PathBuf};

fn main() {
    build_usearch().expect("Failed to build USearch");
}

/// 修复 cxx-build 在本机生成空 `rust/cxx.h` 的问题。
///
/// 背景：部分环境中 `cxx_build` 通过 `out::write(shared_cxx_h, HEADER)` 写出的 cxx.h 为
/// 0 字节（HEADER 常量在预编译的 cxx-build crate 中为空，或符号链接在 Windows 上创建失败），
/// 导致 `lib.rs.cc` / `lib.cpp` 里 `#include "rust/cxx.h"` 解析到空文件，进而报
/// “rust 不是类或命名空间” / C2440 等一连串编译错误。
///
/// 自愈策略：定位 cxx crate 源码中的完整 `include/cxx.h`（29568 字节），就地覆盖所有
/// 生成目录中长度为 0 的 `cxx.h`。若找不到源文件则静默跳过（不影响非空场景）。
fn heal_empty_cxx_header(out_dir: &Path) {
    // 生成目录中的候选 cxx.h 位置
    let mut candidates: Vec<PathBuf> = Vec::new();
    // out_dir = <target>/<profile>/build/usearch-<hash>/out
    candidates.push(out_dir.join("cxxbridge").join("include").join("rust").join("cxx.h"));
    // 沿祖先回溯，定位 target 根（含 cxxbridge/rust/cxx.h 的 shared_dir）
    for anc in out_dir.ancestors() {
        let shared = anc.join("cxxbridge").join("rust").join("cxx.h");
        if shared.exists() {
            candidates.push(shared);
        }
    }

    // 寻找 cxx crate 源码中的完整 cxx.h
    let source = find_cxx_source_header();
    let Some(source) = source else { return };
    let Ok(full) = std::fs::read(&source) else { return };
    if full.is_empty() {
        return;
    }

    for cand in candidates {
        let needs_fix = match std::fs::metadata(&cand) {
            Ok(meta) => meta.len() == 0,
            Err(_) => false,
        };
        if needs_fix {
            if let Some(parent) = cand.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&cand, &full);
            println!("cargo:warning=usearch vendor: 已修复空 cxx.h -> {}", cand.display());
        }
    }
}

/// 在 CARGO_HOME 的 registry 源码中查找 cxx-*/include/cxx.h。
fn find_cxx_source_header() -> Option<PathBuf> {
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join(".cargo")))?;
    let registry_src = home.join("registry").join("src");
    let entries = std::fs::read_dir(&registry_src).ok()?;
    for reg in entries.flatten() {
        let Ok(crates) = std::fs::read_dir(reg.path()) else { continue };
        for c in crates.flatten() {
            let name = c.file_name().to_string_lossy().to_string();
            if name.starts_with("cxx-") {
                let header = c.path().join("include").join("cxx.h");
                if header.exists() {
                    return Some(header);
                }
            }
        }
    }
    None
}

fn build_usearch() -> Result<(), Box<dyn Error>> {
    let mut build = cxx_build::bridge("rust/lib.rs");

    // cxx_build::bridge 已写入 out_dir，尝试自愈空 cxx.h
    if let Ok(out_dir) = std::env::var("OUT_DIR") {
        heal_empty_cxx_header(Path::new(&out_dir));
    }

    build
        .file("rust/lib.cpp")
        .flag_if_supported("-Wno-unknown-pragmas")
        .warnings(false)
        .include("include")
        .include("rust");


    // Check for optional features
    if cfg!(feature = "openmp") {
        build.define("USEARCH_USE_OPENMP", "1");
        let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
        if target_os == "windows" {
            build.flag_if_supported("/openmp");
        } else {
            build.flag_if_supported("-fopenmp");
            println!("cargo:rustc-link-lib=dylib=omp");
        }
    } else {
        build.define("USEARCH_USE_OPENMP", "0");
    }

    // When the `numkong` feature is enabled, the `numkong` crate (pulled from crates.io,
    // not the local git submodule) compiles all SIMD kernels itself, with dynamic dispatch
    // and fallback across ISA backends. We only need its include path for the C++ headers.
    if cfg!(feature = "numkong") {
        let numkong_include = std::env::var("DEP_NUMKONG_INCLUDE")
            .map_err(|_| "numkong crate must set DEP_NUMKONG_INCLUDE via `links` metadata")?;
        build
            .include(&numkong_include)
            .define("USEARCH_USE_NUMKONG", "1")
            .define("NK_DYNAMIC_DISPATCH", "1")
            .define("NK_NATIVE_BF16", "0")
            .define("NK_NATIVE_F16", "0");

        // Link the NumKong static library compiled by the numkong crate. Cargo propagates
        // the library search path via `links` metadata, but doesn't re-emit `-lnumkong`
        // for downstream native code. Our C++ libusearch.a references NumKong symbols
        // (nk_find_kernel_punned, nk_capabilities), so we must link explicitly.
        println!("cargo:rustc-link-lib=static=numkong");
    } else {
        build.define("USEARCH_USE_NUMKONG", "0");
    }

    let target_os = std::env::var("CARGO_CFG_TARGET_OS")?;
    // Conditional compilation depending on the target operating system.
    if target_os == "linux" || target_os == "android" {
        build
            .flag_if_supported("-std=c++17")
            .flag_if_supported("-O3")
            .flag_if_supported("-ffast-math")
            .flag_if_supported("-fdiagnostics-color=always")
            .flag_if_supported("-g1"); // Simplify debugging
    } else if target_os == "macos" {
        build
            .flag_if_supported("-mmacosx-version-min=10.15")
            .flag_if_supported("-std=c++17")
            .flag_if_supported("-O3")
            .flag_if_supported("-ffast-math")
            .flag_if_supported("-fcolor-diagnostics")
            .flag_if_supported("-g1"); // Simplify debugging
    } else if target_os == "windows" {
        build
            // Windows/MSVC 兼容修复：cxxbridge 生成的 lib.rs.cc 在 MSVC 14.44 + `/std:c++17`
            // 下，对 `&NativeIndex::search_f32` 等成员函数取址会错误解析到不匹配的重载
            // （报 C2440：指向成员函数的类型不相关）。提升到 `/std:c++20` 可正确完成
            // 重载解析，规避该上游生成代码缺陷；同时保持 `/permissive-` 严格一致性模式
            // 以避免破坏 lib.cpp 中模板/类型别名的正常解析。
            .flag_if_supported("/std:c++20")
            .flag_if_supported("/O2")
            .flag_if_supported("/fp:fast")
            .flag_if_supported("/W1") // Reduce warnings verbosity
            .flag_if_supported("/EHsc")
            .flag_if_supported("/permissive-")
            .flag_if_supported("/sdl-")
            .define("_ALLOW_RUNTIME_LIBRARY_MISMATCH", None)
            .define("_ALLOW_POINTER_TO_CONST_MISMATCH", None);
    }

    build.try_compile("usearch")?;

    println!("cargo:rerun-if-changed=rust/lib.rs");
    println!("cargo:rerun-if-changed=rust/lib.cpp");
    println!("cargo:rerun-if-changed=rust/lib.hpp");
    println!("cargo:rerun-if-changed=include/usearch/index.hpp");
    println!("cargo:rerun-if-changed=include/usearch/index_plugins.hpp");
    println!("cargo:rerun-if-changed=include/usearch/index_dense.hpp");
    Ok(())
}
