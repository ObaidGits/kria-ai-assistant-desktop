use std::path::PathBuf;

/// Speech-to-Text using whisper.cpp via command-line or whisper-rs.
///
/// Production: replace with whisper-rs bindings for in-process inference.
/// Current: shells out to whisper-cpp binary.
pub struct SpeechToText {
    model_path: PathBuf,
    /// whisper.cpp binary path (if using CLI mode).
    binary_path: Option<PathBuf>,
    language: String,
}

/// STT result.
#[derive(Debug, Clone)]
pub struct TranscriptionResult {
    pub text: String,
    pub language: String,
    pub duration_ms: u64,
}

impl SpeechToText {
    pub fn new(model_path: PathBuf, binary_path: Option<PathBuf>) -> Self {
        Self {
            model_path,
            binary_path,
            language: "en".into(),
        }
    }

    pub fn set_language(&mut self, lang: &str) {
        self.language = lang.to_string();
    }

    /// Transcribe a WAV file.
    pub async fn transcribe_file(&self, wav_path: &std::path::Path) -> anyhow::Result<TranscriptionResult> {
        let start = std::time::Instant::now();

        if let Some(ref binary) = self.binary_path {
            // CLI mode: call whisper.cpp binary
            let output = tokio::process::Command::new(binary)
                .args([
                    "-m", &self.model_path.to_string_lossy(),
                    "-l", &self.language,
                    "-f", &wav_path.to_string_lossy(),
                    "--no-timestamps",
                    "-t", "4",
                ])
                .output()
                .await?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("whisper.cpp failed: {stderr}");
            }

            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            Ok(TranscriptionResult {
                text,
                language: self.language.clone(),
                duration_ms: start.elapsed().as_millis() as u64,
            })
        } else {
            anyhow::bail!("whisper-rs bindings not yet implemented; provide binary_path")
        }
    }

    /// Transcribe raw PCM samples (saves to temp WAV first).
    pub async fn transcribe_samples(
        &self,
        samples: &[f32],
        sample_rate: u32,
    ) -> anyhow::Result<TranscriptionResult> {
        let temp_path = std::env::temp_dir().join("kria_stt_input.wav");
        write_wav(&temp_path, samples, sample_rate)?;
        let result = self.transcribe_file(&temp_path).await;
        let _ = std::fs::remove_file(&temp_path);
        result
    }

    pub fn model_path(&self) -> &PathBuf {
        &self.model_path
    }
}

/// Write PCM f32 samples to a WAV file.
fn write_wav(path: &std::path::Path, samples: &[f32], sample_rate: u32) -> anyhow::Result<()> {
    use std::io::Write;

    let num_samples = samples.len() as u32;
    let byte_rate = sample_rate * 2; // 16-bit mono
    let data_size = num_samples * 2;

    let mut file = std::fs::File::create(path)?;

    // WAV header
    file.write_all(b"RIFF")?;
    file.write_all(&(36 + data_size).to_le_bytes())?;
    file.write_all(b"WAVE")?;
    file.write_all(b"fmt ")?;
    file.write_all(&16u32.to_le_bytes())?; // chunk size
    file.write_all(&1u16.to_le_bytes())?;  // PCM
    file.write_all(&1u16.to_le_bytes())?;  // mono
    file.write_all(&sample_rate.to_le_bytes())?;
    file.write_all(&byte_rate.to_le_bytes())?;
    file.write_all(&2u16.to_le_bytes())?;  // block align
    file.write_all(&16u16.to_le_bytes())?; // bits per sample
    file.write_all(b"data")?;
    file.write_all(&data_size.to_le_bytes())?;

    // Convert f32 to i16
    for &sample in samples {
        let clamped = sample.max(-1.0).min(1.0);
        let i16_val = (clamped * 32767.0) as i16;
        file.write_all(&i16_val.to_le_bytes())?;
    }

    Ok(())
}
