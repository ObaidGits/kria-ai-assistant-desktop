use axum::{
    Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
    routing::get,
};
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use crate::ServerState;

pub fn ws_routes() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/ws", get(ws_handler))
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<ServerState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, _state: Arc<ServerState>) {
    let (mut sender, mut receiver) = socket.split();

    // Send welcome message
    let welcome = serde_json::json!({
        "type": "connected",
        "version": env!("CARGO_PKG_VERSION"),
    });
    if sender.send(Message::Text(welcome.to_string().into())).await.is_err() {
        return;
    }

    // Main message loop
    while let Some(Ok(msg)) = receiver.next().await {
        match msg {
            Message::Text(text) => {
                let parsed: Result<serde_json::Value, _> = serde_json::from_str(&text);
                match parsed {
                    Ok(val) => {
                        let msg_type = val.get("type")
                            .and_then(|t| t.as_str())
                            .unwrap_or("unknown");

                        match msg_type {
                            "chat" => {
                                let user_msg = val.get("message")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("");

                                // Stream agent response events
                                let ack = serde_json::json!({
                                    "type": "ack",
                                    "message": user_msg,
                                });
                                let _ = sender.send(Message::Text(ack.to_string().into())).await;

                                // TODO: integrate with AgentLoop and stream events
                                let done = serde_json::json!({
                                    "type": "done",
                                    "text": format!("Received: {user_msg}"),
                                });
                                let _ = sender.send(Message::Text(done.to_string().into())).await;
                            }
                            "approve" | "deny" => {
                                // HITL approval/denial
                                let resp = serde_json::json!({
                                    "type": "hitl_ack",
                                    "action": msg_type,
                                });
                                let _ = sender.send(Message::Text(resp.to_string().into())).await;
                            }
                            "ping" => {
                                let pong = serde_json::json!({"type": "pong"});
                                let _ = sender.send(Message::Text(pong.to_string().into())).await;
                            }
                            _ => {
                                let err = serde_json::json!({
                                    "type": "error",
                                    "message": format!("unknown message type: {msg_type}"),
                                });
                                let _ = sender.send(Message::Text(err.to_string().into())).await;
                            }
                        }
                    }
                    Err(e) => {
                        let err = serde_json::json!({
                            "type": "error",
                            "message": format!("invalid JSON: {e}"),
                        });
                        let _ = sender.send(Message::Text(err.to_string().into())).await;
                    }
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
}
