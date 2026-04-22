use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;

const DEBUG_MAX_TEXT_CHARS: usize = 320;
const DEBUG_MAX_ARRAY_ITEMS: usize = 8;
const ESSENTIAL_MAX_TEXT_CHARS: usize = 140;
const ESSENTIAL_MAX_ARRAY_ITEMS: usize = 3;
const DEFAULT_MAX_OBJECT_FIELDS: usize = 24;

static PIPELINE_DEBUG_ENABLED: Lazy<bool> =
    Lazy::new(|| std::env::var("KRIA_PIPELINE_DEBUG").is_ok_and(|v| parse_bool_flag(&v)));

static KEY_VALUE_SECRET_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)(api[_-]?key|access[_-]?token|refresh[_-]?token|authorization|password|secret|cookie)\s*[:=]\s*([^\s,;]+)",
    )
    .expect("valid key/value secret regex")
});

static BEARER_TOKEN_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)bearer\s+[A-Za-z0-9._\-]+=*").expect("valid bearer token regex")
});

pub fn pipeline_debug_enabled() -> bool {
    *PIPELINE_DEBUG_ENABLED
}

pub fn parse_bool_flag(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on" | "debug"
    )
}

pub fn sanitize_text_for_logs(text: &str, max_chars: usize) -> String {
    let redacted = redact_text_secrets(text);
    let single_line = normalize_whitespace(&redacted);
    truncate_chars(&single_line, max_chars)
}

pub fn sanitize_json_for_logs(
    value: &Value,
    max_text_chars: usize,
    max_array_items: usize,
) -> Value {
    sanitize_json_inner(value, max_text_chars, max_array_items, DEFAULT_MAX_OBJECT_FIELDS)
}

pub fn log_pipeline_step(session_id: &str, step: &str, message: &str, detail: Option<Value>) {
    let debug_enabled = pipeline_debug_enabled();
    let (max_text_chars, max_array_items) = if debug_enabled {
        (DEBUG_MAX_TEXT_CHARS, DEBUG_MAX_ARRAY_ITEMS)
    } else {
        (ESSENTIAL_MAX_TEXT_CHARS, ESSENTIAL_MAX_ARRAY_ITEMS)
    };

    let sanitized_detail =
        detail.map(|d| sanitize_json_for_logs(&d, max_text_chars, max_array_items));

    if debug_enabled {
        if let Some(detail) = sanitized_detail {
            tracing::debug!(
                target: "kria_pipeline",
                session_id = session_id,
                step = step,
                message = message,
                detail = %detail,
                "pipeline step"
            );
        } else {
            tracing::debug!(
                target: "kria_pipeline",
                session_id = session_id,
                step = step,
                message = message,
                "pipeline step"
            );
        }
        return;
    }

    if is_essential_pipeline_step(step) {
        if let Some(detail) = sanitized_detail {
            tracing::info!(
                target: "kria_pipeline",
                session_id = session_id,
                step = step,
                message = message,
                detail = %detail,
                "pipeline step"
            );
        } else {
            tracing::info!(
                target: "kria_pipeline",
                session_id = session_id,
                step = step,
                message = message,
                "pipeline step"
            );
        }
    }
}

pub fn truncate_chars(input: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }

    let total = input.chars().count();
    if total <= max_chars {
        return input.to_string();
    }

    let mut out: String = input.chars().take(max_chars).collect();
    let remaining = total.saturating_sub(max_chars);
    out.push_str(&format!("...<truncated {remaining} chars>"));
    out
}

fn sanitize_json_inner(
    value: &Value,
    max_text_chars: usize,
    max_array_items: usize,
    max_object_fields: usize,
) -> Value {
    match value {
        Value::Object(map) => {
            let mut sanitized = serde_json::Map::new();
            for (idx, (key, val)) in map.iter().enumerate() {
                if idx >= max_object_fields {
                    sanitized.insert(
                        "_truncated_fields".into(),
                        Value::String(format!("{} more field(s)", map.len() - max_object_fields)),
                    );
                    break;
                }

                if looks_sensitive_key(key) {
                    sanitized.insert(key.clone(), Value::String("[REDACTED]".into()));
                    continue;
                }

                sanitized.insert(
                    key.clone(),
                    sanitize_json_inner(val, max_text_chars, max_array_items, max_object_fields),
                );
            }
            Value::Object(sanitized)
        }
        Value::Array(values) => {
            let mut sanitized: Vec<Value> = values
                .iter()
                .take(max_array_items)
                .map(|v| sanitize_json_inner(v, max_text_chars, max_array_items, max_object_fields))
                .collect();

            if values.len() > max_array_items {
                sanitized.push(Value::String(format!(
                    "...<{} more item(s)>",
                    values.len() - max_array_items
                )));
            }

            Value::Array(sanitized)
        }
        Value::String(text) => Value::String(sanitize_text_for_logs(text, max_text_chars)),
        _ => value.clone(),
    }
}

