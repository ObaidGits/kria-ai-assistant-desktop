//! Voice Pipeline v2 — sub-500 ms TTFA, in-process streaming, barge-in.
//!
//! This module is the architectural successor to the v1 voice stack. v1 lives
//! alongside under [`super`] (e.g. [`super::stt::SpeechToText`]) and remains
//! the default until v2 is validated on every tier and platform.
//!
//! ## Layout
//!
//! ```text
//! voice::v2::
//!     stt          — `trait Stt` + WhisperRsStt + SidecarStt fallback
//!     tts          — `trait Tts` + PiperRsTts + PiperCliTts fallback
//!     sentence     — sentence splitter for streaming LLM tokens
//!     playback     — PlaybackSink with hard-abort barge-in support
//!     wake         — openWakeWord-based wake-word detector (Phase 4)
//!     aec          — WebRTC APM wrapper (Phase 3, opt-in feature)
//!     post_edit    — Hinglish fix-pass over final transcripts (Phase 5)
//!     pipeline     — `VoicePipelineV2` that wires everything together
//! ```
//!
//! ## Cargo features (all default-on except heavy native deps)
//!
//! - `voice-whisper-rs` — whisper.cpp Rust bindings (CPU). Default OFF.
//! - `voice-whisper-cuda` — adds CUDA backend. Default OFF.
//! - `voice-whisper-vulkan` — adds Vulkan backend. Default OFF.
//! - `voice-piper-rs` — sonata-synth in-process TTS via existing `ort`.
//!   Default OFF (uses CLI fallback).
//! - `voice-aec` — WebRTC APM acoustic echo cancellation. Default OFF
//!   (adds clang+cmake build deps).
//! - `voice-wake-oww` — openWakeWord ONNX detector via existing `ort`.
//!   Default OFF (model files must be downloaded separately).
//!
//! With **no features enabled** the v2 module still compiles and exposes the
//! sidecar / CLI fallback engines — the architecture, traits, sentence
//! splitter, playback sink, and post-edit are all pure-Rust and present.

pub mod aec;
pub mod pipeline;
pub mod playback;
pub mod post_edit;
pub mod sentence;
pub mod stt;
pub mod tts;
pub mod wake;

pub use aec::{AecProcessor, AecSettings};
pub use pipeline::{ActivePipeline, VoicePipelineV2, VoiceSessionState, VoiceTelemetry};
pub use playback::{PlaybackSink, PlaybackState};
pub use post_edit::{HinglishPostEditor, PostEditDecision};
pub use sentence::SentenceSplitter;
pub use stt::{PartialTranscript, FinalTranscript, Stt, StreamHandle};
pub use tts::{Tts, TtsSampleRate};
pub use wake::{WakeWordDetector, WakeWordEvent};

/// Construct a [`VoicePipelineV2`] using the v1 CLI engines (`whisper-cpp`
/// + `piper`) wrapped in the v2 streaming traits. This is the path used
/// when `voice.engine = "v2"` is set but native backends are not compiled
/// in — it gives the user the v2 concurrency model (streaming sentence
/// playback, hard barge-in, persistent in-process orchestration) without
/// requiring clang/cmake on their build path.
///
/// Returns the pipeline plus its state-watch + telemetry receivers so the
/// caller can plumb them into the UI event bus.
pub fn build_v2_with_cli_engines(
    voice_cfg: &crate::config::VoiceConfig,
    hw_tier: crate::platform::detect::HardwareTier,
    stt: std::sync::Arc<crate::voice::stt::SpeechToText>,
    tts: std::sync::Arc<crate::voice::tts::TextToSpeech>,
    wake: Option<WakeWordDetector>,
) -> (
    std::sync::Arc<VoicePipelineV2>,
    tokio::sync::watch::Receiver<VoiceSessionState>,
    tokio::sync::mpsc::UnboundedReceiver<VoiceTelemetry>,
) {
    use std::sync::Arc;
    let profile = crate::voice::tier::VoiceTierProfile::build(voice_cfg, hw_tier);
    let stt_v2: Arc<dyn Stt> = Arc::new(stt::CliWhisperStt::new(stt));
    let tts_v2: Arc<dyn Tts> = Arc::new(tts::CliPiperTts::new(tts, 22_050));
    let playback = PlaybackSink::new(22_050);
    let aec = AecProcessor::passthrough(AecSettings::default());
    let post_editor = HinglishPostEditor::from_config(
        &voice_cfg.post_edit,
        profile.post_edit_timeout_ms,
    );
    let (pipeline, state_rx, telemetry_rx) = VoicePipelineV2::new(
        profile, stt_v2, tts_v2, playback, wake, aec, post_editor,
    );
    (Arc::new(pipeline), state_rx, telemetry_rx)
}

/// Snapshot of which v2 native backends were compiled into this build.
/// Fed into the `voice_v2_status` Tauri command so the UI can show the
/// user *why* `engine = "v2"` is or isn't fully active.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct CompiledFeatures {
    pub voice_whisper_rs: bool,
    pub voice_whisper_cuda: bool,
    pub voice_whisper_vulkan: bool,
    pub voice_piper_rs: bool,
    pub voice_aec: bool,
    pub voice_wake_oww: bool,
}

impl CompiledFeatures {
    pub fn current() -> Self {
        Self {
            voice_whisper_rs: cfg!(feature = "voice-whisper-rs"),
            voice_whisper_cuda: cfg!(feature = "voice-whisper-cuda"),
            voice_whisper_vulkan: cfg!(feature = "voice-whisper-vulkan"),
            voice_piper_rs: cfg!(feature = "voice-piper-rs"),
            voice_aec: cfg!(feature = "voice-aec"),
            voice_wake_oww: cfg!(feature = "voice-wake-oww"),
        }
    }

    /// `true` when at least one v2 native backend is compiled in.
    pub fn any_native(self) -> bool {
        self.voice_whisper_rs || self.voice_piper_rs || self.voice_aec || self.voice_wake_oww
    }
}
