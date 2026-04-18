//! Google Workspace tools — hybrid MCP + sidecar handlers.
//!
//! Architecture: tools are ALWAYS registered in the ToolRegistry so the LLM
//! can see them regardless of whether the MCP server is connected.  The actual
//! MCP connection is held in a lazy `GwClientRef` (Arc<RwLock<Option<…>>>).
//! Once the gworkspace MCP server starts successfully, `init_runtime` populates
//! that ref via `set_client()`.  Until then, every handler returns a clear
//! "not connected" message rather than panicking or silently failing.
//!
//! Mount groups:
//!   ambient: gw_gmail_inbox, gw_gmail_search, gw_gmail_read,
//!            gw_calendar_today, gw_calendar_search,
//!            gw_drive_search, gw_drive_list, gw_drive_read
//!   docs:    gw_docs_read, gw_docs_create, gw_docs_edit,
//!            gw_sheets_read, gw_sheets_create, gw_sheets_edit,
//!            gw_slides_read, gw_slides_create,
//!            gw_forms_list, gw_forms_create
//!   admin:   gw_gmail_send, gw_gmail_delete,
//!            gw_drive_delete, gw_calendar_create, gw_calendar_delete

use crate::infra::ToolResult;
use crate::mcp::McpClient;
use crate::safety::RiskLevel;
use crate::sidecar::SidecarBridge;
use crate::tools::registry::{ParamDef, ToolDef, ToolHandler, ToolRegistry};
use async_trait::async_trait;
use std::collections::HashSet;
use std::sync::Arc;

/// Lazy reference to the gworkspace MCP client.
/// Starts as None; populated by `set_client()` once the MCP server connects.
pub type GwClientRef = Arc<tokio::sync::RwLock<Option<Arc<McpClient>>>>;

/// Create an empty lazy client reference (call `set_client()` later).
pub fn new_client_ref() -> GwClientRef {
    Arc::new(tokio::sync::RwLock::new(None))
}

/// Wire in the live MCP client after the server starts.
pub async fn set_client(gw_ref: &GwClientRef, client: Arc<McpClient>) {
    tracing::info!("[GW] wiring live McpClient into GwClientRef");
    *gw_ref.write().await = Some(client);
}

fn param(name: &str, ty: &str, desc: &str, required: bool) -> ParamDef {
    ParamDef {
        name: name.into(),
        param_type: ty.into(),
        description: desc.into(),
        required,
        default: None,
    }
}

/// Shared handles for hybrid MCP + sidecar calls.
#[derive(Clone)]
struct GwBridge {
    /// Lazy client ref — None until the gworkspace MCP server connects.
    mcp: GwClientRef,
    sidecar: Arc<SidecarBridge>,
}

fn active_google_account() -> String {
    std::env::var("KRIA_GW_ACCOUNT").unwrap_or_else(|_| "personal".into())
}

const GMAIL_MAX_RESULTS_CAP: u64 = 200;
const GMAIL_PAGE_SIZE_CAP: u64 = 50;
const GMAIL_MAX_PAGE_FETCHES: usize = 6;

fn gw_kind_for_tool(tool: &str) -> &'static str {
    match tool {
        t if t.contains("Gmail") => "gmail",
        t if t.contains("Calendar") => "calendar",
        t if t.contains("Spreadsheet") => "sheets",
        t if t.contains("Presentation") || t.contains("Slides") => "slides",
        t if t.contains("Form") => "forms",
        t if t.contains("Document") || t.contains("GoogleDoc") => "docs",
        t if t.contains("Folder") || t.contains("Drive") || t.contains("File") => "drive",
        _ => "google_workspace",
    }
}

fn parse_json_or_text(text: &str) -> serde_json::Value {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return serde_json::Value::Null;
    }

    serde_json::from_str::<serde_json::Value>(trimmed)
        .unwrap_or_else(|_| serde_json::json!({ "text": trimmed }))
}

fn envelope_result(tool: &str, data: serde_json::Value, raw_text: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "provider": "google_workspace",
        "kind": gw_kind_for_tool(tool),
        "tool": tool,
        "data": data,
        "raw_text": raw_text.unwrap_or(""),
    })
}

fn looks_like_drive_listing_phrase(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let has_list_intent = ["list", "show", "browse", "contents", "what is in", "what's in"]
        .iter()
        .any(|needle| lower.contains(needle));
    let has_search_intent = ["search", "find", "look for", "locate"]
        .iter()
        .any(|needle| lower.contains(needle));

    has_list_intent && !has_search_intent
}

fn looks_like_gmail_message_object(value: &serde_json::Value) -> bool {
    value
        .as_object()
        .map(|obj| {
            [
                "id",
                "messageId",
                "message_id",
                "threadId",
                "subject",
                "from",
                "snippet",
            ]
            .iter()
            .any(|key| obj.contains_key(*key))
        })
        .unwrap_or(false)
}

fn parse_gmail_heading_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !(trimmed.starts_with("**") && trimmed.ends_with("**") && trimmed.len() > 4) {
        return None;
    }

    let inner = &trimmed[2..trimmed.len() - 2];
    let (index, rest) = inner.split_once(". ")?;
    if !index.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }

    let subject = rest.trim();
    if subject.is_empty() {
        None
    } else {
        Some(subject.to_string())
    }
}

fn parse_gmail_labels(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(|label| label.to_string())
        .collect()
}

fn parse_gmail_messages_from_text(raw: &str) -> Vec<serde_json::Value> {
    let mut messages: Vec<serde_json::Value> = Vec::new();
    let mut current: Option<serde_json::Map<String, serde_json::Value>> = None;

    for line in raw.lines() {
        if let Some(subject) = parse_gmail_heading_line(line) {
            if let Some(msg) = current.take() {
                messages.push(serde_json::Value::Object(msg));
            }

            let mut msg = serde_json::Map::new();
            msg.insert("subject".into(), serde_json::Value::String(subject));
            current = Some(msg);
            continue;
        }

        let Some(msg) = current.as_mut() else {
            continue;
        };
        let trimmed = line.trim();

        if let Some(value) = trimmed.strip_prefix("From:") {
            msg.insert(
                "from".into(),
                serde_json::Value::String(value.trim().to_string()),
            );
            continue;
        }

        if let Some(value) = trimmed.strip_prefix("Date:") {
            msg.insert(
                "date".into(),
                serde_json::Value::String(value.trim().to_string()),
            );
            continue;
        }

        if let Some(value) = trimmed.strip_prefix("ID:") {
            msg.insert(
                "id".into(),
                serde_json::Value::String(value.trim().to_string()),
            );
            continue;
        }

        if let Some(value) = trimmed.strip_prefix("Labels:") {
            let labels = parse_gmail_labels(value.trim());
            msg.insert(
                "labels".into(),
                serde_json::Value::Array(
                    labels
                        .into_iter()
                        .map(serde_json::Value::String)
                        .collect(),
                ),
            );
            continue;
        }

        if let Some(value) = trimmed.strip_prefix("Preview:") {
            msg.insert(
                "preview".into(),
                serde_json::Value::String(value.trim().to_string()),
            );
            continue;
        }

        if let Some(value) = trimmed.strip_prefix("Link:") {
            msg.insert(
                "url".into(),
                serde_json::Value::String(value.trim().to_string()),
            );
            continue;
        }
    }

    if let Some(msg) = current.take() {
        messages.push(serde_json::Value::Object(msg));
    }

    messages
}

