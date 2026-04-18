use chrono::{Datelike, Duration, Local, TimeZone};
use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::agent::response_parser::{
    extract_text_response, parse_tool_calls_with_known, ParsedToolCall,
};
use crate::agent::router::IntentRouter;
use crate::infra::isolation::run_isolated;
use crate::llm::{ChatMessage, ModelRouter, ToolSchema, TOOL_RESULT_MAX_CHARS};
use crate::safety::audit::{DecidedBy, Decision};
use crate::safety::hitl::{ApprovalResponse, HitlGateway};
use crate::safety::{AuditLogger, PolicyEngine, RiskLevel, RollbackManager};
use crate::tools::mount_manager::{google_meet_fallback_metadata, ToolMountManager};
use crate::tools::registry::ToolRegistry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackageIntent {
    Install,
    Uninstall,
}

static REQUESTED_LIMIT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)\b(?:top|last|latest|recent|first|show|get|fetch|read|check)\s+(\d{1,3})\b|\b(\d{1,3})\s+(?:unread|emails?|messages?|results?|files?|folders?|directories?)\b",
    )
    .expect("valid requested limit regex")
});

static QUOTED_TEXT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#""([^"]+)"|'([^']+)'"#).expect("valid quoted text regex"));

static FILE_SEARCH_MARKER_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(?:file|folder|directory)(?:\s+name)?\s+(?:named|called)?\s*([^\n\r,.;!?]+)")
        .expect("valid file search marker regex")
});

static TITLE_MARKER_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(?:called|named|titled|title)\s+([^\n\r,.;!?]+)")
        .expect("valid title marker regex")
});

static CALENDAR_TIME_AMPM_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(\d{1,2})(?::(\d{2}))?\s*(am|pm)\b").expect("valid calendar ampm time regex")
});

static CALENDAR_TIME_24H_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\b([01]?\d|2[0-3]):([0-5]\d)\b").expect("valid calendar 24h time regex")
});

static CALENDAR_DURATION_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\bfor\s+(\d{1,3})\s*(minute|minutes|min|hour|hours|hr|hrs)\b")
        .expect("valid calendar duration regex")
});

fn detect_package_intent(user_text: &str) -> Option<PackageIntent> {
    let text = user_text.to_lowercase();
    if ["uninstall", "remove", "delete package"]
        .iter()
        .any(|m| text.contains(m))
    {
        return Some(PackageIntent::Uninstall);
    }
    if ["install", "setup", "set up"]
        .iter()
        .any(|m| text.contains(m))
    {
        return Some(PackageIntent::Install);
    }
    None
}

fn normalize_package_query(raw: &str) -> String {
    let cleaned = raw.trim().to_lowercase();
    match cleaned.as_str() {
        "chrome"
        | "google chrome"
        | "google-chrome"
        | "google-chrome-stable"
        | "chrome browser"
        | "google chrome browser" => "chromium".into(),
        _ => cleaned,
    }
}

fn extract_after_first_marker<'a>(text: &'a str, markers: &[&str]) -> Option<&'a str> {
    for marker in markers {
        if let Some(idx) = text.find(marker) {
            let start = idx + marker.len();
            return text.get(start..);
        }
    }
    None
}

fn extract_package_query(user_text: &str, intent: PackageIntent) -> Option<String> {
    let lower = user_text.to_lowercase();
    let markers: &[&str] = match intent {
        PackageIntent::Install => &["install ", "setup ", "set up "],
        PackageIntent::Uninstall => &["uninstall ", "remove ", "delete "],
    };

    let mut fragment = extract_after_first_marker(&lower, markers)?
        .split(|c: char| ".,!?;:\n".contains(c))
        .next()
        .unwrap_or("")
        .trim()
        .to_string();

    loop {
        let before = fragment.clone();
        for prefix in [
            "the ",
            "a ",
            "an ",
            "package ",
            "app ",
            "application ",
            "software ",
        ] {
            if fragment.starts_with(prefix) {
                fragment = fragment[prefix.len()..].trim_start().to_string();
            }
        }
        if fragment == before {
            break;
        }
    }

    for suffix in [" please", " now", " for me", " thanks", " thank you"] {
        while fragment.ends_with(suffix) {
            fragment = fragment[..fragment.len() - suffix.len()]
                .trim_end()
                .to_string();
        }
    }

    if fragment.is_empty() {
        return None;
    }

    // Keep the query compact but preserve 2-word app names like "google chrome".
    let mut words = fragment.split_whitespace();
    let first = words.next()?;
    let second = words.next();
    let compact = if matches!(second, Some("chrome")) && first == "google" {
        format!("{first} chrome")
    } else {
        first.to_string()
    };
    Some(normalize_package_query(&compact))
}

fn normalize_package_source_for_action(source: &str) -> Option<String> {
    match source.trim().to_lowercase().as_str() {
        "apt" | "dnf" | "pacman" | "zypper" | "brew" | "winget" | "choco" | "snap" | "flatpak" => {
            Some(source.trim().to_lowercase())
        }
        "brew-formula" | "brew-cask" => Some("brew".into()),
        _ => None,
    }
}

fn infer_news_country_code(text_lower: &str) -> Option<&'static str> {
    if text_lower.contains("india") || text_lower.contains("indian") {
        return Some("IN");
    }
    if text_lower.contains("pakistan") {
        return Some("PK");
    }
    if text_lower.contains("bangladesh") {
        return Some("BD");
    }
    if text_lower.contains("sri lanka") {
        return Some("LK");
    }
    if text_lower.contains("united states")
        || text_lower.contains(" usa")
        || text_lower.contains(" us ")
    {
        return Some("US");
    }
    if text_lower.contains("united kingdom")
        || text_lower.contains(" uk ")
        || text_lower.contains("britain")
    {
        return Some("GB");
    }
    None
}

fn infer_requested_limit(user_text: &str, default: u64, max: u64) -> u64 {
    REQUESTED_LIMIT_RE
        .captures(user_text)
        .and_then(|caps| caps.iter().skip(1).flatten().next())
        .and_then(|m| m.as_str().parse::<u64>().ok())
        .filter(|count| *count > 0)
        .map(|count| count.min(max))
        .unwrap_or(default)
}

fn infer_gmail_list_query(user_text: &str) -> String {
    let text_lower = user_text.to_lowercase();
    let mut filters: Vec<&str> = Vec::new();

    if text_lower.contains("sent") {
        filters.push("in:sent");
    } else if text_lower.contains("draft") {
        filters.push("in:drafts");
    } else if text_lower.contains("spam") {
        filters.push("in:spam");
    } else if text_lower.contains("trash") {
        filters.push("in:trash");
    } else {
        filters.push("in:inbox");
    }

    if text_lower.contains("unread") {
        filters.push("is:unread");
    }
    if text_lower.contains("starred") {
        filters.push("is:starred");
    }
    if text_lower.contains("important") {
        filters.push("is:important");
    }

    filters.join(" ")
}

fn has_gmail_list_signal(text_lower: &str) -> bool {
    [
        "unread",
        "starred",
        "important",
        "sent",
        "draft",
        "spam",
        "trash",
    ]
    .iter()
    .any(|needle| text_lower.contains(needle))
}

