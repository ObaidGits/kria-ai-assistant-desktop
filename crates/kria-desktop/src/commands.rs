use kria_core::config::KriaConfig;
use kria_core::llm::{ChatMessage, ImageAttachment, ModelRouter};
use kria_core::safety::hitl::{HitlGateway, ApprovalResponse};
use kria_core::safety::{PolicyEngine, AuditLogger, RollbackManager};
use kria_core::agent::AgentLoop;
use kria_core::agent::loop_engine::StreamEvent;
use kria_core::tools::registry::{self, ToolRegistry};
use kria_core::tools::mount_manager;
use kria_core::tools::google_workspace as gw;
use kria_core::mcp::McpServerManager;
use kria_core::memory::MemoryStore;
use kria_core::memory::store::ConversationTurn;
use kria_core::memory::embeddings::EmbeddingModel;
use kria_core::memory::vectors::VectorIndex;
use kria_core::infra::EventBus;
use kria_core::infra::health::{HealthRegistry, ServiceStatus};
use kria_core::automation::{AutomationScheduler, MacroRecorder, WorkflowEngine};
use kria_core::platform::detect::{HardwareInfo, HardwareTier, detect_hardware, get_available_package_managers};
use kria_core::sidecar::SidecarBridge;
use kria_core::voice::{VoicePipeline, VoicePipelineState, VoicePipelineEvent, SpeechToText, TextToSpeech};
use std::sync::Arc;
use chrono::Utc;
use tauri::{AppHandle, Manager, State, Emitter};
use tokio::sync::RwLock;

use kria_core::platform::telegram::TelegramBridge;

/// Find a binary on the system PATH.
fn which_binary(name: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join(name))
                .find(|p| p.exists())
        })
}

