//! Google Workspace tools — hybrid MCP + sidecar handlers.
//!
//! Architecture: tools are ALWAYS registered in the ToolRegistry so the LLM
//! can see them regardless of whether the MCP server is connected.  The actual
//! MCP connection is held in a lazy `GwClientRef` (Arc<RwLock<Option<…>>>).
//! Once the gworkspace MCP server starts successfully, `init_runtime` populates
//! that ref via `set_client()`.  Until then, every handler returns a clear
//! "not connected" message rather than panicking or silently failing.
//!
//! Mount groups:
//!   ambient: gw_gmail_inbox, gw_gmail_search, gw_gmail_read,
//!            gw_calendar_today, gw_calendar_search,
//!            gw_drive_search, gw_drive_list, gw_drive_read
//!   docs:    gw_docs_read, gw_docs_create, gw_docs_edit,
//!            gw_sheets_read, gw_sheets_create, gw_sheets_edit,
//!            gw_slides_read, gw_slides_create
//!   admin:   gw_gmail_send, gw_gmail_delete,
//!            gw_drive_delete, gw_calendar_create, gw_calendar_delete

use std::sync::Arc;
use async_trait::async_trait;
use crate::infra::ToolResult;
use crate::mcp::McpClient;
use crate::sidecar::SidecarBridge;
use crate::safety::RiskLevel;
use crate::tools::registry::{ToolRegistry, ToolDef, ToolHandler, ParamDef};

/// Lazy reference to the gworkspace MCP client.
/// Starts as None; populated by `set_client()` once the MCP server connects.
pub type GwClientRef = Arc<tokio::sync::RwLock<Option<Arc<McpClient>>>>;

/// Create an empty lazy client reference (call `set_client()` later).
pub fn new_client_ref() -> GwClientRef {
    Arc::new(tokio::sync::RwLock::new(None))
}

/// Wire in the live MCP client after the server starts.
pub async fn set_client(gw_ref: &GwClientRef, client: Arc<McpClient>) {
    tracing::info!("[GW] wiring live McpClient into GwClientRef");
    *gw_ref.write().await = Some(client);
}

fn param(name: &str, ty: &str, desc: &str, required: bool) -> ParamDef {
    ParamDef {
        name: name.into(),
        param_type: ty.into(),
        description: desc.into(),
        required,
        default: None,
    }
}

/// Shared handles for hybrid MCP + sidecar calls.
#[derive(Clone)]
struct GwBridge {
    /// Lazy client ref — None until the gworkspace MCP server connects.
    mcp: GwClientRef,
    sidecar: Arc<SidecarBridge>,
    /// Google account name as configured via `npx google-workspace-mcp accounts add <name>`.
    /// Injected into every MCP call as the `account` parameter.
    account: String,
}

impl GwBridge {
    /// Inject the `account` field into the params object (required by every tool in
    /// `google-workspace-mcp`), then call the MCP tool.
    async fn mcp_call(&self, tool: &str, mut args: serde_json::Value) -> ToolResult {
        // Ensure args is an object
        if !args.is_object() {
            args = serde_json::json!({});
        }
        // Inject account — never overwrite if the caller already set it
        if let Some(obj) = args.as_object_mut() {
            obj.entry("account").or_insert_with(|| serde_json::json!(self.account));
        }

        tracing::info!("[GW] mcp_call: tool='{}' account='{}'", tool, self.account);
        tracing::debug!("[GW] mcp_call args: {}", args);

        let guard = self.mcp.read().await;
        let client = match guard.as_ref() {
            Some(c) => c.clone(),
            None => {
                let msg = format!(
                    "Google Workspace is not connected. \
                     Run: npx google-workspace-mcp accounts add personal  \
                     Then restart KRIA. (tool={tool})"
                );
                tracing::warn!("[GW] {}", msg);
                return ToolResult { success: false, data: serde_json::Value::Null, error: Some(msg) };
            }
        };
        drop(guard);

        match client.call_tool(tool, Some(args)).await {
            Ok(result) => {
                let text: String = result.content.iter()
                    .filter_map(|c| c.text.as_deref())
                    .collect::<Vec<_>>()
                    .join("\n");
                if result.is_error {
                    tracing::warn!("[GW] tool '{}' returned MCP error: {}", tool, &text[..text.len().min(300)]);
                    // Parse well-known Google API errors into concise, actionable messages.
                    let user_error = parse_gw_error(&text);
                    ToolResult { success: false, data: serde_json::json!(user_error), error: Some(user_error) }
                } else {
                    tracing::info!("[GW] tool '{}' succeeded ({} chars)", tool, text.len());
                    ToolResult { success: true, data: serde_json::json!(text), error: None }
                }
            }
            Err(e) => {
                tracing::error!("[GW] tool '{}' call error: {}", tool, e);
                ToolResult {
                    success: false,
                    data: serde_json::Value::Null,
                    error: Some(format!("MCP call failed: {e}")),
                }
            }
        }
    }

