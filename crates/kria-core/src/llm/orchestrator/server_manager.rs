//! LlamaServerManager — manages the llama-server process lifecycle.
//!
//! Key design decisions:
//! - AtomicU8 for lock-free server state (V7: no RwLock deadlock)
//! - CancellationToken for non-blocking stream abort (V13)
//! - Ephemeral ports via --port 0 + stderr parsing (V5, V14)
//! - Circuit breaker reset after successful swap (V4)

use crate::config::OrchestratorConfig;
use crate::infra::event_bus::EventBus;
use crate::platform::os;
use std::process::Stdio;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Server states (stored in AtomicU8).
pub const STATE_STOPPED: u8 = 0;
pub const STATE_STARTING: u8 = 1;
pub const STATE_READY: u8 = 2;
pub const STATE_SWAPPING: u8 = 3;
pub const STATE_ERROR: u8 = 4;

/// Manages a single llama-server process with atomic state tracking.
pub struct LlamaServerManager {
    config: OrchestratorConfig,
    model_path: String,
    mmproj_path: Option<String>,
    /// Lock-free server state — readable from any task without blocking.
    state: AtomicU8,
    /// Current GPU layers.
    current_ngl: AtomicU32,
    /// Current context window.
    current_ctx: AtomicU32,
    /// The actual API URL (updated after port discovery).
    api_url: tokio::sync::RwLock<String>,
    /// The child process handle.
    child: Mutex<Option<Child>>,
    /// CancellationToken — cancelled during swap to abort in-flight streams.
    cancel_token: CancellationToken,
    /// Token for the stderr reader task.
    reader_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl LlamaServerManager {
    pub fn new(
        config: OrchestratorConfig,
        model_path: String,
        mmproj_path: Option<String>,
    ) -> Self {
        Self {
            config,
            model_path,
            mmproj_path,
            state: AtomicU8::new(STATE_STOPPED),
            current_ngl: AtomicU32::new(0),
            current_ctx: AtomicU32::new(0),
            api_url: tokio::sync::RwLock::new(String::new()),
            child: Mutex::new(None),
            cancel_token: CancellationToken::new(),
            reader_handle: Mutex::new(None),
        }
    }

    /// Current server state (lock-free read).
    pub fn state(&self) -> u8 {
        self.state.load(Ordering::Acquire)
    }

    /// Whether the server is ready to accept requests.
    pub fn is_healthy(&self) -> bool {
        self.state() == STATE_READY
    }

    /// Whether a swap is in progress (streams should be cancelled).
    pub fn is_swapping(&self) -> bool {
        self.state() == STATE_SWAPPING
    }

    /// Current (ngl, context) parameters.
    pub fn current_params(&self) -> (u32, u32) {
        (
            self.current_ngl.load(Ordering::Acquire),
            self.current_ctx.load(Ordering::Acquire),
        )
    }

    /// Get the current API URL.
    pub fn api_url(&self) -> String {
        // Use try_read to avoid blocking; fall back to empty if locked
        self.api_url
            .try_read()
            .map(|u| u.clone())
            .unwrap_or_default()
    }

    /// Get a CancellationToken that streams should select! on.
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    /// Cancel all in-flight streams (non-blocking).
    pub fn cancel_streams(&self) {
        self.cancel_token.cancel();
    }

    /// Spawn a new llama-server with the given parameters.
    ///
    /// - Uses `--port 0` for ephemeral port assignment
    /// - Parses stderr for the actual port
    /// - Waits for /health to report ready
    pub async fn spawn(
        &self,
        ngl: u32,
        context: u32,
        enable_vision: bool,
        _event_bus: Arc<EventBus>,
    ) -> anyhow::Result<()> {
        self.state.store(STATE_STARTING, Ordering::Release);

        // Resolve binary: check ~/.kria/bin/ first, then config path (with .exe on Windows)
        let binary = os::resolve_binary("llama-server", &self.config.llama_server_binary);

        // Build llama-server command
        let mut cmd = tokio::process::Command::new(&binary);
        cmd.arg("--model").arg(&self.model_path);
        cmd.arg("--port").arg("0"); // Ephemeral port (V5)
        cmd.arg("--ctx-size").arg(context.to_string());
        cmd.arg("--n-gpu-layers").arg(ngl.to_string());
        cmd.arg("--batch-size")
            .arg(self.config.batch_size.to_string());

        if self.config.flash_attention {
            cmd.arg("--flash-attn").arg("on");
        }
        if self.config.mlock {
            cmd.arg("--mlock");
        }

        // Vision projector (mmproj)
        if enable_vision {
            if let Some(ref mmproj) = self.mmproj_path {
                cmd.arg("--mmproj").arg(mmproj);
            }
        }

        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);

        tracing::info!(
            binary = %binary,
            ngl,
            ctx = context,
            vision = enable_vision,
            "server_manager: spawning llama-server"
        );

        let mut child = cmd.spawn().map_err(|e| {
            self.state.store(STATE_ERROR, Ordering::Release);
            anyhow::anyhow!("failed to spawn llama-server: {}", e)
        })?;

        // Parse stderr for port discovery and log forwarding
        let stderr = child.stderr.take();
        let port = self.discover_port(stderr).await?;

        let url = format!("http://127.0.0.1:{}/v1", port);
        tracing::info!(port, url = %url, "server_manager: discovered ephemeral port");

        // Update API URL
        {
            let mut lock = self.api_url.write().await;
            *lock = url.clone();
        }

        // Store the child
        {
            let mut lock = self.child.lock().await;
            *lock = Some(child);
        }

        // Wait for the health endpoint to report ready
        self.wait_for_health(&url).await?;

        // Update state and params atomically
        self.current_ngl.store(ngl, Ordering::Release);
        self.current_ctx.store(context, Ordering::Release);
        self.state.store(STATE_READY, Ordering::Release);

        tracing::info!(
            ngl,
            ctx = context,
            port,
            "server_manager: llama-server is ready"
        );

        Ok(())
    }