fn gmail_messages_from_payload(payload: &serde_json::Value) -> Vec<serde_json::Value> {
    if let Some(messages) = payload.get("messages").and_then(|v| v.as_array()) {
        return messages.clone();
    }

    if let Some(results) = payload.get("results").and_then(|v| v.as_array()) {
        return results.clone();
    }

    if let Some(rows) = payload.as_array() {
        return rows.clone();
    }

    if let Some(text) = payload.get("text").and_then(|v| v.as_str()) {
        let parsed = parse_gmail_messages_from_text(text);
        if !parsed.is_empty() {
            return parsed;
        }
    }

    if looks_like_gmail_message_object(payload) {
        return vec![payload.clone()];
    }

    Vec::new()
}

fn gmail_next_page_token(payload: &serde_json::Value) -> Option<String> {
    payload
        .get("nextPageToken")
        .or_else(|| payload.get("next_page_token"))
        .or_else(|| payload.get("nextPage"))
        .or_else(|| {
            payload
                .get("pagination")
                .and_then(|v| v.get("nextPageToken"))
        })
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(|token| token.to_string())
}

fn gmail_message_identifier(message: &serde_json::Value) -> Option<String> {
    ["id", "messageId", "message_id", "threadId", "thread_id"]
        .iter()
        .find_map(|key| {
            message
                .get(*key)
                .and_then(|v| v.as_str())
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        })
}

fn should_ignore_gmail_page_token_error(error: Option<&str>) -> bool {
    let Some(raw) = error else {
        return false;
    };

    let lower = raw.to_ascii_lowercase();
    let mentions_page_token = lower.contains("pagetoken") || lower.contains("page token");
    let looks_like_schema_error = lower.contains("unexpected parameter")
        || lower.contains("additional properties")
        || lower.contains("unknown")
        || lower.contains("invalid argument");

    mentions_page_token && looks_like_schema_error
}

