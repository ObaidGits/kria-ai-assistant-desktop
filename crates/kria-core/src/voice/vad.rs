use crate::voice::capture::AudioChunk;

/// Voice Activity Detection using energy-based detection.
///
/// A production implementation should use Silero VAD (ONNX via `ort` crate).
/// This implementation uses simple energy thresholding as a starting point.
pub struct VoiceActivityDetector {
    energy_threshold: f32,
    /// Minimum speech duration in chunks to trigger (debounce).
    min_speech_chunks: usize,
    /// Number of silent chunks before considering speech ended.
    silence_timeout_chunks: usize,
    speech_chunk_count: usize,
    silence_chunk_count: usize,
    is_speaking: bool,
}

impl VoiceActivityDetector {
    pub fn new(energy_threshold: f32) -> Self {
        Self {
            energy_threshold,
            min_speech_chunks: 3,
            silence_timeout_chunks: 10,
            speech_chunk_count: 0,
            silence_chunk_count: 0,
            is_speaking: false,
        }
    }

    /// Process an audio chunk and return whether speech is occurring.
    pub fn process(&mut self, chunk: &AudioChunk) -> VadResult {
        let energy = rms_energy(&chunk.samples);
        let is_active = energy > self.energy_threshold;

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
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VadResult {
    Silence,
    SpeechStart,
    Speaking,
    SpeechEnd,
}

fn rms_energy(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples.iter().map(|s| s * s).sum();
    (sum / samples.len() as f32).sqrt()
}
