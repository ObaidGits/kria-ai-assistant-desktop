use kria_core::config::KriaConfig;
use kria_core::llm::{ChatMessage, ImageAttachment, ModelRouter};
use kria_core::safety::hitl::{HitlGateway, ApprovalResponse};
use kria_core::safety::{PolicyEngine, AuditLogger, RollbackManager};
use kria_core::agent::AgentLoop;
use kria_core::agent::loop_engine::StreamEvent;
use kria_core::tools::registry::{self, ToolRegistry};
use kria_core::memory::MemoryStore;
use kria_core::memory::store::ConversationTurn;
use kria_core::memory::embeddings::EmbeddingModel;
use kria_core::memory::vectors::VectorIndex;
use kria_core::infra::EventBus;
use kria_core::infra::health::{HealthRegistry, ServiceStatus};
use kria_core::automation::{AutomationScheduler, MacroRecorder, WorkflowEngine};
use kria_core::platform::detect::{HardwareInfo, HardwareTier, detect_hardware};
use kria_core::sidecar::SidecarBridge;
use kria_core::voice::{VoicePipeline, VoicePipelineState, VoicePipelineEvent, SpeechToText, TextToSpeech};
use std::sync::Arc;
use chrono::Utc;
use tauri::{AppHandle, Manager, State, Emitter};
use tokio::sync::RwLock;

/// Find a binary on the system PATH.
fn which_binary(name: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join(name))
                .find(|p| p.exists())
        })
}

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
}

