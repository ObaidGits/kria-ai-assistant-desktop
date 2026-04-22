/// Windows implementation stub for `OsIntentBackend`.
///
/// # Status: COMPILE-ONLY STUB
/// All methods return `Err("not implemented")`. The correct implementation uses:
/// - `ShellExecuteExW` (via the `windows` crate) for URI dispatch — `lpFile` and
///   `lpParameters` are separate fields, preventing shell injection.
/// - `IApplicationActivationManager::ActivateApplication` for UWP app launch.
/// - Win32 restricted-token + Job Object (NOT AppContainer, which is UWP-only)
///   for sandboxing child processes.
/// - `uiautomation-rs` crate for UIAutomation-based AX.
use std::collections::HashSet;

use async_trait::async_trait;

use super::OsIntentBackend;
use crate::platform::intent::capability::{AxAction, CanonicalAppId, SafeArg};

pub struct WindowsBackend;

impl WindowsBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WindowsBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl OsIntentBackend for WindowsBackend {
    async fn open_uri(&self, _url: &url::Url) -> Result<(), String> {
        Err("WindowsBackend not yet implemented — use ShellExecuteExW".to_string())
    }

    async fn launch_app(&self, _app_id: &CanonicalAppId, _args: &[SafeArg]) -> Result<u32, String> {
        Err("WindowsBackend not yet implemented — use IApplicationActivationManager".to_string())
    }

    async fn ax_invoke(&self, _app_id: &CanonicalAppId, _action: &AxAction) -> Result<(), String> {
        Err("WindowsBackend not yet implemented — use uiautomation crate".to_string())
    }

    fn registered_schemes(&self) -> HashSet<String> {
        // TODO: read from HKCR\<scheme>\shell\open\command
        HashSet::new()
    }
}
