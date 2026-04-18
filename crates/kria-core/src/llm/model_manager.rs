use crate::infra::download::{self, DownloadClient, DownloadClientConfig, DownloadProgress};
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

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
                        let name = path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                        models.push(ModelInfo {
                            name,
                            file: path
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into(),
                            size_bytes: size,
                            path,
                        });
                    }
                }
            }
        }
        models
    }

    /// Download a model from a URL using the robust download client.
    ///
    /// Features: resumable, retries with backoff, stream SHA256, disk space check.
    pub async fn download<F>(
        &self,
        url: &str,
        subdir: &str,
        filename: &str,
        expected_sha256: Option<&str>,
        cancel: &CancellationToken,
        on_progress: F,
    ) -> anyhow::Result<PathBuf>
    where
        F: Fn(DownloadProgress) + Send + Sync,
    {
        let target_dir = self.models_dir.join(subdir);
        let client = DownloadClient::new(DownloadClientConfig::default())?;

        let result = client
            .download(
                url,
                &target_dir,
                filename,
                expected_sha256,
                cancel,
                on_progress,
            )
            .await?;

        Ok(result.path)
    }

    /// Verify SHA256 of an existing model file (stream-based, no OOM).
    pub async fn verify_sha256(&self, subdir: &str, filename: &str) -> anyhow::Result<String> {
        let path = self.models_dir.join(subdir).join(filename);
        download::stream_sha256(&path).await
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
