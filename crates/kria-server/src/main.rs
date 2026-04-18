use kria_server::{build_router, ServerState};
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter("kria_server=debug,kria_core=debug,tower_http=info")
        .init();

    let config = kria_core::config::KriaConfig::load(None)?;
    let bind_addr = format!("{}:{}", config.server.host, config.server.port,);

    let state = Arc::new(ServerState { config });
    let app = build_router(state);

    tracing::info!("KRIA server listening on {bind_addr}");
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
