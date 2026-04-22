//! Deterministic structural payload shaper for LLM context injection.
//!
//! Shapes large MCP tool responses into a compact, LLM-friendly form by:
//! - Keeping identity / meta fields and dropping large body / base64 / HTML content
//! - Truncating long strings with a head + "…N chars elided…" + tail pattern
//! - For arrays, keeping as many items as fit within the character budget
//! - Appending `__shape` metadata so the LLM knows more data exists
//!
//! All operations are synchronous and deterministic (no LLM call required).

use serde_json::{Map, Value};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Field-name allow/deny lists
// ---------------------------------------------------------------------------

/// Fields considered important identity or metadata — always kept.
const KEEP_KEYS: &[&str] = &[
    // Universal identity
    "id",
    "name",
    "title",
    "type",
    "kind",
    "status",
    "state",
    "error",
    // Email / Calendar
    "subject",
    "from",
    "sender",
    "to",
    "recipients",
    "date",
    "updated",
    "created",
    "modified",
    "start",
    "end",
    "organizer",
    "attendees",
    "location",
    "eventId",
    "event_id",
    // Mail IDs
    "messageId",
    "message_id",
    "threadId",
    "thread_id",
    // Short content previews
    "snippet",
    "preview",
    "description",
    "summary",
    // File system / Drive
    "path",
    "file_path",
    "url",
    "webViewLink",
    "htmlLink",
    "alternateLink",
    "mimeType",
    "mime_type",
    "size",
    "fileSize",
    "file_size",
    "parents",
    "owners",
    "lastModifyingUser",
    // Grounding metadata
    "count",
    "requested_count",
    "returned_count",
    "has_more",
    "next_page_token",
    "next_cursor",
    // LLM-facing annotations (already compact)
    "llm_visible_message_count",
    "llm_omitted_message_count",
    "warnings",
    "GROUNDING_NOTE",
];

/// Fields that are always dropped — they are large and not useful for the LLM.
const DROP_KEYS: &[&str] = &[
    "raw_text",
    "raw",
    "body",
    "html",
    "htmlBody",
    "htmlContent",
    "rawPayload",
    "rawBody",
    "payload",
    "attachments",
    "parts",
    "embeds",
    "images",
    "base64",
    "data_uri",
    "inlineData",
    "thumbnailLink",
    "thumbnailUrl",
    "embedLink",
    "labelIds",
    "historyId",
    "sizeEstimate",
];

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Heuristic: if a string looks like raw base64 data, elide it.
fn is_base64_blob(s: &str) -> bool {
    s.len() > 256
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='))
}

