//! [`IngressAdapter`] and [`EgressAdapter`] traits.
//!
//! Each supported platform (Telegram, Discord, …) provides one struct that
//! implements both traits.  The inbox pipeline only depends on these traits,
//! keeping platform code fully decoupled.

use async_trait::async_trait;
use uuid::Uuid;

use super::{InboundMessage, OutboundMessage};

// ── Delivery receipts ────────────────────────────────────────────────────────

/// Outcome of delivering an [`OutboundMessage`] to the platform.
#[derive(Debug, Clone)]
pub struct DeliveryReceipt {
    /// The `OutboundMessage::id` this receipt belongs to.
    pub outbound_id: Uuid,
    /// Platform-native sent-message ID (for reply threading etc.)
    pub native_message_id: Option<String>,
    /// Whether the delivery was acknowledged by the platform.
    pub delivered: bool,
    /// Human-readable failure reason (if `!delivered`).
    pub error: Option<String>,
}

// ── Approval prompt ──────────────────────────────────────────────────────────

/// A human-readable approval request surfaced to the user before a sensitive
/// action is executed on behalf of a remote caller.
#[derive(Debug, Clone)]
pub struct ApprovalPrompt {
    /// Unique ID for this approval request.
    pub id: Uuid,
    /// The inbound message that triggered this.
    pub inbound_id: Uuid,
    /// Short human-readable action description (for voice read-out).
    pub action_summary: String,
    /// Full detail: args, impact, rollback path.
    pub detail: String,
    /// Risk tier driving this prompt ("green" / "amber" / "red" / "black").
    pub tier: String,
}

// ── IngressAdapter ───────────────────────────────────────────────────────────

/// Receives messages from a remote platform and normalises them into
/// [`InboundMessage`] envelopes.
///
/// Implementors are expected to run a background polling / webhook loop
/// and push messages to the provided sender channel.
#[async_trait]
pub trait IngressAdapter: Send + Sync + 'static {
    /// Unique identifier for this adapter instance (e.g. "telegram", "discord").
    fn platform_id(&self) -> &'static str;

    /// Start ingesting messages.  The adapter pushes canonical
    /// [`InboundMessage`]s into `tx` until `shutdown` is signalled.
    ///
    /// This is a long-running future that should be `tokio::spawn`ed by
    /// the caller.
    async fn run(
        self: Box<Self>,
        tx: tokio::sync::mpsc::Sender<InboundMessage>,
        shutdown: tokio::sync::watch::Receiver<bool>,
    );
}

// ── EgressAdapter ────────────────────────────────────────────────────────────

/// Sends [`OutboundMessage`]s back to a specific remote platform.
#[async_trait]
pub trait EgressAdapter: Send + Sync + 'static {
    /// Platform this adapter handles (must match [`InboundMessage::conversation::platform`]).
    fn platform_id(&self) -> &'static str;

    /// Deliver a single outbound message.  Returns a [`DeliveryReceipt`].
    async fn send(&self, msg: OutboundMessage) -> DeliveryReceipt;
}