/// Initialize the KRIA runtime (called from setup).
pub async fn init_runtime(handle: &AppHandle) -> anyhow::Result<()> {
    let config = KriaConfig::load(None)?;

    // Initialize logging
    let paths = config.resolve_paths()?;
    kria_core::infra::logging::setup_logging(&paths.logs_dir);

    // Detect hardware tier (with config/env override)
    let mut hw_info = detect_hardware();
    // Allow override via env KRIA_TIER or config [hardware] tier
    if let Ok(env_tier) = std::env::var("KRIA_TIER") {
        if !env_tier.is_empty() {
            hw_info.tier = HardwareTier::from_str(&env_tier);
            tracing::info!(tier = %env_tier, "hardware tier overridden by KRIA_TIER env");
        }
    } else if !config.hardware.tier.is_empty() {
        hw_info.tier = HardwareTier::from_str(&config.hardware.tier);
        tracing::info!(tier = %config.hardware.tier, "hardware tier overridden by config");
    }
    // Cache hardware info to JSON
    let hw_cache_path = paths.data_dir.join("hardware_tier.json");
    if let Ok(json) = serde_json::to_string_pretty(&hw_info) {
        let _ = std::fs::write(&hw_cache_path, json);
    }
    let hardware_info = Arc::new(hw_info);
    tracing::info!(
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
    let mut tool_registry_inner = registry::build_registry_full(
        Some(memory_store.clone()), Some(rag_engine.clone()), Some(proactive_engine.clone()),
    );
    kria_core::tools::precognitive::register(&mut tool_registry_inner, sidecar.clone());
    // Re-register vision tools with sidecar (overrides the None-sidecar registration from build_registry)
    kria_core::tools::vision::register(&mut tool_registry_inner, Some(sidecar.clone()));
    let tool_registry = Arc::new(tool_registry_inner);
    tracing::info!(tools = tool_registry.len(), "tool registry loaded (with RAG + precognitive + knowledge)");

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
    let agent_loop = Arc::new(AgentLoop::new(
        model_router.clone(),
        tool_registry.clone(),
        policy_engine,
        hitl.clone(),
        audit_logger,
        rollback_mgr,
    ));

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
    let state = AppState {
        config,
        model_router,
        agent_loop,
        tool_registry,
        memory_store,
        hitl,
        event_bus,
        sidecar,
        embeddings,
        vectors,
        current_session_id: Arc::new(RwLock::new(uuid::Uuid::new_v4().to_string())),
        voice_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        voice_pipeline,
        health,
        scheduler: scheduler_arc,
        macro_recorder: macro_recorder_arc,
        workflow_engine: workflow_engine_arc,
        started_at: std::time::Instant::now(),
        hardware_info,
        proactive: proactive_engine,
    };

    handle.manage(state);
    Ok(())
}

#[tauri::command]
pub async fn send_message(
    message: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<serde_json::Value, String> {
    tracing::info!("User message: {}", &message);

    let _ = app.emit("agent:thinking", serde_json::json!({"status": "processing"}));

    let agent_loop = state.agent_loop.clone();
    let memory_store = state.memory_store.clone();
    let tool_registry = state.tool_registry.clone();
    let event_bus = state.event_bus.clone();
    let config = state.config.read().await;
    let hw_tier = state.hardware_info.tier.as_str();

    // Build the system prompt with tool descriptions and user context
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

    // Retrieve user context from memory
    let user_name = memory_store.get_preference("user_name")
        .unwrap_or(None)
        .unwrap_or_else(|| "User".to_string());
    let os_name = std::env::consts::OS;

    // Get recent memory facts for context injection
    let memory_context = match memory_store.search_facts(&message, 5) {
        Ok(facts) if !facts.is_empty() => {
            let fact_lines: Vec<String> = facts.iter()
                .map(|f| format!("- {}", f.text))
                .collect();
            format!("Known facts about the user:\n{}", fact_lines.join("\n"))
        }
        _ => String::new(),
    };

    let system_prompt = kria_core::agent::prompts::build_system_prompt(
        &tool_descriptions,
        &user_name,
        os_name,
        hw_tier,
        &memory_context,
    );

    drop(config);

    // Use the persistent session ID from AppState
    let session_id = state.current_session_id.read().await.clone();

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

    // Spawn event consumer that forwards to frontend
    tauri::async_runtime::spawn(async move {
        let mut full_response = String::new();

        while let Some(event) = event_rx.recv().await {
            match event {
                StreamEvent::Token(text) => {
                    full_response.push_str(&text);
                    let _ = app_handle.emit("agent:token", serde_json::json!({
                        "text": text,
                    }));
                }
                StreamEvent::ToolStart { name, params } => {
                    tracing::info!("Tool call: {} with {:?}", name, params);
                    let _ = app_handle.emit("agent:tool_call", serde_json::json!({
                        "name": name,
                        "params": params,
                    }));
                }
                StreamEvent::ToolEnd { name, result, success } => {
                    tracing::info!("Tool result: {} success={}", name, success);
                    let _ = app_handle.emit("agent:tool_result", serde_json::json!({
                        "name": name,
                        "result": result,
                        "success": success,
                    }));
                }
                StreamEvent::ApprovalRequired { request_id, action, risk_level } => {
                    let _ = app_handle.emit("agent:approval_required", serde_json::json!({
                        "request_id": request_id,
                        "action": action,
                        "risk_level": risk_level,
                    }));
                }
                StreamEvent::ApprovalResult { action, approved } => {
                    let _ = app_handle.emit("agent:approval_result", serde_json::json!({
                        "action": action,
                        "approved": approved,
                    }));
                }
                StreamEvent::Plan(plan) => {
                    let _ = app_handle.emit("agent:thinking", serde_json::json!({
                        "status": "planning",
                        "plan": plan,
                    }));
                }
                StreamEvent::Error(err) => {
                    tracing::error!("Agent error: {}", err);
                    let _ = app_handle.emit("agent:token", serde_json::json!({
                        "text": format!("⚠️ {err}"),
                    }));
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

            // Automatic fact extraction from user message + assistant response
            let fact_mgr = kria_core::memory::facts::FactManager::new(
                &memory_store_clone,
                &vectors_clone,
                &embeddings_clone,
            );
            match fact_mgr.extract_from_turn(&user_message_clone, &full_response) {
                Ok(ids) if !ids.is_empty() => {
                    tracing::info!(count = ids.len(), "auto-extracted facts from conversation");
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("fact extraction failed: {}", e),
            }
        }

        let _ = app_handle.emit("agent:done", serde_json::json!({}));
    });

    Ok(serde_json::json!({
        "status": "processing",
    }))
}

#[tauri::command]
pub async fn get_session_history(
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
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
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
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
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
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
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
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
    state: State<'_, AppState>,
) -> Result<(), String> {
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
    state: State<'_, AppState>,
) -> Result<(), String> {
    let key = format!("session_title:{}", session_id);
    state.memory_store.set_preference(&key, &title)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn search_sessions(
    query: String,
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
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
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.hitl.cancel_all().await;
    Ok(())
}

#[tauri::command]
pub async fn approve_action(
    request_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.hitl.respond(&request_id, ApprovalResponse::Approved).await;
    Ok(())
}

#[tauri::command]
pub async fn deny_action(
    request_id: String,
    _reason: Option<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.hitl.respond(&request_id, ApprovalResponse::Denied).await;
    Ok(())
}

#[tauri::command]
pub async fn get_health(
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
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
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
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
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let config = state.config.read().await;
    serde_json::to_value(&*config).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn update_settings(
    settings: serde_json::Value,
    state: State<'_, AppState>,
) -> Result<(), String> {
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
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
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
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
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
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let config = state.config.read().await;
    let paths = config.resolve_paths().map_err(|e| e.to_string())?;
    let mgr = kria_core::llm::model_manager::ModelManager::new(paths.models_dir.join("llm"));
    let models = mgr.list_llm_models();
    Ok(serde_json::to_value(&models).unwrap_or_default())
}

#[tauri::command]
pub async fn start_voice(
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
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
                    let memory_context = match memory_store.search_facts(&text, 5) {
                        Ok(facts) if !facts.is_empty() => {
                            let lines: Vec<String> = facts.iter().map(|f| format!("- {}", f.text)).collect();
                            format!("Known facts about the user:\n{}", lines.join("\n"))
                        }
                        _ => String::new(),
                    };

                    let system_prompt = kria_core::agent::prompts::build_system_prompt(
                        &tool_descriptions, &user_name, os_name, hw_tier, &memory_context,
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
                                let _ = app2.emit("agent:tool_result", serde_json::json!({"name": name, "result": result, "success": success}));
                            }
                            StreamEvent::ApprovalRequired { request_id, action, risk_level } => {
                                let _ = app2.emit("agent:approval_required", serde_json::json!({"request_id": request_id, "action": action, "risk_level": risk_level}));
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
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    state.voice_active.store(false, std::sync::atomic::Ordering::Relaxed);
    state.voice_pipeline.stop().await;
    let _ = app.emit("voice:state", serde_json::json!({ "state": "idle" }));
    Ok(())
}

#[tauri::command]
pub async fn get_voice_status(
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
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
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<serde_json::Value, String> {
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

    // Encode to base64 for the LLM
    let b64 = kria_core::preprocessing::image::ImageProcessor::to_base64(&filepath)
        .map_err(|e| e.to_string())?;

    let user_text = text.unwrap_or_else(|| "What's in this image?".into());
    let _ = app.emit("agent:thinking", serde_json::json!({"status": "processing"}));

    let agent_loop = state.agent_loop.clone();
    let memory_store = state.memory_store.clone();
    let tool_registry = state.tool_registry.clone();
    let event_bus = state.event_bus.clone();
    let config = state.config.read().await;
    let hw_tier = state.hardware_info.tier.as_str();

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

    let memory_context = match memory_store.search_facts(&user_text, 5) {
        Ok(facts) if !facts.is_empty() => {
            let fact_lines: Vec<String> = facts.iter()
                .map(|f| format!("- {}", f.text))
                .collect();
            format!("Known facts about the user:\n{}", fact_lines.join("\n"))
        }
        _ => String::new(),
    };

    let system_prompt = kria_core::agent::prompts::build_system_prompt(
        &tool_descriptions, &user_name, os_name, hw_tier, &memory_context,
    );
    drop(config);

    let session_id = state.current_session_id.read().await.clone();
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

    // Event consumer (same as send_message)
    tauri::async_runtime::spawn(async move {
        let mut full_response = String::new();
        while let Some(event) = event_rx.recv().await {
            match event {
                StreamEvent::Token(text) => {
                    full_response.push_str(&text);
                    let _ = app_handle.emit("agent:token", serde_json::json!({ "text": text }));
                }
                StreamEvent::ToolStart { name, params } => {
                    let _ = app_handle.emit("agent:tool_call", serde_json::json!({ "name": name, "params": params }));
                }
                StreamEvent::ToolEnd { name, result, success } => {
                    let _ = app_handle.emit("agent:tool_result", serde_json::json!({ "name": name, "result": result, "success": success }));
                }
                StreamEvent::ApprovalRequired { request_id, action, risk_level } => {
                    let _ = app_handle.emit("agent:approval_required", serde_json::json!({ "request_id": request_id, "action": action, "risk_level": risk_level }));
                }
                StreamEvent::ApprovalResult { action, approved } => {
                    let _ = app_handle.emit("agent:approval_result", serde_json::json!({ "action": action, "approved": approved }));
                }
                StreamEvent::Plan(plan) => {
                    let _ = app_handle.emit("agent:thinking", serde_json::json!({ "status": "planning", "plan": plan }));
                }
                StreamEvent::Error(err) => {
                    let _ = app_handle.emit("agent:token", serde_json::json!({ "text": format!("⚠️ {err}") }));
                }
                StreamEvent::Done(final_text) => {
                    if !final_text.is_empty() && full_response.is_empty() {
                        full_response = final_text;
                    }
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
            let fact_mgr = kria_core::memory::facts::FactManager::new(
                &memory_store_clone, &vectors_clone, &embeddings_clone,
            );
            match fact_mgr.extract_from_turn(&user_message_clone, &full_response) {
                Ok(ids) if !ids.is_empty() => {
                    tracing::info!(count = ids.len(), "auto-extracted facts from image conversation");
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("fact extraction failed: {}", e),
            }
        }
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
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
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
    state: State<'_, AppState>,
) -> Result<(), String> {
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
    state: State<'_, AppState>,
) -> Result<(), String> {
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
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut config = state.config.write().await;
    if let Some(server) = config.mcp.servers.iter_mut().find(|s| s.name == name) {
        server.enabled = enabled;
        config.save().map_err(|e| e.to_string())?;
        Ok(())
    } else {
        Err(format!("MCP server '{}' not found", name))
    }
}

// ── Automation Commands ─────────────────────────────────────────────

#[tauri::command]
pub async fn list_scheduled_tasks(
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
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
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
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
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut scheduler = state.scheduler.write().await;
    scheduler.remove_task(&task_id);
    Ok(())
}

#[tauri::command]
pub async fn list_macros(
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
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
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut recorder = state.macro_recorder.write().await;
    recorder.start_recording(&name);
    Ok(())
}

#[tauri::command]
pub async fn stop_macro_recording(
    description: String,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
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
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut recorder = state.macro_recorder.write().await;
    if recorder.delete(&name) {
        Ok(())
    } else {
        Err(format!("Macro '{}' not found", name))
    }
}

#[tauri::command]
pub async fn list_workflows(
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
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
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut engine = state.workflow_engine.write().await;
    if engine.delete(&workflow_id) {
        Ok(())
    } else {
        Err(format!("Workflow '{}' not found", workflow_id))
    }
}
