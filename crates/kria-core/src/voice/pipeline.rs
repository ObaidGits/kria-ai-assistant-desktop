use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch, Mutex};

use crate::config::VoiceConfig;
use crate::voice::capture::AudioCapture;
use crate::voice::playback::AudioPlayer;
use crate::voice::stt::SpeechToText;
use crate::voice::tts::TextToSpeech;
use crate::voice::vad::{VadResult, VoiceActivityDetector};

/// Pipeline states visible to the frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VoicePipelineState {
    Idle,
    Listening,
    Processing,
    Speaking,
}

/// Metadata-rich transcript frame used by partial and final transcript events.
#[derive(Debug, Clone, serde::Serialize)]
pub struct VoiceTranscriptFrame {
    pub text: String,
    pub confidence: f32,
    pub language: String,
    /// 0..1 signal indicating how likely the transcript is to remain unchanged.
    pub stability: f32,
}

/// Events emitted by the voice pipeline.
#[derive(Debug, Clone)]
pub enum VoicePipelineEvent {
    /// State changed.
    StateChanged(VoicePipelineState),
    /// Live partial transcription (streamed while user is still speaking).
    PartialTranscript(VoiceTranscriptFrame),
    /// Final STT transcription available (speech segment complete).
    Transcript(VoiceTranscriptFrame),
    /// TTS playback started.
    SpeakingStarted,
    /// TTS playback finished.
    SpeakingDone,
    /// Error occurred (non-fatal, pipeline continues).
    Error(String),
}

/// The orchestrator that wires capture → VAD → STT and TTS → playback.
pub struct VoicePipeline {
    config: VoiceConfig,
    stt: Arc<SpeechToText>,
    tts: Arc<TextToSpeech>,
    player: Arc<AudioPlayer>,
    state: Arc<Mutex<VoicePipelineState>>,
    /// Watch channel for state changes (frontend can poll).
    state_tx: watch::Sender<VoicePipelineState>,
    state_rx: watch::Receiver<VoicePipelineState>,
    /// Stop signal for the capture loop.
    stop_tx: Arc<Mutex<Option<mpsc::Sender<()>>>>,
    /// Whether capture is active (set/cleared on start/stop).
    capture_active: Arc<AtomicBool>,
    /// Echo prevention gate: when `true`, the capture thread discards audio
    /// chunks instead of forwarding them to the VAD/STT loop. Set while KRIA
    /// is speaking (TTS playback) so the microphone does not pick up the
    /// speaker output and transcribe it as user speech.
    mic_muted: Arc<AtomicBool>,
    /// Optional path to Silero VAD ONNX model.
    vad_model_path: Option<std::path::PathBuf>,
}

impl VoicePipeline {
    pub fn new(config: VoiceConfig, stt: SpeechToText, tts: TextToSpeech) -> Self {
        let (state_tx, state_rx) = watch::channel(VoicePipelineState::Idle);
        let follow_system_default_speaker = config.follow_system_default_speaker
            || config.speaker_device.trim().is_empty()
            || config.speaker_device.eq_ignore_ascii_case("auto");
        let preferred_speaker = if follow_system_default_speaker {
            None
        } else {
            Some(config.speaker_device.clone())
        };
        let player = AudioPlayer::new()
            .with_output_device(preferred_speaker)
            .follow_system_default(follow_system_default_speaker);

        Self {
            config,
            stt: Arc::new(stt),
            tts: Arc::new(tts),
            player: Arc::new(player),
            state: Arc::new(Mutex::new(VoicePipelineState::Idle)),
            state_tx,
            state_rx,
            stop_tx: Arc::new(Mutex::new(None)),
            capture_active: Arc::new(AtomicBool::new(false)),
            mic_muted: Arc::new(AtomicBool::new(false)),
            vad_model_path: None,
        }
    }

    /// Set the path to the Silero VAD ONNX model.
    pub fn with_vad_model(mut self, path: std::path::PathBuf) -> Self {
        self.vad_model_path = Some(path);
        self
    }

    /// Get current pipeline state.
    pub async fn state(&self) -> VoicePipelineState {
        *self.state.lock().await
    }

