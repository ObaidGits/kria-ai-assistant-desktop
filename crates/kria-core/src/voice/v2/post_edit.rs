//! Hinglish ASR post-edit / fixer (Phase 5).
//!
//! Whisper.cpp does a decent job on Hindi but its English-spelling output for
//! Hinglish is noisy. We run a tiny, local LLM (default
//! `Qwen2.5-3B-Instruct-Q4_K_M.gguf`) for ≤ `timeout_ms` to clean obvious
//! spelling/spacing errors and normalise Devanagari to Hinglish-Latin
//! transliteration when appropriate.
//!
//! Triggering policy is the responsibility of [`HinglishPostEditor::decide`]:
//! we only pay the latency hit when whisper confidence is low **or** the
//! transcript looks Hinglish (Devanagari char or one of the marker words).
//!
//! This module sits behind an explicit timeout — if the LLM doesn't answer in
//! time, we return the original transcript and continue. The TTFA budget is
//! never sacrificed for fixer accuracy.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::time::timeout;

use crate::config::PostEditConfig;
use crate::llm::{ChatMessage, LlmBackend};

use super::stt::FinalTranscript;

/// Decision returned by [`HinglishPostEditor::decide`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PostEditDecision {
    /// Use the raw whisper transcript as-is.
    Skip,
    /// Run the fixer with the configured timeout.
    Run,
}

/// Words/morphemes that strongly suggest the user is speaking Hinglish.
/// Lowercase ASCII so we can do a cheap `to_ascii_lowercase().contains()`.
const HINGLISH_MARKERS: &[&str] = &[
    "kya", "hai", "karo", "mujhe", "bhai", "nahi", "nahin", "haan", "thoda", "kar", "raha", "rahi",
    "kyun", "kyon", "achha", "accha", "theek", "matlab", "yaar", "abhi", "kaise", "kyaa", "bata",
    "batao", "samajh", "samjha", "chahiye", "wala", "wali", "hua", "hui", "tha", "thi", "the",
];

/// Confidence under which we always run the fixer regardless of language
/// markers.
const LOW_CONFIDENCE_THRESHOLD: f32 = 0.55;

pub struct HinglishPostEditor {
    pub model_name: String,
    pub timeout: Duration,
    pub always_run: bool,
}

impl HinglishPostEditor {
    pub fn from_config(cfg: &PostEditConfig, tier_default_timeout_ms: u64) -> Self {
        let mode = cfg.mode.to_ascii_lowercase();
        let always_run = matches!(mode.as_str(), "always");
        let timeout_ms = if cfg.timeout_ms == 0 {
            tier_default_timeout_ms
        } else {
            cfg.timeout_ms
        };
        Self {
            model_name: cfg.model.clone(),
            timeout: Duration::from_millis(timeout_ms),
            always_run: always_run && cfg.enabled,
        }
    }

    pub fn enabled(cfg: &PostEditConfig) -> bool {
        cfg.enabled
    }

    /// Decide whether to run the fixer for this transcript.
    pub fn decide(&self, t: &FinalTranscript) -> PostEditDecision {
        if self.always_run {
            return PostEditDecision::Run;
        }

        // Devanagari → almost certainly Hinglish.
        if t.text.chars().any(is_devanagari) {
            return PostEditDecision::Run;
        }

        // Low confidence → run.
        if t.confidence > 0.0 && t.confidence < LOW_CONFIDENCE_THRESHOLD {
            return PostEditDecision::Run;
        }

        // Marker word → run.
        let lower = t.text.to_ascii_lowercase();
        if HINGLISH_MARKERS.iter().any(|m| {
            // Word-boundary-ish: leading/trailing non-letter or string edge.
            let needle = *m;
            let mut start = 0;
            while let Some(pos) = lower[start..].find(needle) {
                let abs = start + pos;
                let before_ok = abs == 0
                    || !lower
                        .as_bytes()
                        .get(abs - 1)
                        .copied()
                        .unwrap_or(b' ')
                        .is_ascii_alphabetic();
                let after_idx = abs + needle.len();
                let after_ok = after_idx >= lower.len()
                    || !lower
                        .as_bytes()
                        .get(after_idx)
                        .copied()
                        .unwrap_or(b' ')
                        .is_ascii_alphabetic();
                if before_ok && after_ok {
                    return true;
                }
                start = abs + needle.len();
            }
            false
        }) {
            return PostEditDecision::Run;
        }

        PostEditDecision::Skip
    }

