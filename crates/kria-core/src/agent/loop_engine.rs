use chrono::{Datelike, Duration, Local, SecondsFormat, TimeZone, Utc};
use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agent::response_parser::{
    extract_text_response, parse_tool_calls_with_known, ParsedToolCall,
};
use crate::agent::router::IntentRouter;
use crate::infra::isolation::run_isolated;
use crate::infra::pipeline_trace::{
    log_pipeline_step, sanitize_json_for_logs, sanitize_text_for_logs,
};
use crate::llm::tokenize::count_tokens;
use crate::llm::{
    ChatMessage, ModelRouter, ToolSchema, LLM_TOOL_RESULT_TOKEN_BUDGET, LLM_TURN_TOOL_BUDGET,
    TOOL_RESULT_MAX_CHARS,
};
use crate::mcp::payload_shaper::shape_for_llm;
use crate::safety::audit::{DecidedBy, Decision};
use crate::safety::hitl::{ApprovalResponse, HitlGateway};
use crate::safety::{AuditLogger, PolicyEngine, RiskLevel, RollbackManager};
use crate::tools::mount_manager::{google_meet_fallback_metadata, ToolMountManager};
use crate::tools::registry::{ToolDef, ToolRegistry};

/// Compute a stable u64 hash for a `(tool_name, arguments)` pair.
/// Used to detect duplicate failed tool calls within a single turn.
fn call_dedup_hash(tool_name: &str, arguments: &serde_json::Value) -> u64 {
    // Canonicalize argument JSON (sort keys) so {"a":1,"b":2} == {"b":2,"a":1}.
    let canonical_args = canonical_json(arguments);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    tool_name.hash(&mut h);
    canonical_args.hash(&mut h);
    h.finish()
}

/// Serialize a `serde_json::Value` with object keys sorted for stable comparison.
fn canonical_json(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Object(map) => {
            let mut pairs: Vec<(&String, &serde_json::Value)> = map.iter().collect();
            pairs.sort_by_key(|(k, _)| k.as_str());
            let inner = pairs
                .iter()
                .map(|(k, v)| format!("\"{}\":{}", k, canonical_json(v)))
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{inner}}}")
        }
        serde_json::Value::Array(arr) => {
            let inner = arr
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",");
            format!("[{inner}]")
        }
        other => other.to_string(),
    }
}

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

static GMAIL_SEND_BODY_BEFORE_MAIL_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)\b(?:send|write|compose|draft)\b\s+(?:an?\s+|the\s+)?(.+?)\s+\b(?:mail|email|gmail)\b",
    )
    .expect("valid gmail send body-before-mail regex")
});

static GMAIL_SEND_BODY_AFTER_SAYING_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(?:saying|say|with\s+message|message\s+is)\b\s+(.+?)(?:\s+\bto\b|$)")
        .expect("valid gmail send body-after-saying regex")
});

static GMAIL_SEND_SUBJECT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\bsubject\b\s*(?::|is)?\s+([^\n\r,;!?]+)")
        .expect("valid gmail send subject regex")
});

static GMAIL_MESSAGE_ID_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(?:message[_\s-]?id|gmail[_\s-]?id|email[_\s-]?id)\b\s*[:=]?\s*([A-Za-z0-9_-]{10,})")
        .expect("valid gmail message id regex")
});

static CALENDAR_EVENT_ID_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(?:event[_\s-]?id|calendar[_\s-]?event[_\s-]?id)\b\s*[:=]?\s*([A-Za-z0-9_@-]{8,})")
        .expect("valid calendar event id regex")
});

static GENERIC_RESOURCE_ID_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(?:file[_\s-]?id|document[_\s-]?id|spreadsheet[_\s-]?id|presentation[_\s-]?id|id)\b\s*[:=]?\s*([A-Za-z0-9_-]{10,})")
        .expect("valid generic resource id regex")
});

static SHEETS_RANGE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b([A-Za-z0-9_]+![A-Z]+\d+(?::[A-Z]+\d+)?|[A-Z]+\d+(?::[A-Z]+\d+)?)\b")
        .expect("valid sheets range regex")
});

static APPEND_TEXT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(?:append|add|insert|write)\b\s+(.+?)(?:\s+\b(?:to|into|in)\b|$)")
        .expect("valid append text regex")
});

static SEND_CONFIRMATION_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)^\s*(?:yes\s*,?\s*)?(?:send(?:\s+it)?|go\s+ahead|confirm|proceed)(?:\s+(?:now|immediately|right\s+now))?\s*[.!]?\s*$",
    )
    .expect("valid send confirmation regex")
});

static FORCED_TOOL_DIRECTIVE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?is)^\s*#tool:\s*([a-zA-Z0-9_-]+)\s*(.*)$")
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

fn build_message_preview(messages: &[ChatMessage], max_messages: usize) -> serde_json::Value {
    let start = messages.len().saturating_sub(max_messages);
    let preview: Vec<serde_json::Value> = messages
        .iter()
        .skip(start)
        .map(|m| {
            let content_chars = m.content.chars().count();
            let content_preview = if m.role.eq_ignore_ascii_case("system") {
                format!("[system prompt omitted; {content_chars} chars]")
            } else {
                sanitize_text_for_logs(&m.content, 160)
            };

            serde_json::json!({
                "role": m.role,
                "name": m.name,
                "has_images": m.has_images(),
                "content": content_preview,
                "content_chars": content_chars,
            })
        })
        .collect();

    serde_json::Value::Array(preview)
}

