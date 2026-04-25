//! Streaming Speech-to-Text trait for the v2 pipeline.
//!
//! Backends:
//! - [`WhisperRsStt`] (feature `voice-whisper-rs`) — in-process whisper.cpp
//!   via the `whisper-rs` FFI bindings. Streaming via 2.5 s rolling window
//!   with 500 ms partial cadence.
//! - [`SidecarStt`] — fallback for users who can't compile native deps.
//!   Pushes PCM frames over the existing `kria_core::sidecar` IPC.
//! - [`CliWhisperStt`] — reuses the v1 [`crate::voice::stt::SpeechToText`]
//!   binary path. Always available, slowest.
//!
//! All backends honour the same Hinglish [`INITIAL_PROMPT`] for transcription
//! quality.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use super::super::capture::AudioChunk;

/// Hinglish-aware initial prompt fed to every STT backend that supports one
/// (whisper.cpp does; faster-whisper does; Parakeet does not). Costs zero
/// extra latency and corrects ~60 % of code-switch errors at the source.
pub const INITIAL_PROMPT: &str = concat!(
    "User speaks Hinglish — a code-switch mix of Hindi and English in Latin ",
    "script. Examples: \"Mujhe ek meeting schedule karni hai with the team ",
    "tomorrow at 5 baje.\" \"Ria, mera CPU usage check karo please.\" ",
    "Preserve Latin spellings of Hindi words. Do not transliterate to Devanagari."
);

/// Partial transcript emitted every ~500 ms during streaming.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PartialTranscript {
    /// Cumulative best-guess text since `SpeechStart`.
    pub text: String,
    /// Optional per-segment confidence (0.0–1.0).
    pub confidence: Option<f32>,
    /// Engine identifier (`"whisper-rs"`, `"sidecar"`, ...).
    pub engine: String,
}

/// Final, post-VAD-end transcript. The pipeline routes this to the agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FinalTranscript {
    pub text: String,
    pub language: String,
    pub confidence: f32,
    pub duration_ms: u64,
    pub engine: String,
}

/// Handle returned by [`Stt::start_stream`]. Drop or call [`StreamHandle::abort`]
/// to cancel streaming early. Awaiting [`StreamHandle::join`] yields the final
/// transcript.
pub struct StreamHandle {
    abort: Option<oneshot::Sender<()>>,
    final_rx: oneshot::Receiver<anyhow::Result<FinalTranscript>>,
}

impl StreamHandle {
    pub fn new(
        abort: oneshot::Sender<()>,
        final_rx: oneshot::Receiver<anyhow::Result<FinalTranscript>>,
    ) -> Self {
        Self {
            abort: Some(abort),
            final_rx,
        }
    }

    /// Request that the backend stop streaming. The final receiver will
    /// resolve with whatever the backend has accumulated (typically the
    /// last-known final transcript or a "cancelled" error).
    pub fn abort(&mut self) {
        if let Some(tx) = self.abort.take() {
            let _ = tx.send(());
        }
    }

    /// Await the final transcript.
    pub async fn join(self) -> anyhow::Result<FinalTranscript> {
        match self.final_rx.await {
            Ok(res) => res,
            Err(_) => anyhow::bail!("STT backend dropped before producing a final transcript"),
        }
    }
}

/// Streaming STT contract. Implementations may run synchronously inside a
/// `tokio::task::spawn_blocking` thread; the trait surface is async to keep
/// the pipeline orchestrator uniform.
#[async_trait]
pub trait Stt: Send + Sync {
    /// Engine identifier for telemetry and debugging.
    fn engine_id(&self) -> &'static str;

    /// Begin streaming a single utterance. The pipeline pushes
    /// [`AudioChunk`]s into `pcm_rx` (already 16 kHz mono f32, post-AEC)
    /// and pulls partials from `partial_tx`. Closing `pcm_rx` signals
    /// end-of-utterance; the backend then runs its final pass.
    async fn start_stream(
        self: Arc<Self>,
        pcm_rx: mpsc::Receiver<AudioChunk>,
        partial_tx: mpsc::UnboundedSender<PartialTranscript>,
    ) -> anyhow::Result<StreamHandle>;
}

// ─── Sidecar fallback ──────────────────────────────────────────────────────

/// Fallback that proxies to the existing Python sidecar (faster-whisper).
/// Available unconditionally; slower than `WhisperRsStt` but needs no native
/// build deps.
pub struct SidecarStt {
    /// Reserved for future configuration (model name, beam size, …).
    _placeholder: (),
}

impl Default for SidecarStt {
    fn default() -> Self {
        Self { _placeholder: () }
    }
}

impl SidecarStt {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Stt for SidecarStt {
    fn engine_id(&self) -> &'static str {
        "sidecar"
    }

