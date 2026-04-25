use async_trait::async_trait;
use axum::{
    extract::State as AxumState,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use kria_core::agent::loop_engine::{
    PromptLabToolSelectionStrategy, StreamEvent, TurnExecutionMode, TurnExecutionProfile,
};
use kria_core::agent::AgentLoop;
use kria_core::automation::{AutomationScheduler, MacroRecorder, WorkflowEngine};
use kria_core::config::{ColabConfig, KriaConfig};
use kria_core::infra::health::{HealthRegistry, ServiceStatus};
use kria_core::infra::EventBus;
use kria_core::llm::model_router::RoutingMode;
use kria_core::llm::orchestrator::Orchestrator;
use kria_core::llm::{ChatMessage, ImageAttachment, ModelRouter};
use kria_core::mcp::client::McpServerState;
use kria_core::mcp::server_manager::McpServerStatus;
use kria_core::mcp::{build_colab_capability_summary, McpServerManager};
use kria_core::memory::embeddings::EmbeddingModel;
use kria_core::memory::store::ConversationTurn;
use kria_core::memory::vectors::VectorIndex;
use kria_core::memory::MemoryStore;
use kria_core::platform::detect::{
    detect_hardware, get_available_package_managers, HardwareInfo, HardwareTier,
};
use kria_core::safety::hitl::{ApprovalResponse, HitlGateway};
use kria_core::safety::{AuditLogger, PolicyEngine, RollbackManager};
use kria_core::sidecar::SidecarBridge;
use kria_core::tools::google_workspace as gw;
use kria_core::tools::google_workspace_contract as gw_contract;
use kria_core::tools::mount_manager;
use kria_core::tools::registry::{self, ToolRegistry};
use kria_core::image::ImageOrchestrator;
use kria_core::voice::{
    default_input_device_name, default_output_device_name, list_input_devices, list_output_devices,
    SpeechToText, TextToSpeech, VoicePipeline, VoicePipelineEvent, VoicePipelineState,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::RwLock;

use kria_core::platform::telegram::TelegramBridge;

const AGENT_EVENT_IDLE_TIMEOUT_SECS: u64 = 180;
const AGENT_TIMEOUT_MESSAGE: &str = "⚠️ Timed out waiting for model output. Please verify the model runtime is healthy and try again.";
const IMAGE_PREANALYSIS_TIMEOUT_SECS: u64 = 35;
const IMAGE_SAFE_MAX_ATTACHMENTS_PER_TURN: usize = 1;
const IMAGE_SAFE_MAX_B64_CHARS_2K_CTX: usize = 550_000;
const IMAGE_SAFE_MAX_B64_CHARS_4K_CTX: usize = 900_000;
const GOOGLE_MCP_CONFIG_DIR_ENV: &str = "GOOGLE_MCP_CONFIG_DIR";
const GOOGLE_ACCOUNT_ENV_KEY: &str = "KRIA_GW_ACCOUNT";
const GOOGLE_DEFAULT_ACCOUNT: &str = "personal";
const COLAB_DEFAULT_SERVER_NAME: &str = "colab-mcp";
const COLAB_LEGACY_NPX_COMMAND: &str = "npx";
const COLAB_LEGACY_NPX_PACKAGE: &str = "@googlecolab/colab-mcp";
const COLAB_OFFICIAL_COMMAND: &str = "uvx";
const COLAB_OFFICIAL_SOURCE: &str = "git+https://github.com/googlecolab/colab-mcp";
const COLAB_BROWSER_BOOTSTRAP_TOOL: &str = "open_colab_browser_connection";

fn is_colab_bootstrap_tool_name(tool_name: &str) -> bool {
    tool_name
        .to_ascii_lowercase()
        .ends_with(COLAB_BROWSER_BOOTSTRAP_TOOL)
}

fn is_likely_local_llm_transport_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("local llm transport error")
        || lower.contains("error sending request for url")
        || lower.contains("connection refused")
        || lower.contains("tcp connect")
        || lower.contains("dns error")
        || lower.contains("timed out")
        || lower.contains("connection reset")
        || lower.contains("broken pipe")
}

async fn touch_orchestrator_activity(
    last_activity: &Arc<tokio::sync::Mutex<std::time::Instant>>,
) {
    let mut lock = last_activity.lock().await;
    *lock = std::time::Instant::now();
}

fn decrement_active_turn_counter(active_turns: &Arc<std::sync::atomic::AtomicUsize>) {
    let previous = active_turns.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    if previous == 0 {
        active_turns.store(0, std::sync::atomic::Ordering::SeqCst);
    }
}

async fn ensure_orchestrator_ready_for_turn(
    orchestrator: Option<&Arc<Orchestrator>>,
    reason: &str,
) -> Result<(), String> {
    if let Some(orchestrator) = orchestrator {
        orchestrator
            .ensure_ready(reason)
            .await
            .map_err(|e| format!("Local model runtime is unavailable: {e}"))?;
    }
    Ok(())
}

fn summarize_colab_dispatch_reason(status_payload: &serde_json::Value) -> String {
    let mut reasons: Vec<String> = status_payload
        .get("warnings")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();

    let missing: Vec<String> = status_payload
        .get("capabilities")
        .and_then(|v| v.get("ready_requirements"))
        .and_then(|v| v.get("missing"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();

    if !missing.is_empty() {
        reasons.push(format!("missing capabilities: {}", missing.join(", ")));
    }

    if reasons.is_empty() {
        let runtime_state = status_payload
            .get("runtime_state")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        reasons.push(format!("runtime_state={runtime_state}"));
    }

    reasons.join("; ")
}

async fn enforce_colab_dispatch_requirements(
    state: &AppState,
    app: &AppHandle,
) -> Result<(), String> {
    let requested_mode = {
        let config = state.config.read().await;
        config
            .llm
            .routing_mode
            .parse::<RoutingMode>()
            .unwrap_or(RoutingMode::Local)
    };

    state.model_router.set_mode(requested_mode).await;

    if requested_mode != RoutingMode::Colab {
        return Ok(());
    }

    let status_payload = collect_colab_tier_status(state).await;
    let ready_for_cloud_task = status_payload
        .get("ready_for_cloud_task")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if ready_for_cloud_task {
        emit_agent_stage(
            app,
            "colab_dispatch_ready",
            "Colab tier requirements are satisfied",
            Some(serde_json::json!({
                "requested_mode": "colab",
                "effective_mode": "colab",
                "ready_for_cloud_task": true,
            })),
        );
        emit_colab_status_event(app, state).await;
        return Ok(());
    }

    let reason = summarize_colab_dispatch_reason(&status_payload);
    let fallback_to_local = status_payload
        .get("fallback_to_local")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let runtime_state = status_payload
        .get("runtime_state")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let capability_requirements = status_payload
        .get("capabilities")
        .and_then(|v| v.get("ready_requirements"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    if fallback_to_local {
        state.model_router.set_mode(RoutingMode::Local).await;

        emit_agent_stage(
            app,
            "colab_dispatch_fallback_local",
            "Colab tier requirements were not satisfied; using local fallback",
            Some(serde_json::json!({
                "reason": reason,
                "runtime_state": runtime_state,
                "capability_requirements": capability_requirements,
                "requested_mode": "colab",
                "effective_mode": "local",
                "ready_for_cloud_task": false,
                "fallback_to_local": fallback_to_local,
            })),
        );

        emit_colab_status_event(app, state).await;

        tracing::warn!(
            reason = %reason,
            "colab dispatch requirements not satisfied; using local fallback"
        );
        Ok(())
    } else {
        emit_agent_stage(
            app,
            "colab_dispatch_blocked",
            "Colab tier requirements were not satisfied and fallback is disabled",
            Some(serde_json::json!({
                "reason": reason,
                "runtime_state": runtime_state,
                "capability_requirements": capability_requirements,
                "requested_mode": "colab",
                "effective_mode": "colab",
                "ready_for_cloud_task": false,
                "fallback_to_local": fallback_to_local,
            })),
        );

        emit_colab_status_event(app, state).await;

        Err(format!(
            "Colab tier is not ready for cloud execution and local fallback is disabled: {}",
            reason
        ))
    }
}

fn build_tool_only_fallback_message(name: &str, success: bool, result: &serde_json::Value) -> String {
    let metadata = compute_tool_result_metadata(name, result);
    let summary = summarize_tool_turn_for_history(name, success, result, &metadata);

    if success {
        format!(
            "{summary}\n\n⚠️ Local model became unavailable while preparing the final response. Tool output above is complete."
        )
    } else {
        format!(
            "{summary}\n\n⚠️ Local model became unavailable after a tool failure."
        )
    }
}

// 1x1 white PPM probe image used for sidecar OCR capability checks.
const OCR_HEALTH_PROBE_IMAGE_BYTES: &[u8] = b"P3\n1 1\n255\n255 255 255\n";

#[derive(Debug)]
struct OcrProbeState {
    in_flight: bool,
    next_allowed_at: std::time::Instant,
    consecutive_failures: u32,
}

impl Default for OcrProbeState {
    fn default() -> Self {
        Self {
            in_flight: false,
            next_allowed_at: std::time::Instant::now(),
            consecutive_failures: 0,
        }
    }
}

static OCR_PROBE_STATE: std::sync::OnceLock<tokio::sync::Mutex<OcrProbeState>> =
    std::sync::OnceLock::new();

fn ocr_probe_state() -> &'static tokio::sync::Mutex<OcrProbeState> {
    OCR_PROBE_STATE.get_or_init(|| tokio::sync::Mutex::new(OcrProbeState::default()))
}

async fn finalize_ocr_probe_schedule(success: bool) {
    let mut state = ocr_probe_state().lock().await;
    state.in_flight = false;
    if success {
        state.consecutive_failures = 0;
        state.next_allowed_at = std::time::Instant::now() + std::time::Duration::from_secs(30);
    } else {
        let failures = state.consecutive_failures.saturating_add(1).min(6);
        state.consecutive_failures = failures;
        let backoff_secs = (10u64.saturating_mul(1u64 << (failures.saturating_sub(1)))).min(300);
        state.next_allowed_at =
            std::time::Instant::now() + std::time::Duration::from_secs(backoff_secs);
    }
}

fn encode_base64_bytes(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

fn build_native_preprocessed_attachment(path: &str) -> Option<ImageAttachment> {
    build_native_preprocessed_attachment_with_max(path, 768)
}

fn build_native_preprocessed_attachment_with_max(path: &str, max_dim: u32) -> Option<ImageAttachment> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return None;
    }

    let path_obj = Path::new(trimmed);
    if !path_obj.exists() {
        return None;
    }

    // Native fallback preprocessing: generate a normalized PNG thumbnail.
    let thumb_bytes =
        kria_core::preprocessing::image::ImageProcessor::thumbnail(path_obj, max_dim).ok()?;

    Some(ImageAttachment {
        data: encode_base64_bytes(&thumb_bytes),
        mime_type: "image/png".to_string(),
    })
}

/// Find a binary on the system PATH.
fn which_binary(name: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(name))
            .find(|p| p.exists())
    })
}

fn local_api_base_url(host: &str, port: u16) -> String {
    let probe_host = match host {
        "0.0.0.0" | "::" => "127.0.0.1",
        other => other,
    };
    format!("http://{probe_host}:{port}")
}

fn build_tool_descriptions_for_prompt(tool_defs: &[registry::ToolDef]) -> String {
    // Categories whose tools are so numerous that listing them individually
    // would crowd out other categories. They are collapsed into a single
    // summary line so important tools (image, internet, shell, …) always appear.
    const COLLAPSED_CATEGORIES: &[&str] = &["google_workspace"];

    // Minimum number of lines reserved for non-collapsed tools.
    const MAX_TOOL_LINES: usize = 80;

    // Separate collapsed from normal tools.
    let mut collapsed_groups: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    let mut normal_defs: Vec<registry::ToolDef> = Vec::new();

    for def in tool_defs {
        if COLLAPSED_CATEGORIES.contains(&def.category.as_str()) {
            collapsed_groups
                .entry(def.category.clone())
                .or_default()
                .push(def.name.clone());
        } else {
            normal_defs.push(def.clone());
        }
    }

    // Sort non-collapsed tools: category then name.
    normal_defs.sort_by(|a, b| a.category.cmp(&b.category).then(a.name.cmp(&b.name)));

    let total = tool_defs.len();
    let visible_defs: Vec<registry::ToolDef> = normal_defs
        .into_iter()
        .take(MAX_TOOL_LINES)
        .collect();
    let omitted = total.saturating_sub(
        visible_defs.len() + collapsed_groups.values().map(|v| v.len()).sum::<usize>(),
    );

    let mut lines = Vec::with_capacity(visible_defs.len() + collapsed_groups.len() + 4);
    lines.push(format!(
        "You can call {} tools via function-calling. Use tool schemas for exact arguments.",
        total
    ));
    lines.push("Tool catalog (name [category]: summary):".to_string());
    if omitted > 0 {
        lines.push(format!(
            "{} additional low-priority tools are available via function schemas.",
            omitted
        ));
    }

    // Emit collapsed category summaries first.
    for (cat, names) in &collapsed_groups {
        lines.push(format!(
            "- [{}]: {} tools ({}) — call any by exact name via tool schema.",
            cat,
            names.len(),
            names.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
                + if names.len() > 5 { ", …" } else { "" }
        ));
    }

    // Emit individual tool lines.
    for def in visible_defs {
        let mut line = format!(
            "- {} [{}]: {}",
            def.name,
            def.category,
            kria_core::infra::pipeline_trace::sanitize_text_for_logs(&def.description, 96)
        );

        let param_names: Vec<&str> = def
            .parameters
            .iter()
            .take(3)
            .map(|p| p.name.as_str())
            .collect();
        if !param_names.is_empty() {
            if def.parameters.len() > 3 {
                line.push_str(&format!(" | params: {}, ...", param_names.join(", ")));
            } else {
                line.push_str(&format!(" | params: {}", param_names.join(", ")));
            }
        }

        lines.push(line);
    }

    lines.join("\n")
}

fn telegram_api_url(config: &KriaConfig) -> String {
    format!("http://{}:{}", config.server.host, config.server.port)
}

fn update_server_env_var(
    env: &mut std::collections::HashMap<String, String>,
    key: &str,
    value: Option<String>,
) -> bool {
    match value.filter(|v| !v.trim().is_empty()) {
        Some(next) => {
            if env.get(key) == Some(&next) {
                false
            } else {
                env.insert(key.to_string(), next);
                true
            }
        }
        None => env.remove(key).is_some(),
    }
}

fn should_manage_local_telegram_api_url(current: Option<&String>) -> bool {
    current
        .map(|url| {
            let lower = url.to_ascii_lowercase();
            lower.contains("127.0.0.1") || lower.contains("localhost") || lower.contains("0.0.0.0")
        })
        .unwrap_or(true)
}

fn sync_telegram_mcp_server_config(config: &mut KriaConfig) -> bool {
    let mut changed = false;
    let desired_enabled = config.telegram.enabled;
    let desired_bot_token = config.telegram.bot_token.clone();
    let desired_chat_ids = config.telegram.allowed_chat_ids.clone();
    let desired_api_url = telegram_api_url(config);

    if let Some(server) = config
        .mcp
        .servers
        .iter_mut()
        .find(|s| s.name.eq_ignore_ascii_case("telegram"))
    {
        if server.enabled != desired_enabled {
            server.enabled = desired_enabled;
            changed = true;
        }

        changed |= update_server_env_var(
            &mut server.env,
            "TELEGRAM_BOT_TOKEN",
            Some(desired_bot_token),
        );
        changed |=
            update_server_env_var(&mut server.env, "TELEGRAM_CHAT_IDS", Some(desired_chat_ids));

        if should_manage_local_telegram_api_url(server.env.get("KRIA_API_URL")) {
            changed |=
                update_server_env_var(&mut server.env, "KRIA_API_URL", Some(desired_api_url));
        }
    }

    changed
}

fn default_google_mcp_config_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    PathBuf::from(home).join(".google-mcp")
}

fn configured_google_workspace_server(
    config: &KriaConfig,
) -> Option<&kria_core::config::McpServerConfig> {
    config
        .mcp
        .servers
        .iter()
        .find(|s| s.name.eq_ignore_ascii_case("gworkspace"))
}

fn google_mcp_config_dir_from_config(config: &KriaConfig) -> PathBuf {
    configured_google_workspace_server(config)
        .and_then(|server| server.env.get(GOOGLE_MCP_CONFIG_DIR_ENV).cloned())
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(default_google_mcp_config_dir)
}

fn google_account_from_config(config: &KriaConfig) -> String {
    configured_google_workspace_server(config)
        .and_then(|server| server.env.get(GOOGLE_ACCOUNT_ENV_KEY).cloned())
        .filter(|v| !v.trim().is_empty())
        .or_else(|| std::env::var(GOOGLE_ACCOUNT_ENV_KEY).ok())
        .unwrap_or_else(|| GOOGLE_DEFAULT_ACCOUNT.into())
}

fn apply_google_runtime_env_from_config(config: &KriaConfig) {
    let account = google_account_from_config(config);
    let config_dir = google_mcp_config_dir_from_config(config);

    std::env::set_var(GOOGLE_ACCOUNT_ENV_KEY, account);
    std::env::set_var(
        GOOGLE_MCP_CONFIG_DIR_ENV,
        config_dir.to_string_lossy().to_string(),
    );
}

fn sync_google_workspace_server_config(config: &mut KriaConfig, account: Option<&str>) -> bool {
    let mut changed = false;
    let desired_account = account
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string())
        .unwrap_or_else(|| google_account_from_config(config));

    if let Some(server) = config
        .mcp
        .servers
        .iter_mut()
        .find(|s| s.name.eq_ignore_ascii_case("gworkspace"))
    {
        changed |= update_server_env_var(
            &mut server.env,
            GOOGLE_ACCOUNT_ENV_KEY,
            Some(desired_account),
        );
    }

    changed
}

#[derive(Debug, Clone, serde::Deserialize)]
struct LocalApiChatRequest {
    message: String,
    session_id: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    chat_id: Option<i64>,
    #[serde(default)]
    from_user: Option<String>,
}

#[async_trait]
trait LocalApiResponder: Send + Sync {
    async fn respond(&self, request: &LocalApiChatRequest) -> serde_json::Value;
}

#[derive(Clone)]
struct LocalApiBridgeState {
    responder: Arc<dyn LocalApiResponder>,
}

#[derive(Clone)]
struct AgentLoopLocalApiResponder {
    agent_loop: Arc<AgentLoop>,
    memory_store: Arc<MemoryStore>,
    tool_registry: Arc<ToolRegistry>,
    embeddings: Arc<EmbeddingModel>,
    vectors: Arc<VectorIndex>,
    hw_tier: String,
    orchestrator: Arc<tokio::sync::RwLock<Option<Arc<Orchestrator>>>>,
}

#[async_trait]
impl LocalApiResponder for AgentLoopLocalApiResponder {
    async fn respond(&self, request: &LocalApiChatRequest) -> serde_json::Value {
        let chat_id = request.chat_id.unwrap_or(0);
        let from_user = request.from_user.as_deref().unwrap_or("User");
        let orc_snapshot = self.orchestrator.read().await.clone();
        let reply = kria_core::platform::telegram::process_message(
            &request.message,
            chat_id,
            from_user,
            &self.agent_loop,
            &self.memory_store,
            &self.tool_registry,
            &self.embeddings,
            &self.vectors,
            &self.hw_tier,
            orc_snapshot.as_ref(),
            // Local API bridge is always the owner — it runs inside the desktop
            // process and is not accessible to external callers.
            true,
        )
        .await;

        let session_id = request.session_id.clone().unwrap_or_else(|| {
            if request.chat_id.is_some() || request.source.as_deref() == Some("telegram") {
                format!("telegram_{chat_id}")
            } else {
                uuid::Uuid::new_v4().to_string()
            }
        });

        serde_json::json!({
            "status": "received",
            "message": request.message,
            "source": request.source.clone().unwrap_or_else(|| "api".to_string()),
            "chat_id": request.chat_id,
            "from_user": request.from_user,
            "session_id": session_id,
            "reply": reply,
        })
    }
}

async fn local_api_health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "healthy",
        "bridge": "desktop",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn local_api_chat(
    AxumState(state): AxumState<LocalApiBridgeState>,
    Json(request): Json<LocalApiChatRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    if request.message.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "status": "error",
                "message": "message is required",
            })),
        );
    }

    let response = state.responder.respond(&request).await;
    (StatusCode::OK, Json(response))
}