fn find_string_field_recursive(value: &serde_json::Value, key: &str) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(found) = map
                .get(key)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                return Some(found.to_string());
            }

            for child in map.values() {
                if let Some(found) = find_string_field_recursive(child, key) {
                    return Some(found);
                }
            }

            None
        }
        serde_json::Value::Array(items) => {
            for item in items {
                if let Some(found) = find_string_field_recursive(item, key) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

fn extract_first_string_recursive(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| find_string_field_recursive(value, key))
}

fn extract_id_from_google_url(url: &str, marker: &str) -> Option<String> {
    let (_, rest) = url.split_once(marker)?;
    let id = rest
        .split('/')
        .next()
        .unwrap_or("")
        .split('?')
        .next()
        .unwrap_or("")
        .trim();
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

fn extract_google_resource_id(
    payload: &serde_json::Value,
    id_keys: &[&str],
    url_keys: &[&str],
    url_marker: &str,
) -> Option<String> {
    if let Some(id) = extract_first_string_recursive(payload, id_keys) {
        return Some(id);
    }

    for url_key in url_keys {
        if let Some(url) = find_string_field_recursive(payload, url_key) {
            if let Some(id) = extract_id_from_google_url(&url, url_marker) {
                return Some(id);
            }
        }
    }

    None
}

fn build_google_resource_url(resource_kind: &str, resource_id: &str) -> Option<String> {
    let id = resource_id.trim();
    if id.is_empty() {
        return None;
    }

    match resource_kind {
        "document" => Some(format!("https://docs.google.com/document/d/{id}/edit")),
        "spreadsheet" => Some(format!("https://docs.google.com/spreadsheets/d/{id}/edit")),
        "presentation" => Some(format!("https://docs.google.com/presentation/d/{id}/edit")),
        _ => None,
    }
}

fn calendar_param_str(params: &serde_json::Value, primary: &str, fallback: &str) -> String {
    params
        .get(primary)
        .and_then(|v| v.as_str())
        .or_else(|| params.get(fallback).and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string()
}

fn calendar_create_args(params: &serde_json::Value, alternate_shape: bool) -> serde_json::Value {
    let summary = params
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let start = calendar_param_str(params, "start", "startDateTime");
    let end = calendar_param_str(params, "end", "endDateTime");
    let description = params
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let location = params
        .get("location")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let mut args = if alternate_shape {
        serde_json::json!({
            "summary": summary,
            "startDateTime": start,
            "endDateTime": end,
            "description": description,
            "location": location,
        })
    } else {
        serde_json::json!({
            "summary": summary,
            "start": { "dateTime": start },
            "end": { "dateTime": end },
            "description": description,
            "location": location,
        })
    };

    if let Some(attendees) = params
        .get("attendees")
        .and_then(|v| v.as_array())
        .filter(|arr| !arr.is_empty())
    {
        args["attendees"] = serde_json::Value::Array(attendees.clone());
    }

    args
}

fn should_retry_calendar_with_alternate_shape(error: Option<&str>) -> bool {
    let Some(raw) = error else {
        return true;
    };
    let lower = raw.to_ascii_lowercase();
    if lower.contains("not connected") || lower.contains("mcp call failed") {
        return false;
    }
    if lower.contains("rate limit") || lower.contains("quota") {
        return false;
    }
    true
}

impl GwBridge {
    /// Inject the `account` field into the params object (required by every tool in
    /// `google-workspace-mcp`), then call the MCP tool.
    async fn mcp_call_raw(&self, tool: &str, mut args: serde_json::Value) -> ToolResult {
        // Ensure args is an object
        if !args.is_object() {
            args = serde_json::json!({});
        }
        let account = active_google_account();
        // Inject account — never overwrite if the caller already set it
        if let Some(obj) = args.as_object_mut() {
            obj.entry("account")
                .or_insert_with(|| serde_json::json!(account));
        }

        tracing::info!("[GW] mcp_call: tool='{}' account='{}'", tool, account);
        tracing::debug!("[GW] mcp_call args: {}", args);

        let guard = self.mcp.read().await;
        let client = match guard.as_ref() {
            Some(c) => c.clone(),
            None => {
                let msg = format!(
                    "Google Workspace is not connected. \
                     Run: npx google-workspace-mcp accounts add personal  \
                     Then restart KRIA. (tool={tool})"
                );
                tracing::warn!("[GW] {}", msg);
                return ToolResult {
                    success: false,
                    data: serde_json::Value::Null,
                    error: Some(msg),
                };
            }
        };
        drop(guard);

        match client.call_tool(tool, Some(args)).await {
            Ok(result) => {
                let text: String = result
                    .content
                    .iter()
                    .filter_map(|c| c.text.as_deref())
                    .collect::<Vec<_>>()
                    .join("\n");
                if result.is_error {
                    tracing::warn!(
                        "[GW] tool '{}' returned MCP error: {}",
                        tool,
                        &text[..text.len().min(300)]
                    );
                    // Parse well-known Google API errors into concise, actionable messages.
                    let user_error = parse_gw_error(&text);
                    ToolResult {
                        success: false,
                        data: envelope_result(
                            tool,
                            serde_json::json!({ "error": user_error.clone() }),
                            Some(&text),
                        ),
                        error: Some(user_error),
                    }
                } else {
                    tracing::info!("[GW] tool '{}' succeeded ({} chars)", tool, text.len());
                    ToolResult {
                        success: true,
                        data: serde_json::json!(text),
                        error: None,
                    }
                }
            }
            Err(e) => {
                tracing::error!("[GW] tool '{}' call error: {}", tool, e);
                ToolResult {
                    success: false,
                    data: serde_json::Value::Null,
                    error: Some(format!("MCP call failed: {e}")),
                }
            }
        }
    }

    async fn mcp_call(&self, tool: &str, args: serde_json::Value) -> ToolResult {
        let raw = self.mcp_call_raw(tool, args).await;
        if !raw.success {
            return raw;
        }

        let raw_text = raw.data.as_str().unwrap_or("");
        ToolResult {
            success: true,
            data: envelope_result(tool, parse_json_or_text(raw_text), Some(raw_text)),
            error: None,
        }
    }

    /// Fetch-then-buffer: MCP call → raw data → sidecar digest.
    async fn fetch_and_buffer(
        &self,
        mcp_tool: &str,
        mcp_args: serde_json::Value,
        sidecar_method: &str,
    ) -> ToolResult {
        tracing::debug!(
            "[GW] fetch_and_buffer: mcp_tool={} sidecar={}",
            mcp_tool,
            sidecar_method
        );
        let raw_result = self.mcp_call_raw(mcp_tool, mcp_args).await;
        if !raw_result.success {
            return raw_result;
        }
        let raw_text = raw_result.data.as_str().unwrap_or("").to_string();

        let buffer_params = serde_json::json!({ "raw": raw_result.data });
        match self.sidecar.request(sidecar_method, buffer_params).await {
            Ok(digest) => {
                tracing::info!("[GW] sidecar '{}' digest produced", sidecar_method);
                ToolResult {
                    success: true,
                    data: envelope_result(mcp_tool, digest, Some(&raw_text)),
                    error: None,
                }
            }
            Err(e) => {
                tracing::warn!(
                    "[GW] sidecar '{}' failed ({}), returning raw",
                    sidecar_method,
                    e
                );
                ToolResult {
                    success: true,
                    data: envelope_result(mcp_tool, parse_json_or_text(&raw_text), Some(&raw_text)),
                    error: None,
                }
            }
        }
    }

    async fn grounded_gmail_search(&self, query: String, requested_max: u64) -> ToolResult {
        let requested_count = requested_max.clamp(1, GMAIL_MAX_RESULTS_CAP);
        let page_size = requested_count.clamp(1, GMAIL_PAGE_SIZE_CAP);

        let mut pages_fetched = 0usize;
        let mut collected: Vec<serde_json::Value> = Vec::new();
        let mut seen_ids: HashSet<String> = HashSet::new();
        let mut raw_pages: Vec<String> = Vec::new();
        let mut warnings: Vec<String> = Vec::new();
        let mut partial_error: Option<String> = None;

        let mut page_token: Option<String> = None;
        let mut has_more_results = false;
        let mut page_cap_reached = false;

        while (collected.len() as u64) < requested_count {
            if pages_fetched >= GMAIL_MAX_PAGE_FETCHES {
                page_cap_reached = true;
                break;
            }

            let mut args = serde_json::json!({
                "query": query,
                "maxResults": page_size,
            });
            if let Some(token) = page_token.clone() {
                args["pageToken"] = serde_json::Value::String(token);
            }

            let page_result = self.mcp_call_raw("searchGmail", args).await;
            if !page_result.success {
                if pages_fetched > 0
                    && should_ignore_gmail_page_token_error(page_result.error.as_deref())
                {
                    warnings.push(
                        "Gmail pagination token replay was rejected by upstream schema; returning grounded results from fetched page(s).".into(),
                    );
                    break;
                }

                if collected.is_empty() {
                    return page_result;
                }

                partial_error = page_result.error.clone();
                break;
            }

            pages_fetched += 1;

            let raw_text = page_result.data.as_str().unwrap_or("").to_string();
            raw_pages.push(raw_text.clone());

            let parsed = parse_json_or_text(&raw_text);
            let page_messages = gmail_messages_from_payload(&parsed);
            for message in page_messages {
                if let Some(id) = gmail_message_identifier(&message) {
                    if seen_ids.insert(id) {
                        collected.push(message);
                    }
                } else {
                    collected.push(message);
                }

                if (collected.len() as u64) >= requested_count {
                    break;
                }
            }

            page_token = gmail_next_page_token(&parsed);
            has_more_results = page_token.is_some();

            if (collected.len() as u64) >= requested_count || !has_more_results {
                break;
            }
        }

        let returned_count = collected.len() as u64;
        if returned_count < requested_count {
            warnings.push(format!(
                "Requested {requested_count} message(s), but only {returned_count} grounded message(s) were returned by Gmail."
            ));
        }
        if page_cap_reached {
            warnings.push(
                "Stopped Gmail retrieval after reaching safety page cap before satisfying full requested count.".into(),
            );
        }

        let mut data = serde_json::json!({
            "query": query,
            "messages": collected,
            "count": returned_count,
            "requested_count": requested_count,
            "returned_count": returned_count,
            "fully_satisfied": returned_count >= requested_count,
            "pages_fetched": pages_fetched,
            "page_size": page_size,
            "has_more_results": has_more_results,
            "pagination_exhausted": !has_more_results,
        });

        if let Some(token) = page_token {
            data["next_page_token"] = serde_json::Value::String(token);
        }
        if let Some(err) = partial_error {
            data["partial_error"] = serde_json::Value::String(err);
        }
        if !warnings.is_empty() {
            data["warnings"] = serde_json::Value::Array(
                warnings
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect(),
            );
        }

        let raw_text = raw_pages.join("\n");
        ToolResult {
            success: true,
            data: envelope_result("searchGmail", data, Some(&raw_text)),
            error: None,
        }
    }
}

fn gmail_max_results(params: &serde_json::Value, default: u64) -> u64 {
    params
        .get("max_results")
        .and_then(|v| v.as_u64())
        .filter(|count| *count > 0)
    .map(|count| count.min(GMAIL_MAX_RESULTS_CAP))
        .unwrap_or(default)
}

fn normalize_gmail_inbox_query(query: Option<&str>) -> String {
    let trimmed = query.unwrap_or("").trim();
    if trimmed.is_empty() {
        return "in:inbox".into();
    }

    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("in:") {
        trimmed.to_string()
    } else {
        format!("in:inbox {trimmed}")
    }
}

// ── Error helpers ──────────────────────────────────────────────────────────────

/// Convert verbose Google API error messages into short, actionable strings.
///
/// Google errors for "API not enabled" are typically hundreds of characters long
/// and contain a URL to fix the issue.  This function extracts the key info so
/// KRIA's UI shows something readable.
fn parse_gw_error(raw: &str) -> String {
    // "accessNotConfigured" / "API has not been used" — API disabled in Cloud Console
    if raw.contains("accessNotConfigured")
        || raw.contains("has not been used")
        || raw.contains("is disabled")
    {
        // Try to extract the enable URL
        let url = raw
            .split_once("https://console")
            .map(|(_, rest)| {
                format!(
                    "https://console{}",
                    rest.split_whitespace().next().unwrap_or("")
                )
            })
            .unwrap_or_default();
        // Determine which API from the URL or the error text
        let api_name = if raw.contains("gmail") {
            "Gmail API"
        } else if raw.contains("calendar") {
            "Calendar API"
        } else if raw.contains("drive") {
            "Drive API"
        } else if raw.contains("docs") {
            "Docs API"
        } else if raw.contains("sheets") {
            "Sheets API"
        } else if raw.contains("slides") {
            "Slides API"
        } else {
            "Google API"
        };
        if url.is_empty() {
            return format!(
                "{api_name} is disabled in your Google Cloud project. \
                 Enable it at https://console.cloud.google.com/apis/library then restart KRIA."
            );
        }
        return format!(
            "{api_name} is disabled. Enable it at {url} \
             (wait ~1 min after enabling, then retry or restart KRIA)."
        );
    }

    // "invalid_grant" / token expired / revoked
    if raw.contains("invalid_grant")
        || raw.contains("Token has been expired")
        || raw.contains("Token has been revoked")
    {
        return "Google authentication token expired or revoked. \
                Re-run: bash scripts/setup_google_workspace.sh  then restart KRIA."
            .into();
    }

    // "insufficientPermissions" — scope missing
    if raw.contains("insufficientPermissions")
        || raw.contains("Request had insufficient authentication scopes")
    {
        return "Insufficient OAuth scopes. \
                Re-run: bash scripts/setup_google_workspace.sh  to refresh permissions, then restart KRIA.".into();
    }

    // "rateLimitExceeded" / quota
    if raw.contains("rateLimitExceeded") || raw.contains("quotaExceeded") {
        return "Google API rate limit or quota exceeded. Wait a minute and try again.".into();
    }

    // transient upstream proxy/gateway failure
    if raw.contains("Bad Gateway") || raw.contains("status code 502") {
        return "Google API temporarily unavailable (502 Bad Gateway). Retry in a few seconds."
            .into();
    }

    // Fallback: trim to 300 chars
    if raw.len() > 300 {
        format!("{}…", &raw[..300])
    } else {
        raw.to_string()
    }
}

// ── Gmail tools ────────────────────────────────────────────────────────────────
// Real tool names from `google-workspace-mcp` package (v2.x):
//   listGmailMessages, searchGmail, readGmailMessage,
//   createGmailDraft, sendGmailDraft, deleteGmailMessage

struct GwGmailInbox(GwBridge);
#[async_trait]
impl ToolHandler for GwGmailInbox {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        // searchGmail returns sender, subject, date, labels, preview, and IDs.
        // That is much more useful for "check my inbox" flows than listGmailMessages,
        // which only returns IDs and links.
        let query = normalize_gmail_inbox_query(params.get("query").and_then(|v| v.as_str()));
        let requested = gmail_max_results(&params, 10);
        self.0.grounded_gmail_search(query, requested).await
    }
}

struct GwGmailSearch(GwBridge);
#[async_trait]
impl ToolHandler for GwGmailSearch {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        // searchGmail: account, query, maxResults?
        let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let requested = gmail_max_results(&params, 10);
        self.0
            .grounded_gmail_search(query.to_string(), requested)
            .await
    }
}

struct GwGmailRead(GwBridge);
#[async_trait]
impl ToolHandler for GwGmailRead {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        // readGmailMessage: account, messageId
        let msg_id = params
            .get("message_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let args = serde_json::json!({ "messageId": msg_id });
        self.0.mcp_call("readGmailMessage", args).await
    }
}

struct GwGmailSend(GwBridge);
#[async_trait]
impl ToolHandler for GwGmailSend {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        // Safe send workflow: create draft first, then send it
        // Step 1: createGmailDraft
        let draft_args = serde_json::json!({
            "to": params.get("to").and_then(|v| v.as_str()).unwrap_or(""),
            "subject": params.get("subject").and_then(|v| v.as_str()).unwrap_or(""),
            "body": params.get("body").and_then(|v| v.as_str()).unwrap_or(""),
            "cc": params.get("cc").and_then(|v| v.as_str()).unwrap_or(""),
        });
        let draft_result = self.0.mcp_call("createGmailDraft", draft_args).await;
        if !draft_result.success {
            return draft_result;
        }
        // Extract draft_id from result (JSON string containing draftId field)
        let draft_text = draft_result.data.as_str().unwrap_or("");
        tracing::info!(
            "[GW] draft created: {}",
            &draft_text[..draft_text.len().min(200)]
        );
        // Step 2: sendGmailDraft using the draft_id
        // The result text from createGmailDraft typically contains the draft ID
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(draft_text) {
            if let Some(draft_id) = parsed.get("draftId").and_then(|v| v.as_str()) {
                return self
                    .0
                    .mcp_call("sendGmailDraft", serde_json::json!({ "draftId": draft_id }))
                    .await;
            }
        }
        // Fallback: return the draft result and let user know to send manually
        tracing::warn!("[GW] could not extract draftId from createGmailDraft response — draft created but not sent");
        ToolResult {
            success: false,
            data: draft_result.data,
            error: Some("Draft created but could not auto-send: draftId not found in response. Check Gmail drafts.".into()),
        }
    }
}