fn infer_gmail_search_query(user_text: &str) -> String {
    let lower = user_text.to_lowercase();
    for marker in [
        "search gmail for",
        "search my gmail for",
        "find in gmail",
        "find gmail for",
        "search email for",
        "search emails for",
    ] {
        if let Some((_, rest)) = lower.split_once(marker) {
            let query = rest.trim();
            if !query.is_empty() {
                return query.to_string();
            }
        }
    }

    if has_gmail_list_signal(&lower) {
        return infer_gmail_list_query(user_text);
    }

    user_text.trim().to_string()
}

fn infer_file_search_kind(text_lower: &str) -> &'static str {
    if text_lower.contains("folder") || text_lower.contains("directory") {
        "dir"
    } else if text_lower.contains("file") {
        "file"
    } else {
        "any"
    }
}

fn infer_file_search_root(text_lower: &str) -> String {
    if [
        "this project",
        "this repo",
        "current project",
        "current directory",
        "here",
    ]
    .iter()
    .any(|needle| text_lower.contains(needle))
    {
        return ".".into();
    }

    std::env::var("HOME").unwrap_or_else(|_| "/home".into())
}

fn infer_title(user_text: &str, default_title: &str) -> String {
    let trimmed = user_text.trim();
    if trimmed.is_empty() {
        return default_title.to_string();
    }

    if let Some(caps) = QUOTED_TEXT_RE.captures(trimmed) {
        if let Some(matched) = caps.get(1).or_else(|| caps.get(2)) {
            let title = matched.as_str().trim();
            if !title.is_empty() {
                return title.to_string();
            }
        }
    }

    if let Some(caps) = TITLE_MARKER_RE.captures(trimmed) {
        if let Some(matched) = caps.get(1) {
            let title = matched
                .as_str()
                .trim()
                .trim_matches(|c: char| c == '"' || c == '\'' || c == '`')
                .trim();
            if !title.is_empty() {
                return title.to_string();
            }
        }
    }

    default_title.to_string()
}

fn infer_calendar_time(text_lower: &str) -> Option<(u32, u32)> {
    if let Some(caps) = CALENDAR_TIME_AMPM_RE.captures(text_lower) {
        let hour_raw = caps.get(1)?.as_str().parse::<u32>().ok()?;
        let minute = caps
            .get(2)
            .and_then(|m| m.as_str().parse::<u32>().ok())
            .unwrap_or(0);
        let ampm = caps.get(3)?.as_str().to_ascii_lowercase();

        let mut hour = hour_raw.min(12);
        if ampm == "am" {
            if hour == 12 {
                hour = 0;
            }
        } else if hour != 12 {
            hour += 12;
        }
        return Some((hour.min(23), minute.min(59)));
    }

    if let Some(caps) = CALENDAR_TIME_24H_RE.captures(text_lower) {
        let hour = caps.get(1)?.as_str().parse::<u32>().ok()?.min(23);
        let minute = caps.get(2)?.as_str().parse::<u32>().ok()?.min(59);
        return Some((hour, minute));
    }

    None
}

fn looks_like_google_workspace_request(text_lower: &str) -> bool {
    [
        "google workspace",
        "gmail",
        "gmails",
        "inbox",
        "calendar",
        "google meet",
        "gmeet",
        "meet link",
        "google drive",
        "drive",
        "google doc",
        "google docs",
        "document",
        "google sheet",
        "google sheets",
        "spreadsheet",
        "google slides",
        "slides",
        "presentation",
        "google forms",
        "google form",
        "forms",
    ]
    .iter()
    .any(|needle| text_lower.contains(needle))
}

fn infer_calendar_duration_minutes(text_lower: &str) -> i64 {
    if text_lower.contains("half hour") {
        return 30;
    }

    if let Some(caps) = CALENDAR_DURATION_RE.captures(text_lower) {
        if let Some(value) = caps.get(1).and_then(|m| m.as_str().parse::<i64>().ok()) {
            let unit = caps
                .get(2)
                .map(|m| m.as_str().to_ascii_lowercase())
                .unwrap_or_default();
            if unit.starts_with('h') {
                return (value * 60).clamp(15, 8 * 60);
            }
            return value.clamp(15, 8 * 60);
        }
    }

    60
}

fn infer_calendar_window(user_text: &str) -> Option<(String, String)> {
    let lower = user_text.to_lowercase();
    let day_offset = if lower.contains("day after tomorrow") {
        2
    } else if lower.contains("tomorrow") {
        1
    } else if lower.contains("today") {
        0
    } else if lower.contains("next week") {
        7
    } else {
        return None;
    };

    let base_date = Local::now().date_naive() + Duration::days(day_offset);
    let (hour, minute) = infer_calendar_time(&lower).unwrap_or((9, 0));

    let start = Local
        .with_ymd_and_hms(
            base_date.year(),
            base_date.month(),
            base_date.day(),
            hour,
            minute,
            0,
        )
        .single()?;
    let end = start + Duration::minutes(infer_calendar_duration_minutes(&lower));

    Some((start.to_rfc3339(), end.to_rfc3339()))
}

fn infer_calendar_summary(user_text: &str) -> String {
    let explicit = infer_title(user_text, "");
    if !explicit.is_empty() {
        return explicit;
    }

    let lower = user_text.to_lowercase();
    if lower.contains("google meet") || lower.contains("gmeet") || lower.contains("meet") {
        return "Google Meet".into();
    }
    if lower.contains("interview") {
        return "Interview".into();
    }
    if lower.contains("appointment") {
        return "Appointment".into();
    }
    if lower.contains("call") {
        return "Call".into();
    }
    if lower.contains("meeting") {
        return "Meeting".into();
    }

    "New Event".into()
}

fn infer_calendar_create_arguments(user_text: &str) -> Option<serde_json::Value> {
    let (start, end) = infer_calendar_window(user_text)?;
    let lower = user_text.to_lowercase();

    Some(serde_json::json!({
        "summary": infer_calendar_summary(user_text),
        "start": start,
        "end": end,
        "description": if lower.contains("google meet") || lower.contains("gmeet") || lower.contains("meet link") {
            "Requested via KRIA (Google Meet)"
        } else {
            ""
        },
        "location": "",
    }))
}

fn infer_file_search_target(user_text: &str) -> Option<String> {
    let trimmed = user_text.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(caps) = QUOTED_TEXT_RE.captures(trimmed) {
        if let Some(matched) = caps.get(1).or_else(|| caps.get(2)) {
            let target = matched.as_str().trim();
            if !target.is_empty() {
                return Some(target.to_string());
            }
        }
    }

    if let Some(caps) = FILE_SEARCH_MARKER_RE.captures(trimmed) {
        if let Some(matched) = caps.get(1) {
            let target = matched
                .as_str()
                .trim()
                .trim_matches(|c: char| c == '"' || c == '\'' || c == '`')
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim();
            if !target.is_empty() {
                return Some(target.to_string());
            }
        }
    }

    None
}