    /// Run the fixer. Returns the corrected text on success, or the
    /// original `raw` on timeout / error.
    pub async fn correct(&self, raw: &str, llm: Arc<dyn LlmBackend>) -> String {
        let messages = vec![
            ChatMessage {
                role: "system".into(),
                content: SYSTEM_PROMPT.to_string(),
                name: None,
                images: None,
            },
            ChatMessage {
                role: "user".into(),
                content: raw.to_string(),
                name: None,
                images: None,
            },
        ];
        let fut = llm.chat(&messages, None, 0.2, 256);
        match timeout(self.timeout, fut).await {
            Ok(Ok(resp)) => {
                let cleaned = resp.content.trim();
                if cleaned.is_empty() {
                    raw.to_string()
                } else {
                    cleaned.to_string()
                }
            }
            Ok(Err(e)) => {
                tracing::warn!("post-edit LLM error: {e}; using raw transcript");
                raw.to_string()
            }
            Err(_) => {
                let elapsed_ms = self.timeout.as_millis() as u64;
                tracing::warn!(timeout_ms = elapsed_ms, "post-edit timed out; using raw transcript");
                raw.to_string()
            }
        }
    }
}

fn is_devanagari(c: char) -> bool {
    matches!(c as u32, 0x0900..=0x097F)
}

const SYSTEM_PROMPT: &str = "You are a transcription cleanup assistant for a Hinglish (mix of Hindi and English) voice assistant. \
Fix obvious spelling, spacing, and punctuation errors in the user's transcript. \
Preserve the speaker's intent and casual register. \
Convert any Devanagari Hindi words to natural Hinglish-Latin spelling (e.g. क्या → kya, है → hai). \
Do NOT add new content, do NOT translate to formal English, do NOT explain. \
Return ONLY the corrected transcript on a single line, with no quotes or commentary.";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PostEditConfig;

    fn ed(cfg_mode: &str) -> HinglishPostEditor {
        let cfg = PostEditConfig {
            enabled: true,
            model: "qwen".into(),
            mode: cfg_mode.into(),
            timeout_ms: 200,
        };
        HinglishPostEditor::from_config(&cfg, 200)
    }

    fn t(text: &str, conf: f32) -> FinalTranscript {
        FinalTranscript {
            text: text.into(),
            language: "en".into(),
            confidence: conf,
            duration_ms: 1000,
            engine: "test".into(),
        }
    }

    #[test]
    fn always_mode_runs_unconditionally() {
        let e = ed("always");
        assert_eq!(e.decide(&t("hello world", 0.99)), PostEditDecision::Run);
    }

    #[test]
    fn devanagari_triggers_run() {
        let e = ed("on_low_confidence");
        assert_eq!(e.decide(&t("क्या हाल है", 0.95)), PostEditDecision::Run);
    }

    #[test]
    fn low_confidence_triggers_run() {
        let e = ed("on_low_confidence");
        assert_eq!(e.decide(&t("hello world", 0.30)), PostEditDecision::Run);
    }

    #[test]
    fn hinglish_marker_triggers_run() {
        let e = ed("on_low_confidence");
        assert_eq!(e.decide(&t("kya haal hai bhai", 0.95)), PostEditDecision::Run);
    }

    #[test]
    fn plain_english_skips() {
        let e = ed("on_low_confidence");
        assert_eq!(e.decide(&t("schedule a meeting tomorrow", 0.92)), PostEditDecision::Skip);
    }

    #[test]
    fn marker_inside_word_does_not_trigger() {
        // "haircare" contains "hai" but not as a word.
        let e = ed("on_low_confidence");
        assert_eq!(e.decide(&t("haircare products", 0.95)), PostEditDecision::Skip);
    }
}