    /// Fetch-then-buffer: MCP call → raw data → sidecar digest.
    async fn fetch_and_buffer(
        &self,
        mcp_tool: &str,
        mcp_args: serde_json::Value,
        sidecar_method: &str,
    ) -> ToolResult {
        tracing::debug!("[GW] fetch_and_buffer: mcp_tool={} sidecar={}", mcp_tool, sidecar_method);
        let raw_result = self.mcp_call(mcp_tool, mcp_args).await;
        if !raw_result.success {
            return raw_result;
        }
        let buffer_params = serde_json::json!({ "raw": raw_result.data });
        match self.sidecar.request(sidecar_method, buffer_params).await {
            Ok(digest) => {
                tracing::info!("[GW] sidecar '{}' digest produced", sidecar_method);
                ToolResult { success: true, data: digest, error: None }
            }
            Err(e) => {
                tracing::warn!("[GW] sidecar '{}' failed ({}), returning raw", sidecar_method, e);
                raw_result
            }
        }
    }
}

// ── Error helpers ──────────────────────────────────────────────────────────────

/// Convert verbose Google API error messages into short, actionable strings.
///
/// Google errors for "API not enabled" are typically hundreds of characters long
/// and contain a URL to fix the issue.  This function extracts the key info so
/// KRIA's UI shows something readable.
fn parse_gw_error(raw: &str) -> String {
    // "accessNotConfigured" / "API has not been used" — API disabled in Cloud Console
    if raw.contains("accessNotConfigured") || raw.contains("has not been used") || raw.contains("is disabled") {
        // Try to extract the enable URL
        let url = raw
            .split_once("https://console")
            .map(|(_, rest)| format!("https://console{}", rest.split_whitespace().next().unwrap_or("")))
            .unwrap_or_default();
        // Determine which API from the URL or the error text
        let api_name = if raw.contains("gmail") { "Gmail API" }
            else if raw.contains("calendar") { "Calendar API" }
            else if raw.contains("drive") { "Drive API" }
            else if raw.contains("docs") { "Docs API" }
            else if raw.contains("sheets") { "Sheets API" }
            else if raw.contains("slides") { "Slides API" }
            else { "Google API" };
        if url.is_empty() {
            return format!(
                "{api_name} is disabled in your Google Cloud project. \
                 Enable it at https://console.cloud.google.com/apis/library then restart KRIA."
            );
        }
        return format!(
            "{api_name} is disabled. Enable it at {url} \
             (wait ~1 min after enabling, then retry or restart KRIA)."
        );
    }

    // "invalid_grant" / token expired / revoked
    if raw.contains("invalid_grant") || raw.contains("Token has been expired") || raw.contains("Token has been revoked") {
        return "Google authentication token expired or revoked. \
                Re-run: bash scripts/setup_google_workspace.sh  then restart KRIA.".into();
    }

    // "insufficientPermissions" — scope missing
    if raw.contains("insufficientPermissions") || raw.contains("Request had insufficient authentication scopes") {
        return "Insufficient OAuth scopes. \
                Re-run: bash scripts/setup_google_workspace.sh  to refresh permissions, then restart KRIA.".into();
    }

    // "rateLimitExceeded" / quota
    if raw.contains("rateLimitExceeded") || raw.contains("quotaExceeded") {
        return "Google API rate limit or quota exceeded. Wait a minute and try again.".into();
    }

    // Fallback: trim to 300 chars
    if raw.len() > 300 {
        format!("{}…", &raw[..300])
    } else {
        raw.to_string()
    }
}

