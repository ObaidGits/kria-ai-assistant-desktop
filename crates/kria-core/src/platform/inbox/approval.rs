//! Human-in-the-loop approval broker.
//!
//! When the [`super::policy::PolicyGate`] returns
//! [`GateOutcome::RequiresApproval`], the message is parked here until the
//! owner explicitly approves or denies it (or the request times out).

use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tokio::sync::oneshot;
use tracing::{info, warn};
use uuid::Uuid;

use super::{ConversationKey, InboundMessage};

// ── Decision ──────────────────────────────────────────────────────────────────

/// Owner's decision for a pending approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    Approved,
    Denied,
    /// Request timed out before the owner responded.
    TimedOut,
}

// ── Pending record ────────────────────────────────────────────────────────────

/// Public view of a message waiting for owner approval.
/// The internal resolution channel is managed by [`ApprovalBroker`].
pub struct PendingApproval {
    pub id: Uuid,
    pub message: InboundMessage,
    pub reason: String,
    pub queued_at: Instant,
    /// Timeout after which the record is auto-denied.
    pub timeout: Duration,
}

// ── ApprovalBroker ────────────────────────────────────────────────────────────

/// Stores pending approvals and resolves them when the owner responds.
///
/// Safe to clone — all clones share the same underlying map.
#[derive(Clone)]
pub struct ApprovalBroker {
    pending: Arc<DashMap<Uuid, PendingEntry>>,
    default_timeout: Duration,
}

struct PendingEntry {
    message: InboundMessage,
    reason: String,
    queued_at: Instant,
    timeout: Duration,
    tx: Option<oneshot::Sender<ApprovalDecision>>,
}

impl ApprovalBroker {
    /// Create a new broker with `default_timeout` for each pending request.
    pub fn new(default_timeout: Duration) -> Self {
        Self {
            pending: Arc::new(DashMap::new()),
            default_timeout,
        }
    }

    /// Park a message and return a [`oneshot::Receiver`] that resolves when
    /// the owner approves/denies (or the request times out).
    ///
    /// The caller should spawn a task awaiting on the receiver.
    pub fn park(
        &self,
        message: InboundMessage,
        reason: String,
    ) -> (Uuid, oneshot::Receiver<ApprovalDecision>) {
        let id = Uuid::now_v7();
        let (tx, rx) = oneshot::channel();

        self.pending.insert(
            id,
            PendingEntry {
                message,
                reason,
                queued_at: Instant::now(),
                timeout: self.default_timeout,
                tx: Some(tx),
            },
        );

        info!(approval_id = %id, "approval_broker: parked message awaiting owner decision");
        (id, rx)
    }

    /// Owner approves the pending request.
    /// Returns `true` if the request was found and the decision delivered.
    pub fn approve(&self, id: Uuid) -> bool {
        self.resolve(id, ApprovalDecision::Approved)
    }

    /// Owner denies the pending request.
    /// Returns `true` if the request was found and the decision delivered.
    pub fn deny(&self, id: Uuid) -> bool {
        self.resolve(id, ApprovalDecision::Denied)
    }

    /// Retrieve the pending message for display to the owner (without removing it).
    pub fn peek(&self, id: Uuid) -> Option<(InboundMessage, String)> {
        self.pending
            .get(&id)
            .map(|e| (e.message.clone(), e.reason.clone()))
    }

    /// List all pending approval IDs and their source conversations.
    pub fn list(&self) -> Vec<(Uuid, ConversationKey, String)> {
        self.pending
            .iter()
            .map(|e| (e.key().clone(), e.message.conversation.clone(), e.reason.clone()))
            .collect()
    }

    /// Sweep expired requests and auto-deny them.
    /// Call this periodically (e.g. every 30 seconds).
    pub fn sweep_expired(&self) {
        let expired: Vec<Uuid> = self
            .pending
            .iter()
            .filter(|e| e.queued_at.elapsed() > e.timeout)
            .map(|e| *e.key())
            .collect();

        for id in expired {
            warn!(approval_id = %id, "approval_broker: request timed out, auto-denying");
            self.resolve(id, ApprovalDecision::TimedOut);
        }
    }

    fn resolve(&self, id: Uuid, decision: ApprovalDecision) -> bool {
        if let Some(mut entry) = self.pending.get_mut(&id) {
            if let Some(tx) = entry.tx.take() {
                let _ = tx.send(decision);
                drop(entry);
                self.pending.remove(&id);
                return true;
            }
        }
        false
    }
}

/// Spawn a background task that sweeps the broker for expired approvals
/// every `interval`.
pub fn spawn_sweep_task(
    broker: ApprovalBroker,
    interval: Duration,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(interval) => {
                    broker.sweep_expired();
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        return;
                    }
                }
            }
        }
    })
}