    /// Discover the ephemeral port from llama-server's stderr output.
    /// llama-server prints something like: "main: server is listening on http://127.0.0.1:PORT"
    async fn discover_port(
        &self,
        stderr: Option<tokio::process::ChildStderr>,
    ) -> anyhow::Result<u16> {
        let stderr = stderr.ok_or_else(|| anyhow::anyhow!("no stderr from llama-server"))?;

        let mut reader = BufReader::new(stderr).lines();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);

        while let Ok(result) = tokio::time::timeout_at(deadline, reader.next_line()).await {
            match result {
                Ok(Some(line)) => {
                    tracing::debug!(target: "llama-server", "{}", line);

                    // llama-server stderr format varies but port is in:
                    // "main: server is listening on http://127.0.0.1:<PORT>"
                    // or "main: server is listening on 127.0.0.1:<PORT>"
                    if let Some(port) = Self::extract_port_from_line(&line) {
                        // Spawn a background task to keep reading stderr for logging
                        let handle = tokio::spawn(async move {
                            let mut lines = reader;
                            while let Ok(Some(line)) = lines.next_line().await {
                                tracing::debug!(target: "llama-server", "{}", line);
                            }
                        });
                        *self.reader_handle.lock().await = Some(handle);
                        return Ok(port);
                    }
                }
                Ok(None) => {
                    break;
                }
                Err(e) => {
                    return Err(anyhow::anyhow!("error reading llama-server stderr: {}", e));
                }
            }
        }