// ── Gmail tools ────────────────────────────────────────────────────────────────
// Real tool names from `google-workspace-mcp` package (v2.x):
//   listGmailMessages, searchGmail, readGmailMessage,
//   createGmailDraft, sendGmailDraft, deleteGmailMessage

struct GwGmailInbox(GwBridge);
#[async_trait]
impl ToolHandler for GwGmailInbox {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        // listGmailMessages: account, maxResults?, query?
        // Returns a list — pass directly to sidecar for structured summarization.
        let mut args = serde_json::json!({ "maxResults": 20 });
        if let Some(q) = params.get("query").and_then(|v| v.as_str()) {
            args["query"] = serde_json::json!(q);
        }
        // Use fetch_and_buffer so the sidecar can structure the message list;
        // sidecar fallback returns raw text automatically if it fails.
        self.0.fetch_and_buffer("listGmailMessages", args, "google.summarize_email_thread").await
    }
}

struct GwGmailSearch(GwBridge);
#[async_trait]
impl ToolHandler for GwGmailSearch {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        // searchGmail: account, query, maxResults?
        let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let args = serde_json::json!({ "query": query, "maxResults": 20 });
        self.0.fetch_and_buffer("searchGmail", args, "google.summarize_email_thread").await
    }
}

struct GwGmailRead(GwBridge);
#[async_trait]
impl ToolHandler for GwGmailRead {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        // readGmailMessage: account, messageId
        let msg_id = params.get("message_id").and_then(|v| v.as_str()).unwrap_or("");
        let args = serde_json::json!({ "messageId": msg_id });
        self.0.fetch_and_buffer("readGmailMessage", args, "google.summarize_email_thread").await
    }
}

struct GwGmailSend(GwBridge);
#[async_trait]
impl ToolHandler for GwGmailSend {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        // Safe send workflow: create draft first, then send it
        // Step 1: createGmailDraft
        let draft_args = serde_json::json!({
            "to": params.get("to").and_then(|v| v.as_str()).unwrap_or(""),
            "subject": params.get("subject").and_then(|v| v.as_str()).unwrap_or(""),
            "body": params.get("body").and_then(|v| v.as_str()).unwrap_or(""),
            "cc": params.get("cc").and_then(|v| v.as_str()).unwrap_or(""),
        });
        let draft_result = self.0.mcp_call("createGmailDraft", draft_args).await;
        if !draft_result.success {
            return draft_result;
        }
        // Extract draft_id from result (JSON string containing draftId field)
        let draft_text = draft_result.data.as_str().unwrap_or("");
        tracing::info!("[GW] draft created: {}", &draft_text[..draft_text.len().min(200)]);
        // Step 2: sendGmailDraft using the draft_id
        // The result text from createGmailDraft typically contains the draft ID
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(draft_text) {
            if let Some(draft_id) = parsed.get("draftId").and_then(|v| v.as_str()) {
                return self.0.mcp_call("sendGmailDraft", serde_json::json!({ "draftId": draft_id })).await;
            }
        }
        // Fallback: return the draft result and let user know to send manually
        tracing::warn!("[GW] could not extract draftId from createGmailDraft response — draft created but not sent");
        ToolResult {
            success: false,
            data: draft_result.data,
            error: Some("Draft created but could not auto-send: draftId not found in response. Check Gmail drafts.".into()),
        }
    }
}

struct GwGmailDelete(GwBridge);
#[async_trait]
impl ToolHandler for GwGmailDelete {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let msg_id = params.get("message_id").and_then(|v| v.as_str()).unwrap_or("");
        self.0.mcp_call("deleteGmailMessage", serde_json::json!({ "messageId": msg_id })).await
    }
}

