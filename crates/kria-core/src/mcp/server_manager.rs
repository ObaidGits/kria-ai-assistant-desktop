//! MCP server manager — manages multiple MCP server processes.
//!
//! Responsibilities:
//! - Load server configurations from McpConfig
//! - Start/stop individual servers
//! - Register discovered tools in the ToolRegistry
//! - Health monitoring with auto-restart on crash

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::config::McpServerConfig;
use crate::safety::RiskLevel;
use crate::tools::{ToolDef, ToolHandler, ToolRegistry};
use super::client::{McpClient, McpServerState};
use super::protocol::McpToolDef;
use super::tool_bridge::McpToolHandler;

/// Max consecutive ping failures before a restart attempt.
const MAX_PING_FAILURES: u64 = 3;

/// Manages all configured MCP servers.
pub struct McpServerManager {
    clients: HashMap<String, Arc<McpClient>>,
    configs: Vec<McpServerConfig>,
    /// Per-server consecutive ping failure count.
    ping_failures: HashMap<String, u64>,
}

impl McpServerManager {
    pub fn new(configs: Vec<McpServerConfig>) -> Self {
        Self {
            clients: HashMap::new(),
            configs,
            ping_failures: HashMap::new(),
        }
    }

    /// Start all enabled MCP servers and register their tools.
    /// Start all enabled MCP servers in parallel and register their tools.
    pub async fn start_all(&mut self, registry: &ToolRegistry) {
        let configs = self.configs.clone();
        let total = configs.len();
        let enabled: Vec<_> = configs.iter().filter(|c| c.enabled).cloned().collect();
        tracing::info!("[MCP] start_all: {} configured, {} enabled — launching in parallel", total, enabled.len());

        for config in &configs {
            if !config.enabled {
                tracing::info!("[MCP] server '{}' is disabled — skipping", config.name);
            }
        }

        // Launch all enabled servers concurrently
        let handles: Vec<_> = enabled.iter().map(|config| {
            let config = config.clone();
            tokio::spawn(async move {
                tracing::info!("[MCP] starting server '{}' (command='{}' args={:?})", config.name, config.command, config.args);
                let client = Arc::new(McpClient::new(&config.name));
                match client.start(&config.command, &config.args, &config.env).await {
                    Ok(()) => {
                        let tools = client.tools().await;
                        tracing::info!("[MCP] server '{}' started — {} tool(s) discovered", config.name, tools.len());
                        Ok((config, client, tools))
                    }
                    Err(e) => {
                        tracing::error!("[MCP] server '{}' FAILED to start: {}", config.name, e);
                        Err(config.name.clone())
                    }
                }
            })
        }).collect();

        // Collect results and register tools
        for handle in handles {
            match handle.await {
                Ok(Ok((config, client, tools))) => {
                    for tool_def in &tools {
                        let override_tier = config.tool_overrides.get(&tool_def.name)
                            .map(|s| s.as_str())
                            .unwrap_or("(default)");
                        tracing::info!("[MCP]   tool='{}' override={}", tool_def.name, override_tier);
                        register_mcp_tool(
                            registry,
                            &client,
                            &config.name,
                            tool_def,
                            &config.trust_level,
                            &config.tool_overrides,
                        );
                    }
                    tracing::info!(
                        "[MCP] server '{}' ready: {} tools registered (trust_level={})",
                        config.name, tools.len(), config.trust_level
                    );
                    self.clients.insert(config.name.clone(), client);
                }
                Ok(Err(name)) => {
                    tracing::error!("[MCP] server '{}' failed — skipped", name);
                }
                Err(e) => {
                    tracing::error!("[MCP] server spawn panicked: {}", e);
                }
            }
        }
        tracing::info!("[MCP] start_all complete — {} server(s) running", self.clients.len());
    }

    /// Start a single MCP server and register its tools.
    async fn start_server(
        &mut self,
        config: &McpServerConfig,
        registry: &ToolRegistry,
    ) -> anyhow::Result<()> {
        tracing::debug!("[MCP] creating McpClient for '{}'", config.name);
        let client = Arc::new(McpClient::new(&config.name));

        tracing::debug!("[MCP] calling client.start() for '{}'", config.name);
        client
            .start(&config.command, &config.args, &config.env)
            .await?;

        // Register each discovered tool with a prefixed name
        let tools = client.tools().await;
        tracing::info!("[MCP] server '{}' advertises {} tool(s):", config.name, tools.len());
        for tool_def in &tools {
            let override_tier = config.tool_overrides.get(&tool_def.name)
                .map(|s| s.as_str())
                .unwrap_or("(default)");
            tracing::info!("[MCP]   tool='{}' override={}", tool_def.name, override_tier);
            register_mcp_tool(
                registry,
                &client,
                &config.name,
                tool_def,
                &config.trust_level,
                &config.tool_overrides,
            );
        }

        tracing::info!(
            "[MCP] server '{}' ready: {} tools registered (trust_level={})",
            config.name, tools.len(), config.trust_level
        );

        self.clients.insert(config.name.clone(), client);
        Ok(())
    }

    /// Stop a specific MCP server.
    pub async fn stop_server(&mut self, name: &str) -> anyhow::Result<()> {
        if let Some(client) = self.clients.remove(name) {
            client.stop().await?;
        }
        Ok(())
    }

    /// Stop all MCP servers.
    pub async fn stop_all(&mut self) {
        let names: Vec<String> = self.clients.keys().cloned().collect();
        for name in names {
            if let Err(e) = self.stop_server(&name).await {
                tracing::warn!(server = %name, error = %e, "error stopping MCP server");
            }
        }
    }

