use chrono::{Datelike, Duration, Local, SecondsFormat, TimeZone, Utc};
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

static CREATE_TITLE_CONTEXT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)\b(?:create|new|start|make|build|draft|write)\b.*\b(?:google\s+(?:doc|docs|sheet|sheets|slides|form|forms)|document|spreadsheet|presentation|deck|form)\b",
    )
    .expect("valid title context regex")
});

static CREATE_TITLE_FALLBACK_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(?:for|about)\s+([^\n\r,.;!?]+)")
        .expect("valid title fallback regex")
});

static TITLE_DURATION_ONLY_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)^\d{1,3}\s*(?:minute|minutes|min|hour|hours|hr|hrs)\b")
        .expect("valid title duration regex")
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

static CALENDAR_ATTENDEE_EMAIL_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b([a-z0-9._%+\-]+@[a-z0-9.\-]+\.[a-z]{2,})\b")
        .expect("valid calendar attendee email regex")
});

static FORCED_TOOL_DIRECTIVE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?is)^\s*#tool:\s*([a-zA-Z0-9_]+)\s*(.*)$")
        .expect("valid forced tool directive regex")
});

static FENCED_CODE_BLOCK_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?is)```(?:[a-z0-9_+\-]+)?\s*(.*?)\s*```")
        .expect("valid fenced code block regex")
});

static SENSITIVE_JSON_FIELD_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?i)"([^"]*(?:api[_-]?key|access[_-]?token|refresh[_-]?token|authorization|secret)[^"]*)"\s*:\s*"([^"\n]{12,})""#,
    )
    .expect("valid sensitive json field regex")
});

static MULTI_NEWLINE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\n{3,}").expect("valid multi newline regex"));

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

fn looks_like_raw_gmail_payload_json(block: &str) -> bool {
    let lower = block.to_ascii_lowercase();
    if !lower.contains("\"messages\"") {
        return false;
    }

    let has_payload_shape_markers = lower.contains("\"query\"")
        || lower.contains("\"requested_count\"")
        || lower.contains("\"returned_count\"")
        || lower.contains("\"llm_visible_message_count\"")
        || lower.contains("\"count\"")
        || lower.contains("\"fully_satisfied\"")
        || lower.contains("\"has_more_results\"");

    let has_gmail_row_markers = lower.contains("\"from\"")
        || lower.contains("\"subject\"")
        || lower.contains("\"preview\"")
        || lower.contains("\"labels\"")
        || lower.contains("\"date\"")
        || lower.contains("\"id\"");

    has_payload_shape_markers && has_gmail_row_markers
}