    /// Subscribe to state changes.
    pub fn state_watch(&self) -> watch::Receiver<VoicePipelineState> {
        self.state_rx.clone()
    }

    async fn set_state(&self, new_state: VoicePipelineState) {
        *self.state.lock().await = new_state;
        let _ = self.state_tx.send(new_state);
    }

    /// Start the voice capture → VAD → STT pipeline.
    /// Returns a receiver for pipeline events (transcript, state changes, errors).
    /// The pipeline runs in background tasks until `stop()` is called.
    pub async fn start(
        &self,
        event_tx: mpsc::UnboundedSender<VoicePipelineEvent>,
    ) -> anyhow::Result<()> {
        // Don't double-start
        if *self.state.lock().await != VoicePipelineState::Idle {
            return Ok(());
        }

        let sample_rate = 16000u32;
        let follow_system_default_mic = self.config.follow_system_default_mic
            || self.config.mic_device.trim().is_empty()
            || self.config.mic_device.eq_ignore_ascii_case("auto");
        let capture = AudioCapture::new(sample_rate)
            .with_input_device(self.config.mic_device.clone())
            .follow_system_default(follow_system_default_mic)
            .with_noise_suppression_mode(self.config.noise_suppression_mode.clone());
        let confidence_threshold = clamp01(self.config.confidence_threshold);

        // Start capture on a dedicated std thread (cpal::Stream is !Send).
        // The audio chunks are sent via an mpsc channel to the async VAD→STT task.
        let (chunk_tx, chunk_rx) = mpsc::unbounded_channel();
        let capture_active = self.capture_active.clone();
        let mic_muted = self.mic_muted.clone();
        self.capture_active.store(true, Ordering::Relaxed);
        std::thread::spawn(move || {
            while capture_active.load(Ordering::Relaxed) {
                let (mut audio_rx, handle) = match capture.start() {
                    Ok(pair) => pair,
                    Err(e) => {
                        tracing::error!("audio capture start failed: {e}");
                        std::thread::sleep(std::time::Duration::from_millis(400));
                        continue;
                    }
                };

                let active_device_name = handle.device_name().to_string();

                // Block on the std thread, forwarding chunks to the async side.
                // When capture_active is false or the receiver is dropped, we exit.
                while capture_active.load(Ordering::Relaxed) {
                    if handle.has_failed() {
                        tracing::warn!(
                            device = %active_device_name,
                            "audio capture stream reported failure; restarting"
                        );
                        break;
                    }

                    if capture.should_restart_for_default_change(&active_device_name) {
                        tracing::info!(
                            old_device = %active_device_name,
                            "system default microphone changed; restarting capture"
                        );
                        break;
                    }

                    match audio_rx.try_recv() {
                        Ok(chunk) => {
                            // Echo gate: discard chunks while KRIA is speaking so the
                            // mic does not pick up TTS speaker output and transcribe it.
                            if mic_muted.load(Ordering::Relaxed) {
                                continue;
                            }
                            if chunk_tx.send(chunk).is_err() {
                                return;
                            }
                        }
                        Err(mpsc::error::TryRecvError::Empty) => {
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                        Err(mpsc::error::TryRecvError::Disconnected) => break,
                    }
                }

                drop(handle);

                if capture_active.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(120));
                }
            }
            tracing::info!("audio capture thread exiting");
        });

        let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
        *self.stop_tx.lock().await = Some(stop_tx);

        self.set_state(VoicePipelineState::Listening).await;
        let _ = event_tx.send(VoicePipelineEvent::StateChanged(
            VoicePipelineState::Listening,
        ));

        let stt = self.stt.clone();
        let state = self.state.clone();
        let state_tx = self.state_tx.clone();
        let energy_threshold = self.config.energy_threshold;
        let vad_model_path = self.vad_model_path.clone();
        let partial_update_ms = self.config.partial_update_ms;
        let enable_partials = self.config.enable_partial_transcripts;
        let vad_silence_ms = self.config.vad_silence_ms;

