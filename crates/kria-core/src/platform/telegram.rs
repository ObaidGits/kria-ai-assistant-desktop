//! Telegram Bot bridge — polls for messages and routes them through the agent loop.
//!
//! Runs as a background task inside the desktop (or server) process.
//! No separate HTTP server needed — uses reqwest to talk to the Telegram Bot API
//! and the AgentLoop directly for processing.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::{mpsc, watch, RwLock};
use tracing;

use crate::agent::loop_engine::StreamEvent;
use crate::agent::AgentLoop;
use crate::config::TelegramConfig;
use crate::safety::hitl::ApprovalResponse;
use crate::llm::orchestrator::Orchestrator;
use crate::llm::ChatMessage;
use crate::memory::embeddings::EmbeddingModel;
use crate::memory::store::MemoryStore;
use crate::memory::vectors::VectorIndex;
use crate::platform::detect::get_available_package_managers;
use crate::tools::ToolRegistry;

const TELEGRAM_API: &str = "https://api.telegram.org/bot";
const TELEGRAM_CONFLICT_BASE_BACKOFF_SECS: u64 = 2;
const TELEGRAM_CONFLICT_MAX_BACKOFF_SECS: u64 = 90;
const TELEGRAM_CONFLICT_JITTER_MAX_MS: u64 = 1200;

/// Handle to a running Telegram bridge. Drop or call stop() to shut down.
pub struct TelegramBridge {
    shutdown_tx: watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl TelegramBridge {
    /// Spawn the Telegram polling loop as a background task.
    pub fn spawn(
        config: TelegramConfig,
        agent_loop: Arc<AgentLoop>,
        memory_store: Arc<MemoryStore>,
        tool_registry: Arc<ToolRegistry>,
        embeddings: Arc<EmbeddingModel>,
        vectors: Arc<VectorIndex>,
        hw_tier: String,
        orchestrator: Arc<RwLock<Option<Arc<Orchestrator>>>>,
    ) -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let task = tokio::spawn(async move {
            telegram_poll_loop(
                config,
                agent_loop,
                memory_store,
                tool_registry,
                embeddings,
                vectors,
                hw_tier,
                orchestrator,
                shutdown_rx,
            )
            .await;
        });

        Self {
            shutdown_tx,
            task: Some(task),
        }
    }

