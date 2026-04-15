use std::sync::Arc;
use async_trait::async_trait;
use crate::infra::ToolResult;
use crate::safety::RiskLevel;
use crate::tools::registry::{ToolRegistry, ToolDef, ToolHandler, ParamDef};

fn param(name: &str, ty: &str, desc: &str, required: bool) -> ParamDef {
    ParamDef { name: name.into(), param_type: ty.into(), description: desc.into(), required, default: None }
}

struct SendNotification;
#[async_trait] impl ToolHandler for SendNotification {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let title = params["title"].as_str().unwrap_or("K.R.I.A.");
        let body = params["body"].as_str().unwrap_or("");
        match notify_rust::Notification::new()
            .summary(title)
            .body(body)
            .appname("KRIA")
            .show() {
            Ok(_) => ToolResult::ok(serde_json::json!({ "sent": true, "title": title })),
            Err(e) => ToolResult::err(format!("notification failed: {e}"))
        }
    }
}

struct ComposeEmail;
#[async_trait] impl ToolHandler for ComposeEmail {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let to = params["to"].as_str().unwrap_or("");
        let subject = params["subject"].as_str().unwrap_or("");
        let body = params["body"].as_str().unwrap_or("");
        // Opens default email client with mailto: link (draft only, does NOT send)
        let mailto = format!("mailto:{}?subject={}&body={}",
            urlencoding(to), urlencoding(subject), urlencoding(body));
        let _ = open::that(&mailto);
        ToolResult::ok(serde_json::json!({
            "action": "compose_email",
            "to": to, "subject": subject,
            "note": "Email draft opened in default email client (not sent)",
        }))
    }
}

struct ScheduleReminder;
#[async_trait] impl ToolHandler for ScheduleReminder {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let message = params["message"].as_str().unwrap_or("");
        let delay_minutes = params["delay_minutes"].as_u64().unwrap_or(5);
        // Schedule a notification after delay (using tokio::spawn)
        let msg = message.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(delay_minutes * 60)).await;
            let _ = notify_rust::Notification::new()
                .summary("KRIA Reminder")
                .body(&msg)
                .appname("KRIA")
                .show();
        });
        ToolResult::ok(serde_json::json!({
            "scheduled": true,
            "message": message,
            "delay_minutes": delay_minutes,
        }))
    }
}

// Simple URL encoding helper (no external dep needed for basic mailto)
fn urlencoding(s: &str) -> String {
    s.replace(' ', "%20").replace('\n', "%0A").replace('&', "%26")
}

pub fn register(reg: &mut ToolRegistry) {
    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        (ToolDef {
            name: "send_notification".into(), description: "Send a desktop notification".into(),
            category: "communication".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![
                param("title", "string", "Notification title", false),
                param("body", "string", "Notification body", true),
            ],
        }, Arc::new(SendNotification)),
        (ToolDef {
            name: "compose_email".into(), description: "Open email draft in default client (does NOT send)".into(),
            category: "communication".into(), default_tier: RiskLevel::Green, min_tier: "standard",
            parameters: vec![
                param("to", "string", "Recipient email", true),
                param("subject", "string", "Email subject", true),
                param("body", "string", "Email body", true),
            ],
        }, Arc::new(ComposeEmail)),
        (ToolDef {
            name: "schedule_reminder".into(), description: "Schedule a reminder notification".into(),
            category: "communication".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![
                param("message", "string", "Reminder message", true),
                param("delay_minutes", "integer", "Minutes from now (default 5)", false),
            ],
        }, Arc::new(ScheduleReminder)),
    ];
    for (def, handler) in tools { reg.register(def, handler); }
}
