use std::path::Path;
use tracing_appender::rolling;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// Initialize logging with console + rotating JSON file output.
pub fn setup_logging(log_dir: &Path) {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("info,kria_core=debug,kria_desktop=debug,kria_server=debug")
    });

    // Console layer: compact single-line
    let console_layer = fmt::layer()
        .compact()
        .with_target(true)
        .with_thread_ids(false);

    // File layer: JSON rotating daily
    let file_appender = rolling::daily(log_dir, "kria.log");
    let file_layer = fmt::layer()
        .json()
        .with_writer(file_appender)
        .with_target(true)
        .with_thread_ids(true);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(console_layer)
        .with(file_layer)
        .init();

    tracing::info!("logging initialized");
}
