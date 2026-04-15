use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Audio capture from system microphone using CPAL.
pub struct AudioCapture {
    sample_rate: u32,
    channels: u16,
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
        }
    }

    /// Start capturing audio. Returns a receiver for audio chunks.
    pub fn start(&self) -> anyhow::Result<(mpsc::UnboundedReceiver<AudioChunk>, AudioCaptureHandle)> {
        let host = cpal::default_host();
        let device = host.default_input_device()
            .ok_or_else(|| anyhow::anyhow!("no input device found"))?;

        let config = cpal::StreamConfig {
            channels: self.channels,
            sample_rate: cpal::SampleRate(self.sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        let (tx, rx) = mpsc::unbounded_channel();
        let sample_rate = self.sample_rate;
        let channels = self.channels;

        // Accumulate samples into chunks (~100ms)
        let chunk_size = (sample_rate as usize * channels as usize) / 10;
        let buffer = Arc::new(Mutex::new(Vec::with_capacity(chunk_size)));
        let buf_clone = buffer.clone();
        let tx_clone = tx.clone();

        let stream = device.build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let mut buf = buf_clone.lock().unwrap();
                buf.extend_from_slice(data);
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
                tracing::error!("audio capture error: {}", err);
            },
            None,
        )?;

        stream.play()?;

        Ok((rx, AudioCaptureHandle {
            _stream: stream,
        }))
    }
}

/// Handle to keep the audio stream alive. Drop to stop capture.
pub struct AudioCaptureHandle {
    _stream: cpal::Stream,
}