async fn probe_existing_local_api_bridge(health_url: &str) -> bool {
    match reqwest::Client::new()
        .get(health_url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

fn start_local_api_bridge(
    host: String,
    port: u16,
    responder: Arc<dyn LocalApiResponder>,
    health: Arc<HealthRegistry>,
) {
    let bind_addr = format!("{host}:{port}");
    let health_url = format!("{}/api/health", local_api_base_url(&host, port));
    health.register("local_api_bridge");
    health.update(
        "local_api_bridge",
        ServiceStatus::Starting,
        Some(format!("binding {bind_addr}")),
    );

    tokio::spawn(async move {
        match tokio::net::TcpListener::bind(&bind_addr).await {
            Ok(listener) => {
                let router = Router::new()
                    .route("/api/health", get(local_api_health))
                    .route("/api/chat", post(local_api_chat))
                    .with_state(LocalApiBridgeState { responder });

                health.update(
                    "local_api_bridge",
                    ServiceStatus::Healthy,
                    Some(format!("listening on {health_url}")),
                );

                if let Err(e) = axum::serve(listener, router).await {
                    health.update(
                        "local_api_bridge",
                        ServiceStatus::Degraded,
                        Some(format!("bridge stopped: {e}")),
                    );
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                if probe_existing_local_api_bridge(&health_url).await {
                    health.update(
                        "local_api_bridge",
                        ServiceStatus::Healthy,
                        Some(format!("reusing existing listener at {health_url}")),
                    );
                } else {
                    health.update(
                        "local_api_bridge",
                        ServiceStatus::Degraded,
                        Some(format!(
                            "{bind_addr} already in use, but {health_url} is not responding"
                        )),
                    );
                }
            }
            Err(e) => {
                health.update(
                    "local_api_bridge",
                    ServiceStatus::Degraded,
                    Some(format!("failed to bind {bind_addr}: {e}")),
                );
            }
        }
    });
}

fn emit_agent_stage(app: &AppHandle, step: &str, message: &str, detail: Option<serde_json::Value>) {
    let detail_value = detail.unwrap_or(serde_json::Value::Null);
    let payload = serde_json::json!({
        "step": step,
        "message": message,
        "detail": detail_value.clone(),
        "ts": Utc::now().to_rfc3339(),
    });
    let _ = app.emit("agent:stage", payload);

    if kria_core::infra::pipeline_trace::pipeline_debug_enabled() {
        tracing::debug!(
            target: "kria_pipeline",
            step = step,
            message = message,
            detail = ?detail_value,
            "agent stage emitted"
        );
    }
}

async fn emit_colab_status_event(app: &AppHandle, state: &AppState) {
    let payload = collect_colab_tier_status(state).await;
    let _ = app.emit("colab:status", payload);
}

fn parse_relative_age_hours(age: &str) -> Option<f64> {
    let token = age.trim().to_ascii_lowercase();
    let token = token.split_whitespace().next().unwrap_or("");
    if token.is_empty() {
        return None;
    }
    if let Some(v) = token.strip_suffix('m') {
        return v.parse::<f64>().ok().map(|m| m / 60.0);
    }
    if let Some(v) = token.strip_suffix('h') {
        return v.parse::<f64>().ok();
    }
    if let Some(v) = token.strip_suffix('d') {
        return v.parse::<f64>().ok().map(|d| d * 24.0);
    }
    None
}

fn clamp01(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

fn count_items_in_value(value: &serde_json::Value) -> u64 {
    if let Some(arr) = value.as_array() {
        return arr.len() as u64;
    }

    if let Some(v) = value.get("count").and_then(|v| v.as_u64()) {
        return v;
    }

    for key in ["results", "items", "messages", "events", "files", "rows"] {
        if let Some(arr) = value.get(key).and_then(|v| v.as_array()) {
            return arr.len() as u64;
        }
    }

    0
}

fn infer_google_kind(name: &str, result: &serde_json::Value) -> String {
    if let Some(kind) = result.get("kind").and_then(|v| v.as_str()) {
        return kind.to_string();
    }

    if name.contains("gmail") {
        "gmail".into()
    } else if name.contains("calendar") {
        "calendar".into()
    } else if name.contains("drive") {
        "drive".into()
    } else if name.contains("docs") {
        "docs".into()
    } else if name.contains("sheets") {
        "sheets".into()
    } else if name.contains("slides") {
        "slides".into()
    } else if name.contains("forms") {
        "forms".into()
    } else {
        "google_workspace".into()
    }
}

fn compute_tool_result_metadata(name: &str, result: &serde_json::Value) -> serde_json::Value {
    match name {
        "search_news" => {
            let rows = result.get("results").and_then(|v| v.as_array());
            let source_count = result
                .get("count")
                .and_then(|v| v.as_u64())
                .unwrap_or_else(|| rows.map(|r| r.len() as u64).unwrap_or(0));

            let mut freshness_total = 0.0;
            let mut freshness_n = 0usize;
            let mut trust_total = 0.0;
            let mut trust_n = 0usize;
            let mut corroboration_total = 0.0;
            let mut corroboration_n = 0usize;
            let mut freshness_age_hours: Option<f64> = None;
            let mut region_match = false;

            if let Some(items) = rows {
                for row in items {
                    if let Some(v) = row.get("freshness_score").and_then(|v| v.as_f64()) {
                        freshness_total += clamp01(v);
                        freshness_n += 1;
                    }

                    if let Some(tier) = row.get("source_tier").and_then(|v| v.as_i64()) {
                        let trust = match tier {
                            i if i <= 1 => 1.0,
                            2 => 0.78,
                            _ => 0.5,
                        };
                        trust_total += trust;
                        trust_n += 1;
                    }

                    if let Some(v) = row.get("confirmed_by").and_then(|v| v.as_f64()) {
                        corroboration_total += clamp01(v / 4.0);
                        corroboration_n += 1;
                    }

                    if row
                        .get("region_match")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                    {
                        region_match = true;
                    }

                    if let Some(age_str) = row.get("age").and_then(|v| v.as_str()) {
                        if let Some(age_hours) = parse_relative_age_hours(age_str) {
                            freshness_age_hours = Some(match freshness_age_hours {
                                Some(curr) => curr.min(age_hours),
                                None => age_hours,
                            });
                        }
                    }
                }
            }

            let avg_freshness = if freshness_n > 0 {
                freshness_total / freshness_n as f64
            } else {
                0.25
            };
            let avg_trust = if trust_n > 0 {
                trust_total / trust_n as f64
            } else {
                0.4
            };
            let avg_corroboration = if corroboration_n > 0 {
                corroboration_total / corroboration_n as f64
            } else {
                0.25
            };
            let coverage = clamp01(source_count as f64 / 8.0);
            let confidence = clamp01(
                (avg_freshness * 0.35)
                    + (avg_trust * 0.30)
                    + (avg_corroboration * 0.20)
                    + (coverage * 0.15),
            );

            serde_json::json!({
                "confidence": confidence,
                "source_count": source_count,
                "freshness_age_hours": freshness_age_hours,
                "region_match": region_match,
            })
        }
        "searxng_search" | "web_search" => {
            let source_count = result
                .get("count")
                .and_then(|v| v.as_u64())
                .unwrap_or_else(|| {
                    result
                        .get("results")
                        .and_then(|v| v.as_array())
                        .map(|rows| rows.len() as u64)
                        .unwrap_or(0)
                });

            let confidence = if source_count == 0 {
                0.15
            } else {
                clamp01(0.35 + ((source_count as f64) * 0.08))
            };

            serde_json::json!({
                "confidence": confidence,
                "source_count": source_count,
                "freshness_age_hours": serde_json::Value::Null,
                "region_match": serde_json::Value::Null,
            })
        }
        "fetch_article" => {
            let chars = result
                .get("char_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let confidence = if chars >= 2500 {
                0.82
            } else if chars >= 900 {
                0.70
            } else if chars > 0 {
                0.52
            } else {
                0.20
            };

            serde_json::json!({
                "confidence": confidence,
                "source_count": if chars > 0 { 1 } else { 0 },
                "freshness_age_hours": serde_json::Value::Null,
                "region_match": serde_json::Value::Null,
            })
        }
        _ if name.starts_with("gw_")
            || result
                .get("provider")
                .and_then(|v| v.as_str())
                .map(|p| p.eq_ignore_ascii_case("google_workspace"))
                .unwrap_or(false) =>
        {
            let payload = result.get("data").unwrap_or(result);
            let source_count = count_items_in_value(payload);
            let kind = infer_google_kind(name, result);
            let schema_version = result
                .get(gw_contract::GW_META_KEY)
                .and_then(|meta| meta.get(gw_contract::GW_META_SCHEMA_VERSION_KEY))
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let correlation_id = result
                .get(gw_contract::GW_META_KEY)
                .and_then(|meta| meta.get(gw_contract::GW_META_CORRELATION_ID_KEY))
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let account = result
                .get(gw_contract::GW_META_KEY)
                .and_then(|meta| meta.get(gw_contract::GW_META_ACCOUNT_KEY))
                .cloned()
                .unwrap_or(serde_json::Value::Null);

            let mut confidence = if source_count > 0 { 0.80 } else { 0.58 };
            if ["create", "edit", "send", "delete"]
                .iter()
                .any(|k| name.contains(k))
            {
                confidence = 0.74;
            }

            serde_json::json!({
                "confidence": clamp01(confidence),
                "source_count": source_count,
                "freshness_age_hours": serde_json::Value::Null,
                "region_match": serde_json::Value::Null,
                "kind": kind,
                "schema_version": schema_version,
                "correlation_id": correlation_id,
                "account": account,
            })
        }
        _ => serde_json::Value::Null,
    }
}

fn build_tool_result_event_payload(
    name: &str,
    result: &serde_json::Value,
    success: bool,
) -> serde_json::Value {
    let metadata = compute_tool_result_metadata(name, result);
    serde_json::json!({
        "name": name,
        "result": result,
        "success": success,
        "metadata": metadata,
    })
}

fn summarize_tool_turn_for_history(
    name: &str,
    success: bool,
    result: &serde_json::Value,
    metadata: &serde_json::Value,
) -> String {
    if !success {
        let err = result
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        let clipped: String = err.chars().take(180).collect();
        return format!("Tool '{name}' failed: {clipped}");
    }

    let payload = result.get("data").unwrap_or(result);
    let source_count = metadata.get("source_count").and_then(|v| v.as_u64());

    if name == "gw_gmail_inbox" || name == "gw_gmail_search" {
        let returned = payload
            .get("returned_count")
            .and_then(|v| v.as_u64())
            .or_else(|| {
                payload
                    .get("messages")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.len() as u64)
            })
            .or(source_count)
            .unwrap_or(0);
        return format!("Tool '{name}' returned {returned} Gmail message(s).");
    }

    if let Some(count) = source_count {
        return format!("Tool '{name}' completed with {count} item(s).");
    }

    // No metadata source_count — try common shapes so the LLM still has data
    // to ground its reply on (otherwise it falls back to hallucinated bash).
    if let Some(arr) = payload.as_array() {
        return format!("Tool '{name}' returned {} item(s).", arr.len());
    }
    if let Some(obj) = payload.as_object() {
        // Look for the first array-valued field — list_installed_packages,
        // list_languages, etc. follow this shape.
        for (k, v) in obj.iter() {
            if let Some(arr) = v.as_array() {
                if !arr.is_empty() {
                    return format!(
                        "Tool '{name}' returned {} {} entry/entries.",
                        arr.len(),
                        k
                    );
                }
            }
        }
        // Fall back to a compact JSON preview (clipped) so the LLM sees real
        // values rather than just "completed successfully."
        let preview = serde_json::to_string(payload).unwrap_or_default();
        let clipped: String = preview.chars().take(400).collect();
        if !clipped.is_empty() {
            return format!("Tool '{name}' completed. Result: {clipped}");
        }
    }
    if let Some(s) = payload.as_str() {
        let clipped: String = s.chars().take(400).collect();
        return format!("Tool '{name}' completed: {clipped}");
    }

    format!("Tool '{name}' completed successfully.")
}

fn extract_image_preanalysis_summary(tool_data: &serde_json::Value) -> Option<String> {
    let analysis = tool_data.get("analysis").unwrap_or(tool_data);
    let mut lines: Vec<String> = Vec::new();

    if let Some(summary) = analysis.get("summary").and_then(|v| v.as_str()) {
        let trimmed = summary.trim();
        if !trimmed.is_empty() {
            lines.push(format!("Summary: {}", trimmed));
        }
    }

    let metadata = analysis
        .get("metadata")
        .or_else(|| tool_data.get("metadata"));
    if let Some(meta) = metadata {
        let width = meta.get("width").and_then(|v| v.as_u64());
        let height = meta.get("height").and_then(|v| v.as_u64());
        let format_name = meta
            .get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        if let (Some(w), Some(h)) = (width, height) {
            lines.push(format!("Resolution: {}x{} ({})", w, h, format_name));
        }
    }

    let features = analysis
        .get("features")
        .or_else(|| tool_data.get("features"));
    if let Some(scene) = features
        .and_then(|f| f.get("scene_type"))
        .and_then(|v| v.as_str())
    {
        lines.push(format!("Scene type: {}", scene));
    }

    if let Some(mode) = analysis.get("mode_selected").and_then(|v| v.as_str()) {
        lines.push(format!("Preprocessing mode: {}", mode));
    }

    if let Some(count) = analysis
        .get("selected_images")
        .and_then(|v| v.as_array())
        .map(|arr| arr.len())
    {
        lines.push(format!("Preprocessed images: {}", count));
    }

    let ocr_text = analysis
        .get("ocr_text")
        .or_else(|| tool_data.get("ocr_text"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !ocr_text.trim().is_empty() {
        let compact = ocr_text
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if !compact.is_empty() {
            let excerpt: String = compact.chars().take(420).collect();
            let clipped = if compact.chars().count() > 420 {
                format!("{}...", excerpt)
            } else {
                excerpt
            };
            lines.push(format!("OCR excerpt: {}", clipped));
        }
    } else if let Some(engine) = analysis
        .get("ocr")
        .and_then(|v| v.get("engine"))
        .and_then(|v| v.as_str())
    {
        let status = if engine == "none" {
            "unavailable"
        } else {
            "no text extracted"
        };
        lines.push(format!("OCR status: {}", status));
    }

    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

fn extract_preprocessed_image_attachments(
    tool_data: &serde_json::Value,
    default_mime_type: &str,
) -> Option<Vec<ImageAttachment>> {
    let analysis = tool_data.get("analysis").unwrap_or(tool_data);

    let thumbnail_attachment = analysis
        .get("thumbnail_base64")
        .or_else(|| tool_data.get("thumbnail_base64"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|thumb_b64| ImageAttachment {
            data: thumb_b64.to_string(),
            mime_type: analysis
                .get("thumbnail_mime_type")
                .or_else(|| tool_data.get("thumbnail_mime_type"))
                .and_then(|v| v.as_str())
                .filter(|m| !m.trim().is_empty())
                .unwrap_or(default_mime_type)
                .to_string(),
        });

    if let Some(items) = analysis.get("selected_images").and_then(|v| v.as_array()) {
        let mut attachments = Vec::new();
        let mut has_global_frame = false;
        for item in items {
            let data = item
                .get("data_base64")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            if data.is_empty() {
                continue;
            }

            let mime_type = item
                .get("mime_type")
                .and_then(|v| v.as_str())
                .filter(|m| !m.trim().is_empty())
                .unwrap_or(default_mime_type)
                .to_string();

            if item
                .get("kind")
                .and_then(|v| v.as_str())
                .map(|kind| kind.eq_ignore_ascii_case("global"))
                .unwrap_or(false)
            {
                has_global_frame = true;
            }

            attachments.push(ImageAttachment {
                data: data.to_string(),
                mime_type,
            });
        }

        if !has_global_frame {
            if let Some(thumb) = thumbnail_attachment.clone() {
                attachments.push(thumb);
            }
        }

        if !attachments.is_empty() {
            return Some(attachments);
        }
    }

    if let Some(thumb) = thumbnail_attachment {
        return Some(vec![thumb]);
    }

    // Sidecar may be unavailable and analyze_image can degrade to native metadata only.
    // In that case, create a native preprocessed thumbnail so the LLM still gets an image.
    let path_fallback = analysis
        .get("path")
        .or_else(|| tool_data.get("path"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if let Some(native) = build_native_preprocessed_attachment(path_fallback) {
        return Some(vec![native]);
    }

    None
}

fn image_visual_token_cap_for_context(context_window: usize) -> u64 {
    if context_window <= 2048 {
        320
    } else if context_window <= 3072 {
        448
    } else {
        640
    }
}

fn image_base64_cap_for_context(context_window: usize) -> usize {
    if context_window <= 2048 {
        IMAGE_SAFE_MAX_B64_CHARS_2K_CTX
    } else {
        IMAGE_SAFE_MAX_B64_CHARS_4K_CTX
    }
}

fn constrain_runtime_image_attachments(
    attachments: Vec<ImageAttachment>,
    context_window: usize,
) -> Vec<ImageAttachment> {
    let max_b64_chars = image_base64_cap_for_context(context_window);
    let mut safe: Vec<ImageAttachment> = Vec::new();

    for attachment in attachments {
        if attachment.data.trim().is_empty() {
            continue;
        }
        if attachment.data.len() > max_b64_chars {
            continue;
        }
        safe.push(attachment);
        if safe.len() >= IMAGE_SAFE_MAX_ATTACHMENTS_PER_TURN {
            break;
        }
    }

    safe
}

async fn refresh_ocr_dependency_health(health: &HealthRegistry, sidecar: &SidecarBridge) {
    if !sidecar.is_alive() {
        health.update(
            "ocr_dependency",
            ServiceStatus::Starting,
            Some("Waiting for sidecar startup before OCR dependency probe".into()),
        );
        return;
    }

    {
        let mut probe_state = ocr_probe_state().lock().await;
        let now = std::time::Instant::now();
        if probe_state.in_flight {
            tracing::debug!("OCR dependency probe skipped: already in-flight");
            return;
        }
        if now < probe_state.next_allowed_at {
            tracing::debug!("OCR dependency probe skipped: backoff/interval active");
            return;
        }
        probe_state.in_flight = true;
    }

    let probe_path = std::env::temp_dir().join("kria_ocr_probe.ppm");
    if let Err(e) = std::fs::write(&probe_path, OCR_HEALTH_PROBE_IMAGE_BYTES) {
        health.update(
            "ocr_dependency",
            ServiceStatus::Degraded,
            Some(format!("Failed to write OCR probe image: {e}")),
        );
        finalize_ocr_probe_schedule(false).await;
        return;
    }

    let payload = serde_json::json!({
        "file": probe_path.to_string_lossy().to_string(),
        "operations": ["ocr", "thumbnail"],
        "intent": "text_reading",
    });

    let response = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        sidecar.request("image.analyze", payload),
    )
    .await;

    let _ = std::fs::remove_file(&probe_path);

    let mut probe_success = false;

    match response {
        Ok(Ok(result)) => {
            probe_success = true;
            let engine = result
                .get("ocr")
                .and_then(|v| v.get("engine"))
                .and_then(|v| v.as_str())
                .unwrap_or("none");

            if engine.eq_ignore_ascii_case("none") {
                health.update(
                    "ocr_dependency",
                    ServiceStatus::Degraded,
                    Some(
                        "OCR unavailable in sidecar runtime (engine: none). Image analysis still works via visual path."
                            .into(),
                    ),
                );
            } else {
                health.update(
                    "ocr_dependency",
                    ServiceStatus::Healthy,
                    Some(format!("OCR engine ready in sidecar: {engine}")),
                );
            }
        }
        Ok(Err(e)) => {
            health.update(
                "ocr_dependency",
                ServiceStatus::Degraded,
                Some(format!("OCR probe failed via sidecar: {e}")),
            );
        }
        Err(_) => {
            health.update(
                "ocr_dependency",
                ServiceStatus::Degraded,
                Some("OCR probe timed out while contacting sidecar".into()),
            );
        }
    }

    finalize_ocr_probe_schedule(probe_success).await;
}

fn build_preprocessing_step_status(
    tool_data: &serde_json::Value,
    image_intent: &str,
) -> serde_json::Value {
    let analysis = tool_data.get("analysis").unwrap_or(tool_data);

    let normalization_steps = analysis
        .get("normalization_plan")
        .and_then(|v| v.get("branches"))
        .and_then(|v| v.as_array())
        .map(|arr| arr.len())
        .unwrap_or(0);

    let resized_images = analysis
        .get("resize_plan")
        .and_then(|v| v.get("images"))
        .and_then(|v| v.as_array())
        .map(|arr| arr.len())
        .unwrap_or(0);

    let selected_images = analysis
        .get("selected_images")
        .and_then(|v| v.as_array())
        .map(|arr| arr.len())
        .unwrap_or(0);

    let has_thumbnail = analysis
        .get("thumbnail_base64")
        .or_else(|| tool_data.get("thumbnail_base64"))
        .and_then(|v| v.as_str())
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);

    let has_ocr_text = analysis
        .get("ocr_text")
        .and_then(|v| v.as_str())
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);

    let ocr_engine = analysis
        .get("ocr")
        .and_then(|v| v.get("engine"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let within_context = analysis
        .get("token_accounting")
        .and_then(|v| v.get("within_context"))
        .and_then(|v| v.as_bool());

    serde_json::json!({
        "source": tool_data.get("source").and_then(|v| v.as_str()).unwrap_or("unknown"),
        "image_intent": image_intent,
        "mode_selected": analysis.get("mode_selected").and_then(|v| v.as_str()),
        "normalization_steps": normalization_steps,
        "resized_images": resized_images,
        "selected_images": selected_images,
        "fallback_level_applied": analysis.get("fallback_level_applied").and_then(|v| v.as_i64()).unwrap_or(0),
        "token_accounting_present": analysis.get("token_accounting").is_some(),
        "within_context": within_context,
        "has_thumbnail": has_thumbnail,
        "has_ocr_text": has_ocr_text,
        "ocr_engine": ocr_engine,
    })
}

fn infer_image_intent_from_text(user_text: &str) -> &'static str {
    let text = user_text.trim().to_ascii_lowercase();
    if text.is_empty() {
        return "mixed";
    }

    let has_ui = [
        "ui",
        "screenshot",
        "screen",
        "stack trace",
        "terminal",
        "error",
    ]
    .iter()
    .any(|k| text.contains(k));
    if has_ui {
        return "ui_error_reading";
    }

    let has_document = [
        "document", "invoice", "receipt", "form", "page", "scan", "pdf",
    ]
    .iter()
    .any(|k| text.contains(k));
    if has_document {
        return "document_scan";
    }

    let has_text = [
        "read",
        "text",
        "ocr",
        "extract",
        "transcribe",
        "word",
        "sentence",
    ]
    .iter()
    .any(|k| text.contains(k));
    let has_scene = [
        "describe",
        "scene",
        "object",
        "identify",
        "detect",
        "count",
        "analy",
        "what is in",
        "see",
        "look",
    ]
    .iter()
    .any(|k| text.contains(k));

    match (has_text, has_scene) {
        (true, true) => "mixed",
        (true, false) => "text_reading",
        (false, true) => "scene_understanding",
        (false, false) => "mixed",
    }
}

fn build_image_llm_user_content(
    user_text: &str,
    attachment_path: &str,
    image_intent: &str,
    preanalysis_summary: Option<&str>,
) -> String {
    let mut content = String::new();
    content.push_str(user_text);
    content.push_str("\n\nImage attachment is already included for this turn.");
    content.push_str("\nInterpret the user's request and answer directly from the uploaded image.");
    content.push_str(
        "\nDo not ask the user to re-upload the image, provide a URL, or provide an image path.",
    );
    content.push_str(
        "\nIf detailed OCR/object analysis is needed, use available vision tools automatically.",
    );
    content.push_str("\nOnly ask follow-up questions when the request is genuinely ambiguous.");
    content.push_str("\nPrefer automatic pre-analysis context first, then use the attached image.");
    content.push_str("\nInferred image-intent hint: ");
    content.push_str(image_intent);
    content.push_str("\nAttachment path (available to local tools if needed): ");
    content.push_str(attachment_path);

    if let Some(summary) = preanalysis_summary {
        if !summary.trim().is_empty() {
            content.push_str("\n\nAutomatic pre-analysis context:\n");
            content.push_str(summary);
        }
    }

    content
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn load_cached_hardware_info(cache_path: &std::path::Path) -> Option<HardwareInfo> {
    let text = std::fs::read_to_string(cache_path).ok()?;
    serde_json::from_str::<HardwareInfo>(&text).ok()
}

fn resolve_hardware_info(
    config: &KriaConfig,
    cache_path: &std::path::Path,
) -> (HardwareInfo, String) {
    // Highest precedence: explicit env override.
    if let Ok(env_tier) = std::env::var("KRIA_TIER") {
        let env_tier = env_tier.trim();
        if !env_tier.is_empty() {
            let mut hw = detect_hardware();
            hw.tier = env_tier
                .parse::<HardwareTier>()
                .unwrap_or(HardwareTier::Standard);
            return (hw, format!("env:KRIA_TIER={env_tier}"));
        }
    }

    // Next precedence: config override.
    if !config.hardware.tier.trim().is_empty() {
        let mut hw = detect_hardware();
        hw.tier = config
            .hardware
            .tier
            .parse::<HardwareTier>()
            .unwrap_or(HardwareTier::Standard);
        return (hw, format!("config.hardware.tier={}", config.hardware.tier));
    }

    let force_redetect = env_truthy("KRIA_REDETECT") || env_truthy("KRIA_REDETECT_HARDWARE");

    // Next precedence: cached detection result.
    if !force_redetect {
        if let Some(cached) = load_cached_hardware_info(cache_path) {
            return (cached, "cache:hardware_tier.json".to_string());
        }
    }

    // Fallback: fresh detection.
    (detect_hardware(), "detect_hardware()".to_string())
}

/// OnceCell populated by init_runtime() once the full runtime is ready.
/// Managing this (not AppState) in Tauri allows commands to be registered
/// before init completes without a "state not managed" panic.
pub type AppStateCell = tokio::sync::OnceCell<AppState>;

/// Shared application state managed by Tauri.
pub struct AppState {
    pub config: Arc<RwLock<KriaConfig>>,
    /// Held to keep the Arc alive for the app's lifetime.
    #[allow(dead_code)]
    pub model_router: Arc<ModelRouter>,
    pub agent_loop: Arc<AgentLoop>,
    pub tool_registry: Arc<ToolRegistry>,
    pub memory_store: Arc<MemoryStore>,
    pub hitl: Arc<HitlGateway>,
    pub event_bus: Arc<EventBus>,
    /// Held to keep the sidecar process alive for the app's lifetime.
    #[allow(dead_code)]
    pub sidecar: Arc<SidecarBridge>,
    pub embeddings: Arc<EmbeddingModel>,
    pub vectors: Arc<VectorIndex>,
    pub current_session_id: Arc<RwLock<String>>,
    pub voice_active: Arc<std::sync::atomic::AtomicBool>,
    pub voice_pipeline: Arc<RwLock<Arc<VoicePipeline>>>,
    /// Engine-aware voice handle. Holds either the v1 [`VoicePipeline`]
    /// (default) or the v2 [`kria_core::voice::v2::VoicePipelineV2`] when
    /// `voice.engine = "v2"`. Existing call-sites keep using
    /// `voice_pipeline` directly; v2-aware code reads `active_voice`.
    pub active_voice: Arc<RwLock<kria_core::voice::v2::ActivePipeline>>,
    /// Telemetry receiver for the v2 pipeline (when active). `None` while
    /// running v1. Wrapped in a Mutex so the background driver task can
    /// take it without dropping the AppState lock.
    pub voice_v2_telemetry:
        Arc<tokio::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<
            kria_core::voice::v2::VoiceTelemetry,
        >>>>,
    pub health: Arc<HealthRegistry>,
    pub scheduler: Arc<RwLock<AutomationScheduler>>,
    pub macro_recorder: Arc<RwLock<MacroRecorder>>,
    pub workflow_engine: Arc<RwLock<WorkflowEngine>>,
    pub started_at: std::time::Instant,
    pub hardware_info: Arc<HardwareInfo>,
    pub proactive: Arc<kria_core::automation::ProactiveEngine>,
    pub telegram_bridge: Arc<RwLock<Option<TelegramBridge>>>,
    /// MCP server manager — kept alive for background health monitoring + dynamic tool registration.
    #[allow(dead_code)]
    pub mcp_manager: Arc<tokio::sync::Mutex<McpServerManager>>,
    /// Lazy Google Workspace MCP client reference used by gw_* tool handlers.
    pub gw_client_ref: gw::GwClientRef,
    /// Colab cloud-tier runtime status surface.
    pub colab_runtime: Arc<RwLock<ColabRuntimeSnapshot>>,
    /// Hardware orchestrator — manages llama-server lifecycle and dynamic GPU offloading.
    /// Wrapped in RwLock so the background startup task can populate it after AppState
    /// is set, keeping the main init path non-blocking.
    #[allow(dead_code)]
    pub orchestrator: Arc<tokio::sync::RwLock<Option<Arc<Orchestrator>>>>,
    /// Number of active turn executions that currently depend on local runtime.
    pub orchestrator_active_turns: Arc<std::sync::atomic::AtomicUsize>,
    /// Last observed local-runtime activity timestamp for idle release decisions.
    pub orchestrator_last_activity_at: Arc<tokio::sync::Mutex<std::time::Instant>>,
    /// Image generation orchestrator — ComfyUI sidecar + cloud fallback.
    #[allow(dead_code)]
    pub image_orchestrator: Arc<ImageOrchestrator>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColabRuntimeState {
    Disconnected,
    SidecarStarting,
    AwaitingBrowserConnection,
    NotebookSelectionRequired,
    Ready,
    Degraded,
}

impl ColabRuntimeState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disconnected => "disconnected",
            Self::SidecarStarting => "sidecar_starting",
            Self::AwaitingBrowserConnection => "awaiting_browser_connection",
            Self::NotebookSelectionRequired => "notebook_selection_required",
            Self::Ready => "ready",
            Self::Degraded => "degraded",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ColabRuntimeSnapshot {
    pub state: ColabRuntimeState,
    pub sidecar_server_name: String,
    pub selected_notebook: Option<String>,
    pub last_error: Option<String>,
}

impl ColabRuntimeSnapshot {
    fn new(state: ColabRuntimeState, sidecar_server_name: String) -> Self {
        Self {
            state,
            sidecar_server_name,
            selected_notebook: None,
            last_error: None,
        }
    }
}

fn build_voice_pipeline(
    config: &KriaConfig,
    paths: &kria_core::platform::paths::KriaPaths,
) -> Arc<VoicePipeline> {
    // Log v2 engine selection — the v2 stack is scaffolded under
    // `kria_core::voice::v2` (sentence splitter, post-edit, playback sink,
    // AEC + wake skeletons, FSM). Running v2 end-to-end is gated behind
    // additional cargo features (`voice-whisper-rs`, `voice-piper-rs`, …)
    // and is not yet the default runtime path. Until then we always build
    // the v1 pipeline; v2 is exercised through unit tests + the
    // `voice_v2_status` command.
    let engine = config.voice.engine.to_ascii_lowercase();
    if engine == "v2" {
        tracing::warn!(
            "voice.engine = \"v2\" requested; v2 stack is scaffold-only in this build, \
             falling back to v1. Enable the relevant cargo features and complete the \
             VoicePipelineV2 runtime loop to switch over."
        );
    } else if engine != "v1" && !engine.is_empty() {
        tracing::warn!(engine = %engine, "unknown voice.engine value; using v1");
    }

    let stt_model_path = paths.models_dir.join("stt").join(&config.voice.stt_model);
    let tts_voice_file = format!("{}.onnx", config.voice.tts_voice);
    let tts_model_path = paths.models_dir.join("piper").join(&tts_voice_file);

    // Resolve + log wake-word model wiring so v2 readiness is visible even
    // while the runtime path is still v1. Construction is cheap (no model
    // load when disabled) and any failure falls back silently.
    if config.voice.wake_word.enabled {
        let wake_dir = paths.models_dir.join("wake");
        let wake_path = if config.voice.wake_word.model_path.is_empty() {
            wake_dir.join("hey_ria.onnx")
        } else {
            let p = std::path::PathBuf::from(&config.voice.wake_word.model_path);
            if p.is_absolute() { p } else { wake_dir.join(p.file_name().unwrap_or_default()) }
        };
        let detector = kria_core::voice::v2::WakeWordDetector::try_load(
            wake_path.clone(),
            config.voice.wake_word.sensitivity,
            "hey ria",
            config.voice.wake_word.aliases.clone(),
        );
        tracing::info!(
            keyword_path = %wake_path.display(),
            sensitivity = config.voice.wake_word.sensitivity,
            active = detector.is_active(),
            "wake-word detector resolved"
        );
    }

    let whisper_bin = which_binary("whisper-cpp").or_else(|| which_binary("main"));
    let piper_bin = which_binary("piper");

    // Surface a clear warning if the configured STT model file is missing,
    // so the user knows to run `python scripts/download_models.py`.
    if !stt_model_path.exists() {
        tracing::warn!(
            model = %stt_model_path.display(),
            "configured STT model file not found — run `python scripts/download_models.py --tier lite` to fetch it"
        );
    }

    let mut stt = SpeechToText::new(stt_model_path.clone(), whisper_bin.clone());
    stt.set_language(&config.voice.language);
    if config.hardware.threads > 0 {
        stt.set_threads(config.hardware.threads.clamp(1, 12));
    }
    stt.set_command_timeout(std::time::Duration::from_secs(45));
    let tts = TextToSpeech::new(tts_model_path, piper_bin);
    let vad_model_path = paths.models_dir.join("vad").join("silero_vad.onnx");

    let pipeline = Arc::new(
        VoicePipeline::new(config.voice.clone(), stt, tts).with_vad_model(vad_model_path),
    );

    // Pre-warm whisper at startup: page-cache the model file + (on CUDA/metal)
    // trigger the one-time GPU layer init *before* the first user utterance.
    // Without this, the first transcription pays the full cold-load cost (the
    // "optimizing GPU layer…" pause) and often exceeds the STT timeout.
    // Best-effort — errors are logged and ignored.
    if stt_model_path.exists() && whisper_bin.is_some() {
        let warm_model = stt_model_path.clone();
        let warm_bin = whisper_bin.clone();
        let warm_lang = config.voice.language.clone();
        let warm_threads = config.hardware.threads;
        tokio::spawn(async move {
            let mut warm_stt = SpeechToText::new(warm_model, warm_bin);
            warm_stt.set_language(&warm_lang);
            if warm_threads > 0 {
                warm_stt.set_threads(warm_threads.clamp(1, 12));
            }
            warm_stt.set_command_timeout(std::time::Duration::from_secs(120));
            // 1 second of silence at 16 kHz — just enough to load the model.
            let silence = vec![0.0f32; 16_000];
            let started = std::time::Instant::now();
            match warm_stt.transcribe_samples(&silence, 16_000).await {
                Ok(_) => tracing::info!(
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "whisper warmup complete"
                ),
                Err(e) => tracing::warn!(error = %e, "whisper warmup failed (non-fatal)"),
            }
        });
    }

    pipeline
}

/// Build the v2 in-process streaming pipeline using the v1 CLI engines as
/// the underlying backends. Adds the streaming sentence playback + hard
/// barge-in concurrency model on top, without requiring native deps
/// (whisper-rs, sonata, webrtc-apm). When the user later enables
/// `voice-whisper-rs` / `voice-piper-rs` features at build time the swap
/// is local to this builder.
fn build_v2_pipeline(
    config: &KriaConfig,
    paths: &kria_core::platform::paths::KriaPaths,
    hw_tier: kria_core::platform::detect::HardwareTier,
) -> anyhow::Result<(
    Arc<kria_core::voice::v2::VoicePipelineV2>,
    tokio::sync::watch::Receiver<kria_core::voice::v2::VoiceSessionState>,
    tokio::sync::mpsc::UnboundedReceiver<kria_core::voice::v2::VoiceTelemetry>,
)> {
    use kria_core::voice::v2;

    let stt_model_path = paths.models_dir.join("stt").join(&config.voice.stt_model);
    let tts_voice_file = format!("{}.onnx", config.voice.tts_voice);
    let tts_model_path = paths.models_dir.join("piper").join(&tts_voice_file);

    let whisper_bin = which_binary("whisper-cpp").or_else(|| which_binary("main"));
    let piper_bin = which_binary("piper");

    let mut stt = SpeechToText::new(stt_model_path, whisper_bin);
    stt.set_language(&config.voice.language);
    if config.hardware.threads > 0 {
        stt.set_threads(config.hardware.threads.clamp(1, 12));
    }
    stt.set_command_timeout(std::time::Duration::from_secs(45));
    let tts = TextToSpeech::new(tts_model_path, piper_bin);

    let wake = if config.voice.wake_word.enabled {
        let wake_dir = paths.models_dir.join("wake");
        let wake_path = if config.voice.wake_word.model_path.is_empty() {
            wake_dir.join("hey_ria.onnx")
        } else {
            let p = std::path::PathBuf::from(&config.voice.wake_word.model_path);
            if p.is_absolute() {
                p
            } else {
                wake_dir.join(p.file_name().unwrap_or_default())
            }
        };
        Some(v2::WakeWordDetector::try_load(
            wake_path,
            config.voice.wake_word.sensitivity,
            "hey ria",
            config.voice.wake_word.aliases.clone(),
        ))
    } else {
        None
    };

    let (pipeline, state_rx, telemetry_rx) =
        v2::build_v2_with_cli_engines(&config.voice, hw_tier, Arc::new(stt), Arc::new(tts), wake);
    Ok((pipeline, state_rx, telemetry_rx))
}

// ─── v2 continuous voice loop ─────────────────────────────────────────────
//
// Called from `start_voice` when the engine is "v2". Starts an `AudioCapture`
// thread, broadcasts chunks into the v2 pipeline's `run_turn` loop, and pumps
// telemetry events to the UI. Runs entirely in a background task; `stop_voice`
// signals it to exit via `voice_active = false` + `force_abort`.

#[allow(clippy::too_many_arguments)]
async fn start_voice_v2_loop(
    v2: Arc<kria_core::voice::v2::VoicePipelineV2>,
    voice_active: Arc<std::sync::atomic::AtomicBool>,
    telemetry_slot: Arc<tokio::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<kria_core::voice::v2::VoiceTelemetry>>>>,
    router: Arc<ModelRouter>,
    session_id_lock: Arc<RwLock<String>>,
    config: Arc<RwLock<KriaConfig>>,
    hw_info: Arc<HardwareInfo>,
    memory_store: Arc<MemoryStore>,
    tool_registry: Arc<kria_core::tools::registry::ToolRegistry>,
    app: AppHandle,
) {
    use kria_core::voice::v2::VoiceSessionState;
    use kria_core::voice::capture::AudioCapture;

    // 1. Wire the AudioPlayer to the pipeline.
    {
        let cfg = config.read().await;
        let player = Arc::new(
            kria_core::voice::AudioPlayer::new()
                .with_output_device(Some(cfg.voice.speaker_device.clone()))
                .follow_system_default(cfg.voice.follow_system_default_speaker),
        );
        v2.set_audio_player(player).await;
    }

    // 2. Start AudioCapture and forward to a broadcast channel, gating chunks
    //    when the pipeline is Speaking so the mic doesn't pick up KRIA's voice.
    let (broadcast_tx, _) = tokio::sync::broadcast::channel::<kria_core::voice::capture::AudioChunk>(128);
    let broadcast_tx_arc = Arc::new(broadcast_tx);
    {
        let capture_cfg = config.read().await;
        let mic_device = capture_cfg.voice.mic_device.clone();
        let follow_mic = capture_cfg.voice.follow_system_default_mic
            || mic_device.trim().is_empty()
            || mic_device.eq_ignore_ascii_case("auto");
        let noise_mode = capture_cfg.voice.noise_suppression_mode.clone();
        drop(capture_cfg);

        let capture = AudioCapture::new(16_000)
            .with_input_device(mic_device)
            .follow_system_default(follow_mic)
            .with_noise_suppression_mode(noise_mode);

        let (mut capture_rx, _capture_handle) = match capture.start() {
            Ok(pair) => pair,
            Err(e) => {
                tracing::error!("v2 audio capture failed to start: {e}");
                let _ = app.emit("voice:error", serde_json::json!({ "error": format!("Mic start failed: {e}") }));
                voice_active.store(false, std::sync::atomic::Ordering::Relaxed);
                return;
            }
        };

        let bt = broadcast_tx_arc.clone();
        let v2_state = v2.subscribe_state();
        // Forward mpsc → broadcast, gating when Speaking/Thinking/BargeIn to
        // prevent recording KRIA's own TTS output (echo cancellation gate).
        tokio::spawn(async move {
            // Keep capture_handle alive for the duration of this task.
            // (_capture_handle is moved here to prevent premature drop.)
            while let Some(chunk) = capture_rx.recv().await {
                let st = *v2_state.borrow();
                if matches!(
                    st,
                    VoiceSessionState::Speaking | VoiceSessionState::Thinking | VoiceSessionState::BargeIn
                ) {
                    // Discard — KRIA is generating/speaking; skip to prevent echo.
                    continue;
                }
                if bt.send(chunk).is_err() {
                    break;
                }
            }
        });
    }

    // 3. Pump telemetry events → Tauri UI events.
    {
        let mut rx_opt = telemetry_slot.lock().await.take();
        if let Some(mut rx) = rx_opt.take() {
            let app_h = app.clone();
            let va = voice_active.clone();
            let slot = telemetry_slot.clone();
            tokio::spawn(async move {
                while let Some(ev) = rx.recv().await {
                    let (tauri_event, payload) = v2_telemetry_to_event(&ev);
                    let _ = app_h.emit(tauri_event, payload);
                    // Also forward raw telemetry for debug/UI extensions.
                    if let Ok(raw) = serde_json::to_value(&ev) {
                        let _ = app_h.emit("voice:v2_telemetry", raw);
                    }
                    if !va.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                }
                *slot.lock().await = None;
            });
        }
    }

    let _ = app.emit("voice:state", serde_json::json!({ "state": "listening" }));

    // 4. Main run_turn loop. Each call to run_turn executes one full
    //    wake → capture → STT → LLM → TTS cycle.
    let v2_loop = v2.clone();
    let voice_active_loop = voice_active.clone();
    let router_loop = router.clone();
    let config_loop = config.clone();
    let session_id_loop = session_id_lock.clone();
    let memory_store_loop = memory_store.clone();
    let tool_registry_loop = tool_registry.clone();
    let hw_info_loop = hw_info.clone();
    let app_loop = app.clone();
    let bt_loop = broadcast_tx_arc.clone();

    tauri::async_runtime::spawn(async move {
        while voice_active_loop.load(std::sync::atomic::Ordering::Relaxed) {
            // Transition to Listening before each turn.
            v2_loop.force_wake("auto");

            let audio_rx = bt_loop.subscribe();
            let router_turn = router_loop.clone();
            let config_turn = config_loop.clone();
            let session_id_turn = session_id_loop.clone();
            let memory_turn = memory_store_loop.clone();
            let tool_reg_turn = tool_registry_loop.clone();
            let hw_turn = hw_info_loop.clone();

            let llm = move |user_text: String| async move {
                let (tx, rx) = tokio::sync::mpsc::channel::<String>(64);
                let backend = match router_turn.route("voice").await {
                    Some(b) => b,
                    None => {
                        let _ = tx.send("(No LLM backend — check model config)".into()).await;
                        return rx;
                    }
                };
                // Build messages with system prompt + recent context (mirrors v1 flow).
                let session_id = session_id_turn.read().await.clone();
                let cfg = config_turn.read().await;
                let hw_tier = hw_turn.tier.as_str();
                let tool_defs = tool_reg_turn.list_for_tier(hw_tier);
                let tool_descriptions = build_tool_descriptions_for_prompt(&tool_defs);
                let user_name = memory_turn
                    .get_preference("user_name")
                    .unwrap_or(None)
                    .unwrap_or_else(|| "User".to_string());
                let memory_context = match memory_turn.search_facts(&user_text, 5) {
                    Ok(facts) if !facts.is_empty() => {
                        let lines: Vec<String> = facts.iter().map(|f| format!("- {}", f.text)).collect();
                        format!("Known facts:\n{}", lines.join("\n"))
                    }
                    _ => String::new(),
                };
                let system_prompt = kria_core::agent::prompts::build_system_prompt(
                    &tool_descriptions, &user_name, std::env::consts::OS, hw_tier,
                    "auto", &memory_context,
                );
                drop(cfg);
                let recent_turns = memory_turn.get_recent_turns(&session_id, 20).unwrap_or_default();
                let mut messages = Vec::with_capacity(recent_turns.len() + 2);
                messages.push(ChatMessage { role: "system".into(), content: system_prompt, name: None, images: None });
                for t in &recent_turns {
                    messages.push(ChatMessage { role: t.role.clone(), content: t.content.clone(), name: None, images: None });
                }
                messages.push(ChatMessage { role: "user".into(), content: user_text, name: None, images: None });
                tokio::spawn(async move {
                    use futures::StreamExt;
                    match backend.chat_stream(&messages, None, 0.7, 512).await {
                        Ok(mut stream) => {
                            while let Some(tok) = stream.next().await {
                                if tx.send(tok).await.is_err() { break; }
                            }
                        }
                        Err(e) => { let _ = tx.send(format!("(LLM error: {e})")).await; }
                    }
                });
                rx
            };

            if let Err(e) = v2_loop.clone().run_turn(audio_rx, llm).await {
                tracing::warn!("v2 run_turn error: {e}");
                let _ = app_loop.emit("voice:error", serde_json::json!({ "error": e.to_string() }));
            }

            // Post-turn silence gap: prevents the next turn's STT from picking
            // up residual echo from the speaker (≥300 ms is enough for room echo).
            if voice_active_loop.load(std::sync::atomic::Ordering::Relaxed) {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            }
        }
        let _ = app_loop.emit("voice:state", serde_json::json!({ "state": "idle" }));
        tracing::info!("v2 voice loop exited");
    });
}

/// Map a `VoiceTelemetry` variant to the canonical Tauri event name + JSON
/// payload that the existing UI listeners already handle.
fn v2_telemetry_to_event(ev: &kria_core::voice::v2::VoiceTelemetry) -> (&'static str, serde_json::Value) {
    use kria_core::voice::v2::{VoiceTelemetry, VoiceSessionState};
    match ev {
        VoiceTelemetry::State { state } => {
            let s = match state {
                VoiceSessionState::Sleeping => "idle",
                VoiceSessionState::Listening => "listening",
                VoiceSessionState::Transcribing | VoiceSessionState::Thinking => "processing",
                VoiceSessionState::Speaking => "speaking",
                VoiceSessionState::BargeIn => "listening",
            };
            ("voice:state", serde_json::json!({ "state": s }))
        }
        VoiceTelemetry::Partial { text, engine } => (
            "voice:partial_transcript",
            serde_json::json!({ "text": text, "confidence": 0.7, "language": "auto", "stability": 0.5, "engine": engine }),
        ),
        VoiceTelemetry::Final { text, confidence, engine } => (
            "voice:transcript",
            serde_json::json!({ "text": text, "confidence": confidence, "language": "auto", "stability": 1.0, "engine": engine }),
        ),
        VoiceTelemetry::Error { message } => (
            "voice:error",
            serde_json::json!({ "error": message }),
        ),
        _ => ("voice:v2_telemetry", serde_json::to_value(ev).unwrap_or_default()),
    }
}

/// Initialize the KRIA runtime (called from setup).
pub async fn init_runtime(handle: &AppHandle) -> anyhow::Result<()> {
    // Initialize logging first so startup diagnostics are filterable.
    let bootstrap_paths = kria_core::platform::paths::KriaPaths::resolve();
    kria_core::infra::logging::setup_logging(&bootstrap_paths.logs_dir);

    let mut config = KriaConfig::load(None)?;
    let paths = config.resolve_paths()?;

    // Resolve hardware tier with precedence: env > config > cache > detect.
    let hw_cache_path = paths.data_dir.join("hardware_tier.json");
    let (hw_info, hw_source) = resolve_hardware_info(&config, &hw_cache_path);

    // Cache latest hardware info to JSON.
    if let Ok(json) = serde_json::to_string_pretty(&hw_info) {
        let _ = std::fs::write(&hw_cache_path, json);
    }
    let hardware_info = Arc::new(hw_info);

    // Apply effective tier-aware runtime limits unless explicitly overridden.
    let tier_context_limit = hardware_info.tier.context_window();
    let requested_context_limit = if config.hardware.max_context_tokens > 0 {
        config.hardware.max_context_tokens
    } else {
        config.llm.context_window
    };
    if requested_context_limit == 0 {
        config.llm.context_window = tier_context_limit;
    } else if requested_context_limit > tier_context_limit {
        tracing::warn!(
            requested = requested_context_limit,
            tier_limit = tier_context_limit,
            tier = %hardware_info.tier.as_str(),
            "requested context window exceeded tier capacity; clamping"
        );
        config.llm.context_window = tier_context_limit;
    } else {
        config.llm.context_window = requested_context_limit;
    }

    if config.hardware.threads == 0 {
        config.hardware.threads = hardware_info.tier.thread_count();
    }
    if config.hardware.gpu_layers < 0 {
        config.hardware.gpu_layers = hardware_info.tier.gpu_layers();
    }
    if config.voice.stt_model.eq_ignore_ascii_case("auto") {
        config.voice.stt_model = hardware_info.tier.stt_model().to_string();
    }

    tracing::info!(
        source = %hw_source,
        tier = ?hardware_info.tier,
        ram_mb = hardware_info.total_ram_mb,
        vram_mb = ?hardware_info.vram_mb,
        gpu = ?hardware_info.gpu_name,
        cores = hardware_info.cpu_cores,
        "hardware detected"
    );

    // Initialize memory store (SQLite)
    let memory_store = Arc::new(MemoryStore::open(&paths.db_path)?);

    // Initialize model router from config
    let model_router = Arc::new(ModelRouter::from_config(&config));

    // EventBus (tokio broadcast channels)
    let event_bus = Arc::new(EventBus::new(256));

    // Health registry (created early so sidecar spawn can update it)
    let health = Arc::new(HealthRegistry::new());
    health.register("sidecar");
    health.update("sidecar", ServiceStatus::Starting, None);
    health.register("ocr_dependency");
    health.update(
        "ocr_dependency",
        ServiceStatus::Starting,
        Some("Probing OCR dependency readiness".into()),
    );

    // Python sidecar bridge (created early so tools can reference it)
    let venv_path = paths.data_dir.join("python-env");
    let venv_str = venv_path.to_string_lossy().to_string();
    let sidecar = Arc::new(SidecarBridge::new("python3", Some(&venv_str)));
    // Spawn sidecar in background — non-blocking; tools degrade gracefully if unavailable
    let sidecar_clone = sidecar.clone();
    let event_bus_clone = event_bus.clone();
    let health_sidecar = health.clone();
    tokio::spawn(async move {
        match sidecar_clone.spawn().await {
            Ok(()) => {
                tracing::info!("Python sidecar started successfully");
                event_bus_clone.publish(kria_core::infra::event_bus::KriaEvent::SidecarReady);
                health_sidecar.update("sidecar", ServiceStatus::Healthy, None);
                refresh_ocr_dependency_health(&health_sidecar, &sidecar_clone).await;
            }
            Err(e) => {
                tracing::warn!("Python sidecar failed to start (non-fatal): {}", e);
                health_sidecar.update("sidecar", ServiceStatus::Degraded, Some(format!("{e}")));
                health_sidecar.update(
                    "ocr_dependency",
                    ServiceStatus::Degraded,
                    Some("OCR unavailable: sidecar failed to start".into()),
                );
            }
        }
    });

    // ── Hardware Orchestrator (optional, manages llama-server lifecycle) ───────
    // Helper: resolve a model filename against multiple candidate directories.
    // Checks ~/.kria/models/llm/ first, then the workspace models/llm/ (for dev).
    let resolve_model_file = |filename: &str| -> String {
        // 1. ~/.kria/models/llm/
        let p = paths.llm_models.join(filename);
        if p.exists() {
            return p.to_string_lossy().to_string();
        }
        // 2. Walk up from CWD to find workspace models/llm/ (Tauri dev runs from a sub-crate)
        if let Ok(cwd) = std::env::current_dir() {
            let mut dir = Some(cwd.as_path());
            while let Some(d) = dir {
                let candidate = d.join("models").join("llm").join(filename);
                if candidate.exists() {
                    return candidate.to_string_lossy().to_string();
                }
                dir = d.parent();
            }
        }
        // 3. Return as-is (could be an absolute path already)
        filename.to_string()
    };

    // ── Hardware Orchestrator (non-blocking background startup) ───────────────
    // The orchestrator spawns llama-server and waits for /health (up to 120s).
    // We set AppState immediately (with orchestrator = None) so the frontend
    // is never blocked. The background task populates the RwLock when ready.
    let model_router_bg_ref = model_router.clone();
    let orch_cell: Arc<tokio::sync::RwLock<Option<Arc<Orchestrator>>>> =
        Arc::new(tokio::sync::RwLock::new(None));

    // Resolve model paths now (cheap, synchronous) so the background task
    // captures owned Strings rather than borrowing from `config`.
    //
    // The selection is **tier-aware**: on Lite/Standard hardware we pick the
    // smallest existing model (e.g. Phi-4-mini) instead of trying to load a
    // 4.7 GB Qwen2.5-VL and OOM-ing the GPU. On Performance/High hardware we
    // pick the largest fitting model with vision when available.
    //
    // The user can override the selection by setting `[llm].active_model` to
    // a model name from `[[llm.models]]`; that override is honoured iff the
    // GGUF file actually exists on disk.
    let (orch_model_path, orch_mmproj_path, orch_config, orch_enabled, selected_model_name) = if config.orchestrator.enabled {
        use kria_core::llm::orchestrator::tier_strategy::{
            derive_model_profile, select_model_for_tier, SelectionReason,
        };

        let model_exists = |file: &str| -> bool {
            let resolved = resolve_model_file(file);
            std::path::Path::new(&resolved).exists()
        };

        let choice = select_model_for_tier(
            hardware_info.tier,
            hardware_info.total_ram_mb,
            hardware_info.vram_mb,
            &config.llm.active_model,
            &config.llm.models,
            model_exists,
        );

        match choice {
            None => {
                tracing::warn!(
                    "orchestrator: no models defined in `[[llm.models]]` — \
                     skipping background startup. Add a model entry in \
                     ~/.kria/config.toml or run `scripts/download_models.py`."
                );
                let _ = handle.emit(
                    "orchestrator:disabled",
                    serde_json::json!({
                        "reason": "no_models_configured",
                        "message": "No LLM models are defined in config.toml.",
                    }),
                );
                (String::new(), None, config.orchestrator.clone(), false, String::new())
            }
            Some(c) if matches!(c.reason, SelectionReason::NoModels) => {
                let searched: Vec<String> = config
                    .llm
                    .models
                    .iter()
                    .map(|m| resolve_model_file(&m.file))
                    .collect();
                tracing::error!(
                    tier = %hardware_info.tier.as_str(),
                    searched = ?searched,
                    "orchestrator: no GGUF model files found on disk — skipping startup. \
                     Run `scripts/download_models.py` or place the GGUF in ~/.kria/models/llm/"
                );
                let _ = handle.emit(
                    "orchestrator:disabled",
                    serde_json::json!({
                        "reason": "model_files_missing",
                        "tier": hardware_info.tier.as_str(),
                        "searched_paths": searched,
                        "message": "No GGUF model files found. Download models or update config.",
                    }),
                );
                (String::new(), None, config.orchestrator.clone(), false, String::new())
            }
            Some(c) => {
                let model_path = resolve_model_file(&c.model.file);
                let mmproj_path = c
                    .model
                    .mmproj_file
                    .as_ref()
                    .filter(|_| !c.vision_disabled)
                    .map(|f| resolve_model_file(f));

                tracing::info!(
                    tier = %hardware_info.tier.as_str(),
                    model = %c.model.name,
                    file = %c.model.file,
                    resolved = %model_path,
                    reason = ?c.reason,
                    vision_disabled = c.vision_disabled,
                    mmproj = ?mmproj_path,
                    "orchestrator: tier-aware model selection complete"
                );

                // Override active_model so the model_router and other subsystems
                // agree on which model is actually loaded.
                config.llm.active_model = c.model.name.clone();

                // Derive a tier-appropriate ModelProfile and substitute it
                // into the orchestrator config. This way each model gets its
                // own VRAM-budget calculation (layer count, mmproj size, …).
                let mut orch_cfg = config.orchestrator.clone();
                orch_cfg.model_profile =
                    derive_model_profile(&c.model, &config.orchestrator.model_profile);

                // Hardware-tier safety pass: clamps mlock / flash_attention /
                // batch_size / safety_margin to values the detected machine
                // can actually handle. Without this, defaults like
                // `mlock=true` + a 5GB Qwen2.5-VL on a 16GB laptop will OOM
                // and freeze the system at startup.
                let model_size_mb = std::fs::metadata(&model_path)
                    .map(|m| m.len() / (1024 * 1024))
                    .unwrap_or((c.model.vram_estimate_gb as u64) * 1024);
                orch_cfg.tune_for_tier(
                    hardware_info.tier,
                    hardware_info.total_ram_mb,
                    hardware_info.vram_mb,
                    model_size_mb,
                );

                tracing::info!(
                    tier = %hardware_info.tier.as_str(),
                    ram_mb = hardware_info.total_ram_mb,
                    vram_mb = ?hardware_info.vram_mb,
                    model_size_mb,
                    mlock = orch_cfg.mlock,
                    flash_attention = orch_cfg.flash_attention,
                    batch_size = orch_cfg.batch_size,
                    safety_margin_mb = orch_cfg.safety_margin_mb,
                    "orchestrator: tuned config for detected hardware tier"
                );

                tracing::info!(
                    total_layers = orch_cfg.model_profile.total_layers,
                    per_layer_vram_mb = orch_cfg.model_profile.per_layer_vram_mb,
                    has_vision = orch_cfg.model_profile.has_vision_projector,
                    mmproj_vram_mb = orch_cfg.model_profile.mmproj_vram_mb,
                    max_context = orch_cfg.model_profile.max_context,
                    "orchestrator: derived model profile"
                );

                let _ = handle.emit(
                    "orchestrator:selected",
                    serde_json::json!({
                        "tier": hardware_info.tier.as_str(),
                        "model": c.model.name,
                        "display_name": c.model.display_name,
                        "vram_estimate_gb": c.model.vram_estimate_gb,
                        "vision_enabled": !c.vision_disabled
                            && c.model.capabilities.iter().any(|x| x == "vision"),
                    }),
                );

                let model_name = c.model.name.clone();
                (model_path, mmproj_path, orch_cfg, true, model_name)
            }
        }
    } else {
        tracing::info!("orchestrator: disabled in config (orchestrator.enabled = false)");
        let _ = handle.emit(
            "orchestrator:disabled",
            serde_json::json!({ "reason": "config_disabled" }),
        );
        (String::new(), None, config.orchestrator.clone(), false, String::new())
    };

    let _ = selected_model_name; // currently used only for logging above

    // Initialize embedding model and vector index for fact extraction
    let embeddings = Arc::new(EmbeddingModel::load(384).unwrap_or_else(|e| {
        tracing::warn!("embedding model load error (using fallback): {}", e);
        EmbeddingModel::load(384).expect("fallback always succeeds")
    }));
    let vectors_path = paths.data_dir.join("vectors.bin");
    let vectors = Arc::new(
        VectorIndex::open(&vectors_path, 384).unwrap_or_else(|_| VectorIndex::in_memory(384)),
    );

    // Build the full tool registry (60+ tools + 6 precognitive) with MemoryStore, RAG, and Proactive
    let rag_engine = Arc::new(kria_core::memory::RagEngine::new(
        memory_store.clone(),
        vectors.clone(),
        embeddings.clone(),
    ));
    let proactive_engine = Arc::new(kria_core::automation::ProactiveEngine::new(
        kria_core::automation::proactive::HealthThresholds::default(),
    ));
    let tool_registry_inner = registry::build_registry_full(
        Some(memory_store.clone()),
        Some(rag_engine.clone()),
        Some(proactive_engine.clone()),
    );
    kria_core::tools::precognitive::register(&tool_registry_inner, sidecar.clone());
    kria_core::tools::news::register(&tool_registry_inner, sidecar.clone());
    // Re-register vision tools with sidecar (overrides the None-sidecar registration from build_registry)
    kria_core::tools::vision::register(&tool_registry_inner, Some(sidecar.clone()));

    // ── Image generation orchestrator ─────────────────────────────────────────
    let image_cfg = config.image_generation.clone();
    let image_orchestrator = ImageOrchestrator::new(image_cfg, &paths.data_dir);
    {
        // Build an EventEmitter that forwards image/voice events to the Tauri frontend.
        let handle_img = handle.clone();
        let img_emit_fn: std::sync::Arc<dyn Fn(&str, serde_json::Value) + Send + Sync + 'static> =
            std::sync::Arc::new(move |event_name: &str, payload: serde_json::Value| {
                let _ = handle_img.emit(event_name, payload);
            });
        kria_core::tools::image_generation::register(
            &tool_registry_inner,
            image_orchestrator.clone(),
            img_emit_fn,
            orch_cell.clone(),
        );
    }
    tracing::info!("[INIT] image generation orchestrator ready");

    // ── MCP server startup ────────────────────────────────────────────────────
    // Load MCP server configs from mcp_servers.json (supplements TOML config)
    tracing::info!("[MCP] loading MCP server configs from mcp_servers.json");
    {
        let mut cfg = config.clone();
        kria_core::config::load_mcp_servers(&mut cfg);
        config = cfg;
    }
    sync_telegram_mcp_server_config(&mut config);
    sync_google_workspace_server_config(&mut config, None);
    apply_google_runtime_env_from_config(&config);
    let total_servers = config.mcp.servers.len();
    let enabled_servers = config.mcp.servers.iter().filter(|s| s.enabled).count();
    tracing::info!(
        "[MCP] {} total MCP server(s) configured, {} enabled",
        total_servers,
        enabled_servers
    );
    for s in &config.mcp.servers {
        tracing::info!(
            "[MCP]   server='{}' enabled={} command='{}' args={:?}",
            s.name,
            s.enabled,
            s.command,
            s.args
        );
    }

    // Create the lazy Google Workspace client ref BEFORE starting servers.
    // This is passed to register() so gw_* tools exist in the registry
    // regardless of whether the MCP server connects successfully.
    let gw_client_ref = gw::new_client_ref();
    tracing::info!("[GW] created lazy GwClientRef — registering Google Workspace tools now");
    gw::register(&tool_registry_inner, gw_client_ref.clone(), sidecar.clone());

    // Wrap registry in Arc immediately — thread-safe for background MCP registration
    let tool_registry = Arc::new(tool_registry_inner);
    tracing::info!(
        tools = tool_registry.len(),
        "[INIT] base tool registry ready ({} tools, MCP tools will be added in background)",
        tool_registry.len()
    );

    // Create MCP manager (servers not started yet — will launch in background)
    let mcp_configs = config.mcp.servers.clone();
    let mcp_manager: Arc<tokio::sync::Mutex<McpServerManager>> =
        Arc::new(tokio::sync::Mutex::new(McpServerManager::new(mcp_configs)));

    // Build tool mount manager (controls which tool groups are visible to the LLM)
    let mount_mgr = Arc::new(tokio::sync::RwLock::new(
        mount_manager::build_default_mount_manager(),
    ));

    // Safety subsystems
    let hitl = Arc::new(HitlGateway::new(30));

    let policy_engine = Arc::new(PolicyEngine::new());

    let audit_db = rusqlite::Connection::open(&paths.db_path)?;
    let audit_logger = Arc::new(AuditLogger::new(audit_db));

    let rollback_mgr = Arc::new(RollbackManager::new(
        paths.rollback_dir.clone(),
        24,  // retention hours
        512, // max storage MB
    ));

    // Build the agent loop
    let max_tool_rounds = config.agent.max_tool_rounds.max(1);
    let min_confidence_to_act = config.agent.min_confidence_to_act;
    let clarify_threshold = config.agent.clarify_threshold;
    let agent_loop = Arc::new(
        AgentLoop::new(
            model_router.clone(),
            tool_registry.clone(),
            mount_mgr,
            policy_engine,
            hitl.clone(),
            audit_logger,
            rollback_mgr,
        )
        .with_max_tool_rounds(max_tool_rounds)
        .with_confidence_thresholds(min_confidence_to_act, clarify_threshold)
        .with_hardware_tier(hardware_info.tier.as_str()),
    );

    tracing::info!("KRIA runtime initialized — agent loop active");

    // Build voice pipeline (v1 — always built so the legacy code path keeps
    // working). When `voice.engine = "v2"` we ALSO build the v2 pipeline
    // alongside and store it as `ActivePipeline::Streaming`.
    let voice_pipeline = build_voice_pipeline(&config, &paths);
    let (active_voice_init, voice_v2_telemetry_init) =
        if config.voice.engine.eq_ignore_ascii_case("v2") {
            match build_v2_pipeline(&config, &paths, hardware_info.tier) {
                Ok((v2, _state_rx, telemetry_rx)) => {
                    tracing::info!(engine = "v2", "voice v2 pipeline constructed");
                    (
                        kria_core::voice::v2::ActivePipeline::Streaming(v2),
                        Some(telemetry_rx),
                    )
                }
                Err(e) => {
                    tracing::warn!(error = %e, "v2 pipeline build failed; falling back to v1");
                    (
                        kria_core::voice::v2::ActivePipeline::Legacy(voice_pipeline.clone()),
                        None,
                    )
                }
            }
        } else {
            (
                kria_core::voice::v2::ActivePipeline::Legacy(voice_pipeline.clone()),
                None,
            )
        };

    // Health registry — register all subsystems
    health.register("memory_store");
    health.register("model_router");
    health.register("tool_registry");
    health.register("agent_loop");
    health.register("voice_pipeline");
    health.register("embeddings");
    health.register("vectors");
    // Mark core services as healthy
    health.update("memory_store", ServiceStatus::Healthy, None);
    // model_router: probe the actual LLM server asynchronously
    health.update(
        "model_router",
        ServiceStatus::Starting,
        Some("probing LLM server...".into()),
    );
    health.update(
        "tool_registry",
        ServiceStatus::Healthy,
        Some(format!("{} tools", tool_registry.len())),
    );
    health.update("agent_loop", ServiceStatus::Healthy, None);
    health.update("voice_pipeline", ServiceStatus::Healthy, None);
    health.update("embeddings", ServiceStatus::Healthy, None);
    health.update("vectors", ServiceStatus::Healthy, None);
    // MCP servers start in background — mark as starting
    health.register("mcp_servers");
    health.update(
        "mcp_servers",
        ServiceStatus::Starting,
        Some("connecting to MCP servers...".into()),
    );

    // Async probe of the LLM server — updates health once result is known
    // Wrap config in Arc<RwLock> early so both the probe and AppState share it
    let config = Arc::new(RwLock::new(config));
    {
        let mr = model_router.clone();
        let health_mr = health.clone();
        let config_for_probe = config.clone();
        tokio::spawn(async move {
            let status = mr.status().await;
            let healthy = status["local_healthy"].as_bool().unwrap_or(false);
            if healthy {
                // Try to detect the actual model loaded on the server
                let model_name = match mr.detect_server_model().await {
                    Some(name) => {
                        // Update the config's active_model with the detected name
                        config_for_probe.write().await.llm.active_model = name.clone();
                        name
                    }
                    None => status["local_model"]
                        .as_str()
                        .unwrap_or("unknown")
                        .to_string(),
                };
                health_mr.update(
                    "model_router",
                    ServiceStatus::Healthy,
                    Some(format!("model: {}", model_name)),
                );
            } else {
                health_mr.update(
                    "model_router",
                    ServiceStatus::Degraded,
                    Some("LLM server not reachable".into()),
                );
            }
        });
    }
    // Sidecar/OCR dependency start as "starting" — updated when probes complete.
    health.update("sidecar", ServiceStatus::Starting, None);
    health.update(
        "ocr_dependency",
        ServiceStatus::Starting,
        Some("Waiting for sidecar OCR capability probe".into()),
    );

    // Automation subsystems
    let automation_dir = paths.data_dir.join("automation");
    let _ = std::fs::create_dir_all(&automation_dir);
    // Load persisted macros and workflows
    let mut macro_rec_inner = MacroRecorder::new();
    let _ = macro_rec_inner.load_from_file(&automation_dir.join("macros.json"));
    let mut workflow_engine = WorkflowEngine::new();
    let _ = workflow_engine.load_from_file(&automation_dir.join("workflows.json"));

    let scheduler_arc = Arc::new(RwLock::new(AutomationScheduler::new()));
    let macro_recorder_arc = Arc::new(RwLock::new(macro_rec_inner));
    let workflow_engine_arc = Arc::new(RwLock::new(workflow_engine));

    tracing::info!("Automation subsystems initialized");

    // Store state in Tauri
    let telegram_bridge: Arc<RwLock<Option<TelegramBridge>>> = Arc::new(RwLock::new(None));

    // Auto-start Telegram bridge if configured.
    // If an enabled `telegram` MCP server is present, skip the built-in bridge
    // to avoid competing getUpdates long polls on the same bot token.
    let (telegram_config, telegram_mcp_enabled) = {
        let cfg = config.read().await;
        (
            cfg.telegram.clone(),
            cfg.mcp
                .servers
                .iter()
                .any(|s| s.enabled && s.name.eq_ignore_ascii_case("telegram")),
        )
    };
    if telegram_config.enabled
        && !telegram_config.bot_token.is_empty()
        && telegram_config.auto_start
    {
        if telegram_mcp_enabled {
            tracing::warn!(
                "Skipping built-in Telegram bridge auto-start because enabled MCP server 'telegram' already handles polling"
            );
        } else {
            tracing::info!("Auto-starting Telegram bridge");
            let bridge = TelegramBridge::spawn(
                telegram_config,
                agent_loop.clone(),
                memory_store.clone(),
                tool_registry.clone(),
                embeddings.clone(),
                vectors.clone(),
                hardware_info.tier.as_str().to_string(),
                orch_cell.clone(),
            );
            *telegram_bridge.write().await = Some(bridge);
        }
    }

    let (local_api_host, local_api_port) = {
        let cfg = config.read().await;
        (cfg.server.host.clone(), cfg.server.port)
    };
    let local_api_responder: Arc<dyn LocalApiResponder> = Arc::new(AgentLoopLocalApiResponder {
        agent_loop: agent_loop.clone(),
        memory_store: memory_store.clone(),
        tool_registry: tool_registry.clone(),
        embeddings: embeddings.clone(),
        vectors: vectors.clone(),
        hw_tier: hardware_info.tier.as_str().to_string(),
        orchestrator: orch_cell.clone(),
    });

    let voice_active = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let orchestrator_active_turns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let orchestrator_last_activity_at = Arc::new(tokio::sync::Mutex::new(std::time::Instant::now()));

    let (colab_enabled, colab_server_name) = {
        let cfg = config.read().await;
        (cfg.colab.enabled, cfg.colab.mcp_server_name.clone())
    };
    let colab_runtime = Arc::new(RwLock::new(ColabRuntimeSnapshot::new(
        if colab_enabled {
            ColabRuntimeState::SidecarStarting
        } else {
            ColabRuntimeState::Disconnected
        },
        colab_server_name.clone(),
    )));

    if colab_enabled {
        let colab_server_configured = {
            let cfg = config.read().await;
            cfg.mcp
                .servers
                .iter()
                .any(|s| s.enabled && s.name == colab_server_name)
        };

        if !colab_server_configured {
            let mut runtime = colab_runtime.write().await;
            runtime.state = ColabRuntimeState::Degraded;
            runtime.last_error = Some(format!(
                "Configured MCP server '{}' is missing or disabled",
                runtime.sidecar_server_name
            ));
        }
    }

    let state = AppState {
        config,
        model_router,
        agent_loop,
        tool_registry: tool_registry.clone(),
        memory_store,
        hitl,
        event_bus: event_bus.clone(),
        sidecar,
        embeddings,
        vectors,
        current_session_id: Arc::new(RwLock::new(uuid::Uuid::new_v4().to_string())),
        voice_active: voice_active.clone(),
        voice_pipeline: Arc::new(RwLock::new(voice_pipeline)),
        active_voice: Arc::new(RwLock::new(active_voice_init)),
        voice_v2_telemetry: Arc::new(tokio::sync::Mutex::new(voice_v2_telemetry_init)),
        health: health.clone(),
        scheduler: scheduler_arc,
        macro_recorder: macro_recorder_arc,
        workflow_engine: workflow_engine_arc,
        started_at: std::time::Instant::now(),
        hardware_info,
        proactive: proactive_engine,
        telegram_bridge,
        mcp_manager: mcp_manager.clone(),
        gw_client_ref: gw_client_ref.clone(),
        colab_runtime: colab_runtime.clone(),
        orchestrator: orch_cell.clone(),
        orchestrator_active_turns: orchestrator_active_turns.clone(),
        orchestrator_last_activity_at: orchestrator_last_activity_at.clone(),
        image_orchestrator,
    };

    if handle.state::<AppStateCell>().set(state).is_err() {
        tracing::error!("[INIT] AppState was already initialized — this is a bug");
    }

    tracing::info!("[INIT] AppState set — frontend is now unblocked");

    // ── Background orchestrator startup (non-blocking) ────────────────────────
    // Spawning llama-server and waiting for /health can take 30-180 seconds.
    // We do it after AppState.set() so the UI is immediately responsive.
    if orch_enabled {
        let orch_cell_bg = orch_cell.clone();
        let model_router_bg = model_router_bg_ref.clone();
        let health_bg = health.clone();
        let event_bus_bg = event_bus.clone();
        let active_turns_bg = orchestrator_active_turns.clone();
        let last_activity_bg = orchestrator_last_activity_at.clone();
        let voice_active_bg = voice_active.clone();
        let handle_bg = handle.clone();

        tokio::spawn(async move {
            tracing::info!("orchestrator: starting in background");
            match Orchestrator::start(
                orch_config,
                orch_model_path,
                orch_mmproj_path,
                event_bus_bg.clone(),
                health_bg.clone(),
            )
            .await
            {
                Ok(orch) => {
                    // orch is Arc<Orchestrator> from Orchestrator::start()
                    // Wire server manager into model router (uses OnceLock — idempotent).
                    model_router_bg.attach_server_manager(orch.server_manager.clone());
                    tracing::info!(
                        backend = ?orch.backend,
                        api_url = %orch.api_url(),
                        "orchestrator: started and attached to model router"
                    );

                    // Publish to the UI that the LLM runtime is up.
                    let _ = handle_bg.emit("orchestrator:ready", serde_json::json!({
                        "api_url": orch.api_url(),
                        "backend": format!("{:?}", orch.backend),
                    }));

                    // Start idle-release monitor if enabled.
                    if orch.config.idle_release_enabled {
                        let idle_after_secs = orch.config.idle_release_after_secs.max(30);
                        let check_interval_secs = orch.config.idle_release_check_interval_secs.max(1);
                        let active_turns = active_turns_bg.clone();
                        let last_activity = last_activity_bg.clone();
                        let voice_active_idle = voice_active_bg.clone();
                        let handle_idle = handle_bg.clone();
                        let orch_idle = orch.clone();

                        tracing::info!(
                            idle_after_secs,
                            check_interval_secs,
                            "orchestrator: idle release monitor enabled"
                        );

                        tokio::spawn(async move {
                            let idle_after = std::time::Duration::from_secs(idle_after_secs);
                            let check_interval = std::time::Duration::from_secs(check_interval_secs);
                            loop {
                                tokio::time::sleep(check_interval).await;
                                if voice_active_idle.load(std::sync::atomic::Ordering::Relaxed) {
                                    continue;
                                }
                                if active_turns.load(std::sync::atomic::Ordering::SeqCst) > 0 {
                                    continue;
                                }
                                if orch_idle.server_manager.is_swapping() {
                                    continue;
                                }
                                let idle_for = {
                                    let lock = last_activity.lock().await;
                                    lock.elapsed()
                                };
                                if idle_for < idle_after {
                                    continue;
                                }
                                if !orch_idle.server_manager.has_live_process().await {
                                    continue;
                                }
                                match orch_idle.release_if_idle("desktop_idle_timeout").await {
                                    Ok(true) => {
                                        let _ = handle_idle.emit(
                                            "orchestrator:idle_released",
                                            serde_json::json!({ "idle_for_secs": idle_for.as_secs() }),
                                        );
                                        touch_orchestrator_activity(&last_activity).await;
                                    }
                                    Ok(false) => {}
                                    Err(e) => {
                                        tracing::warn!(?e, "orchestrator: idle release attempt failed");
                                        touch_orchestrator_activity(&last_activity).await;
                                    }
                                }
                            }
                        });
                    }

                    // Start orchestrator event forwarder.
                    {
                        let handle_orch = handle_bg.clone();
                        let mut rx = event_bus_bg.subscribe();
                        tokio::spawn(async move {
                            use kria_core::infra::event_bus::KriaEvent;
                            loop {
                                match rx.recv().await {
                                    Ok(KriaEvent::LlmSwapStarted { from_ngl, to_ngl, emergency }) => {
                                        let _ = handle_orch.emit(
                                            "orchestrator:swap_started",
                                            serde_json::json!({
                                                "from_ngl": from_ngl,
                                                "to_ngl": to_ngl,
                                                "emergency": emergency,
                                            }),
                                        );
                                    }
                                    Ok(KriaEvent::LlmSwapCompleted { new_ngl, new_context, duration_ms }) => {
                                        let _ = handle_orch.emit(
                                            "orchestrator:swap_completed",
                                            serde_json::json!({
                                                "new_ngl": new_ngl,
                                                "new_context": new_context,
                                                "duration_ms": duration_ms,
                                            }),
                                        );
                                    }
                                    Ok(KriaEvent::LlmDegradationChanged { level }) => {
                                        let _ = handle_orch.emit(
                                            "orchestrator:degradation_changed",
                                            serde_json::json!({ "level": level }),
                                        );
                                    }
                                    Ok(KriaEvent::LlmStreamInterrupted) => {
                                        let _ = handle_orch.emit(
                                            "orchestrator:stream_interrupted",
                                            serde_json::json!({}),
                                        );
                                    }
                                    Ok(KriaEvent::VramPressure { free_vram_mb }) => {
                                        let _ = handle_orch.emit(
                                            "orchestrator:vram_pressure",
                                            serde_json::json!({ "free_vram_mb": free_vram_mb }),
                                        );
                                    }
                                    Ok(_) => {}
                                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                        tracing::debug!("orchestrator event forwarder lagged by {n}");
                                    }
                                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                                }
                            }
                        });
                    }

                    // Finally, store the orchestrator in the shared cell so
                    // command handlers can access it via state.orchestrator.
                    *orch_cell_bg.write().await = Some(orch);
                }
                Err(e) => {
                    tracing::error!("orchestrator: failed to start (non-fatal): {e}");
                    health_bg.register("orchestrator");
                    health_bg.update(
                        "orchestrator",
                        ServiceStatus::Degraded,
                        Some(format!("{e}")),
                    );
                    let _ = handle_bg.emit("orchestrator:error", serde_json::json!({ "error": e.to_string() }));
                }
            }
        });
    }

    start_local_api_bridge(
        local_api_host,
        local_api_port,
        local_api_responder,
        health.clone(),
    );

    // ── Background MCP server startup (non-blocking) ──────────────────────────
    // MCP servers (especially npx-based ones) can take minutes to start.
    // They run in background and dynamically register tools into the thread-safe registry.
    {
        let tool_reg_bg = tool_registry.clone();
        let mcp_mgr_bg = mcp_manager.clone();
        let gw_ref_bg = gw_client_ref.clone();
        let colab_runtime_bg = colab_runtime.clone();
        let health_bg = health.clone();
        let handle_bg = handle.clone();
        tokio::spawn(async move {
            tracing::info!("[MCP] starting MCP servers in background (parallel)");
            let mut mgr = mcp_mgr_bg.lock().await;
            mgr.start_all(&tool_reg_bg).await;

            // Wire GW client if gworkspace server started successfully
            if let Some(live_client) = mgr.get_client("gworkspace") {
                gw::set_client(&gw_ref_bg, live_client.clone()).await;
                tracing::info!(
                    "[GW] GwClientRef populated — Google Workspace tools are now active"
                );
                let _ = handle_bg.emit("gw:connected", serde_json::json!({}));
            } else {
                tracing::warn!(
                    "[GW] gworkspace MCP server not available. \
                     Google Workspace tools will return 'not connected' errors."
                );
            }

            let statuses = mgr.status().await;

            let colab_server_name = {
                let runtime = colab_runtime_bg.read().await;
                runtime.sidecar_server_name.clone()
            };
            {
                let mut runtime = colab_runtime_bg.write().await;
                if runtime.state != ColabRuntimeState::Disconnected {
                    match statuses.iter().find(|s| s.name == colab_server_name) {
                        Some(status) if status.state == McpServerState::Running => {
                            let has_notebook = runtime
                                .selected_notebook
                                .as_ref()
                                .map(|value| !value.trim().is_empty())
                                .unwrap_or(false);

                            runtime.state = if status.tool_count == 0 {
                                runtime.selected_notebook = None;
                                ColabRuntimeState::AwaitingBrowserConnection
                            } else if has_notebook {
                                ColabRuntimeState::Ready
                            } else {
                                ColabRuntimeState::NotebookSelectionRequired
                            };
                            runtime.last_error = None;
                        }
                        Some(status) => {
                            runtime.state = ColabRuntimeState::Degraded;
                            runtime.last_error = status.error.clone().or_else(|| {
                                Some(format!(
                                    "MCP server '{}' is {}",
                                    colab_server_name,
                                    mcp_state_name(status.state)
                                ))
                            });
                        }
                        None => {
                            runtime.state = ColabRuntimeState::Degraded;
                            runtime.last_error = Some(format!(
                                "MCP server '{}' not found in runtime status",
                                colab_server_name
                            ));
                        }
                    }
                }
            }

            let running = statuses.iter().filter(|s| s.tool_count > 0).count();
            health_bg.update(
                "mcp_servers",
                ServiceStatus::Healthy,
                Some(format!(
                    "{}/{} servers running, {} total tools",
                    running,
                    statuses.len(),
                    tool_reg_bg.len()
                )),
            );

            let _ = handle_bg.emit(
                "mcp:ready",
                serde_json::json!({
                    "running": running,
                    "total": statuses.len(),
                    "tools": tool_reg_bg.len(),
                }),
            );

            {
                let runtime = colab_runtime_bg.read().await;
                let _ = handle_bg.emit(
                    "colab:status",
                    serde_json::json!({
                        "state": runtime.state.as_str(),
                        "server": runtime.sidecar_server_name,
                        "selected_notebook": runtime.selected_notebook,
                        "last_error": runtime.last_error,
                    }),
                );
            }

            tracing::info!(
                "[MCP] background startup complete — {} tools available",
                tool_reg_bg.len()
            );

            // Start MCP health heartbeat (pings servers every 30s, auto-restarts on failure)
            drop(mgr);
            McpServerManager::spawn_health_heartbeat(mcp_mgr_bg, tool_reg_bg, 30);
        });
    }

    Ok(())
}

pub async fn shutdown_runtime(handle: &AppHandle) {
    let state_cell: tauri::State<'_, AppStateCell> = handle.state();
    let Some(state) = state_cell.get() else {
        tracing::info!("shutdown requested before runtime initialization finished");
        return;
    };

    tracing::info!("runtime shutdown started");

    state
        .voice_active
        .store(false, std::sync::atomic::Ordering::SeqCst);

    {
        let voice_pipeline = state.voice_pipeline.read().await.clone();
        voice_pipeline.stop().await;
    }

    {
        let mut bridge_guard = state.telegram_bridge.write().await;
        if let Some(bridge) = bridge_guard.take() {
            bridge.stop();
            tracing::info!("shutdown: telegram bridge stopped");
        }
    }

    {
        let mut manager = state.mcp_manager.lock().await;
        manager.stop_all().await;
    }

    if let Err(e) = state.sidecar.shutdown().await {
        tracing::warn!("shutdown: failed to stop sidecar cleanly: {e}");
    }

    if let Some(orchestrator) = state.orchestrator.read().await.as_ref().cloned() {
        orchestrator.shutdown().await;
    }

    tracing::info!("runtime shutdown completed");
}

async fn send_message_with_profile(
    message: String,
    execution_profile: TurnExecutionProfile,
    state: State<'_, AppStateCell>,
    app: AppHandle,
) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;

    enforce_colab_dispatch_requirements(state, &app).await?;

    touch_orchestrator_activity(&state.orchestrator_last_activity_at).await;
    let orchestrator_snapshot = state.orchestrator.read().await.clone();
    if orchestrator_snapshot.is_some() {
        emit_agent_stage(
            &app,
            "ensuring_local_runtime",
            "Ensuring local LLM runtime is ready",
            None,
        );
    }
    if let Err(e) = ensure_orchestrator_ready_for_turn(orchestrator_snapshot.as_ref(), "ui_turn").await {
        emit_agent_stage(
            &app,
            "failed",
            "Local runtime preflight failed",
            Some(serde_json::json!({ "error": e.clone() })),
        );
        return Err(e);
    }

    tracing::info!(chars = message.chars().count(), "user prompt received");
    if kria_core::infra::pipeline_trace::pipeline_debug_enabled() {
        tracing::debug!(
            target: "kria_pipeline",
            prompt = %kria_core::infra::pipeline_trace::sanitize_text_for_logs(&message, 320),
            "send_message prompt preview"
        );
    }

    emit_agent_stage(
        &app,
        "input_received",
        "Prompt received from UI",
        Some(serde_json::json!({
            "chars": message.chars().count(),
        })),
    );

    let event_scope_prefix = match execution_profile.mode {
        TurnExecutionMode::Assistant => "agent",
        TurnExecutionMode::PromptLab => "prompt_lab",
    };
    let ev_thinking = format!("{event_scope_prefix}:thinking");
    let ev_token = format!("{event_scope_prefix}:token");
    let ev_done = format!("{event_scope_prefix}:done");
    let ev_tool_call = format!("{event_scope_prefix}:tool_call");
    let ev_tool_result = format!("{event_scope_prefix}:tool_result");
    let ev_approval_required = format!("{event_scope_prefix}:approval_required");
    let ev_approval_result = format!("{event_scope_prefix}:approval_result");
    let ev_tool_choice_required = format!("{event_scope_prefix}:tool_choice_required");

    let _ = app.emit(&ev_thinking, serde_json::json!({"status": "processing"}));

    let agent_loop = state.agent_loop.clone();
    let memory_store = state.memory_store.clone();
    let tool_registry = state.tool_registry.clone();
    let event_bus = state.event_bus.clone();
    let config = state.config.read().await;
    let hw_tier = state.hardware_info.tier.as_str();

    emit_agent_stage(
        &app,
        "preparing_tool_context",
        "Collecting tool descriptions for this hardware tier",
        Some(serde_json::json!({ "hardware_tier": hw_tier })),
    );

    // Build the system prompt with tool descriptions and user context
    let tool_defs = tool_registry.list_for_tier(hw_tier);
    let tool_descriptions = build_tool_descriptions_for_prompt(&tool_defs);

    emit_agent_stage(
        &app,
        "tool_context_ready",
        "Tool descriptions prepared",
        Some(serde_json::json!({ "tool_count": tool_defs.len() })),
    );

    // Retrieve user context from memory
    let user_name = memory_store
        .get_preference("user_name")
        .unwrap_or(None)
        .unwrap_or_else(|| "User".to_string());
    let os_name = std::env::consts::OS;

    // Detect all available package managers and format as "primary (also: alt1, alt2)"
    let pm_string = {
        let pms = get_available_package_managers();
        match pms.as_slice() {
            [] => "unknown".to_string(),
            [only] => only.as_str().to_string(),
            [primary, rest @ ..] => {
                let alts: Vec<&str> = rest.iter().map(|p| p.as_str()).collect();
                format!("{} (also available: {})", primary.as_str(), alts.join(", "))
            }
        }
    };

    // Get recent memory facts for context injection
    emit_agent_stage(
        &app,
        "loading_memory_context",
        "Searching memory for relevant user facts",
        None,
    );

    let memory_context = match memory_store.search_facts(&message, 5) {
        Ok(facts) if !facts.is_empty() => {
            let fact_lines: Vec<String> = facts.iter().map(|f| format!("- {}", f.text)).collect();
            format!("Known facts about the user:\n{}", fact_lines.join("\n"))
        }
        _ => String::new(),
    };

    emit_agent_stage(
        &app,
        "memory_context_ready",
        "Memory context prepared",
        Some(serde_json::json!({
            "has_context": !memory_context.is_empty(),
        })),
    );

    let system_prompt = kria_core::agent::prompts::build_system_prompt(
        &tool_descriptions,
        &user_name,
        os_name,
        hw_tier,
        &pm_string,
        &memory_context,
    );

    emit_agent_stage(
        &app,
        "system_prompt_ready",
        "System prompt prepared and ready for LLM",
        Some(serde_json::json!({
            "prompt_chars": system_prompt.chars().count(),
        })),
    );

    drop(config);

    // Use the persistent session ID from AppState
    let session_id = state.current_session_id.read().await.clone();

    emit_agent_stage(
        &app,
        "building_message_history",
        "Building conversation history for LLM input",
        Some(serde_json::json!({
            "session_id": session_id.clone(),
        })),
    );

    // Build conversation messages (system + recent history + current message)
    let recent_turns = memory_store
        .get_recent_turns(&session_id, 20)
        .unwrap_or_default();

    let mut messages = Vec::with_capacity(recent_turns.len() + 2);
    messages.push(ChatMessage {
        role: "system".into(),
        content: system_prompt,
        name: None,
        images: None,
    });

    // Add recent conversation history
    for turn in &recent_turns {
        messages.push(ChatMessage {
            role: turn.role.clone(),
            content: turn.content.clone(),
            name: turn.tool_name.clone(),
            images: None,
        });
    }

    // Add current user message
    messages.push(ChatMessage {
        role: "user".into(),
        content: message.clone(),
        name: None,
        images: None,
    });

    // Persist user turn
    let _ = memory_store.store_turn(&ConversationTurn {
        id: None,
        session_id: session_id.clone(),
        role: "user".into(),
        content: message.clone(),
        tool_name: None,
        tool_result: None,
        tokens_used: None,
        timestamp: Utc::now(),
    });

    emit_agent_stage(
        &app,
        "user_turn_saved",
        "User prompt stored in session memory",
        Some(serde_json::json!({
            "history_turns": recent_turns.len() + 1,
        })),
    );

    // Auto-title: if this is the first message in the session, generate a title
    {
        let title_key = format!("session_title:{}", session_id);
        if memory_store
            .get_preference(&title_key)
            .unwrap_or(None)
            .is_none()
        {
            let title = if message.len() > 50 {
                format!("{}...", &message[..50])
            } else {
                message.clone()
            };
            let _ = memory_store.set_preference(&title_key, &title);
        }
    }

    // Publish event
    event_bus.publish(kria_core::infra::event_bus::KriaEvent::MessageReceived {
        session_id: session_id.clone(),
        content: message.clone(),
    });

    emit_agent_stage(
        &app,
        "dispatching_to_llm",
        "Dispatching prepared prompt to agent loop",
        None,
    );

    // Create event channel and run agent loop
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<StreamEvent>();
    state
        .orchestrator_active_turns
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let active_turns_for_tracking = state.orchestrator_active_turns.clone();
    let last_activity_for_tracking = state.orchestrator_last_activity_at.clone();

    let app_handle = app.clone();
    let session_id_clone = session_id.clone();
    let memory_store_clone = memory_store.clone();
    let embeddings_clone = state.embeddings.clone();
    let vectors_clone = state.vectors.clone();
    let user_message_clone = message.clone();
    let orchestrator_for_recovery = state.orchestrator.read().await.clone();
    let retry_agent = agent_loop.clone();
    let retry_session_id = session_id.clone();
    let retry_execution_profile = execution_profile.clone();
    let retry_messages_seed = messages.clone();

    // Spawn agent loop in background
    let agent = agent_loop.clone();
    let sid = session_id.clone();
    let run_profile = execution_profile.clone();
    tauri::async_runtime::spawn(async move {
        agent
            .run_with_profile(&sid, &mut messages, event_tx, Some(run_profile))
            .await;
    });

    emit_agent_stage(
        &app,
        "agent_loop_started",
        "Agent loop started; waiting for streamed events",
        None,
    );

    // Spawn event consumer that forwards to frontend
    tauri::async_runtime::spawn(async move {
        let mut full_response = String::new();
        let mut saw_first_token = false;
        let mut successful_tool_count = 0usize;
        let mut last_successful_tool: Option<(String, serde_json::Value)> = None;
        let mut recovery_attempted = false;
        let mut active_rx = event_rx;
        let mut pending_tool_params: std::collections::HashMap<String, serde_json::Value> =
            std::collections::HashMap::new();

        emit_agent_stage(
            &app_handle,
            "awaiting_llm_output",
            "Prompt sent to LLM; waiting for first response token",
            None,
        );

        loop {
            let event = match tokio::time::timeout(
                std::time::Duration::from_secs(AGENT_EVENT_IDLE_TIMEOUT_SECS),
                active_rx.recv(),
            )
            .await
            {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(_) => {
                    emit_agent_stage(
                        &app_handle,
                        "timed_out_waiting_for_llm",
                        "No agent events received within timeout window",
                        Some(serde_json::json!({
                            "timeout_secs": AGENT_EVENT_IDLE_TIMEOUT_SECS,
                        })),
                    );
                    full_response = AGENT_TIMEOUT_MESSAGE.to_string();
                    let _ = app_handle.emit(
                        &ev_token,
                        serde_json::json!({
                            "text": AGENT_TIMEOUT_MESSAGE,
                        }),
                    );
                    break;
                }
            };

            match event {
                StreamEvent::Token(text) => {
                    if !saw_first_token {
                        saw_first_token = true;
                        emit_agent_stage(
                            &app_handle,
                            "llm_streaming",
                            "LLM started streaming tokens",
                            None,
                        );
                    }
                    full_response.push_str(&text);
                    let _ = app_handle.emit(
                        &ev_token,
                        serde_json::json!({
                            "text": text,
                        }),
                    );
                }
                StreamEvent::ToolStart { name, params } => {
                    if kria_core::infra::pipeline_trace::pipeline_debug_enabled() {
                        tracing::debug!(
                            target: "kria_pipeline",
                            tool = %name,
                            params = ?kria_core::infra::pipeline_trace::sanitize_json_for_logs(&params, 280, 8),
                            "tool call event"
                        );
                    }
                    pending_tool_params.insert(name.clone(), params.clone());
                    emit_agent_stage(
                        &app_handle,
                        "tool_started",
                        "Tool execution started",
                        Some(serde_json::json!({
                            "tool": name.clone(),
                        })),
                    );
                    let _ = app_handle.emit(
                        &ev_tool_call,
                        serde_json::json!({
                            "name": name,
                            "params": params,
                        }),
                    );
                }
                StreamEvent::ToolEnd {
                    name,
                    result,
                    success,
                } => {
                    if success {
                        successful_tool_count = successful_tool_count.saturating_add(1);
                        last_successful_tool = Some((name.clone(), result.clone()));
                    }

                    if kria_core::infra::pipeline_trace::pipeline_debug_enabled() {
                        tracing::debug!(
                            target: "kria_pipeline",
                            tool = %name,
                            success,
                            result = ?kria_core::infra::pipeline_trace::sanitize_json_for_logs(&result, 280, 8),
                            "tool result event"
                        );
                    }
                    emit_agent_stage(
                        &app_handle,
                        "tool_finished",
                        "Tool execution completed",
                        Some(serde_json::json!({
                            "tool": name.clone(),
                            "success": success,
                        })),
                    );
                    let args = pending_tool_params
                        .remove(&name)
                        .unwrap_or_else(|| serde_json::json!({}));
                    let payload = build_tool_result_event_payload(&name, &result, success);
                    let metadata = payload
                        .get("metadata")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    let _ = app_handle.emit(&ev_tool_result, payload);

                    let persisted_payload = serde_json::json!({
                        "name": name,
                        "args": args,
                        "success": success,
                        "result": result,
                        "metadata": metadata,
                    });
                    let _ = memory_store_clone.store_turn(&ConversationTurn {
                        id: None,
                        session_id: session_id_clone.clone(),
                        role: "tool".into(),
                        content: summarize_tool_turn_for_history(
                            &name,
                            success,
                            &result,
                            persisted_payload
                                .get("metadata")
                                .unwrap_or(&serde_json::Value::Null),
                        ),
                        tool_name: Some(name),
                        tool_result: Some(persisted_payload.to_string()),
                        tokens_used: None,
                        timestamp: Utc::now(),
                    });
                }
                StreamEvent::ToolProgress { call_id, message, percent } => {
                    let _ = app_handle.emit(
                        "kria:tool-progress",
                        serde_json::json!({
                            "call_id": call_id,
                            "message": message,
                            "percent": percent,
                            "session_id": session_id_clone,
                        }),
                    );
                }
                StreamEvent::ToolPayloadChunk { call_id, seq, is_final, data } => {
                    let _ = app_handle.emit(
                        "kria:tool-payload-chunk",
                        serde_json::json!({
                            "call_id": call_id,
                            "seq": seq,
                            "is_final": is_final,
                            "data": data,
                            "session_id": session_id_clone,
                        }),
                    );
                }
                StreamEvent::ApprovalRequired {
                    request_id,
                    action,
                    risk_level,
                    parameters,
                } => {
                    emit_agent_stage(
                        &app_handle,
                        "approval_required",
                        "Agent requested user approval",
                        Some(serde_json::json!({
                            "action": action.clone(),
                            "risk_level": risk_level.clone(),
                        })),
                    );
                    let _ = app_handle.emit(
                        &ev_approval_required,
                        serde_json::json!({
                            "requestId": request_id,
                            "toolName": action,
                            "riskLevel": risk_level,
                            "args": parameters,
                            "reason": "",
                        }),
                    );
                }
                StreamEvent::ApprovalResult { action, approved } => {
                    emit_agent_stage(
                        &app_handle,
                        "approval_result",
                        "User approval decision received",
                        Some(serde_json::json!({
                            "action": action.clone(),
                            "approved": approved,
                        })),
                    );
                    let _ = app_handle.emit(
                        &ev_approval_result,
                        serde_json::json!({
                            "action": action,
                            "approved": approved,
                        }),
                    );
                }
                StreamEvent::ToolChoiceRequired {
                    query,
                    confidence,
                    min_confidence,
                    candidates,
                } => {
                    emit_agent_stage(
                        &app_handle,
                        "tool_choice_required",
                        "Low-confidence routing requires user tool selection",
                        Some(serde_json::json!({
                            "confidence": confidence,
                            "min_confidence": min_confidence,
                            "candidate_count": candidates.len(),
                        })),
                    );
                    let list: Vec<serde_json::Value> = candidates
                        .into_iter()
                        .map(|c| {
                            serde_json::json!({
                                "name": c.name,
                                "label": c.label,
                                "reason": c.reason,
                                "confidence": c.confidence,
                            })
                        })
                        .collect();
                    let _ = app_handle.emit(
                        &ev_tool_choice_required,
                        serde_json::json!({
                            "query": query,
                            "confidence": confidence,
                            "minConfidence": min_confidence,
                            "candidates": list,
                        }),
                    );
                }
                StreamEvent::Plan(plan) => {
                    emit_agent_stage(
                        &app_handle,
                        "planning",
                        "Agent is updating execution plan",
                        Some(serde_json::json!({
                            "plan": plan.clone(),
                        })),
                    );
                    let _ = app_handle.emit(
                        &ev_thinking,
                        serde_json::json!({
                            "status": "planning",
                            "plan": plan,
                        }),
                    );
                }
                StreamEvent::Error(err) => {
                    tracing::error!("Agent error: {}", err);
                    let is_transport_failure = is_likely_local_llm_transport_error(&err);

                    if is_transport_failure
                        && full_response.is_empty()
                        && successful_tool_count == 0
                        && !recovery_attempted
                    {
                        recovery_attempted = true;
                        emit_agent_stage(
                            &app_handle,
                            "llm_transport_error_recovery_started",
                            "LLM transport failed early; attempting orchestrator recovery and single retry",
                            Some(serde_json::json!({
                                "mode": match retry_execution_profile.mode {
                                    TurnExecutionMode::Assistant => "assistant",
                                    TurnExecutionMode::PromptLab => "prompt_lab",
                                },
                            })),
                        );

                        if let Some(orchestrator) = orchestrator_for_recovery.as_ref() {
                            match orchestrator.restart("transport_failure").await {
                                Ok(()) => {
                                    emit_agent_stage(
                                        &app_handle,
                                        "llm_transport_error_recovery_succeeded",
                                        "Orchestrator recovered; retrying this turn once",
                                        None,
                                    );

                                    let (retry_tx, retry_rx) =
                                        tokio::sync::mpsc::unbounded_channel::<StreamEvent>();
                                    let mut retry_messages = retry_messages_seed.clone();
                                    let retry_agent_clone = retry_agent.clone();
                                    let retry_sid_clone = retry_session_id.clone();
                                    let retry_profile_clone = retry_execution_profile.clone();

                                    tauri::async_runtime::spawn(async move {
                                        retry_agent_clone
                                            .run_with_profile(
                                                &retry_sid_clone,
                                                &mut retry_messages,
                                                retry_tx,
                                                Some(retry_profile_clone),
                                            )
                                            .await;
                                    });

                                    active_rx = retry_rx;
                                    continue;
                                }
                                Err(restart_err) => {
                                    tracing::error!(
                                        ?restart_err,
                                        "orchestrator restart failed after transport error"
                                    );
                                    emit_agent_stage(
                                        &app_handle,
                                        "llm_transport_error_recovery_failed",
                                        "Orchestrator recovery failed; falling back to error handling",
                                        Some(serde_json::json!({
                                            "error": restart_err.to_string(),
                                        })),
                                    );
                                }
                            }
                        } else {
                            emit_agent_stage(
                                &app_handle,
                                "llm_transport_error_recovery_unavailable",
                                "No orchestrator active; skipping auto-recovery",
                                None,
                            );
                        }
                    }

                    if is_transport_failure && full_response.is_empty() && successful_tool_count > 0 {
                        if let Some((tool_name, tool_result)) = last_successful_tool.as_ref() {
                            let fallback_text = build_tool_only_fallback_message(
                                tool_name,
                                true,
                                tool_result,
                            );
                            full_response = fallback_text.clone();
                            emit_agent_stage(
                                &app_handle,
                                "llm_transport_error_tool_fallback",
                                "LLM transport failed after tool success; returning tool-only fallback",
                                Some(serde_json::json!({
                                    "tool": tool_name,
                                    "successful_tool_count": successful_tool_count,
                                })),
                            );
                            let _ = app_handle.emit(
                                &ev_token,
                                serde_json::json!({
                                    "text": fallback_text,
                                }),
                            );
                            continue;
                        }
                    }

                    if is_transport_failure && !full_response.is_empty() {
                        emit_agent_stage(
                            &app_handle,
                            "llm_transport_error_after_partial_output",
                            "LLM transport failed after partial response; preserving generated content",
                            Some(serde_json::json!({
                                "response_chars": full_response.chars().count(),
                            })),
                        );
                        continue;
                    }

                    let user_visible_error = format!("⚠️ {err}");
                    if full_response.is_empty() {
                        full_response = user_visible_error.clone();
                    }
                    emit_agent_stage(
                        &app_handle,
                        "failed",
                        "Agent stream reported an error",
                        Some(serde_json::json!({
                            "error": err.clone(),
                        })),
                    );
                    let _ = app_handle.emit(
                        &ev_token,
                        serde_json::json!({
                            "text": user_visible_error,
                        }),
                    );
                }
                StreamEvent::Done(final_text) => {
                    if !final_text.is_empty() && full_response.is_empty() {
                        full_response = final_text;
                    }
                    emit_agent_stage(
                        &app_handle,
                        "llm_done",
                        "LLM stream completed",
                        Some(serde_json::json!({
                            "response_chars": full_response.chars().count(),
                        })),
                    );
                }
            }
        }

        // Persist assistant response
        if !full_response.is_empty() {
            let _ = memory_store_clone.store_turn(&ConversationTurn {
                id: None,
                session_id: session_id_clone,
                role: "assistant".into(),
                content: full_response.clone(),
                tool_name: None,
                tool_result: None,
                tokens_used: None,
                timestamp: Utc::now(),
            });

            emit_agent_stage(
                &app_handle,
                "assistant_turn_saved",
                "Assistant response stored in session memory",
                Some(serde_json::json!({
                    "response_chars": full_response.chars().count(),
                })),
            );

            // Automatic fact extraction from user message + assistant response
            let fact_mgr = kria_core::memory::facts::FactManager::new(
                &memory_store_clone,
                &vectors_clone,
                &embeddings_clone,
            );
            match fact_mgr.extract_from_turn(&user_message_clone, &full_response) {
                Ok(ids) if !ids.is_empty() => {
                    tracing::info!(count = ids.len(), "auto-extracted facts from conversation");
                    emit_agent_stage(
                        &app_handle,
                        "facts_extracted",
                        "New user facts extracted from the conversation",
                        Some(serde_json::json!({
                            "fact_count": ids.len(),
                        })),
                    );
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("fact extraction failed: {}", e),
            }
        }

        emit_agent_stage(
            &app_handle,
            "completed",
            "Pipeline completed and UI will finalize rendering",
            None,
        );

        let _ = app_handle.emit(&ev_done, serde_json::json!({}));
        decrement_active_turn_counter(&active_turns_for_tracking);
        touch_orchestrator_activity(&last_activity_for_tracking).await;
    });

    Ok(serde_json::json!({
        "status": "processing",
    }))
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct LabExecutionProfileInput {
    pub app_lock: Option<String>,
    pub tool_lock: Option<String>,
    pub strategy: Option<String>,
}

impl LabExecutionProfileInput {
    fn tool_selection_strategy(&self) -> PromptLabToolSelectionStrategy {
        match self
            .strategy
            .as_deref()
            .map(|value| value.trim().to_ascii_lowercase())
        {
            Some(value)
                if value == "direct"
                    || value == "direct_locked_tool"
                    || value == "direct-locked-tool" =>
            {
                PromptLabToolSelectionStrategy::DirectLockedTool
            }
            _ => PromptLabToolSelectionStrategy::RoutedWithinLock,
        }
    }

    fn to_core_profile(&self) -> TurnExecutionProfile {
        TurnExecutionProfile::prompt_lab(
            self.app_lock.clone(),
            self.tool_lock.clone(),
            self.tool_selection_strategy(),
        )
    }
}

#[tauri::command]
pub async fn send_message(
    message: String,
    state: State<'_, AppStateCell>,
    app: AppHandle,
) -> Result<serde_json::Value, String> {
    send_message_with_profile(message, TurnExecutionProfile::assistant(), state, app).await
}

#[tauri::command]
pub async fn send_lab_message(
    message: String,
    profile: Option<LabExecutionProfileInput>,
    state: State<'_, AppStateCell>,
    app: AppHandle,
) -> Result<serde_json::Value, String> {
    let execution_profile = profile
        .map(|value| value.to_core_profile())
        .unwrap_or_else(|| {
            TurnExecutionProfile::prompt_lab(
                None,
                None,
                PromptLabToolSelectionStrategy::RoutedWithinLock,
            )
        });
    send_message_with_profile(message, execution_profile, state, app).await
}

#[tauri::command]
pub async fn get_session_history(
    session_id: Option<String>,
    state: State<'_, AppStateCell>,
) -> Result<Vec<serde_json::Value>, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let session_id = match session_id {
        Some(id) if !id.trim().is_empty() => id,
        _ => state.current_session_id.read().await.clone(),
    };
    let turns = state
        .memory_store
        .get_recent_turns(&session_id, 100)
        .map_err(|e| e.to_string())?;
    let messages: Vec<serde_json::Value> = turns
        .iter()
        .map(|t| {
            serde_json::json!({
                "role": t.role,
                "content": t.content,
                "tool_name": t.tool_name,
                "tool_result": t.tool_result,
                "timestamp": t.timestamp.to_rfc3339(),
            })
        })
        .collect();
    Ok(messages)
}

fn normalize_session_title(raw: &str) -> Option<String> {
    const SESSION_TITLE_MAX_CHARS: usize = 72;

    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut title: String = trimmed.chars().take(SESSION_TITLE_MAX_CHARS).collect();
    if trimmed.chars().count() > SESSION_TITLE_MAX_CHARS {
        title.push('…');
    }
    Some(title)
}

#[tauri::command]
pub async fn create_session(
    title: Option<String>,
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let new_id = uuid::Uuid::new_v4().to_string();
    *state.current_session_id.write().await = new_id.clone();

    // Store metadata preferences so empty sessions are still visible in the UI.
    let provided_title = title
        .as_deref()
        .and_then(normalize_session_title);
    let resolved_title = provided_title
        .clone()
        .unwrap_or_else(|| "New chat".to_string());
    let _ = state
        .memory_store
        .set_preference(&format!("session_title:{}", new_id), &resolved_title);
    let _ = state.memory_store.set_preference(
        &format!("session_title_manual:{}", new_id),
        if provided_title.is_some() { "1" } else { "0" },
    );
    let _ = state.memory_store.set_preference(
        &format!("session_created_at:{}", new_id),
        &Utc::now().to_rfc3339(),
    );

    tracing::info!(session_id = %new_id, "new session created");
    Ok(serde_json::json!({
        "session_id": new_id,
    }))
}

#[tauri::command]
pub async fn list_sessions(
    state: State<'_, AppStateCell>,
) -> Result<Vec<serde_json::Value>, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let sessions = state
        .memory_store
        .list_sessions()
        .map_err(|e| e.to_string())?;
    let current = state.current_session_id.read().await.clone();
    let mut result: Vec<serde_json::Value> = sessions
        .into_iter()
        .map(|(id, count, last_active)| {
            let title = state
                .memory_store
                .get_preference(&format!("session_title:{}", id))
                .unwrap_or(None)
                .unwrap_or_else(|| format!("Session ({})", &id[..8]));
            serde_json::json!({
                "id": id,
                "title": title,
                "turn_count": count,
                "message_count": count,
                "last_active": last_active,
                "is_current": id == current,
            })
        })
        .collect();

    // Include the current session even when it has no turns yet.
    if !current.trim().is_empty()
        && !result
            .iter()
            .any(|row| row.get("id").and_then(|v| v.as_str()) == Some(current.as_str()))
    {
        let title = state
            .memory_store
            .get_preference(&format!("session_title:{}", current))
            .unwrap_or(None)
            .unwrap_or_else(|| "New chat".to_string());
        let created_at = state
            .memory_store
            .get_preference(&format!("session_created_at:{}", current))
            .unwrap_or(None)
            .unwrap_or_else(|| Utc::now().to_rfc3339());
        result.insert(
            0,
            serde_json::json!({
                "id": current,
                "title": title,
                "turn_count": 0,
                "message_count": 0,
                "last_active": created_at,
                "is_current": true,
            }),
        );
    }

    Ok(result)
}

#[tauri::command]
pub async fn switch_session(
    session_id: String,
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    *state.current_session_id.write().await = session_id.clone();
    // Load history for the new session
    let turns = state
        .memory_store
        .get_recent_turns(&session_id, 100)
        .map_err(|e| e.to_string())?;
    let messages: Vec<serde_json::Value> = turns
        .iter()
        .map(|t| {
            serde_json::json!({
                "role": t.role,
                "content": t.content,
                "tool_name": t.tool_name,
                "tool_result": t.tool_result,
                "timestamp": t.timestamp.to_rfc3339(),
            })
        })
        .collect();
    Ok(serde_json::json!({
        "session_id": session_id,
        "messages": messages,
    }))
}

#[tauri::command]
pub async fn delete_session(
    session_id: String,
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let session_id = session_id.trim().to_string();
    if session_id.is_empty() {
        return Err("Session id cannot be empty".into());
    }

    let current = state.current_session_id.read().await.clone();
    state
        .memory_store
        .delete_session(&session_id)
        .map_err(|e| e.to_string())?;

    let mut replacement_session_id: Option<String> = None;

    // If we deleted the current session, create a new one
    if session_id == current {
        let new_id = uuid::Uuid::new_v4().to_string();
        *state.current_session_id.write().await = new_id.clone();

        let _ = state
            .memory_store
            .set_preference(&format!("session_title:{}", new_id), "New chat");
        let _ = state
            .memory_store
            .set_preference(&format!("session_title_manual:{}", new_id), "0");
        let _ = state.memory_store.set_preference(
            &format!("session_created_at:{}", new_id),
            &Utc::now().to_rfc3339(),
        );

        replacement_session_id = Some(new_id);
    }

    Ok(serde_json::json!({
        "deleted_session_id": session_id,
        "replacement_session_id": replacement_session_id,
    }))
}

#[tauri::command]
pub async fn rename_session(
    session_id: String,
    title: String,
    state: State<'_, AppStateCell>,
) -> Result<(), String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let session_id = session_id.trim().to_string();
    if session_id.is_empty() {
        return Err("Session id cannot be empty".into());
    }

    let resolved_title = normalize_session_title(&title)
        .ok_or_else(|| "Session title cannot be empty".to_string())?;

    let key = format!("session_title:{}", session_id);
    state
        .memory_store
        .set_preference(&key, &resolved_title)
        .map_err(|e| e.to_string())?;
    state
        .memory_store
        .set_preference(&format!("session_title_manual:{}", session_id), "1")
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn auto_rename_session(
    session_id: String,
    title: String,
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let session_id = session_id.trim().to_string();
    if session_id.is_empty() {
        return Err("Session id cannot be empty".into());
    }

    let resolved_title = match normalize_session_title(&title) {
        Some(t) => t,
        None => {
            return Ok(serde_json::json!({
                "updated": false,
                "reason": "empty_title",
            }))
        }
    };

    let manual_key = format!("session_title_manual:{}", session_id);
    let manual_flag = state
        .memory_store
        .get_preference(&manual_key)
        .map_err(|e| e.to_string())?
        .unwrap_or_else(|| "0".to_string());

    if manual_flag == "1" {
        let existing_title = state
            .memory_store
            .get_preference(&format!("session_title:{}", session_id))
            .map_err(|e| e.to_string())?
            .unwrap_or_else(|| "New chat".to_string());

        return Ok(serde_json::json!({
            "updated": false,
            "reason": "manual_title",
            "title": existing_title,
        }));
    }

    state
        .memory_store
        .set_preference(&format!("session_title:{}", session_id), &resolved_title)
        .map_err(|e| e.to_string())?;
    state
        .memory_store
        .set_preference(&manual_key, "0")
        .map_err(|e| e.to_string())?;

    Ok(serde_json::json!({
        "updated": true,
        "title": resolved_title,
    }))
}

#[tauri::command]
pub async fn search_sessions(
    query: String,
    state: State<'_, AppStateCell>,
) -> Result<Vec<serde_json::Value>, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let results = state
        .memory_store
        .search_conversations(&query, 20)
        .map_err(|e| e.to_string())?;
    let items: Vec<serde_json::Value> = results
        .into_iter()
        .map(|t| {
            serde_json::json!({
                "session_id": t.session_id,
                "role": t.role,
                "content": t.content,
                "timestamp": t.timestamp.to_rfc3339(),
            })
        })
        .collect();
    Ok(items)
}

#[tauri::command]
pub async fn cancel_request(state: State<'_, AppStateCell>) -> Result<(), String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    state.hitl.cancel_all().await;
    Ok(())
}

#[tauri::command]
pub async fn cancel_turn(
    session_id: String,
    state: State<'_, AppStateCell>,
) -> Result<(), String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    state.agent_loop.cancel_session(&session_id);
    Ok(())
}

#[tauri::command]
pub async fn approve_action(
    request_id: String,
    state: State<'_, AppStateCell>,
) -> Result<(), String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    state
        .hitl
        .respond(&request_id, ApprovalResponse::Approved)
        .await;
    Ok(())
}

#[tauri::command]
pub async fn deny_action(
    request_id: String,
    _reason: Option<String>,
    state: State<'_, AppStateCell>,
) -> Result<(), String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    state
        .hitl
        .respond(&request_id, ApprovalResponse::Denied)
        .await;
    Ok(())
}

#[tauri::command]
pub async fn get_health(state: State<'_, AppStateCell>) -> Result<serde_json::Value, String> {
    // If AppState is not yet initialized, return a "starting" payload so the
    // UI can show "Warming up" instead of staying stuck on "Booting".
    let Some(state) = state.get() else {
        return Ok(serde_json::json!({
            "status": "starting",
            "uptime_secs": 0,
            "tool_count": 0,
            "services": [
                {"name": "runtime", "status": "starting", "message": "KRIA is initializing…"}
            ],
            "hardware": {}
        }));
    };
    // Refresh LLM server health on each call
    let mr_status = state.model_router.status().await;
    let mr_healthy = mr_status["local_healthy"].as_bool().unwrap_or(false);
    let mr_model = mr_status["local_model"].as_str().unwrap_or("unknown");
    if mr_healthy {
        state.health.update(
            "model_router",
            ServiceStatus::Healthy,
            Some(format!("model: {}", mr_model)),
        );
    } else {
        state.health.update(
            "model_router",
            ServiceStatus::Degraded,
            Some("LLM server not reachable".into()),
        );
    }

    // Refresh OCR dependency status from sidecar so UI can warn users before first upload.
    {
        let health = state.health.clone();
        let sidecar = state.sidecar.clone();
        tokio::spawn(async move {
            refresh_ocr_dependency_health(&health, &sidecar).await;
        });
    }

    let services = state.health.status_all();
    let all_healthy = state.health.all_healthy();
    let uptime = state.started_at.elapsed().as_secs();
    let tool_count = state.tool_registry.len();
    let hw = &state.hardware_info;

    Ok(serde_json::json!({
        "status": if all_healthy { "healthy" } else { "degraded" },
        "uptime_secs": uptime,
        "tool_count": tool_count,
        "services": services,
        "hardware": {
            "tier": hw.tier.as_str(),
            "cpu_cores": hw.cpu_cores,
            "total_ram_mb": hw.total_ram_mb,
            "vram_mb": hw.vram_mb,
            "gpu_name": hw.gpu_name,
            "os": format!("{:?}", hw.os),
            "hostname": hw.hostname,
        }
    }))
}

#[tauri::command]
pub async fn get_hardware_info(
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let hw = &state.hardware_info;
    Ok(serde_json::json!({
        "tier": hw.tier.as_str(),
        "cpu_cores": hw.cpu_cores,
        "total_ram_mb": hw.total_ram_mb,
        "vram_mb": hw.vram_mb,
        "gpu_name": hw.gpu_name,
        "os": format!("{:?}", hw.os),
        "hostname": hw.hostname,
        "package_manager": hw.package_manager.map(|pm| format!("{:?}", pm)),
        "vision_capable": hw.tier.has_vision(),
        "recommended_model": hw.tier.recommended_model(),
        "recommended_stt": hw.tier.stt_model(),
        "context_window": hw.tier.context_window(),
        "gpu_layers": hw.tier.gpu_layers(),
        "threads": hw.tier.thread_count(),
    }))
}

#[tauri::command]
pub async fn get_settings(state: State<'_, AppStateCell>) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let config = state.config.read().await;
    serde_json::to_value(&*config).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_audio_devices() -> Result<serde_json::Value, String> {
    let inputs = list_input_devices().unwrap_or_default();
    let outputs = list_output_devices().unwrap_or_default();
    Ok(serde_json::json!({
        "inputs": inputs,
        "outputs": outputs,
        "default_input": default_input_device_name(),
        "default_output": default_output_device_name(),
    }))
}

#[tauri::command]
pub async fn update_settings(
    settings: serde_json::Value,
    state: State<'_, AppStateCell>,
) -> Result<(), String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let mut new_config: KriaConfig = serde_json::from_value(settings).map_err(|e| e.to_string())?;
    sync_telegram_mcp_server_config(&mut new_config);
    sync_google_workspace_server_config(&mut new_config, None);
    apply_google_runtime_env_from_config(&new_config);
    // Persist to disk first
    new_config.save().map_err(|e| e.to_string())?;
    // Then update in-memory config
    let mut config = state.config.write().await;
    *config = new_config;

    drop(config);
    let _ = apply_mcp_runtime_from_config(state).await;

    Ok(())
}

#[tauri::command]
pub async fn list_knowledge_base(
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let docs = state
        .memory_store
        .list_documents()
        .map_err(|e| e.to_string())?;
    let items: Vec<serde_json::Value> = docs
        .iter()
        .map(|(id, name, dtype, chunks)| {
            serde_json::json!({
                "doc_id": id,
                "name": name,
                "type": dtype,
                "chunks": chunks,
            })
        })
        .collect();
    Ok(serde_json::json!({ "documents": items, "count": items.len() }))
}

#[tauri::command]
pub async fn get_alerts(state: State<'_, AppStateCell>) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let alerts = state.proactive.get_alerts().await;
    let items: Vec<serde_json::Value> = alerts
        .iter()
        .map(|a| {
            serde_json::json!({
                "id": a.id,
                "category": format!("{:?}", a.category).to_lowercase(),
                "title": a.title,
                "message": a.message,
                "suggestion": a.suggestion,
                "timestamp": a.timestamp.to_rfc3339(),
            })
        })
        .collect();
    Ok(serde_json::json!({ "alerts": items, "count": items.len() }))
}

/// Write arbitrary text content to a file chosen by the user via a save dialog.
/// Returns the absolute path of the saved file, or null if cancelled.
#[tauri::command]
pub async fn save_export_file(
    content: String,
    default_name: String,
    filter_name: String,
    extensions: Vec<String>,
    app: AppHandle,
) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::{DialogExt, FilePath};

    // Ask the user where to save
    let path = app
        .dialog()
        .file()
        .set_file_name(&default_name)
        .add_filter(
            &filter_name,
            &extensions.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        )
        .blocking_save_file();

    let saved_path = match path {
        Some(FilePath::Path(p)) => p,
        _ => return Ok(None), // cancelled or unsupported
    };

    std::fs::write(&saved_path, content.as_bytes())
        .map_err(|e| format!("Failed to write file: {e}"))?;

    Ok(Some(saved_path.to_string_lossy().to_string()))
}

/// Write HTML to a temp file and return its path so the frontend can open it
/// with the system browser for print-to-PDF.
#[tauri::command]
pub async fn open_html_for_print(
    html: String,
    filename: String,
    _app: AppHandle,
) -> Result<(), String> {
    // Write HTML to the OS temp directory
    let mut path = std::env::temp_dir();
    path.push(&filename);
    std::fs::write(&path, html.as_bytes())
        .map_err(|e| format!("Failed to write temp file: {e}"))?;

    let path_str = path.to_string_lossy().to_string();

    // Open with the default system browser using platform-specific command
    #[cfg(target_os = "linux")]
    std::process::Command::new("xdg-open")
        .arg(&path_str)
        .spawn()
        .map_err(|e| format!("Failed to open file: {e}"))?;

    #[cfg(target_os = "macos")]
    std::process::Command::new("open")
        .arg(&path_str)
        .spawn()
        .map_err(|e| format!("Failed to open file: {e}"))?;

    #[cfg(target_os = "windows")]
    std::process::Command::new("cmd")
        .args(["/c", "start", "", &path_str])
        .spawn()
        .map_err(|e| format!("Failed to open file: {e}"))?;

    Ok(())
}

/// Read a local image file and return it as a base64 data URL.
/// Used by the frontend to display generated/uploaded images stored on disk.
#[tauri::command]
pub async fn read_local_image(path: String, state: State<'_, AppStateCell>) -> Result<String, String> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use std::path::PathBuf;

    // Path safety: allow reads only under ~/.kria and configured image output roots.
    let canonical = std::fs::canonicalize(&path).map_err(|e| format!("Cannot resolve path: {e}"))?;
    let home = dirs::home_dir().unwrap_or_default();

    let mut allowed_roots: Vec<PathBuf> = vec![home.join(".kria")];

    if let Some(app_state) = state.get() {
        let config = app_state.config.read().await;
        if let Ok(paths) = config.resolve_paths() {
            let configured = if config.image_generation.output_dir.trim().is_empty() {
                paths.data_dir.join("cache/images")
            } else {
                let p = PathBuf::from(config.image_generation.output_dir.trim());
                if p.is_absolute() {
                    p
                } else {
                    paths.data_dir.join(p)
                }
            };
            allowed_roots.push(configured);
            allowed_roots.push(paths.data_dir.join("uploads"));
            allowed_roots.push(paths.data_dir.join("attachments"));
        }
    }

    let allowed = allowed_roots.into_iter().any(|root| {
        let normalized = if root.exists() {
            std::fs::canonicalize(&root).unwrap_or(root)
        } else {
            root
        };
        canonical.starts_with(normalized)
    });

    if !allowed {
        return Err("Access denied: image path is outside configured KRIA storage roots".into());
    }

    let bytes = tokio::fs::read(&canonical).await.map_err(|e| format!("Read failed: {e}"))?;

    let ext = canonical
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("jpg")
        .to_lowercase();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "image/jpeg",
    };

    let encoded = STANDARD.encode(&bytes);
    Ok(format!("data:{};base64,{}", mime, encoded))
}

/// Save an uploaded image to ~/.kria/uploads/user/ and return the saved path.
#[tauri::command]
pub async fn save_uploaded_image(
    data: Vec<u8>,
    mime_type: String,
    session_id: String,
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};

    let home = dirs::home_dir().ok_or("Cannot find home directory")?;
    let now = chrono::Utc::now();
    let month_dir = home.join(".kria").join("uploads").join("user")
        .join(now.format("%Y-%m").to_string());
    tokio::fs::create_dir_all(&month_dir).await.map_err(|e| e.to_string())?;

    let ext = match mime_type.as_str() {
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "jpg",
    };
    let ts = now.timestamp_millis();
    let filename = format!("user_{}.{}", ts, ext);
    let path = month_dir.join(&filename);

    tokio::fs::write(&path, &data).await.map_err(|e| e.to_string())?;

    let sha = {
        use sha2::{Sha256, Digest};
        let mut hasher = Sha256::new();
        hasher.update(&data);
        format!("{:x}", hasher.finalize())
    };

    // Store in SQLite chat_media table
    if let Some(s) = state.get() {
        let path_str = path.to_string_lossy().to_string();
        let _ = s.memory_store.store_chat_media(&kria_core::memory::store::ChatMediaRecord {
            session_id: session_id.clone(),
            media_type: "uploaded".into(),
            file_path: path_str.clone(),
            sha256: Some(sha.clone()),
            prompt: None,
            width: None,
            height: None,
            style: None,
            provenance: Some("user_upload".into()),
        });

        // Return base64 data URL so the frontend can display immediately
        let encoded = STANDARD.encode(&data);
        let data_url = format!("data:{};base64,{}", mime_type, encoded);
        return Ok(serde_json::json!({
            "path": path_str,
            "sha256": sha,
            "data_url": data_url,
        }));
    }

    Ok(serde_json::json!({
        "path": path.to_string_lossy().to_string(),
        "sha256": sha,
    }))
}

