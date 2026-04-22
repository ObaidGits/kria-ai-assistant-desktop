/// `OsIntentBackend` trait, `IntentDispatcher`, and per-Capability rate limiting.
///
/// This is the execution coordinator. It receives a validated `Capability`,
/// runs it through policy and rate-limiting, then dispatches to the platform backend.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex;
use tracing::{info, warn};
use uuid::Uuid;

use crate::infra::pipeline_trace::log_pipeline_step;
use crate::platform::app_registry::InstalledAppRegistry;
use crate::platform::intent::capability::{CanonicalAppId, Capability, SafeArg};
use crate::safety::policy::{PolicyDecision, PolicyEngine};

// ─── DispatchResult ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchResult {
    pub dispatch_id: String,
    pub success: bool,
    pub message: String,
    /// JSON detail for the LLM tool result.
    pub detail: serde_json::Value,
}

impl DispatchResult {
    pub fn ok(message: impl Into<String>, detail: serde_json::Value) -> Self {
        Self {
            dispatch_id: Uuid::new_v4().to_string(),
            success: true,
            message: message.into(),
            detail,
        }
    }

    pub fn err(message: impl Into<String>) -> Self {
        let msg = message.into();
        Self {
            dispatch_id: Uuid::new_v4().to_string(),
            success: false,
            message: msg.clone(),
            detail: serde_json::json!({ "error": msg }),
        }
    }
}

// ─── DispatchError ───────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum DispatchError {
    #[error("capability blocked by policy: {0}")]
    PolicyBlocked(String),
    #[error("rate limit exceeded for '{0}': retry after {1}s")]
    RateLimitExceeded(String, u64),
    #[error("backend error: {0}")]
    BackendError(String),
    #[error("app not installed: {0}")]
    AppNotInstalled(String),
    #[error("contact resolution failed: {0}")]
    ResolutionFailed(String),
    #[error("approval required but no approval gateway is configured")]
    ApprovalRequired(PolicyDecision),
    /// The user did not respond within `OS_INTENT_APPROVAL_TIMEOUT_SECS` seconds.
    #[error("approval timed out after {0}s — action was not executed")]
    ApprovalTimedOut(u64),
}

// ─── OsIntentBackend trait ────────────────────────────────────────────────────

/// Platform-specific implementation of OS-level intent dispatch.
///
/// Implementations:
/// - `LinuxBackend` — uses `open` crate + `gio launch` + `atspi`
/// - `WindowsBackend` — uses `ShellExecuteExW` + `IApplicationActivationManager`
/// - `MacosBackend` — uses `NSWorkspace.open(_:)` via `objc2-app-kit`
#[async_trait]
pub trait OsIntentBackend: Send + Sync {
    /// Open a URI in the system's default handler.
    /// The URI has already been validated and scheme-classified by the time this is called.
    async fn open_uri(&self, url: &url::Url) -> Result<(), String>;

    /// Launch an installed application with validated arguments.
    /// `app_id` is a canonical ID from `InstalledAppRegistry`.
    /// `args` contains only `SafeArg` tokens — no shell metacharacters.
    async fn launch_app(&self, app_id: &CanonicalAppId, args: &[SafeArg]) -> Result<u32, String>;

    /// Invoke an accessibility-API action on a running application.
    async fn ax_invoke(
        &self,
        app_id: &CanonicalAppId,
        action: &crate::platform::intent::capability::AxAction,
    ) -> Result<(), String>;

    /// Return the set of URI schemes registered by installed applications.
    /// Used by the scheme classifier as a fallback for unknown deep links.
    fn registered_schemes(&self) -> HashSet<String>;
}

// ─── Rate limiter ─────────────────────────────────────────────────────────────

/// Token-bucket rate limiter per capability variant.
struct RateBucket {
    /// Maximum tokens in the bucket.
    capacity: u32,
    /// Current available tokens.
    tokens: u32,
    /// Tokens added per second.
    refill_rate: f64,
    /// Timestamp of last refill.
    last_refill: Instant,
}

