use std::sync::Arc;
use async_trait::async_trait;
use crate::infra::ToolResult;
use crate::safety::RiskLevel;
use crate::tools::registry::{ToolRegistry, ToolDef, ToolHandler, ParamDef};

fn param(name: &str, ty: &str, desc: &str, required: bool) -> ParamDef {
    ParamDef { name: name.into(), param_type: ty.into(), description: desc.into(), required, default: None }
}

struct SetVolume;
#[async_trait] impl ToolHandler for SetVolume {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let level = params["level"].as_u64().unwrap_or(50);
        if cfg!(target_os = "linux") {
            let output = tokio::process::Command::new("pactl")
                .args(["set-sink-volume", "@DEFAULT_SINK@", &format!("{}%", level)])
                .output().await;
            match output {
                Ok(o) if o.status.success() => ToolResult::ok(serde_json::json!({ "volume": level })),
                _ => ToolResult::err("failed to set volume (pactl)")
            }
        } else {
            ToolResult::err("set_volume not implemented for this OS")
        }
    }
}

struct SetBrightness;
#[async_trait] impl ToolHandler for SetBrightness {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let level = params["level"].as_u64().unwrap_or(50);
        if cfg!(target_os = "linux") {
            let output = tokio::process::Command::new("brightnessctl")
                .args(["set", &format!("{}%", level)])
                .output().await;
            match output {
                Ok(o) if o.status.success() => ToolResult::ok(serde_json::json!({ "brightness": level })),
                _ => ToolResult::err("failed to set brightness (brightnessctl)")
            }
        } else {
            ToolResult::err("set_brightness not implemented for this OS")
        }
    }
}

struct ToggleWifi;
#[async_trait] impl ToolHandler for ToggleWifi {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let enable = params["enable"].as_bool().unwrap_or(true);
        let state = if enable { "on" } else { "off" };
        if cfg!(target_os = "linux") {
            let output = tokio::process::Command::new("nmcli")
                .args(["radio", "wifi", state])
                .output().await;
            match output {
                Ok(o) if o.status.success() => ToolResult::ok(serde_json::json!({ "wifi": state })),
                _ => ToolResult::err("failed to toggle wifi (nmcli)")
            }
        } else {
            ToolResult::err("toggle_wifi not implemented for this OS")
        }
    }
}

struct SetPowerPlan;
#[async_trait] impl ToolHandler for SetPowerPlan {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let plan = params["plan"].as_str().unwrap_or("balanced");
        // Linux: powerprofilesctl
        if cfg!(target_os = "linux") {
            let output = tokio::process::Command::new("powerprofilesctl")
                .args(["set", plan])
                .output().await;
            match output {
                Ok(o) if o.status.success() => ToolResult::ok(serde_json::json!({ "power_plan": plan })),
                _ => ToolResult::err("failed to set power plan (powerprofilesctl)")
            }
        } else {
            ToolResult::err("set_power_plan not implemented for this OS")
        }
    }
}

struct GetPowerPlan;
#[async_trait] impl ToolHandler for GetPowerPlan {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        if cfg!(target_os = "linux") {
            let output = tokio::process::Command::new("powerprofilesctl")
                .arg("get")
                .output().await;
            match output {
                Ok(o) if o.status.success() => {
                    let plan = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    ToolResult::ok(serde_json::json!({ "power_plan": plan }))
                }
                _ => ToolResult::ok(serde_json::json!({ "power_plan": "unknown" }))
            }
        } else {
            ToolResult::ok(serde_json::json!({ "power_plan": "unsupported" }))
        }
    }
}

struct ConnectWifi;
#[async_trait] impl ToolHandler for ConnectWifi {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let ssid = params["ssid"].as_str().unwrap_or("");
        let password = params["password"].as_str();
        let mut cmd = tokio::process::Command::new("nmcli");
        cmd.args(["device", "wifi", "connect", ssid]);
        if let Some(pw) = password {
            cmd.args(["password", pw]);
        }
        match cmd.output().await {
            Ok(o) if o.status.success() => ToolResult::ok(serde_json::json!({ "connected": ssid })),
            Ok(o) => ToolResult::err(String::from_utf8_lossy(&o.stderr).to_string()),
            Err(e) => ToolResult::err(format!("connect_wifi failed: {e}"))
        }
    }
}

