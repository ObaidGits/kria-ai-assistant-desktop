//! Robust HTTP download client for large file downloads (models, binaries).
//!
//! Features:
//! - Resumable downloads via HTTP Range headers (handles 200 vs 206 correctly)
//! - Exponential backoff with jitter (5 retries)
//! - Stream-based SHA256 verification (no OOM on multi-GB files)
//! - Disk space pre-check before download
//! - Cancellation via `CancellationToken`
//! - Progress callbacks
//! - HuggingFace-compatible redirect handling

use futures::StreamExt;
use rand::Rng;
use reqwest::redirect::Policy;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

/// Progress information emitted during a download.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DownloadProgress {
    pub file: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub speed_bps: u64,
}

/// Result of a completed download.
#[derive(Debug)]
pub struct DownloadResult {
    pub path: PathBuf,
    pub size: u64,
    pub sha256: String,
}

/// Configuration for the download client.
#[derive(Debug, Clone)]
pub struct DownloadClientConfig {
    /// Optional proxy URL (overrides HTTP_PROXY env var).
    pub proxy_url: Option<String>,
    /// Optional HuggingFace API token.
    pub hf_token: Option<String>,
    /// Maximum number of retry attempts.
    pub max_retries: u32,
}

impl Default for DownloadClientConfig {
    fn default() -> Self {
        Self {
            proxy_url: None,
            hf_token: None,
            max_retries: 5,
        }
    }
}

/// Robust download client for large file transfers.
pub struct DownloadClient {
    client: reqwest::Client,
    config: DownloadClientConfig,
}

impl DownloadClient {
    /// Build a new client with production-grade settings.
    pub fn new(config: DownloadClientConfig) -> anyhow::Result<Self> {
        let mut builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(600))
            .redirect(Policy::limited(10))
            .user_agent("kria/0.1");

        if let Some(ref proxy_url) = config.proxy_url {
            builder = builder.proxy(reqwest::Proxy::all(proxy_url)?);
        }

