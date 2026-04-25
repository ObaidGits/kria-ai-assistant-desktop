//! HeadlessComfyUI sidecar lifecycle manager.
//!
//! Manages the ComfyUI process via HTTP REST API (no stdio RPC):
//!   - `POST /prompt`  → submit a workflow graph, returns `prompt_id`
//!   - `GET  /system_stats` → health check
//!   - `POST /free?unload_models=true` → idle VRAM release
//!   - `POST /interrupt` → cancel running job
//!
//! A PID lockfile at `~/.kria/comfyui/sidecar.pid` prevents zombie sidecars.
//! On startup we attempt to adopt an existing process (header `X-KRIA-OWNED:1`)
//! before spawning a fresh one.

use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::{AtomicU8, Ordering}};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

// State constants.
pub const COMFY_STOPPED: u8 = 0;
pub const COMFY_STARTING: u8 = 1;
pub const COMFY_READY: u8 = 2;
pub const COMFY_ERROR: u8 = 3;

/// Number of trailing log lines we keep buffered from ComfyUI's stderr/stdout
/// so we can attach them to a `HealthTimeout` / `Spawn` error report.
const LOG_TAIL_LINES: usize = 40;

#[derive(Debug, thiserror::Error)]
pub enum ComfyError {
    #[error("ComfyUI sidecar not running")]
    NotRunning,
    #[error("Health-check timeout after {secs}s\n--- last ComfyUI output ---\n{tail}")]
    HealthTimeout { secs: u64, tail: String },
    #[error("ComfyUI process exited during startup (code {code:?})\n--- last ComfyUI output ---\n{tail}")]
    EarlyExit { code: Option<i32>, tail: String },
    #[error("ComfyUI install missing or incomplete: {0}")]
    InstallMissing(String),
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Spawn failed: {0}")]
    Spawn(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Queue submission failed: {body}")]
    QueueFailed { body: String },
}

/// Configuration snapshot needed at spawn time.
#[derive(Debug, Clone)]
pub struct ComfyLaunchConfig {
    pub port: u16,
    pub venv_dir: PathBuf,
    pub models_dir: PathBuf,
    pub output_dir: PathBuf,
    pub extra_args: Vec<String>,
    pub health_check_timeout_secs: u64,
}

/// A submitted workflow + prompt_id.
#[derive(Debug, Clone)]
pub struct QueuedJob {
    pub prompt_id: String,
    pub client_id: String,
}

/// Manages the headless ComfyUI subprocess.
pub struct ComfySidecar {
    state: Arc<AtomicU8>,
    process: Mutex<Option<Child>>,
    /// Rolling buffer of the last `LOG_TAIL_LINES` lines from ComfyUI's
    /// stdout+stderr. Surfaced inside `HealthTimeout` / `EarlyExit` errors
    /// so the user sees the actual Python traceback instead of a bare
    /// "Health-check timeout after 60s".
    log_tail: Arc<Mutex<std::collections::VecDeque<String>>>,
    client: reqwest::Client,
    config: ComfyLaunchConfig,
    pid_lockfile: PathBuf,
}

impl ComfySidecar {
    pub fn new(config: ComfyLaunchConfig) -> Arc<Self> {
        let pid_lockfile = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".kria")
            .join("comfyui")
            .join("sidecar.pid");

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .user_agent("kria-comfy/1.0")
            .build()
            .unwrap_or_default();

        Arc::new(Self {
            state: Arc::new(AtomicU8::new(COMFY_STOPPED)),
            process: Mutex::new(None),
            log_tail: Arc::new(Mutex::new(std::collections::VecDeque::with_capacity(
                LOG_TAIL_LINES,
            ))),
            client,
            config,
            pid_lockfile,
        })
    }

    /// Current state.
    pub fn state(&self) -> u8 {
        self.state.load(Ordering::Acquire)
    }

    pub fn is_ready(&self) -> bool {
        self.state() == COMFY_READY
    }

