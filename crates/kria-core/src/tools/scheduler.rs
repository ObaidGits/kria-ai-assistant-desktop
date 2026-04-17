use std::sync::Arc;
use async_trait::async_trait;
use crate::infra::ToolResult;
use crate::safety::RiskLevel;
use crate::tools::registry::{ToolRegistry, ToolDef, ToolHandler, ParamDef};

fn param(name: &str, ty: &str, desc: &str, required: bool) -> ParamDef {
    ParamDef { name: name.into(), param_type: ty.into(), description: desc.into(), required, default: None }
}

struct ListScheduledTasks;
#[async_trait] impl ToolHandler for ListScheduledTasks {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        // List cron jobs for current user + systemd timers
        let cron = tokio::process::Command::new("crontab")
            .arg("-l")
            .output().await;
        let timers = tokio::process::Command::new("systemctl")
            .args(["--user", "list-timers", "--no-pager"])
            .output().await;

        let cron_text = cron.map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();
        let timers_text = timers.map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();

        ToolResult::ok(serde_json::json!({
            "cron_jobs": cron_text.trim(),
            "systemd_timers": timers_text.trim(),
        }))
    }
}

struct CreateScheduledTask;
#[async_trait] impl ToolHandler for CreateScheduledTask {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let schedule = params["schedule"].as_str().unwrap_or("");
        let command = params["command"].as_str().unwrap_or("");

        // Add to crontab
        let existing = tokio::process::Command::new("crontab")
            .arg("-l")
            .output().await
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();

        let new_crontab = format!("{}\n{} {}\n", existing.trim(), schedule, command);
        let child = tokio::process::Command::new("crontab")
            .arg("-")
            .stdin(std::process::Stdio::piped())
            .spawn();

        let mut child = match child {
            Ok(c) => c,
            Err(e) => return ToolResult::err(&format!("failed to spawn crontab: {e}")),
        };

        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            let _ = stdin.write_all(new_crontab.as_bytes()).await;
        }
        let status = match child.wait().await {
            Ok(s) => s,
            Err(e) => return ToolResult::err(&format!("crontab wait failed: {e}")),
        };

        if status.success() {
            ToolResult::ok(serde_json::json!({
                "created": true, "schedule": schedule, "command": command,
            }))
        } else {
            ToolResult::err("failed to create cron job")
        }
    }
}

struct DeleteScheduledTask;
#[async_trait] impl ToolHandler for DeleteScheduledTask {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let pattern = params["pattern"].as_str().unwrap_or("");
        let existing = tokio::process::Command::new("crontab")
            .arg("-l")
            .output().await
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();

        let filtered: String = existing.lines()
            .filter(|l| !l.contains(pattern))
            .collect::<Vec<_>>()
            .join("\n");

        let child = tokio::process::Command::new("crontab")
            .arg("-")
            .stdin(std::process::Stdio::piped())
            .spawn();

        let mut child = match child {
            Ok(c) => c,
            Err(e) => return ToolResult::err(&format!("failed to spawn crontab: {e}")),
        };

        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            let _ = stdin.write_all(filtered.as_bytes()).await;
        }
        let status = match child.wait().await {
            Ok(s) => s,
            Err(e) => return ToolResult::err(&format!("crontab wait failed: {e}")),
        };

        if status.success() {
            ToolResult::ok(serde_json::json!({ "deleted_pattern": pattern }))
        } else {
            ToolResult::err("failed to delete cron job")
        }
    }
}

pub fn register(reg: &ToolRegistry) {
    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        (ToolDef {
            name: "list_scheduled_tasks".into(), description: "List cron jobs and systemd timers".into(),
            category: "scheduler".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![],
        }, Arc::new(ListScheduledTasks)),
        (ToolDef {
            name: "create_scheduled_task".into(), description: "Create a cron job or scheduled task".into(),
            category: "scheduler".into(), default_tier: RiskLevel::Red, min_tier: "standard",
            parameters: vec![
                param("schedule", "string", "Cron schedule (e.g. '0 * * * *')", true),
                param("command", "string", "Command to run", true),
            ],
        }, Arc::new(CreateScheduledTask)),
        (ToolDef {
            name: "delete_scheduled_task".into(), description: "Delete a cron job by pattern".into(),
            category: "scheduler".into(), default_tier: RiskLevel::Red, min_tier: "standard",
            parameters: vec![param("pattern", "string", "Text pattern to match in cron entry", true)],
        }, Arc::new(DeleteScheduledTask)),
    ];
    for (def, handler) in tools { reg.register(def, handler); }
}