        // Spawn the VAD→STT processing loop (async, receives chunks from capture thread)
        tokio::spawn(async move {
            // Audio chunks are ~100 ms each (see AudioCapture). Use the configured
            // silence timeout so the pipeline responds as quickly as the user expects.
            const CHUNK_MS: u64 = 100;
            let mut vad = match vad_model_path {
                Some(ref path) => VoiceActivityDetector::with_silero(energy_threshold, path)
                    .with_silence_ms(vad_silence_ms, CHUNK_MS),
                None => VoiceActivityDetector::new(energy_threshold)
                    .with_silence_ms(vad_silence_ms, CHUNK_MS),
            };
            let mut speech_buffer: Vec<f32> = Vec::new();
            let mut chunk_rx = chunk_rx;
            let mut chunk_count: usize = 0;
            let mut last_signal_at = Instant::now();
            let mut no_signal_reported = false;
            let signal_floor = (energy_threshold * 0.35).max(0.0015);
            // Track samples since last partial transcription for live feedback.
            let partial_interval_ms = partial_update_ms.max(200) as usize;
            let partial_interval = ((sample_rate as usize * partial_interval_ms) / 1000)
                .max((sample_rate as usize) / 5);
            let mut samples_since_partial: usize = 0;
            let partial_in_flight = Arc::new(AtomicBool::new(false));
            let partial_cache = Arc::new(Mutex::new(PartialTranscriptCache::default()));

            loop {
                tokio::select! {
                    _ = stop_rx.recv() => {
                        tracing::info!("voice pipeline: stop signal received");
                        break;
                    }
                    chunk = chunk_rx.recv() => {
                        let chunk = match chunk {
                            Some(c) => c,
                            None => break, // capture thread ended
                        };

                        chunk_count += 1;
                        let rms = rms_energy(&chunk.samples);
                        if rms >= signal_floor {
                            last_signal_at = Instant::now();
                            no_signal_reported = false;
                        }

                        // Emit a clear diagnostic when the microphone stream stays near-zero.
                        if !no_signal_reported && last_signal_at.elapsed() >= Duration::from_secs(15) {
                            no_signal_reported = true;
                            tracing::warn!(
                                rms,
                                energy_threshold,
                                signal_floor,
                                "voice pipeline: no usable microphone signal for 15s"
                            );
                            let _ = event_tx.send(VoicePipelineEvent::Error(
                                format!(
                                    "No microphone signal detected (rms={rms:.4}, threshold={energy_threshold:.4}). Check OS input level/mic selection."
                                )
                            ));
                        }

                        let result = vad.process(&chunk);
                        if chunk_count.is_multiple_of(20)
                            || matches!(result, VadResult::SpeechStart | VadResult::SpeechEnd)
                        {
                            tracing::debug!(
                                ?result,
                                rms,
                                energy_threshold,
                                signal_floor,
                                speaking = vad.is_speaking(),
                                speech_samples = speech_buffer.len(),
                                "voice pipeline: VAD decision"
                            );
                        }
                        match result {
                            VadResult::SpeechStart => {
                                speech_buffer.clear();
                                speech_buffer.extend_from_slice(&chunk.samples);
                                samples_since_partial = chunk.samples.len();
                            }
                            VadResult::Speaking => {
                                speech_buffer.extend_from_slice(&chunk.samples);
                                samples_since_partial += chunk.samples.len();

                                // Emit partial transcription periodically for live feedback.
                                // Disabled by default for v1 CLI backend — each partial
                                // spawns a fresh whisper-cpp subprocess that cold-loads
                                // the model and starves the final transcription.
                                if enable_partials
                                    && samples_since_partial >= partial_interval
                                    && speech_buffer.len() >= (sample_rate as usize * 3 / 10)
                                {
                                    samples_since_partial = 0;
                                    if !partial_in_flight.swap(true, Ordering::AcqRel) {
                                        let buf_snapshot = speech_buffer.clone();
                                        let stt_partial = stt.clone();
                                        let event_tx_partial = event_tx.clone();
                                        let partial_in_flight_done = partial_in_flight.clone();
                                        let partial_cache = partial_cache.clone();
                                        tokio::spawn(async move {
                                            let partial_result = tokio::time::timeout(
                                                Duration::from_secs(6),
                                                stt_partial.transcribe_samples(&buf_snapshot, sample_rate),
                                            )
                                            .await;

                                            match partial_result {
                                                Ok(Ok(res)) => {
                                                    let text = res.text.trim().to_string();
                                                    if !text.is_empty() && text != "[BLANK_AUDIO]" {
                                                        let now = Instant::now();
                                                        let should_emit = {
                                                            let mut cache = partial_cache.lock().await;
                                                            let elapsed = cache
                                                                .last_emitted_at
                                                                .as_ref()
                                                                .map(|at| now.saturating_duration_since(*at));
                                                            let emit = should_emit_partial(
                                                                &cache.last_text,
                                                                &text,
                                                                elapsed,
                                                            );
                                                            if emit {
                                                                cache.last_text = text.clone();
                                                                cache.last_emitted_at = Some(now);
                                                            }
                                                            emit
                                                        };

                                                        if should_emit {
                                                            let _ = event_tx_partial.send(
                                                                VoicePipelineEvent::PartialTranscript(VoiceTranscriptFrame {
                                                                    text,
                                                                    confidence: clamp01(res.confidence),
                                                                    language: res.language,
                                                                    stability: estimate_partial_stability(
                                                                        buf_snapshot.len(),
                                                                        sample_rate,
                                                                        res.confidence,
                                                                    ),
                                                                })
                                                            );
                                                        }
                                                    }
                                                }
                                                Ok(Err(e)) => {
                                                    tracing::debug!("voice pipeline: partial STT error: {e}");
                                                }
                                                Err(_) => {
                                                    tracing::debug!("voice pipeline: partial STT timed out");
                                                }
                                            }

                                            partial_in_flight_done.store(false, Ordering::Release);
                                        });
                                    }
                                }

                                // Safety limit: max 30 seconds of speech
                                if speech_buffer.len() > sample_rate as usize * 30 {
                                    tracing::warn!("voice pipeline: speech buffer exceeded 30s, forcing end");
                                    vad.reset();
                                    process_speech(
                                        &speech_buffer, sample_rate, &stt,
                                        &state, &state_tx, &event_tx, confidence_threshold,
                                    ).await;
                                    speech_buffer.clear();
                                }
                            }
                            VadResult::SpeechEnd => {
                                speech_buffer.extend_from_slice(&chunk.samples);
                                if !speech_buffer.is_empty() {
                                    process_speech(
                                        &speech_buffer, sample_rate, &stt,
                                        &state, &state_tx, &event_tx, confidence_threshold,
                                    ).await;
                                }
                                speech_buffer.clear();
                            }
                            VadResult::Silence => {}
                        }
                    }
                }
            }

            *state.lock().await = VoicePipelineState::Idle;
            let _ = state_tx.send(VoicePipelineState::Idle);
            let _ = event_tx.send(VoicePipelineEvent::StateChanged(VoicePipelineState::Idle));
        });

