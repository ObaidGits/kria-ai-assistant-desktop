use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::HashSet;

use crate::platform::intent::capability::Capability;
use crate::platform::intent::scheme::{classify_url, SchemeError};
use crate::safety::blacklist::BlacklistChecker;

/// Risk classification tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum RiskLevel {
    Green,
    Yellow,
    Red,
    Black,
}

impl RiskLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Green => "GREEN",
            Self::Yellow => "YELLOW",
            Self::Red => "RED",
            Self::Black => "BLACK",
        }
    }
}

impl std::fmt::Display for RiskLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Result of policy evaluation.
#[derive(Debug, Clone)]
pub struct PolicyDecision {
    pub risk_level: RiskLevel,
    pub action: String,
    pub requires_approval: bool,
    pub blocked: bool,
    pub reason: String,
    pub escalated_from: Option<RiskLevel>,
}

// ─── Tier 0: GREEN (auto-execute) ───
static GREEN_ACTIONS: Lazy<HashSet<&str>> = Lazy::new(|| {
    [
        // App Control
        "open_application",
        "list_running_apps",
        "focus_window",
        // System Info (read-only)
        "get_cpu_usage",
        "get_memory_info",
        "get_disk_space",
        "get_network_status",
        "get_battery_status",
        "get_gpu_info",
        "get_system_uptime",
        // File Reading
        "read_file",
        "search_files",
        "list_directory",
        "get_file_info",
        "calculate_dir_size",
        "search_file_contents",
        "find_files_by_pattern",
        "get_project_structure",
        "count_lines_of_code",
        "diff_files",
        "find_todos",
        "analyze_code",
        // Document Parsing
        "parse_pdf",
        "parse_docx",
        "parse_xlsx",
        "parse_csv",
        "summarize_document",
        "parse_document",
        // Internet (read-only)
        "web_search",
        "fetch_webpage",
        "get_weather",
        "get_news",
        "get_stock_price",
        "check_url_status",
        "rss_feed_read",
        "get_public_ip",
        "searxng_search",
        "duckduckgo_search",
        "get_current_time",
        "get_exchange_rate",
        "calculate",
        // Network Diagnostics
        "ping_host",
        "dns_lookup",
        "traceroute",
        "get_active_connections",
        "get_wifi_networks",
        "speed_test",
        // Clipboard (read)
        "get_clipboard",
        "clipboard_history",
        "transform_clipboard",
        // UI
        "screenshot",
        "lock_screen",
        // Knowledge (read)
        "recall_fact",
        "list_remembered",
        "search_knowledge",
        "get_snippet",
        "list_snippets",
        // Notifications
        "send_notification",
        "compose_email",
        "open_email_draft",
        "schedule_reminder",
        // Automation (read)
        "list_workflows",
        "list_scheduled_tasks",
        "list_macros",
        // Plugins (read)
        "list_plugins",
        // Memory
        "remember_fact",
        "ingest_document",
        "save_snippet",
        // Environment (read)
        "get_environment_variable",
        "list_environment_variables",
        "get_power_plan",
        // Developer (read-only)
        "git_status",
        "git_log",
        "git_diff",
        "git_branch_list",
        "analyze_project",
        "diff_files_unified",
        // Database (read-only)
        "query_sqlite",
        "describe_database",
        // RAG (read)
        "rag_query",
        "list_knowledge_base",
        // Proactive (read)
        "check_system_health",
        "get_alerts",
        "dismiss_alert",
        "list_watched_dirs",
        "smart_suggest",
        // i18n / Accessibility (read)
        "list_languages",
        "detect_language",
        "get_accessibility_settings",
        // Desktop (read)
        "get_active_window",
        "list_windows",
        // Vision (read-only analysis)
        "ocr_image",
        "analyze_image",
        "screenshot_analyze",
        // Precognitive (analysis only)
        "image_analyze",
        "document_extract",
        "code_analyze_ast",
        "web_extract_article",
        "embeddings_generate",
        "audio_preprocess",
        // Misc
        "open_url",
        "send_message",
        "list_installed_packages",
        // Package queries (read-only)
        "search_package",
        "check_package_installed",
        "check_package_updates",
        "get_package_info",
        "search_news",
        "fetch_article",
        "list_news_sources",
        "news_status",
        // Google Workspace (read-only)
        "gw_gmail_inbox",
        "gw_gmail_search",
        "gw_gmail_read",
        "gw_calendar_today",
        "gw_calendar_search",
        "gw_drive_search",
        "gw_drive_list",
        "gw_drive_read",
        "gw_docs_read",
        "gw_sheets_read",
        "gw_slides_read",
    ]
    .into_iter()
    .collect()
});