        Err(anyhow::anyhow!(
            "timed out waiting for llama-server to report listening port"
        ))
    }

    /// Extract port number from a llama-server log line.
    fn extract_port_from_line(line: &str) -> Option<u16> {
        // Match patterns like "127.0.0.1:8080" or "0.0.0.0:12345"
        // The port appears after the last colon in the address
        if !line.contains("listening") {
            return None;
        }

        // Find the port number — look for :PORT pattern
        for segment in line.rsplit(':') {
            // Port might be followed by other chars like quotes, spaces
            let cleaned: String = segment.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(port) = cleaned.parse::<u16>() {
                if port > 0 {
                    return Some(port);
                }
            }
            break; // Only check the last segment after ':'
        }
        None
    }

    /// Wait for /health to return 200.
    async fn wait_for_health(&self, api_url: &str) -> anyhow::Result<()> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()?;

        let health_url = format!("{}", api_url.replace("/v1", "/health"));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(120);

        loop {
            if tokio::time::Instant::now() > deadline {
                self.state.store(STATE_ERROR, Ordering::Release);
                return Err(anyhow::anyhow!(
                    "llama-server health check timed out after 120s"
                ));
            }

            match client.get(&health_url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    return Ok(());
                }
                Ok(resp) => {
                    tracing::debug!(
                        status = %resp.status(),
                        "server_manager: health check not ready yet"
                    );
                }
                Err(e) => {
                    tracing::debug!(?e, "server_manager: health check connection error");
                }
            }

            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Graceful stop: send interrupt signal, wait for drain, then kill.
    pub async fn graceful_stop(&self) {
        self.state.store(STATE_SWAPPING, Ordering::Release);
        self.cancel_streams();

        let mut child_lock = self.child.lock().await;
        if let Some(ref mut child) = *child_lock {
            // Send SIGTERM equivalent (tokio abstracts per-OS: P2)
            if let Some(id) = child.id() {
                tracing::info!(pid = id, "server_manager: sending stop signal");
            }
            let _ = child.start_kill();

            // Wait up to 10s for graceful exit
            match tokio::time::timeout(Duration::from_secs(10), child.wait()).await {
                Ok(Ok(status)) => {
                    tracing::info!(?status, "server_manager: process exited gracefully");
                }
                Ok(Err(e)) => {
                    tracing::warn!(?e, "server_manager: error waiting for process");
                }
                Err(_) => {
                    tracing::warn!("server_manager: process didn't exit in 10s, killing");
                    let _ = child.kill().await;
                }
            }
        }
        *child_lock = None;

        // Abort stderr reader
        if let Some(handle) = self.reader_handle.lock().await.take() {
            handle.abort();
        }

        self.state.store(STATE_STOPPED, Ordering::Release);
    }

    /// Immediate kill (emergency path).
    pub async fn kill(&self) {
        self.state.store(STATE_SWAPPING, Ordering::Release);
        self.cancel_streams();

        let mut child_lock = self.child.lock().await;
        if let Some(ref mut child) = *child_lock {
            if let Some(id) = child.id() {
                tracing::warn!(pid = id, "server_manager: emergency kill");
            }
            let _ = child.kill().await;
        }
        *child_lock = None;

        // Abort stderr reader
        if let Some(handle) = self.reader_handle.lock().await.take() {
            handle.abort();
        }

        self.state.store(STATE_STOPPED, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_port_from_standard_line() {
        let line = "main: server is listening on http://127.0.0.1:43567";
        assert_eq!(LlamaServerManager::extract_port_from_line(line), Some(43567));
    }

    #[test]
    fn extract_port_from_plain_line() {
        let line = "server is listening on 0.0.0.0:8080";
        assert_eq!(LlamaServerManager::extract_port_from_line(line), Some(8080));
    }

    #[test]
    fn no_port_from_unrelated_line() {
        let line = "model loaded successfully in 2.3s";
        assert_eq!(LlamaServerManager::extract_port_from_line(line), None);
    }

    #[test]
    fn state_transitions() {
        let config = OrchestratorConfig::default();
        let mgr = LlamaServerManager::new(config, "/tmp/model.gguf".into(), None);
        assert_eq!(mgr.state(), STATE_STOPPED);
        mgr.state.store(STATE_READY, Ordering::Release);
        assert!(mgr.is_healthy());
        mgr.state.store(STATE_SWAPPING, Ordering::Release);
        assert!(mgr.is_swapping());
        assert!(!mgr.is_healthy());
    }
}