fn build_tool_calls_preview(tool_calls: &[ParsedToolCall]) -> serde_json::Value {
    let preview: Vec<serde_json::Value> = tool_calls
        .iter()
        .map(|call| {
            serde_json::json!({
                "name": call.name,
                "arguments": sanitize_json_for_logs(&call.arguments, 220, 8),
            })
        })
        .collect();

    serde_json::Value::Array(preview)
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
    matches!(
        tool_name,
        "gw_gmail_inbox" | "gw_gmail_search" | "gw_gmail_read" | "gw_gmail_send"
    )
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

/// Build a user-facing confirmation response for a successful `generate_image` call.
/// This avoids a second LLM round-trip that would crash ctx=2048 with 167 tool schemas.
fn build_image_success_response(tool_result: &serde_json::Value) -> String {
    let images = tool_result.get("images").and_then(|v| v.as_array());
    let count = images.map(|a| a.len()).unwrap_or(0);
    let first = images.and_then(|a| a.first());
    let provenance = first
        .and_then(|img| img.get("provenance"))
        .and_then(|v| v.as_str())
        .unwrap_or("AI");
    let elapsed_ms = tool_result.get("elapsed_ms").and_then(|v| v.as_u64()).unwrap_or(0);
    let elapsed_s = elapsed_ms as f64 / 1000.0;
    let path = first
        .and_then(|img| img.get("path"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let seed = first
        .and_then(|img| img.get("seed"))
        .and_then(|v| v.as_u64());
    let quality = first
        .and_then(|img| img.get("quality"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tier = tool_result
        .get("tier_used")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let source = if provenance.contains("pollinations") {
        "Pollinations.ai (Flux.1-schnell)"
    } else if provenance.starts_with("cloud") {
        "cloud AI"
    } else {
        "local AI"
    };

    let meta = {
        let mut parts = Vec::new();
        if !quality.is_empty() { parts.push(format!("quality: {quality}")); }
        if !tier.is_empty() { parts.push(format!("tier: {tier}")); }
        if let Some(s) = seed { parts.push(format!("seed: {s}")); }
        if parts.is_empty() { String::new() } else { format!(" ({})", parts.join(", ")) }
    };

    if count == 1 && !path.is_empty() {
        format!(
            "Image generated in {elapsed_s:.1}s using {source}{meta}.\nSaved to: `{path}`"
        )
    } else if count > 1 {
        format!("{count} images generated in {elapsed_s:.1}s using {source}{meta}.")
    } else {
        "Image generated successfully.".to_string()
    }
}

fn build_image_failure_response(data: &serde_json::Value) -> String {
    let report = data.get("failure_report");
    let stage = report
        .and_then(|r| r.get("stage"))
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown");
    let message = report
        .and_then(|r| r.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("Image generation failed");
    let hint = report
        .and_then(|r| r.get("hint"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let provider = report
        .and_then(|r| r.get("provider"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let mut msg = format!("Image generation failed at stage **{stage}**");
    if !provider.is_empty() { msg.push_str(&format!(" (provider: {provider})")); }
    msg.push_str(&format!(": {message}"));
    if !hint.is_empty() { msg.push_str(&format!("\n\nHint: {hint}")); }
    msg
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

fn clean_gmail_body_candidate(candidate: &str) -> Option<String> {
    let mut body = candidate
        .trim()
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '`')
        .trim()
        .to_string();

    if body.is_empty() {
        return None;
    }

    // Avoid turning vague references into accidental sends.
    let normalized = body.to_ascii_lowercase();
    if matches!(normalized.as_str(), "mail" | "email" | "gmail" | "this" | "that" | "it") {
        return None;
    }

    // Strip trailing connective phrases that can leak from loose extraction.
    for marker in [" to ", " for "] {
        if let Some((head, _)) = body.split_once(marker) {
            let trimmed = head.trim();
            if !trimmed.is_empty() {
                body = trimmed.to_string();
            }
        }
    }

    if body.is_empty() {
        None
    } else {
        Some(body)
    }
}

fn infer_gmail_send_body(user_text: &str) -> Option<String> {
    if let Some(caps) = GMAIL_SEND_BODY_AFTER_SAYING_RE.captures(user_text) {
        if let Some(matched) = caps.get(1) {
            if let Some(body) = clean_gmail_body_candidate(matched.as_str()) {
                return Some(body);
            }
        }
    }

    if let Some(caps) = GMAIL_SEND_BODY_BEFORE_MAIL_RE.captures(user_text) {
        if let Some(matched) = caps.get(1) {
            if let Some(body) = clean_gmail_body_candidate(matched.as_str()) {
                return Some(body);
            }
        }
    }

    if let Some(caps) = QUOTED_TEXT_RE.captures(user_text) {
        if let Some(matched) = caps.get(1).or_else(|| caps.get(2)) {
            if let Some(body) = clean_gmail_body_candidate(matched.as_str()) {
                return Some(body);
            }
        }
    }

    None
}

fn infer_gmail_send_subject(user_text: &str, body: &str) -> String {
    if let Some(caps) = GMAIL_SEND_SUBJECT_RE.captures(user_text) {
        if let Some(matched) = caps.get(1) {
            let subject = matched.as_str().trim();
            if !subject.is_empty() {
                return subject.to_string();
            }
        }
    }

    let one_line = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if !one_line.is_empty() && one_line.len() <= 64 {
        return one_line;
    }

    "Message from KRIA".to_string()
}

fn infer_gmail_send_arguments(user_text: &str) -> Option<serde_json::Value> {
    let to = CALENDAR_ATTENDEE_EMAIL_RE
        .captures(user_text)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().trim().to_ascii_lowercase())?;

    let body = infer_gmail_send_body(user_text)?;
    let subject = infer_gmail_send_subject(user_text, &body);

    Some(serde_json::json!({
        "to": to,
        "subject": subject,
        "body": body,
    }))
}

fn clean_identifier_candidate(candidate: &str) -> Option<String> {
    let id = candidate
        .trim()
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '`' || c == '(' || c == ')')
        .trim();

    if id.len() < 8 {
        return None;
    }

    if !id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '@'))
    {
        return None;
    }

    Some(id.to_string())
}

fn clean_content_candidate(candidate: &str) -> Option<String> {
    let cleaned = candidate
        .trim()
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '`')
        .trim()
        .trim_end_matches(|c: char| matches!(c, '.' | ',' | ';' | '!'))
        .trim();

    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.to_string())
    }
}

fn extract_identifier_from_url_marker(text: &str, marker: &str) -> Option<String> {
    let (_, rest) = text.split_once(marker)?;
    let candidate = rest
        .trim_start()
        .split(|c: char| {
            c.is_whitespace() || matches!(c, '/' | '?' | '&' | '#' | ',' | ';' | '"' | '\'')
        })
        .next()
        .unwrap_or("");
    clean_identifier_candidate(candidate)
}

fn extract_identifier_after_keyword(text: &str, keywords: &[&str]) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    for keyword in keywords {
        if let Some(idx) = lower.find(keyword) {
            let start = idx + keyword.len();
            if let Some(rest) = text.get(start..) {
                let candidate = rest
                    .trim_start()
                    .trim_start_matches(|c: char| matches!(c, ':' | '=' | '#' | '/'))
                    .split(|c: char| {
                        c.is_whitespace()
                            || matches!(c, '/' | '?' | '&' | '#' | ',' | ';' | '"' | '\'' | '(' | ')')
                    })
                    .next()
                    .unwrap_or("");
                if let Some(id) = clean_identifier_candidate(candidate) {
                    return Some(id);
                }
            }
        }
    }
    None
}

fn infer_google_resource_id(user_text: &str) -> Option<String> {
    for marker in [
        "/document/d/",
        "/spreadsheets/d/",
        "/presentation/d/",
        "/file/d/",
        "/folders/",
        "id=",
    ] {
        if let Some(id) = extract_identifier_from_url_marker(user_text, marker) {
            return Some(id);
        }
    }

    if let Some(caps) = GENERIC_RESOURCE_ID_RE.captures(user_text) {
        if let Some(matched) = caps.get(1) {
            if let Some(id) = clean_identifier_candidate(matched.as_str()) {
                return Some(id);
            }
        }
    }

    if let Some(id) = extract_identifier_after_keyword(
        user_text,
        &[
            "file id",
            "file_id",
            "document id",
            "document_id",
            "spreadsheet id",
            "spreadsheet_id",
            "presentation id",
            "presentation_id",
            "id",
        ],
    ) {
        return Some(id);
    }

    if let Some(caps) = QUOTED_TEXT_RE.captures(user_text) {
        if let Some(matched) = caps.get(1).or_else(|| caps.get(2)) {
            let candidate = matched.as_str().trim();
            if candidate.len() >= 15 {
                return clean_identifier_candidate(candidate);
            }
        }
    }

    None
}

fn infer_gmail_message_id(user_text: &str) -> Option<String> {
    if let Some(caps) = GMAIL_MESSAGE_ID_RE.captures(user_text) {
        if let Some(matched) = caps.get(1) {
            if let Some(id) = clean_identifier_candidate(matched.as_str()) {
                return Some(id);
            }
        }
    }

    for marker in ["/#inbox/", "/#all/", "/#sent/"] {
        if let Some(id) = extract_identifier_from_url_marker(user_text, marker) {
            return Some(id);
        }
    }

    let lower = user_text.to_ascii_lowercase();
    if lower.contains("gmail") || lower.contains("email") || lower.contains("mail") {
        return extract_identifier_after_keyword(user_text, &["message id", "message_id", "id"]);
    }

    None
}

fn infer_calendar_event_id(user_text: &str) -> Option<String> {
    if let Some(caps) = CALENDAR_EVENT_ID_RE.captures(user_text) {
        if let Some(matched) = caps.get(1) {
            if let Some(id) = clean_identifier_candidate(matched.as_str()) {
                return Some(id);
            }
        }
    }

    let lower = user_text.to_ascii_lowercase();
    if lower.contains("calendar") || lower.contains("meeting") || lower.contains("event") {
        return extract_identifier_after_keyword(user_text, &["event id", "event_id", "id"]);
    }

    None
}

fn infer_docs_edit_text(user_text: &str) -> Option<String> {
    if let Some(caps) = QUOTED_TEXT_RE.captures(user_text) {
        if let Some(matched) = caps.get(1).or_else(|| caps.get(2)) {
            if let Some(text) = clean_content_candidate(matched.as_str()) {
                return Some(text);
            }
        }
    }

    if let Some(caps) = APPEND_TEXT_RE.captures(user_text) {
        if let Some(matched) = caps.get(1) {
            if let Some(text) = clean_content_candidate(matched.as_str()) {
                return Some(text);
            }
        }
    }

    None
}

fn infer_sheet_range(user_text: &str) -> Option<String> {
    let caps = SHEETS_RANGE_RE.captures(user_text)?;
    let matched = caps.get(1)?.as_str().trim();
    if matched.is_empty() {
        None
    } else {
        Some(matched.to_string())
    }
}

fn infer_sheet_single_value(user_text: &str) -> Option<String> {
    if let Some(caps) = QUOTED_TEXT_RE.captures(user_text) {
        if let Some(matched) = caps.get(1).or_else(|| caps.get(2)) {
            return clean_content_candidate(matched.as_str());
        }
    }

    let lower = user_text.to_ascii_lowercase();
    for marker in [" to ", " value is ", " value ", " as "] {
        if let Some(idx) = lower.rfind(marker) {
            let start = idx + marker.len();
            if let Some(rest) = user_text.get(start..) {
                let candidate = rest
                    .trim_start()
                    .split(|c: char| matches!(c, '\n' | '\r' | ',' | ';' | '!'))
                    .next()
                    .unwrap_or("")
                    .trim();
                if let Some(value) = clean_content_candidate(candidate) {
                    return Some(value);
                }
            }
        }
    }

    None
}

fn looks_like_send_confirmation_prompt(user_text: &str) -> bool {
    SEND_CONFIRMATION_RE.is_match(user_text.trim())
}

fn infer_confirmation_send_query_from_history(
    last_user_text: &str,
    messages: &[ChatMessage],
) -> Option<String> {
    if !looks_like_send_confirmation_prompt(last_user_text) {
        return None;
    }

    let mut to: Option<String> = None;
    let mut body: Option<String> = None;
    let mut subject: Option<String> = None;
    let mut skipped_current = false;

    for message in messages.iter().rev() {
        if message.role != "user" {
            continue;
        }

        if !skipped_current && message.content.trim() == last_user_text.trim() {
            skipped_current = true;
            continue;
        }

        if to.is_none() {
            to = CALENDAR_ATTENDEE_EMAIL_RE
                .captures(&message.content)
                .and_then(|caps| caps.get(1))
                .map(|m| m.as_str().trim().to_ascii_lowercase());
        }

        if body.is_none() {
            body = infer_gmail_send_body(&message.content);
        }

        if subject.is_none() {
            if let Some(existing_body) = body.as_deref() {
                subject = Some(infer_gmail_send_subject(&message.content, existing_body));
            }
        }

        if to.is_some() && body.is_some() {
            break;
        }
    }

    let to = to?;
    let body = body?;
    let subject = subject.unwrap_or_else(|| infer_gmail_send_subject(last_user_text, &body));
    let safe_body = body.replace('"', "'");
    let safe_subject = subject.replace('"', "'");

    Some(format!(
        "Send \"{}\" mail to {} subject \"{}\"",
        safe_body, to, safe_subject
    ))
}

fn resolve_intent_fallback_query(last_user_text: &str, messages: &[ChatMessage]) -> String {
    infer_confirmation_send_query_from_history(last_user_text, messages)
        .unwrap_or_else(|| last_user_text.trim().to_string())
}

// ─── App-lifecycle intent extractors ─────────────────────────────────────────

/// Extract the application name from utterances like "open chrome", "launch vscode", "start spotify".
fn extract_app_name_from_query(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let prefixes = ["open ", "launch ", "start ", "run "];
    for prefix in prefixes {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let name = rest
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_string();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

/// Extract a bare https?:// URL from the query text.
fn extract_url_from_query(text: &str) -> Option<String> {
    text.split_whitespace()
        .find(|w| w.starts_with("http://") || w.starts_with("https://"))
        .map(|s| s.trim_end_matches(&['.', ',', ')', ']', ';'][..]).to_string())
}

/// Extract (search_query, optional_site) from utterances like:
/// - "open Chrome and search for lo-fi music"
/// - "search YouTube for relaxing music"
/// - "play Shape of You on YouTube"
fn extract_browser_search_intent(text: &str) -> (String, Option<String>) {
    let lower = text.to_lowercase();

    // Detect site preference.
    let site: Option<String> = if lower.contains("youtube") || lower.contains(" yt ") {
        Some("youtube".into())
    } else {
        None
    };

    // Strip out the site/app name and leading verb phrases, leaving the actual query.
    let after_verb = ["search for ", "search ", "google ", "look up ", "find ", "play "]
        .iter()
        .find_map(|prefix| lower.find(prefix).map(|i| text[i + prefix.len()..].trim().to_string()));

    let query = after_verb.unwrap_or_else(|| {
        // Fallback: strip "open <app> and" prefix, take the rest.
        let s = lower
            .strip_prefix("open ")
            .and_then(|s| s.splitn(2, " and ").nth(1))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| text.trim().to_string());
        s
    });

    // Remove "on youtube", "on chrome", "in browser" suffixes.
    let suffixes = [" on youtube", " on chrome", " on firefox", " in browser", " on youtube.com", " in youtube"];
    let clean_query = suffixes
        .iter()
        .fold(query.to_lowercase(), |q, suf| {
            q.trim_end_matches(suf.trim()).trim().to_string()
        });

    let final_query = if clean_query.is_empty() { text.trim().to_string() } else { clean_query };
    (final_query, site)
}

/// Extract (app, contact_name, body) from utterances like:
/// - "WhatsApp Anjali 'are you free?'"
/// - "text Anjali hey"
/// - "send a WhatsApp to Anjali saying hello"
fn extract_send_message_intent(text: &str) -> (String, String, String) {
    let lower = text.to_lowercase();

    // Detect messaging app.
    let app = if lower.contains("telegram") {
        "telegram"
    } else if lower.contains("signal") {
        "signal"
    } else if lower.contains("gmail") || lower.contains("email") || lower.contains("mail") {
        "gmail"
    } else {
        "whatsapp" // default
    };

    // Find contact name — the first capitalised word after the verb / app name.
    // This is a best-effort heuristic; proper NLP lives in the LLM layer.
    let trigger_words = ["to ", "message ", "text ", "msg "];
    let contact_start = trigger_words
        .iter()
        .find_map(|tw| lower.find(tw).map(|i| i + tw.len()));

    let (contact, body_start_idx) = if let Some(start) = contact_start {
        let words: Vec<&str> = text[start..].split_whitespace().collect();
        let name = words.first().copied().unwrap_or("").to_string();
        let body_start = start + name.len() + 1;
        (name, body_start.min(text.len()))
    } else {
        (String::new(), text.len())
    };

    // Rest of the text after the contact name is the message body.
    let body_raw = text[body_start_idx..].trim();
    // Strip common connective words.
    let body = ["saying ", "say ", "with message ", "message "]
        .iter()
        .find_map(|prefix| body_raw.strip_prefix(prefix))
        .unwrap_or(body_raw)
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .to_string();

    (app.to_string(), contact, body)
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

fn looks_like_colab_request(text_lower: &str) -> bool {
    [
        "colab",
        "google colab",
        "notebook",
        "jupyter",
        "python notebook",
        "cell",
        "run code",
        "train model",
    ]
    .iter()
    .any(|needle| text_lower.contains(needle))
}

fn routing_focus_text_from_user_content(user_text: &str) -> String {
    const IMAGE_PROMPT_MARKER: &str = "\n\nImage attachment is already included for this turn.";

    if let Some((prefix, _)) = user_text.split_once(IMAGE_PROMPT_MARKER) {
        let trimmed = prefix.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    user_text.trim().to_string()
}

fn looks_like_pure_image_analysis_request(text_lower: &str) -> bool {
    let has_image_context = [
        "image",
        "photo",
        "picture",
        "screenshot",
        "screen",
        "scan",
    ]
    .iter()
    .any(|needle| text_lower.contains(needle));

    let has_analysis_intent = [
        "analy",
        "describe",
        "what is",
        "what's in",
        "identify",
        "detect",
        "read",
        "extract",
        "ocr",
        "summar",
    ]
    .iter()
    .any(|needle| text_lower.contains(needle));

    let has_non_image_action = [
        "gmail",
        "email",
        "calendar",
        "drive",
        "doc",
        "spreadsheet",
        "sheet",
        "slides",
        "form",
        "install",
        "uninstall",
        "delete",
        "remove",
        "rename",
        "move",
        "copy",
        "web search",
        "news",
        "git",
    ]
    .iter()
    .any(|needle| text_lower.contains(needle));

    has_image_context && has_analysis_intent && !has_non_image_action
}

fn is_tool_allowed_for_image_focus(def: &ToolDef) -> bool {
    if def.category.eq_ignore_ascii_case("vision") {
        return true;
    }

    let name = def.name.to_ascii_lowercase();
    name.contains("image")
        || name.contains("ocr")
        || name.contains("vision")
        || name == "screenshot_analyze"
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
        "gw_gmail_send" if allowed_tool_names.contains("gw_gmail_send") => {
            let args = infer_gmail_send_arguments(user_query)?;
            Some(ParsedToolCall {
                name: "gw_gmail_send".into(),
                arguments: args,
            })
        }
        "gw_gmail_read" if allowed_tool_names.contains("gw_gmail_read") => {
            let message_id = infer_gmail_message_id(user_query)?;
            Some(ParsedToolCall {
                name: "gw_gmail_read".into(),
                arguments: serde_json::json!({
                    "message_id": message_id,
                }),
            })
        }
        "gw_gmail_delete" if allowed_tool_names.contains("gw_gmail_delete") => {
            if let Some(message_id) = infer_gmail_message_id(user_query) {
                Some(ParsedToolCall {
                    name: "gw_gmail_delete".into(),
                    arguments: serde_json::json!({
                        "message_id": message_id,
                    }),
                })
            } else if allowed_tool_names.contains("gw_gmail_search") {
                Some(ParsedToolCall {
                    name: "gw_gmail_search".into(),
                    arguments: serde_json::json!({
                        "query": infer_gmail_search_query(user_query),
                        "max_results": 1,
                    }),
                })
            } else {
                None
            }
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
        "gw_calendar_delete" if allowed_tool_names.contains("gw_calendar_delete") => {
            if let Some(event_id) = infer_calendar_event_id(user_query) {
                Some(ParsedToolCall {
                    name: "gw_calendar_delete".into(),
                    arguments: serde_json::json!({
                        "event_id": event_id,
                    }),
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
        "gw_drive_read" if allowed_tool_names.contains("gw_drive_read") => {
            if let Some(file_id) = infer_google_resource_id(user_query) {
                Some(ParsedToolCall {
                    name: "gw_drive_read".into(),
                    arguments: serde_json::json!({
                        "file_id": file_id,
                    }),
                })
            } else if allowed_tool_names.contains("gw_drive_search") {
                Some(ParsedToolCall {
                    name: "gw_drive_search".into(),
                    arguments: serde_json::json!({
                        "query": user_query,
                    }),
                })
            } else {
                None
            }
        }
        "gw_drive_delete" if allowed_tool_names.contains("gw_drive_delete") => {
            if let Some(file_id) = infer_google_resource_id(user_query) {
                Some(ParsedToolCall {
                    name: "gw_drive_delete".into(),
                    arguments: serde_json::json!({
                        "file_id": file_id,
                    }),
                })
            } else if allowed_tool_names.contains("gw_drive_search") {
                Some(ParsedToolCall {
                    name: "gw_drive_search".into(),
                    arguments: serde_json::json!({
                        "query": user_query,
                    }),
                })
            } else {
                None
            }
        }
        "gw_docs_create" if allowed_tool_names.contains("gw_docs_create") => Some(ParsedToolCall {
            name: "gw_docs_create".into(),
            arguments: serde_json::json!({
                "title": infer_title(user_query, "Untitled Document"),
            }),
        }),
        "gw_docs_read" if allowed_tool_names.contains("gw_docs_read") => {
            if let Some(document_id) = infer_google_resource_id(user_query) {
                Some(ParsedToolCall {
                    name: "gw_docs_read".into(),
                    arguments: serde_json::json!({
                        "document_id": document_id,
                    }),
                })
            } else if allowed_tool_names.contains("gw_drive_search") {
                Some(ParsedToolCall {
                    name: "gw_drive_search".into(),
                    arguments: serde_json::json!({
                        "query": user_query,
                    }),
                })
            } else {
                None
            }
        }
        "gw_docs_edit" if allowed_tool_names.contains("gw_docs_edit") => {
            let text = infer_docs_edit_text(user_query)?;
            if let Some(document_id) = infer_google_resource_id(user_query) {
                Some(ParsedToolCall {
                    name: "gw_docs_edit".into(),
                    arguments: serde_json::json!({
                        "document_id": document_id,
                        "text": text,
                    }),
                })
            } else if allowed_tool_names.contains("gw_drive_search") {
                Some(ParsedToolCall {
                    name: "gw_drive_search".into(),
                    arguments: serde_json::json!({
                        "query": user_query,
                    }),
                })
            } else {
                None
            }
        }
        "gw_sheets_create" if allowed_tool_names.contains("gw_sheets_create") => {
            Some(ParsedToolCall {
                name: "gw_sheets_create".into(),
                arguments: serde_json::json!({
                    "title": infer_title(user_query, "Untitled Spreadsheet"),
                }),
            })
        }
        "gw_sheets_read" if allowed_tool_names.contains("gw_sheets_read") => {
            if let Some(spreadsheet_id) = infer_google_resource_id(user_query) {
                Some(ParsedToolCall {
                    name: "gw_sheets_read".into(),
                    arguments: serde_json::json!({
                        "spreadsheet_id": spreadsheet_id,
                    }),
                })
            } else if allowed_tool_names.contains("gw_drive_search") {
                Some(ParsedToolCall {
                    name: "gw_drive_search".into(),
                    arguments: serde_json::json!({
                        "query": user_query,
                    }),
                })
            } else {
                None
            }
        }
        "gw_sheets_edit" if allowed_tool_names.contains("gw_sheets_edit") => {
            let range = infer_sheet_range(user_query)?;
            let value = infer_sheet_single_value(user_query)?;
            let values = serde_json::to_string(&vec![vec![value]]).ok()?;

            if let Some(spreadsheet_id) = infer_google_resource_id(user_query) {
                Some(ParsedToolCall {
                    name: "gw_sheets_edit".into(),
                    arguments: serde_json::json!({
                        "spreadsheet_id": spreadsheet_id,
                        "range": range,
                        "values": values,
                    }),
                })
            } else if allowed_tool_names.contains("gw_drive_search") {
                Some(ParsedToolCall {
                    name: "gw_drive_search".into(),
                    arguments: serde_json::json!({
                        "query": user_query,
                    }),
                })
            } else {
                None
            }
        }
        "gw_slides_create" if allowed_tool_names.contains("gw_slides_create") => {
            Some(ParsedToolCall {
                name: "gw_slides_create".into(),
                arguments: serde_json::json!({
                    "title": infer_title(user_query, "Untitled Presentation"),
                }),
            })
        }
        "gw_slides_read" if allowed_tool_names.contains("gw_slides_read") => {
            if let Some(presentation_id) = infer_google_resource_id(user_query) {
                Some(ParsedToolCall {
                    name: "gw_slides_read".into(),
                    arguments: serde_json::json!({
                        "presentation_id": presentation_id,
                    }),
                })
            } else if allowed_tool_names.contains("gw_drive_search") {
                Some(ParsedToolCall {
                    name: "gw_drive_search".into(),
                    arguments: serde_json::json!({
                        "query": user_query,
                    }),
                })
            } else {
                None
            }
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
        // ── App lifecycle ─────────────────────────────────────────────────────

        "open_application" if allowed_tool_names.contains("open_application") => {
            // Extract the app name: "open <name>" / "launch <name>" / "start <name>"
            let app_name = extract_app_name_from_query(user_query).unwrap_or_else(|| user_query.trim().to_string());
            Some(ParsedToolCall {
                name: "open_application".into(),
                arguments: serde_json::json!({
                    "name": app_name,
                }),
            })
        }
        "open_url" if allowed_tool_names.contains("open_url") => {
            // Extract the first https?:// URL from the query.
            let url = extract_url_from_query(user_query).unwrap_or_else(|| user_query.trim().to_string());
            Some(ParsedToolCall {
                name: "open_url".into(),
                arguments: serde_json::json!({ "url": url }),
            })
        }
        "browser_search" if allowed_tool_names.contains("browser_search") => {
            // Extract the search query and optional site (youtube or default google).
            let (search_query, site) = extract_browser_search_intent(user_query);
            let mut args = serde_json::json!({ "query": search_query });
            if let Some(s) = site {
                args["site"] = serde_json::Value::String(s);
            }
            Some(ParsedToolCall {
                name: "browser_search".into(),
                arguments: args,
            })
        }
        // Fallback: if browser_search not registered but open_application is, open the browser.
        "browser_search" if allowed_tool_names.contains("open_application") => {
            let (search_query, site) = extract_browser_search_intent(user_query);
            let _ = site; // best-effort: just open the browser
            Some(ParsedToolCall {
                name: "open_application".into(),
                arguments: serde_json::json!({ "name": "browser", "query": search_query }),
            })
        }
        "send_message" if allowed_tool_names.contains("send_message") => {
            // Extract messaging app, contact name, and message body.
            // Note: contact_identifier intentionally left blank here — the LLM or
            // contact-resolution step must fill it in. If the tool receives an empty
            // identifier it will return an error asking for clarification.
            let (app, contact, body) = extract_send_message_intent(user_query);
            Some(ParsedToolCall {
                name: "send_message".into(),
                arguments: serde_json::json!({
                    "app": app,
                    "contact_name": contact,
                    "contact_identifier": "",  // must be resolved before dispatch
                    "body": body,
                }),
            })
        }
        // ── System info ──────────────────────────────────────────────────────
        "get_cpu_usage" if allowed_tool_names.contains("get_cpu_usage") => Some(ParsedToolCall {
            name: "get_cpu_usage".into(),
            arguments: serde_json::json!({}),
        }),
        "get_memory_info" if allowed_tool_names.contains("get_memory_info") => Some(ParsedToolCall {
            name: "get_memory_info".into(),
            arguments: serde_json::json!({}),
        }),
        "get_disk_space" if allowed_tool_names.contains("get_disk_space") => Some(ParsedToolCall {
            name: "get_disk_space".into(),
            arguments: serde_json::json!({}),
        }),
        "get_network_status" if allowed_tool_names.contains("get_network_status") => Some(ParsedToolCall {
            name: "get_network_status".into(),
            arguments: serde_json::json!({}),
        }),
        "get_battery_status" if allowed_tool_names.contains("get_battery_status") => Some(ParsedToolCall {
            name: "get_battery_status".into(),
            arguments: serde_json::json!({}),
        }),
        "get_gpu_info" if allowed_tool_names.contains("get_gpu_info") => Some(ParsedToolCall {
            name: "get_gpu_info".into(),
            arguments: serde_json::json!({}),
        }),
        "get_system_uptime" if allowed_tool_names.contains("get_system_uptime") => Some(ParsedToolCall {
            name: "get_system_uptime".into(),
            arguments: serde_json::json!({}),
        }),
        "check_system_health" if allowed_tool_names.contains("check_system_health") => Some(ParsedToolCall {
            name: "check_system_health".into(),
            arguments: serde_json::json!({}),
        }),
        // ── Alerts ───────────────────────────────────────────────────────────
        "get_alerts" if allowed_tool_names.contains("get_alerts") => Some(ParsedToolCall {
            name: "get_alerts".into(),
            arguments: serde_json::json!({ "include_dismissed": false }),
        }),
        "dismiss_alert" if allowed_tool_names.contains("dismiss_alert") => {
            // Extract alert ID: "Dismiss alert ID sys-thermal-001"
            let id = user_query
                .split_whitespace()
                .filter(|w| {
                    let lw = w.to_lowercase();
                    !["dismiss", "alert", "the", "id", "with", "named"].contains(&lw.as_str())
                })
                .find(|w| w.len() >= 3)
                .unwrap_or("")
                .trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
                .to_string();
            if id.is_empty() {
                return None;
            }
            Some(ParsedToolCall {
                name: "dismiss_alert".into(),
                arguments: serde_json::json!({ "id": id }),
            })
        }
        "watch_directory" if allowed_tool_names.contains("watch_directory") => {
            let path = user_query
                .split_whitespace()
                .find(|w| w.starts_with('/') || w.starts_with("~/"))
                .unwrap_or("/home/obaid/Downloads");
            Some(ParsedToolCall {
                name: "watch_directory".into(),
                arguments: serde_json::json!({ "path": path }),
            })
        }
        "list_watched_dirs" if allowed_tool_names.contains("list_watched_dirs") => Some(ParsedToolCall {
            name: "list_watched_dirs".into(),
            arguments: serde_json::json!({}),
        }),
        "smart_suggest" if allowed_tool_names.contains("smart_suggest") => Some(ParsedToolCall {
            name: "smart_suggest".into(),
            arguments: serde_json::json!({ "context": user_query }),
        }),
        // ── Power ─────────────────────────────────────────────────────────────
        "lock_screen" if allowed_tool_names.contains("lock_screen") => Some(ParsedToolCall {
            name: "lock_screen".into(),
            arguments: serde_json::json!({}),
        }),
        "sleep" if allowed_tool_names.contains("sleep") => Some(ParsedToolCall {
            name: "sleep".into(),
            arguments: serde_json::json!({}),
        }),
        "hibernate" if allowed_tool_names.contains("hibernate") => Some(ParsedToolCall {
            name: "hibernate".into(),
            arguments: serde_json::json!({}),
        }),
        "shutdown_system" if allowed_tool_names.contains("shutdown_system") => {
            let delay = lower
                .split_whitespace()
                .zip(lower.split_whitespace().skip(1))
                .find_map(|(a, b)| {
                    if ["minute", "minutes", "min"].contains(&b.trim_end_matches('.')) {
                        a.parse::<u64>().ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            Some(ParsedToolCall {
                name: "shutdown_system".into(),
                arguments: serde_json::json!({ "delay_minutes": delay }),
            })
        }
        "reboot_system" if allowed_tool_names.contains("reboot_system") => Some(ParsedToolCall {
            name: "reboot_system".into(),
            arguments: serde_json::json!({}),
        }),
        // ── System config ─────────────────────────────────────────────────────
        "get_power_plan" if allowed_tool_names.contains("get_power_plan") => Some(ParsedToolCall {
            name: "get_power_plan".into(),
            arguments: serde_json::json!({}),
        }),
        "set_power_plan" if allowed_tool_names.contains("set_power_plan") => {
            let plan = if lower.contains("power-saver") || lower.contains("power saver") || lower.contains("powersave") {
                "power-saver"
            } else if lower.contains("performance") {
                "performance"
            } else {
                "balanced"
            };
            Some(ParsedToolCall {
                name: "set_power_plan".into(),
                arguments: serde_json::json!({ "plan": plan }),
            })
        }
        "set_volume" if allowed_tool_names.contains("set_volume") => {
            let is_mute = lower.contains("band") || lower.contains("mute") || lower.contains("zero");
            let level: u64 = if is_mute {
                0
            } else {
                lower
                    .split_whitespace()
                    .find_map(|w| {
                        // Strip trailing % before parsing so "100%" → 100
                        w.trim_end_matches('%').parse::<u64>().ok().filter(|&n| n <= 100)
                    })
                    .unwrap_or(50)
            };
            Some(ParsedToolCall {
                name: "set_volume".into(),
                arguments: serde_json::json!({ "level": level }),
            })
        }
        "set_brightness" if allowed_tool_names.contains("set_brightness") => {
            let level: u64 = lower
                .split_whitespace()
                .find_map(|w| {
                    // Strip trailing % before parsing so "80%" → 80
                    w.trim_end_matches('%').parse::<u64>().ok().filter(|&n| n <= 100)
                })
                .unwrap_or(50);
            Some(ParsedToolCall {
                name: "set_brightness".into(),
                arguments: serde_json::json!({ "level": level }),
            })
        }
        "toggle_wifi" if allowed_tool_names.contains("toggle_wifi") => {
            let enable = !(lower.contains(" off") || lower.contains("disable") || lower.contains("turn off") || lower.contains("band "));
            Some(ParsedToolCall {
                name: "toggle_wifi".into(),
                arguments: serde_json::json!({ "enable": enable }),
            })
        }
        "get_wifi_networks" if allowed_tool_names.contains("get_wifi_networks") => Some(ParsedToolCall {
            name: "get_wifi_networks".into(),
            arguments: serde_json::json!({}),
        }),
        "get_environment_variable" if allowed_tool_names.contains("get_environment_variable") => {
            let name = lower.split_whitespace().last().unwrap_or("HOME").to_uppercase();
            Some(ParsedToolCall {
                name: "get_environment_variable".into(),
                arguments: serde_json::json!({ "name": name }),
            })
        }
        "list_environment_variables" if allowed_tool_names.contains("list_environment_variables") => Some(ParsedToolCall {
            name: "list_environment_variables".into(),
            arguments: serde_json::json!({}),
        }),
        // ── Process / service ────────────────────────────────────────────────
        "list_running_apps" if allowed_tool_names.contains("list_running_apps") => Some(ParsedToolCall {
            name: "list_running_apps".into(),
            arguments: serde_json::json!({}),
        }),
        "close_application" if allowed_tool_names.contains("close_application") => {
            let name = extract_app_name_from_query(user_query)
                .unwrap_or_else(|| user_query.trim().to_string());
            Some(ParsedToolCall {
                name: "close_application".into(),
                arguments: serde_json::json!({ "name": name }),
            })
        }
        "kill_process" if allowed_tool_names.contains("kill_process") => {
            let pid = lower.split_whitespace().find_map(|w| w.parse::<u64>().ok());
            let Some(pid) = pid else {
                return None;
            };
            Some(ParsedToolCall {
                name: "kill_process".into(),
                arguments: serde_json::json!({ "pid": pid }),
            })
        }
        "manage_service" if allowed_tool_names.contains("manage_service") => {
            let action = if lower.contains("start") {
                "start"
            } else if lower.contains("stop") {
                "stop"
            } else if lower.contains("restart") {
                "restart"
            } else {
                "status"
            };
            let skip_words = ["start", "stop", "restart", "status", "service", "check", "the", "of", "manage", "my"];
            let service = lower
                .split_whitespace()
                .find(|w| !skip_words.contains(w))
                .unwrap_or("docker")
                .to_string();
            Some(ParsedToolCall {
                name: "manage_service".into(),
                arguments: serde_json::json!({ "name": service, "action": action }),
            })
        }
        "get_active_connections" if allowed_tool_names.contains("get_active_connections") => Some(ParsedToolCall {
            name: "get_active_connections".into(),
            arguments: serde_json::json!({}),
        }),
        "focus_window" if allowed_tool_names.contains("focus_window") => {
            let title = extract_app_name_from_query(user_query)
                .unwrap_or_else(|| user_query.trim().to_string());
            Some(ParsedToolCall {
                name: "focus_window".into(),
                arguments: serde_json::json!({ "title": title }),
            })
        }
        // ── Desktop / interaction ─────────────────────────────────────────────
        "screenshot" if allowed_tool_names.contains("screenshot") => Some(ParsedToolCall {
            name: "screenshot".into(),
            arguments: serde_json::json!({}),
        }),
        "screenshot_analyze" if allowed_tool_names.contains("screenshot_analyze") => Some(ParsedToolCall {
            name: "screenshot_analyze".into(),
            arguments: serde_json::json!({}),
        }),
        "get_clipboard" if allowed_tool_names.contains("get_clipboard") => Some(ParsedToolCall {
            name: "get_clipboard".into(),
            arguments: serde_json::json!({}),
        }),
        "set_clipboard" if allowed_tool_names.contains("set_clipboard") => {
            let text = QUOTED_TEXT_RE
                .captures(user_query)
                .and_then(|c| c.get(1).or_else(|| c.get(2)))
                .map(|m| m.as_str().to_string())
                .unwrap_or_else(|| user_query.trim().to_string());
            Some(ParsedToolCall {
                name: "set_clipboard".into(),
                arguments: serde_json::json!({ "text": text }),
            })
        }
        "transform_clipboard" if allowed_tool_names.contains("transform_clipboard") => {
            let transform = if lower.contains("upper") {
                "uppercase"
            } else if lower.contains("lower") {
                "lowercase"
            } else {
                "uppercase"
            };
            Some(ParsedToolCall {
                name: "transform_clipboard".into(),
                arguments: serde_json::json!({ "transform": transform }),
            })
        }
        "type_text" if allowed_tool_names.contains("type_text") => {
            let text = QUOTED_TEXT_RE
                .captures(user_query)
                .and_then(|c| c.get(1).or_else(|| c.get(2)))
                .map(|m| m.as_str().to_string())
                .unwrap_or_else(|| user_query.trim().to_string());
            Some(ParsedToolCall {
                name: "type_text".into(),
                arguments: serde_json::json!({ "text": text }),
            })
        }
        "get_active_window" if allowed_tool_names.contains("get_active_window") => Some(ParsedToolCall {
            name: "get_active_window".into(),
            arguments: serde_json::json!({}),
        }),
        "list_windows" if allowed_tool_names.contains("list_windows") => Some(ParsedToolCall {
            name: "list_windows".into(),
            arguments: serde_json::json!({}),
        }),
        "maximize_window" if allowed_tool_names.contains("maximize_window") => {
            let title = extract_app_name_from_query(user_query)
                .unwrap_or_else(|| user_query.trim().to_string());
            Some(ParsedToolCall {
                name: "maximize_window".into(),
                arguments: serde_json::json!({ "title": title }),
            })
        }
        "minimize_window" if allowed_tool_names.contains("minimize_window") => {
            let title = extract_app_name_from_query(user_query)
                .unwrap_or_else(|| user_query.trim().to_string());
            Some(ParsedToolCall {
                name: "minimize_window".into(),
                arguments: serde_json::json!({ "title": title }),
            })
        }
        // ── Communication ─────────────────────────────────────────────────────
        "send_notification" if allowed_tool_names.contains("send_notification") => {
            let body = QUOTED_TEXT_RE
                .captures(user_query)
                .and_then(|c| c.get(1).or_else(|| c.get(2)))
                .map(|m| m.as_str().to_string())
                .unwrap_or_else(|| {
                    ["notification: ", "notify: ", "send notification "]
                        .iter()
                        .find_map(|m| lower.find(m).map(|i| user_query[i + m.len()..].trim().to_string()))
                        .unwrap_or_else(|| user_query.trim().to_string())
                });
            Some(ParsedToolCall {
                name: "send_notification".into(),
                arguments: serde_json::json!({ "title": "KRIA", "body": body }),
            })
        }
        "schedule_reminder" if allowed_tool_names.contains("schedule_reminder") => {
            let delay: u64 = lower
                .split_whitespace()
                .zip(lower.split_whitespace().skip(1))
                .find_map(|(a, b)| {
                    if ["minute", "minutes", "min"].contains(&b.trim_end_matches('.')) {
                        a.parse::<u64>().ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(15);
            let message = QUOTED_TEXT_RE
                .captures(user_query)
                .and_then(|c| c.get(1).or_else(|| c.get(2)))
                .map(|m| m.as_str().to_string())
                .unwrap_or_else(|| {
                    ["remind me to ", "reminder: "]
                        .iter()
                        .find_map(|m| lower.find(m).map(|i| user_query[i + m.len()..].trim().to_string()))
                        .unwrap_or_else(|| user_query.trim().to_string())
                });
            Some(ParsedToolCall {
                name: "schedule_reminder".into(),
                arguments: serde_json::json!({ "message": message, "delay_minutes": delay }),
            })
        }
        "compose_email" if allowed_tool_names.contains("compose_email") => {
            let to = lower
                .split_whitespace()
                .find(|w| w.contains('@'))
                .unwrap_or("")
                .to_string();
            Some(ParsedToolCall {
                name: "compose_email".into(),
                arguments: serde_json::json!({ "to": to, "subject": "", "body": "" }),
            })
        }
        // ── Knowledge / memory ────────────────────────────────────────────────
        "remember_fact" if allowed_tool_names.contains("remember_fact") => {
            let (key, value) = if let Some(pos) = lower.find(" is ") {
                let k = user_query[..pos].split_whitespace().last().unwrap_or("note").to_string();
                let v = user_query[pos + 4..].trim().to_string();
                (k, v)
            } else {
                ("note".to_string(), user_query.trim().to_string())
            };
            Some(ParsedToolCall {
                name: "remember_fact".into(),
                arguments: serde_json::json!({ "key": key, "value": value }),
            })
        }
        "recall_fact" if allowed_tool_names.contains("recall_fact") => Some(ParsedToolCall {
            name: "recall_fact".into(),
            arguments: serde_json::json!({ "query": user_query }),
        }),
        "search_knowledge" if allowed_tool_names.contains("search_knowledge") => Some(ParsedToolCall {
            name: "search_knowledge".into(),
            arguments: serde_json::json!({ "query": user_query, "max_results": 5 }),
        }),
        "list_remembered" if allowed_tool_names.contains("list_remembered") => Some(ParsedToolCall {
            name: "list_remembered".into(),
            arguments: serde_json::json!({}),
        }),
        "save_snippet" if allowed_tool_names.contains("save_snippet") => {
            let name = QUOTED_TEXT_RE
                .captures(user_query)
                .and_then(|c| c.get(1).or_else(|| c.get(2)))
                .map(|m| m.as_str().to_string())
                .unwrap_or_else(|| "snippet".to_string());
            Some(ParsedToolCall {
                name: "save_snippet".into(),
                arguments: serde_json::json!({ "name": name, "content": "", "language": "text" }),
            })
        }
        "get_snippet" if allowed_tool_names.contains("get_snippet") => {
            let name = QUOTED_TEXT_RE
                .captures(user_query)
                .and_then(|c| c.get(1).or_else(|| c.get(2)))
                .map(|m| m.as_str().to_string())
                .unwrap_or_else(|| lower.split_whitespace().last().unwrap_or("").to_string());
            Some(ParsedToolCall {
                name: "get_snippet".into(),
                arguments: serde_json::json!({ "name": name }),
            })
        }
        "list_snippets" if allowed_tool_names.contains("list_snippets") => Some(ParsedToolCall {
            name: "list_snippets".into(),
            arguments: serde_json::json!({}),
        }),
        // ── Network / internet ────────────────────────────────────────────────
        "get_public_ip" if allowed_tool_names.contains("get_public_ip") => Some(ParsedToolCall {
            name: "get_public_ip".into(),
            arguments: serde_json::json!({}),
        }),
        "ping_host" if allowed_tool_names.contains("ping_host") => {
            // Extract host — default to google.com for connectivity checks
            let host = lower
                .split_whitespace()
                .find(|w| {
                    (w.contains('.') || w.parse::<std::net::IpAddr>().is_ok())
                        && !w.starts_with('/')
                        && !w.contains('@')
                        && !["internet", "online", "network", "check"].contains(w)
                })
                .unwrap_or("google.com")
                .trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '-')
                .to_string();
            Some(ParsedToolCall {
                name: "ping_host".into(),
                arguments: serde_json::json!({ "host": host }),
            })
        }
        "speed_test" if allowed_tool_names.contains("speed_test") => Some(ParsedToolCall {
            name: "speed_test".into(),
            arguments: serde_json::json!({}),
        }),
        "dns_lookup" if allowed_tool_names.contains("dns_lookup") => {
            let domain = user_query
                .split_whitespace()
                .find(|w| w.contains('.') && !w.starts_with('/'))
                .unwrap_or("google.com")
                .to_string();
            Some(ParsedToolCall {
                name: "dns_lookup".into(),
                arguments: serde_json::json!({ "domain": domain }),
            })
        }
        "fetch_webpage" if allowed_tool_names.contains("fetch_webpage") => {
            // Extract the first http/https URL from the user query
            let url = user_query
                .split_whitespace()
                .find(|w| w.starts_with("http"))
                .map(|w| w.trim_end_matches(|c: char| c == '.' || c == ',' || c == '\'' || c == ')'))
                .unwrap_or("")
                .to_string();
            if url.is_empty() {
                return None;
            }
            Some(ParsedToolCall {
                name: "fetch_webpage".into(),
                arguments: serde_json::json!({ "url": url }),
            })
        }
        "check_url_status" if allowed_tool_names.contains("check_url_status") => {
            let url = user_query
                .split_whitespace()
                .find(|w| w.starts_with("http"))
                .unwrap_or("")
                .to_string();
            if url.is_empty() {
                return None;
            }
            Some(ParsedToolCall {
                name: "check_url_status".into(),
                arguments: serde_json::json!({ "url": url }),
            })
        }
        "download_file" if allowed_tool_names.contains("download_file") => {
            let url = user_query
                .split_whitespace()
                .find(|w| w.starts_with("http"))
                .unwrap_or("")
                .to_string();
            if url.is_empty() {
                return None;
            }
            Some(ParsedToolCall {
                name: "download_file".into(),
                arguments: serde_json::json!({ "url": url, "destination": "/home/obaid/Downloads/" }),
            })
        }
        "get_current_time" if allowed_tool_names.contains("get_current_time") => Some(ParsedToolCall {
            name: "get_current_time".into(),
            arguments: serde_json::json!({}),
        }),
        "get_weather" if allowed_tool_names.contains("get_weather") => Some(ParsedToolCall {
            name: "get_weather".into(),
            arguments: serde_json::json!({}),
        }),
        // ── Developer / git ───────────────────────────────────────────────────
        "git_status" if allowed_tool_names.contains("git_status") => Some(ParsedToolCall {
            name: "git_status".into(),
            arguments: serde_json::json!({ "path": infer_git_path(user_query) }),
        }),
        "git_log" if allowed_tool_names.contains("git_log") => {
            let count = lower
                .split_whitespace()
                .find_map(|w| w.parse::<u64>().ok().filter(|&n| n > 0 && n <= 200))
                .unwrap_or(10);
            Some(ParsedToolCall {
                name: "git_log".into(),
                arguments: serde_json::json!({ "path": infer_git_path(user_query), "count": count }),
            })
        }
        "git_diff" if allowed_tool_names.contains("git_diff") => Some(ParsedToolCall {
            name: "git_diff".into(),
            arguments: serde_json::json!({ "path": infer_git_path(user_query) }),
        }),
        "git_stash" if allowed_tool_names.contains("git_stash") => Some(ParsedToolCall {
            name: "git_stash".into(),
            arguments: serde_json::json!({ "path": infer_git_path(user_query) }),
        }),
        "git_branch_list" if allowed_tool_names.contains("git_branch_list") => Some(ParsedToolCall {
            name: "git_branch_list".into(),
            arguments: serde_json::json!({ "path": infer_git_path(user_query) }),
        }),
        "git_commit" if allowed_tool_names.contains("git_commit") => {
            let message = QUOTED_TEXT_RE
                .captures(user_query)
                .and_then(|c| c.get(1).or_else(|| c.get(2)))
                .map(|m| m.as_str().to_string())
                .unwrap_or_else(|| "chore: update".to_string());
            Some(ParsedToolCall {
                name: "git_commit".into(),
                arguments: serde_json::json!({ "path": infer_git_path(user_query), "message": message }),
            })
        }
        "git_checkout" if allowed_tool_names.contains("git_checkout") => {
            let branch = QUOTED_TEXT_RE
                .captures(user_query)
                .and_then(|c| c.get(1).or_else(|| c.get(2)))
                .map(|m| m.as_str().to_string())
                .unwrap_or_else(|| lower.split_whitespace().last().unwrap_or("main").to_string());
            Some(ParsedToolCall {
                name: "git_checkout".into(),
                arguments: serde_json::json!({ "path": infer_git_path(user_query), "branch": branch }),
            })
        }
        "analyze_project" if allowed_tool_names.contains("analyze_project") => {
            let path = user_query
                .split_whitespace()
                .find(|w| w.starts_with('/') || w.starts_with("./"))
                .unwrap_or("/media/obaid/SSD/KRIA")
                .to_string();
            Some(ParsedToolCall {
                name: "analyze_project".into(),
                arguments: serde_json::json!({ "path": path }),
            })
        }
        // ── Package management ────────────────────────────────────────────────
        // install_package and the legacy install_application hint both route here
        "install_package" | "install_application"
            if allowed_tool_names.contains("install_package") =>
        {
            let pkg = extract_package_query(user_query, PackageIntent::Install)?;
            Some(ParsedToolCall {
                name: "install_package".into(),
                arguments: serde_json::json!({ "name": normalize_package_query(&pkg) }),
            })
        }
        "uninstall_package" | "uninstall_application"
            if allowed_tool_names.contains("uninstall_package") =>
        {
            let pkg = extract_package_query(user_query, PackageIntent::Uninstall)?;
            Some(ParsedToolCall {
                name: "uninstall_package".into(),
                arguments: serde_json::json!({ "name": normalize_package_query(&pkg) }),
            })
        }
        "search_package" if allowed_tool_names.contains("search_package") => {
            let query = lower.split_whitespace().last().unwrap_or("").to_string();
            if query.is_empty() {
                return None;
            }
            Some(ParsedToolCall {
                name: "search_package".into(),
                arguments: serde_json::json!({ "query": query }),
            })
        }
        "check_package_installed" if allowed_tool_names.contains("check_package_installed") => {
            let pkg = lower
                .split_whitespace()
                .last()
                .unwrap_or("")
                .to_string();
            if pkg.is_empty() {
                return None;
            }
            Some(ParsedToolCall {
                name: "check_package_installed".into(),
                arguments: serde_json::json!({ "name": pkg }),
            })
        }
        "check_package_updates" if allowed_tool_names.contains("check_package_updates") => {
            let pkg = lower.split_whitespace().last().unwrap_or("").to_string();
            Some(ParsedToolCall {
                name: "check_package_updates".into(),
                arguments: serde_json::json!({ "name": pkg }),
            })
        }
        "get_package_info" if allowed_tool_names.contains("get_package_info") => {
            let pkg = lower.split_whitespace().last().unwrap_or("").to_string();
            Some(ParsedToolCall {
                name: "get_package_info".into(),
                arguments: serde_json::json!({ "name": pkg }),
            })
        }
        // ── Shell execution ────────────────────────────────────────────────────
        "execute_bash" if allowed_tool_names.contains("execute_bash") => {
            let command = ["run: ", "execute: ", "bash: ", "command: ", "run bash: ", "execute bash: ", "run: bash "]
                .iter()
                .find_map(|m| lower.find(m).map(|i| user_query[i + m.len()..].trim().to_string()))
                .or_else(|| {
                    QUOTED_TEXT_RE
                        .captures(user_query)
                        .and_then(|c| c.get(1).or_else(|| c.get(2)))
                        .map(|m| m.as_str().to_string())
                })
                .unwrap_or_else(|| user_query.trim().to_string());
            Some(ParsedToolCall {
                name: "execute_bash".into(),
                arguments: serde_json::json!({ "command": command }),
            })
        }
        "execute_python" if allowed_tool_names.contains("execute_python") => {
            let code = ["python: ", "execute python: ", "run python: ", "python code: "]
                .iter()
                .find_map(|m| lower.find(m).map(|i| user_query[i + m.len()..].trim().to_string()))
                .or_else(|| {
                    FENCED_CODE_BLOCK_RE
                        .captures(user_query)
                        .and_then(|c| c.get(1))
                        .map(|m| m.as_str().trim().to_string())
                })
                .unwrap_or_else(|| user_query.trim().to_string());
            Some(ParsedToolCall {
                name: "execute_python".into(),
                arguments: serde_json::json!({ "code": code }),
            })
        }
        // ── File operations ───────────────────────────────────────────────────
        "read_file" if allowed_tool_names.contains("read_file") => {
            let path = user_query
                .split_whitespace()
                .find(|w| w.starts_with('/') || w.starts_with("~/"))
                .unwrap_or("")
                .to_string();
            if path.is_empty() {
                return None;
            }
            Some(ParsedToolCall {
                name: "read_file".into(),
                arguments: serde_json::json!({ "path": path }),
            })
        }
        "list_directory" if allowed_tool_names.contains("list_directory") => {
            let path = user_query
                .split_whitespace()
                .find(|w| w.starts_with('/') || w.starts_with("~/"))
                .unwrap_or("/home/obaid")
                .to_string();
            Some(ParsedToolCall {
                name: "list_directory".into(),
                arguments: serde_json::json!({ "path": path }),
            })
        }
        "get_project_structure" if allowed_tool_names.contains("get_project_structure") => {
            let path = user_query
                .split_whitespace()
                .find(|w| w.starts_with('/') || w.starts_with("./"))
                .unwrap_or(".")
                .to_string();
            Some(ParsedToolCall {
                name: "get_project_structure".into(),
                arguments: serde_json::json!({ "path": path, "max_depth": 3 }),
            })
        }
        "count_lines_of_code" if allowed_tool_names.contains("count_lines_of_code") => {
            let path = user_query
                .split_whitespace()
                .find(|w| w.starts_with('/') || w.starts_with("./"))
                .unwrap_or(".")
                .to_string();
            Some(ParsedToolCall {
                name: "count_lines_of_code".into(),
                arguments: serde_json::json!({ "path": path }),
            })
        }
        "find_todos" if allowed_tool_names.contains("find_todos") => {
            let path = user_query
                .split_whitespace()
                .find(|w| w.starts_with('/') || w.starts_with("./"))
                .unwrap_or(".")
                .to_string();
            Some(ParsedToolCall {
                name: "find_todos".into(),
                arguments: serde_json::json!({ "directory": path }),
            })
        }
        "calculate_dir_size" if allowed_tool_names.contains("calculate_dir_size") => {
            let path = user_query
                .split_whitespace()
                .find(|w| w.starts_with('/') || w.starts_with("~/"))
                .unwrap_or("/home/obaid")
                .to_string();
            Some(ParsedToolCall {
                name: "calculate_dir_size".into(),
                arguments: serde_json::json!({ "path": path }),
            })
        }
        "get_file_info" if allowed_tool_names.contains("get_file_info") => {
            let path = user_query
                .split_whitespace()
                .find(|w| w.starts_with('/') || w.starts_with("~/"))
                .unwrap_or("")
                .to_string();
            if path.is_empty() {
                return None;
            }
            Some(ParsedToolCall {
                name: "get_file_info".into(),
                arguments: serde_json::json!({ "path": path }),
            })
        }
        "delete_file" if allowed_tool_names.contains("delete_file") => {
            let path = user_query
                .split_whitespace()
                .find(|w| w.starts_with('/') || w.starts_with("~/"))
                .unwrap_or("")
                .to_string();
            if path.is_empty() {
                return None;
            }
            Some(ParsedToolCall {
                name: "delete_file".into(),
                arguments: serde_json::json!({ "path": path }),
            })
        }
        "delete_directory" if allowed_tool_names.contains("delete_directory") => {
            let path = user_query
                .split_whitespace()
                .find(|w| w.starts_with('/') || w.starts_with("~/"))
                .unwrap_or("")
                .to_string();
            if path.is_empty() {
                return None;
            }
            Some(ParsedToolCall {
                name: "delete_directory".into(),
                arguments: serde_json::json!({ "path": path, "recursive": true }),
            })
        }
        "clean_temp_files" if allowed_tool_names.contains("clean_temp_files") => {
            let days: u64 = lower
                .split_whitespace()
                .find_map(|w| w.parse::<u64>().ok())
                .unwrap_or(7);
            Some(ParsedToolCall {
                name: "clean_temp_files".into(),
                arguments: serde_json::json!({ "older_than_days": days }),
            })
        }
        // ── Vision ────────────────────────────────────────────────────────────
        "ocr_image" if allowed_tool_names.contains("ocr_image") => {
            let path = user_query
                .split_whitespace()
                .find(|w| w.starts_with('/') || w.starts_with("~/"))
                .unwrap_or("")
                .to_string();
            if path.is_empty() {
                return None;
            }
            Some(ParsedToolCall {
                name: "ocr_image".into(),
                arguments: serde_json::json!({ "path": path }),
            })
        }
        "analyze_image" if allowed_tool_names.contains("analyze_image") => {
            let path = user_query
                .split_whitespace()
                .find(|w| w.starts_with('/') || w.starts_with("~/"))
                .unwrap_or("")
                .to_string();
            if path.is_empty() {
                return None;
            }
            Some(ParsedToolCall {
                name: "analyze_image".into(),
                arguments: serde_json::json!({ "path": path, "operations": ["describe"] }),
            })
        }
        // ── Scheduler ─────────────────────────────────────────────────────────
        "list_scheduled_tasks" if allowed_tool_names.contains("list_scheduled_tasks") => Some(ParsedToolCall {
            name: "list_scheduled_tasks".into(),
            arguments: serde_json::json!({}),
        }),
        // ── I18N ──────────────────────────────────────────────────────────────
        "list_languages" if allowed_tool_names.contains("list_languages") => Some(ParsedToolCall {
            name: "list_languages".into(),
            arguments: serde_json::json!({}),
        }),
        "detect_language" if allowed_tool_names.contains("detect_language") => {
            let text = QUOTED_TEXT_RE
                .captures(user_query)
                .and_then(|c| c.get(1).or_else(|| c.get(2)))
                .map(|m| m.as_str().to_string())
                .unwrap_or_else(|| user_query.trim().to_string());
            Some(ParsedToolCall {
                name: "detect_language".into(),
                arguments: serde_json::json!({ "text": text }),
            })
        }
        "get_accessibility_settings" if allowed_tool_names.contains("get_accessibility_settings") => Some(ParsedToolCall {
            name: "get_accessibility_settings".into(),
            arguments: serde_json::json!({}),
        }),
        // ── Image generation ──────────────────────────────────────────────────
        "generate_image" if allowed_tool_names.contains("generate_image") => {
            // Strip leading imperative verbs so only the subject description remains.
            let prompt = {
                let trimmed = user_query.trim();
                // Remove common prefixes: "generate an image of", "draw a photo of", etc.
                let without_prefix = regex::Regex::new(
                    r"(?i)^(generate|create|make|draw|paint|design|render|produce)\s+(me\s+)?(a\s+|an\s+|one\s+)?(image|picture|photo|artwork|art|illustration|wallpaper|poster|banner|thumbnail)\s+(of\s+|showing\s+|depicting\s+)?",
                ).ok()
                    .and_then(|re| re.find(trimmed).map(|m| trimmed[m.end()..].trim().to_string()))
                    .unwrap_or_else(|| trimmed.to_string());
                if without_prefix.is_empty() { trimmed.to_string() } else { without_prefix }
            };
            Some(ParsedToolCall {
                name: "generate_image".into(),
                arguments: serde_json::json!({ "prompt": prompt, "force_cloud": true }),
            })
        }
        _ => None,
    }
}

/// Infer a git repository path from user query text.
/// Falls back to the KRIA workspace root.
fn infer_git_path(user_query: &str) -> String {
    user_query
        .split_whitespace()
        .find(|w| w.starts_with('/') || w.starts_with("./") || w.starts_with("~/"))
        .unwrap_or("/media/obaid/SSD/KRIA")
        .to_string()
}

/// Build multiple intent-fallback tool calls for prompts that require parallel tools.
///
/// Handles multi-tool scenarios (e.g. "system stats" → CPU + memory + disk) that
/// `build_intent_fallback_tool_call` cannot express as a single call.
/// Falls back to the single-call function for everything else.
fn build_multi_intent_fallback_calls(
    user_text: &str,
    allowed_tool_names: &HashSet<String>,
) -> Vec<ParsedToolCall> {
    let lower = user_text.to_lowercase();

    // ── System stats: fire CPU + memory + disk in one round ──────────────────
    let is_system_stats = lower.contains("system stat")
        || lower.contains("system status")
        || lower.contains("mera system stat")
        || (lower.contains("stat") && lower.contains("system"))
        || lower.contains("system vitals");

    if is_system_stats {
        let mut calls = Vec::new();
        if allowed_tool_names.contains("get_cpu_usage") {
            calls.push(ParsedToolCall {
                name: "get_cpu_usage".into(),
                arguments: serde_json::json!({}),
            });
        }
        if allowed_tool_names.contains("get_memory_info") {
            calls.push(ParsedToolCall {
                name: "get_memory_info".into(),
                arguments: serde_json::json!({}),
            });
        }
        if allowed_tool_names.contains("get_disk_space") {
            calls.push(ParsedToolCall {
                name: "get_disk_space".into(),
                arguments: serde_json::json!({}),
            });
        }
        if !calls.is_empty() {
            return calls;
        }
    }

    // ── Internet connectivity: 3-host balanced probe ──────────────────────────
    let is_internet_check = lower.contains("connected to the internet")
        || lower.contains("internet connected")
        || lower.contains("are you connected")
        || lower.contains("am i online")
        || lower.contains("internet check")
        || lower.contains("kya internet")
        || lower.contains("internet hai")
        || (lower.contains("internet") && (lower.contains("check") || lower.contains("working") || lower.contains("status")));

    if is_internet_check && allowed_tool_names.contains("ping_host") {
        return vec![
            ParsedToolCall {
                name: "ping_host".into(),
                arguments: serde_json::json!({ "host": "google.com" }),
            },
            ParsedToolCall {
                name: "ping_host".into(),
                arguments: serde_json::json!({ "host": "1.1.1.1" }),
            },
            ParsedToolCall {
                name: "ping_host".into(),
                arguments: serde_json::json!({ "host": "8.8.8.8" }),
            },
        ];
    }

    // ── Fall back to single-call function ────────────────────────────────────
    build_intent_fallback_tool_call(user_text, allowed_tool_names)
        .into_iter()
        .collect()
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

        if let Some(call) = build_fallback_call_for_hint(&forced_tool, query, allowed_tool_names) {
            return Some(call);
        }

        // Generic fallback for locked/dynamic tools (for example MCP tools discovered at runtime).
        if allowed_tool_names.contains(&forced_tool) {
            let arguments = if query.trim().is_empty() {
                serde_json::json!({})
            } else if let Ok(value) = serde_json::from_str::<serde_json::Value>(query) {
                if value.is_object() {
                    value
                } else {
                    serde_json::json!({ "input": value })
                }
            } else {
                serde_json::json!({ "query": query })
            };

            return Some(ParsedToolCall {
                name: forced_tool,
                arguments,
            });
        }

        return None;
    }

    let intent = IntentRouter::classify(user_text);
    let hint = intent.tool_hint?;
    let user_query = user_text.trim();

    // Colab requests: override hint with the correct Colab flow entry-point.
    if looks_like_colab_request(&user_text.to_ascii_lowercase()) {
        if let Some((colab_intent, title, code)) = detect_colab_intent(user_text) {
            match colab_intent {
                ColabIntent::CreateNotebook => {
                    let full_title = title
                        .as_deref()
                        .map(|t| if t.ends_with(".ipynb") { t.to_string() } else { format!("{}.ipynb", t) })
                        .unwrap_or_else(|| "Untitled.ipynb".to_string());
                    if allowed_tool_names.contains("gw_drive_create") {
                        return Some(ParsedToolCall {
                            name: "gw_drive_create".into(),
                            arguments: serde_json::json!({
                                "title": full_title,
                                "mime_type": "application/vnd.google.colab",
                            }),
                        });
                    }
                    if allowed_tool_names.contains("mcp_colab-mcp_open_colab_browser_connection") {
                        return Some(ParsedToolCall {
                            name: "mcp_colab-mcp_open_colab_browser_connection".into(),
                            arguments: serde_json::json!({}),
                        });
                    }
                }
                ColabIntent::OpenNotebook | ColabIntent::Generic => {
                    if allowed_tool_names.contains("mcp_colab-mcp_open_colab_browser_connection") {
                        return Some(ParsedToolCall {
                            name: "mcp_colab-mcp_open_colab_browser_connection".into(),
                            arguments: serde_json::json!({}),
                        });
                    }
                }
                ColabIntent::ExecuteCode => {
                    // Gate: connection must be established first; let ColabFlowState handle it.
                    if allowed_tool_names.contains("mcp_colab-mcp_open_colab_browser_connection") {
                        return Some(ParsedToolCall {
                            name: "mcp_colab-mcp_open_colab_browser_connection".into(),
                            arguments: serde_json::json!({}),
                        });
                    }
                    if let Some(snippet) = code {
                        if allowed_tool_names.contains("mcp_colab-mcp_execute_cell") {
                            return Some(ParsedToolCall {
                                name: "mcp_colab-mcp_execute_cell".into(),
                                arguments: serde_json::json!({ "code": snippet }),
                            });
                        }
                    }
                }
            }
        }
    }

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
        "open_application" => "Open App".into(),
        "open_url" => "Open URL".into(),
        "browser_search" => "Browser Search".into(),
        "send_message" => "Send Message".into(),
        "close_application" | "kill_process" => "Close App".into(),
        "gw_gmail_inbox" | "gw_gmail_search" | "gw_gmail_read" | "gw_gmail_send"
        | "gw_gmail_delete" => "Gmail".into(),
        "gw_calendar_today" | "gw_calendar_search" | "gw_calendar_create"
        | "gw_calendar_delete" => "Google Calendar".into(),
        "gw_drive_search" | "gw_drive_list" | "gw_drive_read" | "gw_drive_delete" => {
            "Google Drive".into()
        }
        "gw_docs_create" | "gw_docs_read" | "gw_docs_edit" => "Google Docs".into(),
        "gw_sheets_create" | "gw_sheets_read" | "gw_sheets_edit" => "Google Sheets".into(),
        "gw_slides_create" | "gw_slides_read" => "Google Slides".into(),
        "gw_forms_list" | "gw_forms_create" => "Google Forms".into(),
        other if other.starts_with("mcp_") && other.contains("colab") => "Google Colab".into(),
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
            "gw_gmail_search",
            "gw_gmail_send",
            "gw_calendar_search",
            "gw_calendar_create",
            "gw_drive_list",
            "gw_drive_search",
            "gw_docs_read",
            "gw_sheets_read",
            "gw_slides_read",
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

    if looks_like_colab_request(&lower) {
        for tool in allowed_tool_names
            .iter()
            .filter(|name| name.starts_with("mcp_") && name.contains("colab"))
            .take(6)
        {
            push_tool_choice_candidate(
                &mut candidates,
                allowed_tool_names,
                tool,
                "Google Colab request detected",
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

// ─── Colab workflow state machine ────────────────────────────────────────────

/// What the user ultimately wants to do in Google Colab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColabIntent {
    /// Create a new .ipynb notebook (via Google Drive, then open in Colab).
    CreateNotebook,
    /// Open an existing notebook URL in Colab.
    OpenNotebook,
    /// Execute code in the currently active Colab notebook.
    ExecuteCode,
    /// General Colab request that needs the browser bridge but nothing specific.
    Generic,
}

/// Multi-step state machine that orchestrates the Colab workflow:
///   1. For CreateNotebook: drive_create → open_colab_browser_connection
///   2. For OpenNotebook / ExecuteCode / Generic: open_colab_browser_connection → (execute_cell)
#[derive(Debug, Clone)]
struct ColabFlowState {
    intent: ColabIntent,
    /// Notebook title supplied by the user (for CreateNotebook).
    notebook_title: Option<String>,
    /// Code supplied by the user (for ExecuteCode).
    code_snippet: Option<String>,
    /// Whether Drive file creation was attempted (CreateNotebook only).
    drive_create_attempted: bool,
    /// Whether Drive file creation succeeded and what the file ID is.
    drive_file_id: Option<String>,
    /// Whether open_colab_browser_connection has been called.
    browser_open_attempted: bool,
    /// Whether the browser session is confirmed connected.
    browser_connected: bool,
    /// Whether a code execute call has been dispatched.
    execute_attempted: bool,
}

impl ColabFlowState {
    fn from_user_text(text: &str) -> Option<Self> {
        let (intent, title, code) = detect_colab_intent(text)?;
        Some(Self {
            intent,
            notebook_title: title,
            code_snippet: code,
            drive_create_attempted: false,
            drive_file_id: None,
            browser_open_attempted: false,
            browser_connected: false,
            execute_attempted: false,
        })
    }

    /// Drive-create tool call for CreateNotebook flow.
    fn drive_create_call(&self) -> ParsedToolCall {
        let title = self
            .notebook_title
            .as_deref()
            .unwrap_or("Untitled Notebook");
        // gworkspace MCP creates a Google Doc; we use the same pattern but
        // flag it as an ipynb by appending the extension in the title.
        let full_title = if title.ends_with(".ipynb") {
            title.to_string()
        } else {
            format!("{}.ipynb", title)
        };
        ParsedToolCall {
            name: "gw_drive_create".into(),
            arguments: serde_json::json!({
                "title": full_title,
                "mime_type": "application/vnd.google.colab",
            }),
        }
    }

    /// Browser-connection bootstrap call.
    fn browser_open_call() -> ParsedToolCall {
        ParsedToolCall {
            name: "mcp_colab-mcp_open_colab_browser_connection".into(),
            arguments: serde_json::json!({}),
        }
    }

    /// Execute-cell call (only for ExecuteCode intent).
    fn execute_call(&self) -> Option<ParsedToolCall> {
        let code = self.code_snippet.as_deref()?;
        Some(ParsedToolCall {
            name: "mcp_colab-mcp_execute_cell".into(),
            arguments: serde_json::json!({ "code": code }),
        })
    }

    /// Returns the next forced calls for this workflow, if any.
    fn next_required_calls(&self, allowed_tool_names: &std::collections::HashSet<String>) -> Vec<ParsedToolCall> {
        // Step 1 (CreateNotebook only): create the Drive file first.
        if self.intent == ColabIntent::CreateNotebook && !self.drive_create_attempted {
            let call = self.drive_create_call();
            if allowed_tool_names.contains(&call.name) {
                return vec![call];
            }
            // Drive tool not available — fall through to browser open.
        }

        // Step 2: open the browser connection (once Drive file exists or not needed).
        if !self.browser_open_attempted {
            let call = Self::browser_open_call();
            if allowed_tool_names.contains(&call.name) {
                return vec![call];
            }
        }

        // Step 3 (ExecuteCode only): execute after browser is confirmed connected.
        if self.intent == ColabIntent::ExecuteCode && self.browser_connected && !self.execute_attempted {
            if let Some(call) = self.execute_call() {
                if allowed_tool_names.contains(&call.name) {
                    return vec![call];
                }
            }
        }

        vec![]
    }

    fn observe_tool_result(&mut self, call: &ParsedToolCall, success: bool, data: &serde_json::Value) {
        match call.name.as_str() {
            "gw_drive_create" => {
                self.drive_create_attempted = true;
                if success {
                    self.drive_file_id = data
                        .get("id")
                        .or_else(|| data.get("file_id"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                }
            }
            n if n.contains("open_colab_browser_connection") => {
                self.browser_open_attempted = true;
                // The tool returns {result: true/false}.
                let connected = data.get("result").and_then(|v| v.as_bool()).unwrap_or(success);
                self.browser_connected = connected;
            }
            n if n.contains("execute_cell") => {
                self.execute_attempted = true;
            }
            _ => {}
        }
    }

    fn status_summary(&self) -> String {
        match self.intent {
            ColabIntent::CreateNotebook => {
                if self.browser_connected {
                    format!(
                        "Notebook '{}' created on Drive and opened in Colab.",
                        self.notebook_title.as_deref().unwrap_or("Untitled")
                    )
                } else if self.drive_create_attempted {
                    format!(
                        "Notebook '{}' created on Drive. Opening Colab browser...",
                        self.notebook_title.as_deref().unwrap_or("Untitled")
                    )
                } else {
                    "Creating notebook on Google Drive...".into()
                }
            }
            ColabIntent::OpenNotebook => {
                if self.browser_connected {
                    "Colab notebook opened in browser.".into()
                } else {
                    "Opening Colab browser connection...".into()
                }
            }
            ColabIntent::ExecuteCode => {
                if self.execute_attempted {
                    "Code dispatched to Colab.".into()
                } else if self.browser_connected {
                    "Browser connected. Executing code...".into()
                } else {
                    "Connecting to Colab browser...".into()
                }
            }
            ColabIntent::Generic => {
                if self.browser_connected {
                    "Colab browser connection established.".into()
                } else {
                    "Connecting to Colab browser...".into()
                }
            }
        }
    }
}

/// Detect whether the user text is a Colab-related request and classify its intent.
/// Returns `(ColabIntent, optional_title, optional_code)` or `None` if not Colab.
fn detect_colab_intent(text: &str) -> Option<(ColabIntent, Option<String>, Option<String>)> {
    let lower = text.to_ascii_lowercase();

    let is_colab = lower.contains("colab")
        || lower.contains("google colab")
        || (lower.contains("notebook") && (lower.contains("python") || lower.contains("jupyter") || lower.contains("ipynb")));

    if !is_colab {
        return None;
    }

    // Create intent
    let is_create = ["create", "new", "make", "start a", "open a new", "banao", "bana"]
        .iter()
        .any(|kw| lower.contains(kw));

    if is_create {
        // Extract notebook title if present
        let title = infer_title(text, "").pipe_nonempty()
            .or_else(|| extract_notebook_title_from_text(text));
        return Some((ColabIntent::CreateNotebook, title, None));
    }

    // Execute intent
    let is_execute = ["run", "execute", "chalao", "chala", "print(", "import ", "code:"]
        .iter()
        .any(|kw| lower.contains(kw));

    if is_execute {
        let code = extract_code_from_text(text);
        return Some((ColabIntent::ExecuteCode, None, code));
    }

    // Open intent
    let is_open = ["open", "kholo", "kho do", "launch", "set as active", "active"]
        .iter()
        .any(|kw| lower.contains(kw));

    if is_open {
        return Some((ColabIntent::OpenNotebook, None, None));
    }

    // Generic Colab request
    Some((ColabIntent::Generic, None, None))
}

/// Attempt to extract a notebook title from text like "named X" or "called X".
fn extract_notebook_title_from_text(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    for marker in ["named ", "called ", "name ", "title "] {
        if let Some(idx) = lower.find(marker) {
            let rest = text[idx + marker.len()..].trim();
            let title = rest
                .split(|c: char| matches!(c, ' ') && !rest[..rest.find(c).unwrap_or(0)].ends_with('.'))
                .next()
                .unwrap_or(rest)
                .trim_matches(|c: char| c == '"' || c == '\'' || c == '`' || c == '.')
                .trim();
            if !title.is_empty() {
                return Some(title.to_string());
            }
        }
    }

    // Try quoted text
    if let Some(caps) = QUOTED_TEXT_RE.captures(text) {
        if let Some(m) = caps.get(1).or_else(|| caps.get(2)) {
            let t = m.as_str().trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }

    None
}

/// Extract inline code from a user request (for execute intent).
fn extract_code_from_text(text: &str) -> Option<String> {
    // Fenced code block
    if let Some(caps) = FENCED_CODE_BLOCK_RE.captures(text) {
        if let Some(m) = caps.get(1) {
            let code = m.as_str().trim();
            if !code.is_empty() {
                return Some(code.to_string());
            }
        }
    }

    // Backtick inline
    if let Some(caps) = QUOTED_TEXT_RE.captures(text) {
        if let Some(m) = caps.get(1).or_else(|| caps.get(2)) {
            let code = m.as_str().trim();
            if code.contains('\n') || code.contains('(') {
                return Some(code.to_string());
            }
        }
    }

    // "run: ..." or "execute: ..."
    let lower = text.to_ascii_lowercase();
    for marker in ["run:", "execute:", "code:"] {
        if let Some(idx) = lower.find(marker) {
            let rest = text[idx + marker.len()..].trim();
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
    }

    None
}

/// Helper: turn a `String` into `Option<String>`, returning `None` if empty.
trait PipeNonEmpty {
    fn pipe_nonempty(self) -> Option<String>;
}
impl PipeNonEmpty for String {
    fn pipe_nonempty(self) -> Option<String> {
        if self.is_empty() { None } else { Some(self) }
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
    /// Mid-execution heartbeat / progress update from a long-running tool.
    /// `call_id` matches the `name` field of the surrounding `ToolStart`/`ToolEnd`.
    /// `percent` is `None` when progress is indeterminate.
    ToolProgress {
        call_id: String,
        message: String,
        percent: Option<u8>,
    },
    /// A chunk of the **full** MCP payload streamed directly to the UI.
    /// The LLM only ever sees the compact summary; the UI can render full data
    /// by reassembling these chunks.
    ToolPayloadChunk {
        call_id: String,
        seq: u32,
        is_final: bool,
        data: serde_json::Value,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnExecutionMode {
    Assistant,
    PromptLab,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptLabToolSelectionStrategy {
    DirectLockedTool,
    RoutedWithinLock,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnExecutionProfile {
    pub mode: TurnExecutionMode,
    pub app_lock: Option<String>,
    pub tool_lock: Option<String>,
    pub prompt_lab_strategy: PromptLabToolSelectionStrategy,
}

impl TurnExecutionProfile {
    pub fn assistant() -> Self {
        Self::default()
    }

    pub fn prompt_lab(
        app_lock: Option<String>,
        tool_lock: Option<String>,
        prompt_lab_strategy: PromptLabToolSelectionStrategy,
    ) -> Self {
        Self {
            mode: TurnExecutionMode::PromptLab,
            app_lock: app_lock
                .map(|value| value.trim().to_ascii_lowercase())
                .filter(|value| !value.is_empty()),
            tool_lock: tool_lock
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            prompt_lab_strategy,
        }
    }

    fn is_prompt_lab(&self) -> bool {
        matches!(self.mode, TurnExecutionMode::PromptLab)
    }

    fn uses_direct_strategy(&self) -> bool {
        self.is_prompt_lab()
            && matches!(
                self.prompt_lab_strategy,
                PromptLabToolSelectionStrategy::DirectLockedTool
            )
    }
}

impl Default for TurnExecutionProfile {
    fn default() -> Self {
        Self {
            mode: TurnExecutionMode::Assistant,
            app_lock: None,
            tool_lock: None,
            prompt_lab_strategy: PromptLabToolSelectionStrategy::RoutedWithinLock,
        }
    }
}

fn tool_matches_lab_app_lock(tool_name: &str, app_lock: &str) -> bool {
    let lower = app_lock.to_ascii_lowercase();
    let tool_name_lower = tool_name.to_ascii_lowercase();

    match lower.as_str() {
        "gmail" => tool_name_lower.starts_with("gw_gmail_"),
        "drive" => tool_name_lower.starts_with("gw_drive_"),
        "docs" => tool_name_lower.starts_with("gw_docs_"),
        "sheets" => tool_name_lower.starts_with("gw_sheets_"),
        "calendar" => tool_name_lower.starts_with("gw_calendar_"),
        "slides" => tool_name_lower.starts_with("gw_slides_"),
        "forms" => tool_name_lower.starts_with("gw_forms_"),
        "google" | "gworkspace" | "google_workspace" => tool_name_lower.starts_with("gw_"),
        "colab" | "google_colab" | "notebook" => {
            tool_name_lower.starts_with("mcp_") && tool_name_lower.contains("colab")
        }
        _ => {
            if let Some(prefix) = lower.strip_prefix("mcp_") {
                tool_name_lower.starts_with(&format!("mcp_{}", prefix))
            } else {
                false
            }
        }
    }
}

fn tool_allowed_by_execution_profile(profile: &TurnExecutionProfile, tool_name: &str) -> bool {
    if !profile.is_prompt_lab() {
        return true;
    }

    if let Some(tool_lock) = profile.tool_lock.as_deref() {
        return tool_name == tool_lock;
    }

    if let Some(app_lock) = profile.app_lock.as_deref() {
        return tool_matches_lab_app_lock(tool_name, app_lock);
    }

    true
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
    /// Semantic router — None until initialised (falls back to regex router).
    semantic_router: Option<Arc<crate::routing::Router>>,
    max_tool_rounds: usize,
    hardware_tier: String,
    min_confidence_to_act: f32,
    clarify_threshold: f32,
    /// Per-session cancellation tokens.  A token is inserted when a turn starts
    /// and removed when it ends.  Calling `cancel_session` cancels all in-flight
    /// work for that session.
    active_cancels: Arc<dashmap::DashMap<String, tokio_util::sync::CancellationToken>>,
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
            semantic_router: None,
            max_tool_rounds: 10,
            hardware_tier: "standard".into(),
            min_confidence_to_act: 0.55,
            clarify_threshold: 0.40,
            active_cancels: Arc::new(dashmap::DashMap::new()),
        }
    }

    /// Attach an initialised semantic Router.
    pub fn with_semantic_router(mut self, router: Arc<crate::routing::Router>) -> Self {
        self.semantic_router = Some(router);
        self
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

    /// Cancel all in-flight work for `session_id`.
    ///
    /// Safe to call from any thread/task.  If no turn is active for the session
    /// this is a no-op.
    pub fn cancel_session(&self, session_id: &str) {
        if let Some(token) = self.active_cancels.get(session_id) {
            token.cancel();
        }
    }

    /// Returns a clone of the per-session cancel map so that the Tauri command
    /// layer can cancel sessions without holding a reference to `AgentLoop`.
    pub fn active_cancels(&self) -> Arc<dashmap::DashMap<String, CancellationToken>> {
        Arc::clone(&self.active_cancels)
    }

    /// Returns a clone of the HITL gateway so that remote transports (e.g.
    /// Telegram) can resolve pending approval requests without direct access
    /// to `AgentLoop` internals.
    pub fn hitl_gateway(&self) -> Arc<HitlGateway> {
        Arc::clone(&self.hitl_gateway)
    }

    /// Run the agent loop for a single user turn.
    /// Returns a channel of StreamEvents.
    pub async fn run(
        &self,
        session_id: &str,
        messages: &mut Vec<ChatMessage>,
        event_tx: mpsc::UnboundedSender<StreamEvent>,
    ) {
        self.run_with_profile(session_id, messages, event_tx, None)
            .await;
    }

    /// Run the agent loop for a single user turn with an optional execution profile.
    pub async fn run_with_profile(
        &self,
        session_id: &str,
        messages: &mut Vec<ChatMessage>,
        event_tx: mpsc::UnboundedSender<StreamEvent>,
        execution_profile: Option<TurnExecutionProfile>,
    ) {
        let execution_profile = execution_profile.unwrap_or_default();

        // ── Per-turn cancellation token ────────────────────────────────────────
        let turn_cancel = CancellationToken::new();
        self.active_cancels
            .insert(session_id.to_string(), turn_cancel.clone());
        // Guard: remove the token from the map when this function returns,
        // regardless of exit path.
        struct CancelGuard {
            map: Arc<dashmap::DashMap<String, CancellationToken>>,
            key: String,
        }
        impl Drop for CancelGuard {
            fn drop(&mut self) {
                self.map.remove(&self.key);
            }
        }
        let _cancel_guard = CancelGuard {
            map: Arc::clone(&self.active_cancels),
            key: session_id.to_string(),
        };

        // ── Per-turn error-loop guards ─────────────────────────────────────────
        // Maps call_dedup_hash(tool, args) -> (failure_count, last_error_msg).
        let mut failed_calls: HashMap<u64, (u8, String)> = HashMap::new();
        // Count of *consecutive* tool failures this turn (reset on any success).
        let mut consecutive_failures: u8 = 0;
        const MAX_CONSECUTIVE_FAILURES: u8 = 3;

        // ── Per-turn token budget tracker ─────────────────────────────────────
        // Approximate cumulative tokens consumed by all tool outputs this turn.
        let mut turn_tool_tokens: usize = 0;

        // Check if the user message contains images and route accordingly
        let has_images = messages.last().is_some_and(|m| m.has_images());
        let last_user_text = messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| m.content.clone())
            .unwrap_or_default();
        let mut routing_focus_text = routing_focus_text_from_user_content(&last_user_text);
        if execution_profile.uses_direct_strategy() {
            if let Some(tool_lock) = execution_profile.tool_lock.as_deref() {
                if extract_forced_tool_directive(&routing_focus_text).is_none() {
                    routing_focus_text = format!("#tool:{} {}", tool_lock, routing_focus_text);
                }
            }
        }
        let routing_focus_lower = routing_focus_text.to_lowercase();
        let pure_image_analysis_turn =
            has_images && looks_like_pure_image_analysis_request(&routing_focus_lower);

        log_pipeline_step(
            session_id,
            "prompt_entered",
            "Agent loop received prompt",
            Some(serde_json::json!({
                "has_images": has_images,
                "pure_image_analysis_turn": pure_image_analysis_turn,
                "prompt_lab_mode": execution_profile.is_prompt_lab(),
                "prompt_lab_strategy": format!("{:?}", execution_profile.prompt_lab_strategy),
                "app_lock": execution_profile.app_lock.clone(),
                "tool_lock": execution_profile.tool_lock.clone(),
                "message_count": messages.len(),
                "prompt_preview": sanitize_text_for_logs(&routing_focus_text, 260),
            })),
        );

        let backend = if has_images {
            match self.model_router.route_vision().await {
                Some(b) => b,
                None => {
                    log_pipeline_step(
                        session_id,
                        "backend_unavailable",
                        "No vision backend available",
                        Some(serde_json::json!({ "requested": "vision" })),
                    );
                    let _ = event_tx.send(StreamEvent::Error("no vision backend available".into()));
                    return;
                }
            }
        } else {
            match self.model_router.route("chat").await {
                Some(b) => b,
                None => {
                    log_pipeline_step(
                        session_id,
                        "backend_unavailable",
                        "No chat backend available",
                        Some(serde_json::json!({ "requested": "chat" })),
                    );
                    let _ = event_tx.send(StreamEvent::Error("no LLM backend available".into()));
                    return;
                }
            }
        };

        log_pipeline_step(
            session_id,
            "backend_selected",
            "Model backend selected",
            Some(serde_json::json!({
                "model_label": backend.model_label(),
                "capabilities": backend.capabilities(),
            })),
        );

        // Auto-mount tool groups based on user message keywords
        let mut meet_fallback_metadata: Option<serde_json::Value> = None;
        if pure_image_analysis_turn {
            log_pipeline_step(
                session_id,
                "preprocessing_skipped",
                "Skipped keyword auto-mount for pure image analysis turn",
                None,
            );
        } else if let Some(last_msg) = messages.last() {
            if last_msg.role == "user" {
                let mount_probe_text = routing_focus_text_from_user_content(&last_msg.content);
                meet_fallback_metadata = google_meet_fallback_metadata(&mount_probe_text);
                let mut mm = self.mount_manager.write().await;
                let newly = mm.auto_mount_from_message(&mount_probe_text);
                if !newly.is_empty() {
                    tracing::info!(groups = ?newly, "auto-mounted tool groups from user message");
                    log_pipeline_step(
                        session_id,
                        "preprocessing_applied",
                        "Tool auto-mount preprocessing applied",
                        Some(serde_json::json!({ "mounted_groups": newly })),
                    );
                } else {
                    log_pipeline_step(
                        session_id,
                        "preprocessing_skipped",
                        "No tool auto-mount preprocessing needed",
                        None,
                    );
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

            log_pipeline_step(
                session_id,
                "preprocessing_applied",
                "Google Meet fallback metadata injected",
                Some(serde_json::json!({
                    "metadata": sanitize_json_for_logs(&metadata, 220, 8),
                })),
            );
        }
        let google_workspace_intent =
            !pure_image_analysis_turn && looks_like_google_workspace_request(&routing_focus_lower);

        // ── Colab workflow: inject tool-routing guidance into context ──────────
        // This tells the LLM exactly which tools map to each Colab sub-task so
        // it never hallucinates a "colab create" verb.
        if !pure_image_analysis_turn && looks_like_colab_request(&routing_focus_lower) {
            let colab_guidance = concat!(
                "TOOL ROUTING RULES for Google Colab requests:\n",
                "1. CREATE a new Colab notebook → call `gw_drive_create` with mime_type=\"application/vnd.google.colab\", then call `mcp_colab-mcp_open_colab_browser_connection`.\n",
                "2. OPEN an existing Colab notebook / set active → call `mcp_colab-mcp_open_colab_browser_connection` (this opens the Colab tab in the browser).\n",
                "3. RUN / EXECUTE code in Colab → first ensure browser is connected via `mcp_colab-mcp_open_colab_browser_connection`, then call `mcp_colab-mcp_execute_cell` with the code.\n",
                "NEVER output plain text like 'colab create ...' — always emit a structured tool call JSON.",
            );
            messages.push(ChatMessage {
                role: "system".into(),
                content: colab_guidance.to_string(),
                name: None,
                images: None,
            });
            log_pipeline_step(
                session_id,
                "preprocessing_applied",
                "Colab tool-routing guidance injected",
                None,
            );
        }

        // Build tool schemas for the LLM (filtered by mount manager)
        let mount_mgr = self.mount_manager.read().await;
        let tool_defs = self.tool_registry.list_for_tier(&self.hardware_tier);
        let tool_schemas: Vec<ToolSchema> = tool_defs
            .iter()
            .filter(|d| mount_mgr.is_mounted(&d.name))
            .filter(|d| {
                if pure_image_analysis_turn {
                    is_tool_allowed_for_image_focus(d)
                } else {
                    true
                }
            })
            .filter(|d| {
                if d.name.starts_with("mcp_gworkspace_") {
                    google_workspace_intent
                } else {
                    true
                }
            })
            .filter(|d| tool_allowed_by_execution_profile(&execution_profile, &d.name))
            .map(|d| ToolSchema {
                name: d.name.clone(),
                description: d.description.clone(),
                parameters: d.to_function_schema()["function"]["parameters"].clone(),
            })
            .collect();
        let allowed_tool_names: HashSet<String> =
            tool_schemas.iter().map(|s| s.name.clone()).collect();
        drop(mount_mgr);

        let prompt_lab_direct_mode = execution_profile.uses_direct_strategy();

        log_pipeline_step(
            session_id,
            "tool_schemas_built",
            "Prepared mounted tool schemas for LLM",
            Some(serde_json::json!({
                "google_workspace_intent": google_workspace_intent,
                "pure_image_analysis_turn": pure_image_analysis_turn,
                "prompt_lab_mode": execution_profile.is_prompt_lab(),
                "prompt_lab_direct_mode": prompt_lab_direct_mode,
                "tool_count": tool_schemas.len(),
                "tool_names": tool_schemas
                    .iter()
                    .map(|schema| schema.name.clone())
                    .collect::<Vec<_>>(),
            })),
        );

        let llm_tool_schemas: Option<&[ToolSchema]> = if pure_image_analysis_turn {
            None
        } else {
            Some(&tool_schemas)
        };

        // Track tools already approved in this user-turn to avoid re-asking.
        // Key: "tool_name|args_json"
        let mut approved_this_turn: HashSet<String> = HashSet::new();
        let mut package_flow = PackageFlowState::from_user_text(&routing_focus_text);
        let mut colab_flow = ColabFlowState::from_user_text(&routing_focus_text);
        let mut intent_fallback_used = false;
        let mut had_successful_gmail_tool = false;
        let mut had_failed_gmail_tool = false;
        let mut last_successful_gmail_result: Option<serde_json::Value> = None;
        let mut last_successful_image_result: Option<serde_json::Value> = None;
        let intent_result = IntentRouter::classify(&routing_focus_text);
        let forced_tool_requested = extract_forced_tool_directive(&routing_focus_text).is_some();

        // Semantic routing: run async router when available, capturing per-turn modality.
        let turn_modality = if let Some(router) = &self.semantic_router {
            let (_, modality, _trace) = router.route(&routing_focus_text).await;
            modality
        } else {
            crate::routing::verbs::classify_modality(&routing_focus_text)
        };

        log_pipeline_step(
            session_id,
            "intent_classified",
            "Intent classification complete",
            Some(serde_json::json!({
                "intent": format!("{:?}", &intent_result.intent),
                "category": intent_result.category.clone(),
                "tool_hint": intent_result.tool_hint.clone(),
                "confidence": intent_result.confidence,
                "forced_tool_requested": forced_tool_requested,
                "package_flow_detected": package_flow.is_some(),
                "colab_flow_detected": colab_flow.is_some(),
            })),
        );

        for round in 0..self.max_tool_rounds {
            log_pipeline_step(
                session_id,
                "llm_input_prepared",
                "Prepared LLM request payload",
                Some(serde_json::json!({
                    "round": round,
                    "tool_schema_count": llm_tool_schemas.map(|schemas| schemas.len()).unwrap_or(0),
                    "history_message_count": messages.len(),
                    "messages_preview": build_message_preview(messages, 6),
                })),
            );

            // Call LLM
            let response = match backend.chat(messages, llm_tool_schemas, 0.7, 4096).await {
                Ok(r) => r,
                Err(e) => {
                    log_pipeline_step(
                        session_id,
                        "llm_error",
                        "LLM call failed",
                        Some(serde_json::json!({
                            "round": round,
                            "error": sanitize_text_for_logs(&e.to_string(), 260),
                        })),
                    );
                    let _ = event_tx.send(StreamEvent::Error(format!("LLM error: {e}")));
                    return;
                }
            };

            log_pipeline_step(
                session_id,
                "llm_response_received",
                "LLM response received",
                Some(serde_json::json!({
                    "round": round,
                    "model": response.model.clone(),
                    "usage": response.usage.as_ref().map(|u| serde_json::json!({
                        "prompt_tokens": u.prompt_tokens,
                        "completion_tokens": u.completion_tokens,
                        "total_tokens": u.total_tokens,
                    })),
                    "native_tool_calls": response
                        .tool_calls
                        .as_ref()
                        .map(|v| v.len())
                        .unwrap_or(0),
                    "content_preview": sanitize_text_for_logs(&response.content, 320),
                })),
            );

            // Parse tool calls from response — prefer native function-calling format
            // (returned by llama.cpp / OpenAI), fall back to text-embedded format.
            // Pattern 7 (Python-style fallback) fires last, only for single-required-param tools.
            let parse_mode = if response.tool_calls.is_some() {
                "native_function_call"
            } else {
                "text_pattern_fallback"
            };

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

            log_pipeline_step(
                session_id,
                "tool_calls_parsed",
                "Parsed tool calls from LLM response",
                Some(serde_json::json!({
                    "round": round,
                    "parse_mode": parse_mode,
                    "tool_call_count": tool_calls.len(),
                    "tool_calls": build_tool_calls_preview(&tool_calls),
                    "text_response_preview": sanitize_text_for_logs(&text_response, 320),
                })),
            );

            let mut synthetic_package_calls = false;
            let mut synthetic_colab_calls = false;
            let mut synthetic_intent_calls = false;
            if tool_calls.is_empty() {
                if let Some(flow) = package_flow.as_ref() {
                    let fallback_calls = flow.next_required_calls();
                    if !fallback_calls.is_empty() {
                        synthetic_package_calls = true;
                        tool_calls = fallback_calls;
                        log_pipeline_step(
                            session_id,
                            "synthetic_package_calls",
                            "Injected package workflow tool calls",
                            Some(serde_json::json!({
                                "round": round,
                                "tool_calls": build_tool_calls_preview(&tool_calls),
                            })),
                        );
                        let _ = event_tx.send(StreamEvent::Plan(
                            "Enforcing package workflow with pre/post verification".into(),
                        ));
                    }
                }
            }

            // Colab workflow: inject next required Colab step if LLM produced no calls.
            if tool_calls.is_empty() {
                if let Some(flow) = colab_flow.as_ref() {
                    let colab_calls = flow.next_required_calls(&allowed_tool_names);
                    if !colab_calls.is_empty() {
                        synthetic_colab_calls = true;
                        let status = flow.status_summary();
                        tool_calls = colab_calls;
                        log_pipeline_step(
                            session_id,
                            "synthetic_colab_calls",
                            "Injected Colab workflow tool calls",
                            Some(serde_json::json!({
                                "round": round,
                                "tool_calls": build_tool_calls_preview(&tool_calls),
                            })),
                        );
                        let _ = event_tx.send(StreamEvent::Plan(status));
                    }
                }
            }

            if tool_calls.is_empty() && !intent_fallback_used {
                let intent_fallback_query =
                    resolve_intent_fallback_query(&routing_focus_text, &messages);
                let fallback_intent_result = IntentRouter::classify(&intent_fallback_query);
                let fallback_confidence = fallback_intent_result.confidence.max(intent_result.confidence);

                let fallback_calls =
                    build_multi_intent_fallback_calls(&intent_fallback_query, &allowed_tool_names);

                if !fallback_calls.is_empty() {
                    if forced_tool_requested || fallback_confidence >= self.min_confidence_to_act {
                        intent_fallback_used = true;
                        synthetic_intent_calls = true;
                        let names: Vec<&str> = fallback_calls.iter().map(|c| c.name.as_str()).collect();
                        let plan_message = if intent_fallback_query == routing_focus_text {
                            format!(
                                "No tool call returned; applying intent fallback via {}",
                                names.join(", ")
                            )
                        } else {
                            format!(
                                "No tool call returned; applying context-aware intent fallback via {}",
                                names.join(", ")
                            )
                        };
                        let _ = event_tx.send(StreamEvent::Plan(plan_message));
                        tool_calls = fallback_calls;
                        log_pipeline_step(
                            session_id,
                            "synthetic_intent_call",
                            "Injected intent fallback tool call",
                            Some(serde_json::json!({
                                "round": round,
                                "fallback_query": sanitize_text_for_logs(&intent_fallback_query, 220),
                                "confidence": fallback_confidence,
                                "tool_calls": build_tool_calls_preview(&tool_calls),
                            })),
                        );
                    } else if fallback_confidence >= self.clarify_threshold {
                        let candidates = build_tool_choice_candidates(
                            &intent_fallback_query,
                            &allowed_tool_names,
                            fallback_intent_result.tool_hint.as_deref(),
                            fallback_confidence,
                        );

                        if !candidates.is_empty() {
                            log_pipeline_step(
                                session_id,
                                "tool_choice_required",
                                "Low-confidence route needs user tool choice",
                                Some(serde_json::json!({
                                    "round": round,
                                    "fallback_query": sanitize_text_for_logs(&intent_fallback_query, 220),
                                    "confidence": fallback_confidence,
                                    "candidate_count": candidates.len(),
                                })),
                            );
                            let _ = event_tx.send(StreamEvent::ToolChoiceRequired {
                                query: intent_fallback_query.clone(),
                                confidence: fallback_confidence,
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
                log_pipeline_step(
                    session_id,
                    "no_tool_calls",
                    "No tool calls returned for this round",
                    Some(serde_json::json!({
                        "round": round,
                        "synthetic_package_calls": synthetic_package_calls,
                        "synthetic_colab_calls": synthetic_colab_calls,
                        "synthetic_intent_calls": synthetic_intent_calls,
                    })),
                );

                if let Some(flow) = package_flow.as_ref() {
                    if let Some(summary) = flow.verified_summary() {
                        log_pipeline_step(
                            session_id,
                            "final_output_ready",
                            "Using package-flow verification summary",
                            Some(serde_json::json!({
                                "round": round,
                                "final_preview": sanitize_text_for_logs(&summary, 260),
                            })),
                        );
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

                log_pipeline_step(
                    session_id,
                    "final_formatting_started",
                    "Preparing final assistant output",
                    Some(serde_json::json!({
                        "round": round,
                        "had_successful_gmail_tool": had_successful_gmail_tool,
                        "had_failed_gmail_tool": had_failed_gmail_tool,
                        "text_preview": sanitize_text_for_logs(&final_text, 280),
                    })),
                );

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
                                round,
                                has_placeholder_scaffold,
                                has_raw_payload,
                                has_duplicate_rows,
                                "LLM returned non-grounded Gmail response; replacing with grounded summary"
                            );
                            log_pipeline_step(
                                session_id,
                                "final_formatting_adjusted",
                                "Replaced non-grounded Gmail output with grounded summary",
                                Some(serde_json::json!({
                                    "round": round,
                                    "has_placeholder_scaffold": has_placeholder_scaffold,
                                    "has_raw_payload": has_raw_payload,
                                    "has_duplicate_rows": has_duplicate_rows,
                                })),
                            );
                            final_text = grounded_summary;
                        }
                    }
                }

                if !final_text.is_empty() {
                    log_pipeline_step(
                        session_id,
                        "final_output_ready",
                        "Final assistant response ready",
                        Some(serde_json::json!({
                            "round": round,
                            "final_preview": sanitize_text_for_logs(&final_text, 320),
                            "final_chars": final_text.chars().count(),
                        })),
                    );
                    let _ = event_tx.send(StreamEvent::Token(final_text.clone()));
                    let _ = event_tx.send(StreamEvent::Done(final_text));
                } else if had_successful_gmail_tool && !had_failed_gmail_tool {
                    if let Some(summary) = last_successful_gmail_result
                        .as_ref()
                        .and_then(build_grounded_gmail_count_summary)
                    {
                        tracing::info!(
                            has_images,
                            round,
                            "LLM returned empty response with no tool calls; using grounded Gmail count summary"
                        );
                        log_pipeline_step(
                            session_id,
                            "final_output_ready",
                            "Using grounded Gmail count summary fallback",
                            Some(serde_json::json!({
                                "round": round,
                                "final_preview": sanitize_text_for_logs(&summary, 260),
                            })),
                        );
                        let _ = event_tx.send(StreamEvent::Token(summary.clone()));
                        let _ = event_tx.send(StreamEvent::Done(summary));
                    } else {
                        let fallback =
                            "I could not generate a response for this request. Please try again."
                                .to_string();
                        tracing::warn!(
                            has_images,
                            round,
                            "LLM returned empty response with no tool calls and no grounded Gmail summary"
                        );
                        log_pipeline_step(
                            session_id,
                            "final_output_fallback",
                            "Generated generic fallback due empty grounded response",
                            Some(serde_json::json!({
                                "round": round,
                                "final_preview": sanitize_text_for_logs(&fallback, 200),
                            })),
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
                        round,
                        "LLM returned empty response with no tool calls"
                    );
                    log_pipeline_step(
                        session_id,
                        "final_output_fallback",
                        "Generated generic fallback due empty response",
                        Some(serde_json::json!({
                            "round": round,
                            "final_preview": sanitize_text_for_logs(&fallback, 200),
                        })),
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

                log_pipeline_step(
                    session_id,
                    "assistant_tool_history_added",
                    "Added assistant tool-call turn to history",
                    Some(serde_json::json!({
                        "round": round,
                        "tool_calls": build_tool_calls_preview(&tool_calls),
                    })),
                );
            }

            // Execute each tool call
            for call in &tool_calls {
                log_pipeline_step(
                    session_id,
                    "tool_call_started",
                    "Beginning tool execution",
                    Some(serde_json::json!({
                        "round": round,
                        "tool": call.name.clone(),
                        "arguments": sanitize_json_for_logs(&call.arguments, 220, 8),
                    })),
                );

                // Never execute tools outside the current mounted+tier visible set.
                if !allowed_tool_names.contains(&call.name) {
                    let unavailable_msg = format!(
                        "tool '{}' is not available for current hardware tier '{}' or mounted tool groups",
                        call.name, self.hardware_tier
                    );

                    log_pipeline_step(
                        session_id,
                        "tool_call_rejected",
                        "Tool blocked by tier/mount gating",
                        Some(serde_json::json!({
                            "round": round,
                            "tool": call.name.clone(),
                            "reason": sanitize_text_for_logs(&unavailable_msg, 220),
                        })),
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

                // ── Colab browser-connection gate ────────────────────────────
                // If the LLM emits an execute_cell call but the browser connection
                // has not been established yet, transparently prepend the bootstrap
                // call so code never fires into a disconnected session.
                if call.name.contains("execute_cell") && call.name.contains("colab") {
                    let already_connected = colab_flow
                        .as_ref()
                        .map(|f| f.browser_connected)
                        .unwrap_or(false);
                    if !already_connected
                        && allowed_tool_names
                            .contains("mcp_colab-mcp_open_colab_browser_connection")
                    {
                        let _ = event_tx.send(StreamEvent::Plan(
                            "Colab browser not connected — establishing connection first.".into(),
                        ));
                        let bootstrap = ColabFlowState::browser_open_call();
                        // Inject bootstrap ahead of execute — push current call back.
                        // We handle this by bumping execute_cell to the next round
                        // after the browser is confirmed via observe_tool_result.
                        // Replace current call slice with [bootstrap_call, original_call].
                        // The simplest way: execute bootstrap now via recursive inject.
                        // We'll just replace the current `call` reference by mutating
                        // the iteration — instead, mark as gate-injected and continue.
                        let _ = event_tx.send(StreamEvent::ToolStart {
                            name: bootstrap.name.clone(),
                            params: bootstrap.arguments.clone(),
                        });
                        let gate_result = if let Some(gate_handler) =
                            self.tool_registry.get_handler(&bootstrap.name)
                        {
                            let gate_handler = gate_handler.clone();
                            gate_handler.execute(bootstrap.arguments.clone()).await
                        } else {
                            crate::infra::isolation::ToolResult::err(
                                "open_colab_browser_connection handler not found".to_string(),
                            )
                        };
                        if let Some(flow) = colab_flow.as_mut() {
                            flow.observe_tool_result(
                                &bootstrap,
                                gate_result.success,
                                &gate_result.data,
                            );
                        }
                        let _ = event_tx.send(StreamEvent::ToolEnd {
                            name: bootstrap.name.clone(),
                            result: gate_result.data.clone(),
                            success: gate_result.success,
                        });
                        messages.push(ChatMessage {
                            role: "tool".into(),
                            content: serde_json::to_string(&gate_result.data)
                                .unwrap_or_default(),
                            name: Some(bootstrap.name.clone()),
                            images: None,
                        });
                        if !gate_result.success {
                            messages.push(ChatMessage {
                                role: "system".into(),
                                content: "Colab browser connection failed. Cannot execute cell."
                                    .into(),
                                name: None,
                                images: None,
                            });
                            continue;
                        }
                    }
                }

                // Policy check — pass destructive hint from semantic router modality
                let decision = self.policy_engine.evaluate_with_modality_hint(
                    &call.name,
                    &call.arguments,
                    turn_modality.destructive,
                );

                log_pipeline_step(
                    session_id,
                    "policy_evaluated",
                    "Policy evaluation completed for tool call",
                    Some(serde_json::json!({
                        "round": round,
                        "tool": call.name.clone(),
                        "risk_level": decision.risk_level.as_str(),
                        "requires_approval": decision.requires_approval,
                        "blocked": decision.blocked,
                        "reason": sanitize_text_for_logs(&decision.reason, 220),
                    })),
                );

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

                    log_pipeline_step(
                        session_id,
                        "tool_call_blocked",
                        "Tool call blocked by safety policy",
                        Some(serde_json::json!({
                            "round": round,
                            "tool": call.name.clone(),
                            "reason": sanitize_text_for_logs(&decision.reason, 220),
                        })),
                    );

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

                        log_pipeline_step(
                            session_id,
                            "approval_reused",
                            "Reused earlier approval for identical tool call",
                            Some(serde_json::json!({
                                "round": round,
                                "tool": call.name.clone(),
                            })),
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

                        log_pipeline_step(
                            session_id,
                            "approval_requested",
                            "Approval requested for RED-tier tool call",
                            Some(serde_json::json!({
                                "round": round,
                                "tool": call.name.clone(),
                                "request_id": request_id.clone(),
                                "risk_level": decision.risk_level.as_str(),
                            })),
                        );

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

                        log_pipeline_step(
                            session_id,
                            "approval_result",
                            "Approval decision received",
                            Some(serde_json::json!({
                                "round": round,
                                "tool": call.name.clone(),
                                "approved": approved,
                            })),
                        );

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

                            log_pipeline_step(
                                session_id,
                                "tool_call_denied",
                                "Tool call not executed due denied/timeout approval",
                                Some(serde_json::json!({
                                    "round": round,
                                    "tool": call.name.clone(),
                                    "reason": denial_reason,
                                })),
                            );

                            continue;
                        }

                        // Remember this approval for the rest of this turn
                        approved_this_turn.insert(dedup_key);

                        // Create rollback snapshot for RED actions
                        // (actual file backup happens inside specific tool handlers)
                    }
                }

                // ── Dedup guard: abort on repeated identical failure ───────────
                let call_hash = call_dedup_hash(&call.name, &call.arguments);
                if let Some((fail_count, cached_err)) = failed_calls.get(&call_hash) {
                    if *fail_count >= 1 {
                        let abort_msg = format!(
                            "repeated_identical_failure: '{}' with the same arguments already \
                             failed in this turn: {}. Aborting to prevent an infinite loop.",
                            call.name, cached_err
                        );
                        tracing::warn!(
                            session = session_id,
                            tool = %call.name,
                            "dedup guard: aborting duplicate failed call"
                        );
                        log_pipeline_step(
                            session_id,
                            "tool_retry_blocked",
                            "Blocked duplicate failed tool call",
                            Some(serde_json::json!({
                                "round": round,
                                "tool": call.name.clone(),
                                "fail_count": fail_count,
                                "cached_error": cached_err,
                            })),
                        );
                        let _ = event_tx.send(StreamEvent::Error(abort_msg.clone()));
                        return;
                    }
                }

                // ── Turn budget guard: skip tool if cumulative tokens exhausted ─
                if turn_tool_tokens >= LLM_TURN_TOOL_BUDGET {
                    let budget_msg = format!(
                        "TOOL_BUDGET_EXHAUSTED: turn tool-output token budget ({LLM_TURN_TOOL_BUDGET}) \
                         reached; skipping '{}'. Summarise what you have and answer the user.",
                        call.name
                    );
                    tracing::warn!(
                        session = session_id,
                        turn_tool_tokens,
                        tool = %call.name,
                        "turn tool-output budget exhausted; skipping tool"
                    );
                    messages.push(ChatMessage {
                        role: "tool".into(),
                        content: budget_msg,
                        name: Some(call.name.clone()),
                        images: None,
                    });
                    continue;
                }

                // ── Heartbeat: emit ToolProgress every 2 s while tool runs ─────
                let hb_cancel = CancellationToken::new();
                let hb_cancel_clone = hb_cancel.clone();
                let hb_tx = event_tx.clone();
                let hb_tool = call.name.clone();
                tokio::spawn(async move {
                    let mut interval =
                        tokio::time::interval(std::time::Duration::from_secs(2));
                    interval.tick().await; // skip the immediate first tick
                    loop {
                        tokio::select! {
                            biased;
                            _ = hb_cancel_clone.cancelled() => break,
                            _ = interval.tick() => {
                                let _ = hb_tx.send(StreamEvent::ToolProgress {
                                    call_id: hb_tool.clone(),
                                    message: format!("⏳ {} is still running…", hb_tool),
                                    percent: None,
                                });
                            }
                        }
                    }
                });

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
                        "generate_image" => 300,
                        "search_news" | "fetch_article" => 60,
                        "execute_bash" | "execute_python" | "execute_powershell" => 120,
                        "download_file" => 120,
                        _ => 30,
                    };
                    let isolation_name = format!("tool:{}", call.name);
                    let exec_future = run_isolated(
                        &isolation_name,
                        std::time::Duration::from_secs(timeout_secs),
                        move || async move { handler.execute(args).await },
                    );
                    // Wrap execution in a cancellation select so "KRIA stop now"
                    // can abort the entire turn immediately.
                    let turn_cancel_ref = &turn_cancel;
                    tokio::select! {
                        biased;
                        _ = turn_cancel_ref.cancelled() => {
                            crate::infra::isolation::ToolResult::err(
                                "turn cancelled by user".to_string(),
                            )
                        }
                        result = exec_future => result,
                    }
                } else {
                    crate::infra::isolation::ToolResult::err(format!("unknown tool: {}", call.name))
                };

                // Stop the heartbeat task.
                hb_cancel.cancel();

                // ── Update error-loop counters ─────────────────────────────────
                if tool_result.success {
                    consecutive_failures = 0;
                } else {
                    let err_text = tool_result
                        .error
                        .clone()
                        .unwrap_or_else(|| "unknown error".to_string());
                    let entry = failed_calls
                        .entry(call_hash)
                        .or_insert((0, err_text.clone()));
                    entry.0 += 1;
                    entry.1 = err_text;
                    consecutive_failures += 1;

                    if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                        tracing::warn!(
                            session = session_id,
                            consecutive_failures,
                            "3 consecutive tool failures — injecting corrective prompt"
                        );
                        log_pipeline_step(
                            session_id,
                            "consecutive_failures_threshold",
                            "3 consecutive tool failures; injecting corrective system message",
                            Some(serde_json::json!({ "round": round })),
                        );
                        // Inject a corrective system message so the LLM knows to
                        // stop using tools and answer with what it has.
                        messages.push(ChatMessage {
                            role: "system".into(),
                            content: "SYSTEM: 3 consecutive tool executions have failed. \
                                      Stop issuing tool calls. Respond to the user using \
                                      whatever information you have, or ask the user for \
                                      guidance to resolve the problem."
                                .to_string(),
                            name: None,
                            images: None,
                        });
                        // Reset so we don't inject repeatedly.
                        consecutive_failures = 0;
                    }
                }

                if let Some(flow) = package_flow.as_mut() {
                    flow.observe_tool_result(call, tool_result.success, &tool_result.data);
                }

                if let Some(flow) = colab_flow.as_mut() {
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

                if call.name == "generate_image" && tool_result.success {
                    last_successful_image_result = Some(tool_result.data.clone());
                }

                // For generate_image failures: emit a structured user-visible message
                // and skip the LLM round so the user gets clear feedback immediately.
                if call.name == "generate_image" && !tool_result.success {
                    let failure_msg = build_image_failure_response(&tool_result.data);
                    tracing::warn!(session = session_id, "generate_image failed; returning structured failure to user");
                    let _ = event_tx.send(StreamEvent::Token(failure_msg.clone()));
                    let _ = event_tx.send(StreamEvent::Done(failure_msg));
                    return;
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
                //
                // For successful results we apply a two-stage budget strategy:
                //   1. Shape the raw payload (drop bodies/base64, truncate strings)
                //      using the domain-aware shaper.  Gmail payloads first go
                //      through the existing compact_tool_result_for_llm() path.
                //   2. Count tokens via llama.cpp /tokenize; if still over the
                //      per-tool budget, re-shape with a tighter char budget.
                //   3. Hard char-cap as a final safety net.
                let llm_tool_result = compact_tool_result_for_llm(&call.name, &tool_result.data);
                let result_str = if !tool_result.success {
                    let err_msg = tool_result
                        .error
                        .as_deref()
                        .unwrap_or("tool execution failed with no details");
                    format!("TOOL_ERROR: {err_msg}")
                } else {
                    // ── Context Bomb mitigation ────────────────────────────
                    // Per-tool char budget derived from token budget.
                    let char_budget =
                        LLM_TOOL_RESULT_TOKEN_BUDGET * 4; // ~4 chars/token heuristic

                    // Stream the full payload to the UI via ToolPayloadChunk so
                    // the user always sees complete data while the LLM only gets
                    // the compact summary.
                    let full_payload_str = llm_tool_result.to_string();
                    if full_payload_str.len() > char_budget {
                        // Emit a single final chunk with full data for UI rendering.
                        let _ = event_tx.send(StreamEvent::ToolPayloadChunk {
                            call_id: call.name.clone(),
                            seq: 0,
                            is_final: true,
                            data: llm_tool_result.clone(),
                        });
                    }

                    // Stage 1: structural shaping.
                    let shaped = shape_for_llm(&call.name, &llm_tool_result, char_budget);
                    let mut shaped_str = shaped.value.to_string();

                    // Stage 2: token counting — tighten budget if needed.
                    let tokenizer_url = backend.tokenizer_base_url();
                    let token_count = count_tokens(&shaped_str, &tokenizer_url).await;
                    if token_count > LLM_TOOL_RESULT_TOKEN_BUDGET {
                        // Re-shape with a char budget proportional to how much
                        // we need to shrink.
                        let tighter = (char_budget * LLM_TOOL_RESULT_TOKEN_BUDGET / token_count)
                            .max(512);
                        let reshaped = shape_for_llm(&call.name, &llm_tool_result, tighter);
                        shaped_str = reshaped.value.to_string();
                    }

                    // Stage 3: hard char cap as final safety net.
                    if shaped_str.len() > TOOL_RESULT_MAX_CHARS {
                        format!(
                            "{}...<truncated>",
                            &shaped_str[..TOOL_RESULT_MAX_CHARS]
                        )
                    } else {
                        shaped_str
                    }
                };

                // Update the cumulative turn token counter.
                let result_tokens = count_tokens(&result_str, &backend.tokenizer_base_url()).await;
                turn_tool_tokens = turn_tool_tokens.saturating_add(result_tokens);

                // Auto-route: if tool result contains a file path, check if a
                // precognitive tool should process it automatically
                let auto_enrichment = self
                    .auto_route_file_result(&call.name, &tool_result.data)
                    .await;

                log_pipeline_step(
                    session_id,
                    "tool_result_ready",
                    "Tool execution completed",
                    Some(serde_json::json!({
                        "round": round,
                        "tool": call.name.clone(),
                        "success": tool_result.success,
                        "error": tool_result
                            .error
                            .as_ref()
                            .map(|e| sanitize_text_for_logs(e, 220)),
                        "result_preview": sanitize_json_for_logs(&tool_result.data, 220, 8),
                        "result_tokens": result_tokens,
                        "turn_tool_tokens_total": turn_tool_tokens,
                        "auto_enriched": auto_enrichment.is_some(),
                    })),
                );

                let _ = event_tx.send(StreamEvent::ToolEnd {
                    name: call.name.clone(),
                    result: tool_result.data.clone(),
                    success: tool_result.success,
                });

                let tool_msg = if let Some(enrichment) = auto_enrichment {
                    format!(
                        "{}\n\n[Auto-enriched via sidecar]\n{}",
                        result_str, enrichment
                    )
                } else {
                    result_str
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

            // ── Image-generation early exit ────────────────────────────────────────
            // When generate_image succeeded this round, skip the round-N LLM summary
            // call entirely — that call would crash the GPU with ctx=2048 + 167 schemas.
            // Instead, emit a pre-built confirmation response and return immediately.
            if let Some(ref img_data) = last_successful_image_result {
                let summary = build_image_success_response(img_data);
                log_pipeline_step(
                    session_id,
                    "final_output_ready",
                    "Image generation succeeded; skipping LLM summary call",
                    Some(serde_json::json!({
                        "round": round,
                        "final_preview": sanitize_text_for_logs(&summary, 280),
                    })),
                );
                let _ = event_tx.send(StreamEvent::Token(summary.clone()));
                let _ = event_tx.send(StreamEvent::Done(summary));
                return;
            }

            log_pipeline_step(
                session_id,
                "round_completed",
                "Round completed with tool outputs appended; continuing loop",
                Some(serde_json::json!({
                    "round": round,
                    "history_message_count": messages.len(),
                })),
            );
        }

        log_pipeline_step(
            session_id,
            "max_rounds_reached",
            "Agent loop reached max tool rounds",
            Some(serde_json::json!({
                "max_tool_rounds": self.max_tool_rounds,
            })),
        );

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
    fn intent_fallback_uses_gmail_send_for_send_mail_prompt() {
        let mut allowed = HashSet::new();
        allowed.insert("gw_gmail_send".to_string());

        let call = build_intent_fallback_tool_call(
            "Send a Hye mail to \"zeeshanobaid335@gmail.com\"",
            &allowed,
        )
        .expect("expected gmail send fallback call");

        assert_eq!(call.name, "gw_gmail_send");
        assert_eq!(call.arguments["to"], "zeeshanobaid335@gmail.com");
        assert_eq!(call.arguments["body"], "Hye");
        assert_eq!(call.arguments["subject"], "Hye");
    }

    #[test]
    fn intent_fallback_does_not_send_email_without_message_body() {
        let mut allowed = HashSet::new();
        allowed.insert("gw_gmail_send".to_string());

        let call = build_intent_fallback_tool_call(
            "Send mail to zeeshanobaid335@gmail.com",
            &allowed,
        );

        assert!(call.is_none());
    }

    #[test]
    fn contextual_send_confirmation_uses_prior_turn_details() {
        let messages = vec![
            ChatMessage {
                role: "user".into(),
                content: "Send a Hye mail to \"zeeshanobaid335@gmail.com\"".into(),
                name: None,
                images: None,
            },
            ChatMessage {
                role: "assistant".into(),
                content: "Sure, what should I write?".into(),
                name: None,
                images: None,
            },
            ChatMessage {
                role: "user".into(),
                content: "content be \"Hello Zeeshan how are you.\"".into(),
                name: None,
                images: None,
            },
            ChatMessage {
                role: "user".into(),
                content: "send immediately".into(),
                name: None,
                images: None,
            },
        ];

        let contextual_query = resolve_intent_fallback_query("send immediately", &messages);
        assert!(contextual_query.contains("zeeshanobaid335@gmail.com"));
        assert!(contextual_query.contains("Hello Zeeshan how are you."));

        let mut allowed = HashSet::new();
        allowed.insert("gw_gmail_send".to_string());

        let call = build_intent_fallback_tool_call(&contextual_query, &allowed)
            .expect("expected contextual gmail send fallback call");

        assert_eq!(call.name, "gw_gmail_send");
        assert_eq!(call.arguments["to"], "zeeshanobaid335@gmail.com");
        assert_eq!(call.arguments["body"], "Hello Zeeshan how are you.");
    }

    #[test]
    fn intent_fallback_reads_google_doc_from_url() {
        let mut allowed = HashSet::new();
        allowed.insert("gw_docs_read".to_string());

        let call = build_intent_fallback_tool_call(
            "Read this Google Doc https://docs.google.com/document/d/1AbCdEfGhIJKLmNoPqRsTuVwXyZ1234567890/edit",
            &allowed,
        )
        .expect("expected docs read fallback call");

        assert_eq!(call.name, "gw_docs_read");
        assert_eq!(
            call.arguments["document_id"],
            "1AbCdEfGhIJKLmNoPqRsTuVwXyZ1234567890"
        );
    }

    #[test]
    fn intent_fallback_edits_google_doc_from_url_and_text() {
        let mut allowed = HashSet::new();
        allowed.insert("gw_docs_edit".to_string());

        let call = build_intent_fallback_tool_call(
            "Append \"Follow up tomorrow\" to this Google Doc https://docs.google.com/document/d/1AbCdEfGhIJKLmNoPqRsTuVwXyZ1234567890/edit",
            &allowed,
        )
        .expect("expected docs edit fallback call");

        assert_eq!(call.name, "gw_docs_edit");
        assert_eq!(
            call.arguments["document_id"],
            "1AbCdEfGhIJKLmNoPqRsTuVwXyZ1234567890"
        );
        assert_eq!(call.arguments["text"], "Follow up tomorrow");
    }

    #[test]
    fn intent_fallback_reads_google_sheet_from_url() {
        let mut allowed = HashSet::new();
        allowed.insert("gw_sheets_read".to_string());

        let call = build_intent_fallback_tool_call(
            "Read this spreadsheet https://docs.google.com/spreadsheets/d/1ZyXwVuTsRqPoNmLkJiHgFeDcBa9876543210/edit",
            &allowed,
        )
        .expect("expected sheets read fallback call");

        assert_eq!(call.name, "gw_sheets_read");
        assert_eq!(
            call.arguments["spreadsheet_id"],
            "1ZyXwVuTsRqPoNmLkJiHgFeDcBa9876543210"
        );
    }

    #[test]
    fn intent_fallback_edits_google_sheet_cell_from_prompt() {
        let mut allowed = HashSet::new();
        allowed.insert("gw_sheets_edit".to_string());

        let call = build_intent_fallback_tool_call(
            "Update spreadsheet https://docs.google.com/spreadsheets/d/1ZyXwVuTsRqPoNmLkJiHgFeDcBa9876543210/edit set A1 to \"Done\"",
            &allowed,
        )
        .expect("expected sheets edit fallback call");

        assert_eq!(call.name, "gw_sheets_edit");
        assert_eq!(
            call.arguments["spreadsheet_id"],
            "1ZyXwVuTsRqPoNmLkJiHgFeDcBa9876543210"
        );
        assert_eq!(call.arguments["range"], "A1");

        let values = call
            .arguments
            .get("values")
            .and_then(|v| v.as_str())
            .expect("expected values to be encoded string");
        assert_eq!(values, "[[\"Done\"]]");
    }

    #[test]
    fn intent_fallback_deletes_drive_file_from_url() {
        let mut allowed = HashSet::new();
        allowed.insert("gw_drive_delete".to_string());

        let call = build_intent_fallback_tool_call(
            "Delete this Google Drive file https://drive.google.com/file/d/1n2B3c4D5e6F7g8H9i0JkLmNoPq/view",
            &allowed,
        )
        .expect("expected drive delete fallback call");

        assert_eq!(call.name, "gw_drive_delete");
        assert_eq!(
            call.arguments["file_id"],
            "1n2B3c4D5e6F7g8H9i0JkLmNoPq"
        );
    }

    #[test]
    fn intent_fallback_deletes_gmail_from_message_id() {
        let mut allowed = HashSet::new();
        allowed.insert("gw_gmail_delete".to_string());

        let call = build_intent_fallback_tool_call(
            "Delete this Gmail with message_id 18af9f0a8bcdef12",
            &allowed,
        )
        .expect("expected gmail delete fallback call");

        assert_eq!(call.name, "gw_gmail_delete");
        assert_eq!(call.arguments["message_id"], "18af9f0a8bcdef12");
    }

    #[test]
    fn intent_fallback_deletes_calendar_event_with_event_id() {
        let mut allowed = HashSet::new();
        allowed.insert("gw_calendar_delete".to_string());

        let call = build_intent_fallback_tool_call(
            "Cancel this meeting event_id abc123def456ghi789",
            &allowed,
        )
        .expect("expected calendar delete fallback call");

        assert_eq!(call.name, "gw_calendar_delete");
        assert_eq!(call.arguments["event_id"], "abc123def456ghi789");
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
    fn forced_tool_directive_supports_hyphenated_mcp_tool_names() {
        let mut allowed = HashSet::new();
        allowed.insert("mcp_colab-mcp_execute_cell".to_string());

        let call = build_intent_fallback_tool_call(
            r#"#tool:mcp_colab-mcp_execute_cell {"code":"print('hello')"}"#,
            &allowed,
        )
        .expect("expected forced tool fallback call");

        assert_eq!(call.name, "mcp_colab-mcp_execute_cell");
        assert_eq!(call.arguments["code"], "print('hello')");
    }

    #[test]
    fn prompt_lab_colab_app_lock_matches_colab_mcp_tools() {
        assert!(tool_matches_lab_app_lock(
            "mcp_colab-mcp_execute_cell",
            "colab"
        ));
        assert!(tool_matches_lab_app_lock(
            "mcp_mycolabserver_list_notebooks",
            "colab"
        ));
        assert!(!tool_matches_lab_app_lock("gw_gmail_inbox", "colab"));
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
