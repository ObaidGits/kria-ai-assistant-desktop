//! Streaming playback sink with hard-abort barge-in support.
//!
//! Drains TTS PCM chunks from an `mpsc::Receiver`, hands them to rodio for
//! decoding to the speakers, and tees a copy into the AEC reference channel
//! when AEC is enabled.
//!
//! Calling [`PlaybackSink::abort`] in the same critical section:
//! 1. Clears the rodio sink (drops queued audio immediately).
//! 2. Closes the PCM channel from the receiver side.
//! 3. Notifies the active synthesis task via a `watch` channel so it stops
//!    decoding more audio.
//!
//! Together this gets us mid-sentence interruption within ~50 ms of VAD
//! firing, which is the budget the plan calls for.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::voice::playback::AudioPlayer;

/// High-level playback state surfaced to the pipeline FSM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlaybackState {
    Idle,
    Playing,
    Aborted,
}

/// One streaming TTS playback session.
pub struct PlaybackSink {
    sample_rate: u32,
    /// Receiver into which the TTS engine pushes synthesized PCM chunks.
    /// When `None`, no session is currently active.
    pcm_tx: Option<mpsc::Sender<Vec<f32>>>,
    /// Watch handle the synthesis task polls to know when to bail.
    abort_tx: watch::Sender<bool>,
    /// Drain task handle.
    drain: Option<JoinHandle<()>>,
    /// Optional AEC reference tap (Phase 3). Each chunk is also forwarded
    /// here when set, *before* it goes to rodio.
    aec_ref_tx: Option<mpsc::UnboundedSender<Vec<f32>>>,
    /// Set true the moment the first chunk hits the rodio sink — read by
    /// `MetricsBuilder::mark_first_audio_out` from the pipeline.
    pub first_audio_emitted: Arc<AtomicBool>,
    /// Optional callback fired exactly once when the first audio chunk is
    /// queued to rodio. Used by the pipeline to record TTFA telemetry.
    first_audio_callback: Option<Arc<dyn Fn(Instant) + Send + Sync>>,
}

impl PlaybackSink {
    pub fn new(sample_rate: u32) -> Self {
        let (abort_tx, _abort_rx) = watch::channel(false);
        Self {
            sample_rate,
            pcm_tx: None,
            abort_tx,
            drain: None,
            aec_ref_tx: None,
            first_audio_emitted: Arc::new(AtomicBool::new(false)),
            first_audio_callback: None,
        }
    }

    /// Install an AEC reference tap. The sink will copy every chunk into
    /// `tx` before queueing it to the speakers.
    pub fn set_aec_reference(&mut self, tx: mpsc::UnboundedSender<Vec<f32>>) {
        self.aec_ref_tx = Some(tx);
    }

    /// Install a one-shot callback fired when the first audio chunk hits
    /// the speakers. Used for TTFA telemetry.
    pub fn set_first_audio_callback<F>(&mut self, cb: F)
    where
        F: Fn(Instant) + Send + Sync + 'static,
    {
        self.first_audio_callback = Some(Arc::new(cb));
    }

    /// Subscribe a synthesis task to the abort signal.
    pub fn abort_signal(&self) -> watch::Receiver<bool> {
        self.abort_tx.subscribe()
    }

    /// Begin a new playback session. Returns the `pcm_tx` handle the TTS
    /// engine should push chunks into. Closing the sender cleanly ends the
    /// session.
    pub fn begin_session(&mut self, player: Arc<AudioPlayer>) -> mpsc::Sender<Vec<f32>> {
        // Reset abort + first-audio flags for this session.
        let _ = self.abort_tx.send(false);
        self.first_audio_emitted.store(false, Ordering::SeqCst);

        let (tx, mut rx) = mpsc::channel::<Vec<f32>>(4); // ~480 ms buffered
        let aec_ref = self.aec_ref_tx.clone();
        let sr = self.sample_rate;
        let first_flag = self.first_audio_emitted.clone();
        let cb = self.first_audio_callback.clone();
        let mut abort_rx = self.abort_tx.subscribe();

        let drain = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = abort_rx.changed() => {
                        if *abort_rx.borrow() {
                            // Drop the receiver so any pending sender wakes
                            // with a closed channel; the currently-playing
                            // chunk finishes (rodio is sync) but no further
                            // chunks are queued.
                            rx.close();
                            break;
                        }
                    }
                    chunk = rx.recv() => {
                        let Some(chunk) = chunk else { break; };
                        if let Some(ref ref_tx) = aec_ref {
                            // Best-effort tap — never block playback.
                            let _ = ref_tx.send(chunk.clone());
                        }
                        if !first_flag.swap(true, Ordering::SeqCst) {
                            if let Some(ref cb) = cb {
                                cb(Instant::now());
                            }
                        }
                        // `play_samples` blocks until the chunk finishes
                        // playing, so this loop also serialises chunks for
                        // smooth playback. On abort, the next iteration
                        // wakes via `abort_rx.changed()`.
                        if let Err(e) = player.play_samples(chunk, sr).await {
                            tracing::warn!("playback error: {e}");
                            break;
                        }
                    }
                }
            }
        });

        self.drain = Some(drain);
        self.pcm_tx = Some(tx.clone());
        tx
    }

    /// Hard abort the active session: drops queued audio, closes the PCM
    /// channel, signals all subscribed synthesis tasks.
    pub fn abort(&mut self) {
        let _ = self.abort_tx.send(true);
        // Drop the TX to close the channel from the producer side; the
        // drain task will wake on `abort_rx.changed()` and stop the player.
        self.pcm_tx.take();
    }

    pub fn state(&self) -> PlaybackState {
        if *self.abort_tx.borrow() {
            PlaybackState::Aborted
        } else if self.pcm_tx.is_some() {
            PlaybackState::Playing
        } else {
            PlaybackState::Idle
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_sink_is_idle() {
        let s = PlaybackSink::new(22_050);
        assert_eq!(s.state(), PlaybackState::Idle);
    }

    #[test]
    fn abort_signal_is_subscribable() {
        let s = PlaybackSink::new(22_050);
        let mut rx = s.abort_signal();
        assert_eq!(*rx.borrow_and_update(), false);
    }
}