// ── Calendar tools ─────────────────────────────────────────────────────────────
// Real tool names: listCalendarEvents, createCalendarEvent, deleteCalendarEvent

struct GwCalendarToday(GwBridge);
#[async_trait]
impl ToolHandler for GwCalendarToday {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        // listCalendarEvents with today's date range
        let now = chrono::Utc::now();
        let start = now.date_naive().and_hms_opt(0, 0, 0).unwrap()
            .and_utc().to_rfc3339();
        let end = now.date_naive().and_hms_opt(23, 59, 59).unwrap()
            .and_utc().to_rfc3339();
        let args = serde_json::json!({ "timeMin": start, "timeMax": end, "maxResults": 50 });
        self.0.mcp_call("listCalendarEvents", args).await
    }
}

struct GwCalendarSearch(GwBridge);
#[async_trait]
impl ToolHandler for GwCalendarSearch {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let mut args = serde_json::json!({ "maxResults": 20 });
        if let Some(q) = params.get("query").and_then(|v| v.as_str()) {
            args["q"] = serde_json::json!(q);
        }
        if let Some(t) = params.get("time_min").and_then(|v| v.as_str()) {
            args["timeMin"] = serde_json::json!(t);
        }
        if let Some(t) = params.get("time_max").and_then(|v| v.as_str()) {
            args["timeMax"] = serde_json::json!(t);
        }
        self.0.mcp_call("listCalendarEvents", args).await
    }
}

struct GwCalendarCreate(GwBridge);
#[async_trait]
impl ToolHandler for GwCalendarCreate {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let args = serde_json::json!({
            "summary": params.get("summary").and_then(|v| v.as_str()).unwrap_or(""),
            "start": { "dateTime": params.get("start").and_then(|v| v.as_str()).unwrap_or("") },
            "end": { "dateTime": params.get("end").and_then(|v| v.as_str()).unwrap_or("") },
            "description": params.get("description").and_then(|v| v.as_str()).unwrap_or(""),
            "location": params.get("location").and_then(|v| v.as_str()).unwrap_or(""),
        });
        self.0.mcp_call("createCalendarEvent", args).await
    }
}

struct GwCalendarDelete(GwBridge);
#[async_trait]
impl ToolHandler for GwCalendarDelete {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let event_id = params.get("event_id").and_then(|v| v.as_str()).unwrap_or("");
        self.0.mcp_call("deleteCalendarEvent", serde_json::json!({ "eventId": event_id })).await
    }
}

// ── Drive tools ────────────────────────────────────────────────────────────────
// Real tool names: searchGoogleDocs, listFolderContents, readGoogleDoc, deleteFile

struct GwDriveSearch(GwBridge);
#[async_trait]
impl ToolHandler for GwDriveSearch {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let args = serde_json::json!({ "query": query });
        self.0.fetch_and_buffer("searchGoogleDocs", args, "google.summarize_drive_folder").await
    }
}

struct GwDriveList(GwBridge);
#[async_trait]
impl ToolHandler for GwDriveList {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let folder_id = params.get("folder_id").and_then(|v| v.as_str());
        let args = if let Some(id) = folder_id {
            serde_json::json!({ "folderId": id })
        } else {
            serde_json::json!({})
        };
        self.0.fetch_and_buffer("listFolderContents", args, "google.summarize_drive_folder").await
    }
}

struct GwDriveRead(GwBridge);
#[async_trait]
impl ToolHandler for GwDriveRead {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        // Try as a Doc first; format=text is safe for all readable files
        let file_id = params.get("file_id").and_then(|v| v.as_str()).unwrap_or("");
        let args = serde_json::json!({ "documentId": file_id, "format": "text" });
        self.0.mcp_call("readGoogleDoc", args).await
    }
}

struct GwDriveDelete(GwBridge);
#[async_trait]
impl ToolHandler for GwDriveDelete {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let file_id = params.get("file_id").and_then(|v| v.as_str()).unwrap_or("");
        self.0.mcp_call("deleteFile", serde_json::json!({ "fileId": file_id })).await
    }
}

