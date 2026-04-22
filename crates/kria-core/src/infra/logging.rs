use std::path::Path;
use tracing_appender::rolling;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use crate::infra::pipeline_trace;

fn default_filter_directives_with_pipeline_debug(pipeline_debug: bool) -> String {
    let pipeline_directive = if pipeline_debug {
        "kria_pipeline=debug"
    } else {
        "kria_pipeline=info"
    };

    [
        "info",
        "kria_core=info",
        "kria_desktop=info",
        "kria_server=info",
        "tower_http=warn",
        "llama-server=warn",
        "mcp_stderr=warn",
        "sidecar_stderr=warn",
        pipeline_directive,
    ]
    .join(",")
}

pub fn default_filter_directives() -> String {
    default_filter_directives_with_pipeline_debug(pipeline_trace::pipeline_debug_enabled())
}

pub fn build_env_filter() -> EnvFilter {
    EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(default_filter_directives()))
}

/// Initialize logging with console + rotating JSON file output.
pub fn setup_logging(log_dir: &Path) {
    let _ = std::fs::create_dir_all(log_dir);
    let env_filter = build_env_filter();

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

    tracing::info!(
        pipeline_debug = pipeline_trace::pipeline_debug_enabled(),
        "logging initialized"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn essential_profile_disables_pipeline_debug_target() {
        let directives = default_filter_directives_with_pipeline_debug(false);
        assert!(directives.contains("kria_pipeline=info"));
        assert!(directives.contains("llama-server=warn"));
        assert!(directives.contains("mcp_stderr=warn"));
    }

    #[test]
    fn debug_profile_enables_pipeline_debug_target() {
        let directives = default_filter_directives_with_pipeline_debug(true);
        assert!(directives.contains("kria_pipeline=debug"));
    }
}
