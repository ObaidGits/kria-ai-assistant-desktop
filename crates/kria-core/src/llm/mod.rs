pub mod cloud;
pub mod local;
pub mod model_manager;
pub mod model_router;
pub mod orchestrator;
pub mod server_binary;
pub mod tokenize;

use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};
use std::pin::Pin;

pub use model_manager::ModelManager;
pub use model_router::ModelRouter;

/// A chat message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Optional image attachments (base64-encoded) for vision models.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImageAttachment>>,
}

/// An image attachment for multimodal messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageAttachment {
    /// Base64-encoded image data.
    pub data: String,
    /// MIME type (e.g. "image/png", "image/jpeg").
    pub mime_type: String,
}

impl ChatMessage {
    /// Check if this message contains images.
    pub fn has_images(&self) -> bool {
        self.images.as_ref().is_some_and(|imgs| !imgs.is_empty())
    }

    /// Convert to OpenAI multimodal content format for vision APIs.
    pub fn to_multimodal_content(&self) -> serde_json::Value {
        if !self.has_images() {
            return serde_json::json!(self.content);
        }
        let mut parts = Vec::new();
        // Add text first
        if !self.content.is_empty() {
            parts.push(serde_json::json!({
                "type": "text",
                "text": self.content,
            }));
        }
        // Add images
        if let Some(ref images) = self.images {
            for img in images {
                parts.push(serde_json::json!({
                    "type": "image_url",
                    "image_url": {
                        "url": format!("data:{};base64,{}", img.mime_type, img.data),
                    },
                }));
            }
        }
        serde_json::json!(parts)
    }
}

/// Response from an LLM backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResponse {
    pub content: String,
    pub model: String,
    pub usage: Option<TokenUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// Tool schema for LLM function calling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Extract human-readable text from OpenAI-compatible `content` values.
/// Handles string, object, and array-part formats returned by different providers.
pub fn extract_openai_content_text(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(parts) => {
            let mut chunks: Vec<String> = Vec::new();
            for part in parts {
                let piece = extract_openai_content_text(part);
                if !piece.trim().is_empty() {
                    chunks.push(piece);
                }
            }
            chunks.join("\n")
        }
        serde_json::Value::Object(map) => {
            if let Some(v) = map.get("text") {
                return extract_openai_content_text(v);
            }
            if let Some(v) = map.get("content") {
                return extract_openai_content_text(v);
            }
            if let Some(v) = map.get("value") {
                return extract_openai_content_text(v);
            }
            if let Some(v) = map.get("output_text") {
                return extract_openai_content_text(v);
            }
            if let Some(v) = map.get("input_text") {
                return extract_openai_content_text(v);
            }
            String::new()
        }
        _ => String::new(),
    }
}

/// Extract text from `choice.message` object across provider variants.
pub fn extract_openai_message_text(message: &serde_json::Value) -> String {
    extract_openai_content_text(&message["content"])
}

/// Extract tool calls from `choice.message` across provider variants.
/// Supports modern `tool_calls` and legacy `function_call` fields.
pub fn extract_openai_tool_calls(message: &serde_json::Value) -> Option<Vec<serde_json::Value>> {
    if let Some(arr) = message.get("tool_calls").and_then(|v| v.as_array()) {
        if !arr.is_empty() {
            return Some(arr.clone());
        }
    }

    if let Some(fc) = message.get("function_call") {
        let name = fc.get("name").and_then(|v| v.as_str())?;
        let args = fc
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| serde_json::json!("{}"));
        return Some(vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": name,
                "arguments": args,
            }
        })]);
    }

    None
}

/// Trait for all LLM backends (local and cloud).
#[async_trait]
pub trait LlmBackend: Send + Sync {
    fn model_label(&self) -> &str;
    fn capabilities(&self) -> &[String];
    fn is_configured(&self) -> bool;

    /// Returns the base HTTP URL of the backend's inference server, if any.
    /// Used by the tokenizer helper (`llm::tokenize::count_tokens`) to obtain
    /// exact token counts without adding a new crate dependency.
    /// Backends that do not expose a local HTTP server should return `""`.
    fn tokenizer_base_url(&self) -> String {
        String::new()
    }

    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[ToolSchema]>,
        temperature: f32,
        max_tokens: u32,
    ) -> anyhow::Result<LlmResponse>;

    async fn chat_stream(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[ToolSchema]>,
        temperature: f32,
        max_tokens: u32,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = String> + Send>>>;

    async fn health_check(&self) -> bool;
}

/// Context overflow error — exempted from circuit breaker failure counts.
#[derive(Debug, thiserror::Error)]
#[error("context too large for model")]
pub struct ContextTooLargeError;

/// Max chars for tool results in context.
pub const TOOL_RESULT_MAX_CHARS: usize = 3000;

/// Per-tool token budget for shaped LLM injection (≈ 1 024 tokens).
pub const LLM_TOOL_RESULT_TOKEN_BUDGET: usize = 1024;

/// Per-turn aggregate token budget for all tool outputs combined (≈ 4 096 tokens).
/// When the turn total exceeds this, subsequent tools are short-circuited.
pub const LLM_TURN_TOOL_BUDGET: usize = 4096;