fn emit_agent_stage(
    app: &AppHandle,
    step: &str,
    message: &str,
    detail: Option<serde_json::Value>,
) {
    let detail_value = detail.unwrap_or(serde_json::Value::Null);
    let payload = serde_json::json!({
        "step": step,
        "message": message,
        "detail": detail_value,
        "ts": Utc::now().to_rfc3339(),
    });
    let _ = app.emit("agent:stage", payload);
    tracing::info!(step = step, message = message, "agent stage emitted");
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
    v.max(0.0).min(1.0)
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

                    if row.get("region_match").and_then(|v| v.as_bool()).unwrap_or(false) {
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
            let chars = result.get("char_count").and_then(|v| v.as_u64()).unwrap_or(0);
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

fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

fn load_cached_hardware_info(cache_path: &std::path::Path) -> Option<HardwareInfo> {
    let text = std::fs::read_to_string(cache_path).ok()?;
    serde_json::from_str::<HardwareInfo>(&text).ok()
}

fn resolve_hardware_info(config: &KriaConfig, cache_path: &std::path::Path) -> (HardwareInfo, String) {
    // Highest precedence: explicit env override.
    if let Ok(env_tier) = std::env::var("KRIA_TIER") {
        let env_tier = env_tier.trim();
        if !env_tier.is_empty() {
            let mut hw = detect_hardware();
            hw.tier = HardwareTier::from_str(env_tier);
            return (hw, format!("env:KRIA_TIER={env_tier}"));
        }
    }

    // Next precedence: config override.
    if !config.hardware.tier.trim().is_empty() {
        let mut hw = detect_hardware();
        hw.tier = HardwareTier::from_str(&config.hardware.tier);
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
    pub voice_pipeline: Arc<VoicePipeline>,
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
}

/// Initialize the KRIA runtime (called from setup).
pub async fn init_runtime(handle: &AppHandle) -> anyhow::Result<()> {
    let mut config = KriaConfig::load(None)?;

    // Initialize logging
    let paths = config.resolve_paths()?;
    kria_core::infra::logging::setup_logging(&paths.logs_dir);

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
            }
            Err(e) => {
                tracing::warn!("Python sidecar failed to start (non-fatal): {}", e);
                health_sidecar.update("sidecar", ServiceStatus::Degraded, Some(format!("{e}")));
            }
        }
    });

    // Initialize embedding model and vector index for fact extraction
    let embeddings = Arc::new(EmbeddingModel::load(384).unwrap_or_else(|e| {
        tracing::warn!("embedding model load error (using fallback): {}", e);
        EmbeddingModel::load(384).expect("fallback always succeeds")
    }));
    let vectors_path = paths.data_dir.join("vectors.bin");
    let vectors = Arc::new(VectorIndex::open(&vectors_path, 384).unwrap_or_else(|_| VectorIndex::in_memory(384)));

    // Build the full tool registry (60+ tools + 6 precognitive) with MemoryStore, RAG, and Proactive
    let rag_engine = Arc::new(kria_core::memory::RagEngine::new(
        memory_store.clone(), vectors.clone(), embeddings.clone(),
    ));
    let proactive_engine = Arc::new(kria_core::automation::ProactiveEngine::new(
        kria_core::automation::proactive::HealthThresholds::default(),
    ));
    let tool_registry_inner = registry::build_registry_full(
        Some(memory_store.clone()), Some(rag_engine.clone()), Some(proactive_engine.clone()),
    );
    kria_core::tools::precognitive::register(&tool_registry_inner, sidecar.clone());
    kria_core::tools::news::register(&tool_registry_inner, sidecar.clone());
    // Re-register vision tools with sidecar (overrides the None-sidecar registration from build_registry)
    kria_core::tools::vision::register(&tool_registry_inner, Some(sidecar.clone()));

    // ── MCP server startup ────────────────────────────────────────────────────
    // Load MCP server configs from mcp_servers.json (supplements TOML config)
    tracing::info!("[MCP] loading MCP server configs from mcp_servers.json");
    {
        let mut cfg = config.clone();
        kria_core::config::load_mcp_servers(&mut cfg);
        config = cfg;
    }
    let total_servers = config.mcp.servers.len();
    let enabled_servers = config.mcp.servers.iter().filter(|s| s.enabled).count();
    tracing::info!(
        "[MCP] {} total MCP server(s) configured, {} enabled",
        total_servers, enabled_servers
    );
    for s in &config.mcp.servers {
        tracing::info!(
            "[MCP]   server='{}' enabled={} command='{}' args={:?}",
            s.name, s.enabled, s.command, s.args
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
    let mount_mgr = Arc::new(tokio::sync::RwLock::new(mount_manager::build_default_mount_manager()));

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
    let agent_loop = Arc::new(AgentLoop::new(
        model_router.clone(),
        tool_registry.clone(),
        mount_mgr,
        policy_engine,
        hitl.clone(),
        audit_logger,
        rollback_mgr,
    )
    .with_max_tool_rounds(max_tool_rounds)
    .with_hardware_tier(hardware_info.tier.as_str()));

    tracing::info!("KRIA runtime initialized — agent loop active");

    // Build voice pipeline
    let stt_model_path = paths.models_dir.join("stt").join(&config.voice.stt_model);
    let tts_voice_file = format!("{}.onnx", config.voice.tts_voice);
    let tts_model_path = paths.models_dir.join("piper").join(&tts_voice_file);

    // Look for whisper and piper binaries on PATH
    let whisper_bin = which_binary("whisper-cpp").or_else(|| which_binary("main"));
    let piper_bin = which_binary("piper");

    let stt = SpeechToText::new(stt_model_path, whisper_bin);
    let tts = TextToSpeech::new(tts_model_path, piper_bin);
    let vad_model_path = paths.models_dir.join("vad").join("silero_vad.onnx");
    let voice_pipeline = Arc::new(
        VoicePipeline::new(config.voice.clone(), stt, tts)
            .with_vad_model(vad_model_path)
    );

    // Health registry — register all subsystems
    health.register("memory_store");
    health.register("model_router");
    health.register("tool_registry");
    health.register("agent_loop");
    health.register("sidecar");
    health.register("voice_pipeline");
    health.register("embeddings");
    health.register("vectors");
    // Mark core services as healthy
    health.update("memory_store", ServiceStatus::Healthy, None);
    // model_router: probe the actual LLM server asynchronously
    health.update("model_router", ServiceStatus::Starting, Some("probing LLM server...".into()));
    health.update("tool_registry", ServiceStatus::Healthy, Some(format!("{} tools", tool_registry.len())));
    health.update("agent_loop", ServiceStatus::Healthy, None);
    health.update("voice_pipeline", ServiceStatus::Healthy, None);
    health.update("embeddings", ServiceStatus::Healthy, None);
    health.update("vectors", ServiceStatus::Healthy, None);
    // MCP servers start in background — mark as starting
    health.register("mcp_servers");
    health.update("mcp_servers", ServiceStatus::Starting, Some("connecting to MCP servers...".into()));

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
                    None => status["local_model"].as_str().unwrap_or("unknown").to_string(),
                };
                health_mr.update("model_router", ServiceStatus::Healthy,
                    Some(format!("model: {}", model_name)));
            } else {
                health_mr.update("model_router", ServiceStatus::Degraded,
                    Some("LLM server not reachable".into()));
            }
        });
    }
    // Sidecar starts as "starting" — updated when spawn completes or fails
    health.update("sidecar", ServiceStatus::Starting, None);

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

    // Auto-start Telegram bridge if configured
    let telegram_config = config.read().await.telegram.clone();
    if telegram_config.enabled && !telegram_config.bot_token.is_empty() && telegram_config.auto_start {
        tracing::info!("Auto-starting Telegram bridge");
        let bridge = TelegramBridge::spawn(
            telegram_config,
            agent_loop.clone(),
            memory_store.clone(),
            tool_registry.clone(),
            embeddings.clone(),
            vectors.clone(),
            hardware_info.tier.as_str().to_string(),
        );
        *telegram_bridge.write().await = Some(bridge);
    }

    let state = AppState {
        config,
        model_router,
        agent_loop,
        tool_registry: tool_registry.clone(),
        memory_store,
        hitl,
        event_bus,
        sidecar,
        embeddings,
        vectors,
        current_session_id: Arc::new(RwLock::new(uuid::Uuid::new_v4().to_string())),
        voice_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        voice_pipeline,
        health: health.clone(),
        scheduler: scheduler_arc,
        macro_recorder: macro_recorder_arc,
        workflow_engine: workflow_engine_arc,
        started_at: std::time::Instant::now(),
        hardware_info,
        proactive: proactive_engine,
        telegram_bridge,
        mcp_manager: mcp_manager.clone(),
    };

    if let Err(_) = handle.state::<AppStateCell>().set(state) {
        tracing::error!("[INIT] AppState was already initialized — this is a bug");
    }

    tracing::info!("[INIT] AppState set — frontend is now unblocked");

    // ── Background MCP server startup (non-blocking) ──────────────────────────
    // MCP servers (especially npx-based ones) can take minutes to start.
    // They run in background and dynamically register tools into the thread-safe registry.
    {
        let tool_reg_bg = tool_registry.clone();
        let mcp_mgr_bg = mcp_manager.clone();
        let gw_ref_bg = gw_client_ref;
        let health_bg = health.clone();
        let handle_bg = handle.clone();
        tokio::spawn(async move {
            tracing::info!("[MCP] starting MCP servers in background (parallel)");
            let mut mgr = mcp_mgr_bg.lock().await;
            mgr.start_all(&tool_reg_bg).await;

            // Wire GW client if gworkspace server started successfully
            if let Some(live_client) = mgr.get_client("gworkspace") {
                gw::set_client(&gw_ref_bg, live_client.clone()).await;
                tracing::info!("[GW] GwClientRef populated — Google Workspace tools are now active");
                let _ = handle_bg.emit("gw:connected", serde_json::json!({}));
            } else {
                tracing::warn!(
                    "[GW] gworkspace MCP server not available. \
                     Google Workspace tools will return 'not connected' errors."
                );
            }

            let statuses = mgr.status().await;
            let running = statuses.iter().filter(|s| s.tool_count > 0).count();
            health_bg.update("mcp_servers", ServiceStatus::Healthy,
                Some(format!("{}/{} servers running, {} total tools", running, statuses.len(), tool_reg_bg.len())));

            let _ = handle_bg.emit("mcp:ready", serde_json::json!({
                "running": running,
                "total": statuses.len(),
                "tools": tool_reg_bg.len(),
            }));
            tracing::info!("[MCP] background startup complete — {} tools available", tool_reg_bg.len());

            // Start MCP health heartbeat (pings servers every 30s, auto-restarts on failure)
            drop(mgr);
            McpServerManager::spawn_health_heartbeat(mcp_mgr_bg, tool_reg_bg, 30);
        });
    }

    Ok(())
}

