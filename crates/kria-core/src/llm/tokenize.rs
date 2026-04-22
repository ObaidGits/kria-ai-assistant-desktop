//! Token counting via the llama.cpp `/tokenize` HTTP endpoint.
//!
//! Provides an async `count_tokens` function that posts text to the running
//! llama.cpp server and returns the exact token count.  Falls back to a
//! character-based heuristic (`len / 4`) when the endpoint is unavailable so
//! callers never need to handle an error path.

/// Approximate tokens per character for the heuristic fallback.
const CHARS_PER_TOKEN_FALLBACK: usize = 4;

/// Character budget corresponding to `LLM_TOOL_RESULT_TOKEN_BUDGET` tokens.
/// Used in synchronous shaping code that cannot await.
pub const TOOL_RESULT_CHAR_BUDGET: usize =
    crate::llm::LLM_TOOL_RESULT_TOKEN_BUDGET * CHARS_PER_TOKEN_FALLBACK;

/// Character budget corresponding to `LLM_TURN_TOOL_BUDGET` tokens.
pub const TURN_TOOL_CHAR_BUDGET: usize =
    crate::llm::LLM_TURN_TOOL_BUDGET * CHARS_PER_TOKEN_FALLBACK;

/// Count the tokens in `text` using the llama.cpp `/tokenize` endpoint at
/// `base_url`.
///
/// If `base_url` is empty, or if the HTTP request fails, or if the response
/// cannot be parsed, falls back to `text.len() / 4` and logs a warning.
pub async fn count_tokens(text: &str, base_url: &str) -> usize {
    if base_url.is_empty() {
        return text.len() / CHARS_PER_TOKEN_FALLBACK;
    }

    let url = format!("{}/tokenize", base_url.trim_end_matches('/'));
    let body = serde_json::json!({ "content": text });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    match client.post(&url).json(&body).send().await {
        Ok(resp) if resp.status().is_success() => {
            match resp.json::<serde_json::Value>().await {
                Ok(v) => {
                    if let Some(tokens) = v.get("tokens").and_then(|t| t.as_array()) {
                        return tokens.len();
                    }
                    tracing::warn!(
                        target: "kria::tokenize",
                        "llama.cpp /tokenize response missing 'tokens' field; using len/4 fallback"
                    );
                    text.len() / CHARS_PER_TOKEN_FALLBACK
                }
                Err(e) => {
                    tracing::warn!(
                        target: "kria::tokenize",
                        "failed to parse /tokenize response: {e}; using len/4 fallback"
                    );
                    text.len() / CHARS_PER_TOKEN_FALLBACK
                }
            }
        }
        Ok(resp) => {
            tracing::warn!(
                target: "kria::tokenize",
                "/tokenize returned HTTP {}; using len/4 fallback",
                resp.status()
            );
            text.len() / CHARS_PER_TOKEN_FALLBACK
        }
        Err(e) => {
            tracing::warn!(
                target: "kria::tokenize",
                "/tokenize request failed: {e}; using len/4 fallback"
            );
            text.len() / CHARS_PER_TOKEN_FALLBACK
        }
    }
}
