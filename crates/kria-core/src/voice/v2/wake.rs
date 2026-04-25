//! Wake-word detection — Phase 4 ("Hey Ria" via openWakeWord).
//!
//! When the `voice-wake-oww` cargo feature is enabled, this module runs the
//! 3-model [openWakeWord](https://github.com/dscripka/openWakeWord) stack
//! end-to-end on the microphone capture stream:
//!
//! ```text
//!   16 kHz mono audio
//!         │
//!         ▼
//!   melspectrogram.onnx        (audio → log-mel features, 32 bins)
//!         │
//!         ▼
//!   embedding_model.onnx        (76 mel frames → 96-dim embedding)
//!         │
//!         ▼
//!   hey_ria.onnx                (16 embeddings → keyword score 0..1)
//!         │
//!         ▼
//!   score ≥ sensitivity → emit WakeWordEvent("hey ria", score, "oww")
//! ```
//!
//! Without the feature (default builds) the detector compiles to a
//! [`WakeWordDetector::disabled`] no-op so the rest of the pipeline can hold
//! `Option<WakeWordDetector>` unconditionally.
//!
//! ### Buffering invariants
//!
//! - **Audio buffer**: VecDeque<f32>, fed one [`AudioChunk`] at a time.
//!   Drained 1280 samples (= 80 ms @ 16 kHz) at a time into the mel model.
//! - **Mel buffer**: VecDeque<[f32; 32]>, accumulates mel frames produced
//!   by the spectrogram model. Stride 8 frames per embedding step.
//! - **Embedding buffer**: VecDeque<[f32; 96]>, capped at 16. Once full,
//!   each new embedding causes one keyword-head inference.
//!
//! These constants match the public openWakeWord training pipeline. They
//! are exposed as `pub const` so the buffering logic is unit-testable
//! without loading any models.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};

use super::super::capture::AudioChunk;

// ─── openWakeWord protocol constants ───────────────────────────────────────

/// Mic sample rate the openWakeWord stack is trained on.
pub const OWW_SAMPLE_RATE: u32 = 16_000;
/// Audio samples consumed per mel-spectrogram step (80 ms @ 16 kHz).
pub const OWW_AUDIO_STEP: usize = 1280;
/// Mel bins produced per frame.
pub const OWW_MEL_BINS: usize = 32;
/// Mel frames consumed per embedding-model invocation.
pub const OWW_MEL_WINDOW: usize = 76;
/// Mel frames advanced between consecutive embedding invocations (stride).
pub const OWW_MEL_STRIDE: usize = 8;
/// Embedding dimension produced by the speech-embedding model.
pub const OWW_EMBED_DIM: usize = 96;
/// Embeddings consumed per wake-word-head invocation.
pub const OWW_EMBED_WINDOW: usize = 16;

// ─── Public API ────────────────────────────────────────────────────────────

/// Event emitted when a wake phrase is detected.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WakeWordEvent {
    pub phrase: String,
    /// Detector confidence (0.0–1.0).
    pub score: f32,
    /// Source: `"oww"` (openWakeWord), `"ptt"` (push-to-talk force-wake), …
    pub source: String,
}

/// Resolved on-disk paths to the 3-model openWakeWord stack.
#[derive(Debug, Clone)]
pub struct WakeWordModels {
    pub melspectrogram: PathBuf,
    pub embedding: PathBuf,
    pub keyword: PathBuf,
}

impl WakeWordModels {
    /// Construct from a single keyword-model path. The mel + embedding
    /// models are expected to live in the *same directory* under the
    /// canonical openWakeWord names.
    pub fn from_keyword_path(keyword: PathBuf) -> Self {
        let dir = keyword.parent().map(PathBuf::from).unwrap_or_default();
        Self {
            melspectrogram: dir.join("melspectrogram.onnx"),
            embedding: dir.join("embedding_model.onnx"),
            keyword,
        }
    }

