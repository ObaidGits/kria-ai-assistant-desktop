use std::path::{Path, PathBuf};
use sha2::{Sha256, Digest};
use tokio::io::AsyncWriteExt;

/// Model file metadata.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelInfo {
    pub name: String,
    pub file: String,
    pub size_bytes: u64,
    pub path: PathBuf,
}

/// Manages model files: list, download, verify, delete.
pub struct ModelManager {
    models_dir: PathBuf,
}

impl ModelManager {
    pub fn new(models_dir: PathBuf) -> Self {
        Self { models_dir }
    }

    /// List all GGUF model files in the LLM directory.
    pub fn list_llm_models(&self) -> Vec<ModelInfo> {
        let llm_dir = self.models_dir.join("llm");
        Self::scan_dir(&llm_dir, &["gguf"])
    }

    /// List all STT model files.
    pub fn list_stt_models(&self) -> Vec<ModelInfo> {
        let stt_dir = self.models_dir.join("stt");
        Self::scan_dir(&stt_dir, &["bin", "gguf"])
    }

    /// List all TTS voice files.
    pub fn list_tts_voices(&self) -> Vec<ModelInfo> {
        let tts_dir = self.models_dir.join("tts");
        Self::scan_dir(&tts_dir, &["onnx"])
    }

    fn scan_dir(dir: &Path, extensions: &[&str]) -> Vec<ModelInfo> {
        let mut models = Vec::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if extensions.contains(&ext) {
                        let name = path.file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                        models.push(ModelInfo {
                            name,
                            file: path.file_name().unwrap_or_default().to_string_lossy().into(),
                            size_bytes: size,
                            path,
                        });
                    }
                }
            }
        }
        models
    }

    /// Download a model from a URL (resumable).
    pub async fn download(
        &self,
        url: &str,
        subdir: &str,
        filename: &str,
        expected_sha256: Option<&str>,
    ) -> anyhow::Result<PathBuf> {
        let target_dir = self.models_dir.join(subdir);
        let _ = std::fs::create_dir_all(&target_dir);
        let target_path = target_dir.join(filename);

        // Resume support
        let existing_size = if target_path.exists() {
            std::fs::metadata(&target_path)?.len()
        } else {
            0
        };

        let client = reqwest::Client::new();
        let mut request = client.get(url);
        if existing_size > 0 {
            request = request.header("Range", format!("bytes={}-", existing_size));
        }

        let resp = request.send().await?.error_for_status()?;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&target_path)
            .await?;

        let mut stream = resp.bytes_stream();
        use futures::StreamExt;
        while let Some(chunk) = stream.next().await {
            let bytes = chunk?;
            file.write_all(&bytes).await?;
        }
        file.flush().await?;

        // SHA256 verification
        if let Some(expected) = expected_sha256 {
            let actual = sha256_file(&target_path)?;
            if actual != expected {
                std::fs::remove_file(&target_path)?;
                anyhow::bail!("SHA256 mismatch for {filename}: expected {expected}, got {actual}");
            }
        }

        tracing::info!(file = %target_path.display(), "model downloaded");
        Ok(target_path)
    }

    /// Delete a model file.
    pub fn delete(&self, subdir: &str, filename: &str) -> anyhow::Result<()> {
        let path = self.models_dir.join(subdir).join(filename);
        if path.exists() {
            std::fs::remove_file(&path)?;
            tracing::info!(file = %path.display(), "model deleted");
        }
        Ok(())
    }
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let data = std::fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&data);
    Ok(format!("{:x}", hasher.finalize()))
}
