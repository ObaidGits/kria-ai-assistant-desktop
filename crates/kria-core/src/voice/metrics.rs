//! Per-turn voice pipeline telemetry.
//!
//! Records the five canonical timestamps of a voice turn so we can verify
//! the per-tier TTFA (time-to-first-audio-out) budget at runtime and in CI.
//! Without these metrics, the "sub-500ms" goal is unfalsifiable.
//!
//! Timeline (all monotonic, relative to `t_speech_end`):
//!
//! ```text
//! t_speech_end ─┬─► t_first_partial   ─► (partial transcripts shown in UI)
//!                │
//!                ├─► t_final          ─► (VAD end + final whisper pass done)
//!                │
//!                ├─► t_post_edit      ─► (Hinglish fix-pass returned, if run)
//!                │
//!                └─► t_first_audio_out ─► (first PCM chunk hits speakers)
//! ```
//!
//! `t_first_audio_out − t_speech_end` is the TTFA the user perceives.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::tier::VoiceTier;

/// Builder-style metrics collector. Construct one per voice turn at
/// `SpeechEnd`, mutate as later milestones land, then call
/// [`MetricsBuilder::finalise`] to obtain a [`VoiceMetrics`] snapshot.
#[derive(Debug, Clone)]
pub struct MetricsBuilder {
    started: Instant,
    tier: VoiceTier,
    first_partial: Option<Duration>,
    final_transcript: Option<Duration>,
    post_edit: Option<Duration>,
    first_audio_out: Option<Duration>,
    post_edit_skipped: bool,
}

impl MetricsBuilder {
    /// Begin recording at the moment VAD reports `SpeechEnd`.
    pub fn begin_at_speech_end(tier: VoiceTier) -> Self {
        Self {
            started: Instant::now(),
            tier,
            first_partial: None,
            final_transcript: None,
            post_edit: None,
            first_audio_out: None,
            post_edit_skipped: false,
        }
    }

    pub fn mark_first_partial(&mut self) {
        if self.first_partial.is_none() {
            self.first_partial = Some(self.started.elapsed());
        }
    }

    pub fn mark_final(&mut self) {
        self.final_transcript = Some(self.started.elapsed());
    }

    pub fn mark_post_edit(&mut self) {
        self.post_edit = Some(self.started.elapsed());
    }

    /// Indicate that no post-edit was run for this turn (high confidence,
    /// pure-English, etc.). Distinct from "post-edit timed out".
    pub fn skip_post_edit(&mut self) {
        self.post_edit_skipped = true;
    }

    pub fn mark_first_audio_out(&mut self) {
        if self.first_audio_out.is_none() {
            self.first_audio_out = Some(self.started.elapsed());
        }
    }

    pub fn finalise(self) -> VoiceMetrics {
        VoiceMetrics {
            tier: self.tier,
            ttfa_budget_ms: self.tier.ttfa_budget_ms(),
            t_first_partial_ms: self.first_partial.map(|d| d.as_millis() as u64),
            t_final_ms: self.final_transcript.map(|d| d.as_millis() as u64),
            t_post_edit_ms: if self.post_edit_skipped {
                None
            } else {
                self.post_edit.map(|d| d.as_millis() as u64)
            },
            t_first_audio_out_ms: self.first_audio_out.map(|d| d.as_millis() as u64),
            post_edit_skipped: self.post_edit_skipped,
        }
    }
}

/// Snapshot emitted as a `voice:metrics` Tauri event after each turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoiceMetrics {
    pub tier: VoiceTier,
    /// Per-tier TTFA budget that this turn was measured against.
    pub ttfa_budget_ms: u64,
    pub t_first_partial_ms: Option<u64>,
    pub t_final_ms: Option<u64>,
    /// `None` if `post_edit_skipped` is true OR if post-edit hasn't fired yet.
    pub t_post_edit_ms: Option<u64>,
    pub t_first_audio_out_ms: Option<u64>,
    pub post_edit_skipped: bool,
}

impl VoiceMetrics {
    /// `true` when the user-perceived latency exceeded the tier budget.
    pub fn ttfa_overrun(&self) -> bool {
        self.t_first_audio_out_ms
            .map(|t| t > self.ttfa_budget_ms)
            .unwrap_or(false)
    }
}

/// Rolling overrun counter. Three consecutive `ttfa_overrun()` turns trigger
/// a `voice:degraded` event so the UI can offer to demote the tier.
#[derive(Debug, Clone, Copy, Default)]
pub struct OverrunTracker {
    consecutive: u8,
}

impl OverrunTracker {
    /// Record a turn. Returns `true` exactly once when the threshold is
    /// crossed (so the caller can emit the degraded event without spamming).
    pub fn record(&mut self, m: &VoiceMetrics) -> bool {
        if m.ttfa_overrun() {
            self.consecutive = self.consecutive.saturating_add(1);
            if self.consecutive == 3 {
                return true;
            }
        } else {
            self.consecutive = 0;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn builder_records_in_order() {
        let mut b = MetricsBuilder::begin_at_speech_end(VoiceTier::A);
        sleep(Duration::from_millis(2));
        b.mark_first_partial();
        b.mark_final();
        b.skip_post_edit();
        b.mark_first_audio_out();
        let m = b.finalise();
        assert_eq!(m.tier, VoiceTier::A);
        assert_eq!(m.ttfa_budget_ms, 800);
        assert!(m.t_first_partial_ms.is_some());
        assert!(m.t_final_ms.is_some());
        assert!(m.t_post_edit_ms.is_none());
        assert!(m.post_edit_skipped);
    }

    #[test]
    fn overrun_tracker_fires_on_third_overrun_only() {
        let mut t = OverrunTracker::default();
        let bad = VoiceMetrics {
            tier: VoiceTier::S,
            ttfa_budget_ms: 500,
            t_first_partial_ms: Some(100),
            t_final_ms: Some(200),
            t_post_edit_ms: None,
            t_first_audio_out_ms: Some(900),
            post_edit_skipped: true,
        };
        assert!(!t.record(&bad));
        assert!(!t.record(&bad));
        assert!(t.record(&bad), "third consecutive overrun should fire");
        // does not refire on the fourth
        assert!(!t.record(&bad));
    }

    #[test]
    fn good_turn_resets_streak() {
        let mut t = OverrunTracker::default();
        let bad = VoiceMetrics {
            tier: VoiceTier::S,
            ttfa_budget_ms: 500,
            t_first_partial_ms: None,
            t_final_ms: None,
            t_post_edit_ms: None,
            t_first_audio_out_ms: Some(900),
            post_edit_skipped: true,
        };
        let good = VoiceMetrics {
            t_first_audio_out_ms: Some(300),
            ..bad.clone()
        };
        t.record(&bad);
        t.record(&bad);
        t.record(&good);
        assert!(!t.record(&bad), "streak should have reset");
    }
}
