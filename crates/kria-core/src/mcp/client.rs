//! MCP client — manages a single MCP server process over stdio.
//!
//! Handles:
//! - Spawning the server process
//! - JSON-RPC initialize handshake
//! - tools/list, tools/call
//! - Graceful shutdown

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{oneshot, Mutex};

use super::protocol::*;

const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 60;
const STARTUP_REQUEST_TIMEOUT_SECS: u64 = 120;

/// State of an MCP server connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum McpServerState {
    Stopped,
    Starting,
    Running,
    Error,
}

/// An MCP client connected to a single server via stdio transport.
pub struct McpClient {
    pub name: String,
    state: Arc<Mutex<McpServerState>>,
    child: Mutex<Option<Child>>,
    stdin: Mutex<Option<tokio::process::ChildStdin>>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcResponse>>>>,
    next_id: AtomicU64,
    reader_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    tools: Arc<Mutex<Vec<McpToolDef>>>,
    server_info: Arc<Mutex<Option<ServerInfo>>>,
    error_msg: Arc<Mutex<Option<String>>>,
    /// Consecutive restart count for exponential backoff (1s → 2s → 4s → … max 30s).
    restart_count: AtomicU64,
}

impl McpClient {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            state: Arc::new(Mutex::new(McpServerState::Stopped)),
            child: Mutex::new(None),
            stdin: Mutex::new(None),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            reader_task: Mutex::new(None),
            tools: Arc::new(Mutex::new(Vec::new())),
            server_info: Arc::new(Mutex::new(None)),
            error_msg: Arc::new(Mutex::new(None)),
            restart_count: AtomicU64::new(0),
        }
    }

    /// Get current server state.
    pub async fn state(&self) -> McpServerState {
        *self.state.lock().await
    }

    /// Get discovered tools.
    pub async fn tools(&self) -> Vec<McpToolDef> {
        self.tools.lock().await.clone()
    }

    /// Get the last error message, if any.
    pub async fn error(&self) -> Option<String> {
        self.error_msg.lock().await.clone()
    }

    /// Spawn the MCP server process, initialize, and discover tools.
    pub async fn start(
        &self,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> anyhow::Result<()> {
        *self.state.lock().await = McpServerState::Starting;
        *self.error_msg.lock().await = None;

        let result = self.do_start(command, args, env).await;
        if let Err(ref e) = result {
            *self.state.lock().await = McpServerState::Error;
            *self.error_msg.lock().await = Some(e.to_string());
        }
        result
    }

    async fn do_start(
        &self,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> anyhow::Result<()> {
        tracing::info!(
            "[MCP:{}] do_start — spawning: {} {:?}",
            self.name,
            command,
            args
        );
        if !env.is_empty() {
            let keys: Vec<&str> = env.keys().map(|s| s.as_str()).collect();
            tracing::debug!("[MCP:{}] env vars: {:?}", self.name, keys);
        }

        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        for (k, v) in env {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn().map_err(|e| {
            tracing::error!(
                "[MCP:{}] failed to spawn process '{}': {}",
                self.name,
                command,
                e
            );
            e
        })?;
        tracing::info!("[MCP:{}] process spawned (pid={:?})", self.name, child.id());

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

        *self.child.lock().await = Some(child);
        *self.stdin.lock().await = Some(stdin);

        // Spawn stderr logger
        let server_name = self.name.clone();
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
                            tracing::debug!(target: "mcp_stderr", server = %server_name, "{}", trimmed);
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        // Spawn stdout response reader
        let pending = self.pending.clone();
        let reader_name = self.name.clone();
        let reader_handle = tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => {
                        tracing::info!("[MCP:{}] stdout EOF — server process exited", reader_name);
                        // Drop all outstanding response senders so in-flight requests fail fast.
                        pending.lock().await.clear();
                        break;
                    }
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
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
                                    "[MCP:{}] parse error: {}: {}",
                                    reader_name,
                                    e,
                                    &trimmed[..trimmed.len().min(200)]
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!("[MCP:{}] stdout read error: {}", reader_name, e);
                        // Ensure pending callers do not wait for the full timeout window.
                        pending.lock().await.clear();
                        break;
                    }
                }
            }
        });

        *self.reader_task.lock().await = Some(reader_handle);

        // MCP initialize handshake
        tracing::info!(
            "[MCP:{}] sending initialize request (protocol 2024-11-05)",
            self.name
        );
        let init_params = InitializeParams {
            protocol_version: "2024-11-05".into(),
            capabilities: ClientCapabilities::default(),
            client_info: ClientInfo {
                name: "kria".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
        };

        let result = self
            .request_with_timeout(
                "initialize",
                Some(serde_json::to_value(&init_params)?),
                Duration::from_secs(STARTUP_REQUEST_TIMEOUT_SECS),
            )
            .await
            .map_err(|e| {
                tracing::error!("[MCP:{}] initialize request failed: {}", self.name, e);
                e
            })?;

        let init_result: InitializeResult = serde_json::from_value(result).map_err(|e| {
            tracing::error!(
                "[MCP:{}] failed to parse initialize response: {}",
                self.name,
                e
            );
            e
        })?;
        *self.server_info.lock().await = init_result.server_info.clone();

        tracing::info!(
            "[MCP:{}] initialize OK — server_name={:?} protocol={}",
            self.name,
            init_result.server_info.as_ref().map(|s| &s.name),
            init_result.protocol_version
        );

        // Send initialized notification (no id — it's a notification)
        tracing::debug!("[MCP:{}] sending notifications/initialized", self.name);
        self.notify("notifications/initialized", None).await?;

        // Discover tools if the server supports them
        if init_result.capabilities.tools.is_some() {
            tracing::info!(
                "[MCP:{}] server supports tools — requesting tools/list",
                self.name
            );
            let tools_list = self
                .list_all_tools_with_timeout(Duration::from_secs(STARTUP_REQUEST_TIMEOUT_SECS))
                .await
                .map_err(|e| {
                    tracing::error!("[MCP:{}] tools/list request failed: {}", self.name, e);
                    e
                })?;
            tracing::info!(
                "[MCP:{}] discovered {} tool(s):",
                self.name,
                tools_list.len()
            );
            for t in &tools_list {
                tracing::info!("[MCP:{}]   - {}", self.name, t.name);
            }
            *self.tools.lock().await = tools_list;
        } else {
            tracing::warn!(
                "[MCP:{}] server does NOT advertise tools capability",
                self.name
            );
        }

        *self.state.lock().await = McpServerState::Running;
        self.restart_count.store(0, Ordering::Relaxed);
        tracing::info!("[MCP:{}] state = Running", self.name);
        Ok(())
    }

    /// Refresh tools by requesting `tools/list` again.
    ///
    /// Some MCP servers (for example, proxy-style runtimes) expose additional
    /// tools after external state changes and expect the client to re-list tools.
    pub async fn refresh_tools(&self) -> anyhow::Result<Vec<McpToolDef>> {
        let tools = self
            .list_all_tools_with_timeout(Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS))
            .await?;
        *self.tools.lock().await = tools.clone();
        Ok(tools)
    }

    async fn list_all_tools_with_timeout(
        &self,
        timeout: Duration,
    ) -> anyhow::Result<Vec<McpToolDef>> {
        let mut all_tools: Vec<McpToolDef> = Vec::new();
        let mut cursor: Option<String> = None;
        let mut seen_cursors: HashSet<String> = HashSet::new();

        loop {
            let params = cursor
                .as_ref()
                .map(|value| serde_json::json!({ "cursor": value }));

            let tools_result = self
                .request_with_timeout("tools/list", params, timeout)
                .await?;
            let tools_page: ToolsListResult = serde_json::from_value(tools_result)?;

            all_tools.extend(tools_page.tools);

            match tools_page.next_cursor {
                Some(next_cursor) => {
                    if !seen_cursors.insert(next_cursor.clone()) {
                        tracing::warn!(
                            "[MCP:{}] tools/list pagination returned duplicate cursor '{}'; stopping pagination loop",
                            self.name,
                            next_cursor
                        );
                        break;
                    }
                    cursor = Some(next_cursor);
                }
                None => break,
            }
        }

        Ok(all_tools)
    }

    /// Call a tool on the MCP server.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Option<serde_json::Value>,
    ) -> anyhow::Result<ToolCallResult> {
        let params = ToolCallParams {
            name: name.into(),
            arguments,
        };
        let result = self
            .request("tools/call", Some(serde_json::to_value(&params)?))
            .await?;
        let call_result: ToolCallResult = serde_json::from_value(result)?;
        Ok(call_result)
    }

    /// Call a tool on the MCP server, aborting immediately if `cancel` fires.
    ///
    /// On cancellation, the pending map entry is cleaned up and
    /// `Err("MCP tool call '<name>' cancelled")` is returned.  The MCP server
    /// process is NOT killed — it will finish its own work but the result is
    /// silently discarded once the timeout for the orphaned request expires.
    pub async fn call_tool_cancellable(
        &self,
        name: &str,
        arguments: Option<serde_json::Value>,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> anyhow::Result<ToolCallResult> {
        let params = ToolCallParams {
            name: name.into(),
            arguments,
        };
        let json_params = serde_json::to_value(&params)?;

        // Build the request future (which registers the pending entry) but do
        // NOT await it yet — we need to be able to cancel it.
        let request_fut = self.request("tools/call", Some(json_params));

        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                anyhow::bail!("MCP tool call '{}' cancelled", name);
            }
            result = request_fut => {
                let call_result: ToolCallResult = serde_json::from_value(result?)?;
                Ok(call_result)
            }
        }
    }

    /// Send a JSON-RPC request and await the response.
    async fn request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> anyhow::Result<serde_json::Value> {
        self.request_with_timeout(
            method,
            params,
            Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS),
        )
        .await
    }

    /// Send a JSON-RPC request and await the response with a custom timeout.
    async fn request_with_timeout(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout: Duration,
    ) -> anyhow::Result<serde_json::Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = JsonRpcRequest::new(id, method, params);

        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let mut line = serde_json::to_string(&req)?;
        line.push('\n');

        {
            let mut stdin_guard = self.stdin.lock().await;
            if let Some(ref mut stdin) = *stdin_guard {
                stdin.write_all(line.as_bytes()).await?;
                stdin.flush().await?;
            } else {
                self.pending.lock().await.remove(&id);
                anyhow::bail!("MCP server stdin not available");
            }
        }

        let pending_response = match tokio::time::timeout(timeout, rx).await {
            Ok(resp) => resp,
            Err(_) => {
                self.pending.lock().await.remove(&id);
                anyhow::bail!(
                    "MCP request '{}' timed out after {}s",
                    method,
                    timeout.as_secs()
                );
            }
        };

        let resp = match pending_response {
            Ok(resp) => resp,
            Err(_) => {
                self.pending.lock().await.remove(&id);
                anyhow::bail!(
                    "MCP request '{}' failed because the server exited before replying",
                    method
                );
            }
        };

        resp.into_result().map_err(|e| anyhow::anyhow!(e))
    }

    /// Send a JSON-RPC notification (no id, no response expected).
    async fn notify(&self, method: &str, params: Option<serde_json::Value>) -> anyhow::Result<()> {
        #[derive(serde::Serialize)]
        struct Notification {
            jsonrpc: String,
            method: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            params: Option<serde_json::Value>,
        }

        let notif = Notification {
            jsonrpc: "2.0".into(),
            method: method.into(),
            params,
        };

        let mut line = serde_json::to_string(&notif)?;
        line.push('\n');

        let mut stdin_guard = self.stdin.lock().await;
        if let Some(ref mut stdin) = *stdin_guard {
            stdin.write_all(line.as_bytes()).await?;
            stdin.flush().await?;
        }
        Ok(())
    }

    /// Gracefully shut down the MCP server.
    pub async fn stop(&self) -> anyhow::Result<()> {
        tracing::info!(server = %self.name, "stopping MCP server");

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

        *self.tools.lock().await = Vec::new();
        *self.state.lock().await = McpServerState::Stopped;
        Ok(())
    }

    /// Lightweight health ping — sends a `ping` request and expects `pong`.
    /// Returns true if the server responded within 5 seconds.
    pub async fn ping(&self) -> bool {
        if *self.state.lock().await != McpServerState::Running {
            return false;
        }
        match tokio::time::timeout(Duration::from_secs(5), self.request("ping", None)).await {
            Ok(Ok(_)) => true,
            // Some MCP servers don't implement ping — treat "method not found" as alive
            Ok(Err(e))
                if e.to_string().contains("Method not found")
                    || e.to_string().contains("-32601") =>
            {
                true
            }
            _ => false,
        }
    }

    /// Get and reset the restart count (for exponential backoff).
    pub fn restart_count(&self) -> u64 {
        self.restart_count.load(Ordering::Relaxed)
    }

    /// Increment restart count, returns the backoff delay in seconds (1, 2, 4, 8 … max 30).
    pub fn increment_restart(&self) -> u64 {
        let count = self.restart_count.fetch_add(1, Ordering::Relaxed);
        (1u64 << count).min(30)
    }

    /// Reset restart count (called after a successful start).
    pub fn reset_restart_count(&self) {
        self.restart_count.store(0, Ordering::Relaxed);
    }
}

impl std::fmt::Debug for McpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClient")
            .field("name", &self.name)
            .finish()
    }
}
