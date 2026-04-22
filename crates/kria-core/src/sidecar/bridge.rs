use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{oneshot, Mutex};

use super::health::SidecarHealth;
use super::protocol::{JsonRpcRequest, JsonRpcResponse};

/// Manages the Python sidecar process and JSON-RPC communication.
pub struct SidecarBridge {
    child: Mutex<Option<Child>>,
    stdin: Mutex<Option<tokio::process::ChildStdin>>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcResponse>>>>,
    next_id: AtomicU64,
    python_cmd: String,
    venv_path: Option<String>,
    health: Arc<SidecarHealth>,
    hw_tier: Mutex<String>,
    reader_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl SidecarBridge {
    /// Create a new SidecarBridge.
    ///
    /// - `python_cmd`: path to python executable (e.g. "python3" or venv python)
    /// - `venv_path`: optional path to virtualenv for kria-modules
    pub fn new(python_cmd: &str, venv_path: Option<&str>) -> Self {
        Self {
            child: Mutex::new(None),
            stdin: Mutex::new(None),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            python_cmd: python_cmd.to_string(),
            venv_path: venv_path.map(|s| s.to_string()),
            health: Arc::new(SidecarHealth::new(3)),
            hw_tier: Mutex::new("standard".into()),
            reader_task: Mutex::new(None),
        }
    }

    /// Spawn the Python sidecar process and wait for the `ready` signal.
    pub async fn spawn(&self) -> anyhow::Result<()> {
        let python = if let Some(ref venv) = self.venv_path {
            let venv_python = Self::venv_python_path(venv);
            if tokio::fs::metadata(&venv_python).await.is_ok() {
                tracing::info!("sidecar: using venv python at {}", venv_python);
                venv_python
            } else {
                // Venv doesn't exist — attempt to create it and install kria-modules.
                tracing::warn!("sidecar: venv not found at {}; attempting auto-setup", venv);
                match Self::setup_venv(venv, &self.python_cmd).await {
                    Ok(p) => {
                        tracing::info!("sidecar: venv setup complete, using {}", p);
                        p
                    }
                    Err(e) => {
                        tracing::warn!(
                            "sidecar: venv setup failed ({}); falling back to system {}",
                            e,
                            self.python_cmd
                        );
                        self.python_cmd.clone()
                    }
                }
            }
        } else {
            self.python_cmd.clone()
        };

        // Detect kria-modules source directory (workspace layout: exe is target/{debug,release}/kria-desktop).
        // If found, prepend it to PYTHONPATH so the live source files are always used directly —
        // no venv resync needed when Python files change.
        let exe = std::env::current_exe().unwrap_or_default();
        let src_candidates = [
            // target/debug/kria-desktop  →  ../../..  →  workspace root
            exe.parent()
                .and_then(|p| p.parent())
                .and_then(|p| p.parent())
                .map(|ws| ws.join("kria-modules").join("src"))
                .unwrap_or_default(),
            // direct sibling of exe
            exe.parent()
                .map(|p| p.join("kria-modules").join("src"))
                .unwrap_or_default(),
        ];
        let pythonpath: Option<String> = src_candidates
            .iter()
            .find(|p| p.join("kria_modules").exists())
            .map(|p| {
                let existing = std::env::var("PYTHONPATH").unwrap_or_default();
                let src = p.to_string_lossy();
                tracing::info!("sidecar: using live source via PYTHONPATH={}", src);
                if existing.is_empty() {
                    src.into_owned()
                } else {
                    format!("{}{}{}", src, Self::path_separator(), existing)
                }
            });

        // Quick sanity-check: can `python` actually import kria_modules?
        let mut check_cmd = tokio::process::Command::new(&python);
        check_cmd.args(["-c", "import kria_modules.bridge"]);
        if let Some(ref pp) = pythonpath {
            check_cmd.env("PYTHONPATH", pp);
        }
        let can_import = check_cmd
            .output()
            .await
            .map(|o| {
                if !o.status.success() {
                    let err = String::from_utf8_lossy(&o.stderr);
                    tracing::warn!(
                        "sidecar pre-check: kria_modules not importable: {}",
                        err.trim()
                    );
                    false
                } else {
                    true
                }
            })
            .unwrap_or(false);

        if !can_import {
            anyhow::bail!(
                "kria_modules is not installed. \
                 Run the setup script: scripts/setup.sh  \
                 Or manually: python3 -m venv ~/.kria/python-env && \
                 cp -r kria-modules/src/kria_modules ~/.kria/python-env/lib/python*/site-packages/"
            );
        }

        tracing::info!(python = %python, "spawning Python sidecar");

        let mut spawn_cmd = Command::new(&python);
        spawn_cmd
            .arg("-m")
            .arg("kria_modules.bridge")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(ref pp) = pythonpath {
            spawn_cmd.env("PYTHONPATH", pp);
        }
        let mut child = spawn_cmd.spawn()?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("no stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("no stderr"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("no stdin"))?;

        // Store child & stdin
        *self.child.lock().await = Some(child);
        *self.stdin.lock().await = Some(stdin);

        // Spawn stderr reader — logs at WARN so Python errors (ModuleNotFoundError etc.) are visible
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {
                        let trimmed = line.trim();
                        if !trimmed.is_empty() {
                            // Python errors/warnings → WARN so they surface in the log
                            if trimmed.contains("Error")
                                || trimmed.contains("Traceback")
                                || trimmed.contains("error")
                            {
                                tracing::warn!(target: "sidecar_stderr", "{}", trimmed);
                            } else {
                                tracing::debug!(target: "sidecar_stderr", "{}", trimmed);
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        // Spawn stdout reader — dispatches responses to pending waiters
        let pending = self.pending.clone();
        let health = self.health.clone();

        let reader_handle = tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => {
                        tracing::warn!("sidecar stdout closed");
                        break;
                    }
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }

                        // Check for notifications (no id)
                        if trimmed.contains("\"method\"") && !trimmed.contains("\"id\"") {
                            // Notification — e.g. {"jsonrpc":"2.0","method":"ready"}
                            if trimmed.contains("\"ready\"") {
                                tracing::info!("sidecar ready notification received");
                                health.mark_alive();
                            }
                            continue;
                        }

                        match serde_json::from_str::<JsonRpcResponse>(trimmed) {
                            Ok(resp) => {
                                if let Some(id) = resp.id {
                                    let mut map = pending.lock().await;
                                    if let Some(sender) = map.remove(&id) {
                                        let _ = sender.send(resp);
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "failed to parse sidecar response: {}: {}",
                                    e,
                                    trimmed
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!("sidecar stdout read error: {}", e);
                        break;
                    }
                }
            }
        });

        *self.reader_task.lock().await = Some(reader_handle);

        // Wait for ready signal with timeout
        let start = std::time::Instant::now();
        let timeout = Duration::from_secs(30);
        while !self.health.is_alive() {
            if start.elapsed() > timeout {
                anyhow::bail!("sidecar did not become ready within 30s");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        // Configure tier
        let tier = self.hw_tier.lock().await.clone();
        let _ = self
            .request("configure_tier", serde_json::json!({"tier": tier}))
            .await;

        tracing::info!("sidecar bridge established");
        Ok(())
    }

    /// Send a JSON-RPC request and wait for the response.
    pub async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = JsonRpcRequest::new(id, method, params);

        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        // Serialize and send
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');

        {
            let mut stdin_guard = self.stdin.lock().await;
            if let Some(ref mut stdin) = *stdin_guard {
                stdin.write_all(line.as_bytes()).await?;
                stdin.flush().await?;
            } else {
                self.pending.lock().await.remove(&id);
                anyhow::bail!("sidecar stdin not available");
            }
        }

        // Wait for response with timeout
        let resp = tokio::time::timeout(Duration::from_secs(120), rx)
            .await
            .map_err(|_| anyhow::anyhow!("sidecar request timed out after 120s"))?
            .map_err(|_| anyhow::anyhow!("sidecar response channel closed"))?;

        resp.into_result().map_err(|e| anyhow::anyhow!(e))
    }

    /// Ping the sidecar.
    pub async fn ping(&self) -> bool {
        match self.request("ping", serde_json::json!({})).await {
            Ok(v) => v.get("pong").is_some(),
            Err(_) => false,
        }
    }

    /// Get sidecar health check result.
    pub async fn health_check(&self) -> anyhow::Result<serde_json::Value> {
        self.request("health_check", serde_json::json!({})).await
    }

    /// List available sidecar capabilities.
    pub async fn list_capabilities(&self) -> anyhow::Result<serde_json::Value> {
        self.request("list_capabilities", serde_json::json!({}))
            .await
    }

    /// Configure the hardware tier for tier-aware processing.
    pub async fn configure_tier(&self, tier: &str) -> anyhow::Result<()> {
        *self.hw_tier.lock().await = tier.to_string();
        self.request("configure_tier", serde_json::json!({"tier": tier}))
            .await?;
        Ok(())
    }

    /// Check if the sidecar is currently alive.
    pub fn is_alive(&self) -> bool {
        self.health.is_alive()
    }

    /// Gracefully shut down the sidecar.
    pub async fn shutdown(&self) -> anyhow::Result<()> {
        tracing::info!("shutting down sidecar");
        self.health.stop();

        // Send shutdown request (best effort)
        let _ = self.request("shutdown", serde_json::json!({})).await;

        // Close stdin to signal EOF
        *self.stdin.lock().await = None;

        // Wait for child to exit
        if let Some(mut child) = self.child.lock().await.take() {
            let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        }

        // Abort reader task
        if let Some(handle) = self.reader_task.lock().await.take() {
            handle.abort();
        }

        Ok(())
    }

    /// Create a virtualenv at `venv_dir` and install `kria-modules` into it.
    ///
    /// Looks for the `kria-modules` source directory relative to the kria-modules
    /// package folder adjacent to the running binary, then falls back to the
    /// workspace source tree (for dev builds).
    async fn setup_venv(venv_dir: &str, python_cmd: &str) -> anyhow::Result<String> {
        tracing::info!("sidecar setup: creating venv at {venv_dir}");

        // 1. Create the venv
        let out = tokio::process::Command::new(python_cmd)
            .args(["-m", "venv", venv_dir])
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("failed to run `{python_cmd} -m venv`: {e}"))?;

        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            anyhow::bail!("venv creation failed: {}", err.trim());
        }
        tracing::info!("sidecar setup: venv created");

        let venv_python = Self::venv_python_path(venv_dir);
        let venv_pip = Self::venv_pip_path(venv_dir);

        // 2. Upgrade pip quietly
        let _ = tokio::process::Command::new(&venv_pip)
            .args(["install", "--upgrade", "pip", "--quiet"])
            .output()
            .await;

        // 3. Find kria-modules source directory
        //    Try sibling of the current exe, then workspace-relative paths.
        let exe = std::env::current_exe().unwrap_or_default();
        let candidates = [
            // release/debug target dir: exe is target/debug/kria-desktop → go up 3 levels
            exe.parent()
                .and_then(|p| p.parent())
                .and_then(|p| p.parent())
                .map(|ws| ws.join("kria-modules"))
                .unwrap_or_default(),
            // direct sibling
            exe.parent()
                .map(|p| p.join("kria-modules"))
                .unwrap_or_default(),
        ];

        let modules_dir = candidates
            .iter()
            .find(|p| p.join("pyproject.toml").exists());

        match modules_dir {
            Some(src) => {
                tracing::info!(
                    "sidecar setup: installing kria-modules from {}",
                    src.display()
                );

                // Strategy: directly copy kria_modules into site-packages.
                // This avoids any build-backend dependency (hatchling/setuptools)
                // that may not be installed in the new venv.
                let site_pkgs_out = tokio::process::Command::new(&venv_python)
                    .args(["-c", "import site; print(site.getsitepackages()[0])"])
                    .output()
                    .await
                    .map_err(|e| anyhow::anyhow!("failed to get site-packages: {e}"))?;

                let site_pkgs = String::from_utf8_lossy(&site_pkgs_out.stdout)
                    .trim()
                    .to_string();

                if site_pkgs.is_empty() {
                    anyhow::bail!("could not determine venv site-packages path");
                }

                let src_pkg = src.join("src").join("kria_modules");
                let dst_pkg = std::path::Path::new(&site_pkgs).join("kria_modules");

                // Use `cp -r` to copy the package directory
                let cp_out = tokio::process::Command::new("cp")
                    .args([
                        "-r",
                        src_pkg.to_str().unwrap_or(""),
                        dst_pkg.to_str().unwrap_or(""),
                    ])
                    .output()
                    .await
                    .map_err(|e| anyhow::anyhow!("cp failed: {e}"))?;

                if !cp_out.status.success() {
                    let err = String::from_utf8_lossy(&cp_out.stderr);
                    anyhow::bail!("failed to copy kria_modules: {}", err.trim());
                }

                // Install runtime deps for all processors
                let _ = tokio::process::Command::new(&venv_pip)
                    .args(["install", "psutil", "feedparser", "trafilatura", "--quiet"])
                    .output()
                    .await;

                tracing::info!("sidecar setup: kria-modules installed to {}", site_pkgs);
            }
            None => {
                // Couldn't find source — install minimal deps only (bridge.py is self-contained)
                tracing::warn!(
                    "sidecar setup: kria-modules source not found; installing bridge deps only"
                );
                let _ = tokio::process::Command::new(&venv_pip)
                    .args(["install", "psutil", "--quiet"])
                    .output()
                    .await;
            }
        }

        Ok(venv_python)
    }
}

impl std::fmt::Debug for SidecarBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SidecarBridge")
            .field("python_cmd", &self.python_cmd)
            .field("alive", &self.health.is_alive())
            .finish()
    }
}

// ── Cross-platform path helpers ──────────────────────────────────────
impl SidecarBridge {
    /// Return the platform-correct path to the `python` binary inside a venv.
    fn venv_python_path(venv_dir: &str) -> String {
        if cfg!(target_os = "windows") {
            format!("{}\\Scripts\\python.exe", venv_dir)
        } else {
            format!("{}/bin/python", venv_dir)
        }
    }

    /// Return the platform-correct path to the `pip` binary inside a venv.
    fn venv_pip_path(venv_dir: &str) -> String {
        if cfg!(target_os = "windows") {
            format!("{}\\Scripts\\pip.exe", venv_dir)
        } else {
            format!("{}/bin/pip", venv_dir)
        }
    }

    /// Platform-correct separator for `PYTHONPATH` entries.
    fn path_separator() -> &'static str {
        if cfg!(target_os = "windows") {
            ";"
        } else {
            ":"
        }
    }
}