#[tauri::command]
pub async fn send_message(
    message: String,
    state: State<'_, AppStateCell>,
    app: AppHandle,
) -> Result<serde_json::Value, String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    tracing::info!("User message: {}", &message);

    emit_agent_stage(
        &app,
        "input_received",
        "Prompt received from UI",
        Some(serde_json::json!({
            "chars": message.chars().count(),
        })),
    );

    let _ = app.emit("agent:thinking", serde_json::json!({"status": "processing"}));

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
    let tool_descriptions = tool_defs.iter()
        .map(|d| {
            let params: Vec<String> = d.parameters.iter()
                .map(|p| format!("  - {}: {} ({}{})", p.name, p.description, p.param_type,
                    if p.required { ", required" } else { "" }))
                .collect();
            format!("### {}\n{}\nParameters:\n{}", d.name, d.description, params.join("\n"))
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    emit_agent_stage(
        &app,
        "tool_context_ready",
        "Tool descriptions prepared",
        Some(serde_json::json!({ "tool_count": tool_defs.len() })),
    );

    // Retrieve user context from memory
    let user_name = memory_store.get_preference("user_name")
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
            let fact_lines: Vec<String> = facts.iter()
                .map(|f| format!("- {}", f.text))
                .collect();
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
    let recent_turns = memory_store.get_recent_turns(&session_id, 20)
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
        if memory_store.get_preference(&title_key).unwrap_or(None).is_none() {
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
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<StreamEvent>();

    let app_handle = app.clone();
    let session_id_clone = session_id.clone();
    let memory_store_clone = memory_store.clone();
    let embeddings_clone = state.embeddings.clone();
    let vectors_clone = state.vectors.clone();
    let user_message_clone = message.clone();

    // Spawn agent loop in background
    let agent = agent_loop.clone();
    let sid = session_id.clone();
    tauri::async_runtime::spawn(async move {
        agent.run(&sid, &mut messages, event_tx).await;
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

        emit_agent_stage(
            &app_handle,
            "awaiting_llm_output",
            "Prompt sent to LLM; waiting for first response token",
            None,
        );

        while let Some(event) = event_rx.recv().await {
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
                    let _ = app_handle.emit("agent:token", serde_json::json!({
                        "text": text,
                    }));
                }
                StreamEvent::ToolStart { name, params } => {
                    tracing::info!("Tool call: {} with {:?}", name, params);
                    emit_agent_stage(
                        &app_handle,
                        "tool_started",
                        "Tool execution started",
                        Some(serde_json::json!({
                            "tool": name.clone(),
                        })),
                    );
                    let _ = app_handle.emit("agent:tool_call", serde_json::json!({
                        "name": name,
                        "params": params,
                    }));
                }
                StreamEvent::ToolEnd { name, result, success } => {
                    tracing::info!("Tool result: {} success={}", name, success);
                    emit_agent_stage(
                        &app_handle,
                        "tool_finished",
                        "Tool execution completed",
                        Some(serde_json::json!({
                            "tool": name.clone(),
                            "success": success,
                        })),
                    );
                    let payload = build_tool_result_event_payload(&name, &result, success);
                    let _ = app_handle.emit("agent:tool_result", payload);
                }
                StreamEvent::ApprovalRequired { request_id, action, risk_level, parameters } => {
                    emit_agent_stage(
                        &app_handle,
                        "approval_required",
                        "Agent requested user approval",
                        Some(serde_json::json!({
                            "action": action.clone(),
                            "risk_level": risk_level.clone(),
                        })),
                    );
                    let _ = app_handle.emit("agent:approval_required", serde_json::json!({
                        "requestId": request_id,
                        "toolName": action,
                        "riskLevel": risk_level,
                        "args": parameters,
                        "reason": "",
                    }));
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
                    let _ = app_handle.emit("agent:approval_result", serde_json::json!({
                        "action": action,
                        "approved": approved,
                    }));
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
                    let _ = app_handle.emit("agent:thinking", serde_json::json!({
                        "status": "planning",
                        "plan": plan,
                    }));
                }
                StreamEvent::Error(err) => {
                    tracing::error!("Agent error: {}", err);
                    emit_agent_stage(
                        &app_handle,
                        "failed",
                        "Agent stream reported an error",
                        Some(serde_json::json!({
                            "error": err.clone(),
                        })),
                    );
                    let _ = app_handle.emit("agent:token", serde_json::json!({
                        "text": format!("⚠️ {err}"),
                    }));
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

        let _ = app_handle.emit("agent:done", serde_json::json!({}));
    });

    Ok(serde_json::json!({
        "status": "processing",
    }))
}

#[tauri::command]
pub async fn get_session_history(
    state: State<'_, AppStateCell>,
) -> Result<Vec<serde_json::Value>, String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let session_id = state.current_session_id.read().await.clone();
    let turns = state.memory_store.get_recent_turns(&session_id, 100)
        .map_err(|e| e.to_string())?;
    let messages: Vec<serde_json::Value> = turns.iter().map(|t| {
        serde_json::json!({
            "role": t.role,
            "content": t.content,
            "tool_name": t.tool_name,
            "timestamp": t.timestamp.to_rfc3339(),
        })
    }).collect();
    Ok(messages)
}

#[tauri::command]
pub async fn create_session(
    title: Option<String>,
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let new_id = uuid::Uuid::new_v4().to_string();
    *state.current_session_id.write().await = new_id.clone();

    // Store a metadata preference for session title
    if let Some(t) = title {
        let key = format!("session_title:{}", new_id);
        let _ = state.memory_store.set_preference(&key, &t);
    }

    tracing::info!(session_id = %new_id, "new session created");
    Ok(serde_json::json!({
        "session_id": new_id,
    }))
}

#[tauri::command]
pub async fn list_sessions(
    state: State<'_, AppStateCell>,
) -> Result<Vec<serde_json::Value>, String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let sessions = state.memory_store.list_sessions()
        .map_err(|e| e.to_string())?;
    let current = state.current_session_id.read().await.clone();
    let result: Vec<serde_json::Value> = sessions.into_iter().map(|(id, count, last_active)| {
        let title = state.memory_store.get_preference(&format!("session_title:{}", id))
            .unwrap_or(None)
            .unwrap_or_else(|| format!("Session ({})", &id[..8]));
        serde_json::json!({
            "id": id,
            "title": title,
            "message_count": count,
            "last_active": last_active,
            "is_current": id == current,
        })
    }).collect();
    Ok(result)
}

#[tauri::command]
pub async fn switch_session(
    session_id: String,
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    *state.current_session_id.write().await = session_id.clone();
    // Load history for the new session
    let turns = state.memory_store.get_recent_turns(&session_id, 100)
        .map_err(|e| e.to_string())?;
    let messages: Vec<serde_json::Value> = turns.iter().map(|t| {
        serde_json::json!({
            "role": t.role,
            "content": t.content,
            "tool_name": t.tool_name,
            "timestamp": t.timestamp.to_rfc3339(),
        })
    }).collect();
    Ok(serde_json::json!({
        "session_id": session_id,
        "messages": messages,
    }))
}

#[tauri::command]
pub async fn delete_session(
    session_id: String,
    state: State<'_, AppStateCell>,
) -> Result<(), String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let current = state.current_session_id.read().await.clone();
    state.memory_store.delete_session(&session_id)
        .map_err(|e| e.to_string())?;
    // If we deleted the current session, create a new one
    if session_id == current {
        *state.current_session_id.write().await = uuid::Uuid::new_v4().to_string();
    }
    Ok(())
}