    /// Get the status of all servers.
    pub async fn status(&self) -> Vec<McpServerStatus> {
        let mut statuses = Vec::new();
        for config in &self.configs {
            let (state, tool_count, error) = if let Some(client) = self.clients.get(&config.name) {
                let state = client.state().await;
                let tools = client.tools().await;
                let error = client.error().await;
                (state, tools.len(), error)
            } else {
                (McpServerState::Stopped, 0, None)
            };

            statuses.push(McpServerStatus {
                name: config.name.clone(),
                command: config.command.clone(),
                enabled: config.enabled,
                state,
                tool_count,
                error,
            });
        }
        statuses
    }

    /// Get a client by name (for direct tool calls).
    pub fn get_client(&self, name: &str) -> Option<&Arc<McpClient>> {
        self.clients.get(name)
    }

    /// Restart a crashed server.
    pub async fn restart_server(
        &mut self,
        name: &str,
        registry: &ToolRegistry,
    ) -> anyhow::Result<()> {
        self.stop_server(name).await?;
        let config = self
            .configs
            .iter()
            .find(|c| c.name == name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown MCP server: {}", name))?;
        self.start_server(&config, registry).await
    }

    /// Run one health-check cycle: ping every running server, restart failures.
    pub async fn health_check_cycle(&mut self, registry: &ToolRegistry) {
        let names: Vec<String> = self.clients.keys().cloned().collect();
        for name in names {
            let alive = if let Some(client) = self.clients.get(&name) {
                if client.state().await == McpServerState::Running {
                    client.ping().await
                } else {
                    false
                }
            } else {
                continue;
            };

            if alive {
                self.ping_failures.remove(&name);
            } else {
                let count = self.ping_failures.entry(name.clone()).or_insert(0);
                *count += 1;
                tracing::warn!(server = %name, failures = *count, "MCP server ping failed");

                if *count >= MAX_PING_FAILURES {
                    tracing::error!(server = %name, "MCP server unresponsive — restarting");

                    // Exponential backoff based on the client's restart count
                    let backoff_secs = self.clients.get(&name)
                        .map(|c| c.increment_restart())
                        .unwrap_or(1);
                    tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;

                    self.ping_failures.remove(&name);
                    if let Err(e) = self.restart_server(&name, registry).await {
                        tracing::error!(server = %name, error = %e, "MCP server restart failed");
                    }
                }
            }
        }
    }

    /// Spawn a background task that pings all MCP servers every `interval` seconds.
    /// Returns a JoinHandle that can be aborted on shutdown.
    pub fn spawn_health_heartbeat(
        manager: Arc<Mutex<McpServerManager>>,
        registry: Arc<ToolRegistry>,
        interval_secs: u64,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
            // Skip the first immediate tick (servers just started)
            interval.tick().await;
            loop {
                interval.tick().await;
                let mut mgr = manager.lock().await;
                mgr.health_check_cycle(&registry).await;
            }
        })
    }
}

/// Status summary for a single MCP server.
#[derive(Debug, Clone, serde::Serialize)]
pub struct McpServerStatus {
    pub name: String,
    pub command: String,
    pub enabled: bool,
    pub state: McpServerState,
    pub tool_count: usize,
    pub error: Option<String>,
}

// ── Helper: register a single MCP tool in the KRIA registry ─────────

fn register_mcp_tool(
    registry: &ToolRegistry,
    client: &Arc<McpClient>,
    server_name: &str,
    mcp_tool: &McpToolDef,
    default_trust: &str,
    overrides: &HashMap<String, String>,
) {
    let prefixed_name = format!("mcp_{}_{}", server_name, mcp_tool.name);

    let risk_level = if let Some(level_str) = overrides.get(&mcp_tool.name) {
        parse_risk_level(level_str)
    } else {
        parse_risk_level(default_trust)
    };

    // Convert MCP input_schema to KRIA ParamDefs
    let parameters = extract_params_from_schema(&mcp_tool.input_schema);

    let def = ToolDef {
        name: prefixed_name.clone(),
        description: mcp_tool
            .description
            .clone()
            .unwrap_or_else(|| format!("MCP tool: {}", mcp_tool.name)),
        category: format!("mcp_{}", server_name),
        parameters,
        default_tier: risk_level,
        min_tier: "standard",
    };

    let handler: Arc<dyn ToolHandler> =
        Arc::new(McpToolHandler::new(Arc::clone(client), &mcp_tool.name));

    registry.register(def, handler);
    tracing::debug!(tool = %prefixed_name, "registered MCP tool");
}

fn parse_risk_level(s: &str) -> RiskLevel {
    match s.to_uppercase().as_str() {
        "GREEN" => RiskLevel::Green,
        "YELLOW" => RiskLevel::Yellow,
        "RED" => RiskLevel::Red,
        "BLACK" => RiskLevel::Black,
        _ => RiskLevel::Yellow,
    }
}

fn extract_params_from_schema(schema: &serde_json::Value) -> Vec<crate::tools::registry::ParamDef> {
    let mut params = Vec::new();

    let properties = match schema.get("properties").and_then(|p| p.as_object()) {
        Some(p) => p,
        None => return params,
    };

    let required: Vec<&str> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    for (name, prop) in properties {
        let param_type = prop
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("string")
            .to_string();

        let description = prop
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or("")
            .to_string();

        params.push(crate::tools::registry::ParamDef {
            name: name.clone(),
            param_type,
            description,
            required: required.contains(&name.as_str()),
            default: None,
        });
    }

    params
}

impl std::fmt::Debug for McpServerManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServerManager")
            .field("servers", &self.clients.keys().collect::<Vec<_>>())
            .finish()
    }
}
