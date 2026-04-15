use std::sync::Arc;
use async_trait::async_trait;
use crate::infra::ToolResult;
use crate::safety::RiskLevel;
use crate::tools::registry::{ToolRegistry, ToolDef, ToolHandler};

// ─── Handlers ───

struct GetCpuUsage;
#[async_trait] impl ToolHandler for GetCpuUsage {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        let mut sys = sysinfo::System::new();
        sys.refresh_cpu_all();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        sys.refresh_cpu_all();
        let usage: f32 = sys.cpus().iter().map(|c| c.cpu_usage()).sum::<f32>() / sys.cpus().len() as f32;
        ToolResult::ok(serde_json::json!({
            "cpu_usage_percent": format!("{:.1}", usage),
            "cores": sys.cpus().len(),
        }))
    }
}

struct GetMemoryInfo;
#[async_trait] impl ToolHandler for GetMemoryInfo {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        let total = sys.total_memory() / (1024 * 1024);
        let used = sys.used_memory() / (1024 * 1024);
        let available = sys.available_memory() / (1024 * 1024);
        ToolResult::ok(serde_json::json!({
            "total_mb": total,
            "used_mb": used,
            "available_mb": available,
            "usage_percent": format!("{:.1}", (used as f64 / total as f64) * 100.0),
        }))
    }
}

struct GetDiskSpace;
#[async_trait] impl ToolHandler for GetDiskSpace {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        let disks = sysinfo::Disks::new_with_refreshed_list();
        let info: Vec<serde_json::Value> = disks.iter().map(|d| {
            serde_json::json!({
                "mount": d.mount_point().to_string_lossy(),
                "name": d.name().to_string_lossy(),
                "total_gb": d.total_space() / (1024 * 1024 * 1024),
                "available_gb": d.available_space() / (1024 * 1024 * 1024),
                "fs_type": d.file_system().to_string_lossy().to_string(),
            })
        }).collect();
        ToolResult::ok(serde_json::json!({ "disks": info }))
    }
}

struct GetNetworkStatus;
#[async_trait] impl ToolHandler for GetNetworkStatus {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        let networks = sysinfo::Networks::new_with_refreshed_list();
        let info: Vec<serde_json::Value> = networks.iter().map(|(name, data)| {
            serde_json::json!({
                "interface": name,
                "received_bytes": data.total_received(),
                "transmitted_bytes": data.total_transmitted(),
            })
        }).collect();
        ToolResult::ok(serde_json::json!({ "interfaces": info }))
    }
}

struct GetBatteryStatus;
#[async_trait] impl ToolHandler for GetBatteryStatus {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        // Read from /sys/class/power_supply on Linux, or use sysinfo
        let bat_path = std::path::Path::new("/sys/class/power_supply/BAT0");
        if bat_path.exists() {
            let capacity = std::fs::read_to_string(bat_path.join("capacity"))
                .unwrap_or_default().trim().to_string();
            let status = std::fs::read_to_string(bat_path.join("status"))
                .unwrap_or_default().trim().to_string();
            ToolResult::ok(serde_json::json!({
                "percentage": capacity,
                "status": status,
            }))
        } else {
            ToolResult::ok(serde_json::json!({
                "status": "no_battery",
                "message": "No battery detected (desktop system)",
            }))
        }
    }
}

struct GetGpuInfo;
#[async_trait] impl ToolHandler for GetGpuInfo {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        match tokio::process::Command::new("nvidia-smi")
            .args(["--query-gpu=name,memory.total,memory.used,utilization.gpu",
                   "--format=csv,noheader,nounits"])
            .output().await {
            Ok(out) if out.status.success() => {
                let text = String::from_utf8_lossy(&out.stdout).to_string();
                ToolResult::ok_text(text)
            }
            _ => ToolResult::ok(serde_json::json!({ "gpu": "not detected or nvidia-smi not available" }))
        }
    }
}

struct GetSystemUptime;
#[async_trait] impl ToolHandler for GetSystemUptime {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        let uptime = sysinfo::System::uptime();
        let hours = uptime / 3600;
        let minutes = (uptime % 3600) / 60;
        ToolResult::ok(serde_json::json!({
            "uptime_seconds": uptime,
            "formatted": format!("{}h {}m", hours, minutes),
        }))
    }
}

// ─── Registration ───

pub fn register(reg: &mut ToolRegistry) {
    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        (ToolDef {
            name: "get_cpu_usage".into(),
            description: "Get current CPU usage percentage and core count".into(),
            category: "system_info".into(),
            parameters: vec![],
            default_tier: RiskLevel::Green,
            min_tier: "lite",
        }, Arc::new(GetCpuUsage)),

        (ToolDef {
            name: "get_memory_info".into(),
            description: "Get RAM usage: total, used, available in MB".into(),
            category: "system_info".into(),
            parameters: vec![],
            default_tier: RiskLevel::Green,
            min_tier: "lite",
        }, Arc::new(GetMemoryInfo)),

        (ToolDef {
            name: "get_disk_space".into(),
            description: "Get disk space for all mounted drives".into(),
            category: "system_info".into(),
            parameters: vec![],
            default_tier: RiskLevel::Green,
            min_tier: "lite",
        }, Arc::new(GetDiskSpace)),

        (ToolDef {
            name: "get_network_status".into(),
            description: "Get network interfaces and traffic stats".into(),
            category: "system_info".into(),
            parameters: vec![],
            default_tier: RiskLevel::Green,
            min_tier: "lite",
        }, Arc::new(GetNetworkStatus)),

        (ToolDef {
            name: "get_battery_status".into(),
            description: "Get battery level and charging status".into(),
            category: "system_info".into(),
            parameters: vec![],
            default_tier: RiskLevel::Green,
            min_tier: "lite",
        }, Arc::new(GetBatteryStatus)),

        (ToolDef {
            name: "get_gpu_info".into(),
            description: "Get GPU name, VRAM, and utilization (NVIDIA)".into(),
            category: "system_info".into(),
            parameters: vec![],
            default_tier: RiskLevel::Green,
            min_tier: "standard",
        }, Arc::new(GetGpuInfo)),

        (ToolDef {
            name: "get_system_uptime".into(),
            description: "Get system uptime in seconds and human-readable format".into(),
            category: "system_info".into(),
            parameters: vec![],
            default_tier: RiskLevel::Green,
            min_tier: "lite",
        }, Arc::new(GetSystemUptime)),
    ];

    for (def, handler) in tools {
        reg.register(def, handler);
    }
}
