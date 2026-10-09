use clap::{Parser, Subcommand};
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
        #[arg(short, long, default_value = "127.0.0.1:9190")]
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
    },
    /// 提取指定文件的信息与 Markdown 文本
    Extract {
        #[arg(short, long)]
        file: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,anydoc=error,lopdf=error,czkawka_core=off,little_exif=off,symphonia=off,symphonia_bundle_mp3=off,symphonia_core=off,symphonia_bundle_flac=off,symphonia_format_isomp4=off,symphonia_format_riff=off"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_timer(MinuteSecondTimer)
        .init();
    let cli = Cli::parse();

    match &cli.command {
        Some(Commands::Serve { addr, db_path, pack_path, embedding_profile, mrl_dimension }) => {
            let socket_addr: SocketAddr = addr.parse()?;
            let db_path = db_path.as_deref().map(std::path::PathBuf::from);
            let pack_path = pack_path.as_deref().map(std::path::PathBuf::from);
            omni_server::start_server(socket_addr, db_path, pack_path, embedding_profile.clone(), *mrl_dimension).await?;
        }
        Some(Commands::Extract { file }) => {
            let config = omni_core::OmniConfig::default();
            println!("🔍 [firefly-omni] 正在提取文件: {}", file);
            let result = omni_extract::OmniExtractor::extract(file, &config).await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            omni_extract::shutdown_exiftool_daemon();
            std::process::exit(0);
        }
        None => {
            println!("firefly-omni 🚀 运行成功。使用 --help 查看命令选项。");
        }
    }

    Ok(())
}
