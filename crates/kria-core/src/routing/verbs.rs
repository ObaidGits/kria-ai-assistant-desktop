//! Verb / modality classifier (parallel to embedding layer).
//!
//! Scans user text for action verbs to determine intent modality and whether
//! the action is destructive. This feeds the safety policy directly,
//! independently of embedding similarity.

use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};

/// What kind of action the user is requesting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IntentModality {
    Read,
    Write,
    Send,
    Delete,
    Execute,
    Schedule,
    Query,
    /// No clear modality detected.
    Unknown,
}

impl IntentModality {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Send => "send",
            Self::Delete => "delete",
            Self::Execute => "execute",
            Self::Schedule => "schedule",
            Self::Query => "query",
            Self::Unknown => "unknown",
        }
    }

    /// Whether this modality should pre-arm the safety policy for elevated risk.
    pub fn is_destructive(self) -> bool {
        matches!(self, Self::Delete | Self::Send | Self::Execute)
    }
}

/// Result of the verb classifier.
#[derive(Debug, Clone)]
pub struct ModalityResult {
    /// Primary modality.
    pub primary: IntentModality,
    /// All detected modalities (for multi-intent detection).
    pub all: Vec<IntentModality>,
    /// True if any modality is destructive.
    pub destructive: bool,
    /// Number of imperative verb tokens found (used for segmentation gate).
    pub imperative_verb_count: usize,
}

// ─── Verb lexicon ────────────────────────────────────────────────────────────

// Each tuple: (regex pattern, IntentModality)
// Patterns are checked in priority order; first match wins for primary.
static VERB_PATTERNS: Lazy<Vec<(Regex, IntentModality)>> = Lazy::new(|| {
    let entries: &[(&str, IntentModality)] = &[
        // Delete (highest risk — checked first)
        (
            r"(?i)\b(delete|remove|trash|erase|wipe|purge|uninstall|drop|clear all|hatao|mita\s*do|hata\s*do)\b",
            IntentModality::Delete,
        ),
        // Send / transmit
        (
            r"(?i)\b(send|forward|reply|submit|publish|post|upload|bhejo|forward\s*karo|reply\s*karo)\b",
            IntentModality::Send,
        ),
        // Execute / run
        (
            r"(?i)\b(run|execute|launch|start|boot|deploy|install|build|compile|chalao|install\s*karo)\b",
            IntentModality::Execute,
        ),
        // Write / create / modify
        (
            r"(?i)\b(write|create|edit|update|modify|rename|move|copy|save|draft|compose|banao|likhao|badlo)\b",
            IntentModality::Write,
        ),
        // Schedule
        (
            r"(?i)\b(schedule|remind|set\s+a?\s*(reminder|alarm)|book|calendar|plan|add\s+event|yaad\s*dilao|schedule\s*karo)\b",
            IntentModality::Schedule,
        ),
        // Read / open / show
        (
            r"(?i)\b(read|open|show|display|list|view|get|fetch|load|check|preview|padhao|dikhao)\b",
            IntentModality::Read,
        ),
        // Query / search / find
        (
            r"(?i)\b(search|find|look\s*(for|up)|query|what\s+is|what\s+are|who\s+is|how\s+does|dhundo|batao|kya\s+hai)\b",
            IntentModality::Query,
        ),
    ];
    entries
        .iter()
        .filter_map(|(pat, m)| Regex::new(pat).ok().map(|r| (r, *m)))
        .collect()
});

/// Classify the modality of a user prompt.
pub fn classify_modality(text: &str) -> ModalityResult {
    let mut found: Vec<IntentModality> = Vec::new();

    for (re, modality) in VERB_PATTERNS.iter() {
        if re.is_match(text) {
            if !found.contains(modality) {
                found.push(*modality);
            }
        }
    }

    let primary = found.first().copied().unwrap_or(IntentModality::Unknown);
    let destructive = found.iter().any(|m| m.is_destructive());
    let imperative_verb_count = found.len();

    ModalityResult {
        primary,
        all: found,
        destructive,
        imperative_verb_count,
    }
}
