//! Integration tests for the KRIA WebSocket endpoint.
//!
//! Spins up an ephemeral server, connects via WS, and exercises every
//! message type: chat, approve, deny, ping, unknown, and invalid JSON.
//! Fully idempotent — no state mutated between runs.

use futures::{SinkExt, StreamExt};
use kria_core::config::KriaConfig;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// Spin up a test server, return the WebSocket URL.
async fn spawn_ws_server() -> String {
    let state = Arc::new(kria_server::ServerState {
        config: KriaConfig::default(),
    });
    let app = kria_server::build_router(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("ws://{addr}/ws")
}

/// Read the next text message or panic.
async fn next_text(
    stream: &mut (impl StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin),
) -> serde_json::Value {
    loop {
        match stream.next().await {
            Some(Ok(Message::Text(t))) => return serde_json::from_str(&t).unwrap(),
            Some(Ok(_)) => continue, // skip non-text frames
            other => panic!("unexpected ws frame: {other:?}"),
        }
    }
}

// ── Connection lifecycle ────────────────────────────────────────────

#[tokio::test]
async fn ws_sends_welcome_on_connect() {
    let url = spawn_ws_server().await;
    let (ws, _) = connect_async(&url).await.unwrap();
    let (_sink, mut stream) = ws.split();

    let welcome = next_text(&mut stream).await;
    assert_eq!(welcome["type"], "connected");
    assert!(welcome.get("version").is_some());
}

// ── Chat messages ───────────────────────────────────────────────────

#[tokio::test]
async fn ws_chat_returns_ack_then_done() {
    let url = spawn_ws_server().await;
    let (ws, _) = connect_async(&url).await.unwrap();
    let (mut sink, mut stream) = ws.split();

    // consume welcome
    let _ = next_text(&mut stream).await;

    let msg = serde_json::json!({ "type": "chat", "message": "Hello!" });
    sink.send(Message::Text(msg.to_string().into())).await.unwrap();

    let ack = next_text(&mut stream).await;
    assert_eq!(ack["type"], "ack");
    assert_eq!(ack["message"], "Hello!");

    let done = next_text(&mut stream).await;
    assert_eq!(done["type"], "done");
    assert!(done["text"].as_str().unwrap().contains("Hello!"));
}

// ── HITL approve/deny ───────────────────────────────────────────────

#[tokio::test]
async fn ws_approve_returns_hitl_ack() {
    let url = spawn_ws_server().await;
    let (ws, _) = connect_async(&url).await.unwrap();
    let (mut sink, mut stream) = ws.split();

    let _ = next_text(&mut stream).await; // welcome

    let msg = serde_json::json!({ "type": "approve", "request_id": "abc-123" });
    sink.send(Message::Text(msg.to_string().into())).await.unwrap();

    let resp = next_text(&mut stream).await;
    assert_eq!(resp["type"], "hitl_ack");
    assert_eq!(resp["action"], "approve");
}

#[tokio::test]
async fn ws_deny_returns_hitl_ack() {
    let url = spawn_ws_server().await;
    let (ws, _) = connect_async(&url).await.unwrap();
    let (mut sink, mut stream) = ws.split();

    let _ = next_text(&mut stream).await; // welcome

    let msg = serde_json::json!({ "type": "deny", "request_id": "abc-123", "reason": "too risky" });
    sink.send(Message::Text(msg.to_string().into())).await.unwrap();

    let resp = next_text(&mut stream).await;
    assert_eq!(resp["type"], "hitl_ack");
    assert_eq!(resp["action"], "deny");
}

// ── Ping ────────────────────────────────────────────────────────────

#[tokio::test]
async fn ws_ping_returns_pong() {
    let url = spawn_ws_server().await;
    let (ws, _) = connect_async(&url).await.unwrap();
    let (mut sink, mut stream) = ws.split();

    let _ = next_text(&mut stream).await; // welcome

    let ping = serde_json::json!({ "type": "ping" });
    sink.send(Message::Text(ping.to_string().into())).await.unwrap();

    let pong = next_text(&mut stream).await;
    assert_eq!(pong["type"], "pong");
}

// ── Edge cases ──────────────────────────────────────────────────────

#[tokio::test]
async fn ws_unknown_type_returns_error() {
    let url = spawn_ws_server().await;
    let (ws, _) = connect_async(&url).await.unwrap();
    let (mut sink, mut stream) = ws.split();

    let _ = next_text(&mut stream).await; // welcome

    let bad = serde_json::json!({ "type": "foobar" });
    sink.send(Message::Text(bad.to_string().into())).await.unwrap();

    let err = next_text(&mut stream).await;
    assert_eq!(err["type"], "error");
    assert!(err["message"].as_str().unwrap().contains("unknown message type"));
}

#[tokio::test]
async fn ws_invalid_json_returns_error() {
    let url = spawn_ws_server().await;
    let (ws, _) = connect_async(&url).await.unwrap();
    let (mut sink, mut stream) = ws.split();

    let _ = next_text(&mut stream).await; // welcome

    sink.send(Message::Text("not valid json {{{".into())).await.unwrap();

    let err = next_text(&mut stream).await;
    assert_eq!(err["type"], "error");
    assert!(err["message"].as_str().unwrap().contains("invalid JSON"));
}
