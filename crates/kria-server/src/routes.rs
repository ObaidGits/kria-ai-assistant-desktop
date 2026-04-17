use axum::{
    Router,
    Json,
    extract::State,
    routing::{get, post},
};
use std::sync::Arc;
use crate::ServerState;

pub fn api_routes() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/chat", post(chat))
        .route("/api/sessions", get(list_sessions))
        .route("/api/models", get(list_models))
        .route("/api/settings", get(get_settings))
        .route("/api/settings", post(update_settings))
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "healthy",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

#[derive(serde::Deserialize)]
struct ChatRequest {
    message: String,
    session_id: Option<String>,
    /// Source of the message (e.g. "telegram", "web")
    #[serde(default)]
    source: Option<String>,
    /// Telegram chat ID (when source = "telegram")
    #[serde(default)]
    chat_id: Option<i64>,
    /// Sender name
    #[serde(default)]
    from_user: Option<String>,
}

async fn chat(
    State(_state): State<Arc<ServerState>>,
    Json(req): Json<ChatRequest>,
) -> Json<serde_json::Value> {
    // TODO: In production, this routes to the AgentLoop and returns the response.
    // For now, return a structured response that the Telegram MCP server can parse.
    Json(serde_json::json!({
        "status": "received",
        "message": req.message,
        "source": req.source.unwrap_or_else(|| "api".to_string()),
        "chat_id": req.chat_id,
        "from_user": req.from_user,
        "session_id": req.session_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        "reply": format!("I received your message: \"{}\"", req.message),
    }))
}

async fn list_sessions() -> Json<Vec<serde_json::Value>> {
    Json(vec![])
}

async fn list_models(
    State(state): State<Arc<ServerState>>,
) -> Json<serde_json::Value> {
    let paths = match state.config.resolve_paths() {
        Ok(p) => p,
        Err(_) => return Json(serde_json::json!({"models": []})),
    };
    let mgr = kria_core::llm::model_manager::ModelManager::new(paths.models_dir.join("llm"));
    let models = mgr.list_llm_models();
    Json(serde_json::json!({ "models": models }))
}

async fn get_settings(
    State(state): State<Arc<ServerState>>,
) -> Json<serde_json::Value> {
    Json(serde_json::to_value(&state.config).unwrap_or_default())
}

async fn update_settings(
    State(_state): State<Arc<ServerState>>,
    Json(_settings): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    // In production: validate and persist to config file
    Json(serde_json::json!({ "status": "updated" }))
}
