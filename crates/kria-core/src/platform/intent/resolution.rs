/// Contact resolution types and the `ContactResolver` trait.
///
/// # Security design
/// The resolver **never** silently picks the first match when a query is ambiguous.
/// Ambiguity must propagate as a structured error to the LLM context so that K.R.I.A.
/// asks the user to disambiguate before sending any message.
///
/// # Messaging apps
/// `MessagingApp` is an enum, not a string, so the LLM cannot invent app names and
/// route to unintended targets.
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ─── MessagingApp ─────────────────────────────────────────────────────────────

/// Messaging applications that K.R.I.A. supports for `SendMessage` capabilities.
/// Adding a new entry here requires code, not just LLM output.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessagingApp {
    WhatsApp,
    Gmail,
    Telegram,
    Signal,
}

impl MessagingApp {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::WhatsApp => "WhatsApp",
            Self::Gmail => "Gmail",
            Self::Telegram => "Telegram",
            Self::Signal => "Signal",
        }
    }
}

impl std::fmt::Display for MessagingApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.display_name())
    }
}

// ─── ContactId ───────────────────────────────────────────────────────────────

/// A resolved contact identity, scoped to a specific messaging application.
///
/// Created only by a `ContactResolver` implementation — never constructed
/// directly from LLM output.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContactId {
    /// Display name from the address book (for audit logging and UI).
    pub display_name: String,
    /// App-specific routing identifier:
    /// - WhatsApp / Signal: E.164 phone number (e.g., `+919876543210`)
    /// - Gmail: email address
    /// - Telegram: `@username` or phone
    pub identifier: String,
    /// The app this contact ID is valid for.
    pub app: MessagingApp,
}

// ─── Candidate ───────────────────────────────────────────────────────────────

/// A candidate match returned when a query is ambiguous.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Candidate {
    pub contact_id: ContactId,
    /// Match confidence 0.0–1.0 (higher = better match).
    pub confidence: f32,
    /// Human-readable explanation of why this candidate matched
    /// (e.g., "exact name match", "phonetic match: Anjli → Anjali").
    pub match_reason: String,
}

// ─── ResolutionError ─────────────────────────────────────────────────────────

/// All errors that can occur during contact resolution.
///
/// `Ambiguous` is the critical case: it carries the top candidates so the
/// caller can surface them to the user for manual selection.
#[derive(Debug, Error, Serialize, Deserialize)]
pub enum ResolutionError {
    /// Multiple contacts matched the query. The LLM must ask the user to pick one.
    /// Carries up to 3 candidates with confidence scores.
    #[error("ambiguous contact '{query}': {candidate_count} matches found — ask the user to specify")]
    Ambiguous {
        query: String,
        candidates: Vec<Candidate>,
        candidate_count: usize,
    },

    /// No contacts matched the query.
    #[error("no contact found for '{query}'")]
    NotFound { query: String },

    /// A contact was found but lacks the required identifier for the target app.
    /// E.g., contact has no phone number for WhatsApp.
    #[error("contact '{name}' found but has no {field} for {app}")]
    Incomplete {
        name: String,
        field: String,
        app: MessagingApp,
    },

    /// The underlying address book could not be accessed.
    #[error("address book error: {reason}")]
    BackendError { reason: String },
}

impl ResolutionError {
    /// Build an `Ambiguous` error from a set of candidates.
    /// Automatically sets `candidate_count`.
    pub fn ambiguous(query: impl Into<String>, candidates: Vec<Candidate>) -> Self {
        let candidate_count = candidates.len();
        Self::Ambiguous {
            query: query.into(),
            candidates,
            candidate_count,
        }
    }

    /// Serialize to a JSON value suitable for returning as a `ToolResult::err` payload.
    /// The LLM can read this and formulate a clarification question.
    pub fn to_tool_error_payload(&self) -> serde_json::Value {
        match self {
            Self::Ambiguous {
                query, candidates, ..
            } => {
                let candidates_json: Vec<serde_json::Value> = candidates
                    .iter()
                    .map(|c| {
                        serde_json::json!({
                            "name": c.contact_id.display_name,
                            "identifier": c.contact_id.identifier,
                            "confidence": c.confidence,
                            "reason": c.match_reason,
                        })
                    })
                    .collect();
                serde_json::json!({
                    "error": "AMBIGUOUS_CONTACT",
                    "query": query,
                    "message": format!(
                        "Multiple contacts found for '{}'. Please ask the user to specify.",
                        query
                    ),
                    "candidates": candidates_json,
                    "instruction": "Ask the user: which of these did you mean?",
                })
            }
            Self::NotFound { query } => serde_json::json!({
                "error": "CONTACT_NOT_FOUND",
                "query": query,
                "message": format!("No contact found for '{}'. Ask the user for the full name or phone number.", query),
            }),
            Self::Incomplete { name, field, app } => serde_json::json!({
                "error": "CONTACT_INCOMPLETE",
                "name": name,
                "missing_field": field,
                "app": app.display_name(),
                "message": format!(
                    "Found '{}' but they have no {} — cannot use {} for this contact.",
                    name, field, app.display_name()
                ),
            }),
            Self::BackendError { reason } => serde_json::json!({
                "error": "CONTACT_BACKEND_ERROR",
                "reason": reason,
            }),
        }
    }
}

