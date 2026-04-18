use crate::infra::circuit_breaker::{CircuitBreaker, CircuitBreakerError, CircuitState};
use crate::llm::{
    extract_openai_content_text, extract_openai_message_text, extract_openai_tool_calls,
    trim_messages_for_context, ChatMessage, ContextTooLargeError, LlmBackend, LlmResponse,
    ToolSchema,
};
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;
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

    /// Query the llama.cpp `/v1/models` endpoint to detect the actually loaded model.
    /// Returns the model ID string if the server responds, or None.
    pub async fn detect_server_model(&self) -> Option<String> {
        let url = format!("{}/models", self.api_url);
        let resp = self.client.get(&url).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let body: serde_json::Value = resp.json().await.ok()?;
        // llama.cpp returns { "data": [{ "id": "model-name", ... }] }
        body["data"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|m| m["id"].as_str())
            .map(|s| s.to_string())
    }

    /// Update the model label dynamically (e.g. after detecting from server).
    pub fn set_model_label(&mut self, label: String) {
        self.model_label = label;
    }

    async fn chat_inner(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[ToolSchema]>,
        temperature: f32,
        max_tokens: u32,
    ) -> anyhow::Result<LlmResponse> {
        // Convert messages to the OpenAI wire format, using multimodal content
        // for any messages that contain images (required for vision models).
        let wire_messages: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| {
                if m.has_images() {
                    serde_json::json!({
                        "role": m.role,
                        "content": m.to_multimodal_content(),
                    })
                } else {
                    let mut msg = serde_json::json!({
                        "role": m.role,
                        "content": m.content,
                    });
                    if let Some(ref name) = m.name {
                        msg["name"] = serde_json::json!(name);
                    }
                    msg
                }
            })
            .collect();

        let mut payload = serde_json::json!({
            "model": self.model_label,
            "messages": wire_messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
            "stream": false,
        });

        if let Some(t) = tools {
            if !t.is_empty() {
                let tool_defs: Vec<serde_json::Value> = t
                    .iter()
                    .map(|ts| {
                        serde_json::json!({
                            "type": "function",
                            "function": {
                                "name": ts.name,
                                "description": ts.description,
                                "parameters": ts.parameters,
                            }
                        })
                    })
                    .collect();
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
        let message = &choice["message"];
        let content = extract_openai_message_text(message);
        let tool_calls = extract_openai_tool_calls(message);

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
            match self
                .circuit
                .call(
                    self.chat_inner(&current_messages, tools, temperature, max_tokens),
                    |e: &anyhow::Error| e.downcast_ref::<ContextTooLargeError>().is_some(),
                )
                .await
            {
                Ok(resp) => return Ok(resp),
                Err(CircuitBreakerError::Open(name)) => {
                    anyhow::bail!("local LLM unavailable (circuit open: {name})");
                }
                Err(CircuitBreakerError::Inner(e)) => {
                    if e.downcast_ref::<ContextTooLargeError>().is_some() {
                        tracing::warn!(
                            attempt,
                            message_count = current_messages.len(),
                            total_chars = current_messages
                                .iter()
                                .map(|m| m.content.chars().count())
                                .sum::<usize>(),
                            "context too large, trimming"
                        );
                        current_messages = trim_messages_for_context(&current_messages, attempt);
                        continue;
                    }
                    if attempt == 2 {
                        return Err(e);
                    }
                }
            }
        }

        anyhow::bail!(
            "local LLM context overflow after 3 attempts; start a new session or increase model context"
        )
    }

    async fn chat_stream(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[ToolSchema]>,
        temperature: f32,
        max_tokens: u32,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = String> + Send>>> {
        if matches!(self.circuit.state().await, CircuitState::Open) {
            anyhow::bail!("local LLM stream unavailable (circuit open)");
        }

        let mut payload = serde_json::json!({
            "model": self.model_label,
            "messages": messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
            "stream": true,
        });

        if let Some(t) = tools {
            if !t.is_empty() {
                let tool_defs: Vec<serde_json::Value> = t
                    .iter()
                    .map(|ts| {
                        serde_json::json!({
                            "type": "function",
                            "function": {
                                "name": ts.name,
                                "description": ts.description,
                                "parameters": ts.parameters,
                            }
                        })
                    })
                    .collect();
                payload["tools"] = serde_json::Value::Array(tool_defs);
            }
        }

        let url = format!("{}/chat/completions", self.api_url);
        let resp = match tokio::time::timeout(
            Duration::from_secs(45),
            self.client.post(&url).json(&payload).send(),
        )
        .await
        {
            Ok(Ok(resp)) => resp,
            Ok(Err(e)) => {
                self.circuit.on_failure().await;
                return Err(e.into());
            }
            Err(_) => {
                self.circuit.on_failure().await;
                anyhow::bail!("local LLM stream request timed out");
            }
        };

        if resp.status().as_u16() == 400 {
            return Err(ContextTooLargeError.into());
        }

        let resp = match resp.error_for_status() {
            Ok(resp) => resp,
            Err(e) => {
                self.circuit.on_failure().await;
                return Err(e.into());
            }
        };

        self.circuit.on_success().await;

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
                                let delta_content = &v["choices"][0]["delta"]["content"];
                                let tok = extract_openai_content_text(delta_content);
                                if !tok.is_empty() {
                                    tokens.push_str(&tok);
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
        self.client
            .get(&health_url)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }
}
