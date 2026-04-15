use std::sync::Arc;
use async_trait::async_trait;
use crate::infra::ToolResult;
use crate::safety::RiskLevel;
use crate::tools::registry::{ToolRegistry, ToolDef, ToolHandler, ParamDef};

fn param(name: &str, ty: &str, desc: &str, required: bool) -> ParamDef {
    ParamDef { name: name.into(), param_type: ty.into(), description: desc.into(), required, default: None }
}

struct ExecuteBash;
#[async_trait] impl ToolHandler for ExecuteBash {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let command = params["command"].as_str().unwrap_or("");
        let timeout_secs = params["timeout"].as_u64().unwrap_or(30);

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            tokio::process::Command::new("bash")
                .args(["-c", command])
                .output()
        ).await;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let max = 10000;
                ToolResult::ok(serde_json::json!({
                    "exit_code": output.status.code(),
                    "stdout": if stdout.len() > max { &stdout[..max] } else { &stdout },
                    "stderr": if stderr.len() > max { &stderr[..max] } else { &stderr },
                    "truncated": stdout.len() > max || stderr.len() > max,
                }))
            }
            Ok(Err(e)) => ToolResult::err(format!("execution failed: {e}")),
            Err(_) => ToolResult::err(format!("command timed out after {timeout_secs}s")),
        }
    }
}

struct ExecutePython;
#[async_trait] impl ToolHandler for ExecutePython {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let code = params["code"].as_str().unwrap_or("");
        let timeout_secs = params["timeout"].as_u64().unwrap_or(30);

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            tokio::process::Command::new("python3")
                .args(["-c", code])
                .output()
        ).await;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                ToolResult::ok(serde_json::json!({
                    "exit_code": output.status.code(),
                    "stdout": stdout,
                    "stderr": stderr,
                }))
            }
            Ok(Err(e)) => ToolResult::err(format!("python execution failed: {e}")),
            Err(_) => ToolResult::err(format!("python timed out after {timeout_secs}s")),
        }
    }
}

struct ExecutePowershell;
#[async_trait] impl ToolHandler for ExecutePowershell {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let command = params["command"].as_str().unwrap_or("");
        let timeout_secs = params["timeout"].as_u64().unwrap_or(30);

        let ps = if cfg!(target_os = "windows") { "powershell" } else { "pwsh" };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            tokio::process::Command::new(ps)
                .args(["-NoProfile", "-Command", command])
                .output()
        ).await;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                ToolResult::ok(serde_json::json!({
                    "exit_code": output.status.code(),
                    "stdout": stdout,
                    "stderr": stderr,
                }))
            }
            Ok(Err(e)) => ToolResult::err(format!("powershell failed: {e}")),
            Err(_) => ToolResult::err(format!("powershell timed out after {timeout_secs}s")),
        }
    }
}

pub fn register(reg: &mut ToolRegistry) {
    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        (ToolDef {
            name: "execute_bash".into(), description: "Execute a bash shell command".into(),
            category: "shell".into(), default_tier: RiskLevel::Red, min_tier: "lite",
            parameters: vec![
                param("command", "string", "Bash command to execute", true),
                param("timeout", "integer", "Timeout in seconds (default 30)", false),
            ],
        }, Arc::new(ExecuteBash)),
        (ToolDef {
            name: "execute_python".into(), description: "Execute Python code".into(),
            category: "shell".into(), default_tier: RiskLevel::Red, min_tier: "standard",
            parameters: vec![
                param("code", "string", "Python code to execute", true),
                param("timeout", "integer", "Timeout in seconds (default 30)", false),
            ],
        }, Arc::new(ExecutePython)),
        (ToolDef {
            name: "execute_powershell".into(), description: "Execute a PowerShell command".into(),
            category: "shell".into(), default_tier: RiskLevel::Red, min_tier: "lite",
            parameters: vec![
                param("command", "string", "PowerShell command", true),
                param("timeout", "integer", "Timeout in seconds (default 30)", false),
            ],
        }, Arc::new(ExecutePowershell)),
    ];
    for (def, handler) in tools { reg.register(def, handler); }
}
