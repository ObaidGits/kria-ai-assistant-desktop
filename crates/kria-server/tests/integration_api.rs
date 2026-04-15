//! Integration tests for the KRIA server REST API.
//!
//! These tests spin up an ephemeral Axum server on a random port,
//! exercise every API route, and tear down cleanly — making them
//! fully idempotent with no database side-effects.

use axum::Router;
use reqwest::{Client, StatusCode};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;

// ── Test helpers ────────────────────────────────────────────────────

/// Build the full application router with a default config.
fn build_test_app() -> Router {
    use kria_core::config::KriaConfig;

    let state = Arc::new(kria_server::ServerState {
        config: KriaConfig::default(),
    });

    kria_server::build_router(state)
}

/// Start the test server on a random OS-assigned port and return its base URL.
async fn spawn_test_server() -> String {
    let app = build_test_app();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

// ── Health endpoint ─────────────────────────────────────────────────

#[tokio::test]
async fn health_returns_ok_with_version() {
    let base = spawn_test_server().await;
    let client = Client::new();

    let res = client.get(format!("{base}/api/health")).send().await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["status"], "healthy");
    assert!(body.get("version").is_some(), "response must include version");
}

// ── Chat endpoint ───────────────────────────────────────────────────

#[tokio::test]
async fn chat_accepts_valid_message() {
    let base = spawn_test_server().await;
    let client = Client::new();

    let payload = serde_json::json!({
        "message": "Hello, KRIA!",
    });

    let res = client
        .post(format!("{base}/api/chat"))
        .json(&payload)
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);

    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["status"], "received");
    assert_eq!(body["message"], "Hello, KRIA!");
    // A session_id must be auto-generated when none is provided
    assert!(body.get("session_id").is_some());
}

#[tokio::test]
async fn chat_preserves_explicit_session_id() {
    let base = spawn_test_server().await;
    let client = Client::new();

    let payload = serde_json::json!({
        "message": "test",
        "session_id": "my-session-42",
    });

    let res = client
        .post(format!("{base}/api/chat"))
        .json(&payload)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["session_id"], "my-session-42");
}

#[tokio::test]
async fn chat_rejects_missing_message_field() {
    let base = spawn_test_server().await;
    let client = Client::new();

    // Malformed request — no `message` field
    let payload = serde_json::json!({ "wrong_field": "oops" });

    let res = client
        .post(format!("{base}/api/chat"))
        .json(&payload)
        .send()
        .await
        .unwrap();

    // Axum returns 422 when JSON deserialization fails
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn chat_rejects_non_json_body() {
    let base = spawn_test_server().await;
    let client = Client::new();

    let res = client
        .post(format!("{base}/api/chat"))
        .header("content-type", "text/plain")
        .body("not json")
        .send()
        .await
        .unwrap();

    // Should be a 4xx error — either 415 or 422
    assert!(res.status().is_client_error());
}

// ── Sessions endpoint ───────────────────────────────────────────────

#[tokio::test]
async fn sessions_returns_empty_list() {
    let base = spawn_test_server().await;
    let client = Client::new();

    let res = client.get(format!("{base}/api/sessions")).send().await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body: serde_json::Value = res.json().await.unwrap();
    assert!(body.is_array());
    assert_eq!(body.as_array().unwrap().len(), 0);
}

// ── Models endpoint ─────────────────────────────────────────────────

#[tokio::test]
async fn models_returns_models_array() {
    let base = spawn_test_server().await;
    let client = Client::new();

    let res = client.get(format!("{base}/api/models")).send().await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body: serde_json::Value = res.json().await.unwrap();
    // The key must exist even if models dir is missing
    assert!(body.get("models").is_some());
}

// ── Settings endpoints ──────────────────────────────────────────────

#[tokio::test]
async fn get_settings_returns_full_config() {
    let base = spawn_test_server().await;
    let client = Client::new();

    let res = client.get(format!("{base}/api/settings")).send().await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body: serde_json::Value = res.json().await.unwrap();
    // Should contain top-level config sections
    assert!(body.get("llm").is_some(), "settings must include llm section");
    assert!(body.get("voice").is_some(), "settings must include voice section");
    assert!(body.get("memory").is_some(), "settings must include memory section");
    assert!(body.get("safety").is_some(), "settings must include safety section");
    assert!(body.get("server").is_some(), "settings must include server section");
    assert!(body.get("ui").is_some(), "settings must include ui section");
}

#[tokio::test]
async fn update_settings_returns_updated_status() {
    let base = spawn_test_server().await;
    let client = Client::new();

    let payload = serde_json::json!({
        "ui": { "theme": "light" },
    });

    let res = client
        .post(format!("{base}/api/settings"))
        .json(&payload)
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["status"], "updated");
}

// ── Broken API response simulation ──────────────────────────────────

#[tokio::test]
async fn nonexistent_route_returns_404() {
    let base = spawn_test_server().await;
    let client = Client::new();

    let res = client
        .get(format!("{base}/api/nonexistent"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

// ── CORS layer ──────────────────────────────────────────────────────

#[tokio::test]
async fn cors_allows_any_origin() {
    let base = spawn_test_server().await;
    let client = Client::new();

    let res = client
        .get(format!("{base}/api/health"))
        .header("Origin", "http://localhost:5173")
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    // CorsLayer::permissive() reflects the origin
    let cors = res.headers().get("access-control-allow-origin");
    assert!(cors.is_some(), "CORS header must be present");
}