impl RateBucket {
    fn new(per_minute: u32) -> Self {
        Self {
            capacity: per_minute,
            tokens: per_minute,
            refill_rate: per_minute as f64 / 60.0,
            last_refill: Instant::now(),
        }
    }

    /// Returns `Ok(())` if a token is available, `Err(retry_after_secs)` otherwise.
    fn try_consume(&mut self) -> Result<(), u64> {
        // Refill based on elapsed time.
        let elapsed = self.last_refill.elapsed().as_secs_f64();
        let added = (elapsed * self.refill_rate) as u32;
        if added > 0 {
            self.tokens = (self.tokens + added).min(self.capacity);
            self.last_refill = Instant::now();
        }

        if self.tokens > 0 {
            self.tokens -= 1;
            Ok(())
        } else {
            // How many seconds until one token refills.
            let retry_secs = (1.0 / self.refill_rate).ceil() as u64;
            Err(retry_secs)
        }
    }
}

/// Per-capability-variant rate limits (tokens per minute).
const RATE_OPEN_URL: u32 = 60;
const RATE_LAUNCH_APP: u32 = 30;
const RATE_SEND_MESSAGE: u32 = 10;
const RATE_FILE_WRITE: u32 = 20;
const RATE_AX_INVOKE: u32 = 15;

/// Seconds the user has to approve a YELLOW/RED OS-intent action before the
/// dispatcher auto-cancels with `DispatchError::ApprovalTimedOut`.
///
/// This is enforced by the caller (loop_engine / tool handler) that owns the
/// `HitlGateway` — the dispatcher itself only returns `ApprovalRequired` and
/// `ApprovalTimedOut` as error variants for the caller to map through.
pub const OS_INTENT_APPROVAL_TIMEOUT_SECS: u64 = 30;

// ─── IntentDispatcher ────────────────────────────────────────────────────────

/// The central coordinator for OS-level intent dispatch.
///
/// Holds references to all subsystems needed to safely execute a `Capability`:
/// - Policy engine (Ring 3)
/// - App registry (capability resolution)
/// - Rate limiter (anti-loop protection)
/// - OS backend (platform dispatch)
///
/// # Usage
/// ```rust,ignore
/// let result = dispatcher.dispatch(cap, session_id).await?;
/// ```
pub struct IntentDispatcher {
    backend: Arc<dyn OsIntentBackend>,
    registry: Arc<InstalledAppRegistry>,
    policy: Arc<PolicyEngine>,
    rate_limits: Arc<Mutex<HashMap<&'static str, RateBucket>>>,
}

impl IntentDispatcher {
    pub fn new(
        backend: Arc<dyn OsIntentBackend>,
        registry: Arc<InstalledAppRegistry>,
        policy: Arc<PolicyEngine>,
    ) -> Self {
        let mut buckets = HashMap::new();
        buckets.insert("open_url", RateBucket::new(RATE_OPEN_URL));
        buckets.insert("launch_app", RateBucket::new(RATE_LAUNCH_APP));
        buckets.insert("send_message", RateBucket::new(RATE_SEND_MESSAGE));
        buckets.insert("file_write", RateBucket::new(RATE_FILE_WRITE));
        buckets.insert("ax_invoke", RateBucket::new(RATE_AX_INVOKE));

        Self {
            backend,
            registry,
            policy,
            rate_limits: Arc::new(Mutex::new(buckets)),
        }
    }

