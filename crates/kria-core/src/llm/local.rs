use crate::infra::circuit_breaker::{CircuitBreaker, CircuitBreakerError, CircuitState};
use crate::infra::pipeline_trace::sanitize_text_for_logs;
use crate::llm::orchestrator::server_manager::LlamaServerManager;
use crate::llm::{
    extract_openai_content_text, extract_openai_message_text, extract_openai_tool_calls,
    trim_messages_for_context, ChatMessage, ContextTooLargeError, LlmBackend, LlmResponse,
    ToolSchema,
};
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Local LLM backend using llama.cpp via HTTP API.
///
/// When an orchestrator `LlamaServerManager` is attached, the API URL and
/// context window are resolved dynamically from the server manager, and
/// in-flight streams can be cancelled via `CancellationToken` during swaps.
pub struct LocalBackend {
    /// Fallback API URL (used when no server manager is attached).
    api_url: String,
    model_label: String,
    capabilities: Vec<String>,
    /// Dynamic context window (updated by orchestrator swaps).
    context_window: Arc<AtomicUsize>,
    client: reqwest::Client,
    circuit: Arc<CircuitBreaker>,
    /// Optional server manager for orchestrator-managed mode.
    /// Uses `OnceLock` so it can be attached after construction via `&self`
    /// (required because `ModelRouter` stores backends behind `Arc<dyn LlmBackend>`).
    server_manager: OnceLock<Arc<LlamaServerManager>>,
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
            context_window: Arc::new(AtomicUsize::new(context_window)),
            client,
            circuit: Arc::new(CircuitBreaker::with_defaults("local-llm")),
            server_manager: OnceLock::new(),
        }
    }

    /// Attach a server manager from the orchestrator.
    /// Enables dynamic URL resolution and stream cancellation.
    /// Safe to call on `&self` (uses `OnceLock`) — idempotent, first call wins.
    pub fn attach_server_manager(&self, mgr: Arc<LlamaServerManager>) {
        let _ = self.server_manager.set(mgr);
    }

    /// Resolve the current API URL — from server manager if attached, else fallback.
    fn resolve_api_url(&self) -> String {
        if let Some(mgr) = self.server_manager.get() {
            let url = mgr.api_url();
            if !url.is_empty() {
                return url;
            }
        }
        self.api_url.clone()
    }

    /// Update the context window (called by orchestrator after swap).
    pub fn update_context_window(&self, ctx: usize) {
        self.context_window.store(ctx, Ordering::Release);
    }

    /// Get a cancellation token if orchestrator is attached.
    fn cancel_token(&self) -> Option<CancellationToken> {
        self.server_manager.get().map(|mgr| mgr.cancel_token())
    }

    /// Check if the server is in a swapping state.
    #[allow(dead_code)]
    fn is_swapping(&self) -> bool {
        self.server_manager
            .get()
            .map(|mgr| mgr.is_swapping())
            .unwrap_or(false)
    }

    /// Wait for any in-progress swap to finish, returning `false` on timeout.
    /// Replaces the busy-poll loops used before the Notify refactor (Phase 5).
    async fn wait_for_swap(&self, timeout_secs: u64) -> bool {
        let Some(mgr) = self.server_manager.get() else {
            return true;
        };
        mgr.wait_for_swap_done(Duration::from_secs(timeout_secs))
            .await
    }

    /// Query the llama.cpp `/v1/models` endpoint to detect the actually loaded model.
    /// Returns the model ID string if the server responds, or None.
    pub async fn detect_server_model(&self) -> Option<String> {
        let url = format!("{}/models", self.resolve_api_url());
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

    fn looks_like_context_overflow_response(status: reqwest::StatusCode, body: &str) -> bool {
        if matches!(status.as_u16(), 400 | 413 | 422) {
            return true;
        }

        let lower = body.to_ascii_lowercase();
        lower.contains("context")
            && (lower.contains("too large")
                || lower.contains("too long")
                || lower.contains("overflow")
                || lower.contains("token limit")
                || lower.contains("max tokens")
                || lower.contains("exceeds"))
    }

    fn looks_like_transport_connectivity_error(message: &str) -> bool {
        let lower = message.to_ascii_lowercase();
        lower.contains("error sending request")
            || lower.contains("connection refused")
            || lower.contains("tcp connect")
            || lower.contains("dns error")
            || lower.contains("timed out")
            || lower.contains("connection reset")
            || lower.contains("broken pipe")
    }

    fn should_ignore_for_circuit(error: &anyhow::Error) -> bool {
        if error.downcast_ref::<ContextTooLargeError>().is_some() {
            return true;
        }

        Self::looks_like_transport_connectivity_error(&error.to_string())
    }

    async fn health_check_once(&self) -> bool {
        let health_url = self.resolve_api_url().replace("/v1", "/health");
        self.client
            .get(&health_url)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    async fn try_recover_open_circuit(&self, name: &str, attempt: usize) -> bool {
        tracing::warn!(
            circuit = %name,
            attempt,
            "local LLM circuit is open; probing health for fast recovery"
        );

        let healthy = tokio::time::timeout(Duration::from_secs(3), self.health_check_once())
            .await
            .unwrap_or(false);

        if healthy {
            self.circuit.reset().await;
            tracing::info!(circuit = %name, "local LLM circuit reset after successful health probe");
            return true;
        }

        false
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

        let url = format!("{}/chat/completions", self.resolve_api_url());
        let resp = self
            .client
            .post(&url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("local LLM transport error to {url}: {e}"))?;
        let status = resp.status();

        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            if Self::looks_like_context_overflow_response(status, &body_text) {
                return Err(ContextTooLargeError.into());
            }

            anyhow::bail!(
                "local LLM request failed (status {status}): {}",
                sanitize_text_for_logs(&body_text, 220)
            );
        }

        let body: serde_json::Value = resp.json().await?;

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

    /// Grammar-constrained chat call.
    ///
    /// Posts a `json_schema` field to llama.cpp `/v1/chat/completions`, which
    /// activates llguidance-backed constrained decoding inside llama.cpp.
    /// The schema **must** be a valid JSON Schema object describing the exact
    /// structure of the tool-call(s) the LLM may emit.
    ///
    /// Falls back transparently to the standard `chat` path if the server
    /// returns an error for the grammar field (older llama.cpp builds).
    pub async fn chat_with_grammar(
        &self,
        messages: &[ChatMessage],
        json_schema: serde_json::Value,
        temperature: f32,
        max_tokens: u32,
    ) -> anyhow::Result<LlmResponse> {
        // Wait for any in-progress swap before sending (Notify-based, no busy-poll).
        if !self.wait_for_swap(120).await {
            anyhow::bail!("local LLM: swap timeout exceeded (120s) waiting for grammar chat");
        }

        let wire_messages: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| {
                if m.has_images() {
                    serde_json::json!({ "role": m.role, "content": m.to_multimodal_content() })
                } else {
                    let mut msg = serde_json::json!({ "role": m.role, "content": m.content });
                    if let Some(ref name) = m.name {
                        msg["name"] = serde_json::json!(name);
                    }
                    msg
                }
            })
            .collect();

        let payload = serde_json::json!({
            "model": self.model_label,
            "messages": wire_messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
            "stream": false,
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "tool_call",
                    "strict": true,
                    "schema": json_schema,
                }
            }
        });

        let url = format!("{}/chat/completions", self.resolve_api_url());
        let resp = self
            .client
            .post(&url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("grammar chat transport error to {url}: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            // If the server doesn't support json_schema (older llama.cpp), fall back to
            // standard chat without grammar constraint but log a warning.
            if matches!(status.as_u16(), 400 | 422) && body_text.to_ascii_lowercase().contains("json_schema") {
                tracing::warn!(
                    "[LocalBackend] llama.cpp does not support json_schema response_format; \
                     falling back to unconstrained chat. Upgrade llama.cpp for llguidance support."
                );
                return self.chat(messages, None, temperature, max_tokens).await;
            }
            if Self::looks_like_context_overflow_response(status, &body_text) {
                return Err(ContextTooLargeError.into());
            }
            anyhow::bail!(
                "grammar chat request failed (status {status}): {}",
                sanitize_text_for_logs(&body_text, 220)
            );
        }

        let body: serde_json::Value = resp.json().await?;
        let choice = &body["choices"][0];
        let message = &choice["message"];
        let content = extract_openai_message_text(message);

        // With json_schema mode, the model emits structured JSON in the content field.
        // Try to extract tool_calls from the JSON content if it looks like a tool-call object.
        let tool_calls = if content.trim_start().starts_with('{') || content.trim_start().starts_with('[') {
            extract_tool_calls_from_json_content(&content).or_else(|| extract_openai_tool_calls(message))
        } else {
            extract_openai_tool_calls(message)
        };

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

    fn tokenizer_base_url(&self) -> String {
        self.resolve_api_url()
    }

    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[ToolSchema]>,
        temperature: f32,
        max_tokens: u32,
    ) -> anyhow::Result<LlmResponse> {
        // V10: Wait for swap to complete before sending a request
        if !self.wait_for_swap(120).await {
            anyhow::bail!("local LLM: swap timeout exceeded (120s)");
        }

        let mut current_messages = messages.to_vec();

        for attempt in 0..3 {
            match self
                .circuit
                .call(
                    self.chat_inner(&current_messages, tools, temperature, max_tokens),
                    |e: &anyhow::Error| Self::should_ignore_for_circuit(e),
                )
                .await
            {
                Ok(resp) => return Ok(resp),
                Err(CircuitBreakerError::Open(name)) => {
                    if attempt < 2 && self.try_recover_open_circuit(&name, attempt).await {
                        continue;
                    }

                    anyhow::bail!(
                        "local LLM unavailable (circuit open: {name}). Health probe failed; retry in 20-30s or restart the local model runtime"
                    );
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
        // V10: Wait for swap to complete
        if !self.wait_for_swap(120).await {
            anyhow::bail!("local LLM: swap timeout exceeded (120s)");
        }

        if matches!(self.circuit.state().await, CircuitState::Open)
            && !self.try_recover_open_circuit("local-llm", 0).await
            && matches!(self.circuit.state().await, CircuitState::Open)
        {
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

        let url = format!("{}/chat/completions", self.resolve_api_url());
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

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            if Self::looks_like_context_overflow_response(status, &body_text) {
                return Err(ContextTooLargeError.into());
            }

            self.circuit.on_failure().await;
            anyhow::bail!(
                "local LLM stream request failed (status {status}): {}",
                sanitize_text_for_logs(&body_text, 220)
            );
        }

        self.circuit.on_success().await;

        // V13: Build cancellable stream using select! on CancellationToken
        let cancel = self.cancel_token();

        let stream = futures::stream::unfold(
            (resp, cancel),
            |(mut resp, cancel)| async move {
                // If we have a cancel token, use select! to abort on cancellation
                let chunk_result = if let Some(ref token) = cancel {
                    tokio::select! {
                        biased;
                        _ = token.cancelled() => {
                            tracing::info!("local LLM stream: cancelled by orchestrator swap");
                            return None;
                        }
                        result = resp.chunk() => result,
                    }
                } else {
                    resp.chunk().await
                };

                match chunk_result {
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
                        Some((tokens, (resp, cancel)))
                    }
                    _ => None,
                }
            },
        );

        Ok(Box::pin(stream))
    }

    async fn health_check(&self) -> bool {
        self.health_check_once().await
    }
}

/// Parse tool calls from a JSON content string emitted under json_schema mode.
/// Handles both single `{"tool": "...", "arguments": {...}}` and
/// array `[{"tool": "...", "arguments": {...}}, ...]` forms.
/// Returns the same `Vec<serde_json::Value>` shape as `extract_openai_tool_calls`.
fn extract_tool_calls_from_json_content(content: &str) -> Option<Vec<serde_json::Value>> {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(content) else {
        return None;
    };

    let items: Vec<serde_json::Value> = if val.is_array() {
        val.as_array().cloned().unwrap_or_default()
    } else {
        vec![val]
    };

    let calls: Vec<serde_json::Value> = items
        .into_iter()
        .filter_map(|item| {
            let name = item.get("tool")
                .or_else(|| item.get("name"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())?;
            let arguments = item.get("arguments")
                .or_else(|| item.get("args"))
                .cloned()
                .unwrap_or(serde_json::Value::Object(Default::default()));
            // Emit in the OpenAI tool_calls format so the rest of the pipeline is unchanged.
            Some(serde_json::json!({
                "id": format!("grammar_{}", uuid::Uuid::new_v4()),
                "type": "function",
                "function": {
                    "name": name,
                    "arguments": serde_json::to_string(&arguments).unwrap_or_default(),
                }
            }))
        })
        .collect();

    if calls.is_empty() { None } else { Some(calls) }
}