fn build_intent_fallback_tool_call(
    user_text: &str,
    allowed_tool_names: &HashSet<String>,
) -> Option<ParsedToolCall> {
    let intent = IntentRouter::classify(user_text);
    let hint = intent.tool_hint?;
    let user_query = user_text.trim();
    if user_query.is_empty() {
        return None;
    }

    let lower = user_query.to_lowercase();

    match hint.as_str() {
        "gw_gmail_inbox" if allowed_tool_names.contains("gw_gmail_inbox") => {
            let args = serde_json::json!({
                "query": infer_gmail_list_query(user_query),
                "max_results": infer_requested_limit(user_query, 10, 50),
            });

            Some(ParsedToolCall {
                name: "gw_gmail_inbox".into(),
                arguments: args,
            })
        }
        "gw_gmail_search" if allowed_tool_names.contains("gw_gmail_search") => {
            let query = infer_gmail_search_query(user_query);

            Some(ParsedToolCall {
                name: "gw_gmail_search".into(),
                arguments: serde_json::json!({
                    "query": query,
                    "max_results": infer_requested_limit(user_query, 10, 50),
                }),
            })
        }
        "gw_calendar_today" if allowed_tool_names.contains("gw_calendar_today") => {
            Some(ParsedToolCall {
                name: "gw_calendar_today".into(),
                arguments: serde_json::json!({}),
            })
        }
        "gw_calendar_search" if allowed_tool_names.contains("gw_calendar_search") => {
            Some(ParsedToolCall {
                name: "gw_calendar_search".into(),
                arguments: serde_json::json!({
                    "query": user_query,
                }),
            })
        }
        "gw_calendar_create" if allowed_tool_names.contains("gw_calendar_create") => {
            if let Some(args) = infer_calendar_create_arguments(user_query) {
                Some(ParsedToolCall {
                    name: "gw_calendar_create".into(),
                    arguments: args,
                })
            } else if allowed_tool_names.contains("gw_calendar_search") {
                Some(ParsedToolCall {
                    name: "gw_calendar_search".into(),
                    arguments: serde_json::json!({
                        "query": user_query,
                    }),
                })
            } else {
                None
            }
        }
        "gw_drive_search" if allowed_tool_names.contains("gw_drive_search") => {
            Some(ParsedToolCall {
                name: "gw_drive_search".into(),
                arguments: serde_json::json!({
                    "query": user_query,
                }),
            })
        }
        "gw_docs_create" if allowed_tool_names.contains("gw_docs_create") => Some(ParsedToolCall {
            name: "gw_docs_create".into(),
            arguments: serde_json::json!({
                "title": infer_title(user_query, "Untitled Document"),
            }),
        }),
        "gw_sheets_create" if allowed_tool_names.contains("gw_sheets_create") => {
            Some(ParsedToolCall {
                name: "gw_sheets_create".into(),
                arguments: serde_json::json!({
                    "title": infer_title(user_query, "Untitled Spreadsheet"),
                }),
            })
        }
        "gw_slides_create" if allowed_tool_names.contains("gw_slides_create") => {
            Some(ParsedToolCall {
                name: "gw_slides_create".into(),
                arguments: serde_json::json!({
                    "title": infer_title(user_query, "Untitled Presentation"),
                }),
            })
        }
        "mcp_gworkspace_listForms" if allowed_tool_names.contains("mcp_gworkspace_listForms") => {
            Some(ParsedToolCall {
                name: "mcp_gworkspace_listForms".into(),
                arguments: serde_json::json!({}),
            })
        }
        "mcp_gworkspace_createForm" if allowed_tool_names.contains("mcp_gworkspace_createForm") => {
            Some(ParsedToolCall {
                name: "mcp_gworkspace_createForm".into(),
                arguments: serde_json::json!({
                    "title": infer_title(user_query, "Untitled Form"),
                }),
            })
        }
        "search_files" if allowed_tool_names.contains("mcp_fs_search_files") => {
            let target = infer_file_search_target(user_query)?;
            let root = infer_file_search_root(&lower);
            let pattern = format!("**/{target}");

            Some(ParsedToolCall {
                name: "mcp_fs_search_files".into(),
                arguments: serde_json::json!({
                    "path": root,
                    "pattern": pattern,
                }),
            })
        }
        "search_files" if allowed_tool_names.contains("find_files_by_pattern") => {
            let target = infer_file_search_target(user_query)?;
            let root = infer_file_search_root(&lower);

            Some(ParsedToolCall {
                name: "find_files_by_pattern".into(),
                arguments: serde_json::json!({
                    "directory": root,
                    "pattern": target,
                    "type": infer_file_search_kind(&lower),
                    "max_results": infer_requested_limit(user_query, 20, 100),
                }),
            })
        }
        "search_files" if allowed_tool_names.contains("search_files") => {
            let target = infer_file_search_target(user_query)?;
            let root = infer_file_search_root(&lower);

            Some(ParsedToolCall {
                name: "search_files".into(),
                arguments: serde_json::json!({
                    "directory": root,
                    "pattern": target,
                    "max_results": infer_requested_limit(user_query, 20, 100),
                }),
            })
        }
        "search_news" if allowed_tool_names.contains("search_news") => {
            let mut args = serde_json::json!({
                "query": user_query,
                "limit": 8,
            });

            if ["latest", "breaking", "today", "current", "recent", "live"]
                .iter()
                .any(|k| lower.contains(k))
            {
                args["freshness_mode"] = serde_json::json!("live");
            }

            if ["trusted", "authentic", "reliable", "verified"]
                .iter()
                .any(|k| lower.contains(k))
            {
                args["source_profile"] = serde_json::json!("authentic");
            }

            if let Some(country) = infer_news_country_code(&lower) {
                args["country"] = serde_json::json!(country);
            }

            if lower.contains("iran") || lower.contains("israel") || lower.contains("middle east") {
                args["region"] = serde_json::json!("middle-east");
            }

            Some(ParsedToolCall {
                name: "search_news".into(),
                arguments: args,
            })
        }
        "web_search" if allowed_tool_names.contains("web_search") => Some(ParsedToolCall {
            name: "web_search".into(),
            arguments: serde_json::json!({
                "query": user_query,
                "max_results": 8,
            }),
        }),
        "web_search" if allowed_tool_names.contains("searxng_search") => Some(ParsedToolCall {
            name: "searxng_search".into(),
            arguments: serde_json::json!({
                "query": user_query,
                "max_results": 8,
            }),
        }),
        _ => None,
    }
}

#[derive(Debug, Clone)]
struct PackageFlowState {
    intent: PackageIntent,
    query: String,
    package_name: String,
    search_done: bool,
    search_found: Option<bool>,
    search_preferred_source: Option<String>,
    precheck_done: bool,
    precheck_installed: Option<bool>,
    precheck_source: Option<String>,
    action_attempted: bool,
    action_success: Option<bool>,
    postcheck_done: bool,
    postcheck_installed: Option<bool>,
}

impl PackageFlowState {
    fn from_user_text(user_text: &str) -> Option<Self> {
        let intent = detect_package_intent(user_text)?;
        let query = extract_package_query(user_text, intent)?;
        let package_name = query.split_whitespace().next()?.to_string();
        Some(Self {
            intent,
            query,
            package_name,
            search_done: false,
            search_found: None,
            search_preferred_source: None,
            precheck_done: false,
            precheck_installed: None,
            precheck_source: None,
            action_attempted: false,
            action_success: None,
            postcheck_done: false,
            postcheck_installed: None,
        })
    }

