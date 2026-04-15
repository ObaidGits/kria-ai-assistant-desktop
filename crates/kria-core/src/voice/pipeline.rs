use std::sync::Arc;
use tokio::sync::{mpsc, watch, Mutex};

use crate::voice::capture::AudioCapture;
use crate::voice::vad::{VoiceActivityDetector, VadResult};
use crate::voice::stt::SpeechToText;
use crate::voice::tts::TextToSpeech;
use crate::voice::playback::AudioPlayer;
use crate::config::VoiceConfig;

/// Pipeline states visible to the frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VoicePipelineState {
    Idle,
    Listening,
    Processing,
    Speaking,
}

/// Events emitted by the voice pipeline.
#[derive(Debug, Clone)]
pub enum VoicePipelineEvent {
    /// State changed.
    StateChanged(VoicePipelineState),
    /// Live partial transcription (streamed while user is still speaking).
    PartialTranscript(String),
    /// Final STT transcription available (speech segment complete).
    Transcript(String),
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
    capture_active: Arc<std::sync::atomic::AtomicBool>,
    /// Optional path to Silero VAD ONNX model.
    vad_model_path: Option<std::path::PathBuf>,
}

impl VoicePipeline {
    pub fn new(
        config: VoiceConfig,
        stt: SpeechToText,
        tts: TextToSpeech,
    ) -> Self {
        let (state_tx, state_rx) = watch::channel(VoicePipelineState::Idle);
        Self {
            config,
            stt: Arc::new(stt),
            tts: Arc::new(tts),
            player: Arc::new(AudioPlayer::new()),
            state: Arc::new(Mutex::new(VoicePipelineState::Idle)),
            state_tx,
            state_rx,
            stop_tx: Arc::new(Mutex::new(None)),
            capture_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
        let capture = AudioCapture::new(sample_rate);

        // Start capture on a dedicated std thread (cpal::Stream is !Send).
        // The audio chunks are sent via an mpsc channel to the async VAD→STT task.
        let (chunk_tx, chunk_rx) = mpsc::unbounded_channel();
        let capture_active = self.capture_active.clone();
        std::thread::spawn(move || {
            let (mut audio_rx, _handle) = match capture.start() {
                Ok(pair) => pair,
                Err(e) => {
                    tracing::error!("audio capture start failed: {e}");
                    return;
                }
            };
            // Block on the std thread, forwarding chunks to the async side.
            // When capture_active is false or the receiver is dropped, we exit.
            while capture_active.load(std::sync::atomic::Ordering::Relaxed) {
                match audio_rx.try_recv() {
                    Ok(chunk) => {
                        if chunk_tx.send(chunk).is_err() {
                            break;
                        }
                    }
                    Err(mpsc::error::TryRecvError::Empty) => {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(mpsc::error::TryRecvError::Disconnected) => break,
                }
            }
            tracing::info!("audio capture thread exiting");
        });
        self.capture_active.store(true, std::sync::atomic::Ordering::Relaxed);

        let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
        *self.stop_tx.lock().await = Some(stop_tx);

        self.set_state(VoicePipelineState::Listening).await;
        let _ = event_tx.send(VoicePipelineEvent::StateChanged(VoicePipelineState::Listening));

        let stt = self.stt.clone();
        let state = self.state.clone();
        let state_tx = self.state_tx.clone();
        let energy_threshold = self.config.energy_threshold;
        let vad_model_path = self.vad_model_path.clone();

        // Spawn the VAD→STT processing loop (async, receives chunks from capture thread)
        tokio::spawn(async move {
            let mut vad = match vad_model_path {
                Some(ref path) => VoiceActivityDetector::with_silero(energy_threshold, path),
                None => VoiceActivityDetector::new(energy_threshold),
            };
            let mut speech_buffer: Vec<f32> = Vec::new();
            let mut chunk_rx = chunk_rx;
            // Track samples since last partial transcription for live feedback
            let partial_interval = sample_rate as usize * 2; // every ~2 seconds
            let mut samples_since_partial: usize = 0;

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

                        let result = vad.process(&chunk);
                        match result {
                            VadResult::SpeechStart => {
                                speech_buffer.clear();
                                speech_buffer.extend_from_slice(&chunk.samples);
                                samples_since_partial = chunk.samples.len();
                            }
                            VadResult::Speaking => {
                                speech_buffer.extend_from_slice(&chunk.samples);
                                samples_since_partial += chunk.samples.len();

                                // Emit partial transcription periodically for live feedback
                                if samples_since_partial >= partial_interval
                                    && speech_buffer.len() >= (sample_rate as usize * 3 / 10)
                                {
                                    samples_since_partial = 0;
                                    let buf_snapshot = speech_buffer.clone();
                                    let stt_partial = stt.clone();
                                    let event_tx_partial = event_tx.clone();
                                    tokio::spawn(async move {
                                        if let Ok(res) = stt_partial.transcribe_samples(&buf_snapshot, sample_rate).await {
                                            let text = res.text.trim().to_string();
                                            if !text.is_empty() && text != "[BLANK_AUDIO]" {
                                                let _ = event_tx_partial.send(VoicePipelineEvent::PartialTranscript(text));
                                            }
                                        }
                                    });
                                }

                                // Safety limit: max 30 seconds of speech
                                if speech_buffer.len() > sample_rate as usize * 30 {
                                    tracing::warn!("voice pipeline: speech buffer exceeded 30s, forcing end");
                                    vad.reset();
                                    process_speech(
                                        &speech_buffer, sample_rate, &stt,
                                        &state, &state_tx, &event_tx,
                                    ).await;
                                    speech_buffer.clear();
                                }
                            }
                            VadResult::SpeechEnd => {
                                speech_buffer.extend_from_slice(&chunk.samples);
                                if !speech_buffer.is_empty() {
                                    process_speech(
                                        &speech_buffer, sample_rate, &stt,
                                        &state, &state_tx, &event_tx,
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
        self.capture_active.store(false, std::sync::atomic::Ordering::Relaxed);
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
        self.set_state(VoicePipelineState::Speaking).await;
        let samples = self.tts.synthesize_samples(text).await?;
        let sr = self.tts.sample_rate();
        self.player.play_samples(samples, sr).await?;
        // Return to listening if capture is still active, otherwise idle
        if self.capture_active.load(std::sync::atomic::Ordering::Relaxed) {
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
) {
    // Minimum speech length: 0.3 seconds
    if samples.len() < (sample_rate as usize * 3 / 10) {
        return;
    }

    *state.lock().await = VoicePipelineState::Processing;
    let _ = state_tx.send(VoicePipelineState::Processing);
    let _ = event_tx.send(VoicePipelineEvent::StateChanged(VoicePipelineState::Processing));

    match stt.transcribe_samples(samples, sample_rate).await {
        Ok(result) => {
            let text = result.text.trim().to_string();
            if !text.is_empty() && text != "[BLANK_AUDIO]" {
                tracing::info!(
                    text = %text,
                    duration_ms = result.duration_ms,
                    "voice pipeline: transcription"
                );
                let _ = event_tx.send(VoicePipelineEvent::Transcript(text));
            }
        }
        Err(e) => {
            tracing::warn!("voice pipeline: STT error: {e}");
            let _ = event_tx.send(VoicePipelineEvent::Error(format!("STT error: {e}")));
        }
    }

    *state.lock().await = VoicePipelineState::Listening;
    let _ = state_tx.send(VoicePipelineState::Listening);
    let _ = event_tx.send(VoicePipelineEvent::StateChanged(VoicePipelineState::Listening));
}
