use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Audio capture from system microphone using CPAL.
pub struct AudioCapture {
    sample_rate: u32,
    channels: u16,
    preferred_input_device: Option<String>,
    follow_system_default: bool,
    noise_suppression_mode: String,
}

/// A chunk of captured audio samples (f32 PCM).
#[derive(Debug, Clone)]
pub struct AudioChunk {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
}

impl AudioCapture {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            channels: 1,
            preferred_input_device: None,
            follow_system_default: true,
            noise_suppression_mode: "off".to_string(),
        }
    }

    /// Pin capture to a specific input device name.
    /// Use "auto" (or empty) to use the system default device.
    pub fn with_input_device(mut self, device_name: impl Into<String>) -> Self {
        let device_name = device_name.into();
        let trimmed = device_name.trim();
        self.preferred_input_device = if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto")
        {
            None
        } else {
            Some(trimmed.to_string())
        };
        self
    }

    /// Follow system default microphone changes while running.
    pub fn follow_system_default(mut self, follow: bool) -> Self {
        self.follow_system_default = follow;
        self
    }

    /// Configure noise suppression processing mode: off | light | aggressive.
    pub fn with_noise_suppression_mode(mut self, mode: impl Into<String>) -> Self {
        self.noise_suppression_mode = mode.into();
        self
    }

    /// Whether capture is configured to follow system default device changes.
    pub fn is_following_system_default(&self) -> bool {
        self.follow_system_default
    }

    /// Returns true if capture should restart to follow a new system default device.
    pub fn should_restart_for_default_change(&self, active_device_name: &str) -> bool {
        if !self.follow_system_default {
            return false;
        }

        match default_input_device_name() {
            Some(default_name) => default_name != active_device_name,
            None => false,
        }
    }

    /// Start capturing audio. Returns a receiver for audio chunks.
    pub fn start(
        &self,
    ) -> anyhow::Result<(mpsc::UnboundedReceiver<AudioChunk>, AudioCaptureHandle)> {
        let host = cpal::default_host();
        let device = resolve_input_device(
            &host,
            self.preferred_input_device.as_deref(),
            self.follow_system_default,
        )?;
        let device_name = device
            .name()
            .unwrap_or_else(|_| "unknown-input-device".to_string());

        let config = cpal::StreamConfig {
            channels: self.channels,
            sample_rate: cpal::SampleRate(self.sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        let (tx, rx) = mpsc::unbounded_channel();
        let sample_rate = self.sample_rate;
        let channels = self.channels;
        let noise_mode = NoiseSuppressionMode::from_str(&self.noise_suppression_mode);
        let ns_state = Arc::new(Mutex::new(NoiseSuppressionState::default()));

        // Accumulate samples into chunks (~100ms)
        let chunk_size = (sample_rate as usize * channels as usize) / 10;
        let buffer = Arc::new(Mutex::new(Vec::with_capacity(chunk_size)));
        let buf_clone = buffer.clone();
        let tx_clone = tx.clone();
        let ns_state_clone = ns_state.clone();
        let stream_failed = Arc::new(AtomicBool::new(false));
        let stream_failed_cb = stream_failed.clone();

        let stream = device.build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let mut buf = buf_clone.lock().unwrap();
                let mut ns = ns_state_clone.lock().unwrap();
                for &sample in data {
                    buf.push(apply_noise_suppression(
                        sample,
                        sample_rate,
                        noise_mode,
                        &mut ns,
                    ));
                }
                if buf.len() >= chunk_size {
                    let chunk = AudioChunk {
                        samples: buf.drain(..).collect(),
                        sample_rate,
                        channels,
                    };
                    let _ = tx_clone.send(chunk);
                }
            },
            move |err| {
                stream_failed_cb.store(true, Ordering::Relaxed);
                tracing::error!("audio capture error: {}", err);
            },
            None,
        )?;

        stream.play()?;

        tracing::info!(
            device = %device_name,
            mode = ?noise_mode,
            "audio capture started"
        );

        Ok((
            rx,
            AudioCaptureHandle {
                _stream: stream,
                device_name,
                stream_failed,
            },
        ))
    }
}

/// Enumerate available input device names.
pub fn list_input_devices() -> anyhow::Result<Vec<String>> {
    let host = cpal::default_host();
    let mut names = Vec::new();

    if let Ok(devices) = host.input_devices() {
        for device in devices {
            if let Ok(name) = device.name() {
                names.push(name);
            }
        }
    }

    names.sort();
    names.dedup();
    Ok(names)
}

/// Return current system default input device name.
pub fn default_input_device_name() -> Option<String> {
    let host = cpal::default_host();
    host.default_input_device().and_then(|d| d.name().ok())
}