// ─── Tier 1: YELLOW (execute + notify) ───
static YELLOW_ACTIONS: Lazy<HashSet<&str>> = Lazy::new(|| {
    [
        // App Control
        "close_application",
        "kill_process",
        // System Config
        "set_volume",
        "set_brightness",
        "toggle_wifi",
        "set_power_plan",
        "connect_wifi",
        // File Modification
        "write_file",
        "create_directory",
        "rename_file",
        "copy_file",
        // Document Conversion
        "convert_document",
        // Internet (write-ish)
        "download_file",
        // Clipboard (write)
        "set_clipboard",
        "type_text",
        // Package
        "update_application",
        // Power
        "sleep",
        "hibernate",
        // Plugins
        "enable_plugin",
        "disable_plugin",
        // Automation
        "run_workflow",
        "record_macro",
        "replay_macro",
        // Developer (non-destructive write)
        "git_stash",
        // RAG (ingest)
        "ingest_document_rag",
        // Proactive (monitoring)
        "watch_directory",
        // Desktop (window manipulation)
        "move_window",
        "resize_window",
        "maximize_window",
        "minimize_window",
        "tile_windows",
        // Google Workspace (create/edit — reversible)
        "gw_gmail_send",
        "gw_docs_create",
        "gw_docs_edit",
        "gw_sheets_create",
        "gw_sheets_edit",
        "gw_slides_create",
        "gw_calendar_create",
    ]
    .into_iter()
    .collect()
});

// ─── Tier 2: RED (block until approved) ───
static RED_ACTIONS: Lazy<HashSet<&str>> = Lazy::new(|| {
    [
        // File Destruction
        "delete_file",
        "delete_directory",
        "move_file",
        // System Administration
        "manage_service",
        "set_environment_variable",
        "add_to_path",
        "edit_shell_profile",
        "manage_firewall_rule",
        // OS Control
        "shutdown_system",
        "reboot_system",
        "clean_temp_files",
        // Package Management
        "install_application",
        "uninstall_application",
        "update_all_packages",
        "install_package",
        "uninstall_package",
        // Code Execution
        "execute_python",
        "execute_bash",
        "execute_powershell",
        // Scheduled Tasks
        "create_scheduled_task",
        "delete_scheduled_task",
        "modify_scheduled_task",
        // Registry
        "write_registry",
        // Plugins
        "install_plugin",
        "uninstall_plugin",
        // Dangerous
        "set_process_priority",
        "change_network_config",
        // Developer (destructive)
        "git_commit",
        "git_checkout",
        // RAG (destructive)
        "delete_knowledge_item",
        // Google Workspace (send/delete/share — irreversible)
        "gw_gmail_delete",
        "gw_drive_delete",
        "gw_calendar_delete",
    ]
    .into_iter()
    .collect()
});

// ─── Protected paths (auto-escalate to RED) ───
static PROTECTED_PATH_PATTERNS: Lazy<Vec<Regex>> = Lazy::new(|| {
    let raw = [
        // Windows
        r"(?i)^C:\\Windows\\",
        r"(?i)^C:\\Program Files\\",
        r"(?i)^C:\\Program Files \(x86\)\\",
        r"(?i)^C:\\ProgramData\\",
        r"(?i)\\Users\\[^\\]+\\AppData\\Local\\Microsoft\\",
        r"(?i)^C:\\Boot\\",
        r"(?i)\\System32\\",
        r"(?i)\\SysWOW64\\",
        // Linux
        r"^/etc(/|$)",
        r"^/usr(/|$)",
        r"^/var(/|$)",
        r"^/boot(/|$)",
        r"^/sys(/|$)",
        r"^/proc(/|$)",
        r"^/root(/|$)",
        r"^/sbin(/|$)",
        // Common sensitive
        r"/\.ssh(/|$)",
        r"/\.gnupg(/|$)",
        r"/\.kria/rollback(/|$)",
    ];
    raw.iter().filter_map(|p| Regex::new(p).ok()).collect()
});

fn infer_mcp_gworkspace_risk(action: &str) -> Option<RiskLevel> {
    let tool = action.strip_prefix("mcp_gworkspace_")?;
    let lower = tool.to_ascii_lowercase();

    let red_markers = ["delete", "trash", "permanent"];
    if red_markers.iter().any(|marker| lower.contains(marker)) {
        return Some(RiskLevel::Red);
    }

    let yellow_markers = [
        "create", "update", "append", "insert", "write", "add", "remove", "send", "move", "copy",
        "rename", "mark", "clear", "convert", "reply", "resolve", "apply", "format", "upload",
        "save", "edit",
    ];
    if yellow_markers.iter().any(|marker| lower.contains(marker)) {
        return Some(RiskLevel::Yellow);
    }

    Some(RiskLevel::Green)
}