    /// Signal the polling loop to stop.
    pub fn stop(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    /// Check if the background task is still running.
    pub fn is_running(&self) -> bool {
        self.task.as_ref().is_some_and(|t| !t.is_finished())
    }
}

impl Drop for TelegramBridge {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Validate a bot token by calling getMe. Returns (bot_username, bot_id) on success.
pub async fn verify_bot_token(token: &str) -> Result<(String, i64), String> {
    let url = format!("{}{}/getMe", TELEGRAM_API, token);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    let resp: serde_json::Value = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("Connection failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("Invalid response: {e}"))?;

    if resp["ok"].as_bool() == Some(true) {
        let result = &resp["result"];
        let username = result["username"].as_str().unwrap_or("unknown").to_string();
        let id = result["id"].as_i64().unwrap_or(0);
        Ok((username, id))
    } else {
        let desc = resp
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or("unknown error");
        Err(format!("Invalid token: {desc}"))
    }
}

/// Send a text message to a Telegram chat.
async fn send_message(
    client: &reqwest::Client,
    token: &str,
    chat_id: i64,
    text: &str,
) -> Result<(), String> {
    let url = format!("{}{}/sendMessage", TELEGRAM_API, token);

    // Telegram limits messages to 4096 characters. Split if needed.
    let chunks = split_message(text, 4096);
    for chunk in chunks {
        let body = serde_json::json!({
            "chat_id": chat_id,
            "text": chunk,
            "parse_mode": "Markdown",
        });

        let resp = client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Send failed: {e}"))?;

        let result: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Bad response: {e}"))?;

        if result["ok"].as_bool() != Some(true) {
            // If Markdown parse fails, retry without parse_mode
            let body_plain = serde_json::json!({
                "chat_id": chat_id,
                "text": chunk,
            });
            let _ = client.post(&url).json(&body_plain).send().await;
        }
    }
    Ok(())
}

/// Split long messages into chunks that respect Telegram's 4096 char limit.
fn split_message(text: &str, max_len: usize) -> Vec<&str> {
    if text.len() <= max_len {
        return vec![text];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let end = (start + max_len).min(text.len());
        // Try to break at a newline
        let break_at = if end < text.len() {
            text[start..end]
                .rfind('\n')
                .map(|i| start + i + 1)
                .unwrap_or(end)
        } else {
            end
        };
        chunks.push(&text[start..break_at]);
        start = break_at;
    }
    chunks
}

/// Parse allowed chat IDs from a comma-separated string.
fn parse_allowed_chat_ids(s: &str) -> HashSet<i64> {
    s.split(',')
        .filter_map(|id| id.trim().parse::<i64>().ok())
        .collect()
}

fn is_get_updates_conflict(description: &str) -> bool {
    let normalized = description.trim().to_ascii_lowercase();
    normalized.contains("conflict")
        && normalized.contains("getupdates")
        && normalized.contains("only one bot instance")
}

fn conflict_backoff_base_secs(retry_count: u32) -> u64 {
    let exponent = retry_count.saturating_sub(1).min(6);
    (TELEGRAM_CONFLICT_BASE_BACKOFF_SECS.saturating_mul(1u64 << exponent))
        .min(TELEGRAM_CONFLICT_MAX_BACKOFF_SECS)
}

fn conflict_backoff_duration(retry_count: u32) -> Duration {
    let base_secs = conflict_backoff_base_secs(retry_count);
    let jitter_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_millis() as u64
        % TELEGRAM_CONFLICT_JITTER_MAX_MS;
    Duration::from_secs(base_secs) + Duration::from_millis(jitter_ms)
}

/// The main polling loop.
#[allow(clippy::too_many_arguments)]
async fn telegram_poll_loop(
    config: TelegramConfig,
    agent_loop: Arc<AgentLoop>,
    memory_store: Arc<MemoryStore>,
    tool_registry: Arc<ToolRegistry>,
    embeddings: Arc<EmbeddingModel>,
    vectors: Arc<VectorIndex>,
    hw_tier: String,
    orchestrator: Arc<RwLock<Option<Arc<Orchestrator>>>>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let token = &config.bot_token;
    if token.is_empty() {
        tracing::error!("Telegram bridge: bot token is empty, not starting");
        return;
    }

    let allowed_chats = parse_allowed_chat_ids(&config.allowed_chat_ids);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap();

    // Verify token
    match verify_bot_token(token).await {
        Ok((username, _id)) => {
            tracing::info!("Telegram bridge connected as @{username}");
        }
        Err(e) => {
            tracing::error!("Telegram bridge: invalid bot token: {e}");
            return;
        }
    }

    let mut last_update_id: i64 = 0;
    let mut conflict_retries: u32 = 0;

    // Use a dedicated session for Telegram conversations per chat
    loop {
        // Check for shutdown
        if *shutdown_rx.borrow() {
            tracing::info!("Telegram bridge shutting down");
            break;
        }

        let url = format!("{}{}/getUpdates", TELEGRAM_API, token);
        let body = serde_json::json!({
            "offset": last_update_id + 1,
            "limit": 10,
            "timeout": 30,
        });

        let updates_result = tokio::select! {
            result = client.post(&url).json(&body).send() => result,
            _ = shutdown_rx.changed() => {
                tracing::info!("Telegram bridge shutting down (signal during poll)");
                break;
            }
        };

        let updates = match updates_result {
            Ok(resp) => {
                match resp.json::<serde_json::Value>().await {
                    Ok(val) if val["ok"].as_bool() == Some(true) => {
                        conflict_retries = 0;
                        val["result"].as_array().cloned().unwrap_or_default()
                    }
                    Ok(val) => {
                        let description = val
                            .get("description")
                            .and_then(|d| d.as_str())
                            .unwrap_or("unknown telegram api error");
                        if is_get_updates_conflict(description) {
                            conflict_retries = conflict_retries.saturating_add(1);
                            let backoff = conflict_backoff_duration(conflict_retries);
                            tracing::warn!(
                                retry = conflict_retries,
                                backoff_secs = backoff.as_secs_f32(),
                                "Telegram getUpdates conflict: {}. Retrying with backoff.",
                                description
                            );

                            let shutdown_during_backoff = tokio::select! {
                                _ = tokio::time::sleep(backoff) => false,
                                _ = shutdown_rx.changed() => true,
                            };
                            if shutdown_during_backoff {
                                tracing::info!("Telegram bridge shutting down (signal during conflict backoff)");
                                break;
                            }
                            continue;
                        }
                        tracing::warn!("Telegram API error: {:?}", val.get("description"));
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!("Telegram response parse error: {e}");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        continue;
                    }
                }
            }
            Err(e) => {
                if e.is_timeout() {
                    // Normal for long polling
                    continue;
                }
                tracing::warn!("Telegram poll error: {e}");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        for update in &updates {
            if let Some(uid) = update["update_id"].as_i64() {
                last_update_id = uid;
            }

            let msg = &update["message"];
            let text = match msg["text"].as_str() {
                Some(t) if !t.is_empty() => t.to_string(),
                _ => continue,
            };

            let chat_id = match msg["chat"]["id"].as_i64() {
                Some(id) => id,
                None => continue,
            };

            let from_name = msg["from"]["first_name"]
                .as_str()
                .unwrap_or("User")
                .to_string();

            // Check allowed chats
            if !allowed_chats.is_empty() && !allowed_chats.contains(&chat_id) {
                tracing::warn!("Telegram: ignoring message from unauthorized chat {chat_id}");
                let _ = send_message(
                    &client,
                    token,
                    chat_id,
                    "⚠️ Unauthorized. Your chat ID is not in the allowed list.",
                )
                .await;
                continue;
            }

            // Handle /start command
            if text == "/start" {
                let welcome = format!(
                    "👋 Hello {from_name}! I'm KRIA, your AI assistant.\n\n\
                     Your chat ID is `{chat_id}` — add this to the allowed list in Settings.\n\n\
                     Just send me a message and I'll help!"
                );
                let _ = send_message(&client, token, chat_id, &welcome).await;
                continue;
            }

            // Handle /chatid command
            if text == "/chatid" {
                let _ = send_message(
                    &client,
                    token,
                    chat_id,
                    &format!("Your chat ID is: `{chat_id}`"),
                )
                .await;
                continue;
            }

            tracing::info!(
                "Telegram message from {from_name} (chat {chat_id}): {}",
                &text[..text.len().min(100)]
            );

            // Send "typing" indicator
            let typing_url = format!("{}{}/sendChatAction", TELEGRAM_API, token);
            let _ = client
                .post(&typing_url)
                .json(&serde_json::json!({
                    "chat_id": chat_id,
                    "action": "typing",
                }))
                .send()
                .await;

            // Process through agent loop
            // Read the current orchestrator snapshot — it may not be ready at bridge
            // spawn time but will be available once the background startup completes.
            let orc_snapshot = orchestrator.read().await.clone();
            // Anyone whose chat_id is in allowed_chats is the owner — the allowed_chats
            // gate above already rejected every other caller.  If allowed_chats is
            // empty the bot is unconfigured; treat callers as non-owner for safety.
            let is_owner = allowed_chats.contains(&chat_id);
            let reply = process_message(
                &text,
                chat_id,
                &from_name,
                &agent_loop,
                &memory_store,
                &tool_registry,
                &embeddings,
                &vectors,
                &hw_tier,
                orc_snapshot.as_ref(),
                is_owner,
            )
            .await;

            // Send reply
            if let Err(e) = send_message(&client, token, chat_id, &reply).await {
                tracing::error!("Failed to send Telegram reply: {e}");
            }
        }
    }
}

/// Process a single message through the full agent pipeline.
/// Process a single Telegram-style message through the full agent pipeline.
///
/// This is reused by the desktop's local HTTP bridge so external Telegram MCP
/// servers can forward incoming messages into the in-process agent loop.
#[allow(clippy::too_many_arguments)]
pub async fn process_message(
    text: &str,
    chat_id: i64,
    from_name: &str,
    agent_loop: &Arc<AgentLoop>,
    memory_store: &Arc<MemoryStore>,
    tool_registry: &Arc<ToolRegistry>,
    embeddings: &Arc<EmbeddingModel>,
    vectors: &Arc<VectorIndex>,
    hw_tier: &str,
    orchestrator: Option<&Arc<Orchestrator>>,
    // Whether the caller is the authenticated owner. Owner callers have their
    // HITL approval requests auto-approved so that Telegram commands are not
    // silently denied by a timeout. Non-owner callers receive an immediate
    // denial for any RED-tier action.
    is_owner: bool,
) -> String {
    // Build system prompt (similar to desktop send_message)
    let tool_descriptions = tool_registry
        .list_for_tier(hw_tier)
        .iter()
        .map(|d| {
            let params: Vec<String> = d
                .parameters
                .iter()
                .map(|p| {
                    format!(
                        "  - {}: {} ({}{})",
                        p.name,
                        p.description,
                        p.param_type,
                        if p.required { ", required" } else { "" }
                    )
                })
                .collect();
            format!(
                "### {}\n{}\nParameters:\n{}",
                d.name,
                d.description,
                params.join("\n")
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let user_name = memory_store
        .get_preference("user_name")
        .unwrap_or(None)
        .unwrap_or_else(|| from_name.to_string());
    let os_name = std::env::consts::OS;

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

    let memory_context = match memory_store.search_facts(text, 5) {
        Ok(facts) if !facts.is_empty() => {
            let fact_lines: Vec<String> = facts.iter().map(|f| format!("- {}", f.text)).collect();
            format!("Known facts about the user:\n{}", fact_lines.join("\n"))
        }
        _ => String::new(),
    };

    let system_prompt = crate::agent::prompts::build_system_prompt(
        &tool_descriptions,
        &user_name,
        os_name,
        hw_tier,
        &pm_string,
        &memory_context,
    );

    // Use a session per chat ID
    let session_id = format!("telegram_{}", chat_id);

    // Build messages with history
    let recent_turns = memory_store
        .get_recent_turns(&session_id, 10)
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
        content: text.to_string(),
        name: None,
        images: None,
    });

    // Persist user turn
    let _ = memory_store.store_turn(&crate::memory::store::ConversationTurn {
        id: None,
        session_id: session_id.clone(),
        role: "user".into(),
        content: text.to_string(),
        tool_name: None,
        tool_result: None,
        tokens_used: None,
        timestamp: chrono::Utc::now(),
    });

    // Ensure the local LLM runtime is up before entering the agent loop.
    // Without this, messages arriving while the model server is starting up
    // produce "local LLM transport error" instead of a real response.
    if let Some(orc) = orchestrator {
        const MAX_RETRIES: u32 = 2;
        let mut last_err = String::new();
        let mut ok = false;
        for attempt in 0..=MAX_RETRIES {
            match orc.ensure_ready("telegram_turn").await {
                Ok(()) => { ok = true; break; }
                Err(e) => {
                    last_err = e.to_string();
                    if attempt < MAX_RETRIES {
                        tracing::warn!(
                            attempt = attempt + 1,
                            "Telegram: LLM not ready yet ({last_err}); retrying in 3s"
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    }
                }
            }
        }
        if !ok {
            tracing::error!("Telegram: LLM runtime unavailable after retries: {last_err}");
            return format!(
                "⚠️ KRIA's local model is not ready right now. Please try again in a few seconds.\n\nDetails: {last_err}"
            );
        }
    }

    // Run agent loop and collect response
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<StreamEvent>();

    let agent = agent_loop.clone();
    let sid = session_id.clone();
    tokio::spawn(async move {
        agent.run(&sid, &mut messages, event_tx).await;
    });

    // Capture the HITL gateway before entering the event drain loop so we
    // can resolve pending approval requests that arrive while the agent task
    // is running.  Without this, every RED-tier tool call issued from Telegram
    // would silently time out and the user would never see an approval prompt.
    let hitl = agent_loop.hitl_gateway();

    let mut full_response = String::new();
    while let Some(event) = event_rx.recv().await {
        match event {
            StreamEvent::Token(t) => full_response.push_str(&t),
            StreamEvent::Done(final_text) => {
                if !final_text.is_empty() && full_response.is_empty() {
                    full_response = final_text;
                }
            }
            StreamEvent::Error(err) => {
                if full_response.is_empty() {
                    full_response = format!("⚠️ Error: {err}");
                }
            }
            StreamEvent::ToolStart { name, .. } => {
                tracing::debug!("Telegram agent: tool call {name}");
            }
            StreamEvent::ToolEnd { name, success, .. } => {
                tracing::debug!("Telegram agent: tool {name} done (success={success})");
            }
            // ── HITL approval ─────────────────────────────────────────────
            // The desktop path resolves this via the Tauri frontend modal.
            // In the Telegram path there is no GUI, so we resolve the oneshot
            // here before the gateway times it out.
            //
            // Owner callers: auto-approve — the user's own command IS the
            // approval intent (e.g. "run this script", "open chrome").
            //
            // Non-owner callers: auto-deny — RED-tier actions require the
            // account owner to authorise them.
            StreamEvent::ApprovalRequired {
                request_id,
                action,
                risk_level,
                ..
            } => {
                let decision = if is_owner {
                    tracing::info!(
                        request_id = %request_id,
                        action = %action,
                        risk_level = %risk_level,
                        chat_id = %chat_id,
                        "Telegram: auto-approving RED-tier action for owner"
                    );
                    ApprovalResponse::Approved
                } else {
                    tracing::warn!(
                        request_id = %request_id,
                        action = %action,
                        risk_level = %risk_level,
                        chat_id = %chat_id,
                        "Telegram: auto-denying RED-tier action for non-owner"
                    );
                    ApprovalResponse::Denied
                };
                // The agent loop sends the StreamEvent just BEFORE calling
                // request_approval_with_id(), which inserts the entry into the
                // pending map.  On a multi-threaded Tokio executor this event
                // drain loop can run on a different thread and call respond()
                // before the insert completes.  Retry with short back-off so we
                // don't accidentally let the 30-second gateway timeout fire.
                let mut resolved = false;
                for attempt in 0_u8..20 {
                    if hitl.respond(&request_id, decision.clone()).await {
                        resolved = true;
                        break;
                    }
                    if attempt < 19 {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                }
                if !resolved {
                    tracing::error!(
                        request_id = %request_id,
                        "Telegram: HITL request not found after 20 attempts — will time out"
                    );
                }
            }
            _ => {}
        }
    }

    if full_response.is_empty() {
        full_response = "I processed your request but have no text response.".to_string();
    }

    // Persist assistant turn
    let _ = memory_store.store_turn(&crate::memory::store::ConversationTurn {
        id: None,
        session_id: session_id.clone(),
        role: "assistant".into(),
        content: full_response.clone(),
        tool_name: None,
        tool_result: None,
        tokens_used: None,
        timestamp: chrono::Utc::now(),
    });

    // Extract facts
    let fact_mgr = crate::memory::facts::FactManager::new(memory_store, vectors, embeddings);
    match fact_mgr.extract_from_turn(text, &full_response) {
        Ok(ids) if !ids.is_empty() => {
            tracing::info!(
                count = ids.len(),
                "telegram: extracted facts from conversation"
            );
        }
        _ => {}
    }

    full_response
}

// ── Universal Inbox adapter shim ──────────────────────────────────────────────
//
// These implementations allow TelegramBridge to be used with the new
// platform/inbox pipeline while keeping backward compatibility with the
// existing polling loop above.

use crate::platform::inbox::{
    adapter::{DeliveryReceipt, EgressAdapter, IngressAdapter},
    AuthContext as InboxAuthContext, ConversationKey, InboundMessage, OutboundMessage, Participant,
    Platform as InboxPlatform,
};

/// Thin ingress shim — wraps the existing Telegram polling loop and converts
/// each incoming text message into a canonical [`InboundMessage`].
///
/// Phase-1 implementation: uses `allowed_chat_ids` from config to determine
/// owner status (first entry = owner).  Full speaker-verification and
/// contact-book lookup can be wired in later without changing the trait.
///
/// The adapter's only job is normalising Telegram updates into [`InboundMessage`]
/// envelopes and pushing them to the pipeline channel.  Processing dependencies
/// (agent loop, memory, tools) live downstream in the pipeline — not here.
pub struct TelegramIngressAdapter {
    config: crate::config::TelegramConfig,
    /// The first numeric ID in `allowed_chat_ids` is treated as the owner.
    owner_chat_id: Option<i64>,
}

impl TelegramIngressAdapter {
    pub fn new(config: crate::config::TelegramConfig) -> Self {
        let owner_chat_id = config
            .allowed_chat_ids
            .split(',')
            .find_map(|s| s.trim().parse::<i64>().ok());

        Self { config, owner_chat_id }
    }

    fn auth_for_chat(&self, chat_id: i64) -> InboxAuthContext {
        if Some(chat_id) == self.owner_chat_id {
            InboxAuthContext::owner()
        } else {
            // Any chat in the allowed list is "trusted"; everything else is External.
            let allowed: std::collections::HashSet<i64> =
                parse_allowed_chat_ids(&self.config.allowed_chat_ids);
            if allowed.contains(&chat_id) {
                InboxAuthContext::trusted()
            } else {
                InboxAuthContext::unknown()
            }
        }
    }
}

#[async_trait::async_trait]
impl IngressAdapter for TelegramIngressAdapter {
    fn platform_id(&self) -> &'static str {
        "telegram"
    }

    async fn run(
        self: Box<Self>,
        tx: tokio::sync::mpsc::Sender<InboundMessage>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) {
        let token = &self.config.bot_token;
        if token.is_empty() {
            tracing::error!("TelegramIngressAdapter: bot token empty, not starting");
            return;
        }

        let allowed_chats = parse_allowed_chat_ids(&self.config.allowed_chat_ids);
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap();

        let mut last_update_id: i64 = 0;

        loop {
            if *shutdown.borrow() {
                break;
            }

            let url = format!("{}{}/getUpdates", TELEGRAM_API, token);
            let body = serde_json::json!({
                "offset": last_update_id + 1,
                "limit": 10,
                "timeout": 30,
            });

            let updates_result = tokio::select! {
                r = client.post(&url).json(&body).send() => r,
                _ = shutdown.changed() => break,
            };

            let resp = match updates_result {
                Ok(r) => r,
                Err(e) if e.is_timeout() => continue,
                Err(e) => {
                    tracing::warn!("TelegramIngressAdapter: poll error: {e}");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };

            let val: serde_json::Value = match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("TelegramIngressAdapter: parse error: {e}");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };

            if val["ok"].as_bool() != Some(true) {
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }

            let updates = val["result"].as_array().cloned().unwrap_or_default();

            for update in &updates {
                if let Some(uid) = update["update_id"].as_i64() {
                    last_update_id = uid;
                }

                let msg = &update["message"];
                let text = match msg["text"].as_str() {
                    Some(t) if !t.is_empty() => t.to_string(),
                    _ => continue,
                };

                let chat_id = match msg["chat"]["id"].as_i64() {
                    Some(id) => id,
                    None => continue,
                };

                // Silently drop messages from disallowed chats (existing logic
                // in the old bridge sends a warning — keep that in the old path).
                if !allowed_chats.is_empty() && !allowed_chats.contains(&chat_id) {
                    continue;
                }

                let from_name = msg["from"]["first_name"]
                    .as_str()
                    .unwrap_or("User")
                    .to_string();
                let from_id = msg["from"]["id"]
                    .as_i64()
                    .map(|i| i.to_string())
                    .unwrap_or_else(|| chat_id.to_string());
                let native_id = msg["message_id"]
                    .as_i64()
                    .map(|i| i.to_string());

                let auth = self.auth_for_chat(chat_id);

                let mut envelope = InboundMessage::new(
                    ConversationKey {
                        platform: InboxPlatform::Telegram,
                        chat_id: chat_id.to_string(),
                    },
                    Participant {
                        id: from_id,
                        display_name: Some(from_name),
                        is_bot: msg["from"]["is_bot"].as_bool().unwrap_or(false),
                    },
                    auth,
                    SystemTime::now(),
                );
                envelope.text = Some(text);
                envelope.native_message_id = native_id;

                if tx.send(envelope).await.is_err() {
                    tracing::warn!("TelegramIngressAdapter: channel closed");
                    return;
                }
            }
        }

        tracing::info!("TelegramIngressAdapter: stopped");
    }
}

/// Thin egress shim — sends an [`OutboundMessage`] back to the Telegram chat.
pub struct TelegramEgressAdapter {
    token: String,
    client: reqwest::Client,
}

impl TelegramEgressAdapter {
    pub fn new(token: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap();
        Self {
            token: token.into(),
            client,
        }
    }
}

#[async_trait::async_trait]
impl EgressAdapter for TelegramEgressAdapter {
    fn platform_id(&self) -> &'static str {
        "telegram"
    }

    async fn send(&self, msg: OutboundMessage) -> DeliveryReceipt {
        let chat_id: i64 = match msg.conversation.chat_id.parse() {
            Ok(id) => id,
            Err(_) => {
                return DeliveryReceipt {
                    outbound_id: msg.id,
                    native_message_id: None,
                    delivered: false,
                    error: Some(format!(
                        "invalid chat_id '{}'",
                        msg.conversation.chat_id
                    )),
                };
            }
        };

        match send_message(&self.client, &self.token, chat_id, &msg.text).await {
            Ok(()) => DeliveryReceipt {
                outbound_id: msg.id,
                native_message_id: None, // Telegram sendMessage response not parsed here
                delivered: true,
                error: None,
            },
            Err(e) => DeliveryReceipt {
                outbound_id: msg.id,
                native_message_id: None,
                delivered: false,
                error: Some(e),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_get_updates_conflict_message() {
        let msg = "Conflict: terminated by other getUpdates request; make sure that only one bot instance is running";
        assert!(is_get_updates_conflict(msg));
    }

    #[test]
    fn ignores_non_conflict_messages() {
        assert!(!is_get_updates_conflict("Unauthorized"));
        assert!(!is_get_updates_conflict("Bad Request: chat not found"));
    }

    #[test]
    fn conflict_backoff_base_grows_and_caps() {
        assert_eq!(conflict_backoff_base_secs(1), 2);
        assert_eq!(conflict_backoff_base_secs(2), 4);
        assert_eq!(conflict_backoff_base_secs(3), 8);
        assert_eq!(conflict_backoff_base_secs(7), 90);
        assert_eq!(conflict_backoff_base_secs(20), 90);
    }

    #[test]
    fn conflict_backoff_duration_has_bounded_jitter() {
        let d = conflict_backoff_duration(1);
        let base = Duration::from_secs(conflict_backoff_base_secs(1));
        let max = base + Duration::from_millis(TELEGRAM_CONFLICT_JITTER_MAX_MS - 1);
        assert!(d >= base);
        assert!(d <= max);
    }
}
