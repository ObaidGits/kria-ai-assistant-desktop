use cpal::traits::{DeviceTrait, HostTrait};
use rodio::{OutputStream, OutputStreamHandle, Sink};

/// Audio player using rodio for TTS output playback.
pub struct AudioPlayer {
    preferred_output_device: Option<String>,
    follow_system_default: bool,
}

impl AudioPlayer {
    pub fn new() -> Self {
        Self {
            preferred_output_device: None,
            follow_system_default: true,
        }
    }

    /// Prefer a specific output device by name.
    /// Use None or "auto" to use system default.
    pub fn with_output_device(mut self, device_name: Option<String>) -> Self {
        self.preferred_output_device = device_name.and_then(|raw| {
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto") {
                None
            } else {
                Some(trimmed.to_string())
            }
        });
        self
    }

    /// Whether playback should always follow the current system default speaker.
    pub fn follow_system_default(mut self, follow: bool) -> Self {
        self.follow_system_default = follow;
        self
    }

    /// Play WAV file.
    pub async fn play_file(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let path = path.to_path_buf();
        let preferred = self.preferred_output_device.clone();
        let follow_default = self.follow_system_default;
        // rodio is sync, so run in blocking thread
        tokio::task::spawn_blocking(move || {
            let (_stream, handle) = open_output_stream(preferred.as_deref(), follow_default)?;
            let sink = Sink::try_new(&handle)?;
            let file = std::io::BufReader::new(std::fs::File::open(&path)?);
            let source = rodio::Decoder::new(file)?;
            sink.append(source);
            sink.sleep_until_end();
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        Ok(())
    }

    /// Play raw PCM f32 samples.
    pub async fn play_samples(&self, samples: Vec<f32>, sample_rate: u32) -> anyhow::Result<()> {
        let preferred = self.preferred_output_device.clone();
        let follow_default = self.follow_system_default;
        tokio::task::spawn_blocking(move || {
            let (_stream, handle) = open_output_stream(preferred.as_deref(), follow_default)?;
            let sink = Sink::try_new(&handle)?;
            let source = rodio::buffer::SamplesBuffer::new(1, sample_rate, samples);
            sink.append(source);
            sink.sleep_until_end();
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        Ok(())
    }
}

impl Default for AudioPlayer {
    fn default() -> Self {
        Self::new()
    }
}

/// Enumerate available output device names.
pub fn list_output_devices() -> anyhow::Result<Vec<String>> {
    let host = cpal::default_host();
    let mut names = Vec::new();

    if let Ok(devices) = host.output_devices() {
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

/// Return current system default output device name.
pub fn default_output_device_name() -> Option<String> {
    let host = cpal::default_host();
    host.default_output_device().and_then(|d| d.name().ok())
}

fn open_output_stream(
    preferred_device: Option<&str>,
    follow_system_default: bool,
) -> anyhow::Result<(OutputStream, OutputStreamHandle)> {
    if !follow_system_default {
        if let Some(requested) = preferred_device {
            let requested = requested.trim();
            if !requested.is_empty() && !requested.eq_ignore_ascii_case("auto") {
                let host = cpal::default_host();
                if let Ok(devices) = host.output_devices() {
                    for device in devices {
                        if device.name().ok().as_deref() == Some(requested) {
                            tracing::info!(device = %requested, "audio playback using requested output device");
                            return OutputStream::try_from_device(&device).map_err(|e| {
                                anyhow::anyhow!("failed to open output device '{requested}': {e}")
                            });
                        }
                    }
                }
                tracing::warn!(
                    device = %requested,
                    "requested speaker not found, falling back to system default"
                );
            }
        }
    }

    OutputStream::try_default()
        .map_err(|e| anyhow::anyhow!("failed to open default output device: {e}"))
}
