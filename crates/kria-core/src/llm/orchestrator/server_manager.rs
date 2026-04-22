//! LlamaServerManager — manages the llama-server process lifecycle.
//!
//! Key design decisions:
//! - AtomicU8 for lock-free server state (V7: no RwLock deadlock)
//! - CancellationToken for non-blocking stream abort (V13)
//! - Ephemeral ports via --port 0 + stderr parsing (V5, V14)
//! - ChildGuard RAII: SIGTERM→SIGKILL ladder + prctl/setsid (Phase 2)

use crate::config::OrchestratorConfig;
use crate::infra::event_bus::EventBus;
use crate::llm::orchestrator::child_guard::{self, ChildGuard};
use crate::platform::os;
use std::process::Stdio;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{ChildStderr, ChildStdout};
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;

/// Server states (stored in AtomicU8).
pub const STATE_STOPPED: u8 = 0;
pub const STATE_STARTING: u8 = 1;
pub const STATE_READY: u8 = 2;
pub const STATE_SWAPPING: u8 = 3;
pub const STATE_ERROR: u8 = 4;

#[derive(Debug, Clone, Copy)]
struct LaunchTuning {
    batch_size: u32,
    ubatch_size: Option<u32>,
    parallel: Option<u32>,
    no_warmup: bool,
}

