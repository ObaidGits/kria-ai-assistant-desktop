//! MCP tool bridge — adapts MCP server tools to KRIA's ToolHandler trait.

use std::sync::Arc;
use async_trait::async_trait;

use crate::infra::ToolResult;
use crate::tools::ToolHandler;
use super::client::McpClient;

/// A ToolHandler that delegates execution to an MCP server.
pub struct McpToolHandler {
    client: Arc<McpClient>,
    /// The tool name on the MCP server side (without prefix).
    mcp_tool_name: String,
}

impl McpToolHandler {
    pub fn new(client: Arc<McpClient>, mcp_tool_name: &str) -> Self {
        Self {
            client,
            mcp_tool_name: mcp_tool_name.to_string(),
        }
    }
}

#[async_trait]
impl ToolHandler for McpToolHandler {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let arguments = if params.is_null() || params.as_object().map(|o| o.is_empty()).unwrap_or(false) {
            None
        } else {
            Some(params)
        };

        match self.client.call_tool(&self.mcp_tool_name, arguments).await {
            Ok(result) => {
                // Combine all text content into a single string
                let text: String = result
                    .content
                    .iter()
                    .filter_map(|c| c.text.as_deref())
                    .collect::<Vec<_>>()
                    .join("\n");

                if result.is_error {
                    ToolResult {
                        success: false,
                        data: serde_json::json!(text),
                        error: Some(text),
                    }
                } else {
                    ToolResult {
                        success: true,
                        data: serde_json::json!(text),
                        error: None,
                    }
                }
            }
            Err(e) => ToolResult {
                success: false,
                data: serde_json::Value::Null,
                error: Some(format!("MCP call failed: {e}")),
            },
        }
    }
}
