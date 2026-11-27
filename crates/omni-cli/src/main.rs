use clap::{Parser, Subcommand};
use std::io::IsTerminal;
use std::net::SocketAddr;

/// Omni 日志时间格式：仅输出「分:秒」，避免完整时间戳刷屏
struct MinuteSecondTimer;

impl tracing_subscriber::fmt::time::FormatTime for MinuteSecondTimer {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        write!(w, "{}", chrono::Local::now().format("%M:%S"))
    }
}

#[derive(Parser)]
#[command(name = "firefly-omni")]
#[command(version)]
#[command(about = "Universal Multimodal File Intelligence Engine in Rust", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// 启动 HTTP API 与 Web UI 服务
    Serve {
        /// 宿主下发单一引导配置信封 JSON 路径（Spec §3.3）
        #[arg(long)]
        bootstrap: Option<String>,
        #[arg(short, long, default_value = "127.0.0.1:38200")]
        addr: String,
        /// 桌面端 SQLite 业务主库路径（业务持久化操作；缺省时主库软不可用）
        #[arg(long)]
        db_path: Option<String>,
        /// 只读语义包 semantic.pack 路径（零磁盘挂载至内存独占托管；缺省时自动 discover 预置语义包）
        #[arg(long)]
        pack_path: Option<String>,
        /// 激活的嵌入画像档位（classic_light 或 gemma_unified，缺省 classic_light）
        #[arg(long)]
        embedding_profile: Option<String>,
        /// MRL 弹性降维维度（256、512 或 768，缺省 512）
        #[arg(long)]
        mrl_dimension: Option<usize>,
        /// 向量存储目录 vectors/ 路径（缺省时自动对齐 --db-path 父目录下的 vectors/）
        #[arg(long)]
        vectors_path: Option<String>,
        /// 服务发现文件根目录（缺省时取 FIREFLY_USER_DATA_DIR 或 --db-path 父目录，写入 <dir>/run/omni-config.json）
        #[arg(long)]
        discovery_dir: Option<String>,
    },
    /// 输出编译期固化的自描述契约握手 JSON（零 I/O、零日志初始化）
    Contract {
        #[arg(long)]
        json: bool,
    },
    /// 提取指定文件的信息与 Markdown 文本
    Extract {
        #[arg(short, long)]
        file: String,
    },
    /// 校验暂存或已安装视觉适配器目录内的 `vocab_v.{f16,meta.json}`（输出结构化 JSON，失败非零退出）
    VerifyVisualVocab {
        /// 待校验的模型适配器目录（支持位置参数）
        dir: Option<String>,
        /// 待校验的模型适配器目录（兼容 --dir 标志形式）
        #[arg(long = "dir")]
        dir_flag: Option<String>,
        /// 可选预期 encoder_id（2=chinese-clip, 3=mobile-clip, 4=gemma）
        #[arg(long)]
        encoder_id: Option<u8>,
    },
}