fn resolve_input_device(
    host: &cpal::Host,
    preferred_name: Option<&str>,
    follow_system_default: bool,
) -> anyhow::Result<cpal::Device> {
    if !follow_system_default {
        if let Some(name) = preferred_name {
            let trimmed = name.trim();
            if !trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("auto") {
                if let Ok(devices) = host.input_devices() {
                    for device in devices {
                        if device.name().ok().as_deref() == Some(trimmed) {
                            return Ok(device);
                        }
                    }
                }
                tracing::warn!(
                    device = %trimmed,
                    "requested microphone not found, falling back to system default"
                );
            }
        }
    }

    host.default_input_device()
        .ok_or_else(|| anyhow::anyhow!("no input device found"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoiseSuppressionMode {
    Off,
    Light,
    Aggressive,
}

impl NoiseSuppressionMode {
    fn from_str(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "light" => Self::Light,
            "aggressive" => Self::Aggressive,
            _ => Self::Off,
        }
    }
}

#[derive(Debug, Default)]
struct NoiseSuppressionState {
    prev_input: f32,
    prev_output: f32,
}

fn apply_noise_suppression(
    sample: f32,
    sample_rate: u32,
    mode: NoiseSuppressionMode,
    state: &mut NoiseSuppressionState,
) -> f32 {
    if mode == NoiseSuppressionMode::Off {
        return sample;
    }

    let cutoff_hz = match mode {
        NoiseSuppressionMode::Off => 0.0,
        NoiseSuppressionMode::Light => 70.0,
        NoiseSuppressionMode::Aggressive => 110.0,
    };

    // Single-pole high-pass filter to suppress low-frequency rumble.
    let dt = 1.0 / sample_rate.max(8_000) as f32;
    let rc = 1.0 / (2.0 * std::f32::consts::PI * cutoff_hz);
    let alpha = rc / (rc + dt);
    let hp = alpha * (state.prev_output + sample - state.prev_input);
    state.prev_input = sample;
    state.prev_output = hp;

    let abs_hp = hp.abs();
    // Keep suppression conservative by default: use a small static soft-gate.
    // Dynamic floor estimation can incorrectly chase speech amplitudes and mute voice.
    let threshold = match mode {
        NoiseSuppressionMode::Off => 0.0,
        NoiseSuppressionMode::Light => 0.0018,
        NoiseSuppressionMode::Aggressive => 0.0032,
    };

    if abs_hp <= threshold {
        0.0
    } else {
        // Soft gate: attenuate near-threshold content smoothly.
        let gain = ((abs_hp - threshold) / (abs_hp + 1e-6)).clamp(0.0, 1.0);
        let gain = match mode {
            NoiseSuppressionMode::Aggressive => gain * 0.85,
            _ => gain,
        };
        (hp * gain).clamp(-1.0, 1.0)
    }
}

/// Handle to keep the audio stream alive. Drop to stop capture.
pub struct AudioCaptureHandle {
    _stream: cpal::Stream,
    device_name: String,
    stream_failed: Arc<AtomicBool>,
}

impl AudioCaptureHandle {
    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    pub fn has_failed(&self) -> bool {
        self.stream_failed.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::{apply_noise_suppression, NoiseSuppressionMode, NoiseSuppressionState};

    #[test]
    fn light_noise_suppression_preserves_speech_energy() {
        let mut state = NoiseSuppressionState::default();
        let sample_rate = 16_000u32;

        // Approx 0.5 seconds of speech-like sine at moderate amplitude.
        let input: Vec<f32> = (0..8_000)
            .map(|i| {
                0.05 * ((2.0 * std::f32::consts::PI * 220.0 * i as f32) / sample_rate as f32)
                    .sin()
            })
            .collect();

        let output: Vec<f32> = input
            .iter()
            .map(|&s| {
                apply_noise_suppression(s, sample_rate, NoiseSuppressionMode::Light, &mut state)
            })
            .collect();

        let in_rms = rms(&input);
        let out_rms = rms(&output);

        // Suppression should reduce low-level noise, not eliminate actual speech.
        assert!(in_rms > 0.01);
        assert!(
            out_rms > 0.01,
            "speech was over-suppressed: in_rms={in_rms} out_rms={out_rms}"
        );
        assert!(
            out_rms >= in_rms * 0.30,
            "too much attenuation: in_rms={in_rms} out_rms={out_rms}"
        );
    }

    fn rms(samples: &[f32]) -> f32 {
        let sum: f32 = samples.iter().map(|s| s * s).sum();
        (sum / samples.len() as f32).sqrt()
    }
}