/// The core policy engine. Classifies every tool invocation.
pub struct PolicyEngine {
    blacklist: BlacklistChecker,
}

impl PolicyEngine {
    pub fn new() -> Self {
        Self {
            blacklist: BlacklistChecker::new(),
        }
    }

    /// Classify a tool action and its parameters.
    pub fn evaluate(&self, action: &str, params: &serde_json::Value) -> PolicyDecision {
        // 1. Check blacklist first (hardcoded deny, cannot be overridden)
        let param_str = params.to_string();
        if self.blacklist.is_blocked(&param_str) || self.blacklist.is_blocked(action) {
            return PolicyDecision {
                risk_level: RiskLevel::Black,
                action: action.to_string(),
                requires_approval: false,
                blocked: true,
                reason: "matches hardcoded blacklist pattern".into(),
                escalated_from: None,
            };
        }

        // 2. Determine base tier from action name
        let base_tier = if GREEN_ACTIONS.contains(action) {
            RiskLevel::Green
        } else if YELLOW_ACTIONS.contains(action) {
            RiskLevel::Yellow
        } else if RED_ACTIONS.contains(action) {
            RiskLevel::Red
        } else if let Some(mcp_tier) = infer_mcp_gworkspace_risk(action) {
            mcp_tier
        } else {
            // Unknown actions default to RED (fail-safe)
            RiskLevel::Red
        };

        // 3. Path-based escalation: check if any path parameter hits protected paths
        let mut escalated_from = None;
        let effective_tier = if base_tier != RiskLevel::Red {
            if self.touches_protected_path(params) {
                escalated_from = Some(base_tier);
                RiskLevel::Red
            } else {
                base_tier
            }
        } else {
            base_tier
        };

        let (requires_approval, blocked, reason) = match effective_tier {
            RiskLevel::Green => (
                false,
                false,
                "auto-execute: read-only or trivially reversible".into(),
            ),
            RiskLevel::Yellow => (
                false,
                false,
                "execute + notify: modifies user-level state, easily reversible".into(),
            ),
            RiskLevel::Red => (
                true,
                false,
                if escalated_from.is_some() {
                    "escalated to RED: targets protected path".into()
                } else {
                    "requires approval: modifies system state or hard to reverse".into()
                },
            ),
            RiskLevel::Black => (false, true, "always denied: hardcoded safety block".into()),
        };

        PolicyDecision {
            risk_level: effective_tier,
            action: action.to_string(),
            requires_approval,
            blocked,
            reason,
            escalated_from,
        }
    }

    /// Check if any path in params matches protected path patterns.
    fn touches_protected_path(&self, params: &serde_json::Value) -> bool {
        let path_keys = [
            "path",
            "target",
            "destination",
            "file",
            "directory",
            "source",
        ];
        for key in &path_keys {
            if let Some(val) = params.get(key) {
                let path_str = match val {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Array(arr) => {
                        for item in arr {
                            if let Some(s) = item.as_str() {
                                if self.is_protected_path(s) {
                                    return true;
                                }
                            }
                        }
                        continue;
                    }
                    _ => continue,
                };
                if self.is_protected_path(&path_str) {
                    return true;
                }
            }
        }
        // Also check "command" parameter for shell execution tools
        if let Some(cmd) = params.get("command").and_then(|v| v.as_str()) {
            for pat in PROTECTED_PATH_PATTERNS.iter() {
                if pat.is_match(cmd) {
                    return true;
                }
            }
        }
        false
    }

    fn is_protected_path(&self, path: &str) -> bool {
        for pat in PROTECTED_PATH_PATTERNS.iter() {
            if pat.is_match(path) {
                return true;
            }
        }
        false
    }