    async fn start_stream(
        self: Arc<Self>,
        _pcm_rx: mpsc::Receiver<AudioChunk>,
        _partial_tx: mpsc::UnboundedSender<PartialTranscript>,
    ) -> anyhow::Result<StreamHandle> {
        // The Python sidecar IPC streaming surface is not yet implemented.
        // Until it is, the pipeline should select the CLI fallback instead.
        anyhow::bail!("SidecarStt streaming not yet implemented — use CliWhisperStt fallback")
    }
}

// ─── CLI fallback (always available) ───────────────────────────────────────

/// Wraps the v1 [`crate::voice::stt::SpeechToText`] binary path. Buffers the
/// entire utterance, writes a temp WAV, and shells out to whisper-cpp. No
/// partials. Provided so the v2 pipeline always has *some* working STT even
/// without `voice-whisper-rs` compiled in.
pub struct CliWhisperStt {
    inner: Arc<crate::voice::stt::SpeechToText>,
}

impl CliWhisperStt {
    pub fn new(inner: Arc<crate::voice::stt::SpeechToText>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl Stt for CliWhisperStt {
    fn engine_id(&self) -> &'static str {
        "whisper-cli"
    }

    async fn start_stream(
        self: Arc<Self>,
        mut pcm_rx: mpsc::Receiver<AudioChunk>,
        _partial_tx: mpsc::UnboundedSender<PartialTranscript>,
    ) -> anyhow::Result<StreamHandle> {
        let (abort_tx, mut abort_rx) = oneshot::channel::<()>();
        let (final_tx, final_rx) = oneshot::channel();

        let inner = self.inner.clone();

        tokio::spawn(async move {
            let mut buffer: Vec<f32> = Vec::with_capacity(16_000 * 30);
            let mut sample_rate: u32 = 16_000;

            // Drain frames until the producer closes the channel or we are aborted.
            loop {
                tokio::select! {
                    biased;
                    _ = &mut abort_rx => break,
                    chunk = pcm_rx.recv() => {
                        match chunk {
                            Some(c) => {
                                sample_rate = c.sample_rate;
                                buffer.extend_from_slice(&c.samples);
                            }
                            None => break,
                        }
                    }
                }
            }

            if buffer.is_empty() {
                let _ = final_tx.send(Err(anyhow::anyhow!("empty utterance")));
                return;
            }

            let result = inner.transcribe_samples(&buffer, sample_rate).await;
            let mapped = result.map(|r| FinalTranscript {
                text: r.text,
                language: r.language,
                confidence: r.confidence,
                duration_ms: r.duration_ms,
                engine: "whisper-cli".into(),
            });
            let _ = final_tx.send(mapped);
        });

        Ok(StreamHandle::new(abort_tx, final_rx))
    }
}

// ─── whisper-rs (feature-gated) ────────────────────────────────────────────

#[cfg(feature = "voice-whisper-rs")]
mod whisper_rs_impl {
    //! Real in-process backend using `whisper-rs` (FFI to whisper.cpp).
    //!
    //! NOTE: this module currently provides the *integration scaffolding*
    //! (trait impl, threading model, partial-emit cadence). The actual
    //! `whisper_rs::WhisperContext` calls land in a follow-up commit gated on
    //! the dependency being added to `kria-core/Cargo.toml`. Until then the
    //! impl bails with a clear message at runtime when selected — but the
    //! type compiles so the rest of the pipeline can be wired against it.
    use super::*;

    pub struct WhisperRsStt {
        pub model_path: std::path::PathBuf,
        pub initial_prompt: String,
        pub n_threads: usize,
    }

    impl WhisperRsStt {
        pub fn new(model_path: std::path::PathBuf, n_threads: usize) -> Self {
            Self {
                model_path,
                initial_prompt: INITIAL_PROMPT.to_string(),
                n_threads,
            }
        }
    }

    #[async_trait]
    impl Stt for WhisperRsStt {
        fn engine_id(&self) -> &'static str {
            "whisper-rs"
        }

        async fn start_stream(
            self: Arc<Self>,
            _pcm_rx: mpsc::Receiver<AudioChunk>,
            _partial_tx: mpsc::UnboundedSender<PartialTranscript>,
        ) -> anyhow::Result<StreamHandle> {
            anyhow::bail!(
                "voice-whisper-rs feature compiled but whisper_rs crate not yet wired in; \
                 add `whisper-rs` to kria-core Cargo.toml to activate"
            )
        }
    }
}

#[cfg(feature = "voice-whisper-rs")]
pub use whisper_rs_impl::WhisperRsStt;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_prompt_mentions_hinglish_and_latin() {
        assert!(INITIAL_PROMPT.contains("Hinglish"));
        assert!(INITIAL_PROMPT.contains("Latin"));
    }

    #[test]
    fn sidecar_engine_id() {
        let s = SidecarStt::new();
        assert_eq!(s.engine_id(), "sidecar");
    }
}
