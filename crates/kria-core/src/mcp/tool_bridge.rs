//! MCP tool bridge — adapts MCP server tools to KRIA's ToolHandler trait.

use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

use super::client::McpClient;
use super::protocol::ToolCallResult;
use crate::infra::ToolResult;
use crate::tools::ToolHandler;

fn normalize_arguments(params: Value) -> Option<Value> {
    if params.is_null() || params.as_object().map(|o| o.is_empty()).unwrap_or(false) {
        None
    } else {
        Some(params)
    }
}

fn should_skip_gworkspace_account_injection(mcp_tool_name: &str) -> bool {
    let lower = mcp_tool_name.to_ascii_lowercase();
    lower.starts_with("addaccount")
        || lower.starts_with("listaccounts")
        || lower.starts_with("removeaccount")
        || lower.starts_with("testpermissions")
        || lower.starts_with("config")
        || lower == "status"
}

fn inject_gworkspace_account(
    server_name: &str,
    mcp_tool_name: &str,
    arguments: Option<Value>,
) -> (Option<Value>, bool) {
    if !server_name.eq_ignore_ascii_case("gworkspace")
        || should_skip_gworkspace_account_injection(mcp_tool_name)
    {
        return (arguments, false);
    }

    let account = std::env::var("KRIA_GW_ACCOUNT").unwrap_or_else(|_| "personal".into());

    match arguments {
        Some(Value::Object(mut obj)) => {
            if obj.contains_key("account") {
                (Some(Value::Object(obj)), false)
            } else {
                obj.insert("account".to_string(), Value::String(account));
                (Some(Value::Object(obj)), true)
            }
        }
        Some(other) => (Some(other), false),
        None => (
            Some(serde_json::json!({
                "account": account,
            })),
            true,
        ),
    }
}

fn should_retry_without_injected_account(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("account")
        && (lower.contains("unknown")
            || lower.contains("unexpected")
            || lower.contains("not allowed")
            || lower.contains("additional properties"))
}

fn tool_result_text(result: &ToolCallResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| c.text.as_deref())
        .collect::<Vec<_>>()
        .join("\n")
}

/// A ToolHandler that delegates execution to an MCP server.
pub struct McpToolHandler {
    client: Arc<McpClient>,
    server_name: String,
    /// The tool name on the MCP server side (without prefix).
    mcp_tool_name: String,
}

impl McpToolHandler {
    pub fn new(client: Arc<McpClient>, server_name: &str, mcp_tool_name: &str) -> Self {
        Self {
            client,
            server_name: server_name.to_string(),
            mcp_tool_name: mcp_tool_name.to_string(),
        }
    }
}

#[async_trait]
impl ToolHandler for McpToolHandler {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let original_arguments = normalize_arguments(params);
        let (arguments, injected_account) = inject_gworkspace_account(
            &self.server_name,
            &self.mcp_tool_name,
            original_arguments.clone(),
        );

        let mut call_result = self
            .client
            .call_tool(&self.mcp_tool_name, arguments.clone())
            .await;

        // Some gworkspace account-management tools may reject unexpected `account`
        // parameters. Retry once without injected account to keep raw MCP tools usable.
        if injected_account && original_arguments != arguments {
            let should_retry = match &call_result {
                Ok(result) if result.is_error => {
                    should_retry_without_injected_account(&tool_result_text(result))
                }
                Err(e) => should_retry_without_injected_account(&e.to_string()),
                _ => false,
            };

            if should_retry {
                tracing::debug!(
                    server = %self.server_name,
                    tool = %self.mcp_tool_name,
                    "retrying MCP call without injected account"
                );
                call_result = self
                    .client
                    .call_tool(&self.mcp_tool_name, original_arguments)
                    .await;
            }
        }

        match call_result {
            Ok(result) => {
                // Combine all text content into a single string.
                let text = tool_result_text(&result);

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

#[cfg(test)]
mod tests {
    use super::{
        inject_gworkspace_account, should_retry_without_injected_account,
        should_skip_gworkspace_account_injection,
    };

    #[test]
    fn injects_default_account_for_gworkspace_calls() {
        let (args, injected) = inject_gworkspace_account(
            "gworkspace",
            "searchGmail",
            Some(serde_json::json!({ "query": "is:unread" })),
        );

        assert!(injected);
        let account = args
            .and_then(|v| v.get("account").cloned())
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_default();
        assert!(!account.is_empty());
    }

    #[test]
    fn preserves_existing_account_argument() {
        let (args, injected) = inject_gworkspace_account(
            "gworkspace",
            "searchGmail",
            Some(serde_json::json!({ "query": "is:unread", "account": "work" })),
        );

        assert!(!injected);
        assert_eq!(args.unwrap()["account"], serde_json::json!("work"));
    }

    #[test]
    fn skips_account_management_tool_injection() {
        assert!(should_skip_gworkspace_account_injection("listAccounts"));

        let (args, injected) =
            inject_gworkspace_account("gworkspace", "listAccounts", Some(serde_json::json!({})));

        assert!(!injected);
        assert_eq!(args.unwrap(), serde_json::json!({}));
    }

    #[test]
    fn retry_detector_catches_unknown_account_parameter_errors() {
        assert!(should_retry_without_injected_account(
            "Unexpected parameter 'account': additional properties not allowed"
        ));
        assert!(!should_retry_without_injected_account(
            "invalid_grant token expired"
        ));
    }
}