struct GwGmailDelete(GwBridge);
#[async_trait]
impl ToolHandler for GwGmailDelete {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let msg_id = params
            .get("message_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        self.0
            .mcp_call(
                "deleteGmailMessage",
                serde_json::json!({ "messageId": msg_id }),
            )
            .await
    }
}

// ── Calendar tools ─────────────────────────────────────────────────────────────
// Real tool names: listCalendarEvents, createCalendarEvent, deleteCalendarEvent

struct GwCalendarToday(GwBridge);
#[async_trait]
impl ToolHandler for GwCalendarToday {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        // listCalendarEvents with today's date range
        let now = chrono::Utc::now();
        let start = now
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .to_rfc3339();
        let end = now
            .date_naive()
            .and_hms_opt(23, 59, 59)
            .unwrap()
            .and_utc()
            .to_rfc3339();
        let args = serde_json::json!({ "timeMin": start, "timeMax": end, "maxResults": 50 });
        self.0.mcp_call("listCalendarEvents", args).await
    }
}

struct GwCalendarSearch(GwBridge);
#[async_trait]
impl ToolHandler for GwCalendarSearch {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let mut args = serde_json::json!({ "maxResults": 20 });
        if let Some(q) = params.get("query").and_then(|v| v.as_str()) {
            args["q"] = serde_json::json!(q);
        }
        if let Some(t) = params.get("time_min").and_then(|v| v.as_str()) {
            args["timeMin"] = serde_json::json!(t);
        }
        if let Some(t) = params.get("time_max").and_then(|v| v.as_str()) {
            args["timeMax"] = serde_json::json!(t);
        }
        self.0.mcp_call("listCalendarEvents", args).await
    }
}