    /// `true` when all three model files exist on disk.
    pub fn all_present(&self) -> bool {
        self.melspectrogram.exists() && self.embedding.exists() && self.keyword.exists()
    }
}

/// Async wake-word detector. Subscribes to mic frames via a
/// `broadcast::Receiver<AudioChunk>` and pushes [`WakeWordEvent`]s into the
/// supplied `mpsc::UnboundedSender` whenever the keyword score exceeds
/// `sensitivity`.
pub struct WakeWordDetector {
    pub models: WakeWordModels,
    pub sensitivity: f32,
    pub phrase: String,
    pub aliases: Vec<String>,
    backend: Backend,
}

enum Backend {
    /// Always-disabled stub. Used when the `voice-wake-oww` feature is off
    /// or the model files are missing.
    Disabled,
    #[cfg(feature = "voice-wake-oww")]
    OpenWakeWord(Box<oww::OwnBackend>),
}

impl WakeWordDetector {
    /// No-op detector. The pipeline can always construct one of these as
    /// the safe default.
    pub fn disabled() -> Self {
        Self {
            models: WakeWordModels {
                melspectrogram: PathBuf::new(),
                embedding: PathBuf::new(),
                keyword: PathBuf::new(),
            },
            sensitivity: 0.5,
            phrase: "hey ria".into(),
            aliases: vec![],
            backend: Backend::Disabled,
        }
    }

    /// Try to construct a real detector from on-disk models. Falls back to
    /// [`Self::disabled`] when the `voice-wake-oww` feature is off, when any
    /// model file is missing, or when ONNX session creation fails. Reason is
    /// always logged.
    pub fn try_load(
        keyword_model: PathBuf,
        sensitivity: f32,
        phrase: impl Into<String>,
        aliases: Vec<String>,
    ) -> Self {
        let models = WakeWordModels::from_keyword_path(keyword_model);
        let phrase = phrase.into();

        #[cfg(not(feature = "voice-wake-oww"))]
        {
            tracing::info!(
                keyword = %models.keyword.display(),
                "wake-word requested but voice-wake-oww feature not compiled; disabled"
            );
            let _ = (sensitivity, &aliases);
            return Self {
                models,
                sensitivity,
                phrase,
                aliases,
                backend: Backend::Disabled,
            };
        }

        #[cfg(feature = "voice-wake-oww")]
        {
            if !models.all_present() {
                tracing::warn!(
                    mel = %models.melspectrogram.display(),
                    embed = %models.embedding.display(),
                    kw = %models.keyword.display(),
                    "openWakeWord model files missing; detector disabled"
                );
                return Self {
                    models,
                    sensitivity,
                    phrase,
                    aliases,
                    backend: Backend::Disabled,
                };
            }
            match oww::OwnBackend::load(&models) {
                Ok(b) => Self {
                    models,
                    sensitivity,
                    phrase,
                    aliases,
                    backend: Backend::OpenWakeWord(Box::new(b)),
                },
                Err(e) => {
                    tracing::warn!(error = %e, "failed to load openWakeWord; disabled");
                    Self {
                        models,
                        sensitivity,
                        phrase,
                        aliases,
                        backend: Backend::Disabled,
                    }
                }
            }
        }
    }

    /// `true` when the detector is active and will consume audio frames.
    pub fn is_active(&self) -> bool {
        !matches!(self.backend, Backend::Disabled)
    }

    /// Spawn the detector loop. Returns immediately; aborts when the
    /// returned `JoinHandle` is dropped or when `frame_rx` closes.
    ///
    /// When the backend is disabled the loop returns immediately without
    /// touching the channels, so callers can spawn unconditionally.
    pub fn spawn(
        self: Arc<Self>,
        mut _frame_rx: broadcast::Receiver<AudioChunk>,
        #[allow(unused_variables)] event_tx: mpsc::UnboundedSender<WakeWordEvent>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            if !self.is_active() {
                return;
            }