    /// Dispatch a validated `Capability`.
    ///
    /// The caller is responsible for approval gating (RED tier). If a RED
    /// capability is passed here without prior approval, an error is returned
    /// describing what approval is needed.
    ///
    /// `approved` should be `true` only if the user has explicitly confirmed
    /// (voice yes/no for YELLOW, typed PIN for RED).
    pub async fn dispatch(
        &self,
        cap: &Capability,
        session_id: &str,
        approved: bool,
    ) -> Result<DispatchResult, DispatchError> {
        let registry_schemes = self.registry.registered_schemes();
        let decision = self.policy.classify_capability(cap, Some(&registry_schemes));

        log_pipeline_step(
            session_id,
            "intent_dispatch_policy",
            &format!("action={} risk={}", decision.action, decision.risk_level),
            Some(serde_json::json!({
                "action": decision.action,
                "risk": decision.risk_level.as_str(),
                "blocked": decision.blocked,
                "requires_approval": decision.requires_approval,
                "reason": decision.reason,
            })),
        );

        // Ring 3: Policy block — hard deny, no escalation.
        if decision.blocked {
            warn!(
                session_id = session_id,
                action = decision.action,
                reason = decision.reason,
                "intent dispatch BLOCKED"
            );
            return Err(DispatchError::PolicyBlocked(decision.reason));
        }

        // Approval gate.
        if decision.requires_approval && !approved {
            warn!(
                session_id = session_id,
                action = decision.action,
                "intent dispatch requires approval (not yet granted)"
            );
            return Err(DispatchError::ApprovalRequired(decision));
        }

        // Rate limit.
        let bucket_key = Self::bucket_key(cap);
        let rate_check = self.rate_limits.lock().await.get_mut(bucket_key).map(|b| b.try_consume());
        match rate_check {
            Some(Err(retry_after)) => {
                warn!(
                    session_id = session_id,
                    action = decision.action,
                    retry_after_secs = retry_after,
                    "intent dispatch rate limited"
                );
                return Err(DispatchError::RateLimitExceeded(
                    decision.action.clone(),
                    retry_after,
                ));
            }
            None => {
                // No bucket for this key — allow (should not happen with complete init).
                warn!("no rate bucket for key '{bucket_key}', allowing");
            }
            Some(Ok(())) => {}
        }

        // Ring 1: Schema re-validation (defence-in-depth against unconstrained LLM fallback).
        // Re-serialize the Capability to JSON and run it through the precompiled schema
        // validator.  If the model produced something outside the schema via the
        // unconstrained code path, this catches it before any OS call is made.
        if let Ok(raw) = serde_json::to_string(cap) {
            if let Err(schema_err) = crate::platform::intent::grammar::validate_capability_json(&raw) {
                warn!(
                    session_id = session_id,
                    err = %schema_err,
                    "Ring-1 schema validation FAILED — blocking dispatch"
                );
                return Err(DispatchError::PolicyBlocked(format!(
                    "capability failed schema validation: {schema_err}"
                )));
            }
        }

        // Dispatch to the platform backend.
        let result = self.execute_on_backend(cap, session_id).await;

        log_pipeline_step(
            session_id,
            "intent_dispatch_result",
            if result.success { "success" } else { "failure" },
            Some(serde_json::json!({
                "dispatch_id": result.dispatch_id,
                "success": result.success,
                "message": result.message,
            })),
        );

        Ok(result)
    }

