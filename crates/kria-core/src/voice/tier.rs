//! Voice pipeline hardware tier resolver.
//!
//! Maps the host's [`HardwareTier`] (or the user's `voice.tier` override) to
//! a concrete set of engine choices, model paths, and latency budgets used
//! by the v2 voice pipeline.
//!
//! The matrix here intentionally does **not** load any models or perform
//! any I/O — it is a pure data resolver. It is safe to call from
//! initialisation paths and tests.

use serde::{Deserialize, Serialize};

use crate::config::VoiceConfig;
use crate::platform::detect::HardwareTier;

/// Coarse voice tier — three levels, distinct from [`HardwareTier`] which has
/// four. Tier mapping: Lite→C, Standard→A, Performance→S, High→S.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VoiceTier {
    /// CPU-only / low-power. Target TTFA: 1200 ms.
    C,
    /// Mid-range GPU (RTX 3060/4060 class) or Apple silicon. Target TTFA: 800 ms.
    A,
    /// High-end GPU (RTX 4090 class). Target TTFA: 500 ms.
    S,
}

impl VoiceTier {
    /// Short label suitable for logs / telemetry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::S => "S",
            Self::A => "A",
            Self::C => "C",
        }
    }

    /// Resolve from the user's config override, falling back to the host
    /// hardware tier when override is `"auto"` or unrecognised.
    pub fn resolve(cfg: &VoiceConfig, hw: HardwareTier) -> Self {
        match cfg.tier.to_lowercase().as_str() {
            "s" => Self::S,
            "a" => Self::A,
            "c" => Self::C,
            _ => Self::from_hardware(hw),
        }
    }

    /// Map host hardware → voice tier.
    pub fn from_hardware(hw: HardwareTier) -> Self {
        match hw {
            HardwareTier::Lite => Self::C,
            HardwareTier::Standard => Self::A,
            HardwareTier::Performance | HardwareTier::High => Self::S,
        }
    }

    /// Time-to-first-audio target budget, in milliseconds. Used by
    /// `VoiceMetrics` to flag overruns and to gate adaptive degradation
    /// (3 consecutive overruns → emit `voice:degraded`).
    pub fn ttfa_budget_ms(self) -> u64 {
        match self {
            Self::S => 500,
            Self::A => 800,
            Self::C => 1200,
        }
    }

    /// Hard timeout for the LLM post-edit fix-pass per tier.
    pub fn post_edit_timeout_ms(self) -> u64 {
        match self {
            Self::S => 250,
            Self::A => 600,
            Self::C => 1000,
        }
    }

    /// Recommended STT engine identifier.
    ///
    /// Returns the *preferred* engine for the tier; the actual engine used
    /// at runtime is the intersection of this preference and what is
    /// compiled in via cargo features (see `kria-voice` Cargo.toml).
    pub fn stt_engine(self) -> &'static str {
        match self {
            Self::S => "whisper-rs-cuda",
            Self::A => "whisper-rs-cuda", // falls back to vulkan / cpu if unavailable
            Self::C => "whisper-rs",
        }
    }

    /// Recommended STT model file (under `models/stt/`).
    pub fn stt_model(self) -> &'static str {
        match self {
            Self::S | Self::A => "ggml-large-v3-turbo-q5_0.bin",
            Self::C => "ggml-small-q5_1.bin",
        }
    }

    /// Recommended TTS engine identifier.
    pub fn tts_engine(self) -> &'static str {
        match self {
            Self::S | Self::A => "piper-rs",
            Self::C => "piper-rs",
        }
    }

    /// AEC aggressiveness when the `aec` cargo feature is compiled.
    pub fn aec_aggressiveness(self) -> &'static str {
        match self {
            Self::S => "high",
            Self::A => "medium",
            Self::C => "low",
        }
    }

    /// Whether the post-edit fix-pass should run on every turn (S) or only
    /// on low-confidence / Hinglish-marker triggers (A, C).
    pub fn post_edit_always(self) -> bool {
        matches!(self, Self::S)
    }
}