/// Return all chat media (images) for a session.
#[tauri::command]
pub async fn get_session_media(
    session_id: String,
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or("KRIA is still initializing — please try again in a moment")?;
    let records = state.memory_store.get_session_media(&session_id).map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "media": records }))
}

#[tauri::command]
pub async fn list_models(state: State<'_, AppStateCell>) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let config = state.config.read().await;
    let paths = config.resolve_paths().map_err(|e| e.to_string())?;
    let mgr = kria_core::llm::model_manager::ModelManager::new(paths.models_dir.join("llm"));
    let models = mgr.list_llm_models();
    Ok(serde_json::to_value(&models).unwrap_or_default())
}

#[tauri::command]
pub async fn start_voice(state: State<'_, AppStateCell>, app: AppHandle) -> Result<(), String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    if state
        .voice_active
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return Ok(()); // Already active
    }

    // Pre-flight checks: verify required binaries and models exist
    let whisper_available = which_binary("whisper-cpp")
        .or_else(|| which_binary("main"))
        .is_some();
    if !whisper_available {
        return Err("Voice requires whisper-cpp (or 'main' binary from whisper.cpp) on your PATH. Install it with: sudo apt install whisper.cpp OR build from https://github.com/ggerganov/whisper.cpp".into());
    }

    let piper_available = which_binary("piper").is_some();
    if !piper_available {
        return Err("Voice requires Piper TTS binary on your PATH. Install it from: https://github.com/rhasspy/piper/releases".into());
    }

    // Refresh config from disk on every voice start so external edits in
    // ~/.kria/config.toml are not stuck behind stale in-memory state.
    let effective_config = match KriaConfig::load(None) {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::warn!(error = %e, "failed to reload config from disk for voice start; using in-memory config");
            state.config.read().await.clone()
        }
    };
    {
        let mut cfg_guard = state.config.write().await;
        *cfg_guard = effective_config.clone();
    }

    // Verify required models and rebuild pipeline from latest saved settings.
    let voice_pipeline = {
        let paths = effective_config
            .resolve_paths()
            .map_err(|e| e.to_string())?;

        let stt_model = paths
            .models_dir
            .join("stt")
            .join(&effective_config.voice.stt_model);
        if !stt_model.exists() {
            return Err(format!(
                "STT model not found at: {}. Run 'python scripts/download_models.py' to download models.",
                stt_model.display()
            ));
        }

        let tts_voice_file = format!("{}.onnx", effective_config.voice.tts_voice);
        let tts_model = paths.models_dir.join("piper").join(&tts_voice_file);
        if !tts_model.exists() {
            return Err(format!(
                "TTS voice model not found at: {}. Run 'python scripts/download_models.py' to download models.",
                tts_model.display()
            ));
        }

        build_voice_pipeline(&effective_config, &paths)
    };

    {
        let mut vp_guard = state.voice_pipeline.write().await;
        *vp_guard = voice_pipeline.clone();
    }

    // ── v2 hot-swap ───────────────────────────────────────────────────────
    // If the (freshly reloaded) config requests v2 but active_voice is still
    // Legacy (e.g. config changed from v1→v2 after init_runtime ran, or the
    // v2 build failed at startup and the user fixed the prerequisites), try
    // to build the v2 pipeline now and swap it in atomically.
    if effective_config.voice.engine.eq_ignore_ascii_case("v2")
        && !state.active_voice.read().await.is_streaming()
    {
        match effective_config
            .resolve_paths()
            .ok()
            .and_then(|p| {
                build_v2_pipeline(&effective_config, &p, state.hardware_info.tier).ok()
            }) {
            Some((v2_pipeline, _state_rx, telemetry_rx)) => {
                tracing::info!("voice: hot-swapping active_voice → v2");
                *state.active_voice.write().await =
                    kria_core::voice::v2::ActivePipeline::Streaming(v2_pipeline);
                *state.voice_v2_telemetry.lock().await = Some(telemetry_rx);
            }
            None => {
                tracing::warn!(
                    "voice: v2 hot-swap failed; continuing with v1 pipeline"
                );
            }
        }
    }

    state
        .voice_active
        .store(true, std::sync::atomic::Ordering::Relaxed);

    // ── v2 continuous mic-capture loop ────────────────────────────────────
    // When the pipeline is the v2 streaming FSM, bypass the v1 event loop
    // entirely and spin up a self-contained capture→run_turn loop. All v1
    // validation above is still performed (binary/model checks) so the
    // same config requirements apply.
    if let Some(v2) = state.active_voice.read().await.streaming() {
        start_voice_v2_loop(
            v2,
            state.voice_active.clone(),
            state.voice_v2_telemetry.clone(),
            state.model_router.clone(),
            state.current_session_id.clone(),
            state.config.clone(),
            state.hardware_info.clone(),
            state.memory_store.clone(),
            state.tool_registry.clone(),
            app.clone(),
        )
        .await;
        return Ok(());
    }

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<VoicePipelineEvent>();

    if let Err(e) = voice_pipeline.start(event_tx).await {
        state
            .voice_active
            .store(false, std::sync::atomic::Ordering::Relaxed);
        return Err(e.to_string());
    }

    let _ = app.emit("voice:state", serde_json::json!({ "state": "listening" }));

    // Spawn a task that listens for voice pipeline events and forwards them
    let app_handle = app.clone();
    let voice_pipeline = voice_pipeline.clone();
    let memory_store = state.memory_store.clone();
    let agent_loop = state.agent_loop.clone();
    let tool_registry = state.tool_registry.clone();
    let event_bus = state.event_bus.clone();
    let config = state.config.clone();
    let session_id_lock = state.current_session_id.clone();
    let embeddings = state.embeddings.clone();
    let vectors = state.vectors.clone();
    let hw_info_voice = state.hardware_info.clone();
    let orchestrator_voice = state.orchestrator.read().await.clone();
    let active_turns_voice = state.orchestrator_active_turns.clone();
    let last_activity_voice = state.orchestrator_last_activity_at.clone();

    tauri::async_runtime::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            match event {
                VoicePipelineEvent::StateChanged(new_state) => {
                    let state_str = match new_state {
                        VoicePipelineState::Idle => "idle",
                        VoicePipelineState::Listening => "listening",
                        VoicePipelineState::Processing => "processing",
                        VoicePipelineState::Speaking => "speaking",
                    };
                    let _ =
                        app_handle.emit("voice:state", serde_json::json!({ "state": state_str }));
                }
                VoicePipelineEvent::PartialTranscript(frame) => {
                    let _ = app_handle.emit(
                        "voice:partial_transcript",
                        serde_json::json!({
                            "text": frame.text,
                            "confidence": frame.confidence,
                            "language": frame.language,
                            "stability": frame.stability,
                            "partial": true,
                        }),
                    );
                }
                VoicePipelineEvent::Transcript(frame) => {
                    let text = frame.text;
                    let language = frame.language;
                    let confidence = frame.confidence;

                    tracing::info!(
                        language = %language,
                        confidence,
                        chars = text.chars().count(),
                        "voice transcript received"
                    );
                    if kria_core::infra::pipeline_trace::pipeline_debug_enabled() {
                        tracing::debug!(
                            target: "kria_pipeline",
                            transcript = %kria_core::infra::pipeline_trace::sanitize_text_for_logs(&text, 320),
                            language = %language,
                            confidence,
                            "voice transcript preview"
                        );
                    }
                    let _ = app_handle.emit(
                        "voice:transcript",
                        serde_json::json!({
                            "text": text.clone(),
                            "confidence": confidence,
                            "language": language.clone(),
                            "stability": 1.0,
                        }),
                    );

                    touch_orchestrator_activity(&last_activity_voice).await;
                    if let Err(e) = ensure_orchestrator_ready_for_turn(
                        orchestrator_voice.as_ref(),
                        "voice_turn",
                    )
                    .await
                    {
                        tracing::warn!(?e, "voice turn preflight failed");
                        let _ = app_handle.emit(
                            "agent:token",
                            serde_json::json!({ "text": format!("⚠️ {e}") }),
                        );
                        let _ = app_handle.emit("agent:done", serde_json::json!({}));
                        continue;
                    }
                    active_turns_voice.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

                    // Feed transcript through the agent loop (same as send_message)
                    let session_id = session_id_lock.read().await.clone();
                    let config_guard = config.read().await;
                    let hw_tier = hw_info_voice.tier.as_str();

                    let tool_defs = tool_registry.list_for_tier(hw_tier);
                    let tool_descriptions = build_tool_descriptions_for_prompt(&tool_defs);

                    let user_name = memory_store
                        .get_preference("user_name")
                        .unwrap_or(None)
                        .unwrap_or_else(|| "User".to_string());
                    let os_name = std::env::consts::OS;
                    let pm_string_voice = {
                        let pms = get_available_package_managers();
                        match pms.as_slice() {
                            [] => "unknown".to_string(),
                            [only] => only.as_str().to_string(),
                            [primary, rest @ ..] => {
                                let alts: Vec<&str> = rest.iter().map(|p| p.as_str()).collect();
                                format!(
                                    "{} (also available: {})",
                                    primary.as_str(),
                                    alts.join(", ")
                                )
                            }
                        }
                    };
                    let memory_context = match memory_store.search_facts(&text, 5) {
                        Ok(facts) if !facts.is_empty() => {
                            let lines: Vec<String> =
                                facts.iter().map(|f| format!("- {}", f.text)).collect();
                            format!("Known facts about the user:\n{}", lines.join("\n"))
                        }
                        _ => String::new(),
                    };

                    let system_prompt = kria_core::agent::prompts::build_system_prompt(
                        &tool_descriptions,
                        &user_name,
                        os_name,
                        hw_tier,
                        &pm_string_voice,
                        &memory_context,
                    );
                    drop(config_guard);

                    let recent_turns = memory_store
                        .get_recent_turns(&session_id, 20)
                        .unwrap_or_default();
                    let mut messages = Vec::with_capacity(recent_turns.len() + 2);
                    messages.push(ChatMessage {
                        role: "system".into(),
                        content: system_prompt,
                        name: None,
                        images: None,
                    });
                    for turn in &recent_turns {
                        messages.push(ChatMessage {
                            role: turn.role.clone(),
                            content: turn.content.clone(),
                            name: turn.tool_name.clone(),
                            images: None,
                        });
                    }
                    messages.push(ChatMessage {
                        role: "user".into(),
                        content: text.clone(),
                        name: None,
                        images: None,
                    });

                    let _ = memory_store.store_turn(&ConversationTurn {
                        id: None,
                        session_id: session_id.clone(),
                        role: "user".into(),
                        content: format!("🎤 {}", text),
                        tool_name: None,
                        tool_result: None,
                        tokens_used: None,
                        timestamp: Utc::now(),
                    });

                    event_bus.publish(kria_core::infra::event_bus::KriaEvent::MessageReceived {
                        session_id: session_id.clone(),
                        content: text.clone(),
                    });

                    let _ = app_handle.emit(
                        "agent:thinking",
                        serde_json::json!({"status": "processing"}),
                    );

                    let (agent_tx, mut agent_rx) =
                        tokio::sync::mpsc::unbounded_channel::<StreamEvent>();

                    let agent = agent_loop.clone();
                    let sid = session_id.clone();
                    tokio::spawn(async move {
                        agent.run(&sid, &mut messages, agent_tx).await;
                    });

                    // Collect agent response for TTS
                    let mut full_response = String::new();
                    let mut pending_tool_params: std::collections::HashMap<
                        String,
                        serde_json::Value,
                    > = std::collections::HashMap::new();
                    let app2 = app_handle.clone();
                    let ms2 = memory_store.clone();
                    let sid2 = session_id.clone();
                    let emb2 = embeddings.clone();
                    let vec2 = vectors.clone();
                    let text2 = text.clone();
                    let vp = voice_pipeline.clone();

                    while let Some(ev) = agent_rx.recv().await {
                        match ev {
                            StreamEvent::Token(t) => {
                                full_response.push_str(&t);
                                let _ = app2.emit("agent:token", serde_json::json!({"text": t}));
                            }
                            StreamEvent::ToolStart { name, params } => {
                                pending_tool_params.insert(name.clone(), params.clone());
                                let _ = app2.emit(
                                    "agent:tool_call",
                                    serde_json::json!({"name": name, "params": params}),
                                );
                            }
                            StreamEvent::ToolEnd {
                                name,
                                result,
                                success,
                            } => {
                                let args = pending_tool_params
                                    .remove(&name)
                                    .unwrap_or_else(|| serde_json::json!({}));
                                let payload =
                                    build_tool_result_event_payload(&name, &result, success);
                                let metadata = payload
                                    .get("metadata")
                                    .cloned()
                                    .unwrap_or(serde_json::Value::Null);
                                let _ = app2.emit("agent:tool_result", payload);

                                let persisted_payload = serde_json::json!({
                                    "name": name,
                                    "args": args,
                                    "success": success,
                                    "result": result,
                                    "metadata": metadata,
                                });

                                let _ = ms2.store_turn(&ConversationTurn {
                                    id: None,
                                    session_id: sid2.clone(),
                                    role: "tool".into(),
                                    content: summarize_tool_turn_for_history(
                                        persisted_payload
                                            .get("name")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("tool"),
                                        success,
                                        persisted_payload
                                            .get("result")
                                            .unwrap_or(&serde_json::Value::Null),
                                        persisted_payload
                                            .get("metadata")
                                            .unwrap_or(&serde_json::Value::Null),
                                    ),
                                    tool_name: persisted_payload
                                        .get("name")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string()),
                                    tool_result: Some(persisted_payload.to_string()),
                                    tokens_used: None,
                                    timestamp: Utc::now(),
                                });

                                // Persist image metadata in chat_media table when generate_image succeeds
                                if name == "generate_image" && success {
                                    if let Some(imgs) = result.get("images").and_then(|v| v.as_array()) {
                                        for img in imgs {
                                            let file_path = img.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                            if file_path.is_empty() { continue; }
                                            let _ = ms2.store_chat_media(&kria_core::memory::store::ChatMediaRecord {
                                                session_id: sid2.clone(),
                                                media_type: "generated".into(),
                                                file_path,
                                                sha256: img.get("sha256").and_then(|v| v.as_str()).map(|s| s.to_string()),
                                                prompt: result.get("prompt").and_then(|v| v.as_str()).map(|s| s.to_string()),
                                                width: img.get("width").and_then(|v| v.as_u64()).map(|v| v as u32),
                                                height: img.get("height").and_then(|v| v.as_u64()).map(|v| v as u32),
                                                style: img.get("style").and_then(|v| v.as_str()).map(|s| s.to_string()),
                                                provenance: img.get("provenance").and_then(|v| v.as_str()).map(|s| s.to_string()),
                                            });
                                        }
                                    }
                                }
                            }
                            StreamEvent::ToolProgress { call_id, message, percent } => {
                                let _ = app2.emit(
                                    "kria:tool-progress",
                                    serde_json::json!({
                                        "call_id": call_id,
                                        "message": message,
                                        "percent": percent,
                                        "session_id": sid2,
                                    }),
                                );
                            }
                            StreamEvent::ToolPayloadChunk { call_id, seq, is_final, data } => {
                                let _ = app2.emit(
                                    "kria:tool-payload-chunk",
                                    serde_json::json!({
                                        "call_id": call_id,
                                        "seq": seq,
                                        "is_final": is_final,
                                        "data": data,
                                        "session_id": sid2,
                                    }),
                                );
                            }
                            StreamEvent::ApprovalRequired {
                                request_id,
                                action,
                                risk_level,
                                parameters,
                            } => {
                                let _ = app2.emit("agent:approval_required", serde_json::json!({"requestId": request_id, "toolName": action, "riskLevel": risk_level, "args": parameters, "reason": ""}));
                            }
                            StreamEvent::ApprovalResult { action, approved } => {
                                let _ = app2.emit(
                                    "agent:approval_result",
                                    serde_json::json!({"action": action, "approved": approved}),
                                );
                            }
                            StreamEvent::ToolChoiceRequired {
                                query,
                                confidence,
                                min_confidence,
                                candidates,
                            } => {
                                let list: Vec<serde_json::Value> = candidates
                                    .into_iter()
                                    .map(|c| {
                                        serde_json::json!({
                                            "name": c.name,
                                            "label": c.label,
                                            "reason": c.reason,
                                            "confidence": c.confidence,
                                        })
                                    })
                                    .collect();
                                let _ = app2.emit(
                                    "agent:tool_choice_required",
                                    serde_json::json!({
                                        "query": query,
                                        "confidence": confidence,
                                        "minConfidence": min_confidence,
                                        "candidates": list,
                                    }),
                                );
                            }
                            StreamEvent::Plan(plan) => {
                                let _ = app2.emit(
                                    "agent:thinking",
                                    serde_json::json!({"status": "planning", "plan": plan}),
                                );
                            }
                            StreamEvent::Error(err) => {
                                let _ = app2.emit(
                                    "agent:token",
                                    serde_json::json!({"text": format!("⚠️ {err}")}),
                                );
                            }
                            StreamEvent::Done(final_text) => {
                                if !final_text.is_empty() && full_response.is_empty() {
                                    full_response = final_text;
                                }
                            }
                        }
                    }

                    // Persist assistant response
                    if !full_response.is_empty() {
                        let _ = ms2.store_turn(&ConversationTurn {
                            id: None,
                            session_id: sid2.clone(),
                            role: "assistant".into(),
                            content: full_response.clone(),
                            tool_name: None,
                            tool_result: None,
                            tokens_used: None,
                            timestamp: Utc::now(),
                        });
                        let fact_mgr =
                            kria_core::memory::facts::FactManager::new(&ms2, &vec2, &emb2);
                        let _ = fact_mgr.extract_from_turn(&text2, &full_response);

                        // Speak the response via TTS
                        if let Err(e) = vp.speak(&full_response).await {
                            tracing::warn!("TTS playback failed: {e}");
                        }
                    }

                    let _ = app2.emit("agent:done", serde_json::json!({}));
                    decrement_active_turn_counter(&active_turns_voice);
                    touch_orchestrator_activity(&last_activity_voice).await;
                }
                VoicePipelineEvent::SpeakingStarted => {
                    let _ =
                        app_handle.emit("voice:state", serde_json::json!({ "state": "speaking" }));
                }
                VoicePipelineEvent::SpeakingDone => {
                    let _ =
                        app_handle.emit("voice:state", serde_json::json!({ "state": "listening" }));
                }
                VoicePipelineEvent::Error(err) => {
                    tracing::warn!("voice pipeline error: {err}");
                    let _ = app_handle.emit("voice:error", serde_json::json!({ "error": err }));
                }
            }
        }
    });

    Ok(())
}