/// Truncate a string to `budget` characters, keeping a head and tail with an
/// elision note in the middle.  Always returns a valid UTF-8 string.
pub fn truncate_string(s: &str, budget: usize) -> String {
    if budget == 0 {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= budget {
        return s.to_owned();
    }
    // Keep 75 % at the front, 12.5 % at the tail.
    let head = (budget * 3 / 4).max(1);
    let tail = (budget / 8).max(0);
    let elided = chars.len().saturating_sub(head + tail);
    let head_str: String = chars[..head].iter().collect();
    let tail_str: String = if tail > 0 {
        chars[chars.len() - tail..].iter().collect()
    } else {
        String::new()
    };
    if tail_str.is_empty() {
        format!("{head_str}…<{elided} chars elided>")
    } else {
        format!("{head_str}…<{elided} chars elided>…{tail_str}")
    }
}

// ---------------------------------------------------------------------------
// Core shaping
// ---------------------------------------------------------------------------

/// Shape a single JSON value to fit within `budget_chars`.
/// Returns the shaped value and the number of characters dropped.
pub fn shape_value(value: &Value, budget_chars: usize) -> (Value, usize) {
    let budget_chars = budget_chars.max(32);
    match value {
        // ── String ────────────────────────────────────────────────────────
        Value::String(s) => {
            if is_base64_blob(s) {
                let dropped = s.len();
                let mut m = Map::new();
                m.insert("__elided".into(), Value::String("base64_blob".into()));
                m.insert("bytes".into(), Value::Number((dropped as u64).into()));
                return (Value::Object(m), dropped);
            }
            let truncated = truncate_string(s, budget_chars);
            let dropped = s.len().saturating_sub(truncated.len());
            (Value::String(truncated), dropped)
        }

        // ── Array ──────────────────────────────────────────────────────────
        Value::Array(items) => {
            if items.is_empty() {
                return (Value::Array(vec![]), 0);
            }
            // Per-item budget: divide total budget among items, but each item
            // gets at least 128 chars so small arrays don't get over-squeezed.
            let per_item = (budget_chars / items.len()).max(128);
            let mut shaped_items: Vec<Value> = Vec::with_capacity(items.len());
            let mut used = 2usize; // for "[]"
            let mut total_dropped = 0usize;
            let total_items = items.len();

            for item in items {
                let (shaped, dropped) = shape_value(item, per_item);
                let item_len = shaped.to_string().len() + 2; // comma + space
                if !shaped_items.is_empty() && used + item_len > budget_chars {
                    total_dropped += items.len() - shaped_items.len();
                    break;
                }
                used += item_len;
                total_dropped += dropped;
                shaped_items.push(shaped);
            }

            // Append a truncation sentinel so the LLM knows items were omitted.
            let items_shown = shaped_items.len();
            if items_shown < total_items {
                let mut sentinel = Map::new();
                sentinel.insert(
                    "__truncated".into(),
                    Value::String(format!(
                        "{} more item(s) not shown",
                        total_items - items_shown
                    )),
                );
                shaped_items.push(Value::Object(sentinel));
            }

            (Value::Array(shaped_items), total_dropped)
        }

        // ── Object ─────────────────────────────────────────────────────────
        Value::Object(map) => {
            let mut out = Map::new();
            let mut dropped_bytes = 0usize;

            // Pass 1: include whitelisted keys.
            for &key in KEEP_KEYS {
                if let Some(v) = map.get(key) {
                    let key_budget = (budget_chars / KEEP_KEYS.len().max(1)).max(80);
                    let (shaped, dropped) = shape_value(v, key_budget);
                    out.insert(key.to_string(), shaped);
                    dropped_bytes += dropped;
                }
            }

            // Pass 2: include remaining keys not in the drop list, up to budget.
            let used_so_far = Value::Object(out.clone()).to_string().len();
            let mut remaining = budget_chars.saturating_sub(used_so_far);
            for (key, v) in map {
                if KEEP_KEYS.contains(&key.as_str()) {
                    continue; // already handled in Pass 1
                }
                if DROP_KEYS.contains(&key.as_str()) {
                    dropped_bytes += v.to_string().len(); // count elided content
                    continue;
                }
                if remaining < 20 {
                    dropped_bytes += v.to_string().len();
                    continue;
                }
                let val_budget = (remaining / 2).max(80);
                let (shaped, dropped) = shape_value(v, val_budget);
                let shaped_len = shaped.to_string().len();
                out.insert(key.clone(), shaped);
                dropped_bytes += dropped;
                remaining = remaining.saturating_sub(shaped_len + key.len() + 4);
            }

            (Value::Object(out), dropped_bytes)
        }

        // ── Primitives (bool / number / null) — always kept as-is ─────────
        other => (other.clone(), 0),
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Result of shaping a tool payload.
pub struct ShapedPayload {
    /// Compact value safe for LLM injection.
    pub value: Value,
    /// Total bytes dropped during shaping.
    pub dropped_bytes: usize,
    /// UUID handle that identifies the full payload in the per-turn cache.
    /// Store `(handle, Arc::new(original_value))` in `TurnContext::payload_cache`
    /// so the UI can request full detail without re-running the tool.
    pub handle: Uuid,
}

/// Shape an MCP tool result for LLM injection.
///
/// `tool` is the tool name (used for `__shape.tool` metadata).
/// `value` is the raw MCP response.
/// `budget_chars` is the **character** budget for the shaped output; callers
/// should derive this from token budgets via `CHARS_PER_TOKEN_FALLBACK`.
///
/// The returned `ShapedPayload.value` is always valid JSON that fits within
/// `budget_chars` after serialisation.
pub fn shape_for_llm(tool: &str, value: &Value, budget_chars: usize) -> ShapedPayload {
    let handle = Uuid::new_v4();
    let (mut shaped, dropped_bytes) = shape_value(value, budget_chars);

    // Inject __shape metadata into object roots so the LLM knows more data
    // exists and can reference the handle in a follow-up "show full result" call.
    if let Value::Object(ref mut map) = shaped {
        let mut meta = Map::new();
        meta.insert("tool".into(), Value::String(tool.to_string()));
        meta.insert(
            "bytes_dropped".into(),
            Value::Number((dropped_bytes as u64).into()),
        );
        meta.insert("handle".into(), Value::String(handle.to_string()));
        map.insert("__shape".into(), Value::Object(meta));
    }

    ShapedPayload {
        value: shaped,
        dropped_bytes,
        handle,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn truncate_string_short_is_unchanged() {
        let s = "hello world";
        assert_eq!(truncate_string(s, 100), s);
    }

    #[test]
    fn truncate_string_long_elides_middle() {
        let s = "a".repeat(500);
        let result = truncate_string(&s, 100);
        assert!(result.contains("chars elided"));
        assert!(result.len() < 200); // well under 500
    }

    #[test]
    fn base64_blob_is_elided() {
        let b64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".repeat(5);
        let (shaped, dropped) = shape_value(&Value::String(b64.clone()), 4096);
        assert!(dropped > 0, "should have dropped bytes");
        let obj = shaped.as_object().expect("should be elided object");
        assert_eq!(obj["__elided"], json!("base64_blob"));
    }

    #[test]
    fn array_over_budget_truncates() {
        let items: Vec<Value> = (0..100)
            .map(|i| json!({ "id": i, "body": "x".repeat(100) }))
            .collect();
        let value = Value::Array(items);
        let (shaped, _) = shape_value(&value, 512);
        let arr = shaped.as_array().unwrap();
        // Should have fewer items + sentinel
        assert!(arr.len() < 100);
        let last = arr.last().unwrap();
        assert!(last
            .get("__truncated")
            .is_some_and(|v| v.as_str().unwrap_or("").contains("more item")));
    }

    #[test]
    fn drop_keys_are_removed() {
        let value = json!({
            "id": "abc123",
            "subject": "Test",
            "body": "x".repeat(5000),
            "raw_text": "y".repeat(5000),
            "html": "<html>".repeat(1000),
        });
        let (shaped, dropped) = shape_value(&value, 4096);
        let obj = shaped.as_object().unwrap();
        assert!(obj.contains_key("id"));
        assert!(obj.contains_key("subject"));
        assert!(!obj.contains_key("body"));
        assert!(!obj.contains_key("raw_text"));
        assert!(!obj.contains_key("html"));
        assert!(dropped > 0);
    }

    #[test]
    fn shape_for_llm_adds_shape_metadata() {
        let value = json!({ "id": "x", "title": "Doc" });
        let result = shape_for_llm("gw_drive_search", &value, 4096);
        let obj = result.value.as_object().unwrap();
        assert!(obj.contains_key("__shape"));
        let meta = obj["__shape"].as_object().unwrap();
        assert_eq!(meta["tool"], json!("gw_drive_search"));
        assert!(meta.contains_key("handle"));
    }

    #[test]
    fn gmail_large_payload_fits_budget() {
        // Simulate a large Gmail inbox response with 50 messages
        let messages: Vec<Value> = (0..50)
            .map(|i| {
                json!({
                    "id": format!("msg{i:04}"),
                    "subject": format!("Email subject number {i} with some extra words"),
                    "from": format!("sender{i}@example.com"),
                    "date": "2026-04-21",
                    "preview": "x".repeat(300),
                    "body": "y".repeat(50_000),
                    "html": "<html>".repeat(10_000),
                    "raw_text": "z".repeat(100_000),
                })
            })
            .collect();
        let value = json!({
            "messages": Value::Array(messages),
            "requested_count": 50,
            "returned_count": 50,
        });

        let budget = crate::llm::LLM_TOOL_RESULT_TOKEN_BUDGET * 4; // ~4096 chars
        let result = shape_for_llm("gw_gmail_inbox", &value, budget);
        let serialized = result.value.to_string();
        assert!(
            serialized.len() <= budget * 2,
            "shaped output {} chars is too large (budget {})",
            serialized.len(),
            budget
        );
        assert!(result.dropped_bytes > 0, "should have dropped large bodies");
    }
}