fn launch_tuning(config_batch_size: u32, enable_vision: bool) -> LaunchTuning {
    let configured = config_batch_size.max(1);

    if enable_vision {
        // Vision inference on 6GB-class GPUs is unstable with auto parallel
        // slots + warmup. Use a conservative profile to avoid segfault/OOM
        // while keeping the endpoint responsive.
        let safe_batch = configured.min(128).max(1);
        return LaunchTuning {
            batch_size: safe_batch,
            ubatch_size: Some(safe_batch),
            parallel: Some(1),
            no_warmup: true,
        };
    }

    LaunchTuning {
        batch_size: configured,
        ubatch_size: None,
        parallel: None,
        no_warmup: false,
    }
}

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
    /// The child process wrapped in a ChildGuard for safe lifecycle management.
    child: Mutex<Option<ChildGuard>>,
    /// CancellationToken — cancelled during swap to abort in-flight streams.
    cancel_token: CancellationToken,
    /// Token for the stderr reader task.
    reader_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Notified when a swap finishes (STATE_STOPPED or STATE_READY).
    /// Callers waiting for a swap to complete use this instead of busy-polling.
    swap_done: Arc<Notify>,
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
            swap_done: Arc::new(Notify::new()),
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

    /// Whether the underlying process appears alive right now.
    pub async fn has_live_process(&self) -> bool {
        let mut child_lock = self.child.lock().await;
        let Some(guard) = child_lock.as_mut() else {
            return false;
        };

        // Guard with inner child=None means the process was already reaped.
        if !guard.is_alive() {
            *child_lock = None;
            return false;
        }

        match guard.try_wait() {
            Ok(None) => true,
            Ok(Some(_)) | Err(_) => {
                *child_lock = None;
                false
            }
        }
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

    /// Returns an `Arc<Notify>` that is notified every time a swap finishes
    /// (on both success and failure paths). Callers can `notified().await`
    /// instead of busy-polling `is_swapping()`.  The Arc is stable for the
    /// lifetime of the manager.
    pub fn swap_done_notify(&self) -> Arc<Notify> {
        self.swap_done.clone()
    }

    /// Await swap completion with a timeout.
    /// Returns `true` if the swap finished before the deadline, `false` if
    /// the timeout was hit (server is still swapping or stuck).
    pub async fn wait_for_swap_done(&self, timeout: Duration) -> bool {
        // Fast path: not swapping right now.
        if !self.is_swapping() {
            return true;
        }
        // Subscribe before the is_swapping fast-path re-check so we can't
        // miss a notify that fires between the two checks.
        let notified = self.swap_done.notified();
        if !self.is_swapping() {
            return true;
        }
        tokio::time::timeout(timeout, notified).await.is_ok()
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
        let tuning = launch_tuning(self.config.batch_size, enable_vision);

        // Configure prctl(PR_SET_PDEATHSIG=SIGKILL) + setsid() in pre_exec
        // so the child is killed if Kria panics or is force-quit.
        child_guard::configure_child_command(&mut cmd);

        cmd.arg("--model").arg(&self.model_path);
        cmd.arg("--port").arg("0"); // Ephemeral port (V5)
        cmd.arg("--ctx-size").arg(context.to_string());
        cmd.arg("--n-gpu-layers").arg(ngl.to_string());
        cmd.arg("--batch-size").arg(tuning.batch_size.to_string());

        if let Some(ubatch) = tuning.ubatch_size {
            cmd.arg("--ubatch-size").arg(ubatch.to_string());
        }

        if let Some(parallel) = tuning.parallel {
            cmd.arg("--parallel").arg(parallel.to_string());
        }

        if tuning.no_warmup {
            cmd.arg("--no-warmup");
        }

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
            batch_size = tuning.batch_size,
            ubatch_size = ?tuning.ubatch_size,
            parallel = ?tuning.parallel,
            no_warmup = tuning.no_warmup,
            "server_manager: spawning llama-server"
        );

        let child = cmd.spawn().map_err(|e| {
            self.state.store(STATE_ERROR, Ordering::Release);
            anyhow::anyhow!("failed to spawn llama-server: {}", e)
        })?;
        let mut guard = ChildGuard::new(child);

        // Parse stdout/stderr for port discovery and log forwarding.
        // Some llama.cpp builds print the listening line to stdout instead
        // of stderr, so we consume both streams.
        let stderr = guard.take_stderr();
        let stdout = guard.take_stdout();
        let port = match self.discover_port(stderr, stdout).await {
            Ok(p) => p,
            Err(e) => {
                // Kill the child before returning the error so we don't leak it.
                guard.force_kill().await;
                return Err(e);
            }
        };

        let url = format!("http://127.0.0.1:{}/v1", port);
        tracing::info!(port, url = %url, "server_manager: discovered ephemeral port");

        // Update API URL before storing the guard so callers can read it.
        {
            let mut lock = self.api_url.write().await;
            *lock = url.clone();
        }

        // Store the child guard.
        {
            let mut lock = self.child.lock().await;
            *lock = Some(guard);
        }

        // Wait for the health endpoint to report ready.
        // On failure, take the guard back and force-kill to avoid leaving a
        // zombie process that holds the port.
        if let Err(e) = self.wait_for_health(&url).await {
            if let Some(mut g) = self.child.lock().await.take() {
                g.force_kill().await;
            }
            self.state.store(STATE_ERROR, Ordering::Release);
            return Err(e);
        }

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

    /// Discover the ephemeral port from llama-server output.
    /// llama-server prints something like: "main: server is listening on http://127.0.0.1:PORT"
    async fn discover_port(
        &self,
        stderr: Option<ChildStderr>,
        stdout: Option<ChildStdout>,
    ) -> anyhow::Result<u16> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let mut stream_tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();

        if let Some(stderr) = stderr {
            let tx = tx.clone();
            stream_tasks.push(tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "llama-server", "{}", line);
                    let _ = tx.send(line);
                }
            }));
        }

        if let Some(stdout) = stdout {
            let tx = tx.clone();
            stream_tasks.push(tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "llama-server", "{}", line);
                    let _ = tx.send(line);
                }
            }));
        }

        drop(tx);

        if stream_tasks.is_empty() {
            return Err(anyhow::anyhow!("no stdout/stderr from llama-server"));
        }

        let port_timeout_secs = self.config.port_discovery_timeout_secs.max(1);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(port_timeout_secs);

        let discovered = loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(line)) => {
                    // llama-server log format varies but port is usually in
                    // "... listening ... :PORT" or "..., port: PORT ..."
                    if let Some(port) = Self::extract_port_from_line(&line) {
                        break Ok(port);
                    }
                }
                Ok(None) => {
                    break Err(anyhow::anyhow!(
                        "llama-server exited before reporting listening port"
                    ));
                }
                Err(_) => {
                    break Err(anyhow::anyhow!(
                        "timed out waiting for llama-server to report listening port after {}s",
                        port_timeout_secs
                    ));
                }
            }
        };

        match discovered {
            Ok(port) => {
                // Keep draining process logs in background after discovery.
                let handle = tokio::spawn(async move {
                    for task in stream_tasks {
                        let _ = task.await;
                    }
                });
                *self.reader_handle.lock().await = Some(handle);
                Ok(port)
            }
            Err(e) => {
                for task in stream_tasks {
                    task.abort();
                }
                Err(e)
            }
        }
    }

    /// Extract port number from a llama-server log line.
    fn extract_port_from_line(line: &str) -> Option<u16> {
        // Match patterns like "127.0.0.1:8080" or "0.0.0.0:12345"
        // The port appears after the last colon in the address
        if !line.to_ascii_lowercase().contains("listening") {
            return None;
        }

        // Prefer explicit `port` token if present (covers modern llama.cpp logs
        // like `..., port: 44123, n_threads_http: 31`).
        if let Some(port_idx) = line.to_ascii_lowercase().rfind("port") {
            let tail = &line[port_idx + 4..];
            let digits: String = tail
                .chars()
                .skip_while(|c| !c.is_ascii_digit())
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Ok(port) = digits.parse::<u16>() {
                if port >= 1024 {
                    return Some(port);
                }
            }
        }

        // Fallback for older formats that end with host:port.
        for segment in line.rsplit(':') {
            let trimmed = segment.trim_start();
            let digits: String = trimmed
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Ok(port) = digits.parse::<u16>() {
                if port >= 1024 {
                    return Some(port);
                }
            }
        }

        None
    }

    /// Wait for /health to return 200.
    /// Uses exponential backoff: 50 → 100 → 200 → 400 → 800ms (cap).
    async fn wait_for_health(&self, api_url: &str) -> anyhow::Result<()> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()?;

        let health_url = format!("{}", api_url.replace("/v1", "/health"));
        let health_timeout_secs = self.config.health_check_timeout_secs.max(1);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(health_timeout_secs);
        let mut backoff_ms: u64 = 50;

        loop {
            if tokio::time::Instant::now() > deadline {
                self.state.store(STATE_ERROR, Ordering::Release);
                return Err(anyhow::anyhow!(
                    "llama-server health check timed out after {}s",
                    health_timeout_secs
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

            tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
            backoff_ms = (backoff_ms * 2).min(800);
        }
    }

    /// Graceful stop: send interrupt signal, wait for drain, then kill.
    pub async fn graceful_stop(&self) {
        self.graceful_stop_with_timeout(Duration::from_secs(
            self.config.graceful_stop_timeout_secs.max(1),
        ))
        .await;
    }

    /// Graceful stop with explicit timeout override.
    /// SIGTERM → wait(timeout) → SIGKILL → wait (via ChildGuard).
    pub async fn graceful_stop_with_timeout(&self, timeout: Duration) {
        self.state.store(STATE_SWAPPING, Ordering::Release);
        self.cancel_streams();

        // Drain the reader task before killing so we get any final log lines.
        if let Some(handle) = self.reader_handle.lock().await.take() {
            handle.abort();
        }

        if let Some(mut guard) = self.child.lock().await.take() {
            guard.terminate(timeout).await;
        }

        self.state.store(STATE_STOPPED, Ordering::Release);
        self.swap_done.notify_waiters();
    }

    /// Immediate kill (emergency path): SIGKILL → reap.
    pub async fn kill(&self) {
        self.state.store(STATE_SWAPPING, Ordering::Release);
        self.cancel_streams();

        if let Some(handle) = self.reader_handle.lock().await.take() {
            handle.abort();
        }

        if let Some(mut guard) = self.child.lock().await.take() {
            guard.force_kill().await;
        }

        self.state.store(STATE_STOPPED, Ordering::Release);
        self.swap_done.notify_waiters();
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
    fn extract_port_from_line_with_space_after_colon() {
        let line = "srv  listen: HTTP server is listening, hostname: 127.0.0.1, port: 44123, n_threads_http: 31";
        assert_eq!(LlamaServerManager::extract_port_from_line(line), Some(44123));
    }

    #[test]
    fn extract_port_from_port_word_format() {
        let line = "main: server is listening, host 127.0.0.1, port 45321";
        assert_eq!(LlamaServerManager::extract_port_from_line(line), Some(45321));
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

    #[test]
    fn launch_tuning_uses_conservative_profile_for_vision() {
        let tuning = launch_tuning(512, true);
        assert_eq!(tuning.batch_size, 128);
        assert_eq!(tuning.ubatch_size, Some(128));
        assert_eq!(tuning.parallel, Some(1));
        assert!(tuning.no_warmup);
    }

    #[test]
    fn launch_tuning_preserves_config_for_non_vision() {
        let tuning = launch_tuning(256, false);
        assert_eq!(tuning.batch_size, 256);
        assert_eq!(tuning.ubatch_size, None);
        assert_eq!(tuning.parallel, None);
        assert!(!tuning.no_warmup);
    }
}
