use ort::session::Session;
use ort::value::Tensor;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

use crate::voice::capture::AudioChunk;

/// Voice Activity Detection with Silero VAD ONNX backend and energy-based fallback.
///
/// When a `silero_vad.onnx` model is available, uses neural-network-based
/// speech probability — far more accurate than energy thresholding alone.
/// Falls back to energy-based detection if the model cannot be loaded.
pub struct VoiceActivityDetector {
    energy_threshold: f32,
    speech_threshold: f32,
    min_speech_chunks: usize,
    silence_timeout_chunks: usize,
    speech_chunk_count: usize,
    silence_chunk_count: usize,
    is_speaking: bool,
    /// Silero VAD ONNX session (None = energy-only fallback).
    silero: Option<Arc<StdMutex<SileroState>>>,
}

/// Internal state for Silero VAD ONNX inference.
struct SileroState {
    session: Session,
    /// LSTM hidden state — shape [2, 1, 64].
    h: Vec<f32>,
    /// LSTM cell state — shape [2, 1, 64].
    c: Vec<f32>,
}

impl VoiceActivityDetector {
    /// Create a new VAD with energy-based detection (no ONNX model).
    pub fn new(energy_threshold: f32) -> Self {
        let normalized_threshold = normalize_energy_threshold(energy_threshold);
        if (normalized_threshold - energy_threshold).abs() > f32::EPSILON {
            tracing::info!(
                configured = energy_threshold,
                normalized = normalized_threshold,
                "voice VAD: normalized legacy energy threshold"
            );
        }

        Self {
            energy_threshold: normalized_threshold,
            speech_threshold: 0.5,
            min_speech_chunks: 3,
            silence_timeout_chunks: 10,
            speech_chunk_count: 0,
            silence_chunk_count: 0,
            is_speaking: false,
            silero: None,
        }
    }

    /// Create a VAD with Silero ONNX model loaded from the given path.
    /// Falls back to energy-based detection if the model cannot be loaded.
    pub fn with_silero(energy_threshold: f32, model_path: &PathBuf) -> Self {
        let mut vad = Self::new(energy_threshold);

        if model_path.exists() {
            match Session::builder().and_then(|mut b| b.commit_from_file(model_path)) {
                Ok(session) => {
                    let state = SileroState {
                        session,
                        // Silero VAD v4/v5: state shape [2, 1, 64]
                        h: vec![0.0f32; 2 * 64],
                        c: vec![0.0f32; 2 * 64],
                    };
                    vad.silero = Some(Arc::new(StdMutex::new(state)));
                    tracing::info!(path = %model_path.display(), "Silero VAD model loaded");
                }
                Err(e) => {
                    tracing::warn!(
                        path = %model_path.display(),
                        error = %e,
                        "failed to load Silero VAD, using energy-based fallback"
                    );
                }
            }
        } else {
            tracing::info!(
                path = %model_path.display(),
                "Silero VAD model not found, using energy-based fallback"
            );
        }

        vad
    }

    /// Returns true if using Silero ONNX model rather than energy fallback.
    pub fn is_using_silero(&self) -> bool {
        self.silero.is_some()
    }

    /// Compute speech probability for an audio chunk.
    /// Returns a value between 0.0 (silence) and 1.0 (speech).
    fn speech_probability(&self, chunk: &AudioChunk) -> f32 {
        if let Some(silero) = &self.silero {
            if let Ok(mut state) = silero.lock() {
                return silero_infer(&mut state, &chunk.samples, chunk.sample_rate);
            }
        }
        // Fallback: map energy to a 0..1 range using the threshold as midpoint
        let energy = rms_energy(&chunk.samples);
        let threshold = self.energy_threshold.max(f32::EPSILON);
        if energy > threshold {
            (0.5 + 0.5 * (energy - threshold) / threshold).min(1.0)
        } else {
            (0.5 * energy / threshold).max(0.0)
        }
    }

    /// Process an audio chunk and return the VAD decision.
    pub fn process(&mut self, chunk: &AudioChunk) -> VadResult {
        let prob = self.speech_probability(chunk);
        let is_active = prob > self.speech_threshold;

        if is_active {
            self.speech_chunk_count += 1;
            self.silence_chunk_count = 0;

            if !self.is_speaking && self.speech_chunk_count >= self.min_speech_chunks {
                self.is_speaking = true;
                return VadResult::SpeechStart;
            }
        } else {
            self.silence_chunk_count += 1;

            if self.is_speaking && self.silence_chunk_count >= self.silence_timeout_chunks {
                self.is_speaking = false;
                self.speech_chunk_count = 0;
                return VadResult::SpeechEnd;
            }
        }

        if self.is_speaking {
            VadResult::Speaking
        } else {
            VadResult::Silence
        }
    }