// ─── ContactResolver trait ───────────────────────────────────────────────────

/// Resolve a natural-language contact name to a `ContactId` for a specific messaging app.
///
/// # Implementation requirements
/// 1. Use NFC normalization → casefold → diacritic-fold → exact > prefix > phonetic matching.
/// 2. Return at most 3 candidates ordered by confidence.
/// 3. **Never** return `Ok(ContactId)` when multiple candidates have confidence > 0.7.
///    If the top candidate has confidence ≥ 0.95 and the second is < 0.5, auto-resolve.
/// 4. Always return `Err(Incomplete)` if the resolved contact lacks the required identifier
///    for the target app (e.g., no phone for WhatsApp).
#[async_trait::async_trait]
pub trait ContactResolver: Send + Sync {
    /// Attempt to resolve `name` to a unique contact for `app`.
    ///
    /// Returns:
    /// - `Ok(ContactId)` — unambiguous high-confidence match with required identifier.
    /// - `Err(Ambiguous{candidates})` — multiple plausible matches; must ask user.
    /// - `Err(NotFound)` — no candidates.
    /// - `Err(Incomplete)` — unique match but missing required identifier.
    /// - `Err(BackendError)` — storage failure.
    async fn resolve(
        &self,
        name: &str,
        app: &MessagingApp,
    ) -> Result<ContactId, ResolutionError>;
}

// ─── NullContactResolver ─────────────────────────────────────────────────────

/// A no-op resolver for use in tests and when no address book is configured.
/// Always returns `NotFound`.
pub struct NullContactResolver;

#[async_trait::async_trait]
impl ContactResolver for NullContactResolver {
    async fn resolve(
        &self,
        name: &str,
        _app: &MessagingApp,
    ) -> Result<ContactId, ResolutionError> {
        Err(ResolutionError::NotFound {
            query: name.to_string(),
        })
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ambiguous_error_carries_candidates() {
        let candidates = vec![
            Candidate {
                contact_id: ContactId {
                    display_name: "Anjali Sharma".to_string(),
                    identifier: "+919876543210".to_string(),
                    app: MessagingApp::WhatsApp,
                },
                confidence: 0.9,
                match_reason: "exact first name".to_string(),
            },
            Candidate {
                contact_id: ContactId {
                    display_name: "Anjali Verma".to_string(),
                    identifier: "+919123456789".to_string(),
                    app: MessagingApp::WhatsApp,
                },
                confidence: 0.85,
                match_reason: "exact first name".to_string(),
            },
        ];
        let err = ResolutionError::ambiguous("Anjali", candidates);
        match &err {
            ResolutionError::Ambiguous { candidate_count, candidates, .. } => {
                assert_eq!(*candidate_count, 2);
                assert_eq!(candidates.len(), 2);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn ambiguous_error_tool_payload_has_candidates() {
        let candidates = vec![
            Candidate {
                contact_id: ContactId {
                    display_name: "Anjali Sharma".to_string(),
                    identifier: "+919876543210".to_string(),
                    app: MessagingApp::WhatsApp,
                },
                confidence: 0.9,
                match_reason: "exact first name".to_string(),
            },
            Candidate {
                contact_id: ContactId {
                    display_name: "Anjali Verma".to_string(),
                    identifier: "+919123456789".to_string(),
                    app: MessagingApp::WhatsApp,
                },
                confidence: 0.85,
                match_reason: "exact first name".to_string(),
            },
        ];
        let err = ResolutionError::ambiguous("Anjali", candidates);
        let payload = err.to_tool_error_payload();
        assert_eq!(payload["error"], "AMBIGUOUS_CONTACT");
        assert!(payload["candidates"].is_array());
        assert_eq!(payload["candidates"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn incomplete_error_names_missing_field() {
        let err = ResolutionError::Incomplete {
            name: "Anjali Sharma".to_string(),
            field: "phone number".to_string(),
            app: MessagingApp::WhatsApp,
        };
        let payload = err.to_tool_error_payload();
        assert_eq!(payload["error"], "CONTACT_INCOMPLETE");
        assert_eq!(payload["missing_field"], "phone number");
    }

    #[tokio::test]
    async fn null_resolver_returns_not_found() {
        let r = NullContactResolver;
        let result = r.resolve("Anjali", &MessagingApp::WhatsApp).await;
        assert!(matches!(result, Err(ResolutionError::NotFound { .. })));
    }
}
