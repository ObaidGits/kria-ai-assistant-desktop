use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, oneshot};
use std::collections::HashMap;
use std::time::Duration;

use super::protocol::{JsonRpcRequest, JsonRpcResponse};
use super::health::SidecarHealth;

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
            let venv_python = format!("{}/bin/python", venv);
            if tokio::fs::metadata(&venv_python).await.is_ok() {
                venv_python
            } else {
                self.python_cmd.clone()
            }
        } else {
            self.python_cmd.clone()
        };

        tracing::info!(python = %python, "spawning Python sidecar");

        let mut child = Command::new(&python)
            .arg("-m")
            .arg("kria_modules.bridge")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;

        let stdout = child.stdout.take().ok_or_else(|| anyhow::anyhow!("no stdout"))?;
        let stderr = child.stderr.take().ok_or_else(|| anyhow::anyhow!("no stderr"))?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow::anyhow!("no stdin"))?;

        // Store child & stdin
        *self.child.lock().await = Some(child);
        *self.stdin.lock().await = Some(stdin);

        // Spawn stderr reader (logs only)
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
                            tracing::debug!(target: "sidecar_stderr", "{}", trimmed);
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
                                tracing::warn!("failed to parse sidecar response: {}: {}", e, trimmed);
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
        let _ = self.request("configure_tier", serde_json::json!({"tier": tier})).await;

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
        self.request("list_capabilities", serde_json::json!({})).await
    }

    /// Configure the hardware tier for tier-aware processing.
    pub async fn configure_tier(&self, tier: &str) -> anyhow::Result<()> {
        *self.hw_tier.lock().await = tier.to_string();
        self.request("configure_tier", serde_json::json!({"tier": tier})).await?;
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
}

impl std::fmt::Debug for SidecarBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SidecarBridge")
            .field("python_cmd", &self.python_cmd)
            .field("alive", &self.health.is_alive())
            .finish()
    }
}