            #[cfg(feature = "voice-wake-oww")]
            {
                let phrase = self.phrase.clone();
                let sensitivity = self.sensitivity;
                let Backend::OpenWakeWord(ref backend) = self.backend else {
                    return;
                };
                let mut state = oww::StreamingState::new();
                while let Ok(chunk) = _frame_rx.recv().await {
                    let pcm = ensure_16k_mono(&chunk);
                    state.feed(&pcm);
                    while let Some(score) = backend.try_step(&mut state) {
                        if score >= sensitivity {
                            state.note_fire();
                            let _ = event_tx.send(WakeWordEvent {
                                phrase: phrase.clone(),
                                score,
                                source: "oww".into(),
                            });
                        }
                    }
                }
            }
        })
    }
}

/// Resample (linear) and downmix to 16 kHz mono. When the input is already
/// 16 kHz mono this is a clone. Used so the wake-word detector tolerates
/// capture streams at native device rates without failing silently.
#[allow(dead_code)]
pub(crate) fn ensure_16k_mono(chunk: &AudioChunk) -> Vec<f32> {
    let mono: Vec<f32> = if chunk.channels <= 1 {
        chunk.samples.clone()
    } else {
        let ch = chunk.channels as usize;
        chunk
            .samples
            .chunks_exact(ch)
            .map(|frame| frame.iter().sum::<f32>() / ch as f32)
            .collect()
    };
    if chunk.sample_rate == OWW_SAMPLE_RATE {
        return mono;
    }
    let ratio = OWW_SAMPLE_RATE as f32 / chunk.sample_rate as f32;
    let out_len = (mono.len() as f32 * ratio) as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src = i as f32 / ratio;
        let lo = src.floor() as usize;
        let hi = (lo + 1).min(mono.len().saturating_sub(1));
        let t = src - lo as f32;
        let s = mono.get(lo).copied().unwrap_or(0.0) * (1.0 - t)
            + mono.get(hi).copied().unwrap_or(0.0) * t;
        out.push(s);
    }
    out
}

// ─── ONNX backend (feature-gated) ──────────────────────────────────────────

#[cfg(feature = "voice-wake-oww")]
mod oww {
    //! openWakeWord ONNX inference + streaming buffers.

    use std::collections::VecDeque;
    use std::sync::Mutex as StdMutex;
    use std::time::Instant;

    use anyhow::{Context, Result};
    use ort::session::Session;
    use ort::value::Tensor;

    use super::*;

    pub(super) struct OwnBackend {
        mel: StdMutex<Session>,
        embed: StdMutex<Session>,
        keyword: StdMutex<Session>,
    }

    impl OwnBackend {
        pub fn load(m: &WakeWordModels) -> Result<Self> {
            let mel = Session::builder()?
                .commit_from_file(&m.melspectrogram)
                .with_context(|| format!("loading mel model {}", m.melspectrogram.display()))?;
            let embed = Session::builder()?
                .commit_from_file(&m.embedding)
                .with_context(|| format!("loading embedding model {}", m.embedding.display()))?;
            let keyword = Session::builder()?
                .commit_from_file(&m.keyword)
                .with_context(|| format!("loading keyword model {}", m.keyword.display()))?;
            tracing::info!(
                mel = %m.melspectrogram.display(),
                embed = %m.embedding.display(),
                kw = %m.keyword.display(),
                "openWakeWord models loaded"
            );
            Ok(Self {
                mel: StdMutex::new(mel),
                embed: StdMutex::new(embed),
                keyword: StdMutex::new(keyword),
            })
        }

