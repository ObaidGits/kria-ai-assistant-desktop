use kria_server::{build_router, ServerState};
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging (shared profile with desktop runtime)
    let paths = kria_core::platform::paths::KriaPaths::resolve();
    kria_core::infra::logging::setup_logging(&paths.logs_dir);

    let config = kria_core::config::KriaConfig::load(None)?;
    let bind_addr = format!("{}:{}", config.server.host, config.server.port,);

    let state = Arc::new(ServerState { config });
    let app = build_router(state);

    tracing::info!("KRIA server listening on {bind_addr}");
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