#[tauri::command]
pub async fn stop_voice(state: State<'_, AppStateCell>, app: AppHandle) -> Result<(), String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    state
        .voice_active
        .store(false, std::sync::atomic::Ordering::Relaxed);
    // Abort any in-flight v2 turn immediately so barge-in / stop is instant.
    if let Some(v2) = state.active_voice.read().await.streaming() {
        v2.force_abort().await;
    }
    let voice_pipeline = state.voice_pipeline.read().await.clone();
    voice_pipeline.stop().await;
    let _ = app.emit("voice:state", serde_json::json!({ "state": "idle" }));
    Ok(())
}

#[tauri::command]
pub async fn get_voice_status(state: State<'_, AppStateCell>) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let voice_pipeline = state.voice_pipeline.read().await.clone();
    let pipeline_state = voice_pipeline.state().await;
    Ok(serde_json::json!({
        "active": state.voice_active.load(std::sync::atomic::Ordering::Relaxed),
        "state": pipeline_state,
    }))
}

// ───────────────── voice v2 commands (additive) ──────────────────────────
//
// `voice_v2_speak` runs ONE end-to-end v2 turn from a text prompt:
// LLM token stream → SentenceSplitter → CliPiperTts → PlaybackSink with
// hard barge-in. Used by the UI when `voice.engine = "v2"` is set; the v1
// `start_voice` flow is untouched. `voice_v2_abort` cancels the active
// turn (also exposed for the "KRIA stop now" emergency phrase).

