//! ComfyUI WebSocket progress bridge.
//!
//! Spawns a dedicated task that:
//! 1. Connects to `ws://127.0.0.1:{port}/ws?clientId={client_id}`
//! 2. Parses Comfy progress/status/executing/executed frames
//! 3. Fans them out to the Tauri app handle via `app.emit()`
//! 4. Resolves a `oneshot` sender when the final "executed" frame arrives
//!
//! Heartbeat: sends a `{"op":"ping"}` every 2 s; reconnects if no response
//! for 5 s. State is recovered via `GET /history/{prompt_id}` on reconnect.

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};
use futures::{SinkExt, StreamExt};

/// A type-erased event emitter — fulfilled by `Arc<tauri::AppHandle>` in the desktop
/// crate, or a no-op in tests / server builds.
pub type EventEmitter = Arc<dyn Fn(&str, serde_json::Value) + Send + Sync>;

/// Output path of a completed ComfyUI job.
#[derive(Debug, Clone)]
pub struct ComfyOutput {
    pub filename: String,
    pub subfolder: String,
    pub output_type: String,
}

#[derive(Debug, thiserror::Error)]
pub enum WsBridgeError {
    #[error("WebSocket connection failed: {0}")]
    Connect(String),
    #[error("Job timed out after {seconds}s")]
    Timeout { seconds: u64 },
    #[error("ComfyUI reported an error: {message}")]
    ComfyError { message: String },
    #[error("Cancelled")]
    Cancelled,
}

