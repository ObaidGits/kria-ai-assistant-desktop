use std::collections::HashSet;
use once_cell::sync::Lazy;
use regex::Regex;

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
        "open_application", "list_running_apps", "focus_window",
        // System Info (read-only)
        "get_cpu_usage", "get_memory_info", "get_disk_space", "get_network_status",
        "get_battery_status", "get_gpu_info", "get_system_uptime",
        // File Reading
        "read_file", "search_files", "list_directory", "get_file_info", "calculate_dir_size",
        "search_file_contents", "find_files_by_pattern", "get_project_structure",
        "count_lines_of_code", "diff_files", "find_todos", "analyze_code",
        // Document Parsing
        "parse_pdf", "parse_docx", "parse_xlsx", "parse_csv", "summarize_document",
        "parse_document",
        // Internet (read-only)
        "web_search", "fetch_webpage", "get_weather", "get_news", "get_stock_price",
        "check_url_status", "rss_feed_read", "get_public_ip",
        "searxng_search", "duckduckgo_search",
        "get_current_time", "get_exchange_rate", "calculate",
        // Network Diagnostics
        "ping_host", "dns_lookup", "traceroute", "get_active_connections",
        "get_wifi_networks", "speed_test",
        // Clipboard (read)
        "get_clipboard", "clipboard_history", "transform_clipboard",
        // UI
        "screenshot", "lock_screen",
        // Knowledge (read)
        "recall_fact", "list_remembered", "search_knowledge", "get_snippet", "list_snippets",
        // Notifications
        "send_notification", "compose_email", "open_email_draft", "schedule_reminder",
        // Automation (read)
        "list_workflows", "list_scheduled_tasks", "list_macros",
        // Plugins (read)
        "list_plugins",
        // Memory
        "remember_fact", "ingest_document", "save_snippet",
        // Environment (read)
        "get_environment_variable", "list_environment_variables", "get_power_plan",
        // Developer (read-only)
        "git_status", "git_log", "git_diff", "git_branch_list",
        "analyze_project", "diff_files_unified",
        // Database (read-only)
        "query_sqlite", "describe_database",
        // RAG (read)
        "rag_query", "list_knowledge_base",
        // Proactive (read)
        "check_system_health", "get_alerts", "dismiss_alert",
        "list_watched_dirs", "smart_suggest",
        // i18n / Accessibility (read)
        "list_languages", "detect_language", "get_accessibility_settings",
        // Desktop (read)
        "get_active_window", "list_windows",
        // Vision (read-only analysis)
        "ocr_image", "analyze_image", "screenshot_analyze",
        // Precognitive (analysis only)
        "image_analyze", "document_extract", "code_analyze_ast",
        "web_extract_article", "embeddings_generate", "audio_preprocess",
        // Misc
        "open_url", "list_installed_packages",
        // Package queries (read-only)
        "search_package", "check_package_installed", "check_package_updates", "get_package_info",
        "search_news", "fetch_article", "list_news_sources", "news_status",
        // Google Workspace (read-only)
        "gw_gmail_inbox", "gw_gmail_search", "gw_gmail_read",
        "gw_calendar_today", "gw_calendar_search",
        "gw_drive_search", "gw_drive_list", "gw_drive_read",
        "gw_docs_read", "gw_sheets_read", "gw_slides_read",
    ].into_iter().collect()
});

// ─── Tier 1: YELLOW (execute + notify) ───
static YELLOW_ACTIONS: Lazy<HashSet<&str>> = Lazy::new(|| {
    [
        // App Control
        "close_application", "kill_process",
        // System Config
        "set_volume", "set_brightness", "toggle_wifi", "set_power_plan", "connect_wifi",
        // File Modification
        "write_file", "create_directory", "rename_file", "copy_file",
        // Document Conversion
        "convert_document",
        // Internet (write-ish)
        "download_file",
        // Clipboard (write)
        "set_clipboard", "type_text",
        // Package
        "update_application",
        // Power
        "sleep", "hibernate",
        // Plugins
        "enable_plugin", "disable_plugin",
        // Automation
        "run_workflow", "record_macro", "replay_macro",
        // Developer (non-destructive write)
        "git_stash",
        // RAG (ingest)
        "ingest_document_rag",
        // Proactive (monitoring)
        "watch_directory",
        // Desktop (window manipulation)
        "move_window", "resize_window", "maximize_window", "minimize_window", "tile_windows",
        // Google Workspace (create/edit — reversible)
        "gw_docs_create", "gw_docs_edit",
        "gw_sheets_create", "gw_sheets_edit",
        "gw_slides_create",
    ].into_iter().collect()
});

// ─── Tier 2: RED (block until approved) ───
static RED_ACTIONS: Lazy<HashSet<&str>> = Lazy::new(|| {
    [
        // File Destruction
        "delete_file", "delete_directory", "move_file",
        // System Administration
        "manage_service", "set_environment_variable", "add_to_path",
        "edit_shell_profile", "manage_firewall_rule",
        // OS Control
        "shutdown_system", "reboot_system", "clean_temp_files",
        // Package Management
        "install_application", "uninstall_application", "update_all_packages",
        "install_package", "uninstall_package",
        // Code Execution
        "execute_python", "execute_bash", "execute_powershell",
        // Scheduled Tasks
        "create_scheduled_task", "delete_scheduled_task", "modify_scheduled_task",
        // Registry
        "write_registry",
        // Plugins
        "install_plugin", "uninstall_plugin",
        // Dangerous
        "set_process_priority", "change_network_config",
        // Developer (destructive)
        "git_commit", "git_checkout",
        // RAG (destructive)
        "delete_knowledge_item",
        // Google Workspace (send/delete/share — irreversible)
        "gw_gmail_send", "gw_gmail_delete",
        "gw_drive_delete",
        "gw_calendar_create", "gw_calendar_delete",
    ].into_iter().collect()
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
    pub fn evaluate(
        &self,
        action: &str,
        params: &serde_json::Value,
    ) -> PolicyDecision {
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
            RiskLevel::Green => (false, false, "auto-execute: read-only or trivially reversible".into()),
            RiskLevel::Yellow => (false, false, "execute + notify: modifies user-level state, easily reversible".into()),
            RiskLevel::Red => (true, false, if escalated_from.is_some() {
                "escalated to RED: targets protected path".into()
            } else {
                "requires approval: modifies system state or hard to reverse".into()
            }),
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
        let path_keys = ["path", "target", "destination", "file", "directory", "source"];
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
}
