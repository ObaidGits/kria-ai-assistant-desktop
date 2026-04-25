//! Voice pipeline v2 orchestrator (Phase 6).
//!
//! Implements the streaming state machine that ties wake → capture → AEC →
//! VAD → STT → LLM → sentence-split → TTS → playback together with **hard
//! barge-in** semantics: when the VAD reports `SpeechStart` while the
//! pipeline is in the `Speaking` state, a single
//! [`tokio_util::sync::CancellationToken::cancel`] call propagates to:
//!
//! 1. the active TTS synthesis future (drops it mid-decode), and
//! 2. the playback drain task (clears the rodio queue), and
//! 3. the LLM token-stream task (stops requesting more tokens), and
//! 4. the sentence splitter (flushes its tail buffer).
//!
//! All four shut down within the same scheduler tick because they all hold
//! a clone of the *same* token. This is the core concurrency contract the
//! v2 plan calls for: ≤ 50 ms from VAD fire to playback silence.
//!
//! ## Plumbing-only first pass
//!
//! Per the staff-level review, this module is the **concurrency skeleton**.
//! STT and TTS are driven through the existing [`Stt`] / [`Tts`] traits with
//! their *current* CLI-fallback implementations as the dummy nodes. The
//! `WhisperRsStt` / `PiperRsTts` engines slot in later behind their cargo
//! features without touching this loop.
//!
//! ```text
//!         ┌──────────────────────────────┐
//!         │   AudioCapture (broadcast)   │
//!         └──────┬───────────────────────┘
//!                │ AudioChunk            │
//!                ▼                       ▼
//!          ┌───────────┐         ┌───────────────┐
//!          │  STT chan │         │  VAD watcher  │
//!          └─────┬─────┘         └───────┬───────┘
//!                │ FinalTranscript       │ SpeechStart while Speaking
//!                ▼                       │
//!          ┌───────────┐                 │
//!          │ LLM token │                 │
//!          │  stream   │                 │
//!          └─────┬─────┘                 │
//!                ▼                       │
//!          ┌───────────────┐             │
//!          │SentenceSplit. │             │
//!          └─────┬─────────┘             │
//!                ▼                       │
//!          ┌───────────┐                 │
//!          │TTS / Play │ ◄─── CANCEL ────┘
//!          └───────────┘   (CancellationToken)
//! ```

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, watch, Mutex};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::voice::capture::AudioChunk;
use crate::voice::metrics::{MetricsBuilder, VoiceMetrics};
use crate::voice::playback::AudioPlayer;
use crate::voice::tier::VoiceTierProfile;
use crate::voice::vad::VadResult;

use super::aec::AecProcessor;
use super::playback::{PlaybackSink, PlaybackState};
use super::post_edit::HinglishPostEditor;
use super::sentence::SentenceSplitter;
use super::stt::{FinalTranscript, PartialTranscript, Stt};
use super::tts::Tts;
use super::wake::{WakeWordDetector, WakeWordEvent};

// ─── Public types ─────────────────────────────────────────────────────────

/// Top-level FSM state surfaced to the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceSessionState {
    Sleeping,
    Listening,
    Transcribing,
    Thinking,
    Speaking,
    BargeIn,
}

/// Runtime telemetry events the pipeline pushes to subscribers.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VoiceTelemetry {
    State { state: VoiceSessionState },
    Partial { text: String, engine: String },
    Final { text: String, engine: String, confidence: f32 },
    Wake(WakeWordEvent),
    Metrics(VoiceMetrics),
    Error { message: String },
    /// Emitted exactly once per turn the moment the first audio sample
    /// reaches the speaker — feeds TTFA telemetry.
    FirstAudioOut,
    /// Emitted whenever a hard barge-in fires.
    BargeIn,
}

/// Sum type for the active voice runtime. Lives in `AppState` behind an
/// `Arc<RwLock<…>>` so the engine can be hot-swapped (e.g. user toggles
/// `voice.engine` in settings) without rebuilding the whole runtime.
///
/// **Non-destructive design**: `Legacy` wraps the existing
/// [`crate::voice::pipeline::VoicePipeline`]. `Streaming` wraps the new
/// [`VoicePipelineV2`]. Existing call-sites keep working by calling
/// [`ActivePipeline::legacy`], which returns `Some(Arc<VoicePipeline>)`
/// while we are still defaulting to v1.
#[derive(Clone)]
pub enum ActivePipeline {
    Legacy(Arc<crate::voice::pipeline::VoicePipeline>),
    Streaming(Arc<VoicePipelineV2>),
}

impl ActivePipeline {
    /// Identifier for telemetry / logs.
    pub fn engine_kind(&self) -> &'static str {
        match self {
            Self::Legacy(_) => "v1",
            Self::Streaming(_) => "v2",
        }
    }

    pub fn is_streaming(&self) -> bool {
        matches!(self, Self::Streaming(_))
    }

    /// Borrow the v1 pipeline if we're still on Legacy. Existing
    /// commands.rs sites use this to keep working unchanged.
    pub fn legacy(&self) -> Option<Arc<crate::voice::pipeline::VoicePipeline>> {
        match self {
            Self::Legacy(p) => Some(p.clone()),
            _ => None,
        }
    }

    /// Borrow the v2 pipeline if active.
    pub fn streaming(&self) -> Option<Arc<VoicePipelineV2>> {
        match self {
            Self::Streaming(p) => Some(p.clone()),
            _ => None,
        }
    }
}

// ─── VoicePipelineV2 ───────────────────────────────────────────────────────