#[tauri::command]
pub async fn voice_v2_speak(
    prompt: String,
    state: State<'_, AppStateCell>,
    app: AppHandle,
) -> Result<(), String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;

    let v2 = {
        let active = state.active_voice.read().await;
        active.streaming().ok_or_else(|| {
            "voice v2 is not active — set `voice.engine = \"v2\"` in config.toml".to_string()
        })?
    };

    // Lazy-wire the AudioPlayer the first time we speak so the playback
    // sink can open a real session via `begin_session`.
    let player = {
        let cfg = state.config.read().await;
        let speaker = cfg.voice.speaker_device.clone();
        let follow = cfg.voice.follow_system_default_speaker;
        Arc::new(
            kria_core::voice::AudioPlayer::new()
                .with_output_device(Some(speaker))
                .follow_system_default(follow),
        )
    };
    v2.set_audio_player(player).await;

    // Drain telemetry into UI events for the duration of this turn.
    let telemetry_rx = state.voice_v2_telemetry.lock().await.take();
    if let Some(mut rx) = telemetry_rx {
        let app_handle = app.clone();
        let slot = state.voice_v2_telemetry.clone();
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                let payload = serde_json::to_value(&ev).unwrap_or_default();
                let _ = app_handle.emit("voice:v2_telemetry", payload);
            }
            // Receiver closed — put None back (channel can't be revived
            // without rebuilding the pipeline).
            *slot.lock().await = None;
        });
    }

    // Build the LLM closure: takes the user prompt, streams tokens off the
    // routed LlmBackend, returns an mpsc::Receiver<String>. The closure
    // owns the stream so cancellation simply drops it.
    let router = state.model_router.clone();
    let llm = move |prompt: String| async move {
        let (tx, rx) = tokio::sync::mpsc::channel::<String>(64);
        let backend = match router.route(&prompt).await {
            Some(b) => b,
            None => {
                let _ = tx
                    .send("(no LLM backend available — check `voice.engine` / model config)".into())
                    .await;
                return rx;
            }
        };
        tokio::spawn(async move {
            let messages = vec![ChatMessage {
                role: "user".into(),
                content: prompt,
                name: None,
                images: None,
            }];
            match backend.chat_stream(&messages, None, 0.7, 512).await {
                Ok(mut stream) => {
                    use futures::StreamExt;
                    while let Some(tok) = stream.next().await {
                        if tx.send(tok).await.is_err() {
                            break;
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(format!("(LLM error: {e})")).await;
                }
            }
        });
        rx
    };

    // Drive the turn. Errors surface back to the UI.
    v2.clone()
        .run_speak_turn(prompt, llm)
        .await
        .map_err(|e| e.to_string())?;
    let _ = app.emit("voice:state", serde_json::json!({ "state": "idle" }));
    Ok(())
}

#[tauri::command]
pub async fn voice_v2_abort(state: State<'_, AppStateCell>) -> Result<(), String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    if let Some(v2) = state.active_voice.read().await.streaming() {
        v2.force_abort().await;
    }
    Ok(())
}