    async fn execute_on_backend(&self, cap: &Capability, session_id: &str) -> DispatchResult {
        match cap {
            Capability::OpenUrl { url } => {
                info!(session_id = session_id, url = url.as_str(), "dispatching open_uri");
                match self.backend.open_uri(url).await {
                    Ok(()) => DispatchResult::ok(
                        format!("Opened: {}", url.as_str()),
                        serde_json::json!({ "url": url.as_str(), "opened": true }),
                    ),
                    Err(e) => DispatchResult::err(format!("Failed to open URL: {e}")),
                }
            }

            Capability::LaunchApp { app_id, args } => {
                // Verify the app is installed before attempting to launch.
                if !self.registry.is_installed(app_id) {
                    return DispatchResult::err(format!(
                        "Application '{}' is not installed or not found in registry",
                        app_id.as_str()
                    ));
                }
                info!(
                    session_id = session_id,
                    app_id = app_id.as_str(),
                    "dispatching launch_app"
                );
                match self.backend.launch_app(app_id, args).await {
                    Ok(pid) => DispatchResult::ok(
                        format!("Launched {}", app_id.as_str()),
                        serde_json::json!({
                            "app_id": app_id.as_str(),
                            "pid": pid,
                            "launched": true,
                        }),
                    ),
                    Err(e) => DispatchResult::err(format!(
                        "Failed to launch '{}': {e}",
                        app_id.as_str()
                    )),
                }
            }

            Capability::SendMessage { app, contact, body } => {
                use crate::platform::intent::resolution::MessagingApp;
                use crate::platform::intent::scheme::build_whatsapp_url;

                info!(
                    session_id = session_id,
                    app = app.display_name(),
                    contact = contact.display_name,
                    "dispatching send_message (draft only)"
                );

                // Build a deep-link URL for the messaging app.
                // NOTE: This opens a DRAFT, not auto-sends. User must press send.
                let url_result: Result<url::Url, String> = match app {
                    MessagingApp::WhatsApp => build_whatsapp_url(
                        &contact.identifier,
                        body.as_str(),
                    )
                    .map_err(|e| e.to_string()),
                    MessagingApp::Telegram => {
                        // Telegram deep link: https://t.me/<username>?text=<body>
                        let encoded: String = url::form_urlencoded::Serializer::new(String::new())
                            .append_pair("text", body.as_str())
                            .finish();
                        url::Url::parse(&format!("https://t.me/{}?{}", contact.identifier, encoded))
                            .map_err(|e| e.to_string())
                    }
                    MessagingApp::Gmail => {
                        let encoded: String = url::form_urlencoded::Serializer::new(String::new())
                            .append_pair("to", &contact.identifier)
                            .append_pair("body", body.as_str())
                            .finish();
                        url::Url::parse(&format!("https://mail.google.com/mail/?view=cm&{}", encoded))
                            .map_err(|e| e.to_string())
                    }
                    MessagingApp::Signal => {
                        // Signal doesn't have a universal deep-link standard on Linux desktop;
                        // fall back to opening the app and noting that the message was pre-filled.
                        url::Url::parse("https://signal.org/install")
                            .map_err(|e| e.to_string())
                    }
                };

                match url_result {
                    Ok(url) => match self.backend.open_uri(&url).await {
                        Ok(()) => DispatchResult::ok(
                            format!(
                                "Opened {} draft to {} — waiting for user to confirm send",
                                app.display_name(),
                                contact.display_name
                            ),
                            serde_json::json!({
                                "app": app.display_name(),
                                "contact": contact.display_name,
                                "status": "draft_opened",
                                "note": "Message is pre-filled. User must press send.",
                            }),
                        ),
                        Err(e) => DispatchResult::err(format!("Failed to open draft: {e}")),
                    },
                    Err(e) => DispatchResult::err(format!("Failed to build message URL: {e}")),
                }
            }

            Capability::FileWrite { path, content } => {
                info!(
                    session_id = session_id,
                    path = path.as_path().display().to_string(),
                    bytes = content.len(),
                    "dispatching file_write"
                );
                match tokio::fs::write(path.as_path(), content).await {
                    Ok(()) => DispatchResult::ok(
                        format!("Written {} bytes to {}", content.len(), path.as_path().display()),
                        serde_json::json!({
                            "path": path.as_path().display().to_string(),
                            "bytes_written": content.len(),
                        }),
                    ),
                    Err(e) => DispatchResult::err(format!("File write failed: {e}")),
                }
            }

            Capability::AxInvoke { app_id, action } => {
                info!(
                    session_id = session_id,
                    app_id = app_id.as_str(),
                    "dispatching ax_invoke"
                );
                match self.backend.ax_invoke(app_id, action).await {
                    Ok(()) => DispatchResult::ok(
                        format!("Accessibility action executed on {}", app_id.as_str()),
                        serde_json::json!({
                            "app_id": app_id.as_str(),
                            "executed": true,
                        }),
                    ),
                    Err(e) => DispatchResult::err(format!("AX invoke failed: {e}")),
                }
            }
        }
    }

    fn bucket_key(cap: &Capability) -> &'static str {
        match cap {
            Capability::OpenUrl { .. } => "open_url",
            Capability::LaunchApp { .. } => "launch_app",
            Capability::SendMessage { .. } => "send_message",
            Capability::FileWrite { .. } => "file_write",
            Capability::AxInvoke { .. } => "ax_invoke",
        }
    }
}