// ── Docs tools ─────────────────────────────────────────────────────────────────
// Real tool names: readGoogleDoc, createDocument, appendToGoogleDoc

struct GwDocsRead(GwBridge);
#[async_trait]
impl ToolHandler for GwDocsRead {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let doc_id = params.get("document_id").and_then(|v| v.as_str()).unwrap_or("");
        let args = serde_json::json!({ "documentId": doc_id, "format": "markdown" });
        self.0.fetch_and_buffer("readGoogleDoc", args, "google.extract_doc").await
    }
}

struct GwDocsCreate(GwBridge);
#[async_trait]
impl ToolHandler for GwDocsCreate {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let title = params.get("title").and_then(|v| v.as_str()).unwrap_or("Untitled");
        self.0.mcp_call("createDocument", serde_json::json!({ "title": title })).await
    }
}

struct GwDocsEdit(GwBridge);
#[async_trait]
impl ToolHandler for GwDocsEdit {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let doc_id = params.get("document_id").and_then(|v| v.as_str()).unwrap_or("");
        let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
        // Default to append operation
        self.0.mcp_call("appendToGoogleDoc", serde_json::json!({ "documentId": doc_id, "text": text })).await
    }
}

// ── Sheets tools ───────────────────────────────────────────────────────────────
// Real tool names: readSpreadsheet, createSpreadsheet, writeSpreadsheet

struct GwSheetsRead(GwBridge);
#[async_trait]
impl ToolHandler for GwSheetsRead {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let id = params.get("spreadsheet_id").and_then(|v| v.as_str()).unwrap_or("");
        let mut args = serde_json::json!({ "spreadsheetId": id });
        if let Some(r) = params.get("range").and_then(|v| v.as_str()) {
            args["range"] = serde_json::json!(r);
        }
        self.0.fetch_and_buffer("readSpreadsheet", args, "google.extract_sheet").await
    }
}

struct GwSheetsCreate(GwBridge);
#[async_trait]
impl ToolHandler for GwSheetsCreate {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let title = params.get("title").and_then(|v| v.as_str()).unwrap_or("Untitled");
        self.0.mcp_call("createSpreadsheet", serde_json::json!({ "title": title })).await
    }
}

struct GwSheetsEdit(GwBridge);
#[async_trait]
impl ToolHandler for GwSheetsEdit {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let id = params.get("spreadsheet_id").and_then(|v| v.as_str()).unwrap_or("");
        let range = params.get("range").and_then(|v| v.as_str()).unwrap_or("A1");
        let values_str = params.get("values").and_then(|v| v.as_str()).unwrap_or("[]");
        let values: serde_json::Value = serde_json::from_str(values_str).unwrap_or(serde_json::json!([]));
        self.0.mcp_call("writeSpreadsheet", serde_json::json!({
            "spreadsheetId": id, "range": range, "values": values
        })).await
    }
}

// ── Slides tools ───────────────────────────────────────────────────────────────
// Real tool names: readPresentation, createPresentation

struct GwSlidesRead(GwBridge);
#[async_trait]
impl ToolHandler for GwSlidesRead {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let id = params.get("presentation_id").and_then(|v| v.as_str()).unwrap_or("");
        self.0.fetch_and_buffer("readPresentation", serde_json::json!({ "presentationId": id }), "google.extract_slides").await
    }
}

struct GwSlidesCreate(GwBridge);
#[async_trait]
impl ToolHandler for GwSlidesCreate {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let title = params.get("title").and_then(|v| v.as_str()).unwrap_or("Untitled");
        self.0.mcp_call("createPresentation", serde_json::json!({ "title": title })).await
    }
}

// ── Registration ───────────────────────────────────────────────────────────────

