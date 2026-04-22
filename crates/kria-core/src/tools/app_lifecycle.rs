use crate::infra::ToolResult;
use crate::platform::app_registry::InstalledAppRegistry;
use crate::platform::intent::capability::{Capability, SafeArg};
use crate::platform::intent::dispatcher::{DispatchError, IntentDispatcher};
use crate::platform::intent::scheme::{build_search_url, build_youtube_search_url};
use crate::safety::RiskLevel;
use crate::tools::registry::{ParamDef, ToolDef, ToolHandler, ToolRegistry};
use async_trait::async_trait;
use std::sync::Arc;

fn param(name: &str, ty: &str, desc: &str, required: bool) -> ParamDef {
    ParamDef {
        name: name.into(),
        param_type: ty.into(),
        description: desc.into(),
        required,
        default: None,
    }
}

// ─── OpenApplication ─────────────────────────────────────────────────────────
//
// Delegates to `IntentDispatcher` instead of raw `tokio::process::Command`.
// The tool name is preserved ("open_application") for LLM prompt compatibility;
// only the implementation changes.

struct OpenApplication {
    dispatcher: Arc<IntentDispatcher>,
    registry: Arc<InstalledAppRegistry>,
}

#[async_trait]
impl ToolHandler for OpenApplication {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let name = params["name"].as_str().unwrap_or("").trim();
        if name.is_empty() {
            return ToolResult::err("application name is required");
        }

        let session_id = params["session_id"]
            .as_str()
            .unwrap_or("no-session")
            .to_string();

        // Resolve name alias → CanonicalAppId.
        let app_id = match self.registry.resolve_alias(name) {
            Some(id) => id,
            None => {
                return ToolResult::err(format!(
                    "application '{}' is not found in the installed app registry",
                    name
                ))
            }
        };

        // Build SafeArg list from the params "args" array.
        let mut safe_args: Vec<SafeArg> = Vec::new();
        if let Some(arr) = params["args"].as_array() {
            for v in arr {
                if let Some(s) = v.as_str() {
                    match SafeArg::new(s) {
                        Ok(a) => safe_args.push(a),
                        Err(e) => {
                            return ToolResult::err(format!("invalid argument '{s}': {e}"))
                        }
                    }
                }
            }
        }

        let cap = Capability::LaunchApp {
            app_id,
            args: safe_args,
        };

        match self.dispatcher.dispatch(&cap, &session_id, false).await {
            Ok(result) => {
                if result.success {
                    ToolResult::ok(result.detail)
                } else {
                    ToolResult::err(result.message)
                }
            }
            Err(DispatchError::PolicyBlocked(reason)) => {
                ToolResult::err(format!("blocked by policy: {reason}"))
            }
            Err(DispatchError::RateLimitExceeded(action, retry)) => {
                ToolResult::err(format!(
                    "rate limit exceeded for '{action}', retry after {retry}s"
                ))
            }
            Err(e) => ToolResult::err(format!("dispatch error: {e}")),
        }
    }
}

// ─── OpenUrl ─────────────────────────────────────────────────────────────────

struct OpenUrl {
    dispatcher: Arc<IntentDispatcher>,
}

#[async_trait]
impl ToolHandler for OpenUrl {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let raw_url = params["url"].as_str().unwrap_or("").trim();
        if raw_url.is_empty() {
            return ToolResult::err("url is required");
        }

        let session_id = params["session_id"]
            .as_str()
            .unwrap_or("no-session")
            .to_string();

        let url = match url::Url::parse(raw_url) {
            Ok(u) => u,
            Err(e) => return ToolResult::err(format!("invalid URL '{raw_url}': {e}")),
        };

        let cap = Capability::OpenUrl { url };

        match self.dispatcher.dispatch(&cap, &session_id, false).await {
            Ok(result) => {
                if result.success {
                    ToolResult::ok(result.detail)
                } else {
                    ToolResult::err(result.message)
                }
            }
            Err(DispatchError::PolicyBlocked(reason)) => {
                ToolResult::err(format!("blocked: {reason}"))
            }
            Err(DispatchError::RateLimitExceeded(action, retry)) => {
                ToolResult::err(format!("rate limited for '{action}', retry after {retry}s"))
            }
            Err(e) => ToolResult::err(format!("{e}")),
        }
    }
}

// ─── WebSearch (via default browser) ─────────────────────────────────────────
//
// "Open Chrome and search for X" → build a safe Google search URL and dispatch.

struct WebBrowserSearch {
    dispatcher: Arc<IntentDispatcher>,
}

#[async_trait]
impl ToolHandler for WebBrowserSearch {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let query = params["query"].as_str().unwrap_or("").trim();
        if query.is_empty() {
            return ToolResult::err("query is required");
        }

        let session_id = params["session_id"]
            .as_str()
            .unwrap_or("no-session")
            .to_string();