fn redact_text_secrets(text: &str) -> String {
    let bearer_redacted = BEARER_TOKEN_RE
        .replace_all(text, "Bearer [REDACTED]")
        .to_string();

    KEY_VALUE_SECRET_RE
        .replace_all(&bearer_redacted, |caps: &regex::Captures| {
            let key = caps.get(1).map(|m| m.as_str()).unwrap_or("secret");
            format!("{key}=[REDACTED]")
        })
        .to_string()
}

fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn looks_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    [
        "api_key",
        "apikey",
        "token",
        "authorization",
        "password",
        "secret",
        "cookie",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn is_essential_pipeline_step(step: &str) -> bool {
    matches!(
        step,
        "prompt_entered"
            | "backend_selected"
            | "backend_unavailable"
            | "preprocessing_applied"
            | "preprocessing_skipped"
            | "tool_schemas_built"
            | "intent_classified"
            | "llm_input_prepared"
            | "llm_error"
            | "llm_response_received"
            | "tool_calls_parsed"
            | "synthetic_package_calls"
            | "synthetic_intent_call"
            | "tool_choice_required"
            | "no_tool_calls"
            | "assistant_tool_history_added"
            | "tool_call_started"
            | "tool_call_rejected"
            | "policy_evaluated"
            | "tool_call_blocked"
            | "approval_reused"
            | "approval_requested"
            | "approval_result"
            | "tool_call_denied"
            | "tool_result_ready"
            | "round_completed"
            | "final_formatting_started"
            | "final_formatting_adjusted"
            | "final_output_ready"
            | "final_output_fallback"
            | "max_rounds_reached"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bool_flag_accepts_truthy_values() {
        assert!(parse_bool_flag("1"));
        assert!(parse_bool_flag("true"));
        assert!(parse_bool_flag("YES"));
        assert!(parse_bool_flag("on"));
        assert!(parse_bool_flag("debug"));
    }

    #[test]
    fn parse_bool_flag_rejects_falsy_values() {
        assert!(!parse_bool_flag("0"));
        assert!(!parse_bool_flag("false"));
        assert!(!parse_bool_flag(""));
    }

    #[test]
    fn sanitize_text_for_logs_redacts_secrets() {
        let input = "Authorization: Bearer abc.def and api_key=supersecret";
        let out = sanitize_text_for_logs(input, 200);
        assert!(!out.contains("abc.def"));
        assert!(!out.contains("supersecret"));
        assert!(out.contains("[REDACTED]"));
    }

    #[test]
    fn sanitize_json_for_logs_redacts_sensitive_keys() {
        let input = serde_json::json!({
            "api_key": "abcd",
            "nested": {
                "access_token": "xyz",
                "safe": "ok"
            }
        });

        let out = sanitize_json_for_logs(&input, 200, 8);
        assert_eq!(out["api_key"], "[REDACTED]");
        assert_eq!(out["nested"]["access_token"], "[REDACTED]");
        assert_eq!(out["nested"]["safe"], "ok");
    }

    #[test]
    fn truncate_chars_adds_truncation_marker() {
        let out = truncate_chars("abcdefghijklmnopqrstuvwxyz", 5);
        assert!(out.starts_with("abcde"));
        assert!(out.contains("truncated"));
    }

    #[test]
    fn essential_step_detector_includes_prompt_and_llm() {
        assert!(is_essential_pipeline_step("prompt_entered"));
        assert!(is_essential_pipeline_step("llm_response_received"));
        assert!(!is_essential_pipeline_step("some_internal_non_pipeline_step"));
    }
}