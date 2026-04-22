use crate::ServerState;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
    routing::get,
    Router,
};
use futures::{SinkExt, StreamExt};
use kria_core::infra::pipeline_trace::{log_pipeline_step, sanitize_text_for_logs};
use std::sync::Arc;

pub fn ws_routes() -> Router<Arc<ServerState>> {
    Router::new().route("/ws", get(ws_handler))
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
    if sender
        .send(Message::Text(welcome.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    // Main message loop
    while let Some(Ok(msg)) = receiver.next().await {
        match msg {
            Message::Text(text) => {
                let parsed: Result<serde_json::Value, _> = serde_json::from_str(&text);
                match parsed {
                    Ok(val) => {
                        let msg_type = val
                            .get("type")
                            .and_then(|t| t.as_str())
                            .unwrap_or("unknown");

                        match msg_type {
                            "chat" => {
                                let user_msg =
                                    val.get("message").and_then(|m| m.as_str()).unwrap_or("");
                                let session_id = val
                                    .get("session_id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("ws");

                                log_pipeline_step(
                                    session_id,
                                    "server_ws_chat_received",
                                    "WebSocket chat message received",
                                    Some(serde_json::json!({
                                        "message_preview": sanitize_text_for_logs(user_msg, 220),
                                    })),
                                );

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

                                log_pipeline_step(
                                    session_id,
                                    "server_ws_chat_done",
                                    "WebSocket chat stub response sent",
                                    Some(serde_json::json!({
                                        "reply_preview": sanitize_text_for_logs(
                                            &format!("Received: {user_msg}"),
                                            200,
                                        ),
                                    })),
                                );
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
