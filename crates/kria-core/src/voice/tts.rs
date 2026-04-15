use std::path::PathBuf;

/// Text-to-Speech using Piper (ONNX voice models).
///
/// Production: use `ort` crate for in-process ONNX inference.
/// Current: shells out to piper binary.
pub struct TextToSpeech {
    model_path: PathBuf,
    config_path: PathBuf,
    /// Piper binary path (if using CLI mode).
    binary_path: Option<PathBuf>,
    sample_rate: u32,
}

impl TextToSpeech {
    pub fn new(model_path: PathBuf, binary_path: Option<PathBuf>) -> Self {
        let config_path = model_path.with_extension("onnx.json");
        Self {
            model_path,
            config_path,
            binary_path,
            sample_rate: 22050,
        }
    }

    /// Synthesize speech from text, returning WAV file path.
    pub async fn synthesize(&self, text: &str) -> anyhow::Result<PathBuf> {
        let output_path = std::env::temp_dir().join("kria_tts_output.wav");

        if let Some(ref binary) = self.binary_path {
            let mut child = tokio::process::Command::new(binary)
                .args([
                    "--model", &self.model_path.to_string_lossy(),
                    "--config", &self.config_path.to_string_lossy(),
                    "--output_file", &output_path.to_string_lossy(),
                ])
                .stdin(std::process::Stdio::piped())
                .spawn()?;

            if let Some(mut stdin) = child.stdin.take() {
                use tokio::io::AsyncWriteExt;
                stdin.write_all(text.as_bytes()).await?;
            }

            let status = child.wait().await?;
            if !status.success() {
                anyhow::bail!("piper TTS failed");
            }

            Ok(output_path)
        } else {
            anyhow::bail!("piper-rs bindings not yet implemented; provide binary_path")
        }
    }

    /// Synthesize and return raw PCM samples (f32).
    pub async fn synthesize_samples(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let wav_path = self.synthesize(text).await?;
        let data = std::fs::read(&wav_path)?;
        let _ = std::fs::remove_file(&wav_path);

        // Skip WAV header (44 bytes) and convert i16 to f32
        if data.len() < 44 {
            anyhow::bail!("invalid WAV file");
        }

        let samples: Vec<f32> = data[44..]
            .chunks_exact(2)
            .map(|chunk| {
                let sample = i16::from_le_bytes([chunk[0], chunk[1]]);
                sample as f32 / 32768.0
            })
            .collect();

        Ok(samples)
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}