        Ok(())
    }

    /// Stop the voice pipeline.
    pub async fn stop(&self) {
        // Signal the capture thread to stop
        self.capture_active.store(false, Ordering::Relaxed);
        if let Some(tx) = self.stop_tx.lock().await.take() {
            let _ = tx.send(()).await;
        }
        self.set_state(VoicePipelineState::Idle).await;
    }

    /// Speak text through TTS + playback.
    pub async fn speak(&self, text: &str) -> anyhow::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        // Mute the microphone immediately so the mic does not pick up the
        // TTS speaker output and transcribe it as user speech (echo).
        self.mic_muted.store(true, Ordering::Relaxed);
        self.set_state(VoicePipelineState::Speaking).await;
        let samples = self.tts.synthesize_samples(text).await?;
        let sr = self.tts.sample_rate();
        self.player.play_samples(samples, sr).await?;
        // Unmute with a 300 ms delay so residual echo in the room and audio
        // driver buffers can decay before we start listening again.
        let muted = self.mic_muted.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            muted.store(false, Ordering::Relaxed);
        });
        // Return to listening if capture is still active, otherwise idle
        if self.capture_active.load(Ordering::Relaxed) {
            self.set_state(VoicePipelineState::Listening).await;
        } else {
            self.set_state(VoicePipelineState::Idle).await;
        }
        Ok(())
    }

    /// Check if the pipeline is currently active (not idle).
    pub async fn is_active(&self) -> bool {
        *self.state.lock().await != VoicePipelineState::Idle
    }
}