#[tauri::command]
pub async fn send_image_message(
    image_data: Vec<u8>,
    mime_type: String,
    text: Option<String>,
    state: State<'_, AppStateCell>,
    app: AppHandle,
) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    emit_agent_stage(
        &app,
        "input_received",
        "Image prompt received from UI",
        Some(serde_json::json!({
            "mime_type": mime_type.clone(),
            "bytes": image_data.len(),
            "has_text": text.is_some(),
        })),
    );

    // Validate MIME type
    let allowed = [
        "image/png",
        "image/jpeg",
        "image/gif",
        "image/webp",
        "image/bmp",
    ];
    if !allowed.contains(&mime_type.as_str()) {
        return Err(format!("unsupported image type: {}", mime_type));
    }

    // Validate image size (max 10 MB)
    if image_data.len() > 10 * 1024 * 1024 {
        return Err("image too large (max 10 MB)".into());
    }

    touch_orchestrator_activity(&state.orchestrator_last_activity_at).await;
    let orchestrator_img = state.orchestrator.read().await.clone();
    if orchestrator_img.is_some() {
        emit_agent_stage(
            &app,
            "ensuring_local_runtime",
            "Ensuring local LLM runtime is ready for image analysis",
            None,
        );
    }
    if let Err(e) = ensure_orchestrator_ready_for_turn(orchestrator_img.as_ref(), "image_turn").await {
        emit_agent_stage(
            &app,
            "failed",
            "Local runtime preflight failed",
            Some(serde_json::json!({ "error": e.clone() })),
        );
        return Err(e);
    }

    // Store image to ~/.kria/attachments/ with hash-based filename
    let config = state.config.read().await;
    let paths = config.resolve_paths().map_err(|e| e.to_string())?;
    drop(config);
    let attach_dir = paths.data_dir.join("attachments");
    std::fs::create_dir_all(&attach_dir).map_err(|e| e.to_string())?;

    let hash = {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        image_data.hash(&mut h);
        Utc::now().timestamp_nanos_opt().unwrap_or(0).hash(&mut h);
        format!("{:016x}", h.finish())
    };
    let ext = match mime_type.as_str() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        _ => "bin",
    };
    let filename = format!("{}.{}", hash, ext);
    let filepath = attach_dir.join(&filename);
    std::fs::write(&filepath, &image_data).map_err(|e| e.to_string())?;

    tracing::info!(path = %filepath.display(), size = image_data.len(), "image attachment saved");

    emit_agent_stage(
        &app,
        "image_saved",
        "Image attachment saved to local storage",
        Some(serde_json::json!({
            "filename": filename.clone(),
        })),
    );

    let user_text = text.unwrap_or_else(|| "What's in this image?".into());
    let image_intent = infer_image_intent_from_text(&user_text).to_string();
    let _ = app.emit(
        "agent:thinking",
        serde_json::json!({"status": "processing"}),
    );

    let image_path_for_llm = filepath.to_string_lossy().to_string();

    let agent_loop = state.agent_loop.clone();
    let memory_store = state.memory_store.clone();
    let tool_registry = state.tool_registry.clone();
    let event_bus = state.event_bus.clone();
    let config = state.config.read().await;
    let hw_tier = state.hardware_info.tier.as_str();

    emit_agent_stage(
        &app,
        "preparing_tool_context",
        "Collecting tool descriptions for image request",
        Some(serde_json::json!({ "hardware_tier": hw_tier })),
    );

    let tool_defs = tool_registry.list_for_tier(hw_tier);
    let tool_descriptions = build_tool_descriptions_for_prompt(&tool_defs);

    emit_agent_stage(
        &app,
        "tool_context_ready",
        "Tool descriptions prepared",
        Some(serde_json::json!({ "tool_count": tool_defs.len() })),
    );

    let llm_context_window = config.llm.context_window.max(1024);
    let visual_token_cap = image_visual_token_cap_for_context(llm_context_window);
    let response_reserve = if llm_context_window <= 2048 { 480 } else { 640 };
    let system_reserve = if llm_context_window <= 2048 { 320 } else { 480 };
    let history_reserve = if llm_context_window <= 2048 { 320 } else { 700 };
    let ocr_token_cap = if llm_context_window <= 2048 { 256 } else { 320 };

    let (preanalysis_summary, llm_images): (Option<String>, Vec<ImageAttachment>) = if let Some(
        handler,
    ) =
        tool_registry.get_handler("analyze_image")
    {
        emit_agent_stage(
            &app,
            "preanalyzing_image",
            "Running automatic image pre-analysis",
            None,
        );

        let preanalysis_params = serde_json::json!({
            "path": image_path_for_llm.clone(),
            "operations": ["metadata", "ocr", "features", "thumbnail"],
            "intent": image_intent.clone(),
            "context_window": llm_context_window,
            "response_reserve": response_reserve,
            "system_reserve": system_reserve,
            "history_reserve": history_reserve,
            "ocr_token_cap": ocr_token_cap,
            "metadata_token_cap": 72,
            "hard_visual_token_cap": visual_token_cap,
            "max_images_per_turn": IMAGE_SAFE_MAX_ATTACHMENTS_PER_TURN,
        });

        match tokio::time::timeout(
            std::time::Duration::from_secs(IMAGE_PREANALYSIS_TIMEOUT_SECS),
            handler.execute(preanalysis_params),
        )
        .await
        {
            Ok(result) if result.success => {
                let summary = extract_image_preanalysis_summary(&result.data);
                let extracted_images =
                    extract_preprocessed_image_attachments(&result.data, &mime_type)
                    .unwrap_or_default();
                let mut images =
                    constrain_runtime_image_attachments(extracted_images, llm_context_window);
                if images.is_empty() {
                    if let Some(native) =
                        build_native_preprocessed_attachment_with_max(&image_path_for_llm, 640)
                    {
                        images.push(native);
                    }
                }
                let step_status = build_preprocessing_step_status(&result.data, &image_intent);
                emit_agent_stage(
                    &app,
                    "preanalysis_ready",
                    "Image pre-analysis completed",
                    Some(serde_json::json!({
                        "has_summary": summary.is_some(),
                        "llm_image_count": images.len(),
                        "context_window": llm_context_window,
                        "visual_token_cap": visual_token_cap,
                        "step_status": step_status,
                    })),
                );

                if images.is_empty() {
                    emit_agent_stage(
                        &app,
                        "preanalysis_invalid",
                        "Pre-analysis returned no image payload; aborting request",
                        None,
                    );
                    return Err("Image preprocessing produced no usable image payload. Please check sidecar OCR/vision dependencies and try again.".into());
                }

                (summary, images)
            }
            Ok(result) => {
                emit_agent_stage(
                    &app,
                    "preanalysis_failed",
                    "Image pre-analysis failed; aborting before LLM call",
                    Some(serde_json::json!({
                        "error": result.error,
                    })),
                );
                return Err("Image preprocessing failed before LLM dispatch. Please verify sidecar/OCR dependencies and try again.".into());
            }
            Err(_) => {
                emit_agent_stage(
                    &app,
                    "preanalysis_timeout",
                    "Image pre-analysis timed out; aborting before LLM call",
                    Some(serde_json::json!({
                        "timeout_secs": IMAGE_PREANALYSIS_TIMEOUT_SECS,
                    })),
                );
                return Err("Image preprocessing timed out before LLM dispatch. Please retry after the sidecar is healthy.".into());
            }
        }
    } else {
        emit_agent_stage(
            &app,
            "preanalysis_unavailable",
            "Image pre-analysis tool is unavailable; aborting request",
            None,
        );
        return Err(
            "Image preprocessing tool is unavailable. Please restart KRIA and try again.".into(),
        );
    };

    emit_agent_stage(
        &app,
        "image_encoded",
        "Preprocessed image payload encoded for multimodal LLM input",
        Some(serde_json::json!({
            "image_count": llm_images.len(),
        })),
    );

    let user_name = memory_store
        .get_preference("user_name")
        .unwrap_or(None)
        .unwrap_or_else(|| "User".to_string());
    let os_name = std::env::consts::OS;

    // Detect package managers for image message context
    let pm_string_img = {
        let pms = get_available_package_managers();
        match pms.as_slice() {
            [] => "unknown".to_string(),
            [only] => only.as_str().to_string(),
            [primary, rest @ ..] => {
                let alts: Vec<&str> = rest.iter().map(|p| p.as_str()).collect();
                format!("{} (also available: {})", primary.as_str(), alts.join(", "))
            }
        }
    };

    let memory_context = match memory_store.search_facts(&user_text, 5) {
        Ok(facts) if !facts.is_empty() => {
            let fact_lines: Vec<String> = facts.iter().map(|f| format!("- {}", f.text)).collect();
            format!("Known facts about the user:\n{}", fact_lines.join("\n"))
        }
        _ => String::new(),
    };

    emit_agent_stage(
        &app,
        "memory_context_ready",
        "Memory context prepared for image prompt",
        Some(serde_json::json!({
            "has_context": !memory_context.is_empty(),
        })),
    );

    let system_prompt = kria_core::agent::prompts::build_system_prompt(
        &tool_descriptions,
        &user_name,
        os_name,
        hw_tier,
        &pm_string_img,
        &memory_context,
    );

    emit_agent_stage(
        &app,
        "system_prompt_ready",
        "System prompt prepared for image request",
        Some(serde_json::json!({
            "prompt_chars": system_prompt.chars().count(),
        })),
    );
    drop(config);

    let session_id = state.current_session_id.read().await.clone();

    emit_agent_stage(
        &app,
        "building_message_history",
        "Building multimodal conversation history",
        Some(serde_json::json!({
            "session_id": session_id.clone(),
        })),
    );

    let recent_turns = memory_store
        .get_recent_turns(&session_id, 20)
        .unwrap_or_default();

    let mut messages = Vec::with_capacity(recent_turns.len() + 2);
    messages.push(ChatMessage {
        role: "system".into(),
        content: system_prompt,
        name: None,
        images: None,
    });

    if let Some(summary) = preanalysis_summary
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        messages.push(ChatMessage {
            role: "system".into(),
            content: format!(
                "Automatic pre-analysis context (already validated):\n{}",
                summary
            ),
            name: None,
            images: None,
        });
    }

    for turn in &recent_turns {
        messages.push(ChatMessage {
            role: turn.role.clone(),
            content: turn.content.clone(),
            name: turn.tool_name.clone(),
            images: None,
        });
    }
    messages.push(ChatMessage {
        role: "user".into(),
        content: build_image_llm_user_content(
            &user_text,
            &image_path_for_llm,
            &image_intent,
            None,
        ),
        name: None,
        images: Some(llm_images),
    });

    // Persist user turn (content only, images stored in attachments/)
    let _ = memory_store.store_turn(&ConversationTurn {
        id: None,
        session_id: session_id.clone(),
        role: "user".into(),
        content: format!("{}\n[image: {}]", user_text, filename),
        tool_name: None,
        tool_result: None,
        tokens_used: None,
        timestamp: Utc::now(),
    });

    emit_agent_stage(
        &app,
        "user_turn_saved",
        "Image prompt stored in session memory",
        Some(serde_json::json!({
            "history_turns": recent_turns.len() + 1,
        })),
    );

    // Auto-title
    {
        let title_key = format!("session_title:{}", session_id);
        if memory_store
            .get_preference(&title_key)
            .unwrap_or(None)
            .is_none()
        {
            let title = if user_text.len() > 50 {
                format!("{}...", &user_text[..50])
            } else {
                user_text.clone()
            };
            let _ = memory_store.set_preference(&title_key, &format!("📷 {}", title));
        }
    }

    event_bus.publish(kria_core::infra::event_bus::KriaEvent::MessageReceived {
        session_id: session_id.clone(),
        content: user_text.clone(),
    });

    emit_agent_stage(
        &app,
        "dispatching_to_llm",
        "Dispatching multimodal prompt to agent loop",
        None,
    );

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<StreamEvent>();
    state
        .orchestrator_active_turns
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let active_turns_for_tracking = state.orchestrator_active_turns.clone();
    let last_activity_for_tracking = state.orchestrator_last_activity_at.clone();
    let app_handle = app.clone();
    let session_id_clone = session_id.clone();
    let memory_store_clone = memory_store.clone();
    let embeddings_clone = state.embeddings.clone();
    let vectors_clone = state.vectors.clone();
    let user_message_clone = user_text.clone();
    let preanalysis_summary_fallback = preanalysis_summary.clone();

    let agent = agent_loop.clone();
    let sid = session_id.clone();
    tauri::async_runtime::spawn(async move {
        agent.run(&sid, &mut messages, event_tx).await;
    });

    emit_agent_stage(
        &app,
        "agent_loop_started",
        "Agent loop started for image request",
        None,
    );

    // Event consumer (same as send_message)
    tauri::async_runtime::spawn(async move {
        let mut full_response = String::new();
        let mut saw_first_token = false;
        let mut pending_tool_params: std::collections::HashMap<String, serde_json::Value> =
            std::collections::HashMap::new();

        emit_agent_stage(
            &app_handle,
            "awaiting_llm_output",
            "Image prompt sent to LLM; waiting for first response token",
            None,
        );

        loop {
            let event = match tokio::time::timeout(
                std::time::Duration::from_secs(AGENT_EVENT_IDLE_TIMEOUT_SECS),
                event_rx.recv(),
            )
            .await
            {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(_) => {
                    emit_agent_stage(
                        &app_handle,
                        "timed_out_waiting_for_llm",
                        "No agent events received within timeout window",
                        Some(serde_json::json!({
                            "timeout_secs": AGENT_EVENT_IDLE_TIMEOUT_SECS,
                        })),
                    );
                    full_response = AGENT_TIMEOUT_MESSAGE.to_string();
                    let _ = app_handle.emit(
                        "agent:token",
                        serde_json::json!({
                            "text": AGENT_TIMEOUT_MESSAGE,
                        }),
                    );
                    break;
                }
            };

            match event {
                StreamEvent::Token(text) => {
                    if !saw_first_token {
                        saw_first_token = true;
                        emit_agent_stage(
                            &app_handle,
                            "llm_streaming",
                            "LLM started streaming tokens",
                            None,
                        );
                    }
                    full_response.push_str(&text);
                    let _ = app_handle.emit("agent:token", serde_json::json!({ "text": text }));
                }
                StreamEvent::ToolStart { name, params } => {
                    pending_tool_params.insert(name.clone(), params.clone());
                    emit_agent_stage(
                        &app_handle,
                        "tool_started",
                        "Tool execution started",
                        Some(serde_json::json!({ "tool": name.clone() })),
                    );
                    let _ = app_handle.emit(
                        "agent:tool_call",
                        serde_json::json!({ "name": name, "params": params }),
                    );
                }
                StreamEvent::ToolEnd {
                    name,
                    result,
                    success,
                } => {
                    emit_agent_stage(
                        &app_handle,
                        "tool_finished",
                        "Tool execution completed",
                        Some(serde_json::json!({
                            "tool": name.clone(),
                            "success": success,
                        })),
                    );
                    let args = pending_tool_params
                        .remove(&name)
                        .unwrap_or_else(|| serde_json::json!({}));
                    let payload = build_tool_result_event_payload(&name, &result, success);
                    let metadata = payload
                        .get("metadata")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    let _ = app_handle.emit("agent:tool_result", payload);

                    let persisted_payload = serde_json::json!({
                        "name": name,
                        "args": args,
                        "success": success,
                        "result": result,
                        "metadata": metadata,
                    });
                    let tool_name = persisted_payload
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("tool");
                    let _ = memory_store_clone.store_turn(&ConversationTurn {
                        id: None,
                        session_id: session_id_clone.clone(),
                        role: "tool".into(),
                        content: summarize_tool_turn_for_history(
                            tool_name,
                            success,
                            persisted_payload
                                .get("result")
                                .unwrap_or(&serde_json::Value::Null),
                            persisted_payload
                                .get("metadata")
                                .unwrap_or(&serde_json::Value::Null),
                        ),
                        tool_name: Some(tool_name.to_string()),
                        tool_result: Some(persisted_payload.to_string()),
                        tokens_used: None,
                        timestamp: Utc::now(),
                    });
                }
                StreamEvent::ToolProgress { call_id, message, percent } => {
                    let _ = app_handle.emit(
                        "kria:tool-progress",
                        serde_json::json!({
                            "call_id": call_id,
                            "message": message,
                            "percent": percent,
                        }),
                    );
                }
                StreamEvent::ToolPayloadChunk { call_id, seq, is_final, data } => {
                    let _ = app_handle.emit(
                        "kria:tool-payload-chunk",
                        serde_json::json!({
                            "call_id": call_id,
                            "seq": seq,
                            "is_final": is_final,
                            "data": data,
                        }),
                    );
                }
                StreamEvent::ApprovalRequired {
                    request_id,
                    action,
                    risk_level,
                    parameters,
                } => {
                    emit_agent_stage(
                        &app_handle,
                        "approval_required",
                        "Agent requested user approval",
                        Some(serde_json::json!({
                            "action": action.clone(),
                            "risk_level": risk_level.clone(),
                        })),
                    );
                    let _ = app_handle.emit("agent:approval_required", serde_json::json!({ "requestId": request_id, "toolName": action, "riskLevel": risk_level, "args": parameters, "reason": "" }));
                }
                StreamEvent::ApprovalResult { action, approved } => {
                    emit_agent_stage(
                        &app_handle,
                        "approval_result",
                        "User approval decision received",
                        Some(serde_json::json!({
                            "action": action.clone(),
                            "approved": approved,
                        })),
                    );
                    let _ = app_handle.emit(
                        "agent:approval_result",
                        serde_json::json!({ "action": action, "approved": approved }),
                    );
                }
                StreamEvent::ToolChoiceRequired {
                    query,
                    confidence,
                    min_confidence,
                    candidates,
                } => {
                    emit_agent_stage(
                        &app_handle,
                        "tool_choice_required",
                        "Low-confidence routing requires user tool selection",
                        Some(serde_json::json!({
                            "confidence": confidence,
                            "min_confidence": min_confidence,
                            "candidate_count": candidates.len(),
                        })),
                    );
                    let list: Vec<serde_json::Value> = candidates
                        .into_iter()
                        .map(|c| {
                            serde_json::json!({
                                "name": c.name,
                                "label": c.label,
                                "reason": c.reason,
                                "confidence": c.confidence,
                            })
                        })
                        .collect();
                    let _ = app_handle.emit(
                        "agent:tool_choice_required",
                        serde_json::json!({
                            "query": query,
                            "confidence": confidence,
                            "minConfidence": min_confidence,
                            "candidates": list,
                        }),
                    );
                }
                StreamEvent::Plan(plan) => {
                    emit_agent_stage(
                        &app_handle,
                        "planning",
                        "Agent is updating execution plan",
                        Some(serde_json::json!({ "plan": plan.clone() })),
                    );
                    let _ = app_handle.emit(
                        "agent:thinking",
                        serde_json::json!({ "status": "planning", "plan": plan }),
                    );
                }
                StreamEvent::Error(err) => {
                    let lower_err = err.to_ascii_lowercase();
                    let is_transport_failure = lower_err.contains("error sending request for url")
                        || lower_err.contains("connection refused")
                        || lower_err.contains("tcp connect")
                        || lower_err.contains("dns error")
                        || lower_err.contains("timed out");

                    if (lower_err.contains("circuit open")
                        || lower_err.contains("local llm unavailable")
                        || is_transport_failure)
                        && full_response.is_empty()
                    {
                        if let Some(summary) = preanalysis_summary_fallback.as_ref() {
                            let fallback_text = format!(
                                "⚠️ Local vision model is temporarily unavailable. Here is the image pre-analysis:\n\n{}",
                                summary
                            );
                            full_response = fallback_text.clone();
                            emit_agent_stage(
                                &app_handle,
                                "llm_unavailable_preanalysis_fallback",
                                "LLM unavailable; returning pre-analysis summary fallback",
                                None,
                            );
                            let _ = app_handle.emit(
                                "agent:token",
                                serde_json::json!({ "text": fallback_text }),
                            );
                            continue;
                        }
                    }

                    let user_visible_error = format!("⚠️ {err}");
                    if full_response.is_empty() {
                        full_response = user_visible_error.clone();
                    }
                    emit_agent_stage(
                        &app_handle,
                        "failed",
                        "Agent stream reported an error",
                        Some(serde_json::json!({ "error": err.clone() })),
                    );
                    let _ = app_handle.emit(
                        "agent:token",
                        serde_json::json!({ "text": user_visible_error }),
                    );
                }
                StreamEvent::Done(final_text) => {
                    if !final_text.is_empty() && full_response.is_empty() {
                        full_response = final_text;
                    }
                    emit_agent_stage(
                        &app_handle,
                        "llm_done",
                        "LLM stream completed",
                        Some(serde_json::json!({
                            "response_chars": full_response.chars().count(),
                        })),
                    );
                }
            }
        }

        if !full_response.is_empty() {
            let _ = memory_store_clone.store_turn(&ConversationTurn {
                id: None,
                session_id: session_id_clone,
                role: "assistant".into(),
                content: full_response.clone(),
                tool_name: None,
                tool_result: None,
                tokens_used: None,
                timestamp: Utc::now(),
            });

            emit_agent_stage(
                &app_handle,
                "assistant_turn_saved",
                "Assistant response stored in session memory",
                Some(serde_json::json!({
                    "response_chars": full_response.chars().count(),
                })),
            );

            let fact_mgr = kria_core::memory::facts::FactManager::new(
                &memory_store_clone,
                &vectors_clone,
                &embeddings_clone,
            );
            match fact_mgr.extract_from_turn(&user_message_clone, &full_response) {
                Ok(ids) if !ids.is_empty() => {
                    tracing::info!(
                        count = ids.len(),
                        "auto-extracted facts from image conversation"
                    );
                    emit_agent_stage(
                        &app_handle,
                        "facts_extracted",
                        "New user facts extracted from the image conversation",
                        Some(serde_json::json!({
                            "fact_count": ids.len(),
                        })),
                    );
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("fact extraction failed: {}", e),
            }
        }

        emit_agent_stage(
            &app_handle,
            "completed",
            "Pipeline completed and UI will finalize rendering",
            None,
        );

        let _ = app_handle.emit("agent:done", serde_json::json!({}));
        decrement_active_turn_counter(&active_turns_for_tracking);
        touch_orchestrator_activity(&last_activity_for_tracking).await;
    });

    Ok(serde_json::json!({
        "status": "processing",
        "attachment": filename,
    }))
}

// ── MCP Runtime Helpers ──────────────────────────────────────────────

fn mcp_state_name(state: McpServerState) -> &'static str {
    match state {
        McpServerState::Stopped => "stopped",
        McpServerState::Starting => "starting",
        McpServerState::Running => "running",
        McpServerState::Error => "error",
    }
}

fn mcp_status_to_json(status: &McpServerStatus) -> serde_json::Value {
    serde_json::json!({
        "name": status.name.clone(),
        "command": status.command.clone(),
        "enabled": status.enabled,
        "state": mcp_state_name(status.state),
        "tool_count": status.tool_count,
        "error": status.error.clone(),
    })
}

async fn sync_google_workspace_client_ref(
    state: &AppState,
    gw_client: Option<Arc<kria_core::mcp::McpClient>>,
) {
    if let Some(client) = gw_client {
        gw::set_client(&state.gw_client_ref, client).await;
    } else {
        *state.gw_client_ref.write().await = None;
    }
}

async fn sync_colab_runtime_snapshot(state: &AppState, statuses: &[McpServerStatus]) {
    let colab_cfg = { state.config.read().await.colab.clone() };
    let mut runtime = state.colab_runtime.write().await;
    runtime.sidecar_server_name = colab_cfg.mcp_server_name.clone();

    if !colab_cfg.enabled {
        runtime.state = ColabRuntimeState::Disconnected;
        runtime.selected_notebook = None;
        runtime.last_error = None;
        return;
    }

    match statuses
        .iter()
        .find(|s| s.name == runtime.sidecar_server_name)
    {
        Some(status) if status.state == McpServerState::Running => {
            let category = format!("mcp_{}", runtime.sidecar_server_name);
            let category_tools = state.tool_registry.list_by_category(&category);
            let bootstrap_only = status.tool_count == 1
                && category_tools.len() == 1
                && category_tools
                    .first()
                    .map(|tool| is_colab_bootstrap_tool_name(&tool.name))
                    .unwrap_or(false);

            let has_notebook = runtime
                .selected_notebook
                .as_ref()
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false);

            let next_state = if status.tool_count == 0 {
                runtime.selected_notebook = None;
                ColabRuntimeState::AwaitingBrowserConnection
            } else if bootstrap_only {
                ColabRuntimeState::AwaitingBrowserConnection
            } else if has_notebook {
                ColabRuntimeState::Ready
            } else {
                ColabRuntimeState::NotebookSelectionRequired
            };

            runtime.state = next_state;
            if matches!(
                next_state,
                ColabRuntimeState::Ready | ColabRuntimeState::NotebookSelectionRequired
            ) {
                runtime.last_error = None;
            }
        }
        Some(status) => {
            runtime.state = ColabRuntimeState::Degraded;
            runtime.last_error = status.error.clone().or_else(|| {
                Some(format!(
                    "MCP server '{}' is {}",
                    runtime.sidecar_server_name,
                    mcp_state_name(status.state)
                ))
            });
        }
        None => {
            runtime.state = ColabRuntimeState::Degraded;
            runtime.last_error = Some(format!(
                "MCP server '{}' not found in runtime status",
                runtime.sidecar_server_name
            ));
        }
    }
}