    /// Classify a typed `Capability` token and return the policy decision.
    ///
    /// This is the primary entry point for the OS-intent dispatcher. Unlike
    /// `evaluate()` which works on string action names, this function has full
    /// access to the structured capability payload and can make precise decisions
    /// without string matching.
    ///
    /// # Security notes
    /// - `file://`, `smb://`, `javascript:` etc. are permanently BLACK here as a
    ///   defense-in-depth layer — `scheme::classify_url` already blocked them
    ///   before `Capability::OpenUrl` was constructed, but we double-check.
    /// - `AxInvoke` is always RED — it requires typed PIN and per-app opt-in.
    /// - `FileWrite` under system roots is BLACK (caught by `SandboxedPath` constructor,
    ///   but we enforce it here too for defense-in-depth).
    pub fn classify_capability(
        &self,
        cap: &Capability,
        registry_schemes: Option<&HashSet<String>>,
    ) -> PolicyDecision {
        match cap {
            // ── OpenUrl ──────────────────────────────────────────────────────
            Capability::OpenUrl { url } => {
                match classify_url(url, registry_schemes) {
                    Err(SchemeError::PermanentlyBlocked(scheme)) => PolicyDecision {
                        risk_level: RiskLevel::Black,
                        action: format!("open_url:{scheme}"),
                        requires_approval: false,
                        blocked: true,
                        reason: format!("scheme '{scheme}' is permanently blocked"),
                        escalated_from: None,
                    },
                    Err(SchemeError::UnknownDeepLink(scheme)) => PolicyDecision {
                        risk_level: RiskLevel::Black,
                        action: format!("open_url:{scheme}"),
                        requires_approval: false,
                        blocked: true,
                        reason: format!(
                            "scheme '{scheme}' is not registered by any installed application"
                        ),
                        escalated_from: None,
                    },
                    Err(e) => PolicyDecision {
                        risk_level: RiskLevel::Black,
                        action: "open_url".to_string(),
                        requires_approval: false,
                        blocked: true,
                        reason: format!("URL classification failed: {e}"),
                        escalated_from: None,
                    },
                    Ok(classification) => {
                        let (requires_approval, reason) = match classification.risk {
                            RiskLevel::Green => (false, "auto-execute: standard safe URI".into()),
                            RiskLevel::Yellow => (
                                false,
                                "execute + preview: deep-link to registered app".into(),
                            ),
                            RiskLevel::Red => (
                                true,
                                "requires approval: unusual URI scheme".into(),
                            ),
                            RiskLevel::Black => unreachable!("classify_url never returns Black Ok"),
                        };
                        PolicyDecision {
                            risk_level: classification.risk,
                            action: "open_url".to_string(),
                            requires_approval,
                            blocked: false,
                            reason,
                            escalated_from: None,
                        }
                    }
                }
            }

            // ── LaunchApp ────────────────────────────────────────────────────
            // SafeArg constructor already rejected metacharacters; this is defense-in-depth.
            Capability::LaunchApp { app_id, .. } => PolicyDecision {
                risk_level: RiskLevel::Green,
                action: format!("launch_app:{}", app_id.as_str()),
                requires_approval: false,
                blocked: false,
                reason: "auto-execute: launching a known installed application".into(),
                escalated_from: None,
            },

            // ── SendMessage ──────────────────────────────────────────────────
            // Always YELLOW — user must confirm who they're messaging and what body was pre-filled.
            // Per user memory: "always preview recipient + message and ask final confirmation".
            Capability::SendMessage { app, contact, .. } => PolicyDecision {
                risk_level: RiskLevel::Yellow,
                action: format!("send_message:{}", app.display_name()),
                requires_approval: false,
                blocked: false,
                reason: format!(
                    "execute + confirm: will open {} draft to '{}' — user must approve",
                    app.display_name(),
                    contact.display_name
                ),
                escalated_from: None,
            },

            // ── FileWrite ────────────────────────────────────────────────────
            // SandboxedPath already blocked system roots; here we label the risk tier.
            Capability::FileWrite { path, .. } => {
                // Defense-in-depth: redundant check on the canonicalized path.
                if self.is_protected_path(&path.as_path().to_string_lossy()) {
                    return PolicyDecision {
                        risk_level: RiskLevel::Black,
                        action: "file_write".to_string(),
                        requires_approval: false,
                        blocked: true,
                        reason: "path is under a permanently protected system root".into(),
                        escalated_from: None,
                    };
                }
                PolicyDecision {
                    risk_level: RiskLevel::Yellow,
                    action: "file_write".to_string(),
                    requires_approval: false,
                    blocked: false,
                    reason: "execute + notify: modifies user file under allowed root".into(),
                    escalated_from: None,
                }
            }

            // ── AxInvoke ─────────────────────────────────────────────────────
            // Accessibility API automation is always RED — typed PIN required.
            Capability::AxInvoke { app_id, .. } => PolicyDecision {
                risk_level: RiskLevel::Red,
                action: format!("ax_invoke:{}", app_id.as_str()),
                requires_approval: true,
                blocked: false,
                reason: "accessibility automation requires explicit PIN confirmation".into(),
                escalated_from: None,
            },
        }
    }

