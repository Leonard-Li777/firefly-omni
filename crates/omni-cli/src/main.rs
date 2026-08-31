use clap::{Parser, Subcommand};
use std::net::SocketAddr;

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
    tracing_subscriber::fmt().with_env_filter(filter).init();
    let cli = Cli::parse();

    match &cli.command {
        Some(Commands::Serve { addr }) => {
            let socket_addr: SocketAddr = addr.parse()?;
            omni_server::start_server(socket_addr).await?;
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