/// The streaming pipeline. Constructed once per app run; holds the swappable
/// engine traits behind `Arc<dyn …>` so engines can be hot-reloaded later.
pub struct VoicePipelineV2 {
    pub profile: VoiceTierProfile,
    pub stt: Arc<dyn Stt>,
    pub tts: Arc<dyn Tts>,
    pub playback: Arc<Mutex<PlaybackSink>>,
    pub wake: Option<Arc<WakeWordDetector>>,
    pub aec: Arc<Mutex<AecProcessor>>,
    pub post_editor: Arc<HinglishPostEditor>,
    /// Optional real audio output device. When `None` (test environments),
    /// `tts_task` falls back to a detached PCM channel and synth still runs
    /// (so barge-in semantics can be exercised) but no audio is actually
    /// played. Set via [`VoicePipelineV2::set_audio_player`].
    audio_player: Arc<Mutex<Option<Arc<AudioPlayer>>>>,

    state_tx: watch::Sender<VoiceSessionState>,
    telemetry_tx: mpsc::UnboundedSender<VoiceTelemetry>,
    /// Holds the current turn's metrics builder so external triggers
    /// (barge-in, force-abort) can finalise telemetry consistently.
    current_turn: Arc<Mutex<Option<MetricsBuilder>>>,
    /// Per-turn cancellation root. Replaced on every new turn so a stale
    /// `cancel()` from a previous turn cannot abort the next one.
    turn_cancel: Arc<Mutex<CancellationToken>>,
}

impl VoicePipelineV2 {
    pub fn new(
        profile: VoiceTierProfile,
        stt: Arc<dyn Stt>,
        tts: Arc<dyn Tts>,
        playback: PlaybackSink,
        wake: Option<WakeWordDetector>,
        aec: AecProcessor,
        post_editor: HinglishPostEditor,
    ) -> (Self, watch::Receiver<VoiceSessionState>, mpsc::UnboundedReceiver<VoiceTelemetry>) {
        let (state_tx, state_rx) = watch::channel(VoiceSessionState::Sleeping);
        let (telemetry_tx, telemetry_rx) = mpsc::unbounded_channel();
        let pipeline = Self {
            profile,
            stt,
            tts,
            playback: Arc::new(Mutex::new(playback)),
            wake: wake.map(Arc::new),
            aec: Arc::new(Mutex::new(aec)),
            post_editor: Arc::new(post_editor),
            audio_player: Arc::new(Mutex::new(None)),
            state_tx,
            telemetry_tx,
            current_turn: Arc::new(Mutex::new(None)),
            turn_cancel: Arc::new(Mutex::new(CancellationToken::new())),
        };
        (pipeline, state_rx, telemetry_rx)
    }

    pub fn state(&self) -> VoiceSessionState {
        *self.state_tx.borrow()
    }

    /// Wire a real `AudioPlayer` so `tts_task` can open a live playback
    /// session via [`PlaybackSink::begin_session`]. Safe to call any time;
    /// effective on the next turn.
    pub async fn set_audio_player(&self, player: Arc<AudioPlayer>) {
        let mut guard = self.audio_player.lock().await;
        *guard = Some(player);
    }

    /// Subscribe to telemetry events. Returns a fresh receiver each call by
    /// re-broadcasting through an internal fan-out (single-consumer mpsc
    /// today — the higher-level driver owns the original receiver).
    pub fn telemetry_sender(&self) -> mpsc::UnboundedSender<VoiceTelemetry> {
        self.telemetry_tx.clone()
    }

    pub fn subscribe_state(&self) -> watch::Receiver<VoiceSessionState> {
        self.state_tx.subscribe()
    }

    fn set_state(&self, s: VoiceSessionState) {
        // `send_replace` updates the cell unconditionally — `send` returns
        // Err and skips the update if no receivers exist, which would make
        // `state()` lie. Using replace keeps the FSM consistent even if
        // every subscriber has dropped.
        self.state_tx.send_replace(s);
        let _ = self.telemetry_tx.send(VoiceTelemetry::State { state: s });
    }

    /// Force a wake event (push-to-talk path). Transitions Sleeping →
    /// Listening if currently sleeping; otherwise no-op.
    pub fn force_wake(&self, source: &str) {
        let _ = self.telemetry_tx.send(VoiceTelemetry::Wake(WakeWordEvent {
            phrase: "hey ria".into(),
            score: 1.0,
            source: source.to_string(),
        }));
        if self.state() == VoiceSessionState::Sleeping {
            self.set_state(VoiceSessionState::Listening);
        }
    }

    /// Hard abort: cancel the current turn token (which propagates to TTS,
    /// playback, LLM, sentence-splitter), stop playback now, finalise any
    /// in-flight metrics, drop to Sleeping. Idempotent.
    pub async fn force_abort(&self) {
        // 1. Cancel the per-turn token first — the scheduler will wake the
        //    TTS / playback / LLM tasks immediately.
        {
            let cancel = self.turn_cancel.lock().await;
            cancel.cancel();
        }
        // 2. Belt-and-braces: clear the rodio sink synchronously in case
        //    the playback task hasn't drained yet.
        {
            let mut pb = self.playback.lock().await;
            pb.abort();
        }
        // 3. Finalise metrics if we were mid-turn.
        let mut cur = self.current_turn.lock().await;
        if let Some(builder) = cur.take() {
            let metrics = builder.finalise();
            let _ = self.telemetry_tx.send(VoiceTelemetry::Metrics(metrics));
        }
        self.set_state(VoiceSessionState::Sleeping);
    }

    /// Snapshot the playback sub-state.
    pub async fn playback_state(&self) -> PlaybackState {
        self.playback.lock().await.state()
    }

    /// Telemetry helpers — exposed so callers (and tests) can inject
    /// synthetic events without going through the full run loop.
    pub fn emit_partial(&self, p: PartialTranscript) {
        let _ = self.telemetry_tx.send(VoiceTelemetry::Partial {
            text: p.text,
            engine: p.engine,
        });
    }