    fn action_tool_name(&self) -> &'static str {
        match self.intent {
            PackageIntent::Install => "install_package",
            PackageIntent::Uninstall => "uninstall_package",
        }
    }

    fn check_call(&self) -> ParsedToolCall {
        ParsedToolCall {
            name: "check_package_installed".into(),
            arguments: serde_json::json!({ "name": self.package_name }),
        }
    }

    fn action_call(&self) -> ParsedToolCall {
        let mut arguments = serde_json::json!({ "name": self.package_name });
        if let Some(source) = self.source_for_action() {
            arguments["source"] = serde_json::Value::String(source);
        }
        ParsedToolCall {
            name: self.action_tool_name().into(),
            arguments,
        }
    }

    fn search_call(&self) -> ParsedToolCall {
        ParsedToolCall {
            name: "search_package".into(),
            arguments: serde_json::json!({ "query": self.query }),
        }
    }

    fn should_take_action(&self) -> Option<bool> {
        match self.intent {
            PackageIntent::Install => self.precheck_installed.map(|installed| !installed),
            PackageIntent::Uninstall => self.precheck_installed,
        }
    }

    fn source_for_action(&self) -> Option<String> {
        match self.intent {
            PackageIntent::Install => self
                .search_preferred_source
                .clone()
                .or_else(|| self.precheck_source.clone()),
            PackageIntent::Uninstall => self.precheck_source.clone(),
        }
    }

    fn next_required_calls(&self) -> Vec<ParsedToolCall> {
        if matches!(self.intent, PackageIntent::Install) {
            if !self.search_done {
                return vec![self.search_call()];
            }
            // If the package was not found during search, stop forcing actions.
            if matches!(self.search_found, Some(false)) {
                return vec![];
            }
            // If search failed and we have no reliable result, avoid loops.
            if self.search_found.is_none() {
                return vec![];
            }
        }

        if !self.precheck_done {
            return vec![self.check_call()];
        }
        // If precheck failed and we have no reliable installed flag, avoid loops.
        if self.precheck_installed.is_none() {
            return vec![];
        }

        match self.intent {
            PackageIntent::Install => {
                if matches!(self.should_take_action(), Some(true)) {
                    if !self.action_attempted {
                        return vec![self.action_call()];
                    }
                    // Always re-check after an install attempt.
                    if !self.postcheck_done {
                        return vec![self.check_call()];
                    }
                }
            }
            PackageIntent::Uninstall => {
                if matches!(self.precheck_installed, Some(false)) {
                    return vec![];
                }
                if !self.action_attempted {
                    return vec![self.action_call()];
                }
                // Always re-check after each uninstall attempt.
                if !self.postcheck_done {
                    return vec![self.check_call()];
                }
                // If still installed, try uninstalling again using the latest observed source.
                if matches!(self.postcheck_installed, Some(true)) {
                    return vec![self.action_call()];
                }
            }
        }

        vec![]
    }

    fn observe_tool_result(
        &mut self,
        call: &ParsedToolCall,
        success: bool,
        data: &serde_json::Value,
    ) {
        match call.name.as_str() {
            "search_package" => {
                self.search_done = true;
                self.search_found = data
                    .get("count")
                    .and_then(|v| v.as_u64())
                    .map(|count| count > 0);
                self.search_preferred_source = data
                    .get("results")
                    .and_then(|v| v.as_array())
                    .and_then(|results| {
                        let target = self.package_name.to_lowercase();
                        results
                            .iter()
                            .find(|row| {
                                row.get("name")
                                    .and_then(|v| v.as_str())
                                    .map(|name| {
                                        let n = name.to_lowercase();
                                        n == target
                                            || n.starts_with(&(target.clone() + "-"))
                                            || n.contains(&target)
                                    })
                                    .unwrap_or(false)
                            })
                            .or_else(|| results.first())
                    })
                    .and_then(|row| row.get("source"))
                    .and_then(|v| v.as_str())
                    .and_then(normalize_package_source_for_action);
            }
            "check_package_installed" => {
                let installed = data.get("installed").and_then(|v| v.as_bool());
                let source = data
                    .get("source")
                    .and_then(|v| v.as_str())
                    .and_then(normalize_package_source_for_action);
                if !self.precheck_done {
                    self.precheck_done = true;
                    self.precheck_installed = installed;
                    self.precheck_source = source;
                } else if self.action_attempted {
                    self.postcheck_done = true;
                    self.postcheck_installed = installed;
                    self.precheck_source = source.or_else(|| self.precheck_source.clone());
                } else {
                    // A repeated pre-check still refreshes observed state.
                    self.precheck_installed = installed;
                    self.precheck_source = source.or_else(|| self.precheck_source.clone());
                }
            }
            "install_package" if matches!(self.intent, PackageIntent::Install) => {
                self.action_attempted = true;
                self.action_success = Some(success);
                self.postcheck_done = false;
                self.postcheck_installed = None;
            }
            "uninstall_package" if matches!(self.intent, PackageIntent::Uninstall) => {
                self.action_attempted = true;
                self.action_success = Some(success);
                self.postcheck_done = false;
                self.postcheck_installed = None;
            }
            _ => {}
        }
    }

    fn verified_summary(&self) -> Option<String> {
        match self.intent {
            PackageIntent::Install => {
                if matches!(self.precheck_installed, Some(true)) {
                    return Some(format!(
                        "Verified: '{}' is already installed.",
                        self.package_name
                    ));
                }
                if !self.action_attempted || !self.postcheck_done {
                    return None;
                }
                match self.postcheck_installed {
                    Some(true) => Some(format!(
                        "Verified: '{}' is installed after the install attempt.",
                        self.package_name
                    )),
                    Some(false) => Some(format!(
                        "Verification result: '{}' is still not installed after the install attempt.",
                        self.package_name
                    )),
                    None => Some(format!(
                        "Install attempt completed for '{}', but final verification could not determine installed state.",
                        self.package_name
                    )),
                }
            }
            PackageIntent::Uninstall => {
                if matches!(self.precheck_installed, Some(false)) {
                    return Some(format!(
                        "Verified: '{}' is not installed.",
                        self.package_name
                    ));
                }
                if !self.action_attempted || !self.postcheck_done {
                    return None;
                }
                match self.postcheck_installed {
                    Some(false) => Some(format!(
                        "Verified: '{}' is not installed after the uninstall attempt.",
                        self.package_name
                    )),
                    Some(true) => Some(format!(
                        "Verification result: '{}' is still installed after the uninstall attempt.",
                        self.package_name
                    )),
                    None => Some(format!(
                        "Uninstall attempt completed for '{}', but final verification could not determine installed state.",
                        self.package_name
                    )),
                }
            }
        }
    }
}

/// Events emitted during agent loop execution.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// Text token from the LLM.
    Token(String),
    /// Tool is being called.
    ToolStart {
        name: String,
        params: serde_json::Value,
    },
    /// Tool completed.
    ToolEnd {
        name: String,
        result: serde_json::Value,
        success: bool,
    },
    /// Waiting for HITL approval.
    ApprovalRequired {
        request_id: String,
        action: String,
        risk_level: String,
        parameters: serde_json::Value,
    },
    /// Approval result.
    ApprovalResult { action: String, approved: bool },
    /// Planning step.
    Plan(String),
    /// Error.
    Error(String),
    /// Final response text.
    Done(String),
}

