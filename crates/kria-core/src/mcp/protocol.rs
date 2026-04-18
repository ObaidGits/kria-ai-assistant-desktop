//! MCP JSON-RPC protocol types.
//!
//! Implements the subset of MCP (Model Context Protocol) needed to:
//! - Initialize a connection
//! - List tools from a server
//! - Call tools on a server

use serde::{Deserialize, Serialize};

// ── JSON-RPC 2.0 base types ────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

impl JsonRpcRequest {
    pub fn new(id: u64, method: &str, params: Option<serde_json::Value>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcResponse {
    #[allow(dead_code)]
    pub jsonrpc: String,
    pub id: Option<u64>,
    pub result: Option<serde_json::Value>,
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    pub fn into_result(self) -> Result<serde_json::Value, String> {
        if let Some(err) = self.error {
            Err(format!("MCP error {}: {}", err.code, err.message))
        } else {
            Ok(self.result.unwrap_or(serde_json::Value::Null))
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<serde_json::Value>,
}

// ── MCP-specific types ──────────────────────────────────────────────

/// Client capabilities sent during `initialize`.
#[derive(Debug, Clone, Serialize, Default)]
pub struct ClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roots: Option<serde_json::Value>,
}

/// `initialize` request params.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub protocol_version: String,
    pub capabilities: ClientCapabilities,
    pub client_info: ClientInfo,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

/// `initialize` response result.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    pub server_info: Option<ServerInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerCapabilities {
    pub tools: Option<serde_json::Value>,
    pub resources: Option<serde_json::Value>,
    pub prompts: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: Option<String>,
}

/// Tool definition returned by `tools/list`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolDef {
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub input_schema: serde_json::Value,
}

/// `tools/list` response.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolsListResult {
    pub tools: Vec<McpToolDef>,
}

/// `tools/call` request params.
#[derive(Debug, Clone, Serialize)]
pub struct ToolCallParams {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<serde_json::Value>,
}

/// Content item returned by `tools/call`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolCallContent {
    #[serde(rename = "type")]
    pub content_type: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub data: Option<String>,
    #[serde(rename = "mimeType", default)]
    pub mime_type: Option<String>,
}

/// `tools/call` response result.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallResult {
    pub content: Vec<ToolCallContent>,
    #[serde(default)]
    pub is_error: bool,
}