fn contains_forbidden_payload_markers(block: &str) -> bool {
    let lower = block.to_ascii_lowercase();
    [
        "\"toolbench_rapidapi_key\"",
        "\"toolbench_rapidapi_url\"",
        "\"x-rapidapi-key\"",
        "\"rapidapi_key\"",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
        || SENSITIVE_JSON_FIELD_RE.is_match(block)
}

fn should_filter_code_block(block: &str) -> bool {
    let trimmed = block.trim();
    let json_like = trimmed.starts_with('{') || trimmed.starts_with('[');
    if !json_like {
        return false;
    }

    contains_forbidden_payload_markers(trimmed) || looks_like_raw_gmail_payload_json(trimmed)
}

fn sanitize_assistant_text_response(text: &str) -> String {
    if text.trim().is_empty() {
        return String::new();
    }

    let filtered_blocks = FENCED_CODE_BLOCK_RE
        .replace_all(text, |caps: &regex::Captures| {
            let block = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            if should_filter_code_block(block) {
                "[Filtered unsafe raw payload omitted.]".to_string()
            } else {
                caps.get(0)
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_default()
            }
        })
        .to_string();

    let redacted_inline = SENSITIVE_JSON_FIELD_RE
        .replace_all(&filtered_blocks, |caps: &regex::Captures| {
            let key = caps.get(1).map(|m| m.as_str()).unwrap_or("secret");
            format!(r#""{key}": "[REDACTED]""#)
        })
        .to_string();

    MULTI_NEWLINE_RE
        .replace_all(redacted_inline.trim(), "\n\n")
        .to_string()
}

fn build_tool_call_history_content(tool_calls: &[ParsedToolCall]) -> String {
    tool_calls
        .iter()
        .map(|call| {
            format!(
                "<tool_call>\n{{\"name\":\"{}\",\"arguments\":{}}}\n</tool_call>",
                call.name, call.arguments
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_gmail_tool_name(tool_name: &str) -> bool {
    matches!(tool_name, "gw_gmail_inbox" | "gw_gmail_search" | "gw_gmail_read")
}

fn looks_like_spurious_gmail_capability_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    let unsupported = lower.contains("not directly supported")
        || lower.contains("not supported by the current interface")
        || lower.contains("use a web browser")
        || lower.contains("third-party application");
    let gmail_context = lower.contains("gmail") || lower.contains("email") || lower.contains("inbox");
    unsupported && gmail_context
}

fn strip_spurious_gmail_error_lines(text: &str) -> String {
    let filtered = text
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            let lower = trimmed.to_ascii_lowercase();
            !lower.starts_with("tool_error:") && !looks_like_spurious_gmail_capability_line(trimmed)
        })
        .collect::<Vec<_>>()
        .join("\n");

    MULTI_NEWLINE_RE
        .replace_all(filtered.trim(), "\n\n")
        .to_string()
}

fn extract_grounded_gmail_counts(tool_result: &serde_json::Value) -> Option<(u64, u64)> {
    let payload = tool_result.get("data").unwrap_or(tool_result);

    let requested = payload.get("requested_count").and_then(|v| v.as_u64());
    let returned = payload.get("returned_count").and_then(|v| v.as_u64()).or_else(|| {
        payload
            .get("messages")
            .and_then(|v| v.as_array())
            .map(|messages| messages.len() as u64)
    });

    match (requested, returned) {
        (Some(req), Some(ret)) => Some((req, ret)),
        (None, Some(ret)) => Some((ret, ret)),
        _ => None,
    }
}

fn build_grounded_gmail_count_summary(tool_result: &serde_json::Value) -> Option<String> {
    let (requested, returned) = extract_grounded_gmail_counts(tool_result)?;

    if requested == returned {
        Some(format!(
            "I retrieved {returned} grounded Gmail message(s)."
        ))
    } else {
        Some(format!(
            "I retrieved {returned} grounded Gmail message(s) out of {requested} requested."
        ))
    }
}

fn contains_gmail_placeholder_scaffold(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let has_bracket_placeholders = [
        "[sender's name]",
        "[sender’s name]",
        "[subject of the email]",
        "[preview of the email]",
        "[subject]",
        "[preview]",
    ]
    .iter()
    .any(|marker| lower.contains(marker));

    if has_bracket_placeholders {
        return true;
    }

    [
        "the exact content of the second",
        "the exact content of the third",
        "is not provided in the available data",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn extract_prefixed_value_case_insensitive<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let trimmed = line.trim();
    let (prefix, value) = trimmed.split_once(':')?;
    if !prefix.trim().eq_ignore_ascii_case(key) {
        return None;
    }

    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn normalize_gmail_row_value_for_dedup(value: &str) -> String {
    compact_text_for_llm(value, LLM_GMAIL_FIELD_MAX_CHARS).to_ascii_lowercase()
}

fn contains_duplicate_gmail_rows(text: &str) -> bool {
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut id_occurrences = 0usize;
    let mut duplicate_ids = 0usize;

    let mut seen_from_subject_pairs: HashSet<String> = HashSet::new();
    let mut duplicate_pairs = 0usize;
    let mut pending_from: Option<String> = None;
    let mut pending_subject: Option<String> = None;

    for line in text.lines() {
        if let Some(id) = extract_prefixed_value_case_insensitive(line, "id") {
            let normalized_id = normalize_gmail_row_value_for_dedup(id);
            if !normalized_id.is_empty() {
                id_occurrences += 1;
                if !seen_ids.insert(normalized_id) {
                    duplicate_ids += 1;
                }
            }
        }

        if let Some(from) = extract_prefixed_value_case_insensitive(line, "from") {
            pending_from = Some(normalize_gmail_row_value_for_dedup(from));
        }

        if let Some(subject) = extract_prefixed_value_case_insensitive(line, "subject") {
            pending_subject = Some(normalize_gmail_row_value_for_dedup(subject));
        }

        if let (Some(from), Some(subject)) = (pending_from.as_ref(), pending_subject.as_ref()) {
            let signature = format!("{from}|{subject}");
            if !seen_from_subject_pairs.insert(signature) {
                duplicate_pairs += 1;
            }
            pending_from = None;
            pending_subject = None;
        }
    }

    (id_occurrences >= 2 && duplicate_ids > 0) || duplicate_pairs > 0
}

fn dedupe_grounded_gmail_messages(messages: &[serde_json::Value]) -> Vec<&serde_json::Value> {
    let mut deduped: Vec<&serde_json::Value> = Vec::with_capacity(messages.len());
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut seen_from_subject_pairs: HashSet<String> = HashSet::new();

    for message in messages {
        if let Some(id) = first_non_empty_string_field(
            message,
            &["id", "messageId", "message_id", "threadId", "thread_id"],
            LLM_GMAIL_FIELD_MAX_CHARS,
        ) {
            let key = normalize_gmail_row_value_for_dedup(&id);
            if key.is_empty() || seen_ids.insert(key) {
                deduped.push(message);
            }
            continue;
        }

        let from = first_non_empty_string_field(
            message,
            &["from", "sender", "organizer"],
            LLM_GMAIL_FIELD_MAX_CHARS,
        )
        .unwrap_or_default();

        let subject = first_non_empty_string_field(
            message,
            &["subject", "title", "summary"],
            LLM_GMAIL_FIELD_MAX_CHARS,
        )
        .unwrap_or_default();

        let signature = format!(
            "{}|{}",
            normalize_gmail_row_value_for_dedup(&from),
            normalize_gmail_row_value_for_dedup(&subject)
        );

        if signature == "|" || seen_from_subject_pairs.insert(signature) {
            deduped.push(message);
        }
    }

    deduped
}

fn build_grounded_gmail_message_list_summary(tool_result: &serde_json::Value) -> Option<String> {
    let payload = tool_result.get("data").unwrap_or(tool_result);
    let messages = payload
        .get("messages")
        .or_else(|| payload.get("results"))
        .and_then(|v| v.as_array())?;

    if messages.is_empty() {
        return build_grounded_gmail_count_summary(tool_result);
    }

    let deduped_messages = dedupe_grounded_gmail_messages(messages);
    if deduped_messages.is_empty() {
        return build_grounded_gmail_count_summary(tool_result);
    }

    let (requested, returned) = extract_grounded_gmail_counts(tool_result)
        .unwrap_or((deduped_messages.len() as u64, deduped_messages.len() as u64));

    let returned_for_display = returned.min(deduped_messages.len() as u64);
    let visible_count = returned_for_display as usize;
    let mut lines = Vec::with_capacity(1 + visible_count * 3);

    if requested == returned_for_display {
        lines.push(format!("I retrieved {returned_for_display} grounded Gmail message(s):"));
    } else {
        lines.push(format!(
            "I retrieved {returned_for_display} grounded Gmail message(s) out of {requested} requested:"
        ));
    }

    for (index, message) in deduped_messages.iter().take(visible_count).enumerate() {
        let from = first_non_empty_string_field(
            message,
            &["from", "sender", "organizer"],
            LLM_GMAIL_FIELD_MAX_CHARS,
        )
        .unwrap_or_else(|| "Unknown sender".to_string());

        let subject = first_non_empty_string_field(
            message,
            &["subject", "title", "summary"],
            LLM_GMAIL_FIELD_MAX_CHARS,
        )
        .unwrap_or_else(|| "(No subject)".to_string());

        let preview = first_non_empty_string_field(
            message,
            &["preview", "snippet", "description", "text", "content", "body"],
            LLM_GMAIL_PREVIEW_MAX_CHARS,
        )
        .unwrap_or_else(|| "No preview available.".to_string());

        lines.push(format!("{}. From: {}", index + 1, from));
        lines.push(format!("   Subject: {}", subject));
        lines.push(format!("   Preview: {}", preview));
    }

    Some(lines.join("\n"))
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

    if let Some(title) = infer_title_from_creation_context(trimmed) {
        return title;
    }

    default_title.to_string()
}

fn clean_title_candidate(candidate: &str) -> String {
    let mut title = candidate
        .trim()
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '`')
        .trim()
        .to_string();

    loop {
        let before = title.clone();
        for prefix in ["a ", "an ", "the "] {
            if title.to_ascii_lowercase().starts_with(prefix) {
                title = title[prefix.len()..].trim_start().to_string();
            }
        }
        if title == before {
            break;
        }
    }

    for suffix in [" please", " now", " for me"] {
        while title.to_ascii_lowercase().ends_with(suffix) {
            title = title[..title.len().saturating_sub(suffix.len())]
                .trim_end()
                .to_string();
        }
    }

    title
}

fn infer_title_from_creation_context(user_text: &str) -> Option<String> {
    if !CREATE_TITLE_CONTEXT_RE.is_match(user_text) {
        return None;
    }

    let caps = CREATE_TITLE_FALLBACK_RE.captures(user_text)?;
    let candidate = caps.get(1)?.as_str();
    let title = clean_title_candidate(candidate);
    if title.is_empty() {
        return None;
    }

    let lower = title.to_ascii_lowercase();
    if TITLE_DURATION_ONLY_RE.is_match(&lower)
        || ["today", "tomorrow", "next week", "this week"]
            .iter()
            .any(|kw| lower == *kw)
    {
        return None;
    }

    Some(title)
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

fn looks_like_drive_list_request(text_lower: &str) -> bool {
    let has_drive_context = ["google drive", "drive"]
        .iter()
        .any(|needle| text_lower.contains(needle));
    let has_list_intent = ["list", "show", "browse", "contents", "what is in", "what's in"]
        .iter()
        .any(|needle| text_lower.contains(needle));
    let has_search_intent = ["search", "find", "look for", "locate"]
        .iter()
        .any(|needle| text_lower.contains(needle));

    has_drive_context && has_list_intent && !has_search_intent
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

    let start_utc = start.with_timezone(&Utc);
    let end_utc = end.with_timezone(&Utc);
    Some((
        start_utc.to_rfc3339_opts(SecondsFormat::Secs, true),
        end_utc.to_rfc3339_opts(SecondsFormat::Secs, true),
    ))
}

fn infer_calendar_attendees(user_text: &str) -> Vec<String> {
    let mut attendees = Vec::new();
    for caps in CALENDAR_ATTENDEE_EMAIL_RE.captures_iter(user_text) {
        if let Some(matched) = caps.get(1) {
            let email = matched.as_str().trim().to_ascii_lowercase();
            if !email.is_empty() && !attendees.iter().any(|e: &String| e == &email) {
                attendees.push(email);
            }
        }
    }
    attendees
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
    let attendees = infer_calendar_attendees(user_text);

    let mut args = serde_json::json!({
        "summary": infer_calendar_summary(user_text),
        "start": start,
        "end": end,
        "description": if lower.contains("google meet") || lower.contains("gmeet") || lower.contains("meet link") {
            "Requested via KRIA (Google Meet)"
        } else {
            ""
        },
        "location": "",
    });

    if !attendees.is_empty() {
        args["attendees"] = serde_json::Value::Array(
            attendees
                .into_iter()
                .map(|email| serde_json::json!({ "email": email }))
                .collect(),
        );
    }

    Some(args)
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

fn extract_forced_tool_directive(user_text: &str) -> Option<(String, String)> {
    let caps = FORCED_TOOL_DIRECTIVE_RE.captures(user_text.trim())?;
    let tool = caps.get(1)?.as_str().trim().to_string();
    if tool.is_empty() {
        return None;
    }
    let query = caps
        .get(2)
        .map(|m| m.as_str().trim().to_string())
        .unwrap_or_default();
    Some((tool, query))
}

fn build_fallback_call_for_hint(
    hint: &str,
    user_query: &str,
    allowed_tool_names: &HashSet<String>,
) -> Option<ParsedToolCall> {
    if user_query.is_empty() {
        return None;
    }

    let lower = user_query.to_lowercase();

    match hint {
        "gw_gmail_inbox" if allowed_tool_names.contains("gw_gmail_inbox") => {
            let args = serde_json::json!({
                "query": infer_gmail_list_query(user_query),
                "max_results": infer_requested_limit(user_query, 10, 200),
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
                    "max_results": infer_requested_limit(user_query, 10, 200),
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
            if looks_like_drive_list_request(&lower) && allowed_tool_names.contains("gw_drive_list") {
                Some(ParsedToolCall {
                    name: "gw_drive_list".into(),
                    arguments: serde_json::json!({}),
                })
            } else {
                Some(ParsedToolCall {
                    name: "gw_drive_search".into(),
                    arguments: serde_json::json!({
                        "query": user_query,
                    }),
                })
            }
        }
        "gw_drive_list" if allowed_tool_names.contains("gw_drive_list") => {
            Some(ParsedToolCall {
                name: "gw_drive_list".into(),
                arguments: serde_json::json!({}),
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
        "gw_forms_list" if allowed_tool_names.contains("gw_forms_list") => Some(ParsedToolCall {
            name: "gw_forms_list".into(),
            arguments: serde_json::json!({}),
        }),
        "gw_forms_create" if allowed_tool_names.contains("gw_forms_create") => {
            Some(ParsedToolCall {
                name: "gw_forms_create".into(),
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

fn build_intent_fallback_tool_call(
    user_text: &str,
    allowed_tool_names: &HashSet<String>,
) -> Option<ParsedToolCall> {
    if let Some((forced_tool, forced_query)) = extract_forced_tool_directive(user_text) {
        let query = if forced_query.trim().is_empty() {
            user_text.trim()
        } else {
            forced_query.trim()
        };
        return build_fallback_call_for_hint(&forced_tool, query, allowed_tool_names);
    }

    let intent = IntentRouter::classify(user_text);
    let hint = intent.tool_hint?;
    let user_query = user_text.trim();
    build_fallback_call_for_hint(hint.as_str(), user_query, allowed_tool_names)
}

#[derive(Debug, Clone)]
pub struct ToolChoiceCandidate {
    pub name: String,
    pub label: String,
    pub reason: String,
    pub confidence: f32,
}

fn tool_choice_label(name: &str) -> String {
    match name {
        "search_news" => "News Search".into(),
        "web_search" | "searxng_search" => "Web Search".into(),
        "search_files" | "find_files_by_pattern" | "mcp_fs_search_files" => "File Search".into(),
        "gw_gmail_inbox" | "gw_gmail_search" => "Gmail".into(),
        "gw_calendar_today" | "gw_calendar_search" | "gw_calendar_create" => "Google Calendar".into(),
        "gw_drive_search" | "gw_drive_list" | "gw_drive_read" => "Google Drive".into(),
        "gw_docs_create" | "gw_docs_read" | "gw_docs_edit" => "Google Docs".into(),
        "gw_sheets_create" | "gw_sheets_read" | "gw_sheets_edit" => "Google Sheets".into(),
        "gw_slides_create" | "gw_slides_read" => "Google Slides".into(),
        "gw_forms_list" | "gw_forms_create" => "Google Forms".into(),
        other => other.to_string(),
    }
}

fn push_tool_choice_candidate(
    candidates: &mut Vec<ToolChoiceCandidate>,
    allowed_tool_names: &HashSet<String>,
    name: &str,
    reason: &str,
    confidence: f32,
) {
    if !allowed_tool_names.contains(name) {
        return;
    }

    if candidates.iter().any(|c| c.name == name) {
        return;
    }

    candidates.push(ToolChoiceCandidate {
        name: name.to_string(),
        label: tool_choice_label(name),
        reason: reason.to_string(),
        confidence,
    });
}

fn build_tool_choice_candidates(
    user_text: &str,
    allowed_tool_names: &HashSet<String>,
    primary_hint: Option<&str>,
    confidence: f32,
) -> Vec<ToolChoiceCandidate> {
    let mut candidates: Vec<ToolChoiceCandidate> = Vec::new();
    let lower = user_text.to_lowercase();

    if let Some(primary) = primary_hint {
        push_tool_choice_candidate(
            &mut candidates,
            allowed_tool_names,
            primary,
            "Primary match from intent classifier",
            confidence,
        );
    }

    if lower.contains("news") || lower.contains("headline") {
        push_tool_choice_candidate(
            &mut candidates,
            allowed_tool_names,
            "search_news",
            "Best for current events and corroborated headlines",
            0.62,
        );
    }

    if lower.contains("search") || lower.contains("online") || lower.contains("web") {
        push_tool_choice_candidate(
            &mut candidates,
            allowed_tool_names,
            "web_search",
            "Best for broad web lookups",
            0.60,
        );
        push_tool_choice_candidate(
            &mut candidates,
            allowed_tool_names,
            "searxng_search",
            "Best for self-hosted/privacy web lookups",
            0.58,
        );
    }

    if lower.contains("file") || lower.contains("folder") || lower.contains("directory") {
        push_tool_choice_candidate(
            &mut candidates,
            allowed_tool_names,
            "mcp_fs_search_files",
            "Best for workspace/filesystem search",
            0.61,
        );
        push_tool_choice_candidate(
            &mut candidates,
            allowed_tool_names,
            "find_files_by_pattern",
            "Best for local file pattern lookup",
            0.57,
        );
    }

    if looks_like_google_workspace_request(&lower) {
        for tool in [
            "gw_gmail_inbox",
            "gw_calendar_search",
            "gw_drive_list",
            "gw_drive_search",
            "gw_docs_create",
            "gw_sheets_create",
            "gw_slides_create",
            "gw_forms_list",
        ] {
            push_tool_choice_candidate(
                &mut candidates,
                allowed_tool_names,
                tool,
                "Google Workspace request detected",
                0.56,
            );
        }
    }

    candidates.truncate(6);
    candidates
}

fn build_grounding_count_note(tool_name: &str, tool_result: &serde_json::Value) -> Option<String> {
    if !tool_name.starts_with("gw_") {
        return None;
    }

    let payload = tool_result.get("data").unwrap_or(tool_result);
    let requested = payload
        .get("requested_count")
        .and_then(|v| v.as_u64())?;
    let returned = payload
        .get("returned_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(requested);

    if let Some(visible) = payload
        .get("llm_visible_message_count")
        .and_then(|v| v.as_u64())
    {
        if visible < returned {
            return Some(format!(
                "GROUNDING_NOTE: requested {requested} item(s), returned {returned} grounded item(s), but only {visible} row(s) are visible in this context. Do NOT invent or duplicate hidden rows; enumerate at most {visible} visible row(s) and mention that additional rows were omitted."
            ));
        }
    }

    Some(format!(
        "GROUNDING_NOTE: requested {requested} item(s), returned {returned} grounded item(s). Never claim or enumerate more than {returned}."
    ))
}

const LLM_GMAIL_MESSAGES_CHAR_BUDGET: usize = 3500;
const LLM_GMAIL_PREVIEW_MAX_CHARS: usize = 220;
const LLM_GMAIL_FIELD_MAX_CHARS: usize = 160;
const LLM_GMAIL_WARNING_MAX_CHARS: usize = 180;
const LLM_GMAIL_WARNING_LIMIT: usize = 3;

fn compact_text_for_llm(raw: &str, max_chars: usize) -> String {
    let filtered: String = raw
        .chars()
        .filter(|ch| {
            !matches!(
                *ch,
                '\u{034F}' | '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{2060}' | '\u{FEFF}'
            )
        })
        .collect();
    let collapsed = filtered.split_whitespace().collect::<Vec<_>>().join(" ");

    let trimmed = collapsed.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }

    let mut truncated: String = trimmed.chars().take(max_chars).collect();
    truncated.push_str("...");
    truncated
}

fn first_non_empty_string_field(message: &serde_json::Value, keys: &[&str], max_chars: usize) -> Option<String> {
    keys.iter().find_map(|key| {
        message
            .get(*key)
            .and_then(|v| v.as_str())
            .map(|v| compact_text_for_llm(v, max_chars))
            .filter(|v| !v.is_empty())
    })
}

fn compact_gmail_message_for_llm(message: &serde_json::Value) -> serde_json::Value {
    if !message.is_object() {
        return message.clone();
    }

    let mut compacted = serde_json::Map::new();

    if let Some(subject) = first_non_empty_string_field(
        message,
        &["subject", "title", "summary"],
        LLM_GMAIL_FIELD_MAX_CHARS,
    ) {
        compacted.insert("subject".into(), serde_json::Value::String(subject));
    }

    if let Some(from) = first_non_empty_string_field(
        message,
        &["from", "sender", "organizer"],
        LLM_GMAIL_FIELD_MAX_CHARS,
    ) {
        compacted.insert("from".into(), serde_json::Value::String(from));
    }

    if let Some(date) = first_non_empty_string_field(
        message,
        &["date", "updated", "created"],
        LLM_GMAIL_FIELD_MAX_CHARS,
    ) {
        compacted.insert("date".into(), serde_json::Value::String(date));
    }

    if let Some(id) = first_non_empty_string_field(
        message,
        &["id", "messageId", "message_id", "threadId", "thread_id"],
        LLM_GMAIL_FIELD_MAX_CHARS,
    ) {
        compacted.insert("id".into(), serde_json::Value::String(id));
    }

    if let Some(preview) = first_non_empty_string_field(
        message,
        &["preview", "snippet", "description", "text", "content", "body"],
        LLM_GMAIL_PREVIEW_MAX_CHARS,
    ) {
        compacted.insert("preview".into(), serde_json::Value::String(preview));
    }

    if let Some(url) = first_non_empty_string_field(
        message,
        &["url", "htmlLink", "webViewLink", "alternateLink"],
        LLM_GMAIL_FIELD_MAX_CHARS,
    ) {
        compacted.insert("url".into(), serde_json::Value::String(url));
    }

    serde_json::Value::Object(compacted)
}

fn compact_gmail_messages_for_llm(messages: &[serde_json::Value]) -> (Vec<serde_json::Value>, usize) {
    let mut visible = Vec::new();
    let mut used_chars = 0usize;
    let mut omitted = 0usize;

    for (index, message) in messages.iter().enumerate() {
        let compacted = compact_gmail_message_for_llm(message);
        let chunk_len = compacted.to_string().len();

        if index == 0 || used_chars + chunk_len <= LLM_GMAIL_MESSAGES_CHAR_BUDGET {
            used_chars += chunk_len;
            visible.push(compacted);
        } else {
            omitted += 1;
        }
    }

    (visible, omitted)
}

fn compact_gmail_payload_for_llm(payload: &serde_json::Value) -> serde_json::Value {
    let Some(payload_obj) = payload.as_object() else {
        return payload.clone();
    };

    let mut compacted = payload_obj.clone();

    if let Some(query) = compacted.get("query").and_then(|v| v.as_str()) {
        compacted.insert(
            "query".into(),
            serde_json::Value::String(compact_text_for_llm(query, LLM_GMAIL_FIELD_MAX_CHARS)),
        );
    }

    if let Some(warnings) = compacted.get("warnings").and_then(|v| v.as_array()) {
        let compacted_warnings: Vec<serde_json::Value> = warnings
            .iter()
            .take(LLM_GMAIL_WARNING_LIMIT)
            .filter_map(|warning| warning.as_str())
            .map(|warning| {
                serde_json::Value::String(compact_text_for_llm(
                    warning,
                    LLM_GMAIL_WARNING_MAX_CHARS,
                ))
            })
            .collect();
        compacted.insert("warnings".into(), serde_json::Value::Array(compacted_warnings));
    }

    let messages = compacted
        .get("messages")
        .or_else(|| compacted.get("results"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    if !messages.is_empty() {
        let total = messages.len();
        let (visible_messages, omitted_messages) = compact_gmail_messages_for_llm(&messages);
        compacted.insert("messages".into(), serde_json::Value::Array(visible_messages.clone()));
        compacted.insert(
            "llm_visible_message_count".into(),
            serde_json::json!(visible_messages.len()),
        );
        if omitted_messages > 0 {
            compacted.insert(
                "llm_omitted_message_count".into(),
                serde_json::json!(omitted_messages),
            );
            compacted.insert(
                "warnings".into(),
                match compacted.get("warnings").and_then(|v| v.as_array()) {
                    Some(existing) => {
                        let mut merged = existing.clone();
                        merged.push(serde_json::Value::String(format!(
                            "{} Gmail message(s) omitted from LLM context to stay within context budget.",
                            omitted_messages
                        )));
                        serde_json::Value::Array(merged)
                    }
                    None => serde_json::Value::Array(vec![serde_json::Value::String(format!(
                        "{} Gmail message(s) omitted from LLM context to stay within context budget.",
                        omitted_messages
                    ))]),
                },
            );
        } else {
            compacted.remove("llm_omitted_message_count");
        }
        compacted.insert("count".into(), serde_json::json!(total));
    }

    serde_json::Value::Object(compacted)
}

fn compact_tool_result_for_llm(tool_name: &str, tool_result: &serde_json::Value) -> serde_json::Value {
    let is_gmail_tool = matches!(tool_name, "gw_gmail_inbox" | "gw_gmail_search");
    if !is_gmail_tool {
        return tool_result.clone();
    }

    if tool_result
        .get("provider")
        .and_then(|v| v.as_str())
        .map(|provider| provider.eq_ignore_ascii_case("google_workspace"))
        .unwrap_or(false)
    {
        let mut envelope = tool_result.clone();
        if let Some(env_obj) = envelope.as_object_mut() {
            env_obj.remove("raw_text");
            if let Some(payload) = env_obj.get_mut("data") {
                *payload = compact_gmail_payload_for_llm(payload);
            }
        }
        return envelope;
    }

    compact_gmail_payload_for_llm(tool_result)
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
    /// Tool choice confirmation required for low-confidence routing.
    ToolChoiceRequired {
        query: String,
        confidence: f32,
        min_confidence: f32,
        candidates: Vec<ToolChoiceCandidate>,
    },
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
    min_confidence_to_act: f32,
    clarify_threshold: f32,
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
            min_confidence_to_act: 0.55,
            clarify_threshold: 0.40,
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

    /// Configure confidence thresholds for autonomous intent fallback.
    pub fn with_confidence_thresholds(
        mut self,
        min_confidence_to_act: f32,
        clarify_threshold: f32,
    ) -> Self {
        if (0.0..=1.0).contains(&min_confidence_to_act) {
            self.min_confidence_to_act = min_confidence_to_act;
        }
        if (0.0..=1.0).contains(&clarify_threshold) {
            self.clarify_threshold = clarify_threshold;
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
        let has_images = messages.last().is_some_and(|m| m.has_images());

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
        let mut had_successful_gmail_tool = false;
        let mut had_failed_gmail_tool = false;
        let mut last_successful_gmail_result: Option<serde_json::Value> = None;
        let intent_result = IntentRouter::classify(&last_user_text);
        let forced_tool_requested = extract_forced_tool_directive(&last_user_text).is_some();

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
            let text_response_raw = extract_text_response(&response.content);
            let text_response = sanitize_assistant_text_response(&text_response_raw);

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
                    if forced_tool_requested || intent_result.confidence >= self.min_confidence_to_act
                    {
                        intent_fallback_used = true;
                        synthetic_intent_calls = true;
                        let _ = event_tx.send(StreamEvent::Plan(format!(
                            "No tool call returned; applying intent fallback via {}",
                            fallback_call.name
                        )));
                        tool_calls = vec![fallback_call];
                    } else if intent_result.confidence >= self.clarify_threshold {
                        let candidates = build_tool_choice_candidates(
                            &last_user_text,
                            &allowed_tool_names,
                            intent_result.tool_hint.as_deref(),
                            intent_result.confidence,
                        );

                        if !candidates.is_empty() {
                            let _ = event_tx.send(StreamEvent::ToolChoiceRequired {
                                query: last_user_text.clone(),
                                confidence: intent_result.confidence,
                                min_confidence: self.min_confidence_to_act,
                                candidates,
                            });
                            let _ = event_tx.send(StreamEvent::Done(
                                "Please choose a tool so I can continue this request.".into(),
                            ));
                            return;
                        }
                    }
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
                let mut final_text = if had_successful_gmail_tool && !had_failed_gmail_tool {
                    strip_spurious_gmail_error_lines(&text_response)
                } else {
                    text_response.clone()
                };

                if had_successful_gmail_tool && !had_failed_gmail_tool && !final_text.is_empty() {
                    let has_placeholder_scaffold = contains_gmail_placeholder_scaffold(&final_text);
                    let has_raw_payload = looks_like_raw_gmail_payload_json(final_text.trim());
                    let has_duplicate_rows = contains_duplicate_gmail_rows(&final_text);
                    let should_force_grounded =
                        has_placeholder_scaffold || has_raw_payload || has_duplicate_rows;

                    if should_force_grounded {
                        if let Some(grounded_summary) = last_successful_gmail_result
                            .as_ref()
                            .and_then(build_grounded_gmail_message_list_summary)
                        {
                            tracing::warn!(
                                has_images,
                                round = _round,
                                has_placeholder_scaffold,
                                has_raw_payload,
                                has_duplicate_rows,
                                "LLM returned non-grounded Gmail response; replacing with grounded summary"
                            );
                            final_text = grounded_summary;
                        }
                    }
                }

                if !final_text.is_empty() {
                    let _ = event_tx.send(StreamEvent::Token(final_text.clone()));
                    let _ = event_tx.send(StreamEvent::Done(final_text));
                } else if had_successful_gmail_tool && !had_failed_gmail_tool {
                    if let Some(summary) = last_successful_gmail_result
                        .as_ref()
                        .and_then(build_grounded_gmail_count_summary)
                    {
                        tracing::info!(
                            has_images,
                            round = _round,
                            "LLM returned empty response with no tool calls; using grounded Gmail count summary"
                        );
                        let _ = event_tx.send(StreamEvent::Token(summary.clone()));
                        let _ = event_tx.send(StreamEvent::Done(summary));
                    } else {
                        let fallback =
                            "I could not generate a response for this request. Please try again."
                                .to_string();
                        tracing::warn!(
                            has_images,
                            round = _round,
                            "LLM returned empty response with no tool calls and no grounded Gmail summary"
                        );
                        let _ = event_tx.send(StreamEvent::Token(fallback.clone()));
                        let _ = event_tx.send(StreamEvent::Done(fallback));
                    }
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

            // Add assistant message to history
            if !synthetic_package_calls && !synthetic_intent_calls {
                messages.push(ChatMessage {
                    role: "assistant".into(),
                    content: build_tool_call_history_content(&tool_calls),
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

                if is_gmail_tool_name(&call.name) {
                    if tool_result.success {
                        had_successful_gmail_tool = true;
                        last_successful_gmail_result = Some(tool_result.data.clone());
                    } else {
                        had_failed_gmail_tool = true;
                    }
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
                let llm_tool_result = compact_tool_result_for_llm(&call.name, &tool_result.data);
                let result_str = if !tool_result.success {
                    let err_msg = tool_result
                        .error
                        .as_deref()
                        .unwrap_or("tool execution failed with no details");
                    format!("TOOL_ERROR: {err_msg}")
                } else {
                    llm_tool_result.to_string()
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

                let tool_msg = if let Some(note) = build_grounding_count_note(&call.name, &llm_tool_result) {
                    format!("{tool_msg}\n\n{note}")
                } else {
                    tool_msg
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
                    result
                        .data
                        .get("summary")
                        .and_then(|s| s.as_str())
                        .map(|summary| format!("[{}] {}", target_tool, summary))
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
    fn grounding_count_note_uses_google_requested_and_returned_counts() {
        let tool_result = serde_json::json!({
            "provider": "google_workspace",
            "data": {
                "requested_count": 5,
                "returned_count": 3,
            }
        });

        let note = build_grounding_count_note("gw_gmail_inbox", &tool_result)
            .expect("expected grounding note");

        assert!(note.contains("requested 5"));
        assert!(note.contains("returned 3"));
        assert!(note.contains("Never claim or enumerate more than 3"));
    }

        #[test]
        fn sanitize_assistant_text_response_filters_sensitive_json_blocks() {
                let raw = r#"The latest unread emails are listed above.

```json
{
    "data": {"messages": []},
    "toolbench_rapidapi_key": "088440d910mshef857391f2fc461p17ae9ejsnaebc918926ff"
}
```

Please open Gmail for full details."#;

                let sanitized = sanitize_assistant_text_response(raw);
                assert!(sanitized.contains("The latest unread emails are listed above."));
                assert!(sanitized.contains("[Filtered unsafe raw payload omitted.]"));
                assert!(!sanitized.contains("toolbench_rapidapi_key"));
                assert!(!sanitized.contains("088440d910mshef857391f2fc461p17ae9ejsnaebc918926ff"));
        }

        #[test]
        fn sanitize_assistant_text_response_filters_raw_gmail_payload_blocks() {
                let raw = r#"```json
{
    "query": "in:inbox is:unread",
    "requested_count": 3,
    "returned_count": 3,
    "messages": [
        {"id": "m1", "from": "sender@example.com"}
    ]
}
```"#;

                let sanitized = sanitize_assistant_text_response(raw);
                assert_eq!(sanitized.trim(), "[Filtered unsafe raw payload omitted.]");
        }

        #[test]
        fn sanitize_assistant_text_response_filters_gmail_payload_without_query_field() {
                let raw = r#"```json
{
    "data": {
        "count": 3,
        "fully_satisfied": true,
        "messages": [
            {
                "from": "owner@example.com",
                "date": "Sat, 18 Apr 2026 05:49:26 +0000",
                "id": "m1",
                "preview": "You have been invited"
            }
        ]
    }
}
```"#;

                let sanitized = sanitize_assistant_text_response(raw);
                assert_eq!(sanitized.trim(), "[Filtered unsafe raw payload omitted.]");
        }

        #[test]
        fn sanitize_assistant_text_response_preserves_normal_code_blocks() {
                let raw = r#"Use this helper:

```python
print("hello")
```
"#;

                let sanitized = sanitize_assistant_text_response(raw);
                assert!(sanitized.contains("print(\"hello\")"));
                assert!(sanitized.contains("```python"));
        }

            #[test]
            fn build_tool_call_history_content_outputs_canonical_calls_only() {
                let calls = vec![
                    ParsedToolCall {
                        name: "gw_gmail_inbox".into(),
                        arguments: serde_json::json!({
                            "query": "in:inbox is:unread",
                            "max_results": 3
                        }),
                    },
                    ParsedToolCall {
                        name: "gw_gmail_read".into(),
                        arguments: serde_json::json!({ "message_id": "abc123" }),
                    },
                ];

                let serialized = build_tool_call_history_content(&calls);

                assert!(serialized.contains("<tool_call>"));
                assert!(serialized.contains("\"name\":\"gw_gmail_inbox\""));
                assert!(serialized.contains("\"name\":\"gw_gmail_read\""));
                assert!(!serialized.contains("TOOL_ERROR"));
            }

            #[test]
            fn strip_spurious_gmail_error_lines_removes_tool_error_and_capability_claims() {
                let raw = "Fetched 3 unread emails.\nTOOL_ERROR: The operation to fetch emails is not directly supported by the current interface. Please use a web browser or a third-party application for checking your Gmail inbox.\nDone.";
                let cleaned = strip_spurious_gmail_error_lines(raw);

                assert!(cleaned.contains("Fetched 3 unread emails."));
                assert!(cleaned.contains("Done."));
                assert!(!cleaned.contains("TOOL_ERROR:"));
                assert!(!cleaned.contains("not directly supported"));
                assert!(!cleaned.contains("third-party application"));
            }

    #[test]
    fn grounded_gmail_count_summary_uses_requested_and_returned_counts() {
        let tool_result = serde_json::json!({
            "provider": "google_workspace",
            "kind": "gmail",
            "data": {
                "requested_count": 5,
                "returned_count": 3,
            }
        });

        let summary = build_grounded_gmail_count_summary(&tool_result)
            .expect("expected grounded Gmail count summary");

        assert_eq!(
            summary,
            "I retrieved 3 grounded Gmail message(s) out of 5 requested."
        );
    }

    #[test]
    fn grounded_gmail_count_summary_uses_message_count_fallback() {
        let tool_result = serde_json::json!({
            "provider": "google_workspace",
            "kind": "gmail",
            "data": {
                "messages": [
                    {"id": "m1"},
                    {"id": "m2"}
                ]
            }
        });

        let summary = build_grounded_gmail_count_summary(&tool_result)
            .expect("expected grounded Gmail count summary");

        assert_eq!(summary, "I retrieved 2 grounded Gmail message(s).");
    }

    #[test]
    fn grounded_gmail_count_summary_returns_none_without_counts() {
        let tool_result = serde_json::json!({
            "provider": "google_workspace",
            "kind": "gmail",
            "data": {
                "query": "in:inbox is:unread",
            }
        });

        assert!(build_grounded_gmail_count_summary(&tool_result).is_none());
    }

    #[test]
    fn detects_placeholder_scaffold_in_gmail_response() {
        let response = "1. From: [Sender's Name]\n   Subject: [Subject of the Email]\n   Preview: [Preview of the email]";
        assert!(contains_gmail_placeholder_scaffold(response));

        let grounded = "1. From: alerts@example.com\n   Subject: Security alert\n   Preview: A new sign-in was detected";
        assert!(!contains_gmail_placeholder_scaffold(grounded));
    }

    #[test]
    fn detects_duplicate_gmail_rows_in_response_text() {
        let duplicated = "Here are the latest 3 unread Gmails:\nDate: Sat, 18 Apr 2026 05:49:26 +0000\nFrom: obaidullah zeeshan <obaidzeeshan.official@gmail.com>\nID: 19d9f230a2e500b1\nPreview: Invitation details\nSubject: Invitation: Kria Presenta...\nDate: Sat, 18 Apr 2026 05:49:26 +0000\nFrom: obaidullah zeeshan <obaidzeeshan.official@gmail.com>\nID: 19d9f230a2e500b1\nPreview: Invitation details\nSubject: Invitation: Kria Presenta...";
        assert!(contains_duplicate_gmail_rows(duplicated));
    }

    #[test]
    fn does_not_flag_unique_gmail_rows_in_response_text() {
        let unique_rows = "Here are unread emails:\n1. From: Make <info@make.com>\n   Subject: Meet the new Make Grid\n   Preview: Product updates\n2. From: Google <no-reply@accounts.google.com>\n   Subject: Security alert\n   Preview: A new sign-in was detected\nID: m1\nID: m2";
        assert!(!contains_duplicate_gmail_rows(unique_rows));
    }

    #[test]
    fn grounding_note_limits_gmail_enumeration_to_visible_rows() {
        let note = build_grounding_count_note(
            "gw_gmail_inbox",
            &serde_json::json!({
                "provider": "google_workspace",
                "kind": "gmail",
                "data": {
                    "requested_count": 3,
                    "returned_count": 3,
                    "llm_visible_message_count": 1,
                }
            }),
        )
        .expect("expected grounding note");

        assert!(note.contains("only 1 row(s) are visible"));
        assert!(note.contains("enumerate at most 1"));
    }

    #[test]
    fn grounded_gmail_message_list_summary_uses_real_message_fields() {
        let tool_result = serde_json::json!({
            "provider": "google_workspace",
            "kind": "gmail",
            "data": {
                "requested_count": 3,
                "returned_count": 3,
                "messages": [
                    {
                        "from": "Make <info@make.com>",
                        "subject": "Meet the new Make Grid",
                        "preview": "See what's new in your workflow grid."
                    },
                    {
                        "from": "Google <no-reply@accounts.google.com>",
                        "subject": "Security alert",
                        "preview": "A new sign-in was detected."
                    },
                    {
                        "from": "alerts@example.com",
                        "subject": "Deployment complete",
                        "preview": "Your production deployment is now live."
                    }
                ]
            }
        });

        let summary = build_grounded_gmail_message_list_summary(&tool_result)
            .expect("expected grounded gmail list summary");

        assert!(summary.contains("I retrieved 3 grounded Gmail message(s):"));
        assert!(summary.contains("1. From: Make <info@make.com>"));
        assert!(summary.contains("Subject: Meet the new Make Grid"));
        assert!(!summary.contains("[Sender"));
        assert!(!summary.contains("[Subject"));
    }

    #[test]
    fn grounded_gmail_message_list_summary_deduplicates_duplicate_ids() {
        let tool_result = serde_json::json!({
            "provider": "google_workspace",
            "kind": "gmail",
            "data": {
                "requested_count": 3,
                "returned_count": 3,
                "messages": [
                    {
                        "id": "m1",
                        "from": "owner@example.com",
                        "subject": "Invitation",
                        "preview": "Join us"
                    },
                    {
                        "id": "m1",
                        "from": "owner@example.com",
                        "subject": "Invitation",
                        "preview": "Join us"
                    },
                    {
                        "id": "m2",
                        "from": "alerts@example.com",
                        "subject": "Security alert",
                        "preview": "A new sign-in was detected"
                    }
                ]
            }
        });

        let summary = build_grounded_gmail_message_list_summary(&tool_result)
            .expect("expected grounded gmail list summary");

        assert!(summary.contains("I retrieved 2 grounded Gmail message(s) out of 3 requested:"));
        assert_eq!(summary.matches("Subject: Invitation").count(), 1);
        assert_eq!(summary.matches("Subject: Security alert").count(), 1);
    }

    #[test]
    fn compact_tool_result_for_llm_preserves_gmail_rows_and_removes_raw_text() {
        let long_preview = "x".repeat(380);
        let tool_result = serde_json::json!({
            "provider": "google_workspace",
            "kind": "gmail",
            "tool": "searchGmail",
            "data": {
                "query": "in:inbox is:unread",
                "requested_count": 3,
                "returned_count": 3,
                "messages": [
                    {
                        "subject": "Invitation",
                        "from": "owner@example.com",
                        "date": "Sat, 18 Apr 2026 05:49:26 +0000",
                        "id": "m1",
                        "labels": ["UNREAD", "CATEGORY_PERSONAL"],
                        "preview": "You are invited"
                    },
                    {
                        "subject": "Meet the new Make Grid",
                        "from": "Make <info@make.com>",
                        "date": "Fri, 10 Apr 2026 10:47:32 +0000",
                        "id": "m2",
                        "labels": ["CATEGORY_PROMOTIONS", "UNREAD"],
                        "preview": long_preview
                    },
                    {
                        "subject": "Security alert",
                        "from": "Google <no-reply@accounts.google.com>",
                        "date": "Thu, 09 Apr 2026 07:36:37 GMT",
                        "id": "m3",
                        "labels": ["CATEGORY_UPDATES", "UNREAD"],
                        "preview": "A new sign-in was detected"
                    }
                ]
            },
            "raw_text": "raw page output should not be passed into llm context"
        });

        let compact = compact_tool_result_for_llm("gw_gmail_inbox", &tool_result);

        assert!(compact.get("raw_text").is_none());
        let messages = compact["data"]["messages"]
            .as_array()
            .expect("expected compacted gmail messages array");
        assert_eq!(messages.len(), 3);
        assert!(messages[0].get("category").is_none());
        assert!(messages[1].get("category").is_none());
        assert!(messages[2].get("category").is_none());
        assert!(messages[0].get("labels").is_none());
        assert!(messages[1].get("labels").is_none());
        assert!(messages[2].get("labels").is_none());

        let preview_len = messages[1]["preview"]
            .as_str()
            .unwrap_or_default()
            .chars()
            .count();
        assert!(preview_len <= LLM_GMAIL_PREVIEW_MAX_CHARS + 3);
    }

    #[test]
    fn compact_gmail_payload_for_llm_tracks_omitted_messages_when_budget_exceeded() {
        let messages: Vec<serde_json::Value> = (0..24)
            .map(|index| {
                serde_json::json!({
                    "subject": format!("Message {index}"),
                    "from": "sender@example.com",
                    "date": "Sat, 18 Apr 2026 05:49:26 +0000",
                    "id": format!("m{index}"),
                    "labels": ["UNREAD", "CATEGORY_PERSONAL"],
                    "preview": "p".repeat(320),
                })
            })
            .collect();

        let payload = serde_json::json!({
            "query": "in:inbox is:unread",
            "requested_count": 24,
            "returned_count": 24,
            "messages": messages,
        });

        let compact = compact_gmail_payload_for_llm(&payload);
        let visible = compact["messages"]
            .as_array()
            .expect("expected compacted messages")
            .len();

        assert!(visible < 24);
        assert_eq!(compact["llm_visible_message_count"], serde_json::json!(visible));
        assert_eq!(
            compact["llm_omitted_message_count"],
            serde_json::json!(24 - visible)
        );
    }

    #[test]
    fn grounding_note_uses_compacted_visible_count_after_compact_tool_result() {
        // Simulate the exact pipeline: envelope → compact_tool_result_for_llm → build_grounding_count_note
        let messages: Vec<serde_json::Value> = (0..10)
            .map(|i| {
                serde_json::json!({
                    "subject": format!("Message {i} with a long subject to consume budget"),
                    "from": format!("sender{i}@example.com"),
                    "date": "Sat, 18 Apr 2026 05:49:26 +0000",
                    "id": format!("m{i}"),
                    "preview": "p".repeat(300),
                })
            })
            .collect();

        let raw_envelope = serde_json::json!({
            "provider": "google_workspace",
            "kind": "gmail",
            "tool": "searchGmail",
            "data": {
                "query": "in:inbox is:unread",
                "requested_count": 10,
                "returned_count": 10,
                "messages": messages,
            },
            "raw_text": "irrelevant"
        });

        let compacted = compact_tool_result_for_llm("gw_gmail_inbox", &raw_envelope);
        let note = build_grounding_count_note("gw_gmail_inbox", &compacted)
            .expect("expected grounding note from compacted result");

        let visible = compacted
            .pointer("/data/llm_visible_message_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(10);

        if visible < 10 {
            assert!(
                note.contains(&format!("only {visible} row(s) are visible")),
                "grounding note should reflect compacted visible count, got: {note}"
            );
            assert!(
                note.contains(&format!("enumerate at most {visible}")),
                "grounding note should cap enumeration at visible count, got: {note}"
            );
        } else {
            assert!(
                note.contains("returned 10 grounded item(s)"),
                "grounding note should reflect all items visible, got: {note}"
            );
        }
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
    fn intent_fallback_allows_large_gmail_result_limit_up_to_cap() {
        let mut allowed = HashSet::new();
        allowed.insert("gw_gmail_inbox".to_string());

        let call = build_intent_fallback_tool_call(
            "fetch latest 120 unread gmails",
            &allowed,
        )
        .expect("expected gmail inbox fallback call");

        assert_eq!(call.name, "gw_gmail_inbox");
        assert_eq!(call.arguments["query"], "in:inbox is:unread");
        assert_eq!(call.arguments["max_results"], 120);
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
            .ends_with('Z'));
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
            .ends_with('Z'));
        assert!(call
            .arguments
            .get("end")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .contains('T'));
    }

    #[test]
    fn intent_fallback_extracts_calendar_attendees() {
        let mut allowed = HashSet::new();
        allowed.insert("gw_calendar_create".to_string());

        let call = build_intent_fallback_tool_call(
            "Schedule a Google Meet tomorrow at 3pm for 30 minutes and add zeeshanobaid335@gmail.com as an attendee",
            &allowed,
        )
        .expect("expected calendar create fallback call");

        assert_eq!(call.name, "gw_calendar_create");
        assert_eq!(call.arguments["attendees"][0]["email"], "zeeshanobaid335@gmail.com");
    }

    #[test]
    fn intent_fallback_uses_for_clause_for_sheet_title() {
        let mut allowed = HashSet::new();
        allowed.insert("gw_sheets_create".to_string());

        let call = build_intent_fallback_tool_call(
            "Create a Google Sheet for monthly budget",
            &allowed,
        )
        .expect("expected sheets create fallback call");

        assert_eq!(call.name, "gw_sheets_create");
        assert_eq!(call.arguments["title"], "monthly budget");
    }

    #[test]
    fn intent_fallback_routes_drive_listing_prompt_to_drive_list() {
        let allowed: HashSet<String> = ["gw_drive_search", "gw_drive_list"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        let call = build_intent_fallback_tool_call("List files in my Google drive", &allowed)
            .expect("expected drive fallback call");

        assert_eq!(call.name, "gw_drive_list");
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
    fn intent_fallback_maps_forms_listing_to_curated_tool() {
        let mut allowed = HashSet::new();
        allowed.insert("gw_forms_list".to_string());

        let call = build_intent_fallback_tool_call("List my Google Forms", &allowed)
            .expect("expected forms list fallback call");

        assert_eq!(call.name, "gw_forms_list");
    }

    #[test]
    fn forced_tool_directive_overrides_intent_classification() {
        let mut allowed = HashSet::new();
        allowed.insert("gw_gmail_inbox".to_string());

        let call = build_intent_fallback_tool_call(
            "#tool:gw_gmail_inbox please check unread messages",
            &allowed,
        )
        .expect("expected forced tool fallback call");

        assert_eq!(call.name, "gw_gmail_inbox");
        assert_eq!(call.arguments["query"], "in:inbox is:unread");
    }

    #[test]
    fn tool_choice_candidates_include_primary_and_web_alternatives() {
        let allowed: HashSet<String> = ["search_news", "web_search", "searxng_search"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        let candidates = build_tool_choice_candidates(
            "search the web for latest headlines about robotics",
            &allowed,
            Some("search_news"),
            0.49,
        );

        assert!(!candidates.is_empty());
        assert_eq!(candidates[0].name, "search_news");
        assert!(candidates.iter().any(|c| c.name == "web_search"));
    }

    #[test]
    fn tool_choice_candidates_include_google_workspace_options() {
        let allowed: HashSet<String> = ["gw_gmail_inbox", "gw_calendar_search", "gw_drive_search"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        let candidates =
            build_tool_choice_candidates("check my gmail for unread messages", &allowed, None, 0.45);

        assert!(candidates.iter().any(|c| c.name == "gw_gmail_inbox"));
        assert!(candidates.iter().any(|c| c.name == "gw_calendar_search"));
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
