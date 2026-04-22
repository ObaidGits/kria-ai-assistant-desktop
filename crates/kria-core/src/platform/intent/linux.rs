/// Linux implementation of `OsIntentBackend`.
///
/// # URI dispatch
/// Uses the `open` crate which internally selects the correct xdg/gio/kde handler
/// for the current desktop session — avoids xdg-utils fragmentation.
///
/// # App launch
/// Uses `gio launch <.desktop-path>` so that `.desktop` `Exec=` field-codes
/// (`%U`, `%f`, `%F`) are handled correctly by gio's own parser — no naive
/// string substitution is performed in K.R.I.A. code.
///
/// # AX (accessibility)
/// Stub returning `Err("not yet implemented")` — AT-SPI implementation is deferred
/// to Phase E+1 as the three target use cases are 100% URI-resolvable.
use std::collections::HashSet;

use async_trait::async_trait;
use tracing::{info, warn};

use super::OsIntentBackend;
use crate::platform::app_registry::InstalledAppRegistry;
use crate::platform::intent::capability::{AxAction, CanonicalAppId, SafeArg};

pub struct LinuxBackend {
    registry: std::sync::Arc<InstalledAppRegistry>,
}

impl LinuxBackend {
    pub fn new(registry: std::sync::Arc<InstalledAppRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl OsIntentBackend for LinuxBackend {
    async fn open_uri(&self, url: &url::Url) -> Result<(), String> {
        info!(url = url.as_str(), "LinuxBackend::open_uri");

        // The `open` crate selects `xdg-open`, `gio open`, or `kde-open` based on the
        // current desktop session. It does NOT shell out via `sh -c`; it uses a separate
        // process with a single argument.
        open::that(url.as_str()).map_err(|e| format!("open_uri failed: {e}"))
    }

    async fn launch_app(&self, app_id: &CanonicalAppId, args: &[SafeArg]) -> Result<u32, String> {
        info!(app_id = app_id.as_str(), "LinuxBackend::launch_app");

        // Look up the .desktop path from the registry.
        let desktop_path = self
            .registry
            .desktop_path(app_id)
            .ok_or_else(|| format!("no .desktop file found for '{}'", app_id.as_str()))?;

        // `gio launch <path>` handles Exec= field-code substitution (%U, %f, %F etc.)
        // using gio's own parser — safe from injection via our SafeArg tokens.
        //
        // NOTE: We pass SafeArg values as additional arguments ONLY if gio launch supports
        // them for the specific Exec= entry type. For %U-type entries, gio appends the args
        // as URLs; for plain Exec= entries they are positional.
        let mut cmd = tokio::process::Command::new("gio");
        cmd.arg("launch").arg(&desktop_path);
        for arg in args {
            cmd.arg(arg.as_str());
        }

        let child = cmd
            .spawn()
            .map_err(|e| format!("gio launch failed for '{}': {e}", app_id.as_str()))?;

        let pid = child.id().unwrap_or(0);
        Ok(pid)
    }

    async fn ax_invoke(&self, app_id: &CanonicalAppId, _action: &AxAction) -> Result<(), String> {
        // AT-SPI implementation deferred to Phase E+1.
        // The three initial use cases (Chrome search, WhatsApp, YouTube) are fully
        // URI-resolvable and do not require accessibility automation.
        warn!(
            app_id = app_id.as_str(),
            "ax_invoke not yet implemented on Linux (AT-SPI deferred)"
        );
        Err(format!(
            "accessibility automation for '{}' is not yet implemented on Linux; \
             use URI deep-links instead",
            app_id.as_str()
        ))
    }

    fn registered_schemes(&self) -> HashSet<String> {
        self.registry.registered_schemes()
    }
}