        /// Drive one streaming step. Returns `Some(score)` whenever a new
        /// keyword-head inference completed, `None` when more audio is
        /// needed before the next step.
        pub fn try_step(&self, state: &mut StreamingState) -> Option<f32> {
            // 1) Audio → mel.
            while state.audio.len() >= OWW_AUDIO_STEP {
                let frame: Vec<f32> = state.audio.drain(..OWW_AUDIO_STEP).collect();
                if let Some(mel_frames) = self.run_mel(&frame) {
                    for f in mel_frames {
                        state.mel.push_back(f);
                    }
                }
            }

            // 2) Mel → embedding → keyword.
            while state.mel.len() >= OWW_MEL_WINDOW {
                let window: Vec<[f32; OWW_MEL_BINS]> =
                    state.mel.iter().take(OWW_MEL_WINDOW).copied().collect();
                if let Some(emb) = self.run_embedding(&window) {
                    state.embeds.push_back(emb);
                    while state.embeds.len() > OWW_EMBED_WINDOW {
                        state.embeds.pop_front();
                    }
                    for _ in 0..OWW_MEL_STRIDE {
                        state.mel.pop_front();
                    }
                    if state.embeds.len() == OWW_EMBED_WINDOW && !state.in_cooldown() {
                        let embeds: Vec<[f32; OWW_EMBED_DIM]> =
                            state.embeds.iter().copied().collect();
                        return self.run_keyword(&embeds);
                    }
                } else {
                    state.mel.pop_front();
                }
            }
            None
        }

        fn run_mel(&self, audio: &[f32]) -> Option<Vec<[f32; OWW_MEL_BINS]>> {
            let mut sess = self.mel.lock().ok()?;
            let input = Tensor::from_array(([1usize, audio.len()], audio.to_vec())).ok()?;
            let outputs = sess.run(ort::inputs!["input" => input]).ok()?;
            let value = outputs.iter().next()?.1;
            let (shape, data) = value.try_extract_tensor::<f32>().ok()?;
            let bins = *shape.last()? as usize;
            if bins != OWW_MEL_BINS {
                tracing::warn!(bins, "unexpected mel bin count");
                return None;
            }
            // openWakeWord normalisation: (mel/10) + 2
            let mut frames = Vec::with_capacity(data.len() / bins);
            for chunk in data.chunks_exact(bins) {
                let mut frame = [0.0f32; OWW_MEL_BINS];
                for (i, v) in chunk.iter().enumerate() {
                    frame[i] = (v / 10.0) + 2.0;
                }
                frames.push(frame);
            }
            Some(frames)
        }

        fn run_embedding(
            &self,
            mels: &[[f32; OWW_MEL_BINS]],
        ) -> Option<[f32; OWW_EMBED_DIM]> {
            let mut sess = self.embed.lock().ok()?;
            let mut flat = Vec::with_capacity(OWW_MEL_WINDOW * OWW_MEL_BINS);
            for f in mels.iter().take(OWW_MEL_WINDOW) {
                flat.extend_from_slice(f);
            }
            let input =
                Tensor::from_array(([1usize, OWW_MEL_WINDOW, OWW_MEL_BINS, 1usize], flat))
                    .ok()?;
            let outputs = sess.run(ort::inputs!["input_1" => input]).ok()?;
            let value = outputs.iter().next()?.1;
            let (_shape, data) = value.try_extract_tensor::<f32>().ok()?;
            if data.len() < OWW_EMBED_DIM {
                return None;
            }
            let mut emb = [0.0f32; OWW_EMBED_DIM];
            emb.copy_from_slice(&data[..OWW_EMBED_DIM]);
            Some(emb)
        }

        fn run_keyword(&self, embeds: &[[f32; OWW_EMBED_DIM]]) -> Option<f32> {
            let mut sess = self.keyword.lock().ok()?;
            let mut flat = Vec::with_capacity(OWW_EMBED_WINDOW * OWW_EMBED_DIM);
            for e in embeds {
                flat.extend_from_slice(e);
            }
            let input = Tensor::from_array((
                [1usize, OWW_EMBED_WINDOW, OWW_EMBED_DIM],
                flat,
            ))
            .ok()?;
            // Wake-head exports use varying first-input names; pick whatever
            // ort reports.
            let input_name = sess
                .inputs()
                .first()
                .map(|i| i.name().to_string())
                .unwrap_or_else(|| "input".into());
            let outputs = sess
                .run(ort::inputs![input_name.as_str() => input])
                .ok()?;
            let value = outputs.iter().next()?.1;
            let (_shape, data) = value.try_extract_tensor::<f32>().ok()?;
            data.first().copied()
        }
    }