    pub fn is_speaking(&self) -> bool {
        self.is_speaking
    }

    pub fn reset(&mut self) {
        self.speech_chunk_count = 0;
        self.silence_chunk_count = 0;
        self.is_speaking = false;
        // Reset Silero LSTM state
        if let Some(silero) = &self.silero {
            if let Ok(mut state) = silero.lock() {
                state.h.fill(0.0);
                state.c.fill(0.0);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VadResult {
    Silence,
    SpeechStart,
    Speaking,
    SpeechEnd,
}

/// Run Silero VAD inference on a chunk of audio samples.
///
/// The model expects 512-sample windows at 16 kHz. We process in windows
/// and return the maximum speech probability across all windows.
fn silero_infer(state: &mut SileroState, samples: &[f32], sample_rate: u32) -> f32 {
    let window_size = 512;
    let mut max_prob = 0.0f32;

    // Process each 512-sample window
    let chunks: Vec<&[f32]> = samples.chunks(window_size).collect();
    for window in chunks {
        // Pad last window if needed
        let padded: Vec<f32>;
        let input_data = if window.len() < window_size {
            padded = {
                let mut v = window.to_vec();
                v.resize(window_size, 0.0);
                v
            };
            &padded
        } else {
            window
        };

        let prob = silero_infer_window(state, input_data, sample_rate);
        if prob > max_prob {
            max_prob = prob;
        }
    }

    max_prob
}

/// Run Silero VAD inference on a single 512-sample window.
fn silero_infer_window(state: &mut SileroState, samples: &[f32], sample_rate: u32) -> f32 {
    // Input tensors for Silero VAD v4:
    //   "input" : float32 [1, 512]
    //   "sr"    : int64   [1]
    //   "h"     : float32 [2, 1, 64]
    //   "c"     : float32 [2, 1, 64]
    let input = match Tensor::from_array(([1usize, samples.len()], samples.to_vec())) {
        Ok(t) => t,
        Err(e) => {
            tracing::trace!(error = %e, "silero input tensor error");
            return 0.0;
        }
    };

    let sr = match Tensor::from_array(([1usize], vec![sample_rate as i64])) {
        Ok(t) => t,
        Err(e) => {
            tracing::trace!(error = %e, "silero sr tensor error");
            return 0.0;
        }
    };

    let h = match Tensor::from_array(([2usize, 1, 64], state.h.clone())) {
        Ok(t) => t,
        Err(e) => {
            tracing::trace!(error = %e, "silero h tensor error");
            return 0.0;
        }
    };

    let c = match Tensor::from_array(([2usize, 1, 64], state.c.clone())) {
        Ok(t) => t,
        Err(e) => {
            tracing::trace!(error = %e, "silero c tensor error");
            return 0.0;
        }
    };

    let inputs = ort::inputs![
        "input" => input,
        "sr" => sr,
        "h" => h,
        "c" => c,
    ];

    match state.session.run(inputs) {
        Ok(outputs) => {
            // Output: "output" float32 [1,1], "hn" float32 [2,1,64], "cn" float32 [2,1,64]
            let prob = outputs
                .get("output")
                .and_then(|v| v.try_extract_tensor::<f32>().ok())
                .map(|(_shape, data)| if data.is_empty() { 0.0 } else { data[0] })
                .unwrap_or(0.0);

            // Update LSTM hidden states
            if let Some((_shape, data)) = outputs
                .get("hn")
                .and_then(|v| v.try_extract_tensor::<f32>().ok())
            {
                if data.len() == state.h.len() {
                    state.h.copy_from_slice(data);
                }
            }
            if let Some((_shape, data)) = outputs
                .get("cn")
                .and_then(|v| v.try_extract_tensor::<f32>().ok())
            {
                if data.len() == state.c.len() {
                    state.c.copy_from_slice(data);
                }
            }

            prob
        }
        Err(e) => {
            tracing::trace!(error = %e, "silero inference error");
            0.0
        }
    }
}

fn rms_energy(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples.iter().map(|s| s * s).sum();
    (sum / samples.len() as f32).sqrt()
}

fn normalize_energy_threshold(raw: f32) -> f32 {
    if !raw.is_finite() || raw <= 0.0 {
        return 0.02;
    }

    // Backward compatibility: historical configs used int16-style amplitudes
    // (e.g. 2000), but capture now uses normalized float samples in [-1, 1].
    if raw > 1.0 {
        return (raw / 32768.0).clamp(0.005, 0.35);
    }

    raw.clamp(0.0005, 1.0)
}