#[tauri::command]
pub async fn rename_session(
    session_id: String,
    title: String,
    state: State<'_, AppStateCell>,
) -> Result<(), String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let key = format!("session_title:{}", session_id);
    state.memory_store.set_preference(&key, &title)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn search_sessions(
    query: String,
    state: State<'_, AppStateCell>,
) -> Result<Vec<serde_json::Value>, String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let results = state.memory_store.search_conversations(&query, 20)
        .map_err(|e| e.to_string())?;
    let items: Vec<serde_json::Value> = results.into_iter().map(|t| {
        serde_json::json!({
            "session_id": t.session_id,
            "role": t.role,
            "content": t.content,
            "timestamp": t.timestamp.to_rfc3339(),
        })
    }).collect();
    Ok(items)
}

#[tauri::command]
pub async fn cancel_request(
    state: State<'_, AppStateCell>,
) -> Result<(), String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    state.hitl.cancel_all().await;
    Ok(())
}

#[tauri::command]
pub async fn approve_action(
    request_id: String,
    state: State<'_, AppStateCell>,
) -> Result<(), String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    state.hitl.respond(&request_id, ApprovalResponse::Approved).await;
    Ok(())
}

#[tauri::command]
pub async fn deny_action(
    request_id: String,
    _reason: Option<String>,
    state: State<'_, AppStateCell>,
) -> Result<(), String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    state.hitl.respond(&request_id, ApprovalResponse::Denied).await;
    Ok(())
}