/// Trim messages to fit context window.
///
/// Attempt 0: compress tool-result and very large messages.
/// Attempt 1: keep only the latest 8 non-system messages and shorten the system prompt.
/// Attempt 2+: keep only the latest 3 non-system messages and a minimal system prompt.
pub fn trim_messages_for_context(messages: &[ChatMessage], attempt: usize) -> Vec<ChatMessage> {
    if messages.is_empty() {
        return Vec::new();
    }

    match attempt {
        0 => {
            // Stage 1: compress large tool results and oversized non-system messages while
            // preserving the system prompt and full turn history shape.
            messages
                .iter()
                .map(|m| {
                    let mut msg = m.clone();
                    if msg.role == "system" {
                        // Never truncate the system prompt in stage 0 — it contains
                        // the tool-calling schema and critical rules.
                    } else if msg.role == "tool" {
                        msg.content =
                            truncate_with_suffix(&msg.content, 500, "...<tool-truncated>");
                    } else {
                        msg.content = truncate_with_suffix(&msg.content, 1800, "...<truncated>");
                    }
                    msg
                })
                .collect()
        }
        1 => {
            // Stage 2: keep the latest conversation turns and compact the system prompt.
            let mut systems: Vec<ChatMessage> = messages
                .iter()
                .filter(|m| m.role == "system")
                .cloned()
                .collect();
            if let Some(first) = systems.first_mut() {
                first.content = minimal_system_prompt();
            }

            let mut non_system: Vec<ChatMessage> = messages
                .iter()
                .filter(|m| m.role != "system")
                .cloned()
                .collect();
            if non_system.len() > 8 {
                non_system = non_system.split_off(non_system.len() - 8);
            }
            for msg in &mut non_system {
                let max_chars = if msg.role == "tool" { 350 } else { 900 };
                let suffix = if msg.role == "tool" {
                    "...<tool-truncated>"
                } else {
                    "...<truncated>"
                };
                msg.content = truncate_with_suffix(&msg.content, max_chars, suffix);
            }

            systems.into_iter().chain(non_system).collect()
        }
        _ => {
            // Stage 3: emergency context fit — keep minimal instruction and only
            // the newest few turns.
            let mut out = Vec::new();
            out.push(ChatMessage {
                role: "system".into(),
                content: minimal_system_prompt(),
                name: None,
                images: None,
            });

            let mut non_system: Vec<ChatMessage> = messages
                .iter()
                .filter(|m| m.role != "system")
                .cloned()
                .collect();
            if non_system.len() > 3 {
                non_system = non_system.split_off(non_system.len() - 3);
            }
            for msg in &mut non_system {
                let max_chars = if msg.role == "tool" { 240 } else { 700 };
                let suffix = if msg.role == "tool" {
                    "...<tool-truncated>"
                } else {
                    "...<truncated>"
                };
                msg.content = truncate_with_suffix(&msg.content, max_chars, suffix);
            }
            out.extend(non_system);
            out
        }
    }
}

fn truncate_with_suffix(text: &str, max_chars: usize, suffix: &str) -> String {
    let len = text.chars().count();
    if len <= max_chars {
        return text.to_string();
    }

    let suffix_chars = suffix.chars().count();
    let keep = max_chars.saturating_sub(suffix_chars).max(1);
    let mut s: String = text.chars().take(keep).collect();
    s.push_str(suffix);
    s
}

fn minimal_system_prompt() -> String {
    "You are KRIA, an AI assistant. Be concise, accurate, and safe. \
 CRITICAL: When the user asks you to perform an action (generate an image, search the web, \
 send email, run code, etc.), you MUST call the appropriate tool — never refuse or say you cannot. \
 Always respond with a tool call JSON when a tool is available for the request. \
 Use available tools for live/current information instead of claiming no real-time access. \
 Avoid repeating unchanged context."
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        extract_openai_content_text, extract_openai_tool_calls, trim_messages_for_context,
    };
    use crate::llm::ChatMessage;

    #[test]
    fn extract_content_text_handles_string_and_parts() {
        let plain = serde_json::json!("hello world");
        assert_eq!(extract_openai_content_text(&plain), "hello world");

        let parts = serde_json::json!([
            {"type": "text", "text": "first"},
            {"type": "text", "text": "second"}
        ]);
        assert_eq!(extract_openai_content_text(&parts), "first\nsecond");
    }

    #[test]
    fn extract_tool_calls_supports_legacy_function_call() {
        let msg = serde_json::json!({
            "function_call": {
                "name": "analyze_image",
                "arguments": "{\"path\":\"/tmp/a.png\"}"
            }
        });

        let calls = extract_openai_tool_calls(&msg).expect("tool calls expected");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["function"]["name"], "analyze_image");
    }

    #[test]
    fn trim_attempt_two_keeps_minimal_context() {
        let mut msgs = Vec::new();
        msgs.push(ChatMessage {
            role: "system".into(),
            content: "very long system prompt".repeat(40),
            name: None,
            images: None,
        });
        for i in 0..8 {
            msgs.push(ChatMessage {
                role: if i % 2 == 0 {
                    "user".into()
                } else {
                    "assistant".into()
                },
                content: format!("message {i} {}", "x".repeat(1200)),
                name: None,
                images: None,
            });
        }

        let trimmed = trim_messages_for_context(&msgs, 2);
        assert_eq!(trimmed[0].role, "system");
        assert!(trimmed.len() <= 4, "should keep system + latest few turns");
        assert!(trimmed[0].content.contains("You are KRIA"));
    }
}