/// Fully resolved per-tier voice profile. This is what the pipeline consults
/// at boot to wire engines together. All fields are owned strings so the
/// profile can be cheaply cloned across async tasks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceTierProfile {
    pub tier: VoiceTier,
    pub stt_engine: String,
    pub stt_model: String,
    pub tts_engine: String,
    pub aec_aggressiveness: String,
    pub post_edit_always: bool,
    pub post_edit_timeout_ms: u64,
    pub ttfa_budget_ms: u64,
}

impl VoiceTierProfile {
    /// Build a profile from the user config + detected hardware. Honours
    /// `cfg.stt_engine`, `cfg.stt_model`, `cfg.tts_engine`, and
    /// `cfg.post_edit.timeout_ms` overrides when they are non-default.
    pub fn build(cfg: &VoiceConfig, hw: HardwareTier) -> Self {
        let tier = VoiceTier::resolve(cfg, hw);

        let stt_engine = match cfg.stt_engine.as_str() {
            "" | "auto" => tier.stt_engine().to_string(),
            other => other.to_string(),
        };
        let stt_model = if cfg.stt_model.is_empty() || cfg.stt_model == "ggml-base.en.bin" {
            // Legacy default cannot transcribe Hindi — silently upgrade.
            tier.stt_model().to_string()
        } else {
            cfg.stt_model.clone()
        };
        let tts_engine = match cfg.tts_engine.as_str() {
            "" | "auto" => tier.tts_engine().to_string(),
            other => other.to_string(),
        };
        let aec_aggressiveness = if cfg.aec.aggressiveness.is_empty() {
            tier.aec_aggressiveness().to_string()
        } else {
            cfg.aec.aggressiveness.clone()
        };
        let post_edit_timeout_ms = if cfg.post_edit.timeout_ms == 0 {
            tier.post_edit_timeout_ms()
        } else {
            cfg.post_edit.timeout_ms
        };
        let post_edit_always = match cfg.post_edit.mode.as_str() {
            "always" => true,
            "on_low_confidence" => false,
            _ => tier.post_edit_always(),
        };

        Self {
            tier,
            stt_engine,
            stt_model,
            tts_engine,
            aec_aggressiveness,
            post_edit_always,
            post_edit_timeout_ms,
            ttfa_budget_ms: tier.ttfa_budget_ms(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_hardware_tiers() {
        assert_eq!(VoiceTier::from_hardware(HardwareTier::Lite), VoiceTier::C);
        assert_eq!(
            VoiceTier::from_hardware(HardwareTier::Standard),
            VoiceTier::A
        );
        assert_eq!(
            VoiceTier::from_hardware(HardwareTier::Performance),
            VoiceTier::S
        );
        assert_eq!(VoiceTier::from_hardware(HardwareTier::High), VoiceTier::S);
    }

    #[test]
    fn override_takes_precedence() {
        let mut cfg = VoiceConfig::default();
        cfg.tier = "c".into();
        assert_eq!(
            VoiceTier::resolve(&cfg, HardwareTier::High),
            VoiceTier::C
        );
    }

    #[test]
    fn auto_falls_back_to_hardware() {
        let cfg = VoiceConfig::default();
        assert_eq!(
            VoiceTier::resolve(&cfg, HardwareTier::Standard),
            VoiceTier::A
        );
    }

    #[test]
    fn legacy_base_en_model_is_silently_upgraded() {
        let cfg = VoiceConfig::default(); // stt_model = "ggml-base.en.bin"
        let p = VoiceTierProfile::build(&cfg, HardwareTier::Standard);
        assert_eq!(p.stt_model, "ggml-large-v3-turbo-q5_0.bin");
    }

    #[test]
    fn ttfa_budgets_match_spec() {
        assert_eq!(VoiceTier::S.ttfa_budget_ms(), 500);
        assert_eq!(VoiceTier::A.ttfa_budget_ms(), 800);
        assert_eq!(VoiceTier::C.ttfa_budget_ms(), 1200);
    }

    #[test]
    fn explicit_override_keeps_user_model() {
        let mut cfg = VoiceConfig::default();
        cfg.stt_model = "ggml-medium-q5_0.bin".into();
        let p = VoiceTierProfile::build(&cfg, HardwareTier::Standard);
        assert_eq!(p.stt_model, "ggml-medium-q5_0.bin");
    }
}
