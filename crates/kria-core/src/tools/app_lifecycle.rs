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

struct OpenApplication;
#[async_trait]
impl ToolHandler for OpenApplication {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let app = params["name"].as_str().unwrap_or("");
        let args: Vec<String> = params["args"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let result = if cfg!(target_os = "linux") {
            tokio::process::Command::new(app).args(&args).spawn()
        } else if cfg!(target_os = "windows") {
            tokio::process::Command::new("cmd")
                .args(["/C", "start", "", app])
                .args(&args)
                .spawn()
        } else {
            tokio::process::Command::new("open")
                .arg("-a")
                .arg(app)
                .args(&args)
                .spawn()
        };

        match result {
            Ok(child) => ToolResult::ok(serde_json::json!({
                "application": app,
                "pid": child.id(),
                "launched": true,
            })),
            Err(e) => ToolResult::err(format!("failed to open {app}: {e}")),
        }
    }
}

struct ListRunningApps;
#[async_trait]
impl ToolHandler for ListRunningApps {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        let mut sys = sysinfo::System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        let procs: Vec<serde_json::Value> = sys
            .processes()
            .iter()
            .filter(|(_, p)| !p.name().to_string_lossy().is_empty())
            .map(|(pid, p)| {
                serde_json::json!({
                    "pid": pid.as_u32(),
                    "name": p.name().to_string_lossy(),
                    "cpu_percent": format!("{:.1}", p.cpu_usage()),
                    "memory_mb": p.memory() / (1024 * 1024),
                })
            })
            .collect();
        ToolResult::ok(serde_json::json!({ "processes": procs, "count": procs.len() }))
    }
}

struct FocusWindow;
#[async_trait]
impl ToolHandler for FocusWindow {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let title = params["title"].as_str().unwrap_or("");
        if cfg!(target_os = "linux") {
            let output = tokio::process::Command::new("wmctrl")
                .args(["-a", title])
                .output()
                .await;
            match output {
                Ok(o) if o.status.success() => {
                    ToolResult::ok(serde_json::json!({ "focused": title }))
                }
                _ => ToolResult::err(format!(
                    "could not focus window '{title}' (wmctrl required)"
                )),
            }
        } else {
            ToolResult::err("focus_window not implemented for this OS")
        }
    }
}

struct CloseApplication;
#[async_trait]
impl ToolHandler for CloseApplication {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let name = params["name"].as_str().unwrap_or("");
        let mut sys = sysinfo::System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        let mut killed = 0;
        for proc_ in sys.processes().values() {
            if proc_
                .name()
                .to_string_lossy()
                .to_lowercase()
                .contains(&name.to_lowercase())
            {
                proc_.kill();
                killed += 1;
            }
        }
        if killed > 0 {
            ToolResult::ok(serde_json::json!({ "name": name, "processes_closed": killed }))
        } else {
            ToolResult::err(format!("no running process matched '{name}'"))
        }
    }
}

struct KillProcess;
#[async_trait]
impl ToolHandler for KillProcess {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let pid = params["pid"].as_u64().unwrap_or(0) as u32;
        let sys_pid = sysinfo::Pid::from_u32(pid);
        let mut sys = sysinfo::System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        if let Some(proc_) = sys.process(sys_pid) {
            proc_.kill();
            ToolResult::ok(serde_json::json!({ "pid": pid, "killed": true }))
        } else {
            ToolResult::err(format!("process {pid} not found"))
        }
    }
}

pub fn register(reg: &ToolRegistry) {
    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        (
            ToolDef {
                name: "open_application".into(),
                description: "Open/launch an application".into(),
                category: "app_lifecycle".into(),
                default_tier: RiskLevel::Green,
                min_tier: "lite",
                parameters: vec![
                    param("name", "string", "Application name or path", true),
                    param("args", "array", "Command-line arguments", false),
                ],
            },
            Arc::new(OpenApplication),
        ),
        (
            ToolDef {
                name: "list_running_apps".into(),
                description: "List all running processes with CPU and memory usage".into(),
                category: "app_lifecycle".into(),
                default_tier: RiskLevel::Green,
                min_tier: "lite",
                parameters: vec![],
            },
            Arc::new(ListRunningApps),
        ),
        (
            ToolDef {
                name: "focus_window".into(),
                description: "Bring a window to the foreground by title".into(),
                category: "app_lifecycle".into(),
                default_tier: RiskLevel::Green,
                min_tier: "standard",
                parameters: vec![param(
                    "title",
                    "string",
                    "Window title (partial match)",
                    true,
                )],
            },
            Arc::new(FocusWindow),
        ),
        (
            ToolDef {
                name: "close_application".into(),
                description: "Close an application by name".into(),
                category: "app_lifecycle".into(),
                default_tier: RiskLevel::Yellow,
                min_tier: "lite",
                parameters: vec![param("name", "string", "Application name", true)],
            },
            Arc::new(CloseApplication),
        ),
        (
            ToolDef {
                name: "kill_process".into(),
                description: "Kill a process by PID".into(),
                category: "app_lifecycle".into(),
                default_tier: RiskLevel::Yellow,
                min_tier: "lite",
                parameters: vec![param("pid", "integer", "Process ID", true)],
            },
            Arc::new(KillProcess),
        ),
    ];
    for (def, handler) in tools {
        reg.register(def, handler);
    }
}