        // Check if a specific site is requested.
        let site = params["site"].as_str().unwrap_or("google");

        let url = match site.to_lowercase().as_str() {
            "youtube" | "yt" => match build_youtube_search_url(query) {
                Ok(u) => u,
                Err(e) => return ToolResult::err(format!("failed to build YouTube URL: {e}")),
            },
            _ => match build_search_url(query) {
                Ok(u) => u,
                Err(e) => return ToolResult::err(format!("failed to build search URL: {e}")),
            },
        };

        let cap = Capability::OpenUrl { url };
        match self.dispatcher.dispatch(&cap, &session_id, false).await {
            Ok(result) => {
                if result.success {
                    ToolResult::ok(result.detail)
                } else {
                    ToolResult::err(result.message)
                }
            }
            Err(DispatchError::PolicyBlocked(reason)) => {
                ToolResult::err(format!("blocked: {reason}"))
            }
            Err(e) => ToolResult::err(format!("{e}")),
        }
    }
}

// ─── SendMessage ─────────────────────────────────────────────────────────────
//
// Opens a messaging draft. Contact resolution (ambiguity handling) is expected
// to have already happened upstream; if `contact_id` + `identifier` are provided
// directly we use them, otherwise we return an error asking for disambiguation.

struct SendMessage {
    dispatcher: Arc<IntentDispatcher>,
}

#[async_trait]
impl ToolHandler for SendMessage {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        use crate::platform::intent::capability::MessageBody;
        use crate::platform::intent::resolution::{ContactId, MessagingApp};

        let app_str = params["app"].as_str().unwrap_or("whatsapp");
        let contact_name = params["contact_name"].as_str().unwrap_or("").trim();
        let contact_identifier = params["contact_identifier"].as_str().unwrap_or("").trim();
        let body_str = params["body"].as_str().unwrap_or("").trim();
        let session_id = params["session_id"]
            .as_str()
            .unwrap_or("no-session")
            .to_string();

        if contact_name.is_empty() || contact_identifier.is_empty() {
            return ToolResult::err(
                "contact_name and contact_identifier are required; \
                 resolve contact ambiguity first by asking the user to clarify",
            );
        }
        if body_str.is_empty() {
            return ToolResult::err("message body is required");
        }

        let app = match app_str.to_lowercase().as_str() {
            "whatsapp" | "wa" => MessagingApp::WhatsApp,
            "gmail" | "email" => MessagingApp::Gmail,
            "telegram" | "tg" => MessagingApp::Telegram,
            "signal" => MessagingApp::Signal,
            other => {
                return ToolResult::err(format!(
                    "unsupported messaging app '{other}'; use: whatsapp, gmail, telegram, signal"
                ))
            }
        };

        let body = match MessageBody::new(body_str) {
            Ok(b) => b,
            Err(e) => return ToolResult::err(format!("invalid message body: {e}")),
        };

        let contact = ContactId {
            display_name: contact_name.to_string(),
            identifier: contact_identifier.to_string(),
            app: app.clone(),
        };

        let cap = Capability::SendMessage {
            app,
            contact,
            body,
        };

        match self.dispatcher.dispatch(&cap, &session_id, false).await {
            Ok(result) => {
                if result.success {
                    ToolResult::ok(result.detail)
                } else {
                    ToolResult::err(result.message)
                }
            }
            Err(DispatchError::PolicyBlocked(reason)) => {
                ToolResult::err(format!("blocked: {reason}"))
            }
            Err(DispatchError::RateLimitExceeded(action, retry)) => {
                ToolResult::err(format!("rate limited for '{action}', retry after {retry}s"))
            }
            Err(e) => ToolResult::err(format!("{e}")),
        }
    }
}


// ─── Legacy stubs (no dispatcher) ────────────────────────────────────────────
//
// Used when `register_with_dispatcher` is called with `None`.
// Preserved for tests and early startup before the registry is ready.

struct LegacyOpenApplication;
#[async_trait]
impl ToolHandler for LegacyOpenApplication {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let app = params["name"].as_str().unwrap_or("");
        let args: Vec<String> = params["args"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        match tokio::process::Command::new(app).args(&args).spawn() {
            Ok(child) => ToolResult::ok(serde_json::json!({ "application": app, "pid": child.id(), "launched": true })),
            Err(e) => ToolResult::err(format!("failed to open {app}: {e}")),
        }
    }
}

struct LegacyOpenUrl;
#[async_trait]
impl ToolHandler for LegacyOpenUrl {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let raw = params["url"].as_str().unwrap_or("").trim();
        match open::that_detached(raw) {
            Ok(()) => ToolResult::ok(serde_json::json!({ "url": raw, "opened": true })),
            Err(e) => ToolResult::err(format!("failed to open '{raw}': {e}")),
        }
    }
}

