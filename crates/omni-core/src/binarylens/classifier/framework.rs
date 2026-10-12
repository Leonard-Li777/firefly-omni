/// 运行时框架与打包器指纹识别器 (Framework Detector)

use crate::concepts::Concept;

/// 探测程序运行时框架
pub fn detect_framework(
    libraries: &[String],
    section_names: &[String],
    has_clr_header: bool,
) -> Option<String> {
    let libs_lower: Vec<String> = libraries.iter().map(|s| s.to_lowercase()).collect();
    let sections_lower: Vec<String> = section_names.iter().map(|s| s.to_lowercase()).collect();

    // 1. .NET 运行时框架
    if has_clr_header || libs_lower.iter().any(|lib| lib.contains("mscoree.dll")) {
        return Some(Concept::NET.code().to_string());
    }

    // 2. Unity 游戏引擎
    if libs_lower.iter().any(|lib| lib.contains("unityplayer.dll")) {
        return Some(Concept::Unity.code().to_string());
    }

    // 3. Flutter
    if libs_lower.iter().any(|lib| lib.contains("flutter_windows.dll")) {
        return Some(Concept::Flutter.code().to_string());
    }

    // 4. Qt 框架
    if libs_lower.iter().any(|lib| lib.contains("qt5core") || lib.contains("qt6core") || lib.contains("qtcore")) {
        return Some(Concept::Qt.code().to_string());
    }

    // 5. Electron
    if libs_lower.iter().any(|lib| lib.contains("node.dll") || lib.contains("ffmpeg.dll"))
        && sections_lower.iter().any(|s| s.contains("pak") || s.contains("electron"))
    {
        return Some(Concept::Electron.code().to_string());
    }

    // 6. Tauri (需具备 WebView2 且包含 Tauri/Wry 特征，避免将 WinUI/PowerToys 误判为 Tauri)
    if libs_lower.iter().any(|lib| lib.contains("webview2loader.dll"))
        && sections_lower.iter().any(|s| s.contains("tauri") || s.contains("wry"))
    {
        return Some(Concept::Tauri.code().to_string());
    }

    // 7. JVM
    if libs_lower.iter().any(|lib| lib.contains("jvm.dll") || lib.contains("java.dll")) {
        return Some(Concept::JVM.code().to_string());
    }

    // 8. PyInstaller 打包应用
    if sections_lower.iter().any(|s| s.contains("pyi") || s.contains("_mei"))
        || libs_lower.iter().any(|lib| lib.contains("python") || lib.contains("pyinstaller"))
    {
        return Some(Concept::PyInstaller.code().to_string());
    }

    // 兜底为原生程序
    Some(Concept::原生程序.code().to_string())
}
