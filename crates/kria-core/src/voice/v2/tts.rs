//! Streaming Text-to-Speech trait for the v2 pipeline.
//!
//! Backends synthesize a sentence at a time and emit `Vec<f32>` PCM chunks
//! into a bounded channel. The [`PlaybackSink`](super::playback::PlaybackSink)
//! drains the channel, decodes into rodio, and forks a copy into the AEC
//! reference path.

use std::sync::Arc;
#[cfg(feature = "voice-piper-rs")]
use std::path::PathBuf;

use async_trait::async_trait;
use tokio::sync::mpsc;

/// Output sample-rate of a TTS backend. Voiced models are 22.05 kHz; we let
/// playback resample if the output device disagrees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TtsSampleRate(pub u32);

impl Default for TtsSampleRate {
    fn default() -> Self {
        Self(22_050)
    }
}

/// Streaming TTS contract.
#[async_trait]
pub trait Tts: Send + Sync {
    /// Engine identifier.
    fn engine_id(&self) -> &'static str;

    /// Output sample rate.
    fn sample_rate(&self) -> TtsSampleRate;

    /// Synthesize one sentence and push PCM chunks (~120 ms each) into
    /// `pcm_tx`. Closes `pcm_tx`'s send side when synthesis completes.
    /// Implementations should poll `abort_rx` to bail early.
    async fn synthesize_sentence(
        self: Arc<Self>,
        sentence: String,
        pcm_tx: mpsc::Sender<Vec<f32>>,
        abort_rx: tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<()>;
}

// ─── CLI fallback (always available) ───────────────────────────────────────

/// Wraps the v1 [`crate::voice::tts::TextToSpeech`] CLI path. Synthesizes the
/// whole sentence then pushes one big PCM chunk. Provided so v2 always has
/// *some* working TTS even without `voice-piper-rs` compiled.
pub struct CliPiperTts {
    inner: Arc<crate::voice::tts::TextToSpeech>,
    sample_rate: u32,
}

impl CliPiperTts {
    pub fn new(inner: Arc<crate::voice::tts::TextToSpeech>, sample_rate: u32) -> Self {
        Self { inner, sample_rate }
    }
}

#[async_trait]
impl Tts for CliPiperTts {
    fn engine_id(&self) -> &'static str {
        "piper-cli"
    }

    fn sample_rate(&self) -> TtsSampleRate {
        TtsSampleRate(self.sample_rate)
    }

    async fn synthesize_sentence(
        self: Arc<Self>,
        sentence: String,
        pcm_tx: mpsc::Sender<Vec<f32>>,
        mut abort_rx: tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        // Honour an already-set abort before doing any work.
        if *abort_rx.borrow() {
            return Ok(());
        }
        let inner = self.inner.clone();
        let mut synth = tokio::spawn(async move { inner.synthesize_samples(&sentence).await });

        tokio::select! {
            biased;
            _ = abort_rx.changed() => {
                synth.abort();
                Ok(())
            }
            res = &mut synth => {
                let samples = res??;
                if !*abort_rx.borrow() {
                    let _ = pcm_tx.send(samples).await;
                }
                Ok(())
            }
        }
    }
}

// ─── piper-rs (feature-gated) ──────────────────────────────────────────────

#[cfg(feature = "voice-piper-rs")]
mod piper_rs_impl {
    //! Real in-process backend using `sonata-synth` (a.k.a. `piper-rs`) over
    //! the existing `ort` ONNX runtime.
    //!
    //! Same scaffolding-first pattern as `WhisperRsStt`: trait + types compile
    //! today; the actual `sonata::Synthesiser` calls are added when the
    //! `sonata-synth` dep is wired into `kria-core/Cargo.toml`.
    use super::*;

    pub struct PiperRsTts {
        pub model_path: PathBuf,
        pub config_path: PathBuf,
        pub sample_rate: u32,
    }

    impl PiperRsTts {
        pub fn new(model_path: PathBuf) -> Self {
            let config_path = model_path.with_extension("onnx.json");
            Self {
                model_path,
                config_path,
                sample_rate: 22_050,
            }
        }
    }

    #[async_trait]
    impl Tts for PiperRsTts {
        fn engine_id(&self) -> &'static str {
            "piper-rs"
        }

        fn sample_rate(&self) -> TtsSampleRate {
            TtsSampleRate(self.sample_rate)
        }

        async fn synthesize_sentence(
            self: Arc<Self>,
            _sentence: String,
            _pcm_tx: mpsc::Sender<Vec<f32>>,
            _abort_rx: tokio::sync::watch::Receiver<bool>,
        ) -> anyhow::Result<()> {
            anyhow::bail!(
                "voice-piper-rs feature compiled but sonata-synth crate not yet wired in; \
                 add `sonata-synth` to kria-core Cargo.toml to activate"
            )
        }
    }
}

#[cfg(feature = "voice-piper-rs")]
pub use piper_rs_impl::PiperRsTts;
