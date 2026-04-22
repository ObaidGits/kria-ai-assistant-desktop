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

use super::client::{McpClient, McpServerState};
use super::protocol::McpToolDef;
use super::tool_bridge::McpToolHandler;
use tokio::sync::broadcast;

use crate::config::McpServerConfig;
use crate::routing::cache::RouterCacheEvent;
use crate::safety::RiskLevel;
use crate::tools::{ToolDef, ToolHandler, ToolRegistry};

/// Max consecutive ping failures before a restart attempt.
const MAX_PING_FAILURES: u64 = 3;

/// Manages all configured MCP servers.
pub struct McpServerManager {
    clients: HashMap<String, Arc<McpClient>>,
    configs: Vec<McpServerConfig>,
    /// Per-server consecutive ping failure count.
    ping_failures: HashMap<String, u64>,
    /// Optional: notify the semantic router cache when tools change.
    router_event_tx: Option<broadcast::Sender<RouterCacheEvent>>,
}

/// Summary of one reconcile pass between configured and runtime MCP servers.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct McpReconcileReport {
    pub started: Vec<String>,
    pub stopped: Vec<String>,
    pub restarted: Vec<String>,
    pub unchanged: Vec<String>,
    pub errors: Vec<String>,
}

impl McpServerManager {
    pub fn new(configs: Vec<McpServerConfig>) -> Self {
        Self {
            clients: HashMap::new(),
            configs,
            ping_failures: HashMap::new(),
            router_event_tx: None,
        }
    }

    /// Attach a sender so the router cache is invalidated when MCP tools change.
    pub fn with_router_event_sender(mut self, tx: broadcast::Sender<RouterCacheEvent>) -> Self {
        self.router_event_tx = Some(tx);
        self
    }

    fn notify_tools_changed(&self) {
        if let Some(tx) = &self.router_event_tx {
            let _ = tx.send(RouterCacheEvent::ToolsChanged);
        }
    }

