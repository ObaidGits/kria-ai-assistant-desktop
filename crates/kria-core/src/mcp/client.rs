//! MCP client — manages a single MCP server process over stdio.
//!
//! Handles:
//! - Spawning the server process
//! - JSON-RPC initialize handshake
//! - tools/list, tools/call
//! - Graceful shutdown

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, oneshot};

use super::protocol::*;

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
        tracing::info!(server = %self.name, command = %command, "starting MCP server");

        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        for (k, v) in env {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn()?;

        let stdout = child.stdout.take().ok_or_else(|| anyhow::anyhow!("no stdout"))?;
        let stderr = child.stderr.take().ok_or_else(|| anyhow::anyhow!("no stderr"))?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow::anyhow!("no stdin"))?;

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
        let reader_handle = tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
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
                                tracing::warn!("failed to parse MCP response: {}: {}", e, &trimmed[..trimmed.len().min(200)]);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!("MCP stdout read error: {}", e);
                        break;
                    }
                }
            }
        });

        *self.reader_task.lock().await = Some(reader_handle);

        // MCP initialize handshake
        let init_params = InitializeParams {
            protocol_version: "2024-11-05".into(),
            capabilities: ClientCapabilities::default(),
            client_info: ClientInfo {
                name: "kria".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
        };

        let result = self
            .request("initialize", Some(serde_json::to_value(&init_params)?))
            .await?;

        let init_result: InitializeResult = serde_json::from_value(result)?;
        *self.server_info.lock().await = init_result.server_info.clone();

        tracing::info!(
            server = %self.name,
            protocol = %init_result.protocol_version,
            server_name = ?init_result.server_info.as_ref().map(|s| &s.name),
            "MCP initialize complete"
        );

        // Send initialized notification (no id — it's a notification)
        self.notify("notifications/initialized", None).await?;

        // Discover tools if the server supports them
        if init_result.capabilities.tools.is_some() {
            let tools_result = self.request("tools/list", None).await?;
            let tools_list: ToolsListResult = serde_json::from_value(tools_result)?;
            tracing::info!(
                server = %self.name,
                count = tools_list.tools.len(),
                "discovered MCP tools"
            );
            *self.tools.lock().await = tools_list.tools;
        }

        *self.state.lock().await = McpServerState::Running;
        Ok(())
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

    /// Send a JSON-RPC request and await the response.
    async fn request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
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

        let resp = tokio::time::timeout(Duration::from_secs(60), rx)
            .await
            .map_err(|_| anyhow::anyhow!("MCP request timed out after 60s"))?
            .map_err(|_| anyhow::anyhow!("MCP response channel closed"))?;

        resp.into_result().map_err(|e| anyhow::anyhow!(e))
    }

    /// Send a JSON-RPC notification (no id, no response expected).
    async fn notify(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> anyhow::Result<()> {
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
}

impl std::fmt::Debug for McpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClient")
            .field("name", &self.name)
            .finish()
    }
}