struct GwCalendarCreate(GwBridge);
#[async_trait]
impl ToolHandler for GwCalendarCreate {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let primary_args = calendar_create_args(&params, false);
        let primary_result = self.0.mcp_call("createCalendarEvent", primary_args).await;
        if primary_result.success || !should_retry_calendar_with_alternate_shape(primary_result.error.as_deref()) {
            return primary_result;
        }

        tracing::warn!(
            "[GW] calendar create primary argument shape failed; retrying with alternate datetime shape"
        );
        let alternate_result = self
            .0
            .mcp_call("createCalendarEvent", calendar_create_args(&params, true))
            .await;
        if alternate_result.success {
            return alternate_result;
        }

        let merged_error = match (primary_result.error.as_deref(), alternate_result.error.as_deref()) {
            (Some(primary), Some(alternate)) => {
                format!("{primary} (alternate argument retry failed: {alternate})")
            }
            (Some(primary), None) => primary.to_string(),
            (None, Some(alternate)) => alternate.to_string(),
            (None, None) => "Calendar create failed for both supported argument shapes".to_string(),
        };

        ToolResult {
            success: false,
            data: alternate_result.data,
            error: Some(merged_error),
        }
    }
}

struct GwCalendarDelete(GwBridge);
#[async_trait]
impl ToolHandler for GwCalendarDelete {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let event_id = params
            .get("event_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        self.0
            .mcp_call(
                "deleteCalendarEvent",
                serde_json::json!({ "eventId": event_id }),
            )
            .await
    }
}

// ── Drive tools ────────────────────────────────────────────────────────────────
// Real tool names: searchGoogleDocs, listFolderContents, readGoogleDoc, deleteFile

struct GwDriveSearch(GwBridge);
#[async_trait]
impl ToolHandler for GwDriveSearch {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
        if query.trim().is_empty() || looks_like_drive_listing_phrase(query) {
            return self
                .0
                .fetch_and_buffer(
                    "listFolderContents",
                    serde_json::json!({}),
                    "google.summarize_drive_folder",
                )
                .await;
        }

        let args = serde_json::json!({ "query": query });
        self.0
            .fetch_and_buffer("searchGoogleDocs", args, "google.summarize_drive_folder")
            .await
    }
}

struct GwDriveList(GwBridge);
#[async_trait]
impl ToolHandler for GwDriveList {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let folder_id = params.get("folder_id").and_then(|v| v.as_str());
        let args = if let Some(id) = folder_id {
            serde_json::json!({ "folderId": id })
        } else {
            serde_json::json!({})
        };
        self.0
            .fetch_and_buffer("listFolderContents", args, "google.summarize_drive_folder")
            .await
    }
}

struct GwDriveRead(GwBridge);
#[async_trait]
impl ToolHandler for GwDriveRead {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        // Try as a Doc first; format=text is safe for all readable files
        let file_id = params.get("file_id").and_then(|v| v.as_str()).unwrap_or("");
        let args = serde_json::json!({ "documentId": file_id, "format": "text" });
        self.0.mcp_call("readGoogleDoc", args).await
    }
}

struct GwDriveDelete(GwBridge);
#[async_trait]
impl ToolHandler for GwDriveDelete {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let file_id = params.get("file_id").and_then(|v| v.as_str()).unwrap_or("");
        self.0
            .mcp_call("deleteFile", serde_json::json!({ "fileId": file_id }))
            .await
    }
}

// ── Docs tools ─────────────────────────────────────────────────────────────────
// Real tool names: readGoogleDoc, createDocument, appendToGoogleDoc

struct GwDocsRead(GwBridge);
#[async_trait]
impl ToolHandler for GwDocsRead {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let doc_id = params
            .get("document_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let args = serde_json::json!({ "documentId": doc_id, "format": "markdown" });
        self.0
            .fetch_and_buffer("readGoogleDoc", args, "google.extract_doc")
            .await
    }
}

struct GwDocsCreate(GwBridge);
#[async_trait]
impl ToolHandler for GwDocsCreate {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let title = params
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Untitled");

        let create_result = self
            .0
            .mcp_call_raw("createDocument", serde_json::json!({ "title": title }))
            .await;
        if !create_result.success {
            return create_result;
        }

        let create_raw = create_result.data.as_str().unwrap_or("").to_string();
        let create_data = parse_json_or_text(&create_raw);

        let document_id = extract_google_resource_id(
            &create_data,
            &["documentId", "document_id", "id"],
            &["url", "documentUrl", "document_link", "link", "webViewLink"],
            "/document/d/",
        );

        let mut result_data = serde_json::json!({
            "resource": "document",
            "title": title,
            "status": "created_unverified",
            "verified": false,
            "create": create_data,
            "document_id": document_id,
            "url": document_id
                .as_deref()
                .and_then(|id| build_google_resource_url("document", id)),
        });

        if let Some(id) = document_id {
            let verify_result = self
                .0
                .mcp_call_raw(
                    "readGoogleDoc",
                    serde_json::json!({ "documentId": id, "format": "markdown" }),
                )
                .await;

            if verify_result.success {
                result_data["status"] = serde_json::json!("created_verified");
                result_data["verified"] = serde_json::json!(true);
                result_data["verify"] = parse_json_or_text(verify_result.data.as_str().unwrap_or(""));
            } else {
                result_data["verification_error"] = serde_json::json!(
                    verify_result
                        .error
                        .unwrap_or_else(|| "Document verification failed after create".into())
                );
            }
        } else {
            result_data["verification_error"] = serde_json::json!(
                "Could not extract document ID from create response for post-create verification"
            );
        }

        ToolResult {
            success: true,
            data: envelope_result("createDocument", result_data, Some(&create_raw)),
            error: None,
        }
    }
}

struct GwDocsEdit(GwBridge);
#[async_trait]
impl ToolHandler for GwDocsEdit {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let doc_id = params
            .get("document_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
        // Default to append operation
        self.0
            .mcp_call(
                "appendToGoogleDoc",
                serde_json::json!({ "documentId": doc_id, "text": text }),
            )
            .await
    }
}

// ── Sheets tools ───────────────────────────────────────────────────────────────
// Real tool names: readSpreadsheet, createSpreadsheet, writeSpreadsheet