    /// Start all enabled MCP servers and register their tools.
    /// Start all enabled MCP servers in parallel and register their tools.
    pub async fn start_all(&mut self, registry: &ToolRegistry) {
        let configs = self.configs.clone();
        let total = configs.len();
        let enabled: Vec<_> = configs.iter().filter(|c| c.enabled).cloned().collect();
        tracing::info!(
            "[MCP] start_all: {} configured, {} enabled — launching in parallel",
            total,
            enabled.len()
        );

        for config in &configs {
            if !config.enabled {
                tracing::info!("[MCP] server '{}' is disabled — skipping", config.name);
            }
        }

        // Launch all enabled servers concurrently
        let handles: Vec<_> = enabled
            .iter()
            .map(|config| {
                let config = config.clone();
                tokio::spawn(async move {
                    tracing::info!(
                        "[MCP] starting server '{}' (command='{}' args={:?})",
                        config.name,
                        config.command,
                        config.args
                    );
                    let client = Arc::new(McpClient::new(&config.name));
                    let mut last_err: Option<anyhow::Error> = None;

                    for attempt in 1..=2 {
                        match client
                            .start(&config.command, &config.args, &config.env)
                            .await
                        {
                            Ok(()) => {
                                let tools = client.tools().await;
                                tracing::info!(
                                    "[MCP] server '{}' started — {} tool(s) discovered",
                                    config.name,
                                    tools.len()
                                );
                                return Ok((config, client, tools));
                            }
                            Err(e) => {
                                let msg = e.to_string();
                                tracing::warn!(
                                    "[MCP] server '{}' start attempt {}/2 failed: {}",
                                    config.name,
                                    attempt,
                                    msg
                                );
                                last_err = Some(e);

                                if attempt < 2 {
                                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                                }
                            }
                        }
                    }

                    if let Some(err) = last_err {
                        tracing::error!("[MCP] server '{}' FAILED to start: {}", config.name, err);
                    }
                    Err(config.name.clone())
                })
            })
            .collect();

        // Collect results and register tools
        for handle in handles {
            match handle.await {
                Ok(Ok((config, client, tools))) => {
                    for tool_def in &tools {
                        let override_tier = config
                            .tool_overrides
                            .get(&tool_def.name)
                            .map(|s| s.as_str())
                            .unwrap_or("(default)");
                        tracing::info!(
                            "[MCP]   tool='{}' override={}",
                            tool_def.name,
                            override_tier
                        );
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
                        config.name,
                        tools.len(),
                        config.trust_level
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
        tracing::info!(
            "[MCP] start_all complete — {} server(s) running",
            self.clients.len()
        );
        self.notify_tools_changed();
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
        tracing::info!(
            "[MCP] server '{}' advertises {} tool(s):",
            config.name,
            tools.len()
        );
        for tool_def in &tools {
            let override_tier = config
                .tool_overrides
                .get(&tool_def.name)
                .map(|s| s.as_str())
                .unwrap_or("(default)");
            tracing::info!(
                "[MCP]   tool='{}' override={}",
                tool_def.name,
                override_tier
            );
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
            config.name,
            tools.len(),
            config.trust_level
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

    /// Refresh tools for a running server and re-register its MCP category.
    pub async fn refresh_server_tools(
        &mut self,
        name: &str,
        registry: &ToolRegistry,
    ) -> anyhow::Result<usize> {
        let client = self
            .clients
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown or stopped MCP server: {}", name))?;

        let config = self
            .configs
            .iter()
            .find(|cfg| cfg.name == name)
            .ok_or_else(|| anyhow::anyhow!("missing MCP config for server: {}", name))?
            .clone();

        if client.state().await != McpServerState::Running {
            anyhow::bail!("MCP server '{}' is not running", name);
        }

        let tools = client.refresh_tools().await?;
        let category = format!("mcp_{}", name);
        let removed = registry.unregister_category(&category);

        for tool_def in &tools {
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
            server = %name,
            removed,
            registered = tools.len(),
            "refreshed MCP server tools"
        );
        self.notify_tools_changed();

        Ok(tools.len())
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

    fn config_requires_restart(old: &McpServerConfig, new: &McpServerConfig) -> bool {
        old.command != new.command
            || old.args != new.args
            || old.env != new.env
            || old.trust_level != new.trust_level
            || old.tool_overrides != new.tool_overrides
    }

    /// Reconcile runtime MCP processes with the desired config list.
    ///
    /// Behavior:
    /// - Stops running servers that are disabled or removed from config
    /// - Starts newly-enabled servers
    /// - Restarts running servers when launch/runtime config changed
    pub async fn reconcile(
        &mut self,
        desired_configs: Vec<McpServerConfig>,
        registry: &ToolRegistry,
    ) -> McpReconcileReport {
        let previous_configs: HashMap<String, McpServerConfig> = self
            .configs
            .iter()
            .cloned()
            .map(|cfg| (cfg.name.clone(), cfg))
            .collect();

        let desired_by_name: HashMap<String, McpServerConfig> = desired_configs
            .iter()
            .cloned()
            .map(|cfg| (cfg.name.clone(), cfg))
            .collect();

        let mut report = McpReconcileReport::default();

        // Stop runtime servers that should no longer run.
        let running_names: Vec<String> = self.clients.keys().cloned().collect();
        for name in running_names {
            let should_run = desired_by_name
                .get(&name)
                .map(|cfg| cfg.enabled)
                .unwrap_or(false);

            if !should_run {
                match self.stop_server(&name).await {
                    Ok(()) => {
                        self.ping_failures.remove(&name);
                        report.stopped.push(name);
                    }
                    Err(e) => report.errors.push(format!("failed to stop '{name}': {e}")),
                }
            }
        }

        // Ensure all enabled configs are running and up-to-date.
        for cfg in desired_configs.iter().filter(|cfg| cfg.enabled) {
            let is_running = self.clients.contains_key(&cfg.name);

            if !is_running {
                match self.start_server(cfg, registry).await {
                    Ok(()) => report.started.push(cfg.name.clone()),
                    Err(e) => report
                        .errors
                        .push(format!("failed to start '{}': {e}", cfg.name)),
                }
                continue;
            }

            let restart_required = previous_configs
                .get(&cfg.name)
                .map(|old| Self::config_requires_restart(old, cfg))
                .unwrap_or(true);

            if restart_required {
                if let Err(e) = self.stop_server(&cfg.name).await {
                    report
                        .errors
                        .push(format!("failed to restart '{}': stop error: {e}", cfg.name));
                    continue;
                }

                match self.start_server(cfg, registry).await {
                    Ok(()) => report.restarted.push(cfg.name.clone()),
                    Err(e) => report.errors.push(format!(
                        "failed to restart '{}': start error: {e}",
                        cfg.name
                    )),
                }
            } else {
                report.unchanged.push(cfg.name.clone());
            }
        }

        self.configs = desired_configs;
        report
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
                    let backoff_secs = self
                        .clients
                        .get(&name)
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

    let handler: Arc<dyn ToolHandler> = Arc::new(McpToolHandler::new(
        Arc::clone(client),
        server_name,
        &mcp_tool.name,
    ));

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

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(name: &str, enabled: bool, command: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            command: command.to_string(),
            args: vec![],
            env: HashMap::new(),
            enabled,
            trust_level: "YELLOW".into(),
            tool_overrides: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn reconcile_attempts_start_for_new_enabled_server() {
        let mut manager = McpServerManager::new(vec![]);
        let registry = ToolRegistry::new();

        let report = manager
            .reconcile(
                vec![cfg(
                    "alpha",
                    true,
                    "__kria_missing_command_for_start_test__",
                )],
                &registry,
            )
            .await;

        assert!(report.started.is_empty());
        assert!(report
            .errors
            .iter()
            .any(|e| e.contains("failed to start 'alpha'")));
    }

    #[tokio::test]
    async fn reconcile_stops_disabled_running_server() {
        let mut manager = McpServerManager::new(vec![cfg("alpha", true, "old")]);
        manager
            .clients
            .insert("alpha".into(), Arc::new(McpClient::new("alpha")));

        let registry = ToolRegistry::new();
        let report = manager
            .reconcile(vec![cfg("alpha", false, "old")], &registry)
            .await;

        assert_eq!(report.stopped, vec!["alpha".to_string()]);
        assert!(report.errors.is_empty());
        assert!(!manager.clients.contains_key("alpha"));
    }

    #[tokio::test]
    async fn reconcile_attempts_restart_when_runtime_config_changes() {
        let mut manager = McpServerManager::new(vec![cfg("alpha", true, "old")]);
        manager
            .clients
            .insert("alpha".into(), Arc::new(McpClient::new("alpha")));

        let registry = ToolRegistry::new();
        let report = manager
            .reconcile(
                vec![cfg(
                    "alpha",
                    true,
                    "__kria_missing_command_for_restart_test__",
                )],
                &registry,
            )
            .await;

        assert!(report.restarted.is_empty());
        assert!(report
            .errors
            .iter()
            .any(|e| e.contains("failed to restart 'alpha': start error")));
    }

    #[tokio::test]
    async fn reconcile_marks_running_server_unchanged_when_config_matches() {
        let baseline = cfg("alpha", true, "same");
        let mut manager = McpServerManager::new(vec![baseline.clone()]);
        manager
            .clients
            .insert("alpha".into(), Arc::new(McpClient::new("alpha")));

        let registry = ToolRegistry::new();
        let report = manager.reconcile(vec![baseline], &registry).await;

        assert_eq!(report.unchanged, vec!["alpha".to_string()]);
        assert!(report.errors.is_empty());
        assert!(manager.clients.contains_key("alpha"));
    }
}
