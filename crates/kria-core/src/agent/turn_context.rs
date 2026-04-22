//! Per-turn execution context.
//!
//! `TurnContext` is created once per user turn and carries:
//! - A `CancellationToken` that aborts all in-flight work for the turn.
//! - A payload cache that maps UUID handles to full MCP response `Value`s so
//!   that the UI can request full detail without re-running the tool.
//!
//! The token is stored in `AgentLoop::active_cancels` keyed by session ID and
//! is publicly accessible via `AgentLoop::cancel_session`.

use dashmap::DashMap;
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Per-turn execution context shared across the agent loop and MCP handlers.
pub struct TurnContext {
    /// Cancel this token to abort all in-flight work for the current turn.
    pub cancel: CancellationToken,
    /// Full MCP response payloads, keyed by the UUID handle emitted in
    /// `ShapedPayload::handle`.  The UI can request a specific handle to
    /// retrieve the untruncated response.
    pub payload_cache: Arc<DashMap<Uuid, Arc<Value>>>,
}

impl TurnContext {
    /// Create a new context with a fresh cancellation token and empty cache.
    pub fn new() -> Self {
        Self {
            cancel: CancellationToken::new(),
            payload_cache: Arc::new(DashMap::new()),
        }
    }

    /// Store a full payload and return its UUID handle.
    pub fn cache_payload(&self, value: Value) -> Uuid {
        let id = Uuid::new_v4();
        self.payload_cache.insert(id, Arc::new(value));
        id
    }

    /// Retrieve a cached payload by handle.
    pub fn get_payload(&self, handle: Uuid) -> Option<Arc<Value>> {
        self.payload_cache.get(&handle).map(|v| Arc::clone(&*v))
    }
}

impl Default for TurnContext {
    fn default() -> Self {
        Self::new()
    }
}