struct GetWifiNetworks;
#[async_trait] impl ToolHandler for GetWifiNetworks {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        let output = tokio::process::Command::new("nmcli")
            .args(["-t", "-f", "SSID,SIGNAL,SECURITY", "device", "wifi", "list"])
            .output().await;
        match output {
            Ok(o) if o.status.success() => {
                let networks: Vec<serde_json::Value> = String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .filter(|l| !l.is_empty())
                    .map(|l| {
                        let parts: Vec<&str> = l.splitn(3, ':').collect();
                        serde_json::json!({
                            "ssid": parts.first().unwrap_or(&""),
                            "signal": parts.get(1).unwrap_or(&""),
                            "security": parts.get(2).unwrap_or(&""),
                        })
                    }).collect();
                ToolResult::ok(serde_json::json!({ "networks": networks }))
            }
            _ => ToolResult::err("failed to list wifi networks (nmcli)")
        }
    }
}

struct SetEnvironmentVariable;
#[async_trait] impl ToolHandler for SetEnvironmentVariable {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let name = params["name"].as_str().unwrap_or("");
        let value = params["value"].as_str().unwrap_or("");
        std::env::set_var(name, value);
        ToolResult::ok(serde_json::json!({ "name": name, "value": value, "set": true }))
    }
}

struct GetEnvironmentVariable;
#[async_trait] impl ToolHandler for GetEnvironmentVariable {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let name = params["name"].as_str().unwrap_or("");
        let value = std::env::var(name).ok();
        ToolResult::ok(serde_json::json!({ "name": name, "value": value }))
    }
}

struct ListEnvironmentVariables;
#[async_trait] impl ToolHandler for ListEnvironmentVariables {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        let vars: Vec<serde_json::Value> = std::env::vars()
            .filter(|(k, _)| !k.contains("KEY") && !k.contains("SECRET") && !k.contains("TOKEN") && !k.contains("PASSWORD"))
            .map(|(k, v)| serde_json::json!({ "name": k, "value": v }))
            .collect();
        ToolResult::ok(serde_json::json!({ "variables": vars, "count": vars.len() }))
    }
}

pub fn register(reg: &mut ToolRegistry) {
    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        // GREEN
        (ToolDef {
            name: "get_power_plan".into(), description: "Get current power plan".into(),
            category: "system_config".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![],
        }, Arc::new(GetPowerPlan)),
        (ToolDef {
            name: "get_environment_variable".into(), description: "Get an environment variable value".into(),
            category: "system_config".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![param("name", "string", "Variable name", true)],
        }, Arc::new(GetEnvironmentVariable)),
        (ToolDef {
            name: "list_environment_variables".into(), description: "List all environment variables (secrets filtered)".into(),
            category: "system_config".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![],
        }, Arc::new(ListEnvironmentVariables)),
        (ToolDef {
            name: "get_wifi_networks".into(), description: "List available WiFi networks".into(),
            category: "system_config".into(), default_tier: RiskLevel::Green, min_tier: "standard",
            parameters: vec![],
        }, Arc::new(GetWifiNetworks)),
        // YELLOW
        (ToolDef {
            name: "set_volume".into(), description: "Set system audio volume (0-100)".into(),
            category: "system_config".into(), default_tier: RiskLevel::Yellow, min_tier: "lite",
            parameters: vec![param("level", "integer", "Volume 0-100", true)],
        }, Arc::new(SetVolume)),
        (ToolDef {
            name: "set_brightness".into(), description: "Set screen brightness (0-100)".into(),
            category: "system_config".into(), default_tier: RiskLevel::Yellow, min_tier: "lite",
            parameters: vec![param("level", "integer", "Brightness 0-100", true)],
        }, Arc::new(SetBrightness)),
        (ToolDef {
            name: "toggle_wifi".into(), description: "Enable or disable WiFi".into(),
            category: "system_config".into(), default_tier: RiskLevel::Yellow, min_tier: "lite",
            parameters: vec![param("enable", "boolean", "true=on, false=off", true)],
        }, Arc::new(ToggleWifi)),
        (ToolDef {
            name: "set_power_plan".into(), description: "Set power plan (balanced/performance/power-saver)".into(),
            category: "system_config".into(), default_tier: RiskLevel::Yellow, min_tier: "lite",
            parameters: vec![param("plan", "string", "Power plan name", true)],
        }, Arc::new(SetPowerPlan)),
        (ToolDef {
            name: "connect_wifi".into(), description: "Connect to a WiFi network".into(),
            category: "system_config".into(), default_tier: RiskLevel::Yellow, min_tier: "standard",
            parameters: vec![
                param("ssid", "string", "Network name", true),
                param("password", "string", "Network password", false),
            ],
        }, Arc::new(ConnectWifi)),
        // RED
        (ToolDef {
            name: "set_environment_variable".into(), description: "Set an environment variable".into(),
            category: "system_config".into(), default_tier: RiskLevel::Red, min_tier: "lite",
            parameters: vec![
                param("name", "string", "Variable name", true),
                param("value", "string", "Variable value", true),
            ],
        }, Arc::new(SetEnvironmentVariable)),
    ];
    for (def, handler) in tools { reg.register(def, handler); }
}
