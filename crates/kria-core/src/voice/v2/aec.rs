//! WebRTC APM (Acoustic Echo Cancellation + NS + AGC + HPF) — Phase 3.
//!
//! Strictly behind the `voice-aec` cargo feature. Enabling the feature adds
//! the `webrtc-audio-processing` crate (vendored C, BSD-3) and pulls in
//! clang+cmake at build time. The default `cargo build` stays pure-Rust.
//!
//! When the feature is OFF, [`AecProcessor::passthrough`] returns the input
//! frame unchanged so the rest of the pipeline doesn't need to branch.

use serde::{Deserialize, Serialize};

use super::super::capture::AudioChunk;

/// User-facing settings mapped from `[voice.aec]` in the config.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AecSettings {
    pub enabled: bool,
    /// `"low" | "medium" | "high"`.
    pub aggressiveness: String,
}

impl Default for AecSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            aggressiveness: "medium".into(),
        }
    }
}

/// Acoustic Echo Cancellation processor. With `voice-aec` compiled, wraps a
/// `webrtc_audio_processing::Processor`. Without the feature, every method
/// is a passthrough.
pub struct AecProcessor {
    settings: AecSettings,
    backend: AecBackend,
}

enum AecBackend {
    Disabled,
    #[cfg(feature = "voice-aec")]
    WebRtcApm(WebRtcApmState),
}

#[cfg(feature = "voice-aec")]
struct WebRtcApmState {
    /// Reserved for the real `webrtc_audio_processing::Processor` instance
    /// once the dep is wired into kria-core/Cargo.toml.
    _placeholder: (),
}

impl AecProcessor {
    /// Construct an AEC processor according to `settings`. When AEC is
    /// disabled (either by user config or by the `voice-aec` feature being
    /// off), returns a passthrough.
    pub fn new(settings: AecSettings) -> Self {
        if !settings.enabled {
            return Self::passthrough(settings);
        }
        #[cfg(feature = "voice-aec")]
        {
            return Self {
                settings,
                backend: AecBackend::WebRtcApm(WebRtcApmState { _placeholder: () }),
            };
        }
        #[cfg(not(feature = "voice-aec"))]
        {
            tracing::warn!(
                "voice.aec.enabled = true but voice-aec feature not compiled; falling back to passthrough"
            );
            Self::passthrough(settings)
        }
    }

    /// Build a passthrough processor. Useful for the disabled / feature-off
    /// case and for tests.
    pub fn passthrough(settings: AecSettings) -> Self {
        Self {
            settings,
            backend: AecBackend::Disabled,
        }
    }

    pub fn is_active(&self) -> bool {
        !matches!(self.backend, AecBackend::Disabled)
    }

    pub fn settings(&self) -> &AecSettings {
        &self.settings
    }

    /// Run the captured frame through AEC + NS + AGC + HPF. With the feature
    /// off, returns the frame unchanged (clone-free, just consumes + returns).
    pub fn process_capture(&mut self, chunk: AudioChunk) -> AudioChunk {
        match &mut self.backend {
            AecBackend::Disabled => chunk,
            #[cfg(feature = "voice-aec")]
            AecBackend::WebRtcApm(_state) => {
                // TODO(voice-aec): re-frame to 10 ms @ 16 kHz, call
                // process_capture_frame on the APM instance, return the
                // processed frame.
                chunk
            }
        }
    }

    /// Push a render (TTS reference) frame into the AEC. No-op when
    /// disabled.
    pub fn push_render(&mut self, _samples: &[f32], _sample_rate: u32) {
        match &mut self.backend {
            AecBackend::Disabled => {}
            #[cfg(feature = "voice-aec")]
            AecBackend::WebRtcApm(_state) => {
                // TODO(voice-aec): forward into apm.process_render_frame.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_when_settings_disabled() {
        let p = AecProcessor::new(AecSettings::default());
        assert!(!p.is_active());
    }

    #[test]
    fn passthrough_returns_input_unchanged() {
        let mut p = AecProcessor::passthrough(AecSettings::default());
        let chunk = AudioChunk {
            samples: vec![0.1, -0.1, 0.2],
            sample_rate: 16_000,
            channels: 1,
        };
        let out = p.process_capture(chunk.clone());
        assert_eq!(out.samples, chunk.samples);
        assert_eq!(out.sample_rate, chunk.sample_rate);
    }
}
