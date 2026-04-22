/// macOS implementation stub for `OsIntentBackend`.
///
/// # Status: COMPILE-ONLY STUB
/// All methods return `Err("not implemented")`. The correct implementation uses:
/// - `NSWorkspace.open(_:)` via `objc2-app-kit` for URI and app launch.
///   IMPORTANT: `NSWorkspace` requires dispatch to the main thread. Use
///   `MainThreadMarker` from `objc2-app-kit` — do NOT call from a tokio worker thread.
/// - `LSLaunchURLSpec` / `NSWorkspace.launchApplication(at:options:configuration:)`
///   for controlled app launch with sandboxing options.
/// - `AXUIElement` via `accessibility-sys` crate for AX automation.
use std::collections::HashSet;

use async_trait::async_trait;

use super::OsIntentBackend;
use crate::platform::intent::capability::{AxAction, CanonicalAppId, SafeArg};

pub struct MacosBackend;

impl MacosBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MacosBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl OsIntentBackend for MacosBackend {
    async fn open_uri(&self, _url: &url::Url) -> Result<(), String> {
        Err("MacosBackend not yet implemented — use NSWorkspace.open(_:) with MainThreadMarker"
            .to_string())
    }

    async fn launch_app(&self, _app_id: &CanonicalAppId, _args: &[SafeArg]) -> Result<u32, String> {
        Err("MacosBackend not yet implemented — use LSLaunchURLSpec".to_string())
    }

    async fn ax_invoke(&self, _app_id: &CanonicalAppId, _action: &AxAction) -> Result<(), String> {
        Err("MacosBackend not yet implemented — use AXUIElement via accessibility-sys".to_string())
    }

    fn registered_schemes(&self) -> HashSet<String> {
        // TODO: query LaunchServices for registered URI schemes.
        HashSet::new()
    }
}
