use std::sync::Arc;
use async_trait::async_trait;
use crate::infra::ToolResult;
use crate::safety::RiskLevel;
use crate::tools::registry::{ToolRegistry, ToolDef, ToolHandler, ParamDef};
use crate::automation::proactive::ProactiveEngine;

fn param(name: &str, ty: &str, desc: &str, required: bool) -> ParamDef {
    ParamDef { name: name.into(), param_type: ty.into(), description: desc.into(), required, default: None }
}

struct CheckSystemHealth { engine: Arc<ProactiveEngine> }
#[async_trait] impl ToolHandler for CheckSystemHealth {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        self.engine.check_system_health().await;
        let alerts = self.engine.get_alerts().await;
        let items: Vec<serde_json::Value> = alerts.iter().map(|a| {
            serde_json::json!({
                "id": a.id,
                "category": a.category,
                "title": a.title,
                "message": a.message,
                "suggestion": a.suggestion,
                "timestamp": a.timestamp.to_rfc3339(),
            })
        }).collect();
        ToolResult::ok(serde_json::json!({
            "alerts": items,
            "count": items.len(),
            "thresholds": {
                "min_disk_pct": self.engine.thresholds().min_disk_pct,
                "min_ram_mb": self.engine.thresholds().min_ram_mb,
                "min_battery_pct": self.engine.thresholds().min_battery_pct,
            }
        }))
    }
}

struct GetAlerts { engine: Arc<ProactiveEngine> }
#[async_trait] impl ToolHandler for GetAlerts {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let include_dismissed = params["include_dismissed"].as_bool().unwrap_or(false);
        let alerts = if include_dismissed {
            self.engine.get_all_alerts().await
        } else {
            self.engine.get_alerts().await
        };
        let items: Vec<serde_json::Value> = alerts.iter().map(|a| {
            serde_json::json!({
                "id": a.id,
                "category": a.category,
                "title": a.title,
                "message": a.message,
                "suggestion": a.suggestion,
                "dismissed": a.dismissed,
                "timestamp": a.timestamp.to_rfc3339(),
            })
        }).collect();
        ToolResult::ok(serde_json::json!({ "alerts": items, "count": items.len() }))
    }
}

struct DismissAlert { engine: Arc<ProactiveEngine> }
#[async_trait] impl ToolHandler for DismissAlert {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let id = params["id"].as_str().unwrap_or("");
        if id.is_empty() {
            return ToolResult::err("alert id is required");
        }
        let dismissed = self.engine.dismiss_alert(id).await;
        ToolResult::ok(serde_json::json!({ "dismissed": dismissed, "id": id }))
    }
}

struct WatchDirectory { engine: Arc<ProactiveEngine> }
#[async_trait] impl ToolHandler for WatchDirectory {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path = params["path"].as_str().unwrap_or("");
        let label = params["label"].as_str().unwrap_or(path);
        if path.is_empty() {
            return ToolResult::err("path is required");
        }
        let dir = std::path::Path::new(path);
        if !dir.exists() || !dir.is_dir() {
            return ToolResult::err(format!("not a valid directory: {path}"));
        }
        self.engine.watch_dir(dir.to_path_buf(), label).await;
        ToolResult::ok(serde_json::json!({ "watching": path, "label": label }))
    }
}

struct ListWatchedDirs { engine: Arc<ProactiveEngine> }
#[async_trait] impl ToolHandler for ListWatchedDirs {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        let dirs = self.engine.get_watched_dirs().await;
        let items: Vec<serde_json::Value> = dirs.iter().map(|d| {
            serde_json::json!({
                "path": d.path.to_string_lossy(),
                "label": d.label,
                "enabled": d.enabled,
            })
        }).collect();
        ToolResult::ok(serde_json::json!({ "watched": items, "count": items.len() }))
    }
}

struct SmartSuggest;
#[async_trait] impl ToolHandler for SmartSuggest {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let context = params["context"].as_str().unwrap_or("");
        // Generate a suggestion based on time of day
        let hour = chrono::Local::now().hour();
        let suggestions: Vec<&str> = match hour {
            6..=9 => vec!["Check your daily briefing", "Review yesterday's git changes", "Check disk space"],
            10..=12 => vec!["Focus on your main task", "Review open pull requests"],
            13..=14 => vec!["Good time for a break", "Review pending notifications"],
            15..=17 => vec!["Commit your changes before EOD", "Run tests on your work"],
            18..=21 => vec!["Wrap up for the day", "Push your changes"],
            _ => vec!["System looks good", "No urgent items"],
        };
        let filtered: Vec<&str> = if context.is_empty() {
            suggestions
        } else {
            suggestions.into_iter().filter(|s| !s.is_empty()).collect()
        };
        ToolResult::ok(serde_json::json!({
            "suggestions": filtered,
            "time": chrono::Local::now().format("%H:%M").to_string(),
        }))
    }
}

use chrono::Timelike;

pub fn register(reg: &ToolRegistry, engine: Arc<ProactiveEngine>) {
    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        (ToolDef {
            name: "check_system_health".into(), description: "Run system health checks (RAM, disk) and return any alerts".into(),
            category: "proactive".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![],
        }, Arc::new(CheckSystemHealth { engine: engine.clone() })),
        (ToolDef {
            name: "get_alerts".into(), description: "Get all proactive alerts and notifications".into(),
            category: "proactive".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![
                param("include_dismissed", "boolean", "Include dismissed alerts (default: false)", false),
            ],
        }, Arc::new(GetAlerts { engine: engine.clone() })),
        (ToolDef {
            name: "dismiss_alert".into(), description: "Dismiss a proactive alert by ID".into(),
            category: "proactive".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![param("id", "string", "Alert ID to dismiss", true)],
        }, Arc::new(DismissAlert { engine: engine.clone() })),
        (ToolDef {
            name: "watch_directory".into(), description: "Watch a directory for file changes and new files".into(),
            category: "proactive".into(), default_tier: RiskLevel::Green, min_tier: "standard",
            parameters: vec![
                param("path", "string", "Directory path to watch", true),
                param("label", "string", "Label for this watch (e.g., 'Downloads')", false),
            ],
        }, Arc::new(WatchDirectory { engine: engine.clone() })),
        (ToolDef {
            name: "list_watched_dirs".into(), description: "List all watched directories".into(),
            category: "proactive".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![],
        }, Arc::new(ListWatchedDirs { engine: engine.clone() })),
        (ToolDef {
            name: "smart_suggest".into(), description: "Get smart suggestions based on time of day and context".into(),
            category: "proactive".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![
                param("context", "string", "Optional context for suggestions", false),
            ],
        }, Arc::new(SmartSuggest)),
    ];
    for (def, handler) in tools { reg.register(def, handler); }
}