    /// Like `evaluate`, but also accepts an `IntentModality` hint from the routing layer.
    /// If the modality is destructive (Delete, Send, Execute) and the baseline tier is Green,
    /// the decision is escalated to at least Yellow so the safety layer is pre-armed.
    /// This is the intended call-site when routing context is available.
    pub fn evaluate_with_modality_hint(
        &self,
        action: &str,
        params: &serde_json::Value,
        destructive_hint: bool,
    ) -> PolicyDecision {
        let mut decision = self.evaluate(action, params);

        if destructive_hint && decision.risk_level == RiskLevel::Green {
            decision.escalated_from = Some(RiskLevel::Green);
            decision.risk_level = RiskLevel::Yellow;
            decision.reason = "escalated to YELLOW: router flagged destructive modality verb".into();
        }

        decision
    }
}

impl Default for PolicyEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{PolicyEngine, RiskLevel};

    #[test]
    fn relaxed_google_wrapper_send_and_calendar_create_are_yellow() {
        let policy = PolicyEngine::new();

        let send = policy.evaluate("gw_gmail_send", &serde_json::json!({}));
        let create = policy.evaluate("gw_calendar_create", &serde_json::json!({}));

        assert_eq!(send.risk_level, RiskLevel::Yellow);
        assert!(!send.requires_approval);
        assert_eq!(create.risk_level, RiskLevel::Yellow);
        assert!(!create.requires_approval);
    }

    #[test]
    fn mcp_gworkspace_read_actions_default_green() {
        let policy = PolicyEngine::new();
        let decision = policy.evaluate("mcp_gworkspace_readGoogleDoc", &serde_json::json!({}));

        assert_eq!(decision.risk_level, RiskLevel::Green);
        assert!(!decision.requires_approval);
    }

    #[test]
    fn mcp_gworkspace_write_actions_are_yellow() {
        let policy = PolicyEngine::new();
        let decision = policy.evaluate("mcp_gworkspace_sendGmailDraft", &serde_json::json!({}));

        assert_eq!(decision.risk_level, RiskLevel::Yellow);
        assert!(!decision.requires_approval);
    }

    #[test]
    fn mcp_gworkspace_delete_actions_remain_red() {
        let policy = PolicyEngine::new();
        let decision = policy.evaluate("mcp_gworkspace_deleteFile", &serde_json::json!({}));

        assert_eq!(decision.risk_level, RiskLevel::Red);
        assert!(decision.requires_approval);
    }

    // ── classify_capability tests ─────────────────────────────────────────────

    #[test]
    fn capability_https_url_is_green() {
        use crate::platform::intent::capability::Capability;
        use url::Url;

        let policy = PolicyEngine::new();
        let cap = Capability::OpenUrl {
            url: Url::parse("https://google.com/search?q=kittens").unwrap(),
        };
        let decision = policy.classify_capability(&cap, None);
        assert_eq!(decision.risk_level, RiskLevel::Green);
        assert!(!decision.blocked);
    }

    #[test]
    fn capability_file_url_is_black() {
        use crate::platform::intent::capability::Capability;
        use url::Url;

        let policy = PolicyEngine::new();
        let cap = Capability::OpenUrl {
            url: Url::parse("file:///etc/passwd").unwrap(),
        };
        let decision = policy.classify_capability(&cap, None);
        assert_eq!(decision.risk_level, RiskLevel::Black);
        assert!(decision.blocked);
    }

    #[test]
    fn capability_whatsapp_deep_link_is_yellow() {
        use crate::platform::intent::capability::Capability;
        use url::Url;

        let policy = PolicyEngine::new();
        let cap = Capability::OpenUrl {
            url: Url::parse("whatsapp://send?phone=919876543210&text=hye").unwrap(),
        };
        let decision = policy.classify_capability(&cap, None);
        assert_eq!(decision.risk_level, RiskLevel::Yellow);
        assert!(!decision.blocked);
    }

    #[test]
    fn capability_ax_invoke_is_red() {
        use crate::platform::intent::capability::{AxAction, Capability, CanonicalAppId};

        let policy = PolicyEngine::new();
        let cap = Capability::AxInvoke {
            app_id: CanonicalAppId::from_registry("chromium".to_string()),
            action: AxAction::Click {
                element_id: "search-box".to_string(),
            },
        };
        let decision = policy.classify_capability(&cap, None);
        assert_eq!(decision.risk_level, RiskLevel::Red);
        assert!(decision.requires_approval);
        assert!(!decision.blocked);
    }
}