/// The core ReAct agent loop.
pub struct AgentLoop {
    model_router: Arc<ModelRouter>,
    tool_registry: Arc<ToolRegistry>,
    mount_manager: Arc<tokio::sync::RwLock<ToolMountManager>>,
    policy_engine: Arc<PolicyEngine>,
    hitl_gateway: Arc<HitlGateway>,
    audit_logger: Arc<AuditLogger>,
    #[allow(dead_code)]
    rollback_mgr: Arc<RollbackManager>,
    max_tool_rounds: usize,
    hardware_tier: String,
}

impl AgentLoop {
    pub fn new(
        model_router: Arc<ModelRouter>,
        tool_registry: Arc<ToolRegistry>,
        mount_manager: Arc<tokio::sync::RwLock<ToolMountManager>>,
        policy_engine: Arc<PolicyEngine>,
        hitl_gateway: Arc<HitlGateway>,
        audit_logger: Arc<AuditLogger>,
        rollback_mgr: Arc<RollbackManager>,
    ) -> Self {
        Self {
            model_router,
            tool_registry,
            mount_manager,
            policy_engine,
            hitl_gateway,
            audit_logger,
            rollback_mgr,
            max_tool_rounds: 10,
            hardware_tier: "standard".into(),
        }
    }

    /// Override the maximum tool rounds for a single user turn.
    pub fn with_max_tool_rounds(mut self, max_tool_rounds: usize) -> Self {
        if max_tool_rounds > 0 {
            self.max_tool_rounds = max_tool_rounds;
        }
        self
    }

    /// Set the hardware tier used for tool visibility and execution gating.
    pub fn with_hardware_tier(mut self, hardware_tier: impl Into<String>) -> Self {
        let tier = hardware_tier.into();
        if !tier.trim().is_empty() {
            self.hardware_tier = tier;
        }
        self
    }

    /// Run the agent loop for a single user turn.
    /// Returns a channel of StreamEvents.
    pub async fn run(
        &self,
        session_id: &str,
        messages: &mut Vec<ChatMessage>,
        event_tx: mpsc::UnboundedSender<StreamEvent>,
    ) {
        // Check if the user message contains images and route accordingly
        let has_images = messages.last().map_or(false, |m| m.has_images());

        let backend = if has_images {
            match self.model_router.route_vision().await {
                Some(b) => b,
                None => {
                    let _ = event_tx.send(StreamEvent::Error("no vision backend available".into()));
                    return;
                }
            }
        } else {
            match self.model_router.route("chat").await {
                Some(b) => b,
                None => {
                    let _ = event_tx.send(StreamEvent::Error("no LLM backend available".into()));
                    return;
                }
            }
        };

        // Auto-mount tool groups based on user message keywords
        let mut meet_fallback_metadata: Option<serde_json::Value> = None;
        if let Some(last_msg) = messages.last() {
            if last_msg.role == "user" {
                meet_fallback_metadata = google_meet_fallback_metadata(&last_msg.content);
                let mut mm = self.mount_manager.write().await;
                let newly = mm.auto_mount_from_message(&last_msg.content);
                if !newly.is_empty() {
                    tracing::info!(groups = ?newly, "auto-mounted tool groups from user message");
                }
            }
        }

        if let Some(metadata) = meet_fallback_metadata {
            let metadata_json =
                serde_json::to_string_pretty(&metadata).unwrap_or_else(|_| metadata.to_string());
            messages.push(ChatMessage {
                role: "system".into(),
                content: format!(
                    "Google Meet fallback metadata:\n{}\nTool selection rule: when the user requests Google Meet/video-call scheduling, use Calendar conference-link mode with gw_calendar_create (and gw_calendar_search for availability checks).",
                    metadata_json
                ),
                name: None,
                images: None,
            });

            let _ = event_tx.send(StreamEvent::Plan(
                "Applying Google Meet fallback via Calendar conference-link mode metadata".into(),
            ));
        }

        let last_user_text = messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| m.content.clone())
            .unwrap_or_default();
        let google_workspace_intent =
            looks_like_google_workspace_request(&last_user_text.to_lowercase());

        // Build tool schemas for the LLM (filtered by mount manager)
        let mount_mgr = self.mount_manager.read().await;
        let tool_defs = self.tool_registry.list_for_tier(&self.hardware_tier);
        let tool_schemas: Vec<ToolSchema> = tool_defs
            .iter()
            .filter(|d| mount_mgr.is_mounted(&d.name))
            .filter(|d| {
                if d.name.starts_with("mcp_gworkspace_") {
                    google_workspace_intent
                } else {
                    true
                }
            })
            .map(|d| ToolSchema {
                name: d.name.clone(),
                description: d.description.clone(),
                parameters: d.to_function_schema()["function"]["parameters"].clone(),
            })
            .collect();
        let allowed_tool_names: HashSet<String> =
            tool_schemas.iter().map(|s| s.name.clone()).collect();
        drop(mount_mgr);

        // Track tools already approved in this user-turn to avoid re-asking.
        // Key: "tool_name|args_json"
        let mut approved_this_turn: HashSet<String> = HashSet::new();
        let mut package_flow = PackageFlowState::from_user_text(&last_user_text);
        let mut intent_fallback_used = false;

