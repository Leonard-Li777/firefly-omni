fn main() {
    println!("cargo:rerun-if-env-changed=FFP_MASTER_KEY_HEX");
    println!("cargo:rerun-if-env-changed=FFP_MASTER_KEY_FILE");
    println!("cargo:rerun-if-env-changed=FFP_KEY_EPOCH");

    let profile = std::env::var("PROFILE").unwrap_or_default();
    let dev_seed_hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    // 密钥解析顺序：环境变量明文 -> Secret 文件读取 -> debug 模式回退 Dev Seed
    let key_hex = match std::env::var("FFP_MASTER_KEY_HEX") {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => match std::env::var("FFP_MASTER_KEY_FILE") {
            Ok(file_path) if !file_path.trim().is_empty() => std::fs::read_to_string(&file_path)
                .unwrap_or_else(|e| panic!("无法读取 FFP_MASTER_KEY_FILE 文件 {file_path}: {e}"))
                .trim()
                .to_string(),
            _ if profile != "release" => {
                println!("cargo:warning=未提供生产密钥，debug 构建使用 Dev Key 测试种子（严禁随生产制品分发）");
                dev_seed_hex.to_string()
            }
            _ => panic!(
                "release 构建必须显式提供 FFP_MASTER_KEY_HEX 或 FFP_MASTER_KEY_FILE；\
                 禁止以 Dev Key 构建发布制品，CI Secret 配置遗漏请检查流水线"
            ),
        },
    };

    let key_bytes = hex::decode(&key_hex).expect("密钥必须是 64 字符 hex");
    assert_eq!(key_bytes.len(), 32, "密钥必须是 256 位 (32 字节)");

    // 密钥代次：与主密钥同步注入，默认为 1
    let key_epoch: u16 = std::env::var("FFP_KEY_EPOCH")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);

    // 将 32 字节分成 4 片，生成 const 数组，并写入 OUT_DIR/key_frags.rs
    let out_dir = std::env::var("OUT_DIR").expect("缺少 OUT_DIR 环境变量");
    let dest_path = std::path::Path::new(&out_dir).join("key_frags.rs");

    let content = format!(
        "// 自动生成的混淆密钥分片常量，严禁手改\n\
         pub(crate) const KEY_FRAG_A: [u8; 8] = {:?};\n\
         pub(crate) const KEY_FRAG_B: [u8; 8] = {:?};\n\
         pub(crate) const KEY_FRAG_C: [u8; 8] = {:?};\n\
         pub(crate) const KEY_FRAG_D: [u8; 8] = {:?};\n\
         pub const LOCAL_KEY_EPOCH: u16 = {};\n",
        &key_bytes[0..8],
        &key_bytes[8..16],
        &key_bytes[16..24],
        &key_bytes[24..32],
        key_epoch
    );
    std::fs::write(&dest_path, content).expect("写入 key_frags.rs 失败");
}
