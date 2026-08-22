use axum::{routing::get, Router};
use std::net::SocketAddr;
use tracing::info;

pub async fn start_server(addr: SocketAddr) -> anyhow::Result<()> {
    let app = Router::new().route("/health", get(|| async { "OK" }));

    info!("firefly-omni HTTP server starting on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