/// Spawn a WebSocket listener for a single ComfyUI job.
///
/// Returns a handle to the spawned task. Drop the handle to cancel
/// (ComfyUI is NOT interrupted — call `POST /interrupt` separately).
pub fn spawn_ws_listener(
    port: u16,
    client_id: String,
    prompt_id: String,
    emitter: Option<EventEmitter>,
    completion_tx: oneshot::Sender<Result<Vec<ComfyOutput>, WsBridgeError>>,
    cancel: tokio_util::sync::CancellationToken,
    generation_timeout: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let url = format!("ws://127.0.0.1:{}/ws?clientId={}", port, client_id);
        let deadline = tokio::time::Instant::now() + generation_timeout;
        let mut outputs: Vec<ComfyOutput> = Vec::new();
        let mut last_pong = tokio::time::Instant::now();

        // --- connect with retry ---
        let (ws_stream, _) = loop {
            if cancel.is_cancelled() {
                let _ = completion_tx.send(Err(WsBridgeError::Cancelled));
                return;
            }
            match connect_async(&url).await {
                Ok(pair) => break pair,
                Err(e) => {
                    warn!(error = %e, "WsBridge: connect failed, retrying in 500ms");
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        };

        let (mut write, mut read) = ws_stream.split();

        // Heartbeat task sends ping every 2 s.
        let cancel2 = cancel.clone();
        let ping_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(2));
            loop {
                tokio::select! {
                    _ = cancel2.cancelled() => break,
                    _ = interval.tick() => {
                        let msg = Message::Text(r#"{"op":"ping"}"#.to_string().into());
                        if write.send(msg).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });

        // Main receive loop.
        loop {
            // Hard wall-clock deadline.
            if tokio::time::Instant::now() >= deadline {
                ping_task.abort();
                let _ = completion_tx.send(Err(WsBridgeError::Timeout {
                    seconds: generation_timeout.as_secs(),
                }));
                return;
            }

            // Pong staleness check (5 s).
            if last_pong.elapsed() > Duration::from_secs(5) {
                warn!("WsBridge: no pong for 5s — treating as connection lost");
                // Attempt recovery via /history.
                ping_task.abort();
                let recovered = recover_from_history(port, &prompt_id).await;
                let _ = completion_tx.send(recovered);
                return;
            }

            let next = tokio::time::timeout_at(deadline, read.next()).await;
            match next {
                Ok(Some(Ok(msg))) => {
                    last_pong = tokio::time::Instant::now(); // any message counts as alive
                    match msg {
                        Message::Text(text) => {
                            handle_frame(
                                &text,
                                &prompt_id,
                                &emitter,
                                &mut outputs,
                                &completion_tx,
                                &ping_task,
                                &cancel,
                            ).await;
                            // If cancelled (job done or error), we're done.
                            if cancel.is_cancelled() {
                                ping_task.abort();
                                if outputs.is_empty() {
                                    let _ = completion_tx.send(Err(WsBridgeError::ComfyError {
                                        message: "job cancelled or error".into(),
                                    }));
                                } else {
                                    let _ = completion_tx.send(Ok(outputs));
                                }
                                return;
                            }
                        }
                        Message::Close(_) => {
                            debug!("WsBridge: server closed WS, recovering from history");
                            ping_task.abort();
                            let recovered = recover_from_history(port, &prompt_id).await;
                            let _ = completion_tx.send(recovered);
                            return;
                        }
                        _ => {}
                    }
                }
                Ok(Some(Err(e))) => {
                    warn!(error = %e, "WsBridge: read error");
                    ping_task.abort();
                    let recovered = recover_from_history(port, &prompt_id).await;
                    let _ = completion_tx.send(recovered);
                    return;
                }
                Ok(None) | Err(_) => {
                    // Stream ended or deadline.
                    ping_task.abort();
                    let recovered = recover_from_history(port, &prompt_id).await;
                    let _ = completion_tx.send(recovered);
                    return;
                }
            }

            if cancel.is_cancelled() {
                ping_task.abort();
                let _ = completion_tx.send(Err(WsBridgeError::Cancelled));
                return;
            }
        }
    })
}

// Inline async helper — avoids Box<dyn Future>.
async fn handle_frame(
    text: &str,
    prompt_id: &str,
    emitter: &Option<EventEmitter>,
    outputs: &mut Vec<ComfyOutput>,
    _tx: &oneshot::Sender<Result<Vec<ComfyOutput>, WsBridgeError>>,
    ping_task: &tokio::task::JoinHandle<()>,
    cancel: &tokio_util::sync::CancellationToken,
) {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(text) else { return };
    let msg_type = val.get("type").and_then(|v| v.as_str()).unwrap_or("");

    let emit = |event: &str, payload: serde_json::Value| {
        if let Some(e) = emitter { e(event, payload); }
    };

    match msg_type {
        "progress" => {
            let value = val["data"]["value"].as_u64().unwrap_or(0);
            let max = val["data"]["max"].as_u64().unwrap_or(1);
            let percent = (value * 100 / max.max(1)) as u32;
            emit("image:progress", serde_json::json!({
                "value": value,
                "max": max,
                "percent": percent,
            }));
            debug!(value, max, "ComfyUI progress");
        }
        "executing" => {
            let node = val["data"]["node"].as_str().unwrap_or("?");
            emit("image:stage", serde_json::json!({ "node": node }));
        }
        "executed" => {
            // Only handle the frame for our prompt_id.
            if val["data"]["prompt_id"].as_str() != Some(prompt_id) {
                return;
            }
            // Collect output files.
            if let Some(images) = val["data"]["output"]["images"].as_array() {
                for img in images {
                    outputs.push(ComfyOutput {
                        filename: img["filename"].as_str().unwrap_or("").to_string(),
                        subfolder: img["subfolder"].as_str().unwrap_or("").to_string(),
                        output_type: img["type"].as_str().unwrap_or("output").to_string(),
                    });
                }
            }
            info!(outputs = outputs.len(), prompt_id, "ComfyUI job completed via WS");
            ping_task.abort();
            cancel.cancel();
        }
        "status" => {
            let queue = val["data"]["status"]["exec_info"]["queue_remaining"]
                .as_u64()
                .unwrap_or(0);
            emit("image:queue", serde_json::json!({ "queue_remaining": queue }));
        }
        "error" => {
            let message = val["data"]["message"]
                .as_str()
                .unwrap_or("unknown ComfyUI error")
                .to_string();
            warn!(message = %message, "ComfyUI WS error frame");
            ping_task.abort();
            cancel.cancel();
        }
        _ => {}
    }
}

/// Public version of the history recovery — used by orchestrator when no AppHandle.
pub async fn recover_from_history_pub(
    port: u16,
    prompt_id: &str,
) -> Result<Vec<ComfyOutput>, WsBridgeError> {
    recover_from_history(port, prompt_id).await
}

/// Poll `GET /history/{prompt_id}` to recover job state after WS disconnect.
async fn recover_from_history(
    port: u16,
    prompt_id: &str,
) -> Result<Vec<ComfyOutput>, WsBridgeError> {
    let url = format!("http://127.0.0.1:{}/history/{}", port, prompt_id);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default();

    match client.get(&url).send().await {
        Ok(resp) => {
            let Ok(val) = resp.json::<serde_json::Value>().await else {
                return Err(WsBridgeError::ComfyError { message: "history parse failed".into() });
            };
            let mut outputs = Vec::new();
            if let Some(images) = val[prompt_id]["outputs"].as_object() {
                for node_output in images.values() {
                    if let Some(imgs) = node_output["images"].as_array() {
                        for img in imgs {
                            outputs.push(ComfyOutput {
                                filename: img["filename"].as_str().unwrap_or("").to_string(),
                                subfolder: img["subfolder"].as_str().unwrap_or("").to_string(),
                                output_type: img["type"].as_str().unwrap_or("output").to_string(),
                            });
                        }
                    }
                }
            }
            if outputs.is_empty() {
                Err(WsBridgeError::ComfyError { message: "no outputs in history".into() })
            } else {
                Ok(outputs)
            }
        }
        Err(e) => Err(WsBridgeError::ComfyError { message: e.to_string() }),
    }
}
