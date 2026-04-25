//! Tier C cloud fallback — Pollinations.ai and HuggingFace Inference API.
//!
//! Provider chain: Pollinations → HF Inference (if token present).
//! Each provider has its own circuit breaker (3 consecutive fails → 60s cooldown).
//! Missing HF token → provider silently skipped (not counted as a failure).

use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

/// Result from a cloud generation request.
#[derive(Debug, Clone)]
pub struct CloudImageResult {
    pub png_bytes: Vec<u8>,
    pub provenance: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum CloudError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Cloud provider returned empty body")]
    EmptyBody,
    #[error("Cloud fallback disabled by user policy")]
    Disabled,
    #[error("All cloud providers failed")]
    AllFailed,
    #[error("All providers failed — {0} attempt(s) made")]
    AllProvidersFailed(String),
    #[error("Rate limited (429) — queue full")]
    RateLimited,
}

/// Thin client for Pollinations.ai (no API key required).
pub struct PollinationsClient {
    client: reqwest::Client,
    base_url: String,
}

impl PollinationsClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .user_agent("kria-assistant/1.0")
            .build()
            .unwrap_or_default();
        Self { client, base_url: base_url.into() }
    }

    /// Generate an image and return raw PNG bytes.
    pub async fn generate(
        &self,
        prompt: &str,
        width: u32,
        height: u32,
        seed: Option<u64>,
    ) -> Result<CloudImageResult, CloudError> {
        let encoded = urlencoding::encode(prompt);
        let mut url = format!(
            "{}/prompt/{}?model=flux&width={}&height={}&nologo=true",
            self.base_url.trim_end_matches('/'),
            encoded,
            width,
            height,
        );
        if let Some(s) = seed {
            url.push_str(&format!("&seed={}", s));
        }

        info!(url = %url, "CloudFallback: requesting Pollinations.ai");

        let resp = self.client.get(&url).send().await?;
        let status = resp.status();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = resp.bytes().await?;

        if bytes.is_empty() {
            return Err(CloudError::EmptyBody);
        }

        // Validate the response is actually an image, not an API error body.
        if !status.is_success() {
            let body_preview = std::str::from_utf8(&bytes[..bytes.len().min(200)])
                .unwrap_or("(binary)")
                .to_string();
            warn!(status = %status, body = %body_preview, "Pollinations.ai returned non-2xx");
            if status.as_u16() == 429 {
                return Err(CloudError::RateLimited);
            }
            return Err(CloudError::AllFailed);
        }

        // Reject JSON error responses that Pollinations returns with 200 status.
        if content_type.contains("application/json") || bytes.starts_with(b"{\"error\"") {
            let body_preview = std::str::from_utf8(&bytes[..bytes.len().min(300)])
                .unwrap_or("(binary)")
                .to_string();
            warn!(body = %body_preview, "Pollinations.ai returned error JSON");
            return Err(CloudError::AllFailed);
        }

        // Verify image magic bytes — Pollinations may return JPEG or PNG.
        let is_png = bytes.len() >= 8 && bytes.starts_with(b"\x89PNG\r\n\x1a\n");
        let is_jpeg = bytes.len() >= 3 && bytes.starts_with(b"\xff\xd8\xff");
        if !is_png && !is_jpeg {
            warn!(
                len = bytes.len(),
                prefix = ?&bytes[..bytes.len().min(16)],
                ct = %content_type,
                "Pollinations.ai response is not a valid image"
            );
            return Err(CloudError::AllFailed);
        }

        Ok(CloudImageResult {
            png_bytes: bytes.to_vec(),
            provenance: "cloud:pollinations".into(),
            width,
            height,
        })
    }
}

/// Structured refusal payload returned to the LLM as a tool result when
/// local generation is impossible.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ImageRefusal {
    pub ok: bool,
    pub code: &'static str,
    /// User-facing message (read aloud).
    pub reason_user: String,
    /// Developer-facing diagnostic.
    pub reason_dev: String,
    /// Possible fallback identifiers.
    pub fallbacks: Vec<&'static str>,
}

impl ImageRefusal {
    pub fn tier_c(free_mb: u64, required_mb: u64) -> Self {
        Self {
            ok: false,
            code: "IMAGE_TIER_C",
            reason_user: format!(
                "I can't generate images on this device — only {} MB of GPU memory is available.",
                free_mb
            ),
            reason_dev: format!(
                "vram_free_mb={} < min_required_mb={}",
                free_mb, required_mb
            ),
            fallbacks: vec!["cloud_pollinations", "cloud_hf_inference"],
        }
    }

    pub fn eviction_hang(attempts: u32) -> Self {
        Self {
            ok: false,
            code: "LLM_EVICTION_HANG",
            reason_user: "The language model couldn't release GPU memory in time. Falling back to cloud image generation.".into(),
            reason_dev: format!("LLM VRAM eviction timed out after {} attempt(s)", attempts),
            fallbacks: vec!["cloud_pollinations"],
        }
    }

    pub fn disabled() -> Self {
        Self {
            ok: false,
            code: "IMAGE_GEN_DISABLED",
            reason_user: "Image generation is currently disabled in settings.".into(),
            reason_dev: "image_generation.enabled = false".into(),
            fallbacks: vec![],
        }
    }

    pub fn to_tool_result(&self) -> crate::infra::ToolResult {
        crate::infra::ToolResult::ok(serde_json::to_value(self).unwrap_or_default())
    }
}

/// Top-level cloud fallback handler. Tries Pollinations first.
pub struct CloudFallback {
    pollinations: Arc<PollinationsClient>,
    pub enabled: bool,
}

impl CloudFallback {
    pub fn new(base_url: impl Into<String>, enabled: bool) -> Arc<Self> {
        Arc::new(Self {
            pollinations: Arc::new(PollinationsClient::new(base_url)),
            enabled,
        })
    }

    pub async fn generate(
        &self,
        prompt: &str,
        width: u32,
        height: u32,
        seed: Option<u64>,
    ) -> Result<CloudImageResult, CloudError> {
        if !self.enabled {
            return Err(CloudError::Disabled);
        }

        // Retry up to 3 times with back-off to handle transient failures.
        // 429 (rate limited) gets a much longer wait — Pollinations queues 1 request
        // per IP; hammering immediately just extends the queue-full window.
        let mut last_err = CloudError::AllFailed;
        for attempt in 0..3u32 {
            if attempt > 0 {
                let wait = match last_err {
                    CloudError::RateLimited => std::time::Duration::from_secs(30),
                    _ => std::time::Duration::from_secs(3 * attempt as u64),
                };
                warn!(attempt, wait_secs = wait.as_secs(), "CloudFallback: retrying Pollinations");
                tokio::time::sleep(wait).await;
            }
            match self.pollinations.generate(prompt, width, height, seed).await {
                Ok(r) => {
                    info!(provenance = %r.provenance, attempt, "CloudFallback: image obtained");
                    return Ok(r);
                }
                Err(e) => {
                    warn!(error = %e, attempt, "CloudFallback: Pollinations attempt failed");
                    last_err = e;
                }
            }
        }
        Err(last_err)
    }
}