fn handle_fast_diagnostic_command(cli: &Cli) -> Option<i32> {
    match &cli.command {
        Some(Commands::Contract { json }) => {
            let git_sha = option_env!("FIREFLY_GIT_SHA").unwrap_or("unknown");
            let contract = omni_core::BinaryContract::for_edition(
                omni_server::is_pro_enabled(),
                omni_core::VERSION,
                git_sha,
            );
            if *json {
                println!("{}", serde_json::to_string_pretty(&contract).unwrap_or_default());
            } else {
                println!(
                    "firefly-omni {} ({}, git={})",
                    contract.version, contract.edition, contract.git_sha
                );
            }
            Some(0)
        }
        _ => None,
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    // 零 I/O / 零日志 / 零 DB / 零 ORT 极速诊断拦截
    if let Some(exit_code) = handle_fast_diagnostic_command(&cli) {
        std::process::exit(exit_code);
    }

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,anydoc=error,lopdf=error,czkawka_core=off,little_exif=off,symphonia=off,symphonia_bundle_mp3=off,symphonia_core=off,symphonia_bundle_flac=off,symphonia_format_isomp4=off,symphonia_format_riff=off"));
    // ANSI 颜色仅在 stdout 为交互式终端时启用。桌面端（Electron）以管道方式捕获
    // stdout，若强制输出 \x1b[2m 等转义序列，会随日志透传至渲染进程浏览器控制台与
    // 日志文件，表现为时间戳/级别前缀乱码（PRD 无关修复，见 omni-service 同步剥离）。
    let enable_ansi = std::io::stdout().is_terminal();
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_timer(MinuteSecondTimer)
        .with_ansi(enable_ansi)
        .init();

    match &cli.command {
        Some(Commands::Serve {
            bootstrap,
            addr,
            db_path,
            pack_path,
            embedding_profile,
            mrl_dimension,
            vectors_path,
            discovery_dir,
        }) => {
            let res = if let Some(bootstrap_path) = bootstrap {
                let cfg = omni_core::OmniBootstrapConfig::load_from_file(std::path::Path::new(
                    bootstrap_path,
                ))?;
                omni_server::start_server_with_bootstrap(cfg).await
            } else {
                let cfg = build_standalone_bootstrap_config(
                    addr,
                    db_path.as_deref().map(std::path::PathBuf::from),
                    pack_path.as_deref().map(std::path::PathBuf::from),
                    embedding_profile.clone(),
                    *mrl_dimension,
                    vectors_path.as_deref().map(std::path::PathBuf::from),
                    discovery_dir.as_deref().map(std::path::PathBuf::from),
                );
                if let Some(explicit_pack) = pack_path.as_deref().map(std::path::PathBuf::from) {
                    let socket_addr: SocketAddr = cfg.listen_addr.parse()?;
                    omni_server::start_server(
                        socket_addr,
                        cfg.storage.db_path.clone(),
                        Some(explicit_pack),
                        Some(cfg.runtime.embedding_profile.clone()),
                        Some(cfg.runtime.mrl_dimension),
                        cfg.storage.vectors_dir.clone(),
                        cfg.storage.user_data_dir.clone(),
                    )
                    .await
                } else {
                    omni_server::start_server_with_bootstrap(cfg).await
                }
            };
            omni_extract::shutdown_exiftool_daemon();
            res?;
        }
        Some(Commands::Extract { file }) => {
            let config = omni_core::OmniConfig::default();
            println!("🔍 [firefly-omni] 正在提取文件: {}", file);
            let result: omni_core::OmniExtractionResult = omni_extract::OmniExtractor::extract(file, &config).await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            omni_extract::shutdown_exiftool_daemon();
            std::process::exit(0);
        }
        Some(Commands::VerifyVisualVocab {
            dir,
            dir_flag,
            encoder_id,
        }) => {
            let Some(resolved_dir) = dir_flag.as_deref().or(dir.as_deref()) else {
                let err_json = serde_json::json!({
                    "ok": false,
                    "status": "corrupt",
                    "code": "MISSING_DIR_ARG",
                    "error": "缺少待校验的模型适配器目录参数 (<DIR> 或 --dir <DIR>)"
                });
                println!("{}", serde_json::to_string(&err_json)?);
                std::process::exit(2);
            };
            let target = std::path::Path::new(resolved_dir);
            match omni_server::verify_visual_vocab_staging_json(target, *encoder_id) {
                Ok(summary) => {
                    println!("{}", serde_json::to_string(&summary)?);
                    std::process::exit(0);
                }
                Err(err_json) => {
                    println!("{}", serde_json::to_string(&err_json)?);
                    std::process::exit(2);
                }
            }
        }
        Some(Commands::Contract { .. }) => {
            unreachable!("已由 handle_fast_diagnostic_command 前置拦截")
        }
        None => {
            println!("firefly-omni 🚀 运行成功。使用 --help 查看命令选项。");
        }
    }

    Ok(())
}

/// StandaloneCliAdapter：裸跑 `firefly-omni serve` 未传 `--bootstrap` 时，
/// 自动基于 `<exe_dir>/resources` 构造单一 `OmniBootstrapConfig`（Spec §3.3 / Ticket 06）。
fn resolve_standalone_static_root(exe_dir: &std::path::Path) -> std::path::PathBuf {
    let resources = exe_dir.join("resources");
    if resources.is_dir() {
        return resources;
    }
    let extra = exe_dir.join("extraResources");
    if extra.is_dir() {
        return extra;
    }
    resources
}

fn build_standalone_bootstrap_config(
    addr: &str,
    db_path: Option<std::path::PathBuf>,
    _pack_path: Option<std::path::PathBuf>,
    embedding_profile: Option<String>,
    mrl_dimension: Option<usize>,
    vectors_path: Option<std::path::PathBuf>,
    discovery_dir: Option<std::path::PathBuf>,
) -> omni_core::OmniBootstrapConfig {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")));
    build_standalone_bootstrap_config_with_exe_dir(
        &exe_dir,
        addr,
        db_path,
        embedding_profile,
        mrl_dimension,
        vectors_path,
        discovery_dir,
    )
}

fn build_standalone_bootstrap_config_with_exe_dir(
    exe_dir: &std::path::Path,
    addr: &str,
    db_path: Option<std::path::PathBuf>,
    embedding_profile: Option<String>,
    mrl_dimension: Option<usize>,
    vectors_path: Option<std::path::PathBuf>,
    discovery_dir: Option<std::path::PathBuf>,
) -> omni_core::OmniBootstrapConfig {
    let static_root = resolve_standalone_static_root(exe_dir);
    let user_data_dir = discovery_dir.or_else(|| db_path.as_ref().and_then(|p| p.parent().map(|d| d.to_path_buf())));
    let run_dir = user_data_dir.as_ref().map(|d| d.join("run"));
    let vectors_dir = vectors_path.or_else(|| db_path.as_ref().and_then(|p| p.parent().map(|d| d.join("vectors"))));

    omni_core::OmniBootstrapConfig {
        schema_version: omni_core::BOOTSTRAP_SCHEMA_VERSION,
        listen_addr: addr.to_string(),
        auth_token_file: None,
        static_resources_root: static_root,
        storage: omni_core::BootstrapStorageConfig {
            db_path,
            vectors_dir,
            run_dir,
            user_data_dir,
        },
        runtime: omni_core::BootstrapRuntimeConfig {
            embedding_profile: embedding_profile.unwrap_or_else(|| "classic_light".to_string()),
            mrl_dimension: mrl_dimension.unwrap_or(512),
            disable_direct_ml: std::env::var("OMNI_DISABLE_DML").ok().as_deref() == Some("1"),
            watch_stdin: false,
            dev_mode: cfg!(debug_assertions),
            eager_tts_warmup: false,
        },
        plugin_mounts_file: None,
    }
}

#[cfg(test)]
mod standalone_adapter_tests {
    use super::*;

    #[test]
    fn standalone_cli_adapter_builds_bootstrap_from_exe_resources() {
        let fake_exe_dir = std::env::temp_dir().join("omni-standalone-adapter-test");
        let cfg = build_standalone_bootstrap_config_with_exe_dir(
            &fake_exe_dir,
            "127.0.0.1:38200",
            Some(fake_exe_dir.join("app.db")),
            Some("gemma_unified".to_string()),
            Some(256),
            None,
            None,
        );
        assert!(cfg.validate().is_ok());
        assert_eq!(cfg.static_resources_root, fake_exe_dir.join("resources"));
        assert_eq!(cfg.runtime.embedding_profile, "gemma_unified");
        assert_eq!(cfg.runtime.mrl_dimension, 256);
        assert_eq!(cfg.storage.vectors_dir, Some(fake_exe_dir.join("vectors")));
        assert_eq!(cfg.storage.run_dir, Some(fake_exe_dir.join("run")));
    }
}