    /// Base URL for the ComfyUI API.
    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.config.port)
    }

    // ─── Lifecycle ─────────────────────────────────────────────────────────────

    /// Ensure the sidecar is running. Idempotent.
    pub async fn ensure_running(self: &Arc<Self>) -> Result<(), ComfyError> {
        let current = self.state.load(Ordering::Acquire);
        if current == COMFY_READY {
            return Ok(());
        }
        if current == COMFY_STARTING {
            return self.wait_ready().await;
        }
        self.start().await
    }

    /// Spawn the ComfyUI sidecar.
    pub async fn start(self: &Arc<Self>) -> Result<(), ComfyError> {
        // Check for existing healthy process from a previous session.
        if self.try_adopt_existing_process().await {
            self.state.store(COMFY_READY, Ordering::Release);
            return Ok(());
        }

        self.state.store(COMFY_STARTING, Ordering::Release);
        info!("ComfySidecar: spawning on port {}", self.config.port);

        // Resolve the python binary inside the venv.
        let python = {
            let venv_python = self.config.venv_dir.join("bin").join("python");
            if venv_python.exists() {
                venv_python
            } else {
                PathBuf::from("python3")
            }
        };

        // Resolve the ComfyUI app directory:
        //   venv_dir = ~/.kria/comfyui/.venv  →  parent = ~/.kria/comfyui/
        //   ComfyUI app is one level deeper at  ~/.kria/comfyui/ComfyUI/
        let comfy_app_dir = {
            let parent = self.config.venv_dir.parent()
                .unwrap_or(Path::new("."))
                .to_path_buf();
            let candidate = parent.join("ComfyUI");
            if candidate.join("main.py").exists() {
                candidate
            } else {
                parent // fall back if already at ComfyUI root
            }
        };
        let main_py_path = comfy_app_dir.join("main.py");

        // ── Pre-flight: surface install errors with an actionable message
        // BEFORE we burn 60s waiting for /system_stats. The previous code
        // would happily spawn a missing binary and let the user stare at a
        // bare "Health-check timeout" instead.
        if !main_py_path.exists() {
            self.state.store(COMFY_ERROR, Ordering::Release);
            return Err(ComfyError::InstallMissing(format!(
                "ComfyUI main.py not found at {}. Run scripts/setup_comfyui.sh \
                 (or set image_generation.comfy_venv_dir to the correct path).",
                main_py_path.display()
            )));
        }
        if !python.exists() && python.is_absolute() {
            self.state.store(COMFY_ERROR, Ordering::Release);
            return Err(ComfyError::InstallMissing(format!(
                "Python venv binary not found at {}. The venv at {} appears \
                 incomplete — re-run scripts/setup_comfyui.sh.",
                python.display(),
                self.config.venv_dir.display()
            )));
        }

        let main_py = main_py_path.display().to_string();

        // Build launch arguments.
        //
        // ComfyUI cache flags are mutually exclusive (`--cache-classic`,
        // `--cache-lru`, `--cache-none`, `--cache-ram` form an arg group).
        // We pick `--cache-lru 5` because it caps memory usage on 6GB-class
        // GPUs while still benefiting from prompt-conditioning reuse.
        let mut args: Vec<String> = vec![
            main_py,
            "--listen".into(),
            "127.0.0.1".into(),
            "--port".into(),
            self.config.port.to_string(),
            "--disable-auto-launch".into(),
            "--dont-print-server".into(),
            "--output-directory".into(),
            self.config.output_dir.display().to_string(),
            "--cache-lru".into(),
            "5".into(),
        ];
        args.extend(self.config.extra_args.clone());

        // Set PYTORCH_CUDA_ALLOC_CONF to reduce VRAM fragmentation.
        let mut cmd = tokio::process::Command::new(python);
        cmd.args(&args)
            .current_dir(&comfy_app_dir)
            .env("PYTORCH_CUDA_ALLOC_CONF", "expandable_segments:True,max_split_size_mb:512")
            .env("PYTHONUNBUFFERED", "1") // Flush prints/tracebacks immediately
            .env("X-KRIA-OWNED", "1")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        // Linux: request kernel SIGKILL if parent dies.
        #[cfg(target_os = "linux")]
        {
            #[allow(unused_imports)]
            use std::os::unix::process::CommandExt;
            unsafe {
                cmd.pre_exec(|| {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);
                    Ok(())
                });
            }
        }

        let mut child = cmd.spawn().map_err(|e| ComfyError::Spawn(e.to_string()))?;

        // Drain stdout/stderr into the rolling tail buffer + tracing so a
        // failing ComfyUI surfaces its real traceback in the error message.
        // Without this, the user just sees "Health-check timeout after 60s".
        if let Some(stdout) = child.stdout.take() {
            self.spawn_log_drainer(stdout, "stdout");
        }
        if let Some(stderr) = child.stderr.take() {
            self.spawn_log_drainer(stderr, "stderr");
        }

        // Write PID lockfile.
        if let Some(pid) = child.id() {
            self.write_pid_lockfile(pid).await;
        }

        *self.process.lock().await = Some(child);

        // Wait for health endpoint.
        let result = self.wait_ready().await;
        if result.is_err() {
            self.state.store(COMFY_ERROR, Ordering::Release);
            self.cleanup_pid_lockfile().await;
        }
        result
    }

    /// Pipe a child stdout/stderr stream into both `tracing` (debug level,
    /// per line) and a rolling tail buffer that gets attached to spawn
    /// failure errors.
    fn spawn_log_drainer<R>(&self, stream: R, label: &'static str)
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
    {
        let tail = self.log_tail.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stream).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                debug!(target: "comfyui", stream = label, "{}", line);
                let mut buf = tail.lock().await;
                if buf.len() >= LOG_TAIL_LINES {
                    buf.pop_front();
                }
                buf.push_back(line);
            }
        });
    }

    /// Snapshot the current rolling log tail as a single newline-separated
    /// string for embedding in error reports.
    async fn log_tail_snapshot(&self) -> String {
        let buf = self.log_tail.lock().await;
        if buf.is_empty() {
            "(no output captured from ComfyUI; check that python and the venv are valid)".to_string()
        } else {
            buf.iter().cloned().collect::<Vec<_>>().join("\n")
        }
    }

    /// Graceful shutdown.
    pub async fn stop(self: &Arc<Self>) {
        info!("ComfySidecar: stopping");
        self.state.store(COMFY_STOPPED, Ordering::Release);
        let mut guard = self.process.lock().await;
        if let Some(mut child) = guard.take() {
            // Attempt graceful /interrupt first.
            let _ = self.client
                .post(format!("{}/interrupt", self.base_url()))
                .send()
                .await;
            tokio::time::sleep(Duration::from_millis(500)).await;
            let _ = child.kill().await;
        }
        self.cleanup_pid_lockfile().await;
    }

    /// Poll `GET /system_stats` until ComfyUI is up, then confirm node graph
    /// is ready with `GET /object_info`.  The two-step gate catches the window
    /// where the HTTP server is alive but custom nodes (ComfyUI-GGUF) have not
    /// yet finished loading.
    ///
    /// Inside each poll we also `try_wait()` the child process so a fast
    /// crash (missing Python module, CUDA mismatch, port conflict) is
    /// reported in milliseconds with the captured traceback, instead of
    /// silently waiting the full health-check budget.
    async fn wait_ready(&self) -> Result<(), ComfyError> {
        let deadline = tokio::time::Instant::now()
            + Duration::from_secs(self.config.health_check_timeout_secs);
        let stats_url = format!("{}/system_stats", self.base_url());
        let info_url  = format!("{}/object_info", self.base_url());

        // Phase 1: wait for /system_stats
        loop {
            if let Some(err) = self.check_early_exit().await {
                return Err(err);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(ComfyError::HealthTimeout {
                    secs: self.config.health_check_timeout_secs,
                    tail: self.log_tail_snapshot().await,
                });
            }
            match self.client.get(&stats_url).send().await {
                Ok(r) if r.status().is_success() => {
                    debug!("ComfySidecar: /system_stats passed");
                    break;
                }
                Ok(r) => {
                    debug!(status = %r.status(), "ComfySidecar: /system_stats pending");
                }
                Err(e) => {
                    debug!(error = %e, "ComfySidecar: /system_stats not ready");
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        // Phase 2: wait for /object_info (custom nodes finish loading)
        loop {
            if let Some(err) = self.check_early_exit().await {
                return Err(err);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(ComfyError::HealthTimeout {
                    secs: self.config.health_check_timeout_secs,
                    tail: self.log_tail_snapshot().await,
                });
            }
            match self.client.get(&info_url).send().await {
                Ok(r) if r.status().is_success() => {
                    info!("ComfySidecar: ready (system_stats + object_info passed)");
                    self.state.store(COMFY_READY, Ordering::Release);
                    return Ok(());
                }
                Ok(r) => {
                    debug!(status = %r.status(), "ComfySidecar: /object_info pending");
                }
                Err(e) => {
                    debug!(error = %e, "ComfySidecar: /object_info not ready");
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Non-blocking check: has the child process exited already? If so,
    /// build an `EarlyExit` error carrying the captured stderr tail so the
    /// user sees the actual Python traceback (missing module, CUDA OOM at
    /// load time, port already in use, etc.) instead of a 60s timeout.
    async fn check_early_exit(&self) -> Option<ComfyError> {
        let mut guard = self.process.lock().await;
        let child = guard.as_mut()?;
        match child.try_wait() {
            Ok(Some(status)) => {
                let code = status.code();
                let tail = self.log_tail_snapshot().await;
                warn!(?code, "ComfySidecar: child exited during startup");
                Some(ComfyError::EarlyExit { code, tail })
            }
            Ok(None) => None,
            Err(e) => {
                warn!(error = %e, "ComfySidecar: try_wait failed");
                None
            }
        }
    }

    // ─── Job submission ────────────────────────────────────────────────────────

    /// Submit a ComfyUI workflow graph and return the prompt_id + client_id.
    pub async fn submit_workflow(
        &self,
        workflow: serde_json::Value,
    ) -> Result<QueuedJob, ComfyError> {
        if !self.is_ready() {
            return Err(ComfyError::NotRunning);
        }

        let client_id = uuid::Uuid::new_v4().to_string();
        let payload = serde_json::json!({
            "prompt": workflow,
            "client_id": client_id,
        });

        let resp = self.client
            .post(format!("{}/prompt", self.base_url()))
            .json(&payload)
            .send()
            .await?;

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(ComfyError::QueueFailed { body });
        }

        let val: serde_json::Value = serde_json::from_str(&body)
            .map_err(|_| ComfyError::QueueFailed { body: body.clone() })?;

        let prompt_id = val["prompt_id"]
            .as_str()
            .ok_or_else(|| ComfyError::QueueFailed { body: body.clone() })?
            .to_string();

        Ok(QueuedJob { prompt_id, client_id })
    }

    /// Tell ComfyUI to unload all models from VRAM (idle release).
    pub async fn unload_models(&self) -> Result<(), ComfyError> {
        if !self.is_ready() {
            return Ok(());
        }
        let url = format!("{}/free?unload_models=true", self.base_url());
        self.client.post(&url).send().await?;
        info!("ComfySidecar: models unloaded from VRAM");
        Ok(())
    }

    /// Cancel the currently running job.
    pub async fn interrupt(&self) -> Result<(), ComfyError> {
        let url = format!("{}/interrupt", self.base_url());
        self.client.post(&url).send().await?;
        Ok(())
    }

    /// Fetch system stats (useful for diagnostics / VRAM monitoring).
    pub async fn system_stats(&self) -> Result<serde_json::Value, ComfyError> {
        let url = format!("{}/system_stats", self.base_url());
        let val = self.client.get(&url).send().await?.json().await?;
        Ok(val)
    }

    // ─── PID lockfile ──────────────────────────────────────────────────────────

    async fn write_pid_lockfile(&self, pid: u32) {
        if let Some(parent) = self.pid_lockfile.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let content = format!("{}", pid);
        if let Err(e) = tokio::fs::write(&self.pid_lockfile, content).await {
            warn!(error = %e, "ComfySidecar: failed to write PID lockfile");
        }
    }

    async fn cleanup_pid_lockfile(&self) {
        let _ = tokio::fs::remove_file(&self.pid_lockfile).await;
    }

    /// Try to adopt an existing ComfyUI process from a previous session.
    async fn try_adopt_existing_process(&self) -> bool {
        let pid_str = match tokio::fs::read_to_string(&self.pid_lockfile).await {
            Ok(s) => s.trim().to_string(),
            Err(_) => return false,
        };

        // Verify the process is still alive by probing /system_stats then /object_info.
        // We don't check ownership header here — we just probe; if both answer
        // on our expected port, we adopt it.
        let stats_ok = self.client
            .get(format!("{}/system_stats", self.base_url()))
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false);

        if stats_ok {
            let info_ok = self.client
                .get(format!("{}/object_info", self.base_url()))
                .timeout(Duration::from_secs(5))
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false);

            if info_ok {
                info!(pid = %pid_str, "ComfySidecar: adopted existing process");
                return true;
            }
        }

        // Stale lockfile — remove it.
        self.cleanup_pid_lockfile().await;
        false
    }
}

impl Drop for ComfySidecar {
    fn drop(&mut self) {
        // Best-effort synchronous cleanup on drop (process is killed by kill_on_drop(true)).
        let lockfile = self.pid_lockfile.clone();
        tokio::spawn(async move {
            let _ = tokio::fs::remove_file(&lockfile).await;
        });
    }
}