/// Process accumulated speech: transcribe via STT and emit transcript event.
async fn process_speech(
    samples: &[f32],
    sample_rate: u32,
    stt: &Arc<SpeechToText>,
    state: &Arc<Mutex<VoicePipelineState>>,
    state_tx: &watch::Sender<VoicePipelineState>,
    event_tx: &mpsc::UnboundedSender<VoicePipelineEvent>,
    confidence_threshold: f32,
) {
    // Minimum speech length: 0.3 seconds
    if samples.len() < (sample_rate as usize * 3 / 10) {
        return;
    }

    *state.lock().await = VoicePipelineState::Processing;
    let _ = state_tx.send(VoicePipelineState::Processing);
    let _ = event_tx.send(VoicePipelineEvent::StateChanged(
        VoicePipelineState::Processing,
    ));

    match tokio::time::timeout(
        Duration::from_secs(60),
        stt.transcribe_samples(samples, sample_rate),
    )
    .await
    {
        Ok(Ok(result)) => {
            let text = result.text.trim().to_string();
            let confidence = clamp01(result.confidence);
            if !text.is_empty() && text != "[BLANK_AUDIO]" {
                if confidence < confidence_threshold {
                    tracing::info!(
                        confidence,
                        threshold = confidence_threshold,
                        "voice pipeline: rejected low-confidence transcript"
                    );
                    let _ = event_tx.send(VoicePipelineEvent::Error(format!(
                        "low-confidence transcript ignored ({confidence:.2} < {confidence_threshold:.2})"
                    )));
                } else {
                    tracing::info!(
                        text = %text,
                        duration_ms = result.duration_ms,
                        confidence,
                        "voice pipeline: transcription"
                    );
                    let _ = event_tx.send(VoicePipelineEvent::Transcript(VoiceTranscriptFrame {
                        text,
                        confidence,
                        language: result.language,
                        stability: 1.0,
                    }));
                }
            }
        }
        Ok(Err(e)) => {
            tracing::warn!("voice pipeline: STT error: {e}");
            let _ = event_tx.send(VoicePipelineEvent::Error(format!("STT error: {e}")));
        }
        Err(_) => {
            tracing::warn!("voice pipeline: STT timed out");
            let _ = event_tx.send(VoicePipelineEvent::Error(
                "STT timed out while processing speech".to_string(),
            ));
        }
    }

    *state.lock().await = VoicePipelineState::Listening;
    let _ = state_tx.send(VoicePipelineState::Listening);
    let _ = event_tx.send(VoicePipelineEvent::StateChanged(
        VoicePipelineState::Listening,
    ));
}

#[derive(Default)]
struct PartialTranscriptCache {
    last_text: String,
    last_emitted_at: Option<Instant>,
}

fn should_emit_partial(last_text: &str, candidate: &str, elapsed: Option<Duration>) -> bool {
    let cooldown_same = Duration::from_millis(1200);
    let cooldown_extension = Duration::from_millis(700);

    let normalized_last = normalize_partial_text(last_text);
    let normalized_candidate = normalize_partial_text(candidate);

    if normalized_candidate == normalized_last {
        return elapsed.map(|d| d >= cooldown_same).unwrap_or(true);
    }

    // Suppress tiny incremental updates (e.g. punctuation or one extra character)
    // when they arrive too quickly.
    if !normalized_last.is_empty()
        && normalized_candidate.starts_with(&normalized_last)
        && normalized_candidate
            .len()
            .saturating_sub(normalized_last.len())
            <= 2
    {
        return elapsed.map(|d| d >= cooldown_extension).unwrap_or(true);
    }

    true
}

fn normalize_partial_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn estimate_partial_stability(sample_count: usize, sample_rate: u32, confidence: f32) -> f32 {
    let seconds = (sample_count as f32 / sample_rate as f32).max(0.0);
    let length_component = (seconds / 10.0).min(0.55);
    let confidence_component = clamp01(confidence) * 0.35;
    clamp01(0.10 + length_component + confidence_component)
}

fn clamp01(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}

fn rms_energy(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples.iter().map(|s| s * s).sum();
    (sum / samples.len() as f32).sqrt()
}