#[tauri::command]
pub async fn get_health(
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    // Refresh LLM server health on each call
    let mr_status = state.model_router.status().await;
    let mr_healthy = mr_status["local_healthy"].as_bool().unwrap_or(false);
    let mr_model = mr_status["local_model"].as_str().unwrap_or("unknown");
    if mr_healthy {
        state.health.update("model_router", ServiceStatus::Healthy,
            Some(format!("model: {}", mr_model)));
    } else {
        state.health.update("model_router", ServiceStatus::Degraded,
            Some("LLM server not reachable".into()));
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
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
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
pub async fn get_settings(
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let config = state.config.read().await;
    serde_json::to_value(&*config).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn update_settings(
    settings: serde_json::Value,
    state: State<'_, AppStateCell>,
) -> Result<(), String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let new_config: KriaConfig = serde_json::from_value(settings)
        .map_err(|e| e.to_string())?;
    // Persist to disk first
    new_config.save().map_err(|e| e.to_string())?;
    // Then update in-memory config
    let mut config = state.config.write().await;
    *config = new_config;
    Ok(())
}

#[tauri::command]
pub async fn list_knowledge_base(
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let docs = state.memory_store.list_documents().map_err(|e| e.to_string())?;
    let items: Vec<serde_json::Value> = docs.iter().map(|(id, name, dtype, chunks)| {
        serde_json::json!({
            "doc_id": id,
            "name": name,
            "type": dtype,
            "chunks": chunks,
        })
    }).collect();
    Ok(serde_json::json!({ "documents": items, "count": items.len() }))
}

#[tauri::command]
pub async fn get_alerts(
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let alerts = state.proactive.get_alerts().await;
    let items: Vec<serde_json::Value> = alerts.iter().map(|a| {
        serde_json::json!({
            "id": a.id,
            "category": format!("{:?}", a.category).to_lowercase(),
            "title": a.title,
            "message": a.message,
            "suggestion": a.suggestion,
            "timestamp": a.timestamp.to_rfc3339(),
        })
    }).collect();
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
        .add_filter(&filter_name, &extensions.iter().map(|s| s.as_str()).collect::<Vec<_>>())
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
    std::process::Command::new("xdg-open").arg(&path_str).spawn()
        .map_err(|e| format!("Failed to open file: {e}"))?;

    #[cfg(target_os = "macos")]
    std::process::Command::new("open").arg(&path_str).spawn()
        .map_err(|e| format!("Failed to open file: {e}"))?;

    #[cfg(target_os = "windows")]
    std::process::Command::new("cmd").args(["/c", "start", "", &path_str]).spawn()
        .map_err(|e| format!("Failed to open file: {e}"))?;

    Ok(())
}

#[tauri::command]
pub async fn list_models(
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let config = state.config.read().await;
    let paths = config.resolve_paths().map_err(|e| e.to_string())?;
    let mgr = kria_core::llm::model_manager::ModelManager::new(paths.models_dir.join("llm"));
    let models = mgr.list_llm_models();
    Ok(serde_json::to_value(&models).unwrap_or_default())
}

#[tauri::command]
pub async fn start_voice(
    state: State<'_, AppStateCell>,
    app: AppHandle,
) -> Result<(), String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    if state.voice_active.load(std::sync::atomic::Ordering::Relaxed) {
        return Ok(()); // Already active
    }

    // Pre-flight checks: verify required binaries and models exist
    let whisper_available = which_binary("whisper-cpp").or_else(|| which_binary("main")).is_some();
    if !whisper_available {
        return Err("Voice requires whisper-cpp (or 'main' binary from whisper.cpp) on your PATH. Install it with: sudo apt install whisper.cpp OR build from https://github.com/ggerganov/whisper.cpp".into());
    }

    let piper_available = which_binary("piper").is_some();
    if !piper_available {
        return Err("Voice requires Piper TTS binary on your PATH. Install it from: https://github.com/rhasspy/piper/releases".into());
    }

    // Verify STT model exists
    {
        let config = state.config.read().await;
        let paths = config.resolve_paths().map_err(|e| e.to_string())?;
        let stt_model = paths.models_dir.join("stt").join(&config.voice.stt_model);
        if !stt_model.exists() {
            return Err(format!("STT model not found at: {}. Run 'python scripts/download_models.py' to download models.", stt_model.display()));
        }
        let tts_voice_file = format!("{}.onnx", config.voice.tts_voice);
        let tts_model = paths.models_dir.join("piper").join(&tts_voice_file);
        if !tts_model.exists() {
            return Err(format!("TTS voice model not found at: {}. Run 'python scripts/download_models.py' to download models.", tts_model.display()));
        }
    }

    state.voice_active.store(true, std::sync::atomic::Ordering::Relaxed);

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<VoicePipelineEvent>();

    state.voice_pipeline.start(event_tx).await.map_err(|e| e.to_string())?;

    let _ = app.emit("voice:state", serde_json::json!({ "state": "listening" }));

    // Spawn a task that listens for voice pipeline events and forwards them
    let app_handle = app.clone();
    let voice_pipeline = state.voice_pipeline.clone();
    let memory_store = state.memory_store.clone();
    let agent_loop = state.agent_loop.clone();
    let tool_registry = state.tool_registry.clone();
    let event_bus = state.event_bus.clone();
    let config = state.config.clone();
    let session_id_lock = state.current_session_id.clone();
    let embeddings = state.embeddings.clone();
    let vectors = state.vectors.clone();
    let hw_info_voice = state.hardware_info.clone();

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
                    let _ = app_handle.emit("voice:state", serde_json::json!({ "state": state_str }));
                }
                VoicePipelineEvent::PartialTranscript(text) => {
                    let _ = app_handle.emit("voice:partial_transcript", serde_json::json!({ "text": text, "partial": true }));
                }
                VoicePipelineEvent::Transcript(text) => {
                    tracing::info!(transcript = %text, "voice: transcript received");
                    let _ = app_handle.emit("voice:transcript", serde_json::json!({ "text": text }));

                    // Feed transcript through the agent loop (same as send_message)
                    let session_id = session_id_lock.read().await.clone();
                    let config_guard = config.read().await;
                    let hw_tier = hw_info_voice.tier.as_str();

                    let tool_descriptions = tool_registry.list_for_tier(hw_tier).iter()
                        .map(|d| {
                            let params: Vec<String> = d.parameters.iter()
                                .map(|p| format!("  - {}: {} ({}{})", p.name, p.description, p.param_type,
                                    if p.required { ", required" } else { "" }))
                                .collect();
                            format!("### {}\n{}\nParameters:\n{}", d.name, d.description, params.join("\n"))
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n");

                    let user_name = memory_store.get_preference("user_name")
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
                                format!("{} (also available: {})", primary.as_str(), alts.join(", "))
                            }
                        }
                    };
                    let memory_context = match memory_store.search_facts(&text, 5) {
                        Ok(facts) if !facts.is_empty() => {
                            let lines: Vec<String> = facts.iter().map(|f| format!("- {}", f.text)).collect();
                            format!("Known facts about the user:\n{}", lines.join("\n"))
                        }
                        _ => String::new(),
                    };

                    let system_prompt = kria_core::agent::prompts::build_system_prompt(
                        &tool_descriptions, &user_name, os_name, hw_tier, &pm_string_voice, &memory_context,
                    );
                    drop(config_guard);

                    let recent_turns = memory_store.get_recent_turns(&session_id, 20).unwrap_or_default();
                    let mut messages = Vec::with_capacity(recent_turns.len() + 2);
                    messages.push(ChatMessage { role: "system".into(), content: system_prompt, name: None, images: None });
                    for turn in &recent_turns {
                        messages.push(ChatMessage { role: turn.role.clone(), content: turn.content.clone(), name: turn.tool_name.clone(), images: None });
                    }
                    messages.push(ChatMessage { role: "user".into(), content: text.clone(), name: None, images: None });

                    let _ = memory_store.store_turn(&ConversationTurn {
                        id: None, session_id: session_id.clone(), role: "user".into(),
                        content: format!("🎤 {}", text), tool_name: None, tool_result: None,
                        tokens_used: None, timestamp: Utc::now(),
                    });

                    event_bus.publish(kria_core::infra::event_bus::KriaEvent::MessageReceived {
                        session_id: session_id.clone(), content: text.clone(),
                    });

                    let _ = app_handle.emit("agent:thinking", serde_json::json!({"status": "processing"}));

                    let (agent_tx, mut agent_rx) = tokio::sync::mpsc::unbounded_channel::<StreamEvent>();

                    let agent = agent_loop.clone();
                    let sid = session_id.clone();
                    tokio::spawn(async move {
                        agent.run(&sid, &mut messages, agent_tx).await;
                    });

                    // Collect agent response for TTS
                    let mut full_response = String::new();
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
                                let _ = app2.emit("agent:tool_call", serde_json::json!({"name": name, "params": params}));
                            }
                            StreamEvent::ToolEnd { name, result, success } => {
                                let payload = build_tool_result_event_payload(&name, &result, success);
                                let _ = app2.emit("agent:tool_result", payload);
                            }
                            StreamEvent::ApprovalRequired { request_id, action, risk_level, parameters } => {
                                let _ = app2.emit("agent:approval_required", serde_json::json!({"requestId": request_id, "toolName": action, "riskLevel": risk_level, "args": parameters, "reason": ""}));
                            }
                            StreamEvent::ApprovalResult { action, approved } => {
                                let _ = app2.emit("agent:approval_result", serde_json::json!({"action": action, "approved": approved}));
                            }
                            StreamEvent::Plan(plan) => {
                                let _ = app2.emit("agent:thinking", serde_json::json!({"status": "planning", "plan": plan}));
                            }
                            StreamEvent::Error(err) => {
                                let _ = app2.emit("agent:token", serde_json::json!({"text": format!("⚠️ {err}")}));
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
                            id: None, session_id: sid2.clone(), role: "assistant".into(),
                            content: full_response.clone(), tool_name: None, tool_result: None,
                            tokens_used: None, timestamp: Utc::now(),
                        });
                        let fact_mgr = kria_core::memory::facts::FactManager::new(&ms2, &vec2, &emb2);
                        let _ = fact_mgr.extract_from_turn(&text2, &full_response);

                        // Speak the response via TTS
                        if let Err(e) = vp.speak(&full_response).await {
                            tracing::warn!("TTS playback failed: {e}");
                        }
                    }

                    let _ = app2.emit("agent:done", serde_json::json!({}));
                }
                VoicePipelineEvent::SpeakingStarted => {
                    let _ = app_handle.emit("voice:state", serde_json::json!({ "state": "speaking" }));
                }
                VoicePipelineEvent::SpeakingDone => {
                    let _ = app_handle.emit("voice:state", serde_json::json!({ "state": "listening" }));
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
pub async fn stop_voice(
    state: State<'_, AppStateCell>,
    app: AppHandle,
) -> Result<(), String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    state.voice_active.store(false, std::sync::atomic::Ordering::Relaxed);
    state.voice_pipeline.stop().await;
    let _ = app.emit("voice:state", serde_json::json!({ "state": "idle" }));
    Ok(())
}