struct GwSheetsRead(GwBridge);
#[async_trait]
impl ToolHandler for GwSheetsRead {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let id = params
            .get("spreadsheet_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let mut args = serde_json::json!({ "spreadsheetId": id });
        if let Some(r) = params.get("range").and_then(|v| v.as_str()) {
            args["range"] = serde_json::json!(r);
        }
        self.0
            .fetch_and_buffer("readSpreadsheet", args, "google.extract_sheet")
            .await
    }
}

struct GwSheetsCreate(GwBridge);
#[async_trait]
impl ToolHandler for GwSheetsCreate {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let title = params
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Untitled");

        let create_result = self
            .0
            .mcp_call_raw("createSpreadsheet", serde_json::json!({ "title": title }))
            .await;
        if !create_result.success {
            return create_result;
        }

        let create_raw = create_result.data.as_str().unwrap_or("").to_string();
        let create_data = parse_json_or_text(&create_raw);

        let spreadsheet_id = extract_google_resource_id(
            &create_data,
            &["spreadsheetId", "spreadsheet_id", "id"],
            &["url", "spreadsheetUrl", "spreadsheet_link", "link", "webViewLink"],
            "/spreadsheets/d/",
        );

        let mut result_data = serde_json::json!({
            "resource": "spreadsheet",
            "title": title,
            "status": "created_unverified",
            "verified": false,
            "create": create_data,
            "spreadsheet_id": spreadsheet_id,
            "url": spreadsheet_id
                .as_deref()
                .and_then(|id| build_google_resource_url("spreadsheet", id)),
        });

        if let Some(id) = spreadsheet_id {
            let verify_result = self
                .0
                .mcp_call_raw(
                    "readSpreadsheet",
                    serde_json::json!({ "spreadsheetId": id }),
                )
                .await;

            if verify_result.success {
                result_data["status"] = serde_json::json!("created_verified");
                result_data["verified"] = serde_json::json!(true);
                result_data["verify"] = parse_json_or_text(verify_result.data.as_str().unwrap_or(""));
            } else {
                result_data["verification_error"] = serde_json::json!(
                    verify_result
                        .error
                        .unwrap_or_else(|| "Spreadsheet verification failed after create".into())
                );
            }
        } else {
            result_data["verification_error"] = serde_json::json!(
                "Could not extract spreadsheet ID from create response for post-create verification"
            );
        }

        ToolResult {
            success: true,
            data: envelope_result("createSpreadsheet", result_data, Some(&create_raw)),
            error: None,
        }
    }
}

struct GwSheetsEdit(GwBridge);
#[async_trait]
impl ToolHandler for GwSheetsEdit {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let id = params
            .get("spreadsheet_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let range = params.get("range").and_then(|v| v.as_str()).unwrap_or("A1");
        let values_str = params
            .get("values")
            .and_then(|v| v.as_str())
            .unwrap_or("[]");
        let values: serde_json::Value =
            serde_json::from_str(values_str).unwrap_or(serde_json::json!([]));
        self.0
            .mcp_call(
                "writeSpreadsheet",
                serde_json::json!({
                    "spreadsheetId": id, "range": range, "values": values
                }),
            )
            .await
    }
}

// ── Slides tools ───────────────────────────────────────────────────────────────
// Real tool names: readPresentation, createPresentation

struct GwSlidesRead(GwBridge);
#[async_trait]
impl ToolHandler for GwSlidesRead {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let id = params
            .get("presentation_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        self.0
            .fetch_and_buffer(
                "readPresentation",
                serde_json::json!({ "presentationId": id }),
                "google.extract_slides",
            )
            .await
    }
}

struct GwSlidesCreate(GwBridge);
#[async_trait]
impl ToolHandler for GwSlidesCreate {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let title = params
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Untitled");

        let create_result = self
            .0
            .mcp_call_raw("createPresentation", serde_json::json!({ "title": title }))
            .await;
        if !create_result.success {
            return create_result;
        }

        let create_raw = create_result.data.as_str().unwrap_or("").to_string();
        let create_data = parse_json_or_text(&create_raw);

        let presentation_id = extract_google_resource_id(
            &create_data,
            &["presentationId", "presentation_id", "id"],
            &["url", "presentationUrl", "presentation_link", "link", "webViewLink"],
            "/presentation/d/",
        );

        let mut result_data = serde_json::json!({
            "resource": "presentation",
            "title": title,
            "status": "created_unverified",
            "verified": false,
            "create": create_data,
            "presentation_id": presentation_id,
            "url": presentation_id
                .as_deref()
                .and_then(|id| build_google_resource_url("presentation", id)),
        });

        if let Some(id) = presentation_id {
            let verify_result = self
                .0
                .mcp_call_raw(
                    "readPresentation",
                    serde_json::json!({ "presentationId": id }),
                )
                .await;

            if verify_result.success {
                result_data["status"] = serde_json::json!("created_verified");
                result_data["verified"] = serde_json::json!(true);
                result_data["verify"] = parse_json_or_text(verify_result.data.as_str().unwrap_or(""));
            } else {
                result_data["verification_error"] = serde_json::json!(
                    verify_result
                        .error
                        .unwrap_or_else(|| "Presentation verification failed after create".into())
                );
            }
        } else {
            result_data["verification_error"] = serde_json::json!(
                "Could not extract presentation ID from create response for post-create verification"
            );
        }

        ToolResult {
            success: true,
            data: envelope_result("createPresentation", result_data, Some(&create_raw)),
            error: None,
        }
    }
}

// ── Forms tools ───────────────────────────────────────────────────────────────
// Real tool names: listForms, createForm

struct GwFormsList(GwBridge);
#[async_trait]
impl ToolHandler for GwFormsList {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let mut args = serde_json::json!({});
        if let Some(query) = params.get("query").and_then(|v| v.as_str()) {
            args["query"] = serde_json::json!(query);
        }
        self.0.mcp_call("listForms", args).await
    }
}

struct GwFormsCreate(GwBridge);
#[async_trait]
impl ToolHandler for GwFormsCreate {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let title = params
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Untitled Form");
        self.0
            .mcp_call("createForm", serde_json::json!({ "title": title }))
            .await
    }
}

// ── Registration ───────────────────────────────────────────────────────────────