struct LegacyWebBrowserSearch;
#[async_trait]
impl ToolHandler for LegacyWebBrowserSearch {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        use crate::platform::intent::scheme::{build_search_url, build_youtube_search_url};
        let query = params["query"].as_str().unwrap_or("").trim();
        let site = params["site"].as_str().unwrap_or("google");
        let url = match site.to_lowercase().as_str() {
            "youtube" | "yt" => build_youtube_search_url(query).map_err(|e| e.to_string()),
            _ => build_search_url(query).map_err(|e| e.to_string()),
        };
        match url {
            Ok(u) => match open::that_detached(u.as_str()) {
                Ok(()) => ToolResult::ok(serde_json::json!({ "url": u.as_str(), "opened": true })),
                Err(e) => ToolResult::err(format!("{e}")),
            },
            Err(e) => ToolResult::err(e),
        }
    }
}

struct NullSendMessage;
#[async_trait]
impl ToolHandler for NullSendMessage {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        ToolResult::err("send_message is not available: IntentDispatcher not initialized yet")
    }
}


struct ListRunningApps;
#[async_trait]
impl ToolHandler for ListRunningApps {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        let mut sys = sysinfo::System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        let procs: Vec<serde_json::Value> = sys
            .processes()
            .iter()
            .filter(|(_, p)| !p.name().to_string_lossy().is_empty())
            .map(|(pid, p)| {
                serde_json::json!({
                    "pid": pid.as_u32(),
                    "name": p.name().to_string_lossy(),
                    "cpu_percent": format!("{:.1}", p.cpu_usage()),
                    "memory_mb": p.memory() / (1024 * 1024),
                })
            })
            .collect();
        ToolResult::ok(serde_json::json!({ "processes": procs, "count": procs.len() }))
    }
}

struct FocusWindow;
#[async_trait]
impl ToolHandler for FocusWindow {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let title = params["title"].as_str().unwrap_or("");
        if cfg!(target_os = "linux") {
            let output = tokio::process::Command::new("wmctrl")
                .args(["-a", title])
                .output()
                .await;
            match output {
                Ok(o) if o.status.success() => {
                    ToolResult::ok(serde_json::json!({ "focused": title }))
                }
                _ => ToolResult::err(format!(
                    "could not focus window '{title}' (wmctrl required)"
                )),
            }
        } else {
            ToolResult::err("focus_window not implemented for this OS")
        }
    }
}

struct CloseApplication;
#[async_trait]
impl ToolHandler for CloseApplication {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let name = params["name"].as_str().unwrap_or("");
        let mut sys = sysinfo::System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        let mut killed = 0;
        for proc_ in sys.processes().values() {
            if proc_
                .name()
                .to_string_lossy()
                .to_lowercase()
                .contains(&name.to_lowercase())
            {
                proc_.kill();
                killed += 1;
            }
        }
        if killed > 0 {
            ToolResult::ok(serde_json::json!({ "name": name, "processes_closed": killed }))
        } else {
            ToolResult::err(format!("no running process matched '{name}'"))
        }
    }
}

struct KillProcess;
#[async_trait]
impl ToolHandler for KillProcess {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let pid = params["pid"].as_u64().unwrap_or(0) as u32;
        let sys_pid = sysinfo::Pid::from_u32(pid);
        let mut sys = sysinfo::System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        if let Some(proc_) = sys.process(sys_pid) {
            proc_.kill();
            ToolResult::ok(serde_json::json!({ "pid": pid, "killed": true }))
        } else {
            ToolResult::err(format!("process {pid} not found"))
        }
    }
}

pub fn register(reg: &ToolRegistry) {
    register_with_dispatcher(reg, None, None);
}