        let client = builder.build()?;
        Ok(Self { client, config })
    }

    /// Download a file with resume support, retries, and progress reporting.
    ///
    /// - `url`: Source URL (supports HuggingFace CDN redirects)
    /// - `dest_dir`: Directory to save the file in
    /// - `filename`: Target filename
    /// - `expected_sha256`: Optional SHA256 hex digest for verification
    /// - `cancel`: Cancellation token (checked between chunk writes)
    /// - `on_progress`: Called with progress updates (~every 256KB)
    pub async fn download<F>(
        &self,
        url: &str,
        dest_dir: &Path,
        filename: &str,
        expected_sha256: Option<&str>,
        cancel: &CancellationToken,
        on_progress: F,
    ) -> anyhow::Result<DownloadResult>
    where
        F: Fn(DownloadProgress) + Send + Sync,
    {
        std::fs::create_dir_all(dest_dir)?;
        let target_path = dest_dir.join(filename);
        let temp_path = dest_dir.join(format!("{filename}.part"));

        // ── Disk space pre-check ──
        // Do a HEAD request first to learn the total size
        let total_size = self.query_content_length(url).await.unwrap_or(0);
        if total_size > 0 {
            check_disk_space(dest_dir, total_size)?;
        }

        // ── Retry loop with exponential backoff ──
        let mut last_err = anyhow::anyhow!("download not attempted");
        for attempt in 0..=self.config.max_retries {
            if cancel.is_cancelled() {
                anyhow::bail!("download cancelled");
            }

            if attempt > 0 {
                let base_delay_ms = 2000u64 * (1u64 << (attempt - 1).min(5));
                let jitter = rand::thread_rng().gen_range(0..=(base_delay_ms / 4));
                let delay = Duration::from_millis(base_delay_ms + jitter);
                tracing::info!(
                    attempt,
                    delay_ms = delay.as_millis() as u64,
                    "download: retrying after backoff"
                );
                tokio::time::sleep(delay).await;
            }

            match self
                .download_attempt(
                    url,
                    &target_path,
                    &temp_path,
                    total_size,
                    cancel,
                    &on_progress,
                )
                .await
            {
                Ok(size) => {
                    // ── SHA256 verification (stream-based) ──
                    let hash = stream_sha256(&temp_path).await?;
                    if let Some(expected) = expected_sha256 {
                        if hash != expected {
                            let _ = tokio::fs::remove_file(&temp_path).await;
                            anyhow::bail!(
                                "SHA256 mismatch for {filename}: expected {expected}, got {hash}"
                            );
                        }
                    }

                    // Atomic rename: .part → final
                    tokio::fs::rename(&temp_path, &target_path).await?;

                    tracing::info!(file = %target_path.display(), size, "download complete");
                    return Ok(DownloadResult {
                        path: target_path,
                        size,
                        sha256: hash,
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        attempt,
                        max = self.config.max_retries,
                        error = %e,
                        "download attempt failed"
                    );
                    last_err = e;
                }
            }
        }

        Err(last_err.context(format!(
            "download failed after {} retries",
            self.config.max_retries
        )))
    }

    /// Single download attempt with resume support.
    async fn download_attempt<F>(
        &self,
        url: &str,
        _target_path: &Path,
        temp_path: &Path,
        total_size: u64,
        cancel: &CancellationToken,
        on_progress: &F,
    ) -> anyhow::Result<u64>
    where
        F: Fn(DownloadProgress) + Send + Sync,
    {
        let filename = temp_path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();

        // Check existing partial download size
        let existing_size = if temp_path.exists() {
            tokio::fs::metadata(temp_path).await?.len()
        } else {
            0
        };

        let mut request = self.client.get(url);

        // Add HF auth token if available
        if let Some(ref token) = self.config.hf_token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }

        // Request resume from existing offset
        if existing_size > 0 {
            request = request.header("Range", format!("bytes={existing_size}-"));
        }

        let resp = request.send().await?.error_for_status()?;
        let status = resp.status();

        // ── Handle 200 vs 206 for resume correctness ──
        // If we requested a Range but got 200 (not 206), the server ignored our
        // Range header. We must truncate and restart from byte 0.
        let resume_offset = if existing_size > 0 && status == reqwest::StatusCode::OK {
            tracing::info!("server returned 200 (ignored Range header); restarting download");
            // Truncate the partial file
            tokio::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .create(true)
                .open(temp_path)
                .await?;
            0
        } else {
            existing_size
        };

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(temp_path)
            .await?;

        let mut stream = resp.bytes_stream();
        let mut downloaded = resume_offset;
        let start_time = std::time::Instant::now();
        let mut last_progress_bytes = downloaded;
        const PROGRESS_INTERVAL: u64 = 256 * 1024; // Report every 256KB

        while let Some(chunk) = stream.next().await {
            if cancel.is_cancelled() {
                file.flush().await?;
                anyhow::bail!("download cancelled");
            }

            let bytes = chunk?;
            file.write_all(&bytes).await?;
            downloaded += bytes.len() as u64;

            // Emit progress
            if downloaded - last_progress_bytes >= PROGRESS_INTERVAL {
                let elapsed = start_time.elapsed().as_secs_f64().max(0.001);
                let speed = ((downloaded - resume_offset) as f64 / elapsed) as u64;
                on_progress(DownloadProgress {
                    file: filename.clone(),
                    downloaded_bytes: downloaded,
                    total_bytes: total_size,
                    speed_bps: speed,
                });
                last_progress_bytes = downloaded;
            }
        }

        file.flush().await?;

        // Final progress
        let elapsed = start_time.elapsed().as_secs_f64().max(0.001);
        let speed = ((downloaded - resume_offset) as f64 / elapsed) as u64;
        on_progress(DownloadProgress {
            file: filename,
            downloaded_bytes: downloaded,
            total_bytes: total_size,
            speed_bps: speed,
        });

        Ok(downloaded)
    }

    /// Query content length via HEAD request.
    async fn query_content_length(&self, url: &str) -> anyhow::Result<u64> {
        let mut request = self.client.head(url);
        if let Some(ref token) = self.config.hf_token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let resp = request.send().await?.error_for_status()?;
        let len = resp
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        Ok(len)
    }
}

/// Retryable status codes.
#[allow(dead_code)]
fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    matches!(
        status.as_u16(),
        429 | 500 | 502 | 503 | 504
    )
}

/// Check if enough disk space is available.
fn check_disk_space(dir: &Path, needed_bytes: u64) -> anyhow::Result<()> {
    // Use fs4 if available, otherwise fall back to statvfs on Unix
    let available = available_space(dir);
    if let Some(avail) = available {
        let margin = needed_bytes + (needed_bytes / 10); // 10% margin
        if avail < margin {
            let need_gb = margin as f64 / 1_073_741_824.0;
            let have_gb = avail as f64 / 1_073_741_824.0;
            anyhow::bail!(
                "Insufficient disk space: need {need_gb:.1} GB, only {have_gb:.1} GB available. \
                 Free up space or choose a lighter hardware tier."
            );
        }
    }
    Ok(())
}

/// Get available disk space for a path by shelling out to `df` (Unix) or skipping (Windows).
fn available_space(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        let output = std::process::Command::new("df")
            .arg("--output=avail")
            .arg("-B1")
            .arg(path)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        // Second line contains the number
        text.lines().nth(1)?.trim().parse::<u64>().ok()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Stream-based SHA256 hash of a file (64KB buffer — no OOM on multi-GB files).
pub async fn stream_sha256(path: &Path) -> anyhow::Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_retryable_status() {
        assert!(is_retryable_status(reqwest::StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retryable_status(reqwest::StatusCode::BAD_GATEWAY));
        assert!(!is_retryable_status(reqwest::StatusCode::NOT_FOUND));
        assert!(!is_retryable_status(reqwest::StatusCode::FORBIDDEN));
    }

    #[tokio::test]
    async fn test_stream_sha256_small_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        tokio::fs::write(&path, b"hello world").await.unwrap();
        let hash = stream_sha256(&path).await.unwrap();
        // SHA256("hello world") = b94d27...
        assert_eq!(
            hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }
}