/// Register all Google Workspace tools.
///
/// Always registers all curated Google Workspace tools regardless of whether the MCP server is up.
/// Pass the `GwClientRef` returned by `new_client_ref()`; call `set_client()`
/// after the MCP server connects so handlers start forwarding requests.
pub fn register(reg: &ToolRegistry, mcp_ref: GwClientRef, sidecar: Arc<SidecarBridge>) {
    tracing::info!(
        "[GW] registering Google Workspace tools (account source=KRIA_GW_ACCOUNT, lazy MCP ref)"
    );

    let gw = GwBridge { mcp: mcp_ref, sidecar };

    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        // ── Ambient (always mounted) ─────────────────────
        (
            ToolDef {
                name: "gw_gmail_inbox".into(),
                description: "List recent emails from Gmail inbox. USE THIS to check inbox, see recent mail, or list emails. Returns sender, subject, date, preview, labels, and message IDs.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Green,
                min_tier: "lite",
                parameters: vec![
                    param("query", "string", "Optional Gmail search filter (e.g. 'is:unread')", false),
                    param("max_results", "integer", "Maximum messages to return (default 10, max 200)", false),
                ],
            },
            Arc::new(GwGmailInbox(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_gmail_search".into(),
                description: "Search Gmail with a query string (same syntax as Gmail search bar). Use for filtering by sender, subject, label, date, etc.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Green,
                min_tier: "lite",
                parameters: vec![
                    param("query", "string", "Gmail search query (e.g. 'from:boss subject:report')", true),
                    param("max_results", "integer", "Maximum messages to return (default 10, max 200)", false),
                ],
            },
            Arc::new(GwGmailSearch(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_gmail_read".into(),
                description: "Read the FULL content of a single Gmail message. Requires the message_id obtained from gw_gmail_inbox or gw_gmail_search. Do NOT use this to list or check inbox.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Green,
                min_tier: "lite",
                parameters: vec![
                    param("message_id", "string", "Gmail message ID", true),
                ],
            },
            Arc::new(GwGmailRead(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_calendar_today".into(),
                description: "Get today's calendar events from Google Calendar.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Green,
                min_tier: "lite",
                parameters: vec![],
            },
            Arc::new(GwCalendarToday(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_calendar_search".into(),
                description: "Search Google Calendar events by keyword or date range.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Green,
                min_tier: "lite",
                parameters: vec![
                    param("query", "string", "Search text for event titles/descriptions", false),
                    param("time_min", "string", "Start of time range (ISO 8601)", false),
                    param("time_max", "string", "End of time range (ISO 8601)", false),
                ],
            },
            Arc::new(GwCalendarSearch(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_drive_search".into(),
                description: "Search Google Drive files by name or content.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Green,
                min_tier: "lite",
                parameters: vec![
                    param("query", "string", "Search query (supports Drive search operators)", true),
                ],
            },
            Arc::new(GwDriveSearch(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_drive_list".into(),
                description: "List files in a Google Drive folder.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Green,
                min_tier: "lite",
                parameters: vec![
                    param("folder_id", "string", "Drive folder ID (omit for root)", false),
                ],
            },
            Arc::new(GwDriveList(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_drive_read".into(),
                description: "Read content of a Google Drive file / Google Doc by ID.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Green,
                min_tier: "lite",
                parameters: vec![
                    param("file_id", "string", "Google Drive file or Google Doc ID", true),
                ],
            },
            Arc::new(GwDriveRead(gw.clone())),
        ),

        // ── Docs group (on-demand mount) ─────────────────
        (
            ToolDef {
                name: "gw_docs_read".into(),
                description: "Read a Google Doc by ID (markdown format).".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Green,
                min_tier: "lite",
                parameters: vec![
                    param("document_id", "string", "Google Docs document ID", true),
                ],
            },
            Arc::new(GwDocsRead(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_docs_create".into(),
                description: "Create a new Google Doc.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Yellow,
                min_tier: "lite",
                parameters: vec![
                    param("title", "string", "Document title", true),
                ],
            },
            Arc::new(GwDocsCreate(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_docs_edit".into(),
                description: "Append text to an existing Google Doc.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Yellow,
                min_tier: "lite",
                parameters: vec![
                    param("document_id", "string", "Google Docs document ID", true),
                    param("text", "string", "Text to append", true),
                ],
            },
            Arc::new(GwDocsEdit(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_sheets_read".into(),
                description: "Read a Google Spreadsheet by ID.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Green,
                min_tier: "lite",
                parameters: vec![
                    param("spreadsheet_id", "string", "Google Sheets spreadsheet ID", true),
                    param("range", "string", "Cell range like 'Sheet1!A1:D10' (optional)", false),
                ],
            },
            Arc::new(GwSheetsRead(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_sheets_create".into(),
                description: "Create a new Google Spreadsheet.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Yellow,
                min_tier: "lite",
                parameters: vec![
                    param("title", "string", "Spreadsheet title", true),
                ],
            },
            Arc::new(GwSheetsCreate(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_sheets_edit".into(),
                description: "Write data to a Google Sheet range.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Yellow,
                min_tier: "lite",
                parameters: vec![
                    param("spreadsheet_id", "string", "Google Sheets spreadsheet ID", true),
                    param("range", "string", "Target cell range (e.g. 'Sheet1!A1:C3')", true),
                    param("values", "string", "JSON array of row arrays, e.g. [[\"a\",\"b\"],[\"c\",\"d\"]]", true),
                ],
            },
            Arc::new(GwSheetsEdit(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_slides_read".into(),
                description: "Read a Google Slides presentation by ID.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Green,
                min_tier: "lite",
                parameters: vec![
                    param("presentation_id", "string", "Google Slides presentation ID", true),
                ],
            },
            Arc::new(GwSlidesRead(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_slides_create".into(),
                description: "Create a new Google Slides presentation.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Yellow,
                min_tier: "lite",
                parameters: vec![
                    param("title", "string", "Presentation title", true),
                ],
            },
            Arc::new(GwSlidesCreate(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_forms_list".into(),
                description: "List Google Forms (optionally filtered by query).".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Green,
                min_tier: "lite",
                parameters: vec![
                    param("query", "string", "Optional search query for forms", false),
                ],
            },
            Arc::new(GwFormsList(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_forms_create".into(),
                description: "Create a new Google Form.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Yellow,
                min_tier: "lite",
                parameters: vec![
                    param("title", "string", "Google Form title", true),
                ],
            },
            Arc::new(GwFormsCreate(gw.clone())),
        ),

        // ── Admin group (on-demand mount) ────────────────
        (
            ToolDef {
                name: "gw_gmail_send".into(),
                description: "Send an email via Gmail (creates draft then sends). Requires HITL approval.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Red,
                min_tier: "lite",
                parameters: vec![
                    param("to", "string", "Recipient email address", true),
                    param("subject", "string", "Email subject line", true),
                    param("body", "string", "Email body (plain text)", true),
                    param("cc", "string", "CC recipients (comma-separated)", false),
                ],
            },
            Arc::new(GwGmailSend(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_gmail_delete".into(),
                description: "Delete a Gmail message. Requires HITL approval.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Red,
                min_tier: "lite",
                parameters: vec![
                    param("message_id", "string", "Gmail message ID to delete", true),
                ],
            },
            Arc::new(GwGmailDelete(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_drive_delete".into(),
                description: "Delete a file from Google Drive. Requires HITL approval.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Red,
                min_tier: "lite",
                parameters: vec![
                    param("file_id", "string", "Google Drive file ID to delete", true),
                ],
            },
            Arc::new(GwDriveDelete(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_calendar_create".into(),
                description: "Create a new Google Calendar event. Requires HITL approval.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Yellow,
                min_tier: "lite",
                parameters: vec![
                    param("summary", "string", "Event title", true),
                    param("start", "string", "Start time (ISO 8601)", true),
                    param("end", "string", "End time (ISO 8601)", true),
                    param("description", "string", "Event description", false),
                    param("location", "string", "Event location", false),
                ],
            },
            Arc::new(GwCalendarCreate(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_calendar_delete".into(),
                description: "Delete a Google Calendar event. Requires HITL approval.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Red,
                min_tier: "lite",
                parameters: vec![
                    param("event_id", "string", "Calendar event ID to delete", true),
                ],
            },
            Arc::new(GwCalendarDelete(gw.clone())),
        ),
    ];

    let gw_tool_count = tools.len();
    for (def, handler) in tools {
        tracing::debug!("[GW] registering tool: {}", def.name);
        reg.register(def, handler);
    }

    tracing::info!(
        "[GW] {} Google Workspace tools registered (MCP connection pending)",
        gw_tool_count
    );
}

#[cfg(test)]
mod tests {
    use super::{
        build_google_resource_url, calendar_create_args, extract_google_resource_id,
        gmail_max_results, gmail_messages_from_payload, gmail_next_page_token,
        looks_like_drive_listing_phrase, normalize_gmail_inbox_query,
        parse_gmail_messages_from_text,
    };

    #[test]
    fn gmail_inbox_query_defaults_to_inbox() {
        assert_eq!(normalize_gmail_inbox_query(None), "in:inbox");
        assert_eq!(
            normalize_gmail_inbox_query(Some("is:unread")),
            "in:inbox is:unread"
        );
        assert_eq!(normalize_gmail_inbox_query(Some("in:sent")), "in:sent");
    }

    #[test]
    fn gmail_max_results_uses_param_and_caps_values() {
        assert_eq!(gmail_max_results(&serde_json::json!({}), 10), 10);
        assert_eq!(
            gmail_max_results(&serde_json::json!({"max_results": 3}), 10),
            3
        );
        assert_eq!(
            gmail_max_results(&serde_json::json!({"max_results": 500}), 10),
            200
        );
    }

    #[test]
    fn gmail_helpers_extract_messages_and_next_page_token() {
        let payload = serde_json::json!({
            "messages": [
                {"id": "m1", "subject": "A"},
                {"id": "m2", "subject": "B"}
            ],
            "nextPageToken": "token-2"
        });

        let messages = gmail_messages_from_payload(&payload);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["id"], "m1");
        assert_eq!(gmail_next_page_token(&payload).as_deref(), Some("token-2"));
    }

    #[test]
    fn gmail_helpers_parse_text_search_results_into_messages() {
        let raw = r#"
**Search Results for:** "in:inbox is:unread"
Total estimate: 201 messages

**1. Invitation: Kria Presentation Pitching**
   From: obaidullah zeeshan <obaidzeeshan.official@gmail.com>
   Date: Sat, 18 Apr 2026 05:49:26 +0000
   ID: 19d9f230a2e500b1
   Labels: UNREAD, IMPORTANT, CATEGORY_PERSONAL, INBOX
   Preview: You have been invited
   Link: https://mail.google.com/mail/?authuser=personal#all/19d9f230a2e500b1

**2. Meet the new Make Grid**
   From: Make <info@make.com>
   Date: Fri, 10 Apr 2026 10:47:32 +0000
   ID: 19d770115374cefc
   Labels: CATEGORY_PROMOTIONS, UNREAD, INBOX
   Preview: You asked, we delivered
   Link: https://mail.google.com/mail/?authuser=personal#all/19d770115374cefc
"#;

        let parsed = parse_gmail_messages_from_text(raw);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0]["id"], "19d9f230a2e500b1");
        assert_eq!(parsed[1]["id"], "19d770115374cefc");
        assert!(parsed[0].get("category").is_none());
        assert!(parsed[1].get("category").is_none());
        assert_eq!(parsed[0]["labels"][0], "UNREAD");
        assert_eq!(parsed[1]["labels"][0], "CATEGORY_PROMOTIONS");

        let wrapped = serde_json::json!({ "text": raw });
        let wrapped_parsed = gmail_messages_from_payload(&wrapped);
        assert_eq!(wrapped_parsed.len(), 2);
    }

    #[test]
    fn drive_listing_phrase_detector_distinguishes_list_from_search() {
        assert!(looks_like_drive_listing_phrase("list files in my google drive"));
        assert!(looks_like_drive_listing_phrase("show drive contents"));
        assert!(!looks_like_drive_listing_phrase("search drive for quarterly report"));
    }

    #[test]
    fn calendar_create_args_supports_primary_and_alternate_shapes() {
        let params = serde_json::json!({
            "summary": "Google Meet",
            "start": "2026-04-19T09:30:00Z",
            "end": "2026-04-19T10:00:00Z",
            "attendees": [{"email":"example@domain.com"}],
        });

        let primary = calendar_create_args(&params, false);
        assert_eq!(primary["start"]["dateTime"], "2026-04-19T09:30:00Z");
        assert_eq!(primary["end"]["dateTime"], "2026-04-19T10:00:00Z");
        assert_eq!(primary["attendees"][0]["email"], "example@domain.com");

        let alternate = calendar_create_args(&params, true);
        assert_eq!(alternate["startDateTime"], "2026-04-19T09:30:00Z");
        assert_eq!(alternate["endDateTime"], "2026-04-19T10:00:00Z");
        assert_eq!(alternate["attendees"][0]["email"], "example@domain.com");
    }

    #[test]
    fn extract_google_resource_id_supports_direct_and_url_based_ids() {
        let direct_payload = serde_json::json!({
            "documentId": "doc_direct_id"
        });
        assert_eq!(
            extract_google_resource_id(
                &direct_payload,
                &["documentId", "id"],
                &["url", "link"],
                "/document/d/"
            )
            .as_deref(),
            Some("doc_direct_id")
        );

        let url_payload = serde_json::json!({
            "result": {
                "link": "https://docs.google.com/document/d/doc_from_url/edit?usp=sharing"
            }
        });
        assert_eq!(
            extract_google_resource_id(
                &url_payload,
                &["documentId", "id"],
                &["url", "link"],
                "/document/d/"
            )
            .as_deref(),
            Some("doc_from_url")
        );
    }

    #[test]
    fn build_google_resource_url_generates_edit_links() {
        assert_eq!(
            build_google_resource_url("document", "abc123").as_deref(),
            Some("https://docs.google.com/document/d/abc123/edit")
        );
        assert_eq!(
            build_google_resource_url("spreadsheet", "sheet456").as_deref(),
            Some("https://docs.google.com/spreadsheets/d/sheet456/edit")
        );
        assert_eq!(
            build_google_resource_url("presentation", "slide789").as_deref(),
            Some("https://docs.google.com/presentation/d/slide789/edit")
        );
    }
}
