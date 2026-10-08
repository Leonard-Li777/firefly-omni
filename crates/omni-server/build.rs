//! build.rs for omni-server
//! 确保在 Windows 环境下将 ONNX Runtime 与 Sherpa 动态库同步至 target/{profile}/deps/
//! 彻底免疫 cargo test 执行测试二进制时发生 C:\Windows\System32\onnxruntime.dll (v1.17.1) 劫持问题。

use std::path::PathBuf;

fn main() {
    #[cfg(target_os = "windows")]
    {
        if let Ok(out_dir) = std::env::var("OUT_DIR") {
            let out_path = PathBuf::from(out_dir);
            // OUT_DIR 通常为 target/{debug|release}/build/omni-server-xxxx/out
            if let Some(target_dir) = out_path.ancestors().find(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map_or(false, |s| s == "debug" || s == "release")
            }) {
                let deps_dir = target_dir.join("deps");
                let _ = std::fs::create_dir_all(&deps_dir);

                // 1. 同步 target_dir 下的 DLL 到 deps
                if let Ok(entries) = std::fs::read_dir(target_dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().and_then(|e| e.to_str()) == Some("dll") {
                            if let Some(name) = path.file_name() {
                                let _ = std::fs::copy(&path, deps_dir.join(name));
                            }
                        }
                    }
                }

                // 2. 检查 extraResources/bin/onnx 兜底
                let extra_onnx = PathBuf::from("../../../desktop/build/extraResources/bin/onnx");
                if extra_onnx.exists() {
                    if let Ok(entries) = std::fs::read_dir(&extra_onnx) {
                        for entry in entries.flatten() {
                            let path = entry.path();
                            if path.extension().and_then(|e| e.to_str()) == Some("dll") {
                                if let Some(name) = path.file_name() {
                                    let _ = std::fs::copy(&path, target_dir.join(name));
                                    let _ = std::fs::copy(&path, deps_dir.join(name));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
