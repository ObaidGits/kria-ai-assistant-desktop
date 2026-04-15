use regex::Regex;
use once_cell::sync::Lazy;

/// Parsed tool call from LLM output.
#[derive(Debug, Clone)]
pub struct ParsedToolCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

// ─── Tool call regexes (4 patterns, matching Python's parser) ───

/// Pattern 1: XML-style <tool_call>{"name": ..., "arguments": ...}</tool_call>
static TOOL_CALL_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?s)<tool_call>\s*(\{.*?\})\s*</tool_call>").unwrap()
});

/// Pattern 2: Bracket style [[tool_name(arg1=val1, arg2=val2)]]
static BRACKET_CALL_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\[\[(\w+)\(([^)]*)\)\]\]").unwrap()
});

/// Pattern 3: Raw JSON {"name": "tool_name", "arguments": {...}}
static RAW_JSON_TOOL_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"\{"name"\s*:\s*"(\w+)"\s*,\s*"arguments"\s*:\s*(\{[^}]*\})\}"#).unwrap()
});

/// Pattern 4: Key-value style tool_name: key1=val1, key2=val2
static KV_TOOL_CALL_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^(\w+):\s*(.+)$").unwrap()
});

/// Parse all tool calls from LLM output text.
pub fn parse_tool_calls(text: &str) -> Vec<ParsedToolCall> {
    let mut calls = Vec::new();

    // Try Pattern 1: <tool_call>JSON</tool_call>
    for cap in TOOL_CALL_RE.captures_iter(text) {
        if let Some(json_str) = cap.get(1) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str.as_str()) {
                if let (Some(name), Some(args)) = (
                    val.get("name").and_then(|n| n.as_str()),
                    val.get("arguments"),
                ) {
                    calls.push(ParsedToolCall {
                        name: name.to_string(),
                        arguments: args.clone(),
                    });
                }
            }
        }
    }

    if !calls.is_empty() {
        return calls;
    }

    // Try Pattern 2: [[tool_name(args)]]
    for cap in BRACKET_CALL_RE.captures_iter(text) {
        let name = cap.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
        let args_str = cap.get(2).map(|m| m.as_str()).unwrap_or("");
        let arguments = parse_kv_args(args_str);
        calls.push(ParsedToolCall { name, arguments });
    }

    if !calls.is_empty() {
        return calls;
    }

    // Try Pattern 3: Raw JSON
    for cap in RAW_JSON_TOOL_RE.captures_iter(text) {
        let name = cap.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
        let args_str = cap.get(2).map(|m| m.as_str()).unwrap_or("{}");
        let arguments = serde_json::from_str(args_str).unwrap_or(serde_json::json!({}));
        calls.push(ParsedToolCall { name, arguments });
    }

    if !calls.is_empty() {
        return calls;
    }

    // Try Pattern 4: key=value style (only on lines)
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(cap) = KV_TOOL_CALL_RE.captures(trimmed) {
            let name = cap.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
            let args_str = cap.get(2).map(|m| m.as_str()).unwrap_or("");
            let arguments = parse_kv_args(args_str);
            calls.push(ParsedToolCall { name, arguments });
        }
    }

    calls
}

/// Parse key=value comma-separated arguments into JSON.
fn parse_kv_args(s: &str) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for part in s.split(',') {
        let trimmed = part.trim();
        if let Some((key, val)) = trimmed.split_once('=') {
            let key = key.trim().to_string();
            let val = val.trim();
            // Try to parse as JSON value, fallback to string
            let json_val = serde_json::from_str(val)
                .unwrap_or_else(|_| {
                    let unquoted = val.trim_matches('"').trim_matches('\'');
                    serde_json::Value::String(unquoted.to_string())
                });
            map.insert(key, json_val);
        }
    }
    serde_json::Value::Object(map)
}

/// Extract just the text response (non-tool-call parts) from LLM output.
pub fn extract_text_response(text: &str) -> String {
    let mut result = text.to_string();
    // Remove all tool call blocks
    result = TOOL_CALL_RE.replace_all(&result, "").to_string();
    result = BRACKET_CALL_RE.replace_all(&result, "").to_string();
    result = RAW_JSON_TOOL_RE.replace_all(&result, "").to_string();
    result.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_xml_style() {
        let text = r#"Let me check that.
<tool_call>
{"name": "get_cpu_usage", "arguments": {}}
</tool_call>"#;
        let calls = parse_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "get_cpu_usage");
    }

    #[test]
    fn parse_bracket_style() {
        let text = "I'll search for that: [[search_files(directory=/home, pattern=*.txt)]]";
        let calls = parse_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "search_files");
    }

    #[test]
    fn parse_raw_json_style() {
        let text = r#"{"name": "read_file", "arguments": {"path": "/tmp/test.txt"}}"#;
        let calls = parse_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
    }
}