async fn update_mcp_health_status(state: &AppState, statuses: &[McpServerStatus]) {
    let total = statuses.len();
    let running = statuses
        .iter()
        .filter(|s| s.state == McpServerState::Running)
        .count();
    let total_tools: usize = statuses.iter().map(|s| s.tool_count).sum();

    let unhealthy_enabled: Vec<&str> = statuses
        .iter()
        .filter(|s| s.enabled && s.state != McpServerState::Running)
        .map(|s| s.name.as_str())
        .collect();

    let (service, detail) = if total == 0 {
        (
            ServiceStatus::Healthy,
            "no MCP servers configured".to_string(),
        )
    } else if unhealthy_enabled.is_empty() {
        (
            ServiceStatus::Healthy,
            format!("{running}/{total} servers running, {total_tools} tools"),
        )
    } else {
        (
            ServiceStatus::Degraded,
            format!(
                "{running}/{total} servers running, {total_tools} tools; degraded: {}",
                unhealthy_enabled.join(", ")
            ),
        )
    };

    state.health.update("mcp_servers", service, Some(detail));
}

async fn apply_mcp_runtime_from_config(state: &AppState) -> serde_json::Value {
    let desired = { state.config.read().await.mcp.servers.clone() };

    let mut manager = state.mcp_manager.lock().await;
    let report = manager.reconcile(desired, &state.tool_registry).await;
    let statuses = manager.status().await;
    let gw_client = manager.get_client("gworkspace").cloned();
    drop(manager);

    sync_google_workspace_client_ref(state, gw_client).await;
    sync_colab_runtime_snapshot(state, &statuses).await;
    update_mcp_health_status(state, &statuses).await;

    let status_json: Vec<serde_json::Value> = statuses.iter().map(mcp_status_to_json).collect();
    serde_json::json!({
        "report": report,
        "servers": status_json,
    })
}

// ── MCP Server Management Commands ──────────────────────────────────

#[tauri::command]
pub async fn list_mcp_servers(state: State<'_, AppStateCell>) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let configured_servers = { state.config.read().await.mcp.servers.clone() };
    let runtime_statuses = {
        let manager = state.mcp_manager.lock().await;
        manager.status().await
    };

    let runtime_by_name: std::collections::HashMap<String, McpServerStatus> = runtime_statuses
        .into_iter()
        .map(|s| (s.name.clone(), s))
        .collect();

    let servers: Vec<serde_json::Value> = configured_servers
        .iter()
        .map(|s| {
            let runtime = runtime_by_name.get(&s.name);
            serde_json::json!({
                "name": s.name.clone(),
                "command": s.command.clone(),
                "args": s.args.clone(),
                "enabled": s.enabled,
                "trust_level": s.trust_level.clone(),
                "runtime_state": runtime.map(|r| mcp_state_name(r.state)).unwrap_or("stopped"),
                "runtime_tool_count": runtime.map(|r| r.tool_count).unwrap_or(0),
                "runtime_error": runtime.and_then(|r| r.error.clone()),
            })
        })
        .collect();
    Ok(serde_json::json!(servers))
}

#[tauri::command]
pub async fn reconcile_mcp_runtime(
    app: AppHandle,
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let report = apply_mcp_runtime_from_config(state).await;
    emit_colab_status_event(&app, state).await;
    Ok(report)
}

#[tauri::command]
pub async fn restart_mcp_server_runtime(
    name: String,
    app: AppHandle,
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;

    let mut manager = state.mcp_manager.lock().await;
    manager
        .restart_server(&name, &state.tool_registry)
        .await
        .map_err(|e| e.to_string())?;
    let statuses = manager.status().await;
    let gw_client = manager.get_client("gworkspace").cloned();
    drop(manager);

    sync_google_workspace_client_ref(state, gw_client).await;
    sync_colab_runtime_snapshot(state, &statuses).await;
    update_mcp_health_status(state, &statuses).await;
    emit_colab_status_event(&app, state).await;

    let servers: Vec<serde_json::Value> = statuses.iter().map(mcp_status_to_json).collect();
    Ok(serde_json::json!({
        "status": "restarted",
        "name": name,
        "servers": servers,
    }))
}

#[tauri::command]
pub async fn add_mcp_server(
    name: String,
    command: String,
    args: Vec<String>,
    trust_level: Option<String>,
    state: State<'_, AppStateCell>,
) -> Result<(), String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    use kria_core::config::McpServerConfig;

    let server = McpServerConfig {
        name: name.clone(),
        command,
        args,
        env: std::collections::HashMap::new(),
        enabled: true,
        trust_level: trust_level.unwrap_or_else(|| "YELLOW".into()),
        tool_overrides: std::collections::HashMap::new(),
    };

    let mut config = state.config.write().await;
    // Prevent duplicate names
    if config.mcp.servers.iter().any(|s| s.name == name) {
        return Err(format!("MCP server '{}' already configured", name));
    }
    config.mcp.servers.push(server);
    config.save().map_err(|e| e.to_string())?;

    drop(config);
    let _ = apply_mcp_runtime_from_config(state).await;

    Ok(())
}

#[tauri::command]
pub async fn remove_mcp_server(name: String, state: State<'_, AppStateCell>) -> Result<(), String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let mut config = state.config.write().await;
    let before = config.mcp.servers.len();
    config.mcp.servers.retain(|s| s.name != name);
    if config.mcp.servers.len() == before {
        return Err(format!("MCP server '{}' not found", name));
    }
    config.save().map_err(|e| e.to_string())?;

    drop(config);
    let _ = apply_mcp_runtime_from_config(state).await;

    Ok(())
}

#[tauri::command]
pub async fn toggle_mcp_server(
    name: String,
    enabled: bool,
    state: State<'_, AppStateCell>,
) -> Result<(), String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let mut config = state.config.write().await;
    if let Some(server) = config.mcp.servers.iter_mut().find(|s| s.name == name) {
        server.enabled = enabled;
        if name.eq_ignore_ascii_case("telegram") {
            config.telegram.enabled = enabled;
            sync_telegram_mcp_server_config(&mut config);
        }
        config.save().map_err(|e| e.to_string())?;

        drop(config);
        let _ = apply_mcp_runtime_from_config(state).await;

        Ok(())
    } else {
        Err(format!("MCP server '{}' not found", name))
    }
}

// ── Telegram Integration Commands ───────────────────────────────────

#[tauri::command]
pub async fn get_telegram_config(
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let config = state.config.read().await;
    Ok(serde_json::json!({
        "enabled": config.telegram.enabled,
        "bot_token": config.telegram.bot_token,
        "allowed_chat_ids": config.telegram.allowed_chat_ids,
        "auto_start": config.telegram.auto_start,
    }))
}

#[tauri::command]
pub async fn update_telegram_config(
    enabled: bool,
    bot_token: String,
    allowed_chat_ids: String,
    auto_start: bool,
    state: State<'_, AppStateCell>,
) -> Result<(), String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let mut config = state.config.write().await;
    config.telegram.enabled = enabled;
    config.telegram.bot_token = bot_token;
    config.telegram.allowed_chat_ids = allowed_chat_ids;
    config.telegram.auto_start = auto_start;
    sync_telegram_mcp_server_config(&mut config);
    config.save().map_err(|e| e.to_string())?;
    drop(config);

    let _ = apply_mcp_runtime_from_config(state).await;
    Ok(())
}

#[tauri::command]
pub async fn start_telegram_mcp(
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let mut config = state.config.write().await;
    config.telegram.enabled = true;
    sync_telegram_mcp_server_config(&mut config);
    let tg_config = config.telegram.clone();
    let telegram_mcp_configured = config
        .mcp
        .servers
        .iter()
        .any(|s| s.name.eq_ignore_ascii_case("telegram"));
    config.save().map_err(|e| e.to_string())?;
    drop(config);

    if tg_config.bot_token.is_empty() {
        return Err("Telegram bot token is not configured".into());
    }

    if telegram_mcp_configured {
        let runtime = apply_mcp_runtime_from_config(state).await;
        let telegram_status = runtime["servers"]
            .as_array()
            .and_then(|servers| {
                servers.iter().find(|server| {
                    server["name"]
                        .as_str()
                        .map(|name| name.eq_ignore_ascii_case("telegram"))
                        .unwrap_or(false)
                })
            })
            .cloned()
            .unwrap_or_default();

        if telegram_status["state"] == "running" {
            return Ok(serde_json::json!({
                "status": "running",
                "message": "Telegram MCP server is running and can now forward messages into KRIA.",
                "runtime": runtime,
            }));
        }

        return Err(format!(
            "Telegram MCP server failed to start: {}",
            telegram_status["error"].as_str().unwrap_or("unknown error")
        ));
    }

    // Stop existing bridge if running
    {
        let mut guard = state.telegram_bridge.write().await;
        if let Some(bridge) = guard.take() {
            bridge.stop();
        }
    }

    let hw_tier = state.hardware_info.tier.as_str().to_string();
    let bridge = TelegramBridge::spawn(
        tg_config,
        state.agent_loop.clone(),
        state.memory_store.clone(),
        state.tool_registry.clone(),
        state.embeddings.clone(),
        state.vectors.clone(),
        hw_tier,
        state.orchestrator.clone(),
    );

    *state.telegram_bridge.write().await = Some(bridge);

    Ok(serde_json::json!({
        "status": "running",
        "message": "Telegram bridge started. Bot is now polling for messages.",
    }))
}

#[tauri::command]
pub async fn stop_telegram_mcp(state: State<'_, AppStateCell>) -> Result<(), String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    // Stop the bridge
    {
        let mut guard = state.telegram_bridge.write().await;
        if let Some(bridge) = guard.take() {
            bridge.stop();
            tracing::info!("Telegram bridge stopped");
        }
    }

    // Update config
    let mut config = state.config.write().await;
    config.telegram.enabled = false;
    sync_telegram_mcp_server_config(&mut config);
    config.save().map_err(|e| e.to_string())?;
    drop(config);

    let _ = apply_mcp_runtime_from_config(state).await;
    Ok(())
}

#[tauri::command]
pub async fn test_telegram_connection(bot_token: String) -> Result<serde_json::Value, String> {
    // Test the bot token by calling getMe
    let url = format!("https://api.telegram.org/bot{}/getMe", bot_token);
    let client = reqwest::Client::new();
    let resp: reqwest::Response = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("Connection failed: {e}"))?;

    let body: serde_json::Value = resp
        .json::<serde_json::Value>()
        .await
        .map_err(|e| format!("Invalid response: {e}"))?;

    if body["ok"].as_bool() == Some(true) {
        let result = &body["result"];
        Ok(serde_json::json!({
            "valid": true,
            "bot_name": result["first_name"],
            "bot_username": result["username"],
            "bot_id": result["id"],
        }))
    } else {
        let desc = body
            .get("description")
            .and_then(|d: &serde_json::Value| d.as_str())
            .unwrap_or("unknown error");
        Err(format!("Invalid token: {}", desc))
    }
}

// ── Automation Commands ─────────────────────────────────────────────

#[tauri::command]
pub async fn list_scheduled_tasks(
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let scheduler = state.scheduler.read().await;
    let tasks: Vec<serde_json::Value> = scheduler
        .list_tasks()
        .iter()
        .map(|t| {
            serde_json::json!({
                "id": t.id,
                "name": t.name,
                "interval_secs": t.interval_secs,
                "prompt": t.prompt,
                "enabled": t.enabled,
            })
        })
        .collect();
    Ok(serde_json::json!(tasks))
}

#[tauri::command]
pub async fn add_scheduled_task(
    name: String,
    interval_secs: u64,
    prompt: String,
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    use kria_core::automation::scheduler::ScheduledTask;

    let id = uuid::Uuid::new_v4().to_string();
    let task = ScheduledTask {
        id: id.clone(),
        name,
        interval_secs,
        prompt,
        enabled: true,
    };

    let mut scheduler = state.scheduler.write().await;
    scheduler.add_task(task);
    Ok(serde_json::json!({"id": id}))
}

#[tauri::command]
pub async fn remove_scheduled_task(
    task_id: String,
    state: State<'_, AppStateCell>,
) -> Result<(), String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let mut scheduler = state.scheduler.write().await;
    scheduler.remove_task(&task_id);
    Ok(())
}

#[tauri::command]
pub async fn list_macros(state: State<'_, AppStateCell>) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let recorder = state.macro_recorder.read().await;
    let macros: Vec<serde_json::Value> = recorder
        .list()
        .iter()
        .map(|m| {
            serde_json::json!({
                "name": m.name,
                "description": m.description,
                "step_count": m.steps.len(),
                "created_at": m.created_at,
            })
        })
        .collect();
    Ok(serde_json::json!(macros))
}

#[tauri::command]
pub async fn start_macro_recording(
    name: String,
    state: State<'_, AppStateCell>,
) -> Result<(), String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let mut recorder = state.macro_recorder.write().await;
    recorder.start_recording(&name);
    Ok(())
}

#[tauri::command]
pub async fn stop_macro_recording(
    description: String,
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let mut recorder = state.macro_recorder.write().await;
    match recorder.stop_recording(&description) {
        Some(m) => Ok(serde_json::json!({
            "name": m.name,
            "steps": m.steps.len(),
        })),
        None => Err("No recording in progress".into()),
    }
}

#[tauri::command]
pub async fn delete_macro(name: String, state: State<'_, AppStateCell>) -> Result<(), String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let mut recorder = state.macro_recorder.write().await;
    if recorder.delete(&name) {
        Ok(())
    } else {
        Err(format!("Macro '{}' not found", name))
    }
}

#[tauri::command]
pub async fn list_workflows(state: State<'_, AppStateCell>) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let engine = state.workflow_engine.read().await;
    let workflows: Vec<serde_json::Value> = engine
        .list()
        .iter()
        .map(|w| {
            serde_json::json!({
                "id": w.id,
                "name": w.name,
                "description": w.description,
                "step_count": w.steps.len(),
                "created_at": w.created_at,
            })
        })
        .collect();
    Ok(serde_json::json!(workflows))
}

#[tauri::command]
pub async fn delete_workflow(
    workflow_id: String,
    state: State<'_, AppStateCell>,
) -> Result<(), String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let mut engine = state.workflow_engine.write().await;
    if engine.delete(&workflow_id) {
        Ok(())
    } else {
        Err(format!("Workflow '{}' not found", workflow_id))
    }
}

// ── Colab Cloud Tier Commands ───────────────────────────────────────────────

fn migrate_legacy_colab_server_command(server: &mut kria_core::config::McpServerConfig) -> bool {
    if server.command != COLAB_LEGACY_NPX_COMMAND {
        return false;
    }

    if !server
        .args
        .iter()
        .any(|arg| arg == COLAB_LEGACY_NPX_PACKAGE)
    {
        return false;
    }

    server.command = COLAB_OFFICIAL_COMMAND.to_string();
    server.args = vec![COLAB_OFFICIAL_SOURCE.to_string()];
    true
}

fn default_colab_server_config() -> kria_core::config::McpServerConfig {
    kria_core::config::McpServerConfig {
        name: COLAB_DEFAULT_SERVER_NAME.to_string(),
        command: COLAB_OFFICIAL_COMMAND.to_string(),
        args: vec![COLAB_OFFICIAL_SOURCE.to_string()],
        env: std::collections::HashMap::new(),
        enabled: true,
        trust_level: "YELLOW".into(),
        tool_overrides: std::collections::HashMap::new(),
    }
}

fn build_colab_tier_status_payload(
    config: &ColabConfig,
    runtime: &ColabRuntimeSnapshot,
    mcp_runtime: Option<&McpServerStatus>,
    capability_summary: &serde_json::Value,
    additional_warnings: &[String],
) -> serde_json::Value {
    let (mcp_state, mcp_tool_count, mcp_error, mcp_running) = match mcp_runtime {
        Some(status) => (
            mcp_state_name(status.state).to_string(),
            status.tool_count,
            status.error.clone(),
            status.state == McpServerState::Running,
        ),
        None => ("not_configured".to_string(), 0usize, None, false),
    };

    let browser_connected = matches!(
        runtime.state,
        ColabRuntimeState::NotebookSelectionRequired | ColabRuntimeState::Ready
    );
    let connected = config.enabled && mcp_running && browser_connected;

    let selected_notebook = runtime
        .selected_notebook
        .as_ref()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);

    let mut capability_missing: Vec<String> = capability_summary
        .get("ready_requirements")
        .and_then(|v| v.get("missing"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();

    if selected_notebook {
        capability_missing.retain(|item| item != "notebook_selection_or_discovery");
    }

    let capability_ready = capability_missing.is_empty();
    let ready_for_cloud_task =
        connected && runtime.state == ColabRuntimeState::Ready && capability_ready;
    let notebook_selection_required =
        connected && runtime.state == ColabRuntimeState::NotebookSelectionRequired;

    let discovered_tool_count = capability_summary
        .get("tool_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;

    let bootstrap_only = capability_summary
        .get("discovered_tools")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.len() == 1
                && arr.iter().any(|entry| {
                    entry
                        .get("operation")
                        .and_then(|v| v.as_str())
                        .map(is_colab_bootstrap_tool_name)
                        .unwrap_or(false)
                        || entry
                            .get("name")
                            .and_then(|v| v.as_str())
                            .map(is_colab_bootstrap_tool_name)
                            .unwrap_or(false)
                })
        })
        .unwrap_or(false);

    let mut warnings: Vec<String> = Vec::new();
    if !config.enabled {
        warnings.push("Colab tier is disabled in config".into());
    }
    if !mcp_running {
        warnings.push(format!(
            "Colab MCP runtime is not running (state={})",
            mcp_state
        ));
    }
    if runtime.state == ColabRuntimeState::AwaitingBrowserConnection {
        warnings.push("Awaiting browser connection to Colab session".into());
    }
    if mcp_running && bootstrap_only {
        warnings.push(
            "Colab MCP is exposing only bootstrap tooling. Use Connect Colab to open browser session and unlock notebook tools".into(),
        );
    }
    if mcp_running && discovered_tool_count == 0 {
        warnings.push(format!(
            "Colab MCP server '{}' is running but no tools were discovered",
            runtime.sidecar_server_name
        ));
    }
    if connected && !capability_ready {
        if !capability_missing.is_empty() {
            warnings.push(format!(
                "Colab capability requirements are not satisfied: {}",
                capability_missing.join(", ")
            ));
        }
    }
    if notebook_selection_required {
        warnings.push("Notebook must be selected before executing cloud tasks".into());
    }
    if let Some(err) = runtime.last_error.as_ref() {
        warnings.push(format!("Last runtime error: {err}"));
    }
    warnings.extend(additional_warnings.iter().cloned());

    serde_json::json!({
        "enabled": config.enabled,
        "connected": connected,
        "ready_for_cloud_task": ready_for_cloud_task,
        "notebook_selection_required": notebook_selection_required,
        "runtime_state": runtime.state.as_str(),
        "selected_notebook": runtime.selected_notebook,
        "mcp_server_name": runtime.sidecar_server_name,
        "auto_escalate": config.auto_escalate,
        "fallback_to_local": config.fallback_to_local,
        "connect_timeout_secs": config.connect_timeout_secs,
        "keepalive_interval_secs": config.keepalive_interval_secs,
        "checkpoint_interval_secs": config.checkpoint_interval_secs,
        "mcp": {
            "state": mcp_state,
            "tool_count": mcp_tool_count,
            "error": mcp_error,
        },
        "capabilities": capability_summary.clone(),
        "warnings": warnings,
    })
}

async fn maybe_bootstrap_colab_browser_connection(state: &AppState, server_name: &str) {
    let client = {
        let manager = state.mcp_manager.lock().await;
        manager.get_client(server_name).cloned()
    };

    let Some(client) = client else {
        return;
    };

    let tools = client.tools().await;
    let has_bootstrap_tool = tools
        .iter()
        .any(|tool| is_colab_bootstrap_tool_name(&tool.name));

    if !has_bootstrap_tool {
        return;
    }

    match client.call_tool(COLAB_BROWSER_BOOTSTRAP_TOOL, None).await {
        Ok(result) => {
            let connected = result.content.iter().any(|content| {
                content
                    .text
                    .as_ref()
                    .map(|text| {
                        let normalized = text.trim().to_ascii_lowercase();
                        normalized == "true"
                            || normalized.contains("\"result\": true")
                            || normalized.contains("connected")
                    })
                    .unwrap_or(false)
            });

            tracing::info!(
                server = %server_name,
                connected,
                "invoked Colab browser bootstrap MCP tool"
            );

            if connected {
                let mut runtime = state.colab_runtime.write().await;
                runtime.last_error = None;
            }

            let mut manager = state.mcp_manager.lock().await;
            if let Err(err) = manager
                .refresh_server_tools(server_name, &state.tool_registry)
                .await
            {
                tracing::warn!(
                    server = %server_name,
                    error = %err,
                    "colab MCP tool refresh after bootstrap failed"
                );
                let mut runtime = state.colab_runtime.write().await;
                runtime.last_error = Some(format!(
                    "Colab MCP tool refresh after bootstrap failed: {err}"
                ));
            }
        }
        Err(err) => {
            tracing::warn!(
                server = %server_name,
                error = %err,
                "colab browser bootstrap tool invocation failed"
            );
            let mut runtime = state.colab_runtime.write().await;
            runtime.last_error = Some(format!(
                "Colab browser bootstrap failed: {err}"
            ));
        }
    }
}

async fn collect_colab_tier_status(state: &AppState) -> serde_json::Value {
    let colab_config = {
        let config = state.config.read().await;
        config.colab.clone()
    };

    let colab_server_name = {
        let runtime = state.colab_runtime.read().await;
        runtime.sidecar_server_name.clone()
    };

    let mut transient_warnings: Vec<String> = Vec::new();

    let statuses = {
        let mut manager = state.mcp_manager.lock().await;
        if colab_config.enabled {
            if let Err(err) = manager
                .refresh_server_tools(&colab_server_name, &state.tool_registry)
                .await
            {
                tracing::warn!(
                    server = %colab_server_name,
                    error = %err,
                    "colab MCP tool refresh failed"
                );
                transient_warnings.push(format!(
                    "Colab MCP tool refresh failed: {err}"
                ));
            }
        }
        manager.status().await
    };

    sync_colab_runtime_snapshot(state, &statuses).await;

    let runtime = state.colab_runtime.read().await.clone();
    let mcp_runtime = statuses
        .iter()
        .find(|s| s.name == runtime.sidecar_server_name);

    let capability_summary =
        build_colab_capability_summary(&state.tool_registry, &runtime.sidecar_server_name);

    build_colab_tier_status_payload(
        &colab_config,
        &runtime,
        mcp_runtime,
        &capability_summary,
        &transient_warnings,
    )
}

#[tauri::command]
pub async fn get_colab_tier_status(
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;

    Ok(collect_colab_tier_status(state).await)
}

#[tauri::command]
pub async fn connect_colab_tier(
    server_name: Option<String>,
    app: AppHandle,
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;

    let mut changed = false;
    let mut server_found = false;
    let resolved_server_name = {
        let mut config = state.config.write().await;

        if let Some(name) = server_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let next = name.to_string();
            if config.colab.mcp_server_name != next {
                config.colab.mcp_server_name = next;
                changed = true;
            }
        }

        if !config.colab.enabled {
            config.colab.enabled = true;
            changed = true;
        }

        let server_name = config.colab.mcp_server_name.clone();
        if let Some(server) = config.mcp.servers.iter_mut().find(|s| s.name == server_name) {
            server_found = true;
            if migrate_legacy_colab_server_command(server) {
                changed = true;
            }
            if !server.enabled {
                server.enabled = true;
                changed = true;
            }
        } else if server_name == COLAB_DEFAULT_SERVER_NAME {
            config.mcp.servers.push(default_colab_server_config());
            server_found = true;
            changed = true;
        }

        if changed {
            config.save().map_err(|e| e.to_string())?;
        }

        server_name
    };

    {
        let mut runtime = state.colab_runtime.write().await;
        runtime.sidecar_server_name = resolved_server_name.clone();
        runtime.selected_notebook = None;
        runtime.state = ColabRuntimeState::SidecarStarting;
        runtime.last_error = if server_found {
            None
        } else {
            Some(format!(
                "Configured MCP server '{}' is missing from mcp.servers",
                resolved_server_name
            ))
        };
    }

    let runtime_report = apply_mcp_runtime_from_config(state).await;

    if server_found {
        maybe_bootstrap_colab_browser_connection(state, &resolved_server_name).await;
    }

    let colab_status = collect_colab_tier_status(state).await;
    let _ = app.emit("colab:status", colab_status.clone());

    Ok(serde_json::json!({
        "status": "connecting",
        "server_name": resolved_server_name,
        "server_found": server_found,
        "runtime": runtime_report,
        "colab": colab_status,
    }))
}

#[tauri::command]
pub async fn disconnect_colab_tier(
    app: AppHandle,
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;

    let mut changed = false;
    {
        let mut config = state.config.write().await;
        if config.colab.enabled {
            config.colab.enabled = false;
            changed = true;
        }

        let target_server = config.colab.mcp_server_name.clone();
        if let Some(server) = config.mcp.servers.iter_mut().find(|s| s.name == target_server) {
            if server.enabled {
                server.enabled = false;
                changed = true;
            }
        }

        if changed {
            config.save().map_err(|e| e.to_string())?;
        }
    }

    {
        let mut runtime = state.colab_runtime.write().await;
        runtime.state = ColabRuntimeState::Disconnected;
        runtime.selected_notebook = None;
        runtime.last_error = None;
    }

    let runtime_report = apply_mcp_runtime_from_config(state).await;

    let colab_status = collect_colab_tier_status(state).await;
    let _ = app.emit("colab:status", colab_status.clone());

    Ok(serde_json::json!({
        "status": "disconnected",
        "runtime": runtime_report,
        "colab": colab_status,
    }))
}

#[tauri::command]
pub async fn set_colab_selected_notebook(
    notebook_id: String,
    app: AppHandle,
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;

    let notebook_id = notebook_id.trim();
    if notebook_id.is_empty() {
        return Err("Notebook identifier cannot be empty".into());
    }

    {
        let mut runtime = state.colab_runtime.write().await;
        if runtime.state == ColabRuntimeState::Disconnected {
            return Err("Colab tier is disconnected. Connect it first.".into());
        }
        runtime.selected_notebook = Some(notebook_id.to_string());
        runtime.state = ColabRuntimeState::Ready;
        runtime.last_error = None;
    }

    let colab_status = collect_colab_tier_status(state).await;
    let _ = app.emit("colab:status", colab_status.clone());
    Ok(colab_status)
}

// ── Google Workspace Commands ────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct GoogleWorkspaceRuntimeSnapshot {
    configured_enabled: bool,
    mcp_state: String,
    mcp_tool_count: usize,
    mcp_error: Option<String>,
    mcp_running: bool,
    gw_client_wired: bool,
}

fn build_google_workspace_status_payload(
    account: &str,
    config_dir: &Path,
    credentials_configured: bool,
    token_present: bool,
    runtime: GoogleWorkspaceRuntimeSnapshot,
) -> serde_json::Value {
    let auth_ready = token_present && credentials_configured;
    let runtime_ready = runtime.mcp_running && runtime.gw_client_wired;
    let connected = auth_ready && runtime_ready;
    let credentials_display_path = config_dir.join("credentials.json");

    let mut warnings: Vec<String> = Vec::new();
    if !credentials_configured {
        warnings.push(format!(
            "credentials.json missing at {}",
            credentials_display_path.display()
        ));
    }
    if !token_present {
        warnings.push(format!("OAuth token missing for account '{account}'"));
    }
    if !runtime.configured_enabled {
        warnings.push("gworkspace MCP server is disabled in config".into());
    }
    if !runtime.mcp_running {
        warnings.push(format!(
            "gworkspace MCP runtime is not running (state={})",
            runtime.mcp_state
        ));
    }
    if runtime.mcp_running && !runtime.gw_client_wired {
        warnings.push("Google tool bridge not yet wired to active MCP client".into());
    }

    serde_json::json!({
        "connected": connected,
        "account": account,
        "credentials_configured": credentials_configured,
        "token_present": token_present,
        "auth_ready": auth_ready,
        "runtime_ready": runtime_ready,
        "gw_client_wired": runtime.gw_client_wired,
        "mcp": {
            "configured_enabled": runtime.configured_enabled,
            "state": runtime.mcp_state,
            "tool_count": runtime.mcp_tool_count,
            "error": runtime.mcp_error,
        },
        "capabilities": {
            "gmail": true,
            "drive": true,
            "calendar": true,
            "docs": true,
            "sheets": true,
            "slides": true,
            "forms": true,
            "meet": false,
            "meet_via_calendar": true,
        },
        "config_dir": config_dir.to_string_lossy(),
        "meet_support_mode": "calendar_conference_link",
        "warnings": warnings,
    })
}

/// Return Google Workspace status with separate auth/runtime/capability signals.
///
/// `connected` is true only when OAuth artifacts are present and the
/// gworkspace MCP runtime is currently usable.
#[tauri::command]
pub async fn get_google_workspace_status(
    account: Option<String>,
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let config_guard = state.config.read().await;
    let account = account
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| google_account_from_config(&config_guard));
    let config_dir = google_mcp_config_dir_from_config(&config_guard);
    let token_path = config_dir.join("tokens").join(format!("{}.json", account));
    let credentials_path = config_dir.join("credentials.json");

    let token_present = token_path.exists();
    let credentials_configured = credentials_path.exists();

    let gworkspace_runtime = {
        let manager = state.mcp_manager.lock().await;
        manager
            .status()
            .await
            .into_iter()
            .find(|s| s.name == "gworkspace")
    };

    let configured_enabled = configured_google_workspace_server(&config_guard)
        .map(|s| s.enabled)
        .unwrap_or(false);
    drop(config_guard);

    let (mcp_state, mcp_tool_count, mcp_error, mcp_running) =
        if let Some(status) = gworkspace_runtime {
            (
                mcp_state_name(status.state).to_string(),
                status.tool_count,
                status.error,
                status.state == McpServerState::Running,
            )
        } else {
            ("not_configured".to_string(), 0usize, None, false)
        };

    let gw_client_wired = state.gw_client_ref.read().await.is_some();
    let payload = build_google_workspace_status_payload(
        &account,
        &config_dir,
        credentials_configured,
        token_present,
        GoogleWorkspaceRuntimeSnapshot {
            configured_enabled,
            mcp_state: mcp_state.clone(),
            mcp_tool_count,
            mcp_error,
            mcp_running,
            gw_client_wired,
        },
    );

    tracing::debug!(
        "[GW] status check: account='{}' connected={} auth_ready={} runtime_ready={} state={}",
        account,
        payload["connected"].as_bool().unwrap_or(false),
        payload["auth_ready"].as_bool().unwrap_or(false),
        payload["runtime_ready"].as_bool().unwrap_or(false),
        mcp_state
    );

    Ok(payload)
}

/// Launch the Google OAuth flow in the system browser.
///
/// Spawns `npx google-workspace-mcp accounts add <account>` which:
/// 1. Starts a local redirect-receiver HTTP server
/// 2. Opens the Google consent page in the default browser
/// 3. Saves the token when the user completes sign-in
///
/// Returns immediately with `status: "pending"`. The frontend should poll
/// `get_google_workspace_status` until `connected` becomes true.
/// Events emitted: `gw:connected` on success, `gw:error` on failure.
#[tauri::command]
pub async fn set_google_workspace_account(
    account: String,
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;

    let account = account.trim();
    if account.is_empty() {
        return Err("Google account name cannot be empty".into());
    }

    let mut config = state.config.write().await;
    let updated = sync_google_workspace_server_config(&mut config, Some(account));
    apply_google_runtime_env_from_config(&config);
    if updated {
        config.save().map_err(|e| e.to_string())?;
    }
    drop(config);

    let runtime = apply_mcp_runtime_from_config(state).await;

    Ok(serde_json::json!({
        "account": account,
        "updated": updated,
        "runtime": runtime,
    }))
}

