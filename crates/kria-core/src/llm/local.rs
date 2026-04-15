use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;
use crate::llm::{ChatMessage, LlmResponse, LlmBackend, ToolSchema, ContextTooLargeError, trim_messages_for_context};
use crate::infra::CircuitBreaker;
use std::sync::Arc;
use std::time::Duration;

/// Local LLM backend using llama.cpp via HTTP API.
///
/// In production, replace HTTP calls with llama-cpp-rs embedded bindings.
/// Keeping HTTP API for now allows using existing llama-server binary.
pub struct LocalBackend {
    api_url: String,
    model_label: String,
    capabilities: Vec<String>,
    #[allow(dead_code)]
    context_window: usize,
    client: reqwest::Client,
    circuit: Arc<CircuitBreaker>,
}

impl LocalBackend {
    pub fn new(
        api_url: String,
        model_label: String,
        capabilities: Vec<String>,
        context_window: usize,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .unwrap_or_default();

        Self {
            api_url,
            model_label,
            capabilities,
            context_window,
            client,
            circuit: Arc::new(CircuitBreaker::with_defaults("local-llm")),
        }
    }

    async fn chat_inner(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[ToolSchema]>,
        temperature: f32,
        max_tokens: u32,
    ) -> anyhow::Result<LlmResponse> {
        let mut payload = serde_json::json!({
            "model": self.model_label,
            "messages": messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
            "stream": false,
        });

        if let Some(t) = tools {
            if !t.is_empty() {
                let tool_defs: Vec<serde_json::Value> = t.iter().map(|ts| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": ts.name,
                            "description": ts.description,
                            "parameters": ts.parameters,
                        }
                    })
                }).collect();
                payload["tools"] = serde_json::Value::Array(tool_defs);
            }
        }

        let url = format!("{}/chat/completions", self.api_url);
        let resp = self.client.post(&url).json(&payload).send().await?;
        let status = resp.status();

        if status.as_u16() == 400 {
            return Err(ContextTooLargeError.into());
        }

        let body: serde_json::Value = resp.error_for_status()?.json().await?;

        let choice = &body["choices"][0];
        let content = choice["message"]["content"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let tool_calls = choice["message"]["tool_calls"].as_array().cloned();

        let usage = body["usage"].as_object().map(|u| crate::llm::TokenUsage {
            prompt_tokens: u["prompt_tokens"].as_u64().unwrap_or(0) as u32,
            completion_tokens: u["completion_tokens"].as_u64().unwrap_or(0) as u32,
            total_tokens: u["total_tokens"].as_u64().unwrap_or(0) as u32,
        });

        Ok(LlmResponse {
            content,
            model: self.model_label.clone(),
            usage,
            tool_calls,
        })
    }
}

#[async_trait]
impl LlmBackend for LocalBackend {
    fn model_label(&self) -> &str {
        &self.model_label
    }

    fn capabilities(&self) -> &[String] {
        &self.capabilities
    }

    fn is_configured(&self) -> bool {
        true
    }

    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[ToolSchema]>,
        temperature: f32,
        max_tokens: u32,
    ) -> anyhow::Result<LlmResponse> {
        let mut current_messages = messages.to_vec();

        for attempt in 0..3 {
            match self.chat_inner(&current_messages, tools, temperature, max_tokens).await {
                Ok(resp) => {
                    self.circuit.on_success().await;
                    return Ok(resp);
                }
                Err(e) => {
                    if e.downcast_ref::<ContextTooLargeError>().is_some() {
                        tracing::warn!(attempt, "context too large, trimming");
                        current_messages = trim_messages_for_context(&current_messages, attempt);
                        continue;
                    }
                    self.circuit.on_failure().await;
                    if attempt == 2 {
                        return Err(e);
                    }
                }
            }
        }

        anyhow::bail!("local LLM failed after 3 attempts")
    }

    async fn chat_stream(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[ToolSchema]>,
        temperature: f32,
        max_tokens: u32,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = String> + Send>>> {
        let mut payload = serde_json::json!({
            "model": self.model_label,
            "messages": messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
            "stream": true,
        });

        if let Some(t) = tools {
            if !t.is_empty() {
                let tool_defs: Vec<serde_json::Value> = t.iter().map(|ts| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": ts.name,
                            "description": ts.description,
                            "parameters": ts.parameters,
                        }
                    })
                }).collect();
                payload["tools"] = serde_json::Value::Array(tool_defs);
            }
        }

        let url = format!("{}/chat/completions", self.api_url);
        let resp = self.client.post(&url).json(&payload).send().await?.error_for_status()?;

        let stream = futures::stream::unfold(resp, |mut resp| async move {
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    let text = String::from_utf8_lossy(&chunk).to_string();
                    // Parse SSE: lines starting with "data: "
                    let mut tokens = String::new();
                    for line in text.lines() {
                        if let Some(data) = line.strip_prefix("data: ") {
                            if data == "[DONE]" {
                                continue;
                            }
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                                if let Some(tok) = v["choices"][0]["delta"]["content"].as_str() {
                                    tokens.push_str(tok);
                                }
                            }
                        }
                    }
                    Some((tokens, resp))
                }
                _ => None,
            }
        });

        Ok(Box::pin(stream))
    }

    async fn health_check(&self) -> bool {
        let health_url = self.api_url.replace("/v1", "/health");
        self.client.get(&health_url).send().await.map(|r| r.status().is_success()).unwrap_or(false)
    }
}