/// Register all Google Workspace tools.
///
/// Always registers all 16 tools regardless of whether the MCP server is up.
/// Pass the `GwClientRef` returned by `new_client_ref()`; call `set_client()`
/// after the MCP server connects so handlers start forwarding requests.
///
/// `account` is the name given to the Google account when running
/// `npx google-workspace-mcp accounts add <account>` (e.g. "personal").
pub fn register(
    reg: &ToolRegistry,
    mcp_ref: GwClientRef,
    sidecar: Arc<SidecarBridge>,
) {
    // Default account name — user sets this up via CLI before starting KRIA
    let account = std::env::var("KRIA_GW_ACCOUNT").unwrap_or_else(|_| "personal".into());
    tracing::info!("[GW] registering Google Workspace tools (account='{}', lazy MCP ref)", account);

    let gw = GwBridge { mcp: mcp_ref, sidecar, account };

    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        // ── Ambient (always mounted) ─────────────────────
        (
            ToolDef {
                name: "gw_gmail_inbox".into(),
                description: "List recent emails from Gmail inbox. USE THIS to check inbox, see recent mail, or list emails. Returns sender, subject, date, and snippet for each message.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Green,
                min_tier: "standard",
                parameters: vec![
                    param("query", "string", "Optional Gmail search filter (e.g. 'is:unread')", false),
                ],
            },
            Arc::new(GwGmailInbox(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_gmail_search".into(),
                description: "Search Gmail with a query string (same syntax as Gmail search bar). Use for filtering by sender, subject, label, date, etc.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Green,
                min_tier: "standard",
                parameters: vec![
                    param("query", "string", "Gmail search query (e.g. 'from:boss subject:report')", true),
                ],
            },
            Arc::new(GwGmailSearch(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_gmail_read".into(),
                description: "Read the FULL content of a single Gmail message. Requires the message_id obtained from gw_gmail_inbox or gw_gmail_search. Do NOT use this to list or check inbox.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Green,
                min_tier: "standard",
                parameters: vec![
                    param("message_id", "string", "Gmail message ID", true),
                ],
            },
            Arc::new(GwGmailRead(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_calendar_today".into(),
                description: "Get today's calendar events from Google Calendar.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Green,
                min_tier: "standard",
                parameters: vec![],
            },
            Arc::new(GwCalendarToday(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_calendar_search".into(),
                description: "Search Google Calendar events by keyword or date range.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Green,
                min_tier: "standard",
                parameters: vec![
                    param("query", "string", "Search text for event titles/descriptions", false),
                    param("time_min", "string", "Start of time range (ISO 8601)", false),
                    param("time_max", "string", "End of time range (ISO 8601)", false),
                ],
            },
            Arc::new(GwCalendarSearch(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_drive_search".into(),
                description: "Search Google Drive files by name or content.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Green,
                min_tier: "standard",
                parameters: vec![
                    param("query", "string", "Search query (supports Drive search operators)", true),
                ],
            },
            Arc::new(GwDriveSearch(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_drive_list".into(),
                description: "List files in a Google Drive folder.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Green,
                min_tier: "standard",
                parameters: vec![
                    param("folder_id", "string", "Drive folder ID (omit for root)", false),
                ],
            },
            Arc::new(GwDriveList(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_drive_read".into(),
                description: "Read content of a Google Drive file / Google Doc by ID.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Green,
                min_tier: "standard",
                parameters: vec![
                    param("file_id", "string", "Google Drive file or Google Doc ID", true),
                ],
            },
            Arc::new(GwDriveRead(gw.clone())),
        ),

        // ── Docs group (on-demand mount) ─────────────────
        (
            ToolDef {
                name: "gw_docs_read".into(),
                description: "Read a Google Doc by ID (markdown format).".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Green,
                min_tier: "standard",
                parameters: vec![
                    param("document_id", "string", "Google Docs document ID", true),
                ],
            },
            Arc::new(GwDocsRead(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_docs_create".into(),
                description: "Create a new Google Doc.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Yellow,
                min_tier: "standard",
                parameters: vec![
                    param("title", "string", "Document title", true),
                ],
            },
            Arc::new(GwDocsCreate(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_docs_edit".into(),
                description: "Append text to an existing Google Doc.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Yellow,
                min_tier: "standard",
                parameters: vec![
                    param("document_id", "string", "Google Docs document ID", true),
                    param("text", "string", "Text to append", true),
                ],
            },
            Arc::new(GwDocsEdit(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_sheets_read".into(),
                description: "Read a Google Spreadsheet by ID.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Green,
                min_tier: "standard",
                parameters: vec![
                    param("spreadsheet_id", "string", "Google Sheets spreadsheet ID", true),
                    param("range", "string", "Cell range like 'Sheet1!A1:D10' (optional)", false),
                ],
            },
            Arc::new(GwSheetsRead(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_sheets_create".into(),
                description: "Create a new Google Spreadsheet.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Yellow,
                min_tier: "standard",
                parameters: vec![
                    param("title", "string", "Spreadsheet title", true),
                ],
            },
            Arc::new(GwSheetsCreate(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_sheets_edit".into(),
                description: "Write data to a Google Sheet range.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Yellow,
                min_tier: "standard",
                parameters: vec![
                    param("spreadsheet_id", "string", "Google Sheets spreadsheet ID", true),
                    param("range", "string", "Target cell range (e.g. 'Sheet1!A1:C3')", true),
                    param("values", "string", "JSON array of row arrays, e.g. [[\"a\",\"b\"],[\"c\",\"d\"]]", true),
                ],
            },
            Arc::new(GwSheetsEdit(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_slides_read".into(),
                description: "Read a Google Slides presentation by ID.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Green,
                min_tier: "standard",
                parameters: vec![
                    param("presentation_id", "string", "Google Slides presentation ID", true),
                ],
            },
            Arc::new(GwSlidesRead(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_slides_create".into(),
                description: "Create a new Google Slides presentation.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Yellow,
                min_tier: "standard",
                parameters: vec![
                    param("title", "string", "Presentation title", true),
                ],
            },
            Arc::new(GwSlidesCreate(gw.clone())),
        ),

        // ── Admin group (on-demand mount) ────────────────
        (
            ToolDef {
                name: "gw_gmail_send".into(),
                description: "Send an email via Gmail (creates draft then sends). Requires HITL approval.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Red,
                min_tier: "standard",
                parameters: vec![
                    param("to", "string", "Recipient email address", true),
                    param("subject", "string", "Email subject line", true),
                    param("body", "string", "Email body (plain text)", true),
                    param("cc", "string", "CC recipients (comma-separated)", false),
                ],
            },
            Arc::new(GwGmailSend(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_gmail_delete".into(),
                description: "Delete a Gmail message. Requires HITL approval.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Red,
                min_tier: "standard",
                parameters: vec![
                    param("message_id", "string", "Gmail message ID to delete", true),
                ],
            },
            Arc::new(GwGmailDelete(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_drive_delete".into(),
                description: "Delete a file from Google Drive. Requires HITL approval.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Red,
                min_tier: "standard",
                parameters: vec![
                    param("file_id", "string", "Google Drive file ID to delete", true),
                ],
            },
            Arc::new(GwDriveDelete(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_calendar_create".into(),
                description: "Create a new Google Calendar event. Requires HITL approval.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Yellow,
                min_tier: "standard",
                parameters: vec![
                    param("summary", "string", "Event title", true),
                    param("start", "string", "Start time (ISO 8601)", true),
                    param("end", "string", "End time (ISO 8601)", true),
                    param("description", "string", "Event description", false),
                    param("location", "string", "Event location", false),
                ],
            },
            Arc::new(GwCalendarCreate(gw.clone())),
        ),
        (
            ToolDef {
                name: "gw_calendar_delete".into(),
                description: "Delete a Google Calendar event. Requires HITL approval.".into(),
                category: "google_workspace".into(),
                default_tier: RiskLevel::Red,
                min_tier: "standard",
                parameters: vec![
                    param("event_id", "string", "Calendar event ID to delete", true),
                ],
            },
            Arc::new(GwCalendarDelete(gw.clone())),
        ),
    ];

    for (def, handler) in tools {
        tracing::debug!("[GW] registering tool: {}", def.name);
        reg.register(def, handler);
    }

    tracing::info!("[GW] 22 Google Workspace tools registered (account='{}', MCP connection pending)", gw.account);
}

