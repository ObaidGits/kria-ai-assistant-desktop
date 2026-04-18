use crate::llm::{
    extract_openai_content_text, extract_openai_message_text, extract_openai_tool_calls,
    ChatMessage, LlmBackend, LlmResponse, TokenUsage, ToolSchema,
};
use async_trait::async_trait;
use futures::Stream;
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Cloud LLM backend via OpenAI-compatible API (Gemini, GPT, Claude, Groq, OpenRouter).
pub struct CloudBackend {
    endpoint: String,
    api_key: String,
    model_id: String,
    display_name: String,
    capabilities: Vec<String>,
    client: reqwest::Client,
    rate_limiter: Option<RateLimiter>,
}

struct RateLimiter {
    rpm: u32,
    timestamps: Mutex<VecDeque<Instant>>,
}

impl RateLimiter {
    fn new(rpm: u32) -> Self {
        Self {
            rpm,
            timestamps: Mutex::new(VecDeque::new()),
        }
    }

    async fn acquire(&self) {
        loop {
            let should_wait = {
                let mut ts = self.timestamps.lock().unwrap();
                let now = Instant::now();
                let window = Duration::from_secs(60);
                ts.retain(|t| now.duration_since(*t) < window);
                if (ts.len() as u32) < self.rpm {
                    ts.push_back(now);
                    false
                } else {
                    true
                }
            };
            if should_wait {
                tokio::time::sleep(Duration::from_millis(500)).await;
            } else {
                break;
            }
        }
    }
}

impl CloudBackend {
    pub fn new(
        endpoint: String,
        api_key: String,
        model_id: String,
        display_name: String,
        capabilities: Vec<String>,
        rate_limit_rpm: Option<u32>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_default();

        Self {
            endpoint,
            api_key,
            model_id,
            display_name,
            capabilities,
            client,
            rate_limiter: rate_limit_rpm.map(RateLimiter::new),
        }
    }
}

#[async_trait]
impl LlmBackend for CloudBackend {
    fn model_label(&self) -> &str {
        &self.display_name
    }

    fn capabilities(&self) -> &[String] {
        &self.capabilities
    }

    fn is_configured(&self) -> bool {
        !self.api_key.is_empty() && !self.endpoint.is_empty()
    }

    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[ToolSchema]>,
        temperature: f32,
        max_tokens: u32,
    ) -> anyhow::Result<LlmResponse> {
        if let Some(ref rl) = self.rate_limiter {
            rl.acquire().await;
        }

        let mut payload = serde_json::json!({
            "model": self.model_id,
            "messages": messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
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

        let url = format!("{}/chat/completions", self.endpoint);

        for attempt in 0..3 {
            let resp = self
                .client
                .post(&url)
                .bearer_auth(&self.api_key)
                .json(&payload)
                .send()
                .await?;

            let status = resp.status();
            if status.as_u16() == 429 {
                let wait = 2u64.pow(attempt as u32);
                tracing::warn!(attempt, wait_secs = wait, "rate limited, retrying");
                tokio::time::sleep(Duration::from_secs(wait)).await;
                continue;
            }

            let body: serde_json::Value = resp.error_for_status()?.json().await?;

            let choice = &body["choices"][0];
            let message = &choice["message"];
            let content = extract_openai_message_text(message);
            let tool_calls = extract_openai_tool_calls(message);

            let usage = body["usage"].as_object().map(|u| TokenUsage {
                prompt_tokens: u["prompt_tokens"].as_u64().unwrap_or(0) as u32,
                completion_tokens: u["completion_tokens"].as_u64().unwrap_or(0) as u32,
                total_tokens: u["total_tokens"].as_u64().unwrap_or(0) as u32,
            });

            return Ok(LlmResponse {
                content,
                model: self.model_id.clone(),
                usage,
                tool_calls,
            });
        }

        anyhow::bail!("cloud LLM failed after 3 retries (rate limited)")
    }

    async fn chat_stream(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[ToolSchema]>,
        temperature: f32,
        max_tokens: u32,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = String> + Send>>> {
        if let Some(ref rl) = self.rate_limiter {
            rl.acquire().await;
        }

        let mut payload = serde_json::json!({
            "model": self.model_id,
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

        let url = format!("{}/chat/completions", self.endpoint);
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .await?
            .error_for_status()?;

        let stream = futures::stream::unfold(resp, |mut resp| async move {
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    let text = String::from_utf8_lossy(&chunk).to_string();
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
        self.is_configured()
    }
}