/// Full registration with an `IntentDispatcher` and `InstalledAppRegistry`.
/// Called from the Tauri command setup after both are initialized.
pub fn register_with_dispatcher(
    reg: &ToolRegistry,
    dispatcher: Option<Arc<IntentDispatcher>>,
    registry: Option<Arc<InstalledAppRegistry>>,
) {
    // Fallback: if no dispatcher is provided, use the stateless legacy handlers.
    let _has_dispatcher = dispatcher.is_some();

    let open_app_handler: Arc<dyn ToolHandler> = if let (Some(d), Some(r)) =
        (dispatcher.clone(), registry.clone())
    {
        Arc::new(OpenApplication {
            dispatcher: d,
            registry: r,
        })
    } else {
        // Legacy fallback (no dispatcher yet) — uses raw process::Command.
        Arc::new(LegacyOpenApplication)
    };

    let open_url_handler: Arc<dyn ToolHandler> = if let Some(d) = dispatcher.clone() {
        Arc::new(OpenUrl {
            dispatcher: Arc::clone(&d),
        })
    } else {
        Arc::new(LegacyOpenUrl)
    };

    let search_handler: Arc<dyn ToolHandler> = if let Some(d) = dispatcher.clone() {
        Arc::new(WebBrowserSearch {
            dispatcher: Arc::clone(&d),
        })
    } else {
        Arc::new(LegacyWebBrowserSearch)
    };

    let send_message_handler: Arc<dyn ToolHandler> = if let Some(d) = dispatcher.clone() {
        Arc::new(SendMessage {
            dispatcher: Arc::clone(&d),
        })
    } else {
        Arc::new(NullSendMessage)
    };

    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        (
            ToolDef {
                name: "open_application".into(),
                description: "Open/launch an installed application by name. \
                              Use 'browser_search' to open a browser and search simultaneously."
                    .into(),
                category: "app_lifecycle".into(),
                default_tier: RiskLevel::Green,
                min_tier: "lite",
                parameters: vec![
                    param(
                        "name",
                        "string",
                        "Application name (e.g., 'chrome', 'firefox', 'vscode', 'whatsapp')",
                        true,
                    ),
                    param("args", "array", "Optional launch arguments (no shell metacharacters)", false),
                    param("session_id", "string", "Session identifier for audit logging", false),
                ],
            },
            open_app_handler,
        ),
        (
            ToolDef {
                name: "open_url".into(),
                description: "Open a URL in the system's default handler. \
                              Only https, http, mailto, tel, and registered deep-links are allowed. \
                              file://, javascript:, data:, smb:// and similar are blocked."
                    .into(),
                category: "app_lifecycle".into(),
                default_tier: RiskLevel::Green,
                min_tier: "lite",
                parameters: vec![
                    param("url", "string", "URL to open (must use https, http, mailto, tel, or a registered deep-link scheme)", true),
                    param("session_id", "string", "Session identifier for audit logging", false),
                ],
            },
            open_url_handler,
        ),
        (
            ToolDef {
                name: "browser_search".into(),
                description: "Open the default browser and search for a topic. \
                              Use site='youtube' to search YouTube. \
                              Example: 'Open Chrome and search for lo-fi music'."
                    .into(),
                category: "app_lifecycle".into(),
                default_tier: RiskLevel::Green,
                min_tier: "lite",
                parameters: vec![
                    param("query", "string", "Search query", true),
                    param("site", "string", "Search site: 'google' (default) or 'youtube'", false),
                    param("session_id", "string", "Session identifier for audit logging", false),
                ],
            },
            search_handler,
        ),
        (
            ToolDef {
                name: "send_message".into(),
                description: "Open a messaging app with a pre-filled draft. \
                              The user must press send. Does NOT auto-send. \
                              Requires contact_name AND contact_identifier (resolved phone/email). \
                              If contact is ambiguous, ask the user to clarify first."
                    .into(),
                category: "app_lifecycle".into(),
                default_tier: RiskLevel::Yellow,
                min_tier: "lite",
                parameters: vec![
                    param("app", "string", "Messaging app: whatsapp, gmail, telegram, signal", true),
                    param("contact_name", "string", "Contact display name", true),
                    param("contact_identifier", "string", "Phone (E.164) or email, resolved from contacts", true),
                    param("body", "string", "Message body (max 4096 characters)", true),
                    param("session_id", "string", "Session identifier for audit logging", false),
                ],
            },
            send_message_handler,
        ),
        (
            ToolDef {
                name: "list_running_apps".into(),
                description: "List all running processes with CPU and memory usage".into(),
                category: "app_lifecycle".into(),
                default_tier: RiskLevel::Green,
                min_tier: "lite",
                parameters: vec![],
            },
            Arc::new(ListRunningApps),
        ),
        (
            ToolDef {
                name: "focus_window".into(),
                description: "Bring a window to the foreground by title".into(),
                category: "app_lifecycle".into(),
                default_tier: RiskLevel::Green,
                min_tier: "standard",
                parameters: vec![param(
                    "title",
                    "string",
                    "Window title (partial match)",
                    true,
                )],
            },
            Arc::new(FocusWindow),
        ),
        (
            ToolDef {
                name: "close_application".into(),
                description: "Close an application by name".into(),
                category: "app_lifecycle".into(),
                default_tier: RiskLevel::Yellow,
                min_tier: "lite",
                parameters: vec![param("name", "string", "Application name", true)],
            },
            Arc::new(CloseApplication),
        ),
        (
            ToolDef {
                name: "kill_process".into(),
                description: "Kill a process by PID".into(),
                category: "app_lifecycle".into(),
                default_tier: RiskLevel::Yellow,
                min_tier: "lite",
                parameters: vec![param("pid", "integer", "Process ID", true)],
            },
            Arc::new(KillProcess),
        ),
    ];
    for (def, handler) in tools {
        reg.register(def, handler);
    }
}

