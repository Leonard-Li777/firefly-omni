/// 二进制程序能力特征聚类分析器 (Capability Classifier)
/// 根据导入库 (DLL/so/dylib) 与核心 API 符号聚类映射为受控特征标签

use crate::concepts::Concept;

/// 提取出的特征符号集
#[derive(Debug, Default, Clone)]
pub struct SymbolProfile {
    pub libraries: Vec<String>,
    pub imported_functions: Vec<String>,
    pub is_console_subsystem: bool,
    pub is_gui_subsystem: bool,
}

/// 分析符号画像，输出命中的受控特征代码列表
pub fn classify_capabilities(profile: &SymbolProfile) -> Vec<String> {
    let mut caps = Vec::new();

    let libs_lower: Vec<String> = profile.libraries.iter().map(|s| s.to_lowercase()).collect();
    let fns_lower: Vec<String> = profile.imported_functions.iter().map(|s| s.to_lowercase()).collect();

    // 1. 网络访问能力
    let has_net = libs_lower.iter().any(|lib| {
        lib.contains("ws2_32")
            || lib.contains("wsock32")
            || lib.contains("wininet")
            || lib.contains("winhttp")
            || lib.contains("libcurl")
            || lib.contains("curl")
    }) || fns_lower.iter().any(|f| {
        f == "socket"
            || f == "connect"
            || f == "send"
            || f == "recv"
            || f == "wsastartup"
            || f == "internetopenw"
            || f == "httpopenrequestw"
    });
    if has_net {
        caps.push(Concept::具备网络访问能力.code().to_string());
    }

    // 2. 多媒体音视频支持
    let has_media = libs_lower.iter().any(|lib| {
        lib.contains("avcodec")
            || lib.contains("avformat")
            || lib.contains("avutil")
            || lib.contains("mfplat")
            || lib.contains("dsound")
            || lib.contains("xaudio2")
            || lib.contains("winmm")
            || lib.contains("openal")
            || lib.contains("sdl2")
    }) || fns_lower.iter().any(|f| {
        f.contains("playsound")
            || f.contains("waveout")
            || f.contains("mfsession")
            || f.contains("directsoundcreate")
    });
    if has_media {
        caps.push(Concept::多媒体音视频支持.code().to_string());
    }

    // 3. 硬件加速与3D图形
    let has_3d = libs_lower.iter().any(|lib| {
        lib.contains("d3d9")
            || lib.contains("d3d11")
            || lib.contains("d3d12")
            || lib.contains("dxgi")
            || lib.contains("opengl32")
            || lib.contains("vulkan-1")
            || lib.contains("glu32")
    });
    if has_3d {
        caps.push(Concept::硬件加速与3D图形.code().to_string());
    }

    // 4. 底层驱动与内核交互 (剔除通用 ntdll，保留专有驱动库与系统符号)
    let has_driver = libs_lower.iter().any(|lib| {
        lib.contains("hal.dll")
            || lib.contains("fltlib.dll")
            || lib.contains("setupapi")
    }) || fns_lower.iter().any(|f| {
        f.eq_ignore_ascii_case("DeviceIoControl")
            || f.starts_with("zw")
            || f.starts_with("ntcreate")
            || f.starts_with("ioattach")
    });
    if has_driver {
        caps.push(Concept::底层驱动与内核交互.code().to_string());
    }

    // 5. GUI 界面 vs CLI 交互
    if profile.is_gui_subsystem
        || libs_lower.iter().any(|lib| lib.contains("user32") || lib.contains("gdi32") || lib.contains("comctl32"))
    {
        caps.push(Concept::图形用户界面.code().to_string());
    } else if profile.is_console_subsystem {
        caps.push(Concept::命令行交互.code().to_string());
    }

    // 6. 本地文件存储读写 (剔除所有程序通用加载的 kernel32，纯基于文件系统操作 API 符号判定)
    let has_fs = fns_lower.iter().any(|f| {
        f.contains("createfile")
            || f.contains("writefile")
            || f.contains("readfile")
            || f.contains("fopen")
            || f.contains("fwrite")
    });
    if has_fs {
        caps.push(Concept::本地文件存储读写.code().to_string());
    }

    // 7. 数据加解密与安全运算
    let has_crypto = libs_lower.iter().any(|lib| {
        lib.contains("crypt32")
            || lib.contains("bcrypt")
            || lib.contains("ncrypt")
            || lib.contains("libcrypto")
            || lib.contains("openssl")
    }) || fns_lower.iter().any(|f| {
        f.contains("cryptacquirecontext")
            || f.contains("bcryptexportkey")
            || f.contains("bcryptencrypt")
    });
    if has_crypto {
        caps.push(Concept::数据加解密与安全运算.code().to_string());
    }

    caps
}
