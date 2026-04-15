pub mod local;
pub mod cloud;
pub mod model_router;
pub mod model_manager;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use futures::Stream;
use std::pin::Pin;

pub use model_router::ModelRouter;
pub use model_manager::ModelManager;

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
        self.images.as_ref().map_or(false, |imgs| !imgs.is_empty())
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

/// Trait for all LLM backends (local and cloud).
#[async_trait]
pub trait LlmBackend: Send + Sync {
    fn model_label(&self) -> &str;
    fn capabilities(&self) -> &[String];
    fn is_configured(&self) -> bool;

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

/// Trim messages to fit context window.
///
/// Attempt 0: compress tool-result messages to 500 chars.
/// Attempt 1+: drop 2 oldest non-system messages.
pub fn trim_messages_for_context(messages: &[ChatMessage], attempt: usize) -> Vec<ChatMessage> {
    let mut result = messages.to_vec();

    if attempt == 0 {
        // Stage 1: compress large tool results
        for msg in &mut result {
            if msg.role == "tool" && msg.content.len() > 500 {
                msg.content = format!("{}...<truncated>", &msg.content[..500]);
            }
        }
    } else {
        // Stage 2: drop oldest non-system messages
        let system_count = result.iter().filter(|m| m.role == "system").count();
        let drop_count = 2.min(result.len().saturating_sub(system_count + 2));
        let mut dropped = 0;
        result.retain(|m| {
            if m.role == "system" || dropped >= drop_count {
                true
            } else {
                dropped += 1;
                false
            }
        });
    }

    result
}