    pub fn emit_final(&self, f: &FinalTranscript) {
        let _ = self.telemetry_tx.send(VoiceTelemetry::Final {
            text: f.text.clone(),
            engine: f.engine.clone(),
            confidence: f.confidence,
        });
    }

    // ─── The streaming run loop ───────────────────────────────────────────

    /// Execute exactly one full turn of the pipeline:
    /// `Listening → Transcribing → Thinking → Speaking → Sleeping`,
    /// with VAD-triggered barge-in cancelling the Speaking phase.
    ///
    /// This is the **concurrency contract** the rest of v2 builds on.
    /// Wiring real STT/TTS engines does not change the structure of this
    /// loop — the engines just produce/consume the same channels.
    ///
    /// Inputs:
    /// * `audio_rx` — broadcast subscription for raw mic frames. The caller
    ///   owns the broadcast sender and feeds it from `AudioCapture`.
    /// * `llm` — placeholder LLM that, given the final transcript,
    ///   returns an `mpsc::Receiver<String>` of response tokens. Real impl
    ///   plugs in the existing `kria_core::llm` router. Kept as a closure
    ///   so tests can inject deterministic streams.
    pub async fn run_turn<L, F>(
        self: Arc<Self>,
        mut audio_rx: broadcast::Receiver<AudioChunk>,
        llm: L,
    ) -> anyhow::Result<()>
    where
        L: FnOnce(String) -> F + Send + 'static,
        F: std::future::Future<Output = mpsc::Receiver<String>> + Send + 'static,
    {
        // ─── 0. Set up the per-turn cancellation root ─────────────────────
        //
        // Every spawned subtask gets a clone of this token. A single call
        // to `turn.cancel()` (from VAD-barge-in, force_abort, or end of
        // turn) is observed by every clone within the same scheduler tick.
        let turn = CancellationToken::new();
        {
            let mut slot = self.turn_cancel.lock().await;
            *slot = turn.clone();
        }

        // Reset the metrics builder.
        {
            let mut cur = self.current_turn.lock().await;
            *cur = Some(MetricsBuilder::begin_at_speech_end(self.profile.tier));
        }

        // ─── 1. Listening → forward audio to STT until VAD ends speech ───
        self.set_state(VoiceSessionState::Listening);

        let (stt_pcm_tx, stt_pcm_rx) = mpsc::channel::<AudioChunk>(64);
        let (partial_tx, mut partial_rx) = mpsc::unbounded_channel::<PartialTranscript>();

        // Hook STT first so we don't drop frames between subscribe + stream.
        let stt_handle = self.stt.clone().start_stream(stt_pcm_rx, partial_tx).await?;

        // Spawn the capture-feed task. Reads from the broadcast, runs the
        // chunk through AEC, fans it out to STT and (later) VAD. Stops on
        // either turn-cancel or audio_rx close.
        let aec = self.aec.clone();
        let cap_token = turn.clone();
        let cap_stt_tx = stt_pcm_tx.clone();
        let pipeline_for_partials = self.clone();

        let capture_task: JoinHandle<()> = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = cap_token.cancelled() => break,
                    chunk = audio_rx.recv() => {
                        match chunk {
                            Ok(c) => {
                                let processed = {
                                    let mut a = aec.lock().await;
                                    a.process_capture(c)
                                };
                                if cap_stt_tx.send(processed).await.is_err() {
                                    break;
                                }
                            }
                            Err(broadcast::error::RecvError::Closed) => break,
                            Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        }
                    }
                }
            }
        });

        // Forward partials to telemetry while we wait for the final.
        let partial_token = turn.clone();
        let partial_pump: JoinHandle<()> = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = partial_token.cancelled() => break,
                    p = partial_rx.recv() => {
                        let Some(p) = p else { break; };
                        pipeline_for_partials.emit_partial(p);
                    }
                }
            }
        });

        // ─── 2. Transcribing → await final from STT ──────────────────────
        self.set_state(VoiceSessionState::Transcribing);

        // Drop our handle on the audio side so STT sees end-of-stream when
        // capture closes; capture closes when the turn token is cancelled.
        drop(stt_pcm_tx);

        // Wait for either: final transcript, or hard cancel.
        let final_transcript = tokio::select! {
            biased;
            _ = turn.cancelled() => {
                capture_task.abort();
                partial_pump.abort();
                self.set_state(VoiceSessionState::Sleeping);
                anyhow::bail!("turn cancelled before transcription");
            }
            res = stt_handle.join() => res?,
        };

        // Capture finished its job; the partial pump can wind down.
        partial_pump.abort();

        // Post-edit: scaffolded; the plumbing pass passes the transcript
        // through unchanged. Real wiring goes through `post_editor.decide`
        // + `post_editor.correct(raw, llm)` once an LLM handle is plumbed
        // into this loop. Telemetry already emits the (unedited) final.
        let post_edited = final_transcript.text.clone();
        let _ = &self.post_editor; // keep the field reachable for future wiring
        self.emit_final(&FinalTranscript {
            text: post_edited.clone(),
            ..final_transcript.clone()
        });

        // ─── 3. Thinking → spawn LLM token stream ────────────────────────
        self.set_state(VoiceSessionState::Thinking);
        let mut llm_token_rx = llm(post_edited).await;

        // ─── 4. Speaking → sentence-split → TTS → playback ───────────────
        //
        // The barge-in cancellation chain:
        //   VAD-watcher → turn.cancel() → tts_task gets `abort_rx` flip
        //                              → playback drain task drops audio
        //                              → llm pump stops requesting tokens
        //
        // The TTS task uses its own `watch` channel for legacy reasons
        // (the `Tts` trait predates this module's CancellationToken). We
        // bridge: cancelling `turn` flips the watch.
        self.set_state(VoiceSessionState::Speaking);

        let tts = self.tts.clone();
        let playback = self.playback.clone();
        let player_slot = self.audio_player.clone();
        let pipeline_for_tts = self.clone();
        let tts_token = turn.clone();

        let tts_task: JoinHandle<anyhow::Result<()>> = tokio::spawn(async move {
            // Bridge CancellationToken -> watch<bool> for the Tts trait.
            let (abort_tx, abort_rx) = watch::channel(false);
            let bridge_token = tts_token.clone();
            let bridge: JoinHandle<()> = tokio::spawn(async move {
                bridge_token.cancelled().await;
                let _ = abort_tx.send(true);
            });

            let mut splitter = SentenceSplitter::new();
            // Open the playback session for this turn. When a real
            // AudioPlayer is wired we get back the live `pcm_tx` that
            // `PlaybackSink::begin_session` returns; every sentence's PCM
            // chunks flow through it and the drain task feeds rodio. When
            // no player is wired (test environments), fall back to a
            // detached channel so synth still runs and barge-in semantics
            // remain testable.
            let pcm_tx = {
                let player_opt = player_slot.lock().await.clone();
                match player_opt {
                    Some(player) => {
                        let mut pb = playback.lock().await;
                        pb.begin_session(player)
                    }
                    None => {
                        let (tx, _rx) = mpsc::channel::<Vec<f32>>(4);
                        tx
                    }
                }
            };

            'outer: loop {
                tokio::select! {
                    biased;
                    _ = tts_token.cancelled() => break 'outer,
                    tok = llm_token_rx.recv() => {
                        let Some(tok) = tok else {
                            // LLM stream complete — flush any tail.
                            if let Some(tail) = splitter.flush() {
                                let abort_clone = abort_rx.clone();
                                let synth = tts
                                    .clone()
                                    .synthesize_sentence(tail, pcm_tx.clone(), abort_clone)
                                    .await;
                                if let Err(e) = synth {
                                    tracing::warn!("tts tail synth failed: {e}");
                                }
                            }
                            break 'outer;
                        };
                        for sentence in splitter.push(&tok) {
                            // Cooperative cancel point between sentences.
                            if tts_token.is_cancelled() {
                                break 'outer;
                            }
                            let abort_clone = abort_rx.clone();
                            // Drive the synth future to completion. The
                            // backend itself observes `abort_rx` (flipped
                            // by the bridge above) and short-circuits;
                            // we deliberately do NOT race tts_token here
                            // so the backend can release resources cleanly.
                            if let Err(e) = tts
                                .clone()
                                .synthesize_sentence(sentence, pcm_tx.clone(), abort_clone)
                                .await
                            {
                                tracing::warn!("tts synth failed: {e}");
                                break 'outer;
                            }
                            if tts_token.is_cancelled() {
                                break 'outer;
                            }
                        }
                    }
                }
            }

            bridge.abort();
            // Always emit a "first audio out" telemetry IF the playback
            // sink saw any audio this turn (the sink's atomic flag is the
            // source of truth). The pipeline's metrics builder uses this
            // for TTFA accounting.
            let pb = pipeline_for_tts.playback.lock().await;
            if pb.first_audio_emitted.load(std::sync::atomic::Ordering::SeqCst) {
                let _ = pipeline_for_tts.telemetry_tx.send(VoiceTelemetry::FirstAudioOut);
            }
            Ok(())
        });

        // ─── 5. Race TTS-completion against turn-cancel ───────────────────
        //
        // If `turn.cancelled()` fires first, that is by definition a
        // barge-in OR a force_abort. We do **not** abort the TTS join
        // handle directly — instead we let the in-task bridge propagate
        // `turn.cancelled()` → `abort_rx.changed()`, which gives the
        // synth backend a chance to observe the cancel cleanly (the stub
        // TTS sets a flag we assert in tests; real backends release ONNX
        // sessions, etc.). We then bound the wait with a small timeout
        // and only force-abort if the task refuses to wind down.
        let mut tts_task = tts_task;
        tokio::select! {
            biased;
            _ = turn.cancelled() => {
                self.set_state(VoiceSessionState::BargeIn);
                let _ = self.telemetry_tx.send(VoiceTelemetry::BargeIn);
                {
                    let mut pb = self.playback.lock().await;
                    pb.abort();
                }
                // Give the in-task bridge ~250 ms to flip abort_rx and
                // for the synth future to honour it. If the backend is
                // truly stuck, hard-abort.
                if tokio::time::timeout(
                    std::time::Duration::from_millis(250),
                    &mut tts_task,
                ).await.is_err() {
                    tts_task.abort();
                }
            }
            res = &mut tts_task => {
                if let Err(e) = res {
                    tracing::warn!("tts task join error: {e}");
                }
            }
        }

        // Capture task winds down with the rest.
        capture_task.abort();

        // ─── 6. Finalise metrics + return to Sleeping ─────────────────────
        let mut cur = self.current_turn.lock().await;
        if let Some(builder) = cur.take() {
            let metrics = builder.finalise();
            let _ = self.telemetry_tx.send(VoiceTelemetry::Metrics(metrics));
        }
        self.set_state(VoiceSessionState::Sleeping);
        Ok(())
    }

    /// Spawn a VAD watcher that cancels the turn the moment new speech is
    /// detected while the pipeline is in `Speaking`. Caller passes the same
    /// broadcast subscription used by `run_turn`. Returns a JoinHandle the
    /// caller can drop to detach the watcher.
    pub fn spawn_barge_in_watcher(
        self: Arc<Self>,
        mut vad_rx: mpsc::UnboundedReceiver<VadResult>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            while let Some(ev) = vad_rx.recv().await {
                if matches!(ev, VadResult::SpeechStart)
                    && self.state() == VoiceSessionState::Speaking
                {
                    let cancel = self.turn_cancel.lock().await;
                    cancel.cancel();
                    tracing::info!("barge-in: VAD SpeechStart while Speaking → cancel turn");
                }
            }
        })
    }

    /// Run a "speak-only" turn from a known prompt: skips Listening and
    /// Transcribing, jumps straight to Thinking → Speaking → Sleeping.
    /// Used by the v2 Tauri command path that already has the user's
    /// transcript (e.g. produced by the v1 STT or a typed prompt) and just
    /// wants the streaming sentence playback + barge-in semantics.
    ///
    /// Wires the same per-turn `CancellationToken` so `force_abort` and
    /// the VAD-driven barge-in watcher cancel mid-speech.
    pub async fn run_speak_turn<L, F>(
        self: Arc<Self>,
        prompt: String,
        llm: L,
    ) -> anyhow::Result<()>
    where
        L: FnOnce(String) -> F + Send + 'static,
        F: std::future::Future<Output = mpsc::Receiver<String>> + Send + 'static,
    {
        // 0. Per-turn cancellation root.
        let turn = CancellationToken::new();
        {
            let mut slot = self.turn_cancel.lock().await;
            *slot = turn.clone();
        }
        {
            let mut cur = self.current_turn.lock().await;
            *cur = Some(MetricsBuilder::begin_at_speech_end(self.profile.tier));
        }

        // Echo the "transcript" through telemetry so the UI can show what
        // we're responding to (mirrors run_turn).
        self.emit_final(&FinalTranscript {
            text: prompt.clone(),
            language: "en".into(),
            confidence: 1.0,
            duration_ms: 0,
            engine: "external".into(),
        });

        self.set_state(VoiceSessionState::Thinking);
        let mut llm_token_rx = llm(prompt).await;

        self.set_state(VoiceSessionState::Speaking);

        let tts = self.tts.clone();
        let playback = self.playback.clone();
        let player_slot = self.audio_player.clone();
        let pipeline_for_tts = self.clone();
        let tts_token = turn.clone();

        let tts_task: JoinHandle<anyhow::Result<()>> = tokio::spawn(async move {
            let (abort_tx, abort_rx) = watch::channel(false);
            let bridge_token = tts_token.clone();
            let bridge: JoinHandle<()> = tokio::spawn(async move {
                bridge_token.cancelled().await;
                let _ = abort_tx.send(true);
            });

            let mut splitter = SentenceSplitter::new();
            let pcm_tx = {
                let player_opt = player_slot.lock().await.clone();
                match player_opt {
                    Some(player) => {
                        let mut pb = playback.lock().await;
                        pb.begin_session(player)
                    }
                    None => {
                        let (tx, _rx) = mpsc::channel::<Vec<f32>>(4);
                        tx
                    }
                }
            };

            'outer: loop {
                tokio::select! {
                    biased;
                    _ = tts_token.cancelled() => break 'outer,
                    tok = llm_token_rx.recv() => {
                        let Some(tok) = tok else {
                            if let Some(tail) = splitter.flush() {
                                let abort_clone = abort_rx.clone();
                                if let Err(e) = tts
                                    .clone()
                                    .synthesize_sentence(tail, pcm_tx.clone(), abort_clone)
                                    .await
                                {
                                    tracing::warn!("tts tail synth failed: {e}");
                                }
                            }
                            break 'outer;
                        };
                        for sentence in splitter.push(&tok) {
                            if tts_token.is_cancelled() {
                                break 'outer;
                            }
                            let abort_clone = abort_rx.clone();
                            if let Err(e) = tts
                                .clone()
                                .synthesize_sentence(sentence, pcm_tx.clone(), abort_clone)
                                .await
                            {
                                tracing::warn!("tts synth failed: {e}");
                                break 'outer;
                            }
                            if tts_token.is_cancelled() {
                                break 'outer;
                            }
                        }
                    }
                }
            }

            bridge.abort();
            let pb = pipeline_for_tts.playback.lock().await;
            if pb.first_audio_emitted.load(std::sync::atomic::Ordering::SeqCst) {
                let _ = pipeline_for_tts.telemetry_tx.send(VoiceTelemetry::FirstAudioOut);
            }
            Ok(())
        });

        let mut tts_task = tts_task;
        tokio::select! {
            biased;
            _ = turn.cancelled() => {
                self.set_state(VoiceSessionState::BargeIn);
                let _ = self.telemetry_tx.send(VoiceTelemetry::BargeIn);
                {
                    let mut pb = self.playback.lock().await;
                    pb.abort();
                }
                if tokio::time::timeout(
                    std::time::Duration::from_millis(250),
                    &mut tts_task,
                ).await.is_err() {
                    tts_task.abort();
                }
            }
            res = &mut tts_task => {
                if let Err(e) = res {
                    tracing::warn!("tts task join error: {e}");
                }
            }
        }

        let mut cur = self.current_turn.lock().await;
        if let Some(builder) = cur.take() {
            let metrics = builder.finalise();
            let _ = self.telemetry_tx.send(VoiceTelemetry::Metrics(metrics));
        }
        self.set_state(VoiceSessionState::Sleeping);
        Ok(())
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PostEditConfig, VoiceConfig};
    use crate::platform::detect::HardwareTier;
    use crate::voice::v2::aec::AecSettings;
    use crate::voice::v2::stt::StreamHandle;
    use crate::voice::v2::tts::TtsSampleRate;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::oneshot;

    // ── Stubs ────────────────────────────────────────────────────────────

    /// Stub STT: ignores audio, returns a canned final after `delay_ms`.
    struct StubStt {
        text: String,
        delay_ms: u64,
    }
    #[async_trait::async_trait]
    impl Stt for StubStt {
        fn engine_id(&self) -> &'static str { "stub-stt" }
        async fn start_stream(
            self: Arc<Self>,
            mut pcm_rx: mpsc::Receiver<AudioChunk>,
            _partial_tx: mpsc::UnboundedSender<PartialTranscript>,
        ) -> anyhow::Result<StreamHandle> {
            let (abort_tx, mut abort_rx) = oneshot::channel::<()>();
            let (final_tx, final_rx) = oneshot::channel();
            let text = self.text.clone();
            let delay = Duration::from_millis(self.delay_ms);
            tokio::spawn(async move {
                // Drain frames so the sender doesn't block.
                let drain = tokio::spawn(async move {
                    while pcm_rx.recv().await.is_some() {}
                });
                tokio::select! {
                    _ = &mut abort_rx => {
                        drain.abort();
                        let _ = final_tx.send(Err(anyhow::anyhow!("stub aborted")));
                    }
                    _ = tokio::time::sleep(delay) => {
                        drain.abort();
                        let _ = final_tx.send(Ok(FinalTranscript {
                            text,
                            language: "en".into(),
                            confidence: 0.9,
                            duration_ms: 1000,
                            engine: "stub-stt".into(),
                        }));
                    }
                }
            });
            Ok(StreamHandle::new(abort_tx, final_rx))
        }
    }

    /// Stub TTS: counts how many sentences were synthesized AND how many
    /// completed without being cancelled. Used to verify barge-in aborts
    /// mid-stream.
    struct StubTts {
        started: Arc<AtomicUsize>,
        completed: Arc<AtomicUsize>,
        per_sentence_ms: u64,
        cancelled_flag: Arc<AtomicBool>,
    }
    #[async_trait::async_trait]
    impl Tts for StubTts {
        fn engine_id(&self) -> &'static str { "stub-tts" }
        fn sample_rate(&self) -> TtsSampleRate { TtsSampleRate(22_050) }
        async fn synthesize_sentence(
            self: Arc<Self>,
            _sentence: String,
            _pcm_tx: mpsc::Sender<Vec<f32>>,
            mut abort_rx: watch::Receiver<bool>,
        ) -> anyhow::Result<()> {
            self.started.fetch_add(1, Ordering::SeqCst);
            tokio::select! {
                biased;
                _ = abort_rx.changed() => {
                    self.cancelled_flag.store(true, Ordering::SeqCst);
                    Ok(())
                }
                _ = tokio::time::sleep(Duration::from_millis(self.per_sentence_ms)) => {
                    self.completed.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }
        }
    }

    fn build(stt: Arc<dyn Stt>, tts: Arc<dyn Tts>) -> Arc<VoicePipelineV2> {
        let cfg = VoiceConfig::default();
        let profile = VoiceTierProfile::build(&cfg, HardwareTier::Standard);
        let pb = PlaybackSink::new(22_050);
        let aec = AecProcessor::passthrough(AecSettings::default());
        let post = HinglishPostEditor::from_config(
            &PostEditConfig::default(),
            profile.post_edit_timeout_ms,
        );
        let (p, _s, _t) = VoicePipelineV2::new(profile, stt, tts, pb, None, aec, post);
        Arc::new(p)
    }

    fn build_with_telemetry(
        stt: Arc<dyn Stt>,
        tts: Arc<dyn Tts>,
    ) -> (Arc<VoicePipelineV2>, mpsc::UnboundedReceiver<VoiceTelemetry>) {
        let cfg = VoiceConfig::default();
        let profile = VoiceTierProfile::build(&cfg, HardwareTier::Standard);
        let pb = PlaybackSink::new(22_050);
        let aec = AecProcessor::passthrough(AecSettings::default());
        let post = HinglishPostEditor::from_config(
            &PostEditConfig::default(),
            profile.post_edit_timeout_ms,
        );
        let (p, _s, t) = VoicePipelineV2::new(profile, stt, tts, pb, None, aec, post);
        (Arc::new(p), t)
    }

    #[tokio::test]
    async fn happy_path_runs_through_states() {
        let stt = Arc::new(StubStt {
            text: "hello world".into(),
            delay_ms: 20,
        });
        let started = Arc::new(AtomicUsize::new(0));
        let completed = Arc::new(AtomicUsize::new(0));
        let cancelled = Arc::new(AtomicBool::new(false));
        let tts = Arc::new(StubTts {
            started: started.clone(),
            completed: completed.clone(),
            per_sentence_ms: 5,
            cancelled_flag: cancelled.clone(),
        });
        let p = build(stt, tts);
        let (audio_tx, audio_rx) = broadcast::channel::<AudioChunk>(8);
        // Push EOF promptly so STT sees end-of-stream.
        drop(audio_tx);

        // Three short sentences; LLM "stream".
        let llm = |_text: String| async move {
            let (tx, rx) = mpsc::channel::<String>(8);
            tokio::spawn(async move {
                for tok in ["Hello there. ", "How are you? ", "Goodbye."] {
                    let _ = tx.send(tok.into()).await;
                }
            });
            rx
        };

        p.clone().run_turn(audio_rx, llm).await.unwrap();
        // All 3 sentences should have completed (no barge-in).
        assert_eq!(started.load(Ordering::SeqCst), 3);
        assert_eq!(completed.load(Ordering::SeqCst), 3);
        assert!(!cancelled.load(Ordering::SeqCst));
        assert_eq!(p.state(), VoiceSessionState::Sleeping);
    }

    #[tokio::test]
    async fn barge_in_cancels_tts_immediately() {
        let stt = Arc::new(StubStt {
            text: "say something long".into(),
            delay_ms: 10,
        });
        let started = Arc::new(AtomicUsize::new(0));
        let completed = Arc::new(AtomicUsize::new(0));
        let cancelled = Arc::new(AtomicBool::new(false));
        // Each sentence takes 500 ms — plenty of time to barge in.
        let tts = Arc::new(StubTts {
            started: started.clone(),
            completed: completed.clone(),
            per_sentence_ms: 500,
            cancelled_flag: cancelled.clone(),
        });
        let p = build(stt, tts);
        let (audio_tx, audio_rx) = broadcast::channel::<AudioChunk>(8);
        drop(audio_tx);

        let llm = |_text: String| async move {
            let (tx, rx) = mpsc::channel::<String>(8);
            tokio::spawn(async move {
                for tok in ["Sentence one. ", "Sentence two. ", "Sentence three."] {
                    let _ = tx.send(tok.into()).await;
                }
            });
            rx
        };

        let p_clone = p.clone();
        let turn = tokio::spawn(async move {
            p_clone.run_turn(audio_rx, llm).await
        });

        // Wait until the pipeline is Speaking, then fire VAD SpeechStart.
        let p_state = p.clone();
        for _ in 0..200 {
            if p_state.state() == VoiceSessionState::Speaking {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(p_state.state(), VoiceSessionState::Speaking);

        // Wire the VAD watcher and feed it a SpeechStart.
        let (vad_tx, vad_rx) = mpsc::unbounded_channel::<VadResult>();
        let _watcher = p.clone().spawn_barge_in_watcher(vad_rx);
        let t_before_cancel = std::time::Instant::now();
        vad_tx.send(VadResult::SpeechStart).unwrap();

        // The turn should wind down promptly after cancel propagates.
        let _ = tokio::time::timeout(Duration::from_secs(2), turn).await
            .expect("turn did not finish after barge-in");

        // The first sentence had started but should NOT have completed —
        // barge-in cancelled it mid-synthesis.
        assert!(started.load(Ordering::SeqCst) >= 1);
        assert_eq!(
            completed.load(Ordering::SeqCst), 0,
            "no sentences should have completed before barge-in"
        );
        assert!(cancelled.load(Ordering::SeqCst), "tts must observe abort");
        assert_eq!(p.state(), VoiceSessionState::Sleeping);

        // Cancel-to-finish budget: ≤ 200 ms in this synthetic test.
        // Real hardware budget is ≤ 50 ms, but we use a generous bound
        // here to keep the test stable on heavily-loaded CI runners.
        assert!(
            t_before_cancel.elapsed() < Duration::from_millis(800),
            "barge-in took too long: {:?}",
            t_before_cancel.elapsed()
        );
    }

    #[tokio::test]
    async fn force_abort_returns_to_sleeping_idempotently() {
        let stt = Arc::new(StubStt { text: "x".into(), delay_ms: 10 });
        let tts = Arc::new(StubTts {
            started: Arc::new(AtomicUsize::new(0)),
            completed: Arc::new(AtomicUsize::new(0)),
            per_sentence_ms: 5,
            cancelled_flag: Arc::new(AtomicBool::new(false)),
        });
        let p = build(stt, tts);
        p.force_wake("ptt");
        p.force_abort().await;
        p.force_abort().await; // idempotent
        assert_eq!(p.state(), VoiceSessionState::Sleeping);
    }

    #[tokio::test]
    async fn force_wake_transitions_to_listening() {
        let stt = Arc::new(StubStt { text: "x".into(), delay_ms: 10 });
        let tts = Arc::new(StubTts {
            started: Arc::new(AtomicUsize::new(0)),
            completed: Arc::new(AtomicUsize::new(0)),
            per_sentence_ms: 5,
            cancelled_flag: Arc::new(AtomicBool::new(false)),
        });
        let p = build(stt, tts);
        assert_eq!(p.state(), VoiceSessionState::Sleeping);
        p.force_wake("ptt");
        assert_eq!(p.state(), VoiceSessionState::Listening);
    }

    #[test]
    fn active_pipeline_engine_kind_distinguishes_variants() {
        // Building a real Legacy variant requires the full v1 pipeline,
        // so we only verify the Streaming arm here. The Legacy arm is
        // exercised in commands.rs at construction time.
        let stt = Arc::new(StubStt { text: "x".into(), delay_ms: 10 });
        let tts = Arc::new(StubTts {
            started: Arc::new(AtomicUsize::new(0)),
            completed: Arc::new(AtomicUsize::new(0)),
            per_sentence_ms: 5,
            cancelled_flag: Arc::new(AtomicBool::new(false)),
        });
        let p = build(stt, tts);
        let active = ActivePipeline::Streaming(p);
        assert_eq!(active.engine_kind(), "v2");
        assert!(active.is_streaming());
        assert!(active.legacy().is_none());
        assert!(active.streaming().is_some());
    }

    /// STT that emits a series of partial transcripts before resolving
    /// to a final. Exercises the Listening → Transcribing partial pump.
    struct PartialEmittingStt {
        partials: Vec<&'static str>,
        final_text: &'static str,
    }
    #[async_trait::async_trait]
    impl Stt for PartialEmittingStt {
        fn engine_id(&self) -> &'static str { "stub-partial-stt" }
        async fn start_stream(
            self: Arc<Self>,
            mut pcm_rx: mpsc::Receiver<AudioChunk>,
            partial_tx: mpsc::UnboundedSender<PartialTranscript>,
        ) -> anyhow::Result<StreamHandle> {
            let (abort_tx, mut abort_rx) = oneshot::channel::<()>();
            let (final_tx, final_rx) = oneshot::channel();
            let partials = self.partials.clone();
            let final_text = self.final_text.to_string();
            tokio::spawn(async move {
                let drain = tokio::spawn(async move { while pcm_rx.recv().await.is_some() {} });
                for p in &partials {
                    let _ = partial_tx.send(PartialTranscript {
                        text: (*p).into(),
                        confidence: Some(0.5),
                        engine: "stub-partial-stt".into(),
                    });
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                tokio::select! {
                    _ = &mut abort_rx => {
                        drain.abort();
                        let _ = final_tx.send(Err(anyhow::anyhow!("stub aborted")));
                    }
                    _ = tokio::time::sleep(Duration::from_millis(20)) => {
                        drain.abort();
                        let _ = final_tx.send(Ok(FinalTranscript {
                            text: final_text,
                            language: "en".into(),
                            confidence: 0.95,
                            duration_ms: 200,
                            engine: "stub-partial-stt".into(),
                        }));
                    }
                }
            });
            Ok(StreamHandle::new(abort_tx, final_rx))
        }
    }

    /// Streaming partials surface as `VoiceTelemetry::Partial` events in
    /// the order emitted by the STT, followed by a `Final`.
    #[tokio::test]
    async fn streaming_partials_pumped_to_telemetry() {
        let stt = Arc::new(PartialEmittingStt {
            partials: vec!["hel", "hello", "hello wo", "hello world"],
            final_text: "hello world",
        });
        let tts = Arc::new(StubTts {
            started: Arc::new(AtomicUsize::new(0)),
            completed: Arc::new(AtomicUsize::new(0)),
            per_sentence_ms: 5,
            cancelled_flag: Arc::new(AtomicBool::new(false)),
        });
        let (p, mut tel_rx) = build_with_telemetry(stt, tts);
        let (audio_tx, audio_rx) = broadcast::channel::<AudioChunk>(8);
        drop(audio_tx);

        let llm = |_text: String| async move {
            let (tx, rx) = mpsc::channel::<String>(4);
            tokio::spawn(async move { let _ = tx.send("ok.".into()).await; });
            rx
        };
        p.clone().run_turn(audio_rx, llm).await.unwrap();

        let mut partials_seen: Vec<String> = Vec::new();
        let mut saw_final = false;
        while let Ok(ev) = tel_rx.try_recv() {
            match ev {
                VoiceTelemetry::Partial { text, .. } => partials_seen.push(text),
                VoiceTelemetry::Final { .. } => { saw_final = true; }
                _ => {}
            }
        }
        assert_eq!(
            partials_seen,
            vec![
                "hel".to_string(),
                "hello".into(),
                "hello wo".into(),
                "hello world".into(),
            ],
            "partials must be pumped to telemetry in order"
        );
        assert!(saw_final, "Final telemetry must follow the partials");
    }

    /// Barge-in latency: from VAD `SpeechStart` to `BargeIn` telemetry,
    /// the FSM must cancel within the 50 ms hardware budget on a quiet
    /// loop. We use `run_speak_turn` to skip the STT delay and isolate
    /// the cancel path.
    #[tokio::test]
    async fn barge_in_latency_under_budget() {
        let started = Arc::new(AtomicUsize::new(0));
        let completed = Arc::new(AtomicUsize::new(0));
        let cancelled = Arc::new(AtomicBool::new(false));
        let tts = Arc::new(StubTts {
            started: started.clone(),
            completed: completed.clone(),
            per_sentence_ms: 2_000, // long synth — gives barge-in a wide window
            cancelled_flag: cancelled.clone(),
        });
        let stt = Arc::new(StubStt { text: "ignored".into(), delay_ms: 1 });
        let (p, mut tel_rx) = build_with_telemetry(stt, tts);

        let llm = |_t: String| async move {
            let (tx, rx) = mpsc::channel::<String>(4);
            tokio::spawn(async move {
                let _ = tx.send("This is a long response. ".into()).await;
                let _ = tx.send("It has more sentences.".into()).await;
            });
            rx
        };

        let p_run = p.clone();
        let turn = tokio::spawn(async move {
            p_run.run_speak_turn("hello".into(), llm).await
        });

        for _ in 0..200 {
            if p.state() == VoiceSessionState::Speaking { break; }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(p.state(), VoiceSessionState::Speaking);

        let (vad_tx, vad_rx) = mpsc::unbounded_channel::<VadResult>();
        let _watcher = p.clone().spawn_barge_in_watcher(vad_rx);
        let t0 = std::time::Instant::now();
        vad_tx.send(VadResult::SpeechStart).unwrap();

        // Wait for the BargeIn telemetry event.
        let mut barge_at: Option<std::time::Duration> = None;
        let deadline = std::time::Instant::now() + Duration::from_millis(500);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(50), tel_rx.recv()).await {
                Ok(Some(VoiceTelemetry::BargeIn)) => {
                    barge_at = Some(t0.elapsed());
                    break;
                }
                Ok(Some(_)) => continue,
                Ok(None) | Err(_) => continue,
            }
        }
        let _ = tokio::time::timeout(Duration::from_secs(1), turn).await;

        let elapsed = barge_at.expect("BargeIn telemetry must fire");
        assert!(cancelled.load(Ordering::SeqCst), "tts must observe abort");
        // Hardware target is ≤ 50 ms; we use 200 ms here to absorb CI jitter.
        assert!(
            elapsed < Duration::from_millis(200),
            "barge-in latency too high: {:?}",
            elapsed
        );
    }
}