        for _round in 0..self.max_tool_rounds {
            // Call LLM
            let response = match backend.chat(messages, Some(&tool_schemas), 0.7, 4096).await {
                Ok(r) => r,
                Err(e) => {
                    let _ = event_tx.send(StreamEvent::Error(format!("LLM error: {e}")));
                    return;
                }
            };

            // Parse tool calls from response — prefer native function-calling format
            // (returned by llama.cpp / OpenAI), fall back to text-embedded format.
            // Pattern 7 (Python-style fallback) fires last, only for single-required-param tools.
            let mut tool_calls: Vec<ParsedToolCall> = if let Some(native) = &response.tool_calls {
                native
                    .iter()
                    .filter_map(|tc| {
                        let name = tc["function"]["name"].as_str()?.to_string();
                        let arguments: serde_json::Value = tc["function"]["arguments"]
                            .as_str()
                            .and_then(|s| serde_json::from_str(s).ok())
                            .unwrap_or_else(|| tc["function"]["arguments"].clone());
                        Some(ParsedToolCall { name, arguments })
                    })
                    .collect()
            } else {
                // Build the single-required-param lookup for Pattern 7
                let single_param_tools: Vec<(String, String)> = self
                    .tool_registry
                    .list_defs()
                    .into_iter()
                    .filter_map(|d| {
                        let required: Vec<_> = d.parameters.iter().filter(|p| p.required).collect();
                        if required.len() == 1 {
                            Some((d.name.clone(), required[0].name.clone()))
                        } else {
                            None
                        }
                    })
                    .collect();
                let known: Vec<(&str, &str)> = single_param_tools
                    .iter()
                    .map(|(n, p)| (n.as_str(), p.as_str()))
                    .collect();
                parse_tool_calls_with_known(&response.content, &known)
            };
            let text_response = extract_text_response(&response.content);

            let mut synthetic_package_calls = false;
            let mut synthetic_intent_calls = false;
            if tool_calls.is_empty() {
                if let Some(flow) = package_flow.as_ref() {
                    let fallback_calls = flow.next_required_calls();
                    if !fallback_calls.is_empty() {
                        synthetic_package_calls = true;
                        tool_calls = fallback_calls;
                        let _ = event_tx.send(StreamEvent::Plan(
                            "Enforcing package workflow with pre/post verification".into(),
                        ));
                    }
                }
            }

            if tool_calls.is_empty() && !intent_fallback_used {
                if let Some(fallback_call) =
                    build_intent_fallback_tool_call(&last_user_text, &allowed_tool_names)
                {
                    intent_fallback_used = true;
                    synthetic_intent_calls = true;
                    let _ = event_tx.send(StreamEvent::Plan(format!(
                        "No tool call returned; applying intent fallback via {}",
                        fallback_call.name
                    )));
                    tool_calls = vec![fallback_call];
                }
            }

            // If no tool calls, we're done
            if tool_calls.is_empty() {
                if let Some(flow) = package_flow.as_ref() {
                    if let Some(summary) = flow.verified_summary() {
                        let _ = event_tx.send(StreamEvent::Token(summary.clone()));
                        let _ = event_tx.send(StreamEvent::Done(summary));
                        return;
                    }
                }
                if !text_response.is_empty() {
                    let _ = event_tx.send(StreamEvent::Token(text_response.clone()));
                    let _ = event_tx.send(StreamEvent::Done(text_response));
                } else {
                    let fallback =
                        "I could not generate a response for this request. Please try again."
                            .to_string();
                    tracing::warn!(
                        has_images,
                        round = _round,
                        "LLM returned empty response with no tool calls"
                    );
                    let _ = event_tx.send(StreamEvent::Token(fallback.clone()));
                    let _ = event_tx.send(StreamEvent::Done(fallback));
                }
                return;
            }

            if !synthetic_package_calls && !synthetic_intent_calls && !text_response.is_empty() {
                let _ = event_tx.send(StreamEvent::Token(text_response.clone()));
            }

            // Add assistant message to history
            if !synthetic_package_calls && !synthetic_intent_calls {
                messages.push(ChatMessage {
                    role: "assistant".into(),
                    content: response.content.clone(),
                    name: None,
                    images: None,
                });
            }

            // Execute each tool call
            for call in &tool_calls {
                // Never execute tools outside the current mounted+tier visible set.
                if !allowed_tool_names.contains(&call.name) {
                    let unavailable_msg = format!(
                        "tool '{}' is not available for current hardware tier '{}' or mounted tool groups",
                        call.name, self.hardware_tier
                    );
                    let _ = event_tx.send(StreamEvent::ToolEnd {
                        name: call.name.clone(),
                        result: serde_json::json!({ "error": unavailable_msg }),
                        success: false,
                    });
                    if let Some(flow) = package_flow.as_mut() {
                        flow.observe_tool_result(call, false, &serde_json::Value::Null);
                    }
                    messages.push(ChatMessage {
                        role: "tool".into(),
                        content: format!(
                            "TOOL_ERROR: '{}' is not available in this context (tier/mount gating).",
                            call.name
                        ),
                        name: Some(call.name.clone()),
                        images: None,
                    });
                    continue;
                }

                let _ = event_tx.send(StreamEvent::ToolStart {
                    name: call.name.clone(),
                    params: call.arguments.clone(),
                });

                // Policy check
                let decision = self.policy_engine.evaluate(&call.name, &call.arguments);

                if decision.blocked {
                    // BLACK tier — always denied
                    self.audit_logger.log(
                        session_id,
                        &call.name,
                        &call.arguments,
                        RiskLevel::Black,
                        Decision::Blocked,
                        DecidedBy::Hardcoded,
                    );
                    let _ = event_tx.send(StreamEvent::ToolEnd {
                        name: call.name.clone(),
                        result: serde_json::json!({ "error": "blocked by safety policy" }),
                        success: false,
                    });
                    messages.push(ChatMessage {
                        role: "tool".into(),
                        content: format!(
                            "Tool '{}' blocked by safety policy: {}",
                            call.name, decision.reason
                        ),
                        name: Some(call.name.clone()),
                        images: None,
                    });
                    continue;
                }

                if decision.requires_approval {
                    // RED tier — needs HITL approval (but skip if same tool+args already approved this turn)
                    let dedup_key = format!("{}|{}", call.name, call.arguments);
                    let already_approved = approved_this_turn.contains(&dedup_key);

                    if already_approved {
                        // Already approved earlier in this turn — auto-proceed, log it
                        self.audit_logger.log(
                            session_id,
                            &call.name,
                            &call.arguments,
                            decision.risk_level,
                            Decision::Approved,
                            DecidedBy::Policy,
                        );
                    } else {
                        // Generate the request ID up front so the frontend receives the
                        // same ID that the HITL gateway stores in its pending map.
                        let request_id = HitlGateway::generate_request_id();

                        let _ = event_tx.send(StreamEvent::ApprovalRequired {
                            request_id: request_id.clone(),
                            action: call.name.clone(),
                            risk_level: decision.risk_level.as_str().into(),
                            parameters: call.arguments.clone(),
                        });

                        let approval = self
                            .hitl_gateway
                            .request_approval_with_id(
                                &request_id,
                                &call.name,
                                call.arguments.clone(),
                                decision.risk_level,
                                &format!("Execute {} with params: {}", call.name, call.arguments),
                                true,
                            )
                            .await;

                        let (audit_decision, decided_by, approved, denial_reason) = match approval {
                            ApprovalResponse::Approved => {
                                (Decision::Approved, DecidedBy::UserGui, true, "")
                            }
                            ApprovalResponse::Denied => (
                                Decision::Denied,
                                DecidedBy::UserGui,
                                false,
                                "denied by user",
                            ),
                            ApprovalResponse::Timeout => (
                                Decision::Timeout,
                                DecidedBy::Timeout,
                                false,
                                "approval timed out — user did not respond",
                            ),
                        };

                        self.audit_logger.log(
                            session_id,
                            &call.name,
                            &call.arguments,
                            decision.risk_level,
                            audit_decision,
                            decided_by,
                        );

                        let _ = event_tx.send(StreamEvent::ApprovalResult {
                            action: call.name.clone(),
                            approved,
                        });

                        if !approved {
                            // Emit ToolEnd so the UI shows the tool as failed (not just pending).
                            let _ = event_tx.send(StreamEvent::ToolEnd {
                                name: call.name.clone(),
                                result: serde_json::json!({ "error": denial_reason }),
                                success: false,
                            });
                            messages.push(ChatMessage {
                                role: "tool".into(),
                                content: format!(
                                    "TOOL_ERROR: '{}' was NOT executed — {}. \
                                     The operation did not happen. \
                                     You MUST tell the user the action failed and why.",
                                    call.name, denial_reason
                                ),
                                name: Some(call.name.clone()),
                                images: None,
                            });
                            continue;
                        }

                        // Remember this approval for the rest of this turn
                        approved_this_turn.insert(dedup_key);

                        // Create rollback snapshot for RED actions
                        // (actual file backup happens inside specific tool handlers)
                    }
                }

                // Execute the tool
                let tool_result = if let Some(handler) = self.tool_registry.get_handler(&call.name)
                {
                    let handler = handler.clone();
                    let args = call.arguments.clone();
                    // Long-running tools get extended timeouts
                    let timeout_secs = match call.name.as_str() {
                        "install_application"
                        | "uninstall_application"
                        | "update_all_packages"
                        | "install_package"
                        | "uninstall_package" => 300,
                        "search_news" | "fetch_article" => 60,
                        "execute_bash" | "execute_python" | "execute_powershell" => 120,
                        "download_file" => 120,
                        _ => 30,
                    };
                    run_isolated(
                        &format!("tool:{}", call.name),
                        std::time::Duration::from_secs(timeout_secs),
                        move || async move { handler.execute(args).await },
                    )
                    .await
                } else {
                    crate::infra::isolation::ToolResult::err(format!("unknown tool: {}", call.name))
                };

                if let Some(flow) = package_flow.as_mut() {
                    flow.observe_tool_result(call, tool_result.success, &tool_result.data);
                }

                // Log GREEN/YELLOW auto-executed
                if !decision.requires_approval {
                    self.audit_logger.log(
                        session_id,
                        &call.name,
                        &call.arguments,
                        decision.risk_level,
                        Decision::AutoExecuted,
                        DecidedBy::Policy,
                    );
                }

                // Build the string the LLM will see.
                // IMPORTANT: if the tool failed, send the error — not "null" —
                // so the LLM knows to report the failure instead of hallucinating.
                let result_str = if !tool_result.success {
                    let err_msg = tool_result
                        .error
                        .as_deref()
                        .unwrap_or("tool execution failed with no details");
                    format!("TOOL_ERROR: {err_msg}")
                } else {
                    tool_result.data.to_string()
                };
                let truncated = if result_str.len() > TOOL_RESULT_MAX_CHARS {
                    format!("{}...<truncated>", &result_str[..TOOL_RESULT_MAX_CHARS])
                } else {
                    result_str
                };

                // Auto-route: if tool result contains a file path, check if a
                // precognitive tool should process it automatically
                let auto_enrichment = self
                    .auto_route_file_result(&call.name, &tool_result.data)
                    .await;

                let _ = event_tx.send(StreamEvent::ToolEnd {
                    name: call.name.clone(),
                    result: tool_result.data.clone(),
                    success: tool_result.success,
                });

                let tool_msg = if let Some(enrichment) = auto_enrichment {
                    format!(
                        "{}\n\n[Auto-enriched via sidecar]\n{}",
                        truncated, enrichment
                    )
                } else {
                    truncated
                };

                messages.push(ChatMessage {
                    role: "tool".into(),
                    content: tool_msg,
                    name: Some(call.name.clone()),
                    images: None,
                });
            }
        }

