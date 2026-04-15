use kria_core::config::KriaConfig;
use kria_core::llm::{ChatMessage, ModelRouter};
use kria_core::safety::hitl::{HitlGateway, ApprovalResponse};
use std::sync::Arc;
use tauri::{AppHandle, Manager, State, Emitter};
use tokio::sync::RwLock;

/// Shared application state managed by Tauri.
pub struct AppState {
    pub config: Arc<RwLock<KriaConfig>>,
    pub model_router: Arc<ModelRouter>,
    pub hitl: Arc<HitlGateway>,
    pub voice_active: Arc<std::sync::atomic::AtomicBool>,
}

/// Initialize the KRIA runtime (called from setup).
pub async fn init_runtime(handle: &AppHandle) -> anyhow::Result<()> {
    let config = KriaConfig::load(None)?;

    // Initialize logging
    let paths = config.resolve_paths()?;
    kria_core::infra::logging::setup_logging(&paths.logs_dir);

    // Initialize memory store
    let _store = kria_core::memory::store::MemoryStore::open(&paths.db_path)?;

    // Initialize model router from config
    let model_router = Arc::new(ModelRouter::from_config(&config));

    tracing::info!("KRIA runtime initialized");

    // Store state in Tauri
    let hitl = Arc::new(HitlGateway::new(30));
    let state = AppState {
        config: Arc::new(RwLock::new(config)),
        model_router,
        hitl,
        voice_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
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

    let router = state.model_router.clone();

    // Spawn the LLM call so we can stream events back
    let app_handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let messages = vec![
            ChatMessage {
                role: "system".into(),
                content: "You are K.R.I.A., a helpful desktop AI assistant. Be concise and helpful.".into(),
                name: None,
            },
            ChatMessage {
                role: "user".into(),
                content: message.clone(),
                name: None,
            },
        ];

        match router.route("chat").await {
            Some(backend) => {
                tracing::info!("Routing to backend: {}", backend.model_label());
                match backend.chat(&messages, None, 0.7, 2048).await {
                    Ok(response) => {
                        tracing::info!("LLM response received ({} chars)", response.content.len());
                        let _ = app_handle.emit("agent:token", serde_json::json!({
                            "text": response.content
                        }));
                    }
                    Err(e) => {
                        tracing::error!("LLM error: {e}");
                        let _ = app_handle.emit("agent:token", serde_json::json!({
                            "text": format!("⚠️ LLM backend error: {e}\n\nMake sure llama-server is running on the configured port.")
                        }));
                    }
                }
            }
            None => {
                tracing::warn!("No LLM backend available");
                let _ = app_handle.emit("agent:token", serde_json::json!({
                    "text": "⚠️ No LLM backend connected.\n\nTo get responses, start a llama.cpp server:\n```\n./llama-server -m models/llm/microsoft_Phi-4-mini-instruct-Q4_K_M.gguf --host 127.0.0.1 --port 8080\n```\n\nOr set a cloud API key in Settings."
                }));
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
    _state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    Ok(vec![])
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
    _state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "status": "healthy",
        "uptime_secs": 0,
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
    let mut config = state.config.write().await;
    *config = new_config;
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
) -> Result<(), String> {
    state.voice_active.store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

#[tauri::command]
pub async fn stop_voice(
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.voice_active.store(false, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

#[tauri::command]
pub async fn get_voice_status(
    state: State<'_, AppState>,
) -> Result<bool, String> {
    Ok(state.voice_active.load(std::sync::atomic::Ordering::Relaxed))
}
