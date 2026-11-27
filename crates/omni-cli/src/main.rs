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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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
    let cli = Cli::parse();

    match &cli.command {
        Some(Commands::Serve {
            addr,
            db_path,
            pack_path,
            embedding_profile,
            mrl_dimension,
            vectors_path,
            discovery_dir,
        }) => {
            let socket_addr: SocketAddr = addr.parse()?;
            let db_path = db_path.as_deref().map(std::path::PathBuf::from);
            let pack_path = pack_path.as_deref().map(std::path::PathBuf::from);
            let vectors_path = vectors_path.as_deref().map(std::path::PathBuf::from);
            let discovery_dir = discovery_dir.as_deref().map(std::path::PathBuf::from);
            let res = omni_server::start_server(
                socket_addr,
                db_path,
                pack_path,
                embedding_profile.clone(),
                *mrl_dimension,
                vectors_path,
                discovery_dir,
            )
            .await;
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
        None => {
            println!("firefly-omni 🚀 运行成功。使用 --help 查看命令选项。");
        }
    }

    Ok(())
}
