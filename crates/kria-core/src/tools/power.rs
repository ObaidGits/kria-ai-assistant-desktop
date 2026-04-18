use crate::infra::ToolResult;
use crate::safety::RiskLevel;
use crate::tools::registry::{ParamDef, ToolDef, ToolHandler, ToolRegistry};
use async_trait::async_trait;
use std::sync::Arc;

fn param(name: &str, ty: &str, desc: &str, required: bool) -> ParamDef {
    ParamDef {
        name: name.into(),
        param_type: ty.into(),
        description: desc.into(),
        required,
        default: None,
    }
}

struct Sleep;
#[async_trait]
impl ToolHandler for Sleep {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        let cmd = if cfg!(target_os = "linux") {
            "systemctl suspend"
        } else if cfg!(target_os = "macos") {
            "pmset sleepnow"
        } else {
            "rundll32.exe powrprof.dll,SetSuspendState 0,1,0"
        };
        match tokio::process::Command::new("sh")
            .args(["-c", cmd])
            .status()
            .await
        {
            Ok(s) if s.success() => ToolResult::ok(serde_json::json!({ "action": "sleep" })),
            _ => ToolResult::err("failed to sleep"),
        }
    }
}

struct Hibernate;
#[async_trait]
impl ToolHandler for Hibernate {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        let cmd = if cfg!(target_os = "linux") {
            "systemctl hibernate"
        } else {
            "shutdown /h"
        };
        match tokio::process::Command::new("sh")
            .args(["-c", cmd])
            .status()
            .await
        {
            Ok(s) if s.success() => ToolResult::ok(serde_json::json!({ "action": "hibernate" })),
            _ => ToolResult::err("failed to hibernate"),
        }
    }
}

struct ShutdownSystem;
#[async_trait]
impl ToolHandler for ShutdownSystem {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let delay_minutes = params["delay_minutes"].as_u64().unwrap_or(0);
        let cmd = if cfg!(target_os = "linux") {
            format!("shutdown -h +{}", delay_minutes)
        } else {
            format!("shutdown /s /t {}", delay_minutes * 60)
        };
        match tokio::process::Command::new("sh")
            .args(["-c", &cmd])
            .status()
            .await
        {
            Ok(s) if s.success() => ToolResult::ok(serde_json::json!({
                "action": "shutdown", "delay_minutes": delay_minutes,
            })),
            _ => ToolResult::err("failed to initiate shutdown"),
        }
    }
}

struct RebootSystem;
#[async_trait]
impl ToolHandler for RebootSystem {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        let cmd = if cfg!(target_os = "linux") {
            "reboot"
        } else {
            "shutdown /r /t 0"
        };
        match tokio::process::Command::new("sh")
            .args(["-c", cmd])
            .status()
            .await
        {
            Ok(s) if s.success() => ToolResult::ok(serde_json::json!({ "action": "reboot" })),
            _ => ToolResult::err("failed to reboot"),
        }
    }
}

struct LockScreen;
#[async_trait]
impl ToolHandler for LockScreen {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        let cmd = if cfg!(target_os = "linux") {
            "loginctl lock-session"
        } else if cfg!(target_os = "macos") {
            "osascript -e 'tell application \"System Events\" to keystroke \"q\" using {control down, command down}'"
        } else {
            "rundll32.exe user32.dll,LockWorkStation"
        };
        match tokio::process::Command::new("sh")
            .args(["-c", cmd])
            .status()
            .await
        {
            Ok(_) => ToolResult::ok(serde_json::json!({ "action": "lock_screen" })),
            Err(e) => ToolResult::err(format!("lock_screen failed: {e}")),
        }
    }
}

pub fn register(reg: &ToolRegistry) {
    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        // GREEN
        (
            ToolDef {
                name: "lock_screen".into(),
                description: "Lock the screen".into(),
                category: "power".into(),
                default_tier: RiskLevel::Green,
                min_tier: "lite",
                parameters: vec![],
            },
            Arc::new(LockScreen),
        ),
        // YELLOW
        (
            ToolDef {
                name: "sleep".into(),
                description: "Put system to sleep".into(),
                category: "power".into(),
                default_tier: RiskLevel::Yellow,
                min_tier: "lite",
                parameters: vec![],
            },
            Arc::new(Sleep),
        ),
        (
            ToolDef {
                name: "hibernate".into(),
                description: "Hibernate the system".into(),
                category: "power".into(),
                default_tier: RiskLevel::Yellow,
                min_tier: "lite",
                parameters: vec![],
            },
            Arc::new(Hibernate),
        ),
        // RED
        (
            ToolDef {
                name: "shutdown_system".into(),
                description: "Shutdown the system".into(),
                category: "power".into(),
                default_tier: RiskLevel::Red,
                min_tier: "lite",
                parameters: vec![param(
                    "delay_minutes",
                    "integer",
                    "Delay in minutes (default 0)",
                    false,
                )],
            },
            Arc::new(ShutdownSystem),
        ),
        (
            ToolDef {
                name: "reboot_system".into(),
                description: "Reboot the system".into(),
                category: "power".into(),
                default_tier: RiskLevel::Red,
                min_tier: "lite",
                parameters: vec![],
            },
            Arc::new(RebootSystem),
        ),
    ];
    for (def, handler) in tools {
        reg.register(def, handler);
    }
}