        let _ = event_tx.send(StreamEvent::Error(format!(
            "max tool rounds ({}) reached",
            self.max_tool_rounds
        )));
    }

    /// Check if a tool result contains a file path that should be auto-routed
    /// to a precognitive processor for enrichment.
    async fn auto_route_file_result(
        &self,
        tool_name: &str,
        result: &serde_json::Value,
    ) -> Option<String> {
        // Only auto-route results from file-related tools, not from precognitive tools themselves
        if tool_name.starts_with("image_")
            || tool_name.starts_with("document_")
            || tool_name.starts_with("code_")
            || tool_name.starts_with("audio_")
            || tool_name.starts_with("web_")
            || tool_name.starts_with("embeddings_")
        {
            return None;
        }

        // Look for a file path in the result
        let path = result
            .get("path")
            .or_else(|| result.get("file_path"))
            .or_else(|| result.get("output_path"))
            .and_then(|v| v.as_str())?;

        // Determine the target precognitive tool based on extension
        let ext = path.rsplit('.').next()?.to_lowercase();
        let target_tool = match ext.as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tiff" | "svg" => "image_analyze",
            "pdf" | "docx" | "doc" | "csv" | "tsv" | "xlsx" => "document_extract",
            "py" | "rs" | "js" | "ts" | "jsx" | "tsx" | "go" | "java" | "c" | "cpp" | "h"
            | "rb" | "cs" => "code_analyze_ast",
            "wav" | "mp3" | "ogg" | "flac" | "m4a" => "audio_preprocess",
            _ => return None,
        };

        // Execute the precognitive tool
        if let Some(handler) = self.tool_registry.get_handler(target_tool) {
            let params = serde_json::json!({"file_path": path});
            let handler = handler.clone();
            match tokio::time::timeout(std::time::Duration::from_secs(30), handler.execute(params))
                .await
            {
                Ok(result) if result.success => {
                    // Return summary only to save tokens
                    if let Some(summary) = result.data.get("summary").and_then(|s| s.as_str()) {
                        Some(format!("[{}] {}", target_tool, summary))
                    } else {
                        None
                    }
                }
                _ => None,
            }
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_flow_install_starts_with_search() {
        let flow = PackageFlowState::from_user_text("install chrome").unwrap();
        let calls = flow.next_required_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "search_package");
        assert_eq!(calls[0].arguments["query"], "chromium");
    }

    #[test]
    fn package_flow_uninstall_starts_with_precheck() {
        let flow = PackageFlowState::from_user_text("remove chromium").unwrap();
        let calls = flow.next_required_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "check_package_installed");
        assert_eq!(calls[0].arguments["name"], "chromium");
    }

    #[test]
    fn package_flow_uninstall_enforces_action_then_recheck() {
        let mut flow = PackageFlowState::from_user_text("uninstall chromium").unwrap();

        let precheck = flow.next_required_calls();
        assert_eq!(precheck[0].name, "check_package_installed");
        flow.observe_tool_result(
            &precheck[0],
            true,
            &serde_json::json!({ "installed": true }),
        );

        let action = flow.next_required_calls();
        assert_eq!(action[0].name, "uninstall_package");
        flow.observe_tool_result(&action[0], true, &serde_json::json!({ "success": true }));

        let postcheck = flow.next_required_calls();
        assert_eq!(postcheck[0].name, "check_package_installed");
        flow.observe_tool_result(
            &postcheck[0],
            true,
            &serde_json::json!({ "installed": false }),
        );

        assert!(flow.next_required_calls().is_empty());
    }

    #[test]
    fn package_flow_install_stops_when_search_finds_nothing() {
        let mut flow = PackageFlowState::from_user_text("install imaginary-package").unwrap();
        let search = flow.next_required_calls();
        assert_eq!(search[0].name, "search_package");
        flow.observe_tool_result(&search[0], true, &serde_json::json!({ "count": 0 }));

        assert!(flow.next_required_calls().is_empty());
    }

    #[test]
    fn package_flow_uninstall_uses_source_from_precheck() {
        let mut flow = PackageFlowState::from_user_text("uninstall chromium").unwrap();
        let precheck = flow.next_required_calls();
        flow.observe_tool_result(
            &precheck[0],
            true,
            &serde_json::json!({
                "installed": true,
                "source": "snap",
            }),
        );

        let action = flow.next_required_calls();
        assert_eq!(action[0].name, "uninstall_package");
        assert_eq!(action[0].arguments["source"], "snap");
    }

    #[test]
    fn package_flow_uninstall_retries_with_new_source_if_still_installed() {
        let mut flow = PackageFlowState::from_user_text("uninstall chromium").unwrap();

        let precheck = flow.next_required_calls();
        flow.observe_tool_result(
            &precheck[0],
            true,
            &serde_json::json!({
                "installed": true,
                "source": "apt",
            }),
        );

        let action1 = flow.next_required_calls();
        assert_eq!(action1[0].name, "uninstall_package");
        assert_eq!(action1[0].arguments["source"], "apt");
        flow.observe_tool_result(&action1[0], true, &serde_json::json!({ "success": true }));

        let postcheck1 = flow.next_required_calls();
        assert_eq!(postcheck1[0].name, "check_package_installed");
        flow.observe_tool_result(
            &postcheck1[0],
            true,
            &serde_json::json!({
                "installed": true,
                "source": "snap",
            }),
        );

        let action2 = flow.next_required_calls();
        assert_eq!(action2[0].name, "uninstall_package");
        assert_eq!(action2[0].arguments["source"], "snap");
    }

    #[test]
    fn package_flow_install_uses_source_from_search() {
        let mut flow = PackageFlowState::from_user_text("install chromium").unwrap();
        let search = flow.next_required_calls();
        flow.observe_tool_result(
            &search[0],
            true,
            &serde_json::json!({
                "count": 2,
                "results": [
                    {"name": "chromium", "source": "snap"},
                    {"name": "chromium-browser", "source": "apt"}
                ]
            }),
        );

        let precheck = flow.next_required_calls();
        flow.observe_tool_result(
            &precheck[0],
            true,
            &serde_json::json!({
                "installed": false,
                "source": null,
            }),
        );

        let action = flow.next_required_calls();
        assert_eq!(action[0].name, "install_package");
        assert_eq!(action[0].arguments["source"], "snap");
    }

    #[test]
    fn package_flow_ignores_non_package_text() {
        assert!(PackageFlowState::from_user_text("what time is it").is_none());
    }

    #[test]
    fn intent_fallback_prefers_news_tool_for_latest_news_prompt() {
        let mut allowed = HashSet::new();
        allowed.insert("search_news".to_string());

        let call =
            build_intent_fallback_tool_call("latest trusted updates on iran israel war", &allowed)
                .expect("expected fallback tool call");

        assert_eq!(call.name, "search_news");
        assert_eq!(call.arguments["freshness_mode"], "live");
        assert_eq!(call.arguments["source_profile"], "authentic");
        assert_eq!(call.arguments["region"], "middle-east");
    }

    #[test]
    fn intent_fallback_uses_web_search_when_available() {
        let mut allowed = HashSet::new();
        allowed.insert("web_search".to_string());

        let call =
            build_intent_fallback_tool_call("search online for rust ownership examples", &allowed)
                .expect("expected fallback tool call");

        assert_eq!(call.name, "web_search");
        assert_eq!(call.arguments["max_results"], 8);
    }

    #[test]
    fn intent_fallback_uses_gmail_inbox_for_check_gmail_prompt() {
        let mut allowed = HashSet::new();
        allowed.insert("gw_gmail_inbox".to_string());

        let call =
            build_intent_fallback_tool_call("check my gmail inbox for unread messages", &allowed)
                .expect("expected gmail inbox fallback call");

        assert_eq!(call.name, "gw_gmail_inbox");
        assert_eq!(call.arguments["query"], "in:inbox is:unread");
        assert_eq!(call.arguments["max_results"], 10);
    }

    #[test]
    fn intent_fallback_respects_requested_gmail_result_limit() {
        let mut allowed = HashSet::new();
        allowed.insert("gw_gmail_inbox".to_string());

        let call = build_intent_fallback_tool_call(
            "check my gmail for latest 3 unread messages",
            &allowed,
        )
        .expect("expected gmail inbox fallback call");

        assert_eq!(call.name, "gw_gmail_inbox");
        assert_eq!(call.arguments["query"], "in:inbox is:unread");
        assert_eq!(call.arguments["max_results"], 3);
    }

    #[test]
    fn intent_fallback_handles_fetch_latest_unread_gmails_variant() {
        let mut allowed = HashSet::new();
        allowed.insert("gw_gmail_inbox".to_string());

        let call = build_intent_fallback_tool_call("Fetch 3 latest unread gmails", &allowed)
            .expect("expected gmail inbox fallback call");

        assert_eq!(call.name, "gw_gmail_inbox");
        assert_eq!(call.arguments["query"], "in:inbox is:unread");
        assert_eq!(call.arguments["max_results"], 3);
    }

    #[test]
    fn intent_fallback_can_schedule_calendar_event_from_relative_time_prompt() {
        let mut allowed = HashSet::new();
        allowed.insert("gw_calendar_create".to_string());

        let call = build_intent_fallback_tool_call(
            "Schedule a Google Meet tomorrow at 3pm for 30 minutes",
            &allowed,
        )
        .expect("expected calendar create fallback call");

        assert_eq!(call.name, "gw_calendar_create");
        assert_eq!(call.arguments["summary"], "Google Meet");
        assert!(call
            .arguments
            .get("start")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .contains('T'));
        assert!(call
            .arguments
            .get("end")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .contains('T'));
    }

    #[test]
    fn intent_fallback_builds_doc_create_title_from_quotes() {
        let mut allowed = HashSet::new();
        allowed.insert("gw_docs_create".to_string());

        let call = build_intent_fallback_tool_call(
            "Create a Google doc called \"Quarterly Plan\"",
            &allowed,
        )
        .expect("expected docs create fallback call");

        assert_eq!(call.name, "gw_docs_create");
        assert_eq!(call.arguments["title"], "Quarterly Plan");
    }

    #[test]
    fn intent_fallback_maps_forms_listing_to_raw_tool() {
        let mut allowed = HashSet::new();
        allowed.insert("mcp_gworkspace_listForms".to_string());

        let call = build_intent_fallback_tool_call("List my Google Forms", &allowed)
            .expect("expected forms list fallback call");

        assert_eq!(call.name, "mcp_gworkspace_listForms");
    }

    #[test]
    fn google_workspace_request_detector_matches_common_workspace_terms() {
        assert!(looks_like_google_workspace_request(
            "fetch latest unread gmails from inbox"
        ));
        assert!(looks_like_google_workspace_request(
            "create a google form for interview feedback"
        ));
        assert!(!looks_like_google_workspace_request(
            "search latest rust compiler updates"
        ));
    }

    #[test]
    fn intent_fallback_uses_gmail_search_when_available() {
        let mut allowed = HashSet::new();
        allowed.insert("gw_gmail_search".to_string());

        let call =
            build_intent_fallback_tool_call("search gmail for from:boss subject:invoice", &allowed)
                .expect("expected gmail search fallback call");

        assert_eq!(call.name, "gw_gmail_search");
        assert_eq!(call.arguments["query"], "from:boss subject:invoice");
        assert_eq!(call.arguments["max_results"], 10);
    }

    #[test]
    fn intent_fallback_prefers_filesystem_mcp_for_folder_lookup() {
        let mut allowed = HashSet::new();
        allowed.insert("mcp_fs_search_files".to_string());

        let call = build_intent_fallback_tool_call("search for folder name zrok", &allowed)
            .expect("expected filesystem MCP fallback call");

        assert_eq!(call.name, "mcp_fs_search_files");
        assert_eq!(call.arguments["pattern"], "**/zrok");
    }

    #[test]
    fn intent_fallback_uses_builtin_file_pattern_search_when_mcp_unavailable() {
        let mut allowed = HashSet::new();
        allowed.insert("find_files_by_pattern".to_string());

        let call = build_intent_fallback_tool_call("search for folder name zrok", &allowed)
            .expect("expected builtin file fallback call");

        assert_eq!(call.name, "find_files_by_pattern");
        assert_eq!(call.arguments["pattern"], "zrok");
        assert_eq!(call.arguments["type"], "dir");
    }
}