    pub(super) struct StreamingState {
        pub(super) audio: VecDeque<f32>,
        pub(super) mel: VecDeque<[f32; OWW_MEL_BINS]>,
        pub(super) embeds: VecDeque<[f32; OWW_EMBED_DIM]>,
        last_fire: Option<Instant>,
    }

    impl StreamingState {
        pub fn new() -> Self {
            Self {
                audio: VecDeque::with_capacity(OWW_AUDIO_STEP * 4),
                mel: VecDeque::with_capacity(OWW_MEL_WINDOW * 2),
                embeds: VecDeque::with_capacity(OWW_EMBED_WINDOW),
                last_fire: None,
            }
        }

        pub fn feed(&mut self, samples: &[f32]) {
            self.audio.extend(samples.iter().copied());
        }

        pub fn note_fire(&mut self) {
            self.last_fire = Some(Instant::now());
        }

        pub fn in_cooldown(&self) -> bool {
            self.last_fire
                .map(|t| t.elapsed() < std::time::Duration::from_millis(500))
                .unwrap_or(false)
        }
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_detector_is_inactive() {
        let d = WakeWordDetector::disabled();
        assert!(!d.is_active());
    }

    #[test]
    fn missing_models_fall_back_to_disabled() {
        let d = WakeWordDetector::try_load(
            PathBuf::from("/nonexistent/path/hey_ria.onnx"),
            0.5,
            "hey ria",
            vec!["hey riya".into()],
        );
        assert!(!d.is_active());
        assert_eq!(d.phrase, "hey ria");
    }

    #[test]
    fn model_paths_resolve_alongside_keyword() {
        let m = WakeWordModels::from_keyword_path(PathBuf::from("/m/wake/hey_ria.onnx"));
        assert_eq!(m.melspectrogram, PathBuf::from("/m/wake/melspectrogram.onnx"));
        assert_eq!(m.embedding, PathBuf::from("/m/wake/embedding_model.onnx"));
        assert_eq!(m.keyword, PathBuf::from("/m/wake/hey_ria.onnx"));
    }

    #[test]
    fn ensure_16k_mono_passthrough_when_native_rate() {
        let chunk = AudioChunk {
            samples: vec![0.1, -0.1, 0.2, -0.2],
            sample_rate: 16_000,
            channels: 1,
        };
        assert_eq!(ensure_16k_mono(&chunk), chunk.samples);
    }

    #[test]
    fn ensure_16k_mono_downmixes_stereo() {
        let chunk = AudioChunk {
            samples: vec![1.0, -1.0, 0.5, -0.5],
            sample_rate: 16_000,
            channels: 2,
        };
        assert_eq!(ensure_16k_mono(&chunk), vec![0.0, 0.0]);
    }

    #[test]
    fn ensure_16k_mono_downsamples_48k() {
        let chunk = AudioChunk {
            samples: vec![0.0; 4800], // 100 ms @ 48k
            sample_rate: 48_000,
            channels: 1,
        };
        let out = ensure_16k_mono(&chunk);
        // 100 ms @ 16k = 1600 samples; allow ±2 for floor() rounding.
        assert!(
            (out.len() as i32 - 1600).abs() <= 2,
            "expected ~1600, got {}",
            out.len()
        );
    }

    #[test]
    fn protocol_constants_match_openwakeword() {
        // Guardrails: any change to these must be intentional + tested.
        assert_eq!(OWW_SAMPLE_RATE, 16_000);
        assert_eq!(OWW_AUDIO_STEP, 1280);
        assert_eq!(OWW_MEL_BINS, 32);
        assert_eq!(OWW_MEL_WINDOW, 76);
        assert_eq!(OWW_MEL_STRIDE, 8);
        assert_eq!(OWW_EMBED_DIM, 96);
        assert_eq!(OWW_EMBED_WINDOW, 16);
    }
}
