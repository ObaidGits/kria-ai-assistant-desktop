use std::sync::Arc;
use async_trait::async_trait;
use crate::infra::ToolResult;
use crate::safety::RiskLevel;
use crate::tools::registry::{ToolRegistry, ToolDef, ToolHandler, ParamDef};

fn param(name: &str, ty: &str, desc: &str, required: bool) -> ParamDef {
    ParamDef { name: name.into(), param_type: ty.into(), description: desc.into(), required, default: None }
}

struct SetProcessPriority;
#[async_trait] impl ToolHandler for SetProcessPriority {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let pid = params["pid"].as_u64().unwrap_or(0);
        let priority = params["priority"].as_i64().unwrap_or(0);
        let output = tokio::process::Command::new("renice")
            .args([&priority.to_string(), "-p", &pid.to_string()])
            .output().await;
        match output {
            Ok(o) if o.status.success() => ToolResult::ok(serde_json::json!({
                "pid": pid, "priority": priority, "set": true
            })),
            _ => ToolResult::err(format!("failed to set priority for PID {pid}"))
        }
    }
}

struct ManageService;
#[async_trait] impl ToolHandler for ManageService {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let name = params["name"].as_str().unwrap_or("");
        let action = params["action"].as_str().unwrap_or("status");
        let output = tokio::process::Command::new("systemctl")
            .args([action, name])
            .output().await;
        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout).to_string();
                let stderr = String::from_utf8_lossy(&o.stderr).to_string();
                ToolResult::ok(serde_json::json!({
                    "service": name, "action": action,
                    "success": o.status.success(),
                    "output": stdout, "error": stderr,
                }))
            }
            Err(e) => ToolResult::err(format!("manage_service failed: {e}"))
        }
    }
}

struct GetActiveConnections;
#[async_trait] impl ToolHandler for GetActiveConnections {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        let output = tokio::process::Command::new("ss")
            .args(["-tuln"])
            .output().await;
        match output {
            Ok(o) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stdout).to_string();
                ToolResult::ok_text(text)
            }
            _ => ToolResult::err("failed to get active connections")
        }
    }
}

pub fn register(reg: &mut ToolRegistry) {
    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        // GREEN
        (ToolDef {
            name: "get_active_connections".into(), description: "Get active network connections".into(),
            category: "process".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![],
        }, Arc::new(GetActiveConnections)),
        // RED
        (ToolDef {
            name: "set_process_priority".into(), description: "Set process priority/niceness".into(),
            category: "process".into(), default_tier: RiskLevel::Red, min_tier: "standard",
            parameters: vec![
                param("pid", "integer", "Process ID", true),
                param("priority", "integer", "Nice value (-20 to 19)", true),
            ],
        }, Arc::new(SetProcessPriority)),
        (ToolDef {
            name: "manage_service".into(), description: "Start, stop, or restart a systemd service".into(),
            category: "process".into(), default_tier: RiskLevel::Red, min_tier: "standard",
            parameters: vec![
                param("name", "string", "Service name", true),
                param("action", "string", "start|stop|restart|status", true),
            ],
        }, Arc::new(ManageService)),
    ];
    for (def, handler) in tools { reg.register(def, handler); }
}
