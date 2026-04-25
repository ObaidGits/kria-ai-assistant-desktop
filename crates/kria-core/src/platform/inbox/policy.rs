//! Origin-aware policy gate — classifies each [`InboundMessage`] into a
//! risk tier and decides whether it may proceed.
//!
//! # Tier matrix
//!
//! | Origin   | Default tier | Allowed capabilities               |
//! |----------|--------------|------------------------------------|
//! | Owner    | GREEN        | All (bounded by user preferences)  |
//! | Trusted  | AMBER        | Read-only + safe writes            |
//! | External | RED          | Read-only public info only         |
//! | Unknown  | BLACK        | Rejected immediately               |
//!
//! Higher-tier messages may be downgraded but never upgraded without explicit
//! owner approval.

use serde::{Deserialize, Serialize};

use super::{InboundMessage, Origin};

// ── Tier ─────────────────────────────────────────────────────────────────────

/// Risk tier assigned to an inbound message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Owner — full capability set.
    Green = 0,
    /// Trusted contact — safe writes, no destructive/system actions.
    Amber = 1,
    /// External authenticated — read-only, public info only.
    Red = 2,
    /// Unknown / unauthenticated — reject immediately.
    Black = 3,
}

impl std::fmt::Display for Tier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Tier::Green => write!(f, "GREEN"),
            Tier::Amber => write!(f, "AMBER"),
            Tier::Red => write!(f, "RED"),
            Tier::Black => write!(f, "BLACK"),
        }
    }
}

// ── Gate outcome ─────────────────────────────────────────────────────────────

/// What the policy gate decided.
#[derive(Debug, Clone)]
pub enum GateOutcome {
    /// Message may proceed; carries the assigned tier.
    Allow { tier: Tier },
    /// Message must wait for owner approval before proceeding.
    RequiresApproval { tier: Tier, reason: String },
    /// Message is rejected outright.
    Reject(RejectionReceipt),
}

/// Rejection details returned to the adapter for optional reply to the sender.
#[derive(Debug, Clone)]
pub struct RejectionReceipt {
    pub code: RejectionCode,
    pub tier: Tier,
    pub human_message: String,
}

/// Machine-readable rejection reason codes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectionCode {
    /// Sender is unknown / unauthenticated.
    UnknownSender,
    /// Sender is blocked by the owner.
    Blocked,
    /// Message exceeds rate limit for this origin.
    RateLimited,
    /// Message content is empty or unparseable.
    EmptyContent,
    /// Origin is not allowed for this action class.
    Unauthorized,
}

// ── PolicyGate ────────────────────────────────────────────────────────────────

/// Stateless policy gate (pure function — no side effects).
///
/// For stateful rate-limit enforcement use the [`RateLimitedGate`] wrapper.
pub struct PolicyGate;

impl PolicyGate {
    /// Evaluate an inbound message and return a [`GateOutcome`].
    pub fn evaluate(&self, msg: &InboundMessage) -> GateOutcome {
        let origin = Origin::from_auth(&msg.auth);

        // Empty / useless messages are rejected regardless of origin.
        if msg.text.as_deref().map(|t| t.trim().is_empty()).unwrap_or(true)
            && msg.media.is_empty()
        {
            return GateOutcome::Reject(RejectionReceipt {
                code: RejectionCode::EmptyContent,
                tier: self.base_tier(&origin),
                human_message: "Empty message — nothing to do.".into(),
            });
        }

        match origin {
            Origin::Owner => GateOutcome::Allow { tier: Tier::Green },

            Origin::Trusted => GateOutcome::Allow { tier: Tier::Amber },

            Origin::External => {
                // External senders need owner approval for every message.
                GateOutcome::RequiresApproval {
                    tier: Tier::Red,
                    reason: format!(
                        "Message from external sender '{}' on {} requires owner approval.",
                        msg.sender.display_name.as_deref().unwrap_or(&msg.sender.id),
                        msg.conversation.platform,
                    ),
                }
            }

            Origin::Unknown => GateOutcome::Reject(RejectionReceipt {
                code: RejectionCode::UnknownSender,
                tier: Tier::Black,
                human_message: "Unknown sender — message rejected.".into(),
            }),
        }
    }

    fn base_tier(&self, origin: &Origin) -> Tier {
        match origin {
            Origin::Owner => Tier::Green,
            Origin::Trusted => Tier::Amber,
            Origin::External => Tier::Red,
            Origin::Unknown => Tier::Black,
        }
    }
}

// ── RateLimitedGate ───────────────────────────────────────────────────────────

use governor::{
    clock::DefaultClock,
    state::{InMemoryState, NotKeyed},
    Quota, RateLimiter,
};
use std::num::NonZeroU32;
use std::sync::Arc;

/// [`PolicyGate`] wrapper that enforces per-origin rate limits.
pub struct RateLimitedGate {
    inner: PolicyGate,
    owner_limiter: Arc<RateLimiter<NotKeyed, InMemoryState, DefaultClock>>,
    trusted_limiter: Arc<RateLimiter<NotKeyed, InMemoryState, DefaultClock>>,
    external_limiter: Arc<RateLimiter<NotKeyed, InMemoryState, DefaultClock>>,
}

impl RateLimitedGate {
    /// Create with sensible defaults:
    /// - Owner: 120 messages / minute
    /// - Trusted: 60 messages / minute
    /// - External: 10 messages / minute
    pub fn with_defaults() -> Self {
        Self {
            inner: PolicyGate,
            owner_limiter: Arc::new(RateLimiter::direct(
                Quota::per_minute(NonZeroU32::new(120).unwrap()),
            )),
            trusted_limiter: Arc::new(RateLimiter::direct(
                Quota::per_minute(NonZeroU32::new(60).unwrap()),
            )),
            external_limiter: Arc::new(RateLimiter::direct(
                Quota::per_minute(NonZeroU32::new(10).unwrap()),
            )),
        }
    }

    pub fn evaluate(&self, msg: &InboundMessage) -> GateOutcome {
        let origin = Origin::from_auth(&msg.auth);

        let limiter = match &origin {
            Origin::Owner => &self.owner_limiter,
            Origin::Trusted => &self.trusted_limiter,
            Origin::External | Origin::Unknown => &self.external_limiter,
        };

        if limiter.check().is_err() {
            return GateOutcome::Reject(RejectionReceipt {
                code: RejectionCode::RateLimited,
                tier: self.inner.base_tier(&origin),
                human_message: "Rate limit exceeded — please slow down.".into(),
            });
        }

        self.inner.evaluate(msg)
    }
}
