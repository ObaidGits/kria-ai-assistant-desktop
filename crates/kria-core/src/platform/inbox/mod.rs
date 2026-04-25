//! Universal Remote Inbox Pipeline — canonical types.
//!
//! All remote inbound messages (Telegram, Discord, future platforms) are
//! normalised into [`InboundMessage`] before entering the agent pipeline.
//! All outbound replies are expressed as [`OutboundMessage`] and dispatched
//! by [`egress::EgressRouter`].

pub mod adapter;
pub mod approval;
pub mod egress;
pub mod media;
pub mod policy;
pub mod queue;

use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Platform identity ────────────────────────────────────────────────────────

/// Source / destination platform for a message.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Telegram,
    Discord,
    /// Catch-all for future platforms identified by an opaque tag.
    Other(String),
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Platform::Telegram => write!(f, "telegram"),
            Platform::Discord => write!(f, "discord"),
            Platform::Other(s) => write!(f, "{s}"),
        }
    }
}

// ── Participants ─────────────────────────────────────────────────────────────

/// A participant (sender or recipient) in a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Participant {
    /// Platform-native user/channel ID (stable, opaque).
    pub id: String,
    /// Display name (may change; used for logging / voice).
    pub display_name: Option<String>,
    /// Whether this participant is a bot account on the platform.
    pub is_bot: bool,
}

/// Identifies a unique conversation thread (used as queue partition key).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConversationKey {
    pub platform: Platform,
    /// Platform-native chat/channel/guild ID.
    pub chat_id: String,
}

impl std::fmt::Display for ConversationKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.platform, self.chat_id)
    }
}

// ── Auth context ─────────────────────────────────────────────────────────────

/// Authorization context attached to every inbound message.
/// The [`policy::PolicyGate`] uses this to decide the tier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthContext {
    /// Whether the sender is the owner of this KRIA instance.
    pub is_owner: bool,
    /// Whether the sender has been pre-approved as a trusted contact.
    pub is_trusted: bool,
    /// Raw platform role tags, e.g. "admin", "member".
    pub roles: Vec<String>,
}

impl AuthContext {
    pub fn owner() -> Self {
        Self {
            is_owner: true,
            is_trusted: true,
            roles: vec![],
        }
    }

    pub fn trusted() -> Self {
        Self {
            is_owner: false,
            is_trusted: true,
            roles: vec![],
        }
    }

    pub fn unknown() -> Self {
        Self {
            is_owner: false,
            is_trusted: false,
            roles: vec![],
        }
    }
}

// ── Media references ─────────────────────────────────────────────────────────

/// Kind of media attachment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Image,
    Audio,
    Video,
    Document,
    Sticker,
    Voice,
    Other(String),
}

/// A media attachment whose bytes may be local or still remote (cloud URL).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaRef {
    pub kind: MediaKind,
    /// Platform-native file identifier (e.g. Telegram file_id).
    pub remote_id: Option<String>,
    /// Direct download URL (if available).
    pub remote_url: Option<String>,
    /// Path on local disk — populated by [`media::MediaResolver`] at runtime.
    pub local_path: Option<PathBuf>,
    /// MIME type when known.
    pub mime_type: Option<String>,
    /// File size in bytes when known.
    pub size_bytes: Option<u64>,
}

// ── Canonical inbound message ─────────────────────────────────────────────────

/// Canonical normalised inbound message envelope.
///
/// Created by an [`adapter::IngressAdapter`] and stored in the durable queue
/// before being handed to the agent pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundMessage {
    /// Stable UUID (v7 — time-sortable) assigned at ingress.
    pub id: Uuid,
    /// Source platform + conversation.
    pub conversation: ConversationKey,
    /// Who sent the message.
    pub sender: Participant,
    /// Plain text body (may be empty if message is media-only).
    pub text: Option<String>,
    /// Attached media (zero or more).
    pub media: Vec<MediaRef>,
    /// Auth context for policy gating.
    pub auth: AuthContext,
    /// Platform-native message ID (for reply threading, dedup).
    pub native_message_id: Option<String>,
    /// When the message was originally sent on the platform.
    pub sent_at: SystemTime,
    /// When this envelope was created by KRIA ingress.
    pub ingested_at: SystemTime,
    /// Arbitrary platform-specific metadata (for debugging / future use).
    pub metadata: serde_json::Value,
}

impl InboundMessage {
    /// Create a new envelope with `id` and `ingested_at` set automatically.
    pub fn new(
        conversation: ConversationKey,
        sender: Participant,
        auth: AuthContext,
        sent_at: SystemTime,
    ) -> Self {
        Self {
            id: Uuid::now_v7(),
            conversation,
            sender,
            text: None,
            media: vec![],
            auth,
            native_message_id: None,
            sent_at,
            ingested_at: SystemTime::now(),
            metadata: serde_json::Value::Null,
        }
    }
}

// ── Canonical outbound message ────────────────────────────────────────────────

/// Canonical outbound reply envelope.
///
/// Created by the agent pipeline and dispatched by [`egress::EgressRouter`]
/// to the correct [`adapter::EgressAdapter`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundMessage {
    /// Stable UUID (v7) assigned at creation.
    pub id: Uuid,
    /// Destination conversation.
    pub conversation: ConversationKey,
    /// Plain text content to send.
    pub text: String,
    /// Media to attach (optional).
    pub media: Vec<MediaRef>,
    /// If replying to a specific message, the native ID of that message.
    pub reply_to_native_id: Option<String>,
    /// When this reply was composed.
    pub composed_at: SystemTime,
}

impl OutboundMessage {
    pub fn text_reply(conversation: ConversationKey, text: impl Into<String>) -> Self {
        Self {
            id: Uuid::now_v7(),
            conversation,
            text: text.into(),
            media: vec![],
            reply_to_native_id: None,
            composed_at: SystemTime::now(),
        }
    }
}

// ── Origin classification (for policy) ───────────────────────────────────────

/// High-level origin category used by the policy gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// The owner of this KRIA instance.
    Owner,
    /// A pre-approved trusted contact.
    Trusted,
    /// Any authenticated platform user (not explicitly trusted).
    External,
    /// Unknown / unauthenticated sender.
    Unknown,
}

impl Origin {
    pub fn from_auth(auth: &AuthContext) -> Self {
        if auth.is_owner {
            Origin::Owner
        } else if auth.is_trusted {
            Origin::Trusted
        } else {
            Origin::External
        }
    }
}
