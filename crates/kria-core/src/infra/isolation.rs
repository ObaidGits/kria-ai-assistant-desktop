use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Result envelope returned by every tool execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub success: bool,
    pub data: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ToolResult {
    pub fn ok(data: serde_json::Value) -> Self {
        Self {
            success: true,
            data,
            error: None,
        }
    }

    pub fn ok_text(msg: impl Into<String>) -> Self {
        Self::ok(serde_json::Value::String(msg.into()))
    }

    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            success: false,
            data: serde_json::Value::Null,
            error: Some(msg.into()),
        }
    }
}

/// Execute a tool function with timeout and panic isolation.
pub async fn run_isolated<F, Fut>(
    name: &str,
    timeout: Duration,
    f: F,
) -> ToolResult
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ToolResult> + Send + 'static,
{
    let tool_name = name.to_string();
    let handle = tokio::spawn(async move {
        tokio::time::timeout(timeout, f()).await
    });

    match handle.await {
        Ok(Ok(result)) => result,
        Ok(Err(_elapsed)) => {
            tracing::warn!(tool = %tool_name, "tool execution timed out");
            ToolResult::err(format!("tool '{tool_name}' timed out after {}s", timeout.as_secs()))
        }
        Err(join_err) => {
            tracing::error!(tool = %tool_name, error = %join_err, "tool panicked");
            ToolResult::err(format!("tool '{tool_name}' panicked: {join_err}"))
        }
    }
}