#[tauri::command]
pub async fn connect_google_workspace(
    account: Option<String>,
    state: State<'_, AppStateCell>,
    app_handle: AppHandle,
) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;

    if let Some(requested) = account.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let mut config = state.config.write().await;
        let changed = sync_google_workspace_server_config(&mut config, Some(requested));
        apply_google_runtime_env_from_config(&config);
        if changed {
            config.save().map_err(|e| e.to_string())?;
        }
    }

    let (account, config_dir) = {
        let config = state.config.read().await;
        let resolved_account = account
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| google_account_from_config(&config));
        let resolved_dir = google_mcp_config_dir_from_config(&config);
        (resolved_account, resolved_dir)
    };
    let config_dir_display = config_dir.to_string_lossy().to_string();

    // Fail fast if credentials.json is missing
    let creds_path = config_dir.join("credentials.json");
    if !creds_path.exists() {
        return Err(
            format!(
                "credentials.json not found at {}. Please add your Google Cloud OAuth client credentials first.",
                creds_path.display()
            ),
        );
    }

    let account_clone = account.clone();
    let config_dir_clone = config_dir_display.clone();
    let mcp_manager = state.mcp_manager.clone();
    let tool_registry = state.tool_registry.clone();
    let gw_client_ref = state.gw_client_ref.clone();
    let config_arc = state.config.clone();
    tokio::spawn(async move {
        tracing::info!("[GW] Starting OAuth flow for account '{}'", account_clone);
        let result = tokio::process::Command::new("npx")
            .args([
                "-y",
                "google-workspace-mcp",
                "accounts",
                "add",
                &account_clone,
            ])
            .env(GOOGLE_MCP_CONFIG_DIR_ENV, &config_dir_clone)
            // inherit stdio so the process can open the browser
            .status()
            .await;

        match result {
            Ok(status) if status.success() => {
                let runtime_refresh_result = async {
                    let desired = { config_arc.read().await.mcp.servers.clone() };
                    let mut manager = mcp_manager.lock().await;
                    let _ = manager.reconcile(desired, &tool_registry).await;
                    let gw_client = manager.get_client("gworkspace").cloned();
                    drop(manager);

                    if let Some(client) = gw_client {
                        gw::set_client(&gw_client_ref, client).await;
                        Ok::<(), String>(())
                    } else {
                        *gw_client_ref.write().await = None;
                        Err("gworkspace runtime not available after OAuth completion".into())
                    }
                }
                .await;

                tracing::info!("[GW] OAuth completed successfully for '{}'", account_clone);
                let _ = app_handle.emit(
                    "gw:connected",
                    serde_json::json!({
                        "account": account_clone,
                        "runtime_refreshed": runtime_refresh_result.is_ok(),
                    }),
                );

                if let Err(msg) = runtime_refresh_result {
                    let _ = app_handle.emit("gw:error", serde_json::json!({ "message": msg }));
                }
            }
            Ok(status) => {
                let msg = format!("OAuth process exited with: {status}");
                tracing::warn!("[GW] {}", msg);
                let _ = app_handle.emit("gw:error", serde_json::json!({ "message": msg }));
            }
            Err(e) => {
                let msg = format!("Failed to spawn OAuth process: {e}");
                tracing::error!("[GW] {}", msg);
                let _ = app_handle.emit("gw:error", serde_json::json!({ "message": msg }));
            }
        }
    });

    Ok(serde_json::json!({
        "status": "pending",
        "account": account,
        "config_dir": config_dir_display,
        "message": "Browser opened for Google sign-in. Complete authorization and return here.",
    }))
}

/// Remove the OAuth token for a Google Workspace account (sign out).
#[tauri::command]
pub async fn disconnect_google_workspace(
    account: Option<String>,
    state: State<'_, AppStateCell>,
) -> Result<(), String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;

    if let Some(requested) = account.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let mut config = state.config.write().await;
        let changed = sync_google_workspace_server_config(&mut config, Some(requested));
        apply_google_runtime_env_from_config(&config);
        if changed {
            config.save().map_err(|e| e.to_string())?;
        }
    }

    let (account, config_dir) = {
        let config = state.config.read().await;
        (
            account
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| google_account_from_config(&config)),
            google_mcp_config_dir_from_config(&config),
        )
    };

    let token_path = config_dir.join("tokens").join(format!("{}.json", account));

    if token_path.exists() {
        std::fs::remove_file(&token_path).map_err(|e| format!("Failed to remove token: {e}"))?;
        tracing::info!("[GW] Disconnected Google account '{}'", account);
    }

    let mut manager = state.mcp_manager.lock().await;
    let _ = manager
        .restart_server("gworkspace", &state.tool_registry)
        .await;
    let statuses = manager.status().await;
    let gw_client = manager.get_client("gworkspace").cloned();
    drop(manager);

    sync_google_workspace_client_ref(state, gw_client).await;
    update_mcp_health_status(state, &statuses).await;
    Ok(())
}

/// Return a snapshot of the hardware orchestrator state.
#[tauri::command]
pub async fn get_orchestrator_status(
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state
        .get()
        .ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let orch_guard = state.orchestrator.read().await.clone();
    match orch_guard.as_ref() {
        Some(orch) => {
            let snap = orch.snapshot();
            let process_alive = orch.server_manager.has_live_process().await;
            let state_healthy = snap.server_healthy;
            let active_turns = state
                .orchestrator_active_turns
                .load(std::sync::atomic::Ordering::SeqCst);
            let idle_for_secs = {
                let lock = state.orchestrator_last_activity_at.lock().await;
                lock.elapsed().as_secs()
            };
            Ok(serde_json::json!({
                "enabled": true,
                "backend": format!("{:?}", snap.backend),
                "current_ngl": snap.current_ngl,
                "current_context": snap.current_context,
                "degradation": format!("{:?}", snap.degradation),
                "server_healthy": state_healthy && process_alive,
                "server_healthy_state": state_healthy,
                "process_alive": process_alive,
                "server_state_code": orch.server_manager.state(),
                "server_swapping": orch.server_manager.is_swapping(),
                "idle_release_enabled": orch.config.idle_release_enabled,
                "idle_release_after_secs": orch.config.idle_release_after_secs,
                "idle_release_check_interval_secs": orch.config.idle_release_check_interval_secs,
                "active_turns": active_turns,
                "idle_for_secs": idle_for_secs,
                "api_url": orch.api_url(),
            }))
        }
        None => Ok(serde_json::json!({
            "enabled": false,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_colab_tier_status_payload,
        build_google_workspace_status_payload, build_image_llm_user_content,
        build_tool_result_event_payload, extract_image_preanalysis_summary,
        extract_preprocessed_image_attachments, infer_image_intent_from_text, local_api_chat,
        migrate_legacy_colab_server_command, sync_telegram_mcp_server_config, ColabRuntimeSnapshot,
        ColabRuntimeState,
        GoogleWorkspaceRuntimeSnapshot, LocalApiBridgeState, LocalApiChatRequest,
        LocalApiResponder, COLAB_OFFICIAL_COMMAND, COLAB_OFFICIAL_SOURCE,
        OCR_HEALTH_PROBE_IMAGE_BYTES,
    };
    use async_trait::async_trait;
    use kria_core::config::ColabConfig;
    use kria_core::mcp::client::McpServerState;
    use kria_core::mcp::server_manager::McpServerStatus;
    use std::path::Path;

    fn assert_confidence_range(metadata: &serde_json::Value) {
        let confidence = metadata
            .get("confidence")
            .and_then(|v| v.as_f64())
            .expect("metadata.confidence should be a number");
        assert!(
            (0.0..=1.0).contains(&confidence),
            "metadata.confidence should be in [0, 1], got {confidence}"
        );
    }

    fn has_warning(payload: &serde_json::Value, needle: &str) -> bool {
        payload["warnings"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .any(|w| w.as_str().map(|s| s.contains(needle)).unwrap_or(false))
            })
            .unwrap_or(false)
    }

    #[test]
    fn migrate_legacy_colab_server_command_rewrites_npx_entry() {
        let mut server = kria_core::config::McpServerConfig {
            name: "colab-mcp".into(),
            command: "npx".into(),
            args: vec!["-y".into(), "@googlecolab/colab-mcp".into()],
            env: std::collections::HashMap::new(),
            enabled: true,
            trust_level: "YELLOW".into(),
            tool_overrides: std::collections::HashMap::new(),
        };

        let changed = migrate_legacy_colab_server_command(&mut server);

        assert!(changed);
        assert_eq!(server.command, COLAB_OFFICIAL_COMMAND);
        assert_eq!(server.args, vec![COLAB_OFFICIAL_SOURCE.to_string()]);
    }

    #[test]
    fn migrate_legacy_colab_server_command_keeps_official_entry() {
        let mut server = kria_core::config::McpServerConfig {
            name: "colab-mcp".into(),
            command: COLAB_OFFICIAL_COMMAND.into(),
            args: vec![COLAB_OFFICIAL_SOURCE.into()],
            env: std::collections::HashMap::new(),
            enabled: true,
            trust_level: "YELLOW".into(),
            tool_overrides: std::collections::HashMap::new(),
        };

        let changed = migrate_legacy_colab_server_command(&mut server);

        assert!(!changed);
        assert_eq!(server.command, COLAB_OFFICIAL_COMMAND);
        assert_eq!(server.args, vec![COLAB_OFFICIAL_SOURCE.to_string()]);
    }

    #[test]
    fn colab_ready_allows_selected_notebook_without_discovery_tool() {
        let mut config = ColabConfig::default();
        config.enabled = true;

        let runtime = ColabRuntimeSnapshot {
            state: ColabRuntimeState::Ready,
            sidecar_server_name: "colab-mcp".into(),
            selected_notebook: Some("mcp_test.ipynb".into()),
            last_error: None,
        };

        let mcp_status = McpServerStatus {
            name: "colab-mcp".into(),
            command: "uvx".into(),
            enabled: true,
            state: McpServerState::Running,
            tool_count: 1,
            error: None,
        };

        let capability_summary = serde_json::json!({
            "category": "mcp_colab-mcp",
            "tool_count": 1,
            "discovered_tools": [],
            "features": {
                "notebook_discovery": false,
                "notebook_selection": false,
                "cell_execution": true,
                "artifact_io": false,
                "runtime_lifecycle": true,
                "package_management": false,
                "checkpointing": false
            },
            "ready_requirements": {
                "requires": ["cell_execution", "notebook_selection_or_discovery"],
                "satisfied": false,
                "missing": ["notebook_selection_or_discovery"]
            }
        });

        let payload = build_colab_tier_status_payload(
            &config,
            &runtime,
            Some(&mcp_status),
            &capability_summary,
            &[],
        );

        assert_eq!(payload["connected"], serde_json::json!(true));
        assert_eq!(payload["ready_for_cloud_task"], serde_json::json!(true));
        assert!(!has_warning(
            &payload,
            "Colab capability requirements are not satisfied"
        ));
    }

    #[test]
    fn colab_ready_still_requires_cell_execution_even_with_selected_notebook() {
        let mut config = ColabConfig::default();
        config.enabled = true;

        let runtime = ColabRuntimeSnapshot {
            state: ColabRuntimeState::Ready,
            sidecar_server_name: "colab-mcp".into(),
            selected_notebook: Some("mcp_test.ipynb".into()),
            last_error: None,
        };

        let mcp_status = McpServerStatus {
            name: "colab-mcp".into(),
            command: "uvx".into(),
            enabled: true,
            state: McpServerState::Running,
            tool_count: 1,
            error: None,
        };

        let capability_summary = serde_json::json!({
            "category": "mcp_colab-mcp",
            "tool_count": 1,
            "discovered_tools": [],
            "features": {
                "notebook_discovery": false,
                "notebook_selection": false,
                "cell_execution": false,
                "artifact_io": false,
                "runtime_lifecycle": true,
                "package_management": false,
                "checkpointing": false
            },
            "ready_requirements": {
                "requires": ["cell_execution", "notebook_selection_or_discovery"],
                "satisfied": false,
                "missing": ["cell_execution", "notebook_selection_or_discovery"]
            }
        });

        let payload = build_colab_tier_status_payload(
            &config,
            &runtime,
            Some(&mcp_status),
            &capability_summary,
            &[],
        );

        assert_eq!(payload["ready_for_cloud_task"], serde_json::json!(false));
        assert!(has_warning(&payload, "cell_execution"));
        assert!(!has_warning(&payload, "notebook_selection_or_discovery"));
    }

    #[test]
    fn google_status_requires_auth_and_runtime_readiness() {
        let payload = build_google_workspace_status_payload(
            "personal",
            Path::new("/tmp/google-mcp"),
            true,
            true,
            GoogleWorkspaceRuntimeSnapshot {
                configured_enabled: true,
                mcp_state: "running".into(),
                mcp_tool_count: 22,
                mcp_error: None,
                mcp_running: true,
                gw_client_wired: false,
            },
        );

        assert_eq!(payload["token_present"], serde_json::json!(true));
        assert_eq!(payload["auth_ready"], serde_json::json!(true));
        assert_eq!(payload["runtime_ready"], serde_json::json!(false));
        assert_eq!(payload["connected"], serde_json::json!(false));
        assert!(has_warning(&payload, "not yet wired"));
    }

    #[test]
    fn google_status_includes_meet_fallback_capabilities_and_runtime_warnings() {
        let payload = build_google_workspace_status_payload(
            "work",
            Path::new("/tmp/google-mcp"),
            true,
            false,
            GoogleWorkspaceRuntimeSnapshot {
                configured_enabled: false,
                mcp_state: "stopped".into(),
                mcp_tool_count: 0,
                mcp_error: Some("process exited".into()),
                mcp_running: false,
                gw_client_wired: false,
            },
        );

        assert_eq!(
            payload["meet_support_mode"],
            serde_json::json!("calendar_conference_link")
        );
        assert_eq!(payload["capabilities"]["meet"], serde_json::json!(false));
        assert_eq!(payload["capabilities"]["forms"], serde_json::json!(true));
        assert_eq!(
            payload["capabilities"]["meet_via_calendar"],
            serde_json::json!(true)
        );
        assert!(has_warning(&payload, "OAuth token missing"));
        assert!(has_warning(&payload, "disabled in config"));
        assert!(has_warning(&payload, "runtime is not running"));
    }

    #[test]
    fn tool_result_payload_news_includes_metadata_keys() {
        let result = serde_json::json!({
            "count": 2,
            "results": [
                {
                    "title": "Story A",
                    "source_tier": 1,
                    "freshness_score": 0.84,
                    "confirmed_by": 3,
                    "age": "2h ago",
                    "region_match": true
                },
                {
                    "title": "Story B",
                    "source_tier": 2,
                    "freshness_score": 0.66,
                    "confirmed_by": 2,
                    "age": "5h ago",
                    "region_match": false
                }
            ]
        });

        let payload = build_tool_result_event_payload("search_news", &result, true);
        let metadata = &payload["metadata"];

        assert!(payload.get("metadata").is_some());
        assert!(metadata.get("confidence").is_some());
        assert!(metadata.get("source_count").is_some());
        assert!(metadata.get("freshness_age_hours").is_some());
        assert!(metadata.get("region_match").is_some());

        assert_confidence_range(metadata);

        assert_eq!(
            metadata["source_count"].as_u64(),
            Some(2),
            "news source_count should match result count"
        );
        assert_eq!(
            metadata["freshness_age_hours"].as_f64(),
            Some(2.0),
            "freshness_age_hours should use the freshest article age"
        );
        assert_eq!(
            metadata["region_match"].as_bool(),
            Some(true),
            "region_match should be true when any row matches region"
        );
    }

    #[test]
    fn tool_result_payload_web_includes_metadata_keys() {
        let result = serde_json::json!({
            "count": 3,
            "results": [
                {"title": "A", "url": "https://example.com/a"},
                {"title": "B", "url": "https://example.com/b"},
                {"title": "C", "url": "https://example.com/c"}
            ]
        });

        let payload = build_tool_result_event_payload("web_search", &result, true);
        let metadata = &payload["metadata"];

        assert!(payload.get("metadata").is_some());
        assert!(metadata.get("confidence").is_some());
        assert!(metadata.get("source_count").is_some());
        assert!(metadata.get("freshness_age_hours").is_some());
        assert!(metadata.get("region_match").is_some());

        assert_confidence_range(metadata);

        assert_eq!(
            metadata["source_count"].as_u64(),
            Some(3),
            "web source_count should match result count"
        );
        assert_eq!(
            metadata["freshness_age_hours"],
            serde_json::Value::Null,
            "web freshness_age_hours should be null"
        );
        assert_eq!(
            metadata["region_match"],
            serde_json::Value::Null,
            "web region_match should be null"
        );
    }

    #[test]
    fn tool_result_payload_google_includes_contract_meta_keys() {
        let result = serde_json::json!({
            "provider": "google_workspace",
            "kind": "gmail",
            "tool": "searchGmail",
            "data": {
                "messages": [
                    {"id": "m1", "subject": "Hello"}
                ]
            },
            "_meta": {
                "schema_version": "1.1",
                "correlation_id": "cid-123",
                "account": "personal"
            }
        });

        let payload = build_tool_result_event_payload("gw_gmail_search", &result, true);
        let metadata = &payload["metadata"];

        assert_eq!(metadata["kind"], serde_json::json!("gmail"));
        assert_eq!(metadata["source_count"], serde_json::json!(1));
        assert_eq!(metadata["schema_version"], serde_json::json!("1.1"));
        assert_eq!(metadata["correlation_id"], serde_json::json!("cid-123"));
        assert_eq!(metadata["account"], serde_json::json!("personal"));
    }

    #[test]
    fn image_user_content_includes_path_and_instruction() {
        let content = build_image_llm_user_content(
            "Analyze this image",
            "/home/test/.kria/attachments/demo.png",
            "mixed",
            Some("Summary: screenshot with text"),
        );

        assert!(content.contains("Analyze this image"));
        assert!(content.contains("Image attachment is already included for this turn."));
        assert!(content.contains("Do not ask the user to re-upload the image"));
        assert!(content.contains("Inferred image-intent hint: mixed"));
        assert!(content.contains("/home/test/.kria/attachments/demo.png"));
        assert!(content.contains("Automatic pre-analysis context"));
        assert!(content.contains("Summary: screenshot with text"));
    }

    #[test]
    fn extract_preprocessed_attachments_prefers_selected_images() {
        let tool_data = serde_json::json!({
            "analysis": {
                "selected_images": [
                    {
                        "kind": "global",
                        "mime_type": "image/jpeg",
                        "data_base64": "abc123"
                    },
                    {
                        "mime_type": "image/png",
                        "data_base64": "xyz789"
                    }
                ],
                "thumbnail_base64": "thumb-data"
            }
        });

        let attachments = extract_preprocessed_image_attachments(&tool_data, "image/png")
            .expect("attachments should be extracted");

        assert_eq!(attachments.len(), 2);
        assert_eq!(attachments[0].mime_type, "image/jpeg");
        assert_eq!(attachments[0].data, "abc123");
        assert_eq!(attachments[1].mime_type, "image/png");
        assert_eq!(attachments[1].data, "xyz789");
    }

    #[test]
    fn extract_preprocessed_attachments_adds_thumbnail_for_roi_only() {
        let tool_data = serde_json::json!({
            "analysis": {
                "selected_images": [
                    {
                        "kind": "roi",
                        "mime_type": "image/jpeg",
                        "data_base64": "roi-only"
                    }
                ],
                "thumbnail_base64": "global-thumb",
                "thumbnail_mime_type": "image/png"
            }
        });

        let attachments = extract_preprocessed_image_attachments(&tool_data, "image/webp")
            .expect("attachments should be extracted");

        assert_eq!(attachments.len(), 2);
        assert_eq!(attachments[0].data, "roi-only");
        assert_eq!(attachments[1].data, "global-thumb");
        assert_eq!(attachments[1].mime_type, "image/png");
    }

    #[test]
    fn extract_preprocessed_attachments_uses_thumbnail_fallback() {
        let tool_data = serde_json::json!({
            "analysis": {
                "selected_images": [],
                "thumbnail_base64": "thumb-data"
            }
        });

        let attachments = extract_preprocessed_image_attachments(&tool_data, "image/webp")
            .expect("thumbnail fallback should produce one attachment");

        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].mime_type, "image/webp");
        assert_eq!(attachments[0].data, "thumb-data");
    }

    #[test]
    fn extract_preprocessed_attachments_falls_back_to_native_thumbnail_from_path() {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("kria_native_preprocessed_{suffix}.ppm"));
        std::fs::write(&path, OCR_HEALTH_PROBE_IMAGE_BYTES)
            .expect("probe image should be writable");

        let tool_data = serde_json::json!({
            "path": path.to_string_lossy().to_string(),
        });

        let attachments = extract_preprocessed_image_attachments(&tool_data, "image/jpeg")
            .expect("native thumbnail fallback should produce one attachment");

        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].mime_type, "image/png");
        assert!(!attachments[0].data.is_empty());

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn extract_image_preanalysis_summary_reads_nested_analysis() {
        let tool_data = serde_json::json!({
            "analysis": {
                "summary": "A terminal screenshot with a stack trace.",
                "metadata": {
                    "width": 1280,
                    "height": 720,
                    "format": "png"
                },
                "features": {
                    "scene_type": "screenshot_or_document"
                },
                "ocr_text": "Error: connection failed on line 42"
            }
        });

        let summary =
            extract_image_preanalysis_summary(&tool_data).expect("summary should be extracted");

        assert!(summary.contains("Summary:"));
        assert!(summary.contains("Resolution: 1280x720"));
        assert!(summary.contains("Scene type: screenshot_or_document"));
        assert!(summary.contains("OCR excerpt:"));
    }

    #[test]
    fn infer_image_intent_handles_varied_prompts() {
        assert_eq!(
            infer_image_intent_from_text("Analyze this image"),
            "scene_understanding"
        );
        assert_eq!(
            infer_image_intent_from_text("Read all text from this screenshot"),
            "ui_error_reading"
        );
        assert_eq!(
            infer_image_intent_from_text("Extract text from this invoice"),
            "document_scan"
        );
        assert_eq!(
            infer_image_intent_from_text("How many objects are in this photo?"),
            "scene_understanding"
        );
        assert_eq!(
            infer_image_intent_from_text("What do you see and what text is there?"),
            "mixed"
        );
    }

    #[test]
    fn syncs_telegram_mcp_server_env_from_primary_telegram_config() {
        let mut config = crate::commands::KriaConfig::default();
        config.server.host = "127.0.0.1".into();
        config.server.port = 3001;
        config.telegram.enabled = true;
        config.telegram.bot_token = "secret-token".into();
        config.telegram.allowed_chat_ids = "123,456".into();
        config.mcp.servers.push(kria_core::config::McpServerConfig {
            name: "telegram".into(),
            command: "kria-telegram-mcp".into(),
            args: vec![],
            env: std::collections::HashMap::new(),
            enabled: false,
            trust_level: "YELLOW".into(),
            tool_overrides: std::collections::HashMap::new(),
        });

        let changed = sync_telegram_mcp_server_config(&mut config);
        assert!(changed);

        let server = config
            .mcp
            .servers
            .iter()
            .find(|s| s.name == "telegram")
            .expect("telegram server should exist");
        assert!(server.enabled);
        assert_eq!(
            server.env.get("TELEGRAM_BOT_TOKEN").map(String::as_str),
            Some("secret-token")
        );
        assert_eq!(
            server.env.get("TELEGRAM_CHAT_IDS").map(String::as_str),
            Some("123,456")
        );
        assert_eq!(
            server.env.get("KRIA_API_URL").map(String::as_str),
            Some("http://127.0.0.1:3001")
        );
    }

    struct EchoLocalApiResponder;

    #[async_trait]
    impl LocalApiResponder for EchoLocalApiResponder {
        async fn respond(&self, request: &LocalApiChatRequest) -> serde_json::Value {
            serde_json::json!({
                "reply": format!("echo: {}", request.message),
                "source": request.source.clone().unwrap_or_else(|| "api".into()),
            })
        }
    }

    #[tokio::test]
    async fn local_api_chat_rejects_empty_messages() {
        let state = LocalApiBridgeState {
            responder: std::sync::Arc::new(EchoLocalApiResponder),
        };

        let (status, body) = local_api_chat(
            axum::extract::State(state),
            axum::Json(LocalApiChatRequest {
                message: "   ".into(),
                session_id: None,
                source: Some("telegram".into()),
                chat_id: Some(42),
                from_user: Some("Tester".into()),
            }),
        )
        .await;

        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(body.0["status"], "error");
    }

    #[tokio::test]
    async fn local_api_chat_uses_responder_payload() {
        let state = LocalApiBridgeState {
            responder: std::sync::Arc::new(EchoLocalApiResponder),
        };

        let (status, body) = local_api_chat(
            axum::extract::State(state),
            axum::Json(LocalApiChatRequest {
                message: "hello".into(),
                session_id: None,
                source: Some("telegram".into()),
                chat_id: Some(42),
                from_user: Some("Tester".into()),
            }),
        )
        .await;

        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(body.0["reply"], "echo: hello");
        assert_eq!(body.0["source"], "telegram");
    }
}

// ────────────────────────────────────────────────────────────────────────────────
// Provisioning commands — first-boot setup wizard
// ────────────────────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn get_provisioning_state() -> Result<serde_json::Value, String> {
    let state = kria_core::infra::provisioning::ProvisioningState::load();
    serde_json::to_value(&state).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn start_provisioning(handle: AppHandle) -> Result<serde_json::Value, String> {
    let cancel = tokio_util::sync::CancellationToken::new();
    let handle_clone = handle.clone();

    let mut engine = kria_core::infra::provisioning::ProvisioningEngine::new(cancel);

    // Run hardware detection synchronously (fast)
    engine.run_hardware_detection().map_err(|e| e.to_string())?;

    let profile = engine
        .state
        .hardware_profile
        .as_ref()
        .ok_or("hardware detection failed")?;

    let event_payload = serde_json::json!({
        "step": "hardware_detection",
        "status": "done",
        "profile": profile,
    });

    // Emit event to frontend
    let _ = handle_clone.emit("provisioning:state_changed", &event_payload);

    serde_json::to_value(&engine.state).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn complete_provisioning() -> Result<serde_json::Value, String> {
    let mut state = kria_core::infra::provisioning::ProvisioningState::load();
    state.current_step = kria_core::infra::provisioning::ProvisioningStep::Complete;
    state.complete_step(kria_core::infra::provisioning::ProvisioningStep::Complete);
    state.save().map_err(|e| e.to_string())?;
    serde_json::to_value(&state).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_provisioning_backend(
    choice_type: String,
    url: Option<String>,
    api_key: Option<String>,
    model_name: Option<String>,
) -> Result<serde_json::Value, String> {
    let cancel = tokio_util::sync::CancellationToken::new();
    let mut engine = kria_core::infra::provisioning::ProvisioningEngine::new(cancel);

    let choice = match choice_type.as_str() {
        "external" => {
            let url = url.ok_or("url is required for external backend")?;
            kria_core::infra::provisioning::BackendChoice::External {
                url,
                api_key,
                model_name,
            }
        }
        _ => kria_core::infra::provisioning::BackendChoice::Local,
    };

    engine.set_backend_choice(choice);
    serde_json::to_value(&engine.state).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn run_provisioning_step(
    handle: AppHandle,
    step: String,
) -> Result<serde_json::Value, String> {
    let cancel = tokio_util::sync::CancellationToken::new();
    let mut engine = kria_core::infra::provisioning::ProvisioningEngine::new(cancel);
    let handle_clone = handle.clone();

    let progress_callback = move |progress: kria_core::infra::download::DownloadProgress| {
        let _ = handle_clone.emit("provisioning:progress", &progress);
    };

    match step.as_str() {
        "model_download" => engine
            .run_model_download(progress_callback)
            .await
            .map_err(|e| e.to_string())?,
        "sidecar_setup" => engine
            .run_sidecar_setup()
            .await
            .map_err(|e| e.to_string())?,
        "server_verification" => engine
            .run_server_verification(progress_callback)
            .await
            .map_err(|e| e.to_string())?,
        _ => return Err(format!("unknown provisioning step: {step}")),
    };

    let _ = handle.emit(
        "provisioning:state_changed",
        serde_json::json!({ "step": step, "status": "done" }),
    );

    serde_json::to_value(&engine.state).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_provisioning_diagnostics() -> Result<String, String> {
    let cancel = tokio_util::sync::CancellationToken::new();
    let engine = kria_core::infra::provisioning::ProvisioningEngine::new(cancel);
    Ok(engine.diagnostic_info())
}

#[tauri::command]
pub async fn get_hardware_profile() -> Result<serde_json::Value, String> {
    // Try loading saved profile first
    if let Some(profile) = kria_core::infra::hardware_profiler::load_profile() {
        return serde_json::to_value(&profile).map_err(|e| e.to_string());
    }
    // Otherwise, run detection
    let profile = kria_core::infra::hardware_profiler::profile_hardware();
    serde_json::to_value(&profile).map_err(|e| e.to_string())
}

/// Inspect the v2 voice stack: which engines are compiled in (cargo features)
/// and what the resolved [`VoiceTierProfile`] would look like for the current
/// config + detected hardware. Used during the v2 rollout to verify that
/// builds + downloads + tier resolution are all consistent before flipping
/// `voice.engine` to `"v2"` in production.
#[tauri::command]
pub async fn voice_v2_status() -> Result<serde_json::Value, String> {
    use kria_core::voice::tier::VoiceTierProfile;
    use kria_core::voice::v2::wake::{WakeWordDetector, WakeWordModels};
    use kria_core::voice::v2::CompiledFeatures;

    let config = KriaConfig::load(None).map_err(|e| e.to_string())?;
    let paths = config.resolve_paths().map_err(|e| e.to_string())?;
    let hw = kria_core::platform::detect::detect_hardware();
    let profile = VoiceTierProfile::build(&config.voice, hw.tier);
    let features = CompiledFeatures::current();

    // Resolve the wake-word model path against KriaPaths. Treat the config
    // value as either an absolute path or a name relative to
    // `<models>/wake/`. Probe both paths so the UI can tell the user which
    // file is missing without installing one.
    let wake_cfg = &config.voice.wake_word;
    let wake_dir = paths.models_dir.join("wake");
    let wake_keyword_path = if wake_cfg.model_path.is_empty() {
        wake_dir.join("hey_ria.onnx")
    } else {
        let p = std::path::PathBuf::from(&wake_cfg.model_path);
        if p.is_absolute() {
            p
        } else if p.components().count() > 1 {
            paths.models_dir.join(p.strip_prefix("models").unwrap_or(&p))
        } else {
            wake_dir.join(p)
        }
    };
    let wake_models = WakeWordModels::from_keyword_path(wake_keyword_path.clone());

    // Try to load the detector; falls back to disabled when the feature is
    // off or model files are missing. Either outcome surfaces in the JSON.
    let wake_detector = if wake_cfg.enabled {
        WakeWordDetector::try_load(
            wake_keyword_path.clone(),
            wake_cfg.sensitivity,
            "hey ria",
            wake_cfg.aliases.clone(),
        )
    } else {
        WakeWordDetector::disabled()
    };

    Ok(serde_json::json!({
        "engine_setting": config.voice.engine,
        "tier": profile.tier.as_str(),
        "ttfa_budget_ms": profile.ttfa_budget_ms,
        "post_edit_timeout_ms": profile.post_edit_timeout_ms,
        "stt_engine": profile.stt_engine,
        "stt_model": profile.stt_model,
        "tts_engine": profile.tts_engine,
        "aec_aggressiveness": profile.aec_aggressiveness,
        "post_edit_always": profile.post_edit_always,
        "hardware_tier": hw.tier.as_str(),
        "compiled_features": features,
        "any_native_backend": features.any_native(),
        "wake_word": {
            "enabled_in_config": wake_cfg.enabled,
            "feature_compiled": features.voice_wake_oww,
            "active": wake_detector.is_active(),
            "sensitivity": wake_cfg.sensitivity,
            "aliases": wake_cfg.aliases,
            "models_dir": wake_dir.display().to_string(),
            "keyword_path": wake_models.keyword.display().to_string(),
            "embedding_path": wake_models.embedding.display().to_string(),
            "melspectrogram_path": wake_models.melspectrogram.display().to_string(),
            "all_models_present": wake_models.all_present(),
        },
        "note": "v2 runtime loop pending; engine='v2' currently falls back to v1.",
    }))
}
