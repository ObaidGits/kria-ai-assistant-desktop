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

struct SendNotification;
#[async_trait]
impl ToolHandler for SendNotification {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let title = params["title"].as_str().unwrap_or("K.R.I.A.");
        let body = params["body"].as_str().unwrap_or("");
        // Resolve D-Bus session address (notify-send needs this from a background server process)
        let dbus_addr = std::env::var("DBUS_SESSION_BUS_ADDRESS")
            .or_else(|_| std::env::var("XDG_RUNTIME_DIR").map(|d| format!("unix:path={}/bus", d)))
            .unwrap_or_else(|_| "unix:path=/run/user/1000/bus".to_string());
        let display = std::env::var("DISPLAY").unwrap_or_else(|_| ":1".to_string());
        // Primary: notify-send CLI — always shows popup banner on GNOME 44+ when D-Bus env is set
        let cli_ok = tokio::process::Command::new("notify-send")
            .env("DBUS_SESSION_BUS_ADDRESS", &dbus_addr)
            .env("DISPLAY", &display)
            .args(["-a", "KRIA", "-u", "normal", "-t", "8000",
                   "--icon=dialog-information", title, body])
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false);
        if cli_ok {
            return ToolResult::ok(serde_json::json!({ "sent": true, "title": title, "method": "notify-send" }));
        }
        // Fallback: notify_rust (may go to notification bell on GNOME 44+ instead of popup)
        let rust_result = notify_rust::Notification::new()
            .summary(title)
            .body(body)
            .appname("KRIA")
            .timeout(notify_rust::Timeout::Milliseconds(8000))
            .urgency(notify_rust::Urgency::Normal)
            .show();
        if rust_result.is_ok() {
            return ToolResult::ok(serde_json::json!({ "sent": true, "title": title, "method": "notify_rust" }));
        }
        ToolResult::err("notification failed: notify-send CLI and notify_rust both failed")
    }
}

struct ComposeEmail;
#[async_trait]
impl ToolHandler for ComposeEmail {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let to = params["to"].as_str().unwrap_or("");
        let subject = params["subject"].as_str().unwrap_or("");
        let body = params["body"].as_str().unwrap_or("");
        // Opens default email client with mailto: link (draft only, does NOT send)
        let mailto = format!(
            "mailto:{}?subject={}&body={}",
            urlencoding(to),
            urlencoding(subject),
            urlencoding(body)
        );
        let _ = open::that(&mailto);
        ToolResult::ok(serde_json::json!({
            "action": "compose_email",
            "to": to, "subject": subject,
            "note": "Email draft opened in default email client (not sent)",
        }))
    }
}

struct ScheduleReminder;
#[async_trait]
impl ToolHandler for ScheduleReminder {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let message = params["message"].as_str().unwrap_or("");
        let delay_secs = params["delay_minutes"].as_f64().unwrap_or(5.0) * 60.0;
        let delay_secs = delay_secs as u64;
        let msg = message.to_string();
        // Capture D-Bus env NOW (before spawn) so the spawned task can use it
        let dbus_addr = std::env::var("DBUS_SESSION_BUS_ADDRESS")
            .or_else(|_| std::env::var("XDG_RUNTIME_DIR").map(|d| format!("unix:path={}/bus", d)))
            .unwrap_or_else(|_| "unix:path=/run/user/1000/bus".to_string());
        let display = std::env::var("DISPLAY").unwrap_or_else(|_| ":1".to_string());
        // Play an immediate sound so user knows the reminder was accepted
        let _ = tokio::process::Command::new("paplay")
            .arg("/usr/share/sounds/freedesktop/stereo/complete.oga")
            .spawn();
        // Spawn persistent task that fires the reminder after the delay
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
            // Primary: notify-send CLI with explicit D-Bus env and critical urgency
            let cli_ok = tokio::process::Command::new("notify-send")
                .env("DBUS_SESSION_BUS_ADDRESS", &dbus_addr)
                .env("DISPLAY", &display)
                .args(["-a", "KRIA", "-u", "critical", "-t", "0",
                       "--icon=alarm", "\u{23f0} KRIA Reminder", &msg])
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false);
            if !cli_ok {
                // Fallback: notify_rust with Critical urgency + Never timeout
                let _ = notify_rust::Notification::new()
                    .summary("\u{23f0} KRIA Reminder")
                    .body(&msg)
                    .appname("KRIA")
                    .urgency(notify_rust::Urgency::Critical)
                    .timeout(notify_rust::Timeout::Never)
                    .show();
            }
            // Play alert sound when reminder fires
            let _ = tokio::process::Command::new("paplay")
                .arg("/usr/share/sounds/freedesktop/stereo/alarm-clock-elapsed.oga")
                .spawn();
        });
        let display_mins = delay_secs / 60;
        let display_secs = delay_secs % 60;
        let time_str = if display_secs == 0 {
            format!("{display_mins} minute{}", if display_mins == 1 { "" } else { "s" })
        } else {
            format!("{display_mins}m {display_secs}s")
        };
        ToolResult::ok(serde_json::json!({
            "scheduled": true,
            "message": message,
            "fires_in": time_str,
        }))
    }
}

// Simple URL encoding helper (no external dep needed for basic mailto)
fn urlencoding(s: &str) -> String {
    s.replace(' ', "%20")
        .replace('\n', "%0A")
        .replace('&', "%26")
}

pub fn register(reg: &ToolRegistry) {
    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        (
            ToolDef {
                name: "send_notification".into(),
                description: "Send a desktop notification".into(),
                category: "communication".into(),
                default_tier: RiskLevel::Green,
                min_tier: "lite",
                parameters: vec![
                    param("title", "string", "Notification title", false),
                    param("body", "string", "Notification body", true),
                ],
            },
            Arc::new(SendNotification),
        ),
        (
            ToolDef {
                name: "compose_email".into(),
                description: "Open email draft in default client (does NOT send)".into(),
                category: "communication".into(),
                default_tier: RiskLevel::Green,
                min_tier: "standard",
                parameters: vec![
                    param("to", "string", "Recipient email", true),
                    param("subject", "string", "Email subject", true),
                    param("body", "string", "Email body", true),
                ],
            },
            Arc::new(ComposeEmail),
        ),
        (
            ToolDef {
                name: "schedule_reminder".into(),
                description: "Schedule a reminder notification".into(),
                category: "communication".into(),
                default_tier: RiskLevel::Green,
                min_tier: "lite",
                parameters: vec![
                    param("message", "string", "Reminder message", true),
                    param(
                        "delay_minutes",
                        "integer",
                        "Minutes from now (default 5)",
                        false,
                    ),
                ],
            },
            Arc::new(ScheduleReminder),
        ),
    ];
    for (def, handler) in tools {
        reg.register(def, handler);
    }
}