#[tauri::command]
pub async fn get_voice_status(
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let pipeline_state = state.voice_pipeline.state().await;
    Ok(serde_json::json!({
        "active": state.voice_active.load(std::sync::atomic::Ordering::Relaxed),
        "state": pipeline_state,
    }))
}

#[tauri::command]
pub async fn send_image_message(
    image_data: Vec<u8>,
    mime_type: String,
    text: Option<String>,
    state: State<'_, AppStateCell>,
    app: AppHandle,
) -> Result<serde_json::Value, String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
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
    let allowed = ["image/png", "image/jpeg", "image/gif", "image/webp", "image/bmp"];
    if !allowed.contains(&mime_type.as_str()) {
        return Err(format!("unsupported image type: {}", mime_type));
    }

    // Validate image size (max 10 MB)
    if image_data.len() > 10 * 1024 * 1024 {
        return Err("image too large (max 10 MB)".into());
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

    // Encode to base64 for the LLM
    let b64 = kria_core::preprocessing::image::ImageProcessor::to_base64(&filepath)
        .map_err(|e| e.to_string())?;

    emit_agent_stage(
        &app,
        "image_encoded",
        "Image encoded for multimodal LLM input",
        None,
    );

    let user_text = text.unwrap_or_else(|| "What's in this image?".into());
    let _ = app.emit("agent:thinking", serde_json::json!({"status": "processing"}));

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
    let tool_descriptions = tool_defs.iter()
        .map(|d| {
            let params: Vec<String> = d.parameters.iter()
                .map(|p| format!("  - {}: {} ({}{})", p.name, p.description, p.param_type,
                    if p.required { ", required" } else { "" }))
                .collect();
            format!("### {}\n{}\nParameters:\n{}", d.name, d.description, params.join("\n"))
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    emit_agent_stage(
        &app,
        "tool_context_ready",
        "Tool descriptions prepared",
        Some(serde_json::json!({ "tool_count": tool_defs.len() })),
    );

    let user_name = memory_store.get_preference("user_name")
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
            let fact_lines: Vec<String> = facts.iter()
                .map(|f| format!("- {}", f.text))
                .collect();
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
        &tool_descriptions, &user_name, os_name, hw_tier, &pm_string_img, &memory_context,
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

    let recent_turns = memory_store.get_recent_turns(&session_id, 20).unwrap_or_default();

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
        content: user_text.clone(),
        name: None,
        images: Some(vec![ImageAttachment {
            data: b64,
            mime_type: mime_type.clone(),
        }]),
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
        if memory_store.get_preference(&title_key).unwrap_or(None).is_none() {
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
    let app_handle = app.clone();
    let session_id_clone = session_id.clone();
    let memory_store_clone = memory_store.clone();
    let embeddings_clone = state.embeddings.clone();
    let vectors_clone = state.vectors.clone();
    let user_message_clone = user_text.clone();

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

        emit_agent_stage(
            &app_handle,
            "awaiting_llm_output",
            "Image prompt sent to LLM; waiting for first response token",
            None,
        );

        while let Some(event) = event_rx.recv().await {
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
                    emit_agent_stage(
                        &app_handle,
                        "tool_started",
                        "Tool execution started",
                        Some(serde_json::json!({ "tool": name.clone() })),
                    );
                    let _ = app_handle.emit("agent:tool_call", serde_json::json!({ "name": name, "params": params }));
                }
                StreamEvent::ToolEnd { name, result, success } => {
                    emit_agent_stage(
                        &app_handle,
                        "tool_finished",
                        "Tool execution completed",
                        Some(serde_json::json!({
                            "tool": name.clone(),
                            "success": success,
                        })),
                    );
                    let payload = build_tool_result_event_payload(&name, &result, success);
                    let _ = app_handle.emit("agent:tool_result", payload);
                }
                StreamEvent::ApprovalRequired { request_id, action, risk_level, parameters } => {
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
                    let _ = app_handle.emit("agent:approval_result", serde_json::json!({ "action": action, "approved": approved }));
                }
                StreamEvent::Plan(plan) => {
                    emit_agent_stage(
                        &app_handle,
                        "planning",
                        "Agent is updating execution plan",
                        Some(serde_json::json!({ "plan": plan.clone() })),
                    );
                    let _ = app_handle.emit("agent:thinking", serde_json::json!({ "status": "planning", "plan": plan }));
                }
                StreamEvent::Error(err) => {
                    emit_agent_stage(
                        &app_handle,
                        "failed",
                        "Agent stream reported an error",
                        Some(serde_json::json!({ "error": err.clone() })),
                    );
                    let _ = app_handle.emit("agent:token", serde_json::json!({ "text": format!("⚠️ {err}") }));
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
                &memory_store_clone, &vectors_clone, &embeddings_clone,
            );
            match fact_mgr.extract_from_turn(&user_message_clone, &full_response) {
                Ok(ids) if !ids.is_empty() => {
                    tracing::info!(count = ids.len(), "auto-extracted facts from image conversation");
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
    });

    Ok(serde_json::json!({
        "status": "processing",
        "attachment": filename,
    }))
}

// ── MCP Server Management Commands ──────────────────────────────────

#[tauri::command]
pub async fn list_mcp_servers(
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let config = state.config.read().await;
    // Return configured servers plus their runtime status
    let servers: Vec<serde_json::Value> = config
        .mcp
        .servers
        .iter()
        .map(|s| {
            serde_json::json!({
                "name": s.name,
                "command": s.command,
                "args": s.args,
                "enabled": s.enabled,
                "trust_level": s.trust_level,
            })
        })
        .collect();
    Ok(serde_json::json!(servers))
}

#[tauri::command]
pub async fn add_mcp_server(
    name: String,
    command: String,
    args: Vec<String>,
    trust_level: Option<String>,
    state: State<'_, AppStateCell>,
) -> Result<(), String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
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
    Ok(())
}

#[tauri::command]
pub async fn remove_mcp_server(
    name: String,
    state: State<'_, AppStateCell>,
) -> Result<(), String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let mut config = state.config.write().await;
    let before = config.mcp.servers.len();
    config.mcp.servers.retain(|s| s.name != name);
    if config.mcp.servers.len() == before {
        return Err(format!("MCP server '{}' not found", name));
    }
    config.save().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn toggle_mcp_server(
    name: String,
    enabled: bool,
    state: State<'_, AppStateCell>,
) -> Result<(), String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let mut config = state.config.write().await;
    if let Some(server) = config.mcp.servers.iter_mut().find(|s| s.name == name) {
        server.enabled = enabled;
        config.save().map_err(|e| e.to_string())?;
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
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
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
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let mut config = state.config.write().await;
    config.telegram.enabled = enabled;
    config.telegram.bot_token = bot_token;
    config.telegram.allowed_chat_ids = allowed_chat_ids;
    config.telegram.auto_start = auto_start;
    config.save().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn start_telegram_mcp(
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let config = state.config.read().await;
    let tg_config = config.telegram.clone();
    drop(config);

    if tg_config.bot_token.is_empty() {
        return Err("Telegram bot token is not configured".into());
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
    );

    *state.telegram_bridge.write().await = Some(bridge);

    Ok(serde_json::json!({
        "status": "running",
        "message": "Telegram bridge started. Bot is now polling for messages.",
    }))
}

#[tauri::command]
pub async fn stop_telegram_mcp(
    state: State<'_, AppStateCell>,
) -> Result<(), String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
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
    config.save().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn test_telegram_connection(
    bot_token: String,
) -> Result<serde_json::Value, String> {
    // Test the bot token by calling getMe
    let url = format!("https://api.telegram.org/bot{}/getMe", bot_token);
    let client = reqwest::Client::new();
    let resp: reqwest::Response = client.get(&url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("Connection failed: {e}"))?;

    let body: serde_json::Value = resp.json::<serde_json::Value>().await
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
        let desc = body.get("description")
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
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
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
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
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
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let mut scheduler = state.scheduler.write().await;
    scheduler.remove_task(&task_id);
    Ok(())
}

#[tauri::command]
pub async fn list_macros(
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
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
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let mut recorder = state.macro_recorder.write().await;
    recorder.start_recording(&name);
    Ok(())
}

#[tauri::command]
pub async fn stop_macro_recording(
    description: String,
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
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
pub async fn delete_macro(
    name: String,
    state: State<'_, AppStateCell>,
) -> Result<(), String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let mut recorder = state.macro_recorder.write().await;
    if recorder.delete(&name) {
        Ok(())
    } else {
        Err(format!("Macro '{}' not found", name))
    }
}

#[tauri::command]
pub async fn list_workflows(
    state: State<'_, AppStateCell>,
) -> Result<serde_json::Value, String> {
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
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
    let state = state.get().ok_or_else(|| "KRIA is still initializing — please try again in a moment".to_string())?;
    let mut engine = state.workflow_engine.write().await;
    if engine.delete(&workflow_id) {
        Ok(())
    } else {
        Err(format!("Workflow '{}' not found", workflow_id))
    }
}

// ── Google Workspace Commands ────────────────────────────────────────────────

/// Return the OAuth connection status for a Google Workspace account.
/// Checks whether credentials.json and the account token file exist on disk.
#[tauri::command]
pub async fn get_google_workspace_status(
    account: Option<String>,
) -> Result<serde_json::Value, String> {
    let account = account
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| std::env::var("KRIA_GW_ACCOUNT").unwrap_or_else(|_| "personal".into()));
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    let config_dir = std::path::PathBuf::from(&home).join(".google-mcp");
    let token_path = config_dir.join("tokens").join(format!("{}.json", account));
    let credentials_path = config_dir.join("credentials.json");

    let connected = token_path.exists();
    let credentials_configured = credentials_path.exists();

    tracing::debug!(
        "[GW] status check: account='{}' connected={} creds={}",
        account, connected, credentials_configured
    );

    Ok(serde_json::json!({
        "connected": connected,
        "account": account,
        "credentials_configured": credentials_configured,
    }))
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
pub async fn connect_google_workspace(
    account: Option<String>,
    app_handle: AppHandle,
) -> Result<serde_json::Value, String> {
    let account = account
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| std::env::var("KRIA_GW_ACCOUNT").unwrap_or_else(|_| "personal".into()));
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    let config_dir = format!("{}/.google-mcp", home);

    // Fail fast if credentials.json is missing
    let creds_path = std::path::PathBuf::from(&config_dir).join("credentials.json");
    if !creds_path.exists() {
        return Err(
            "credentials.json not found at ~/.google-mcp/credentials.json. \
             Please add your Google Cloud OAuth client credentials first."
                .into(),
        );
    }

    let account_clone = account.clone();
    tokio::spawn(async move {
        tracing::info!("[GW] Starting OAuth flow for account '{}'", account_clone);
        let result = tokio::process::Command::new("npx")
            .args(["-y", "google-workspace-mcp", "accounts", "add", &account_clone])
            .env("GOOGLE_MCP_CONFIG_DIR", &config_dir)
            // inherit stdio so the process can open the browser
            .status()
            .await;

        match result {
            Ok(status) if status.success() => {
                tracing::info!("[GW] OAuth completed successfully for '{}'", account_clone);
                let _ = app_handle.emit("gw:connected", serde_json::json!({ "account": account_clone }));
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
        "message": "Browser opened for Google sign-in. Complete authorization and return here.",
    }))
}

/// Remove the OAuth token for a Google Workspace account (sign out).
#[tauri::command]
pub async fn disconnect_google_workspace(
    account: Option<String>,
) -> Result<(), String> {
    let account = account
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| std::env::var("KRIA_GW_ACCOUNT").unwrap_or_else(|_| "personal".into()));
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    let token_path = std::path::PathBuf::from(home)
        .join(".google-mcp")
        .join("tokens")
        .join(format!("{}.json", account));

    if token_path.exists() {
        std::fs::remove_file(&token_path)
            .map_err(|e| format!("Failed to remove token: {e}"))?;
        tracing::info!("[GW] Disconnected Google account '{}'", account);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::build_tool_result_event_payload;

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
}
