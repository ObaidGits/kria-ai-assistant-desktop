use std::sync::Arc;
use async_trait::async_trait;
use crate::infra::ToolResult;
use crate::safety::RiskLevel;
use crate::tools::registry::{ToolRegistry, ToolDef, ToolHandler, ParamDef};

fn param(name: &str, ty: &str, desc: &str, required: bool) -> ParamDef {
    ParamDef { name: name.into(), param_type: ty.into(), description: desc.into(), required, default: None }
}

struct CleanTempFiles;
#[async_trait] impl ToolHandler for CleanTempFiles {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let older_than_days = params["older_than_days"].as_u64().unwrap_or(7);
        let temp_dir = std::env::temp_dir();
        let threshold = std::time::SystemTime::now() -
            std::time::Duration::from_secs(older_than_days * 86400);

        let mut deleted = 0u64;
        let mut freed_bytes = 0u64;

        if let Ok(entries) = std::fs::read_dir(&temp_dir) {
            for entry in entries.flatten() {
                if let Ok(meta) = entry.metadata() {
                    if let Ok(modified) = meta.modified() {
                        if modified < threshold {
                            freed_bytes += meta.len();
                            if meta.is_dir() {
                                let _ = std::fs::remove_dir_all(entry.path());
                            } else {
                                let _ = std::fs::remove_file(entry.path());
                            }
                            deleted += 1;
                        }
                    }
                }
            }
        }

        ToolResult::ok(serde_json::json!({
            "temp_dir": temp_dir.to_string_lossy(),
            "files_deleted": deleted,
            "freed_mb": freed_bytes / (1024 * 1024),
            "older_than_days": older_than_days,
        }))
    }
}

struct InstallApplication;
#[async_trait] impl ToolHandler for InstallApplication {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let name = params["name"].as_str().unwrap_or("");
        if name.is_empty() {
            return ToolResult::err("package name is required");
        }

        // Validate package name (only allow alphanumeric, dash, dot, underscore, plus)
        if !name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '.' || c == '_' || c == '+') {
            return ToolResult::err("invalid package name: only alphanumeric, dash, dot, underscore, + allowed");
        }

        let pm = crate::platform::detect::get_package_manager();
        let (cmd, args): (&str, Vec<&str>) = match pm {
            Some(crate::platform::detect::PackageManager::Apt) => ("apt", vec!["install", "-y", name]),
            Some(crate::platform::detect::PackageManager::Dnf) => ("dnf", vec!["install", "-y", name]),
            Some(crate::platform::detect::PackageManager::Pacman) => ("pacman", vec!["-S", "--noconfirm", name]),
            Some(crate::platform::detect::PackageManager::Brew) => ("brew", vec!["install", name]),
            _ => return ToolResult::err("no supported package manager found"),
        };

        // Check if sudo is available and non-interactive (NOPASSWD or cached credentials)
        // Skip sudo check for brew (doesn't need it)
        let use_sudo = !matches!(pm, Some(crate::platform::detect::PackageManager::Brew));
        if use_sudo {
            let sudo_check = tokio::process::Command::new("sudo")
                .args(["-n", "true"])
                .output()
                .await;
            match sudo_check {
                Ok(o) if !o.status.success() => {
                    return ToolResult::err(
                        "sudo requires a password. Please run 'sudo -v' in a terminal first to cache your credentials, or configure NOPASSWD for package management."
                    );
                }
                Err(e) => return ToolResult::err(format!("sudo not available: {e}")),
                _ => {}
            }
        }

        let output = if use_sudo {
            tokio::process::Command::new("sudo")
                .arg(cmd).args(&args)
                .output().await
        } else {
            tokio::process::Command::new(cmd)
                .args(&args)
                .output().await
        };

        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout).to_string();
                let stderr = String::from_utf8_lossy(&o.stderr).to_string();
                if o.status.success() {
                    ToolResult::ok(serde_json::json!({
                        "package": name,
                        "success": true,
                        "output": stdout,
                        "message": format!("Successfully installed '{}'", name),
                    }))
                } else {
                    ToolResult::err(format!("Installation failed for '{}': {}", name, if stderr.is_empty() { &stdout } else { &stderr }))
                }
            }
            Err(e) => ToolResult::err(format!("failed to run package manager: {e}"))
        }
    }
}

struct UninstallApplication;
#[async_trait] impl ToolHandler for UninstallApplication {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let name = params["name"].as_str().unwrap_or("");
        let pm = crate::platform::detect::get_package_manager();
        let (cmd, args): (&str, Vec<&str>) = match pm {
            Some(crate::platform::detect::PackageManager::Apt) => ("apt", vec!["remove", "-y", name]),
            Some(crate::platform::detect::PackageManager::Dnf) => ("dnf", vec!["remove", "-y", name]),
            Some(crate::platform::detect::PackageManager::Pacman) => ("pacman", vec!["-R", "--noconfirm", name]),
            Some(crate::platform::detect::PackageManager::Brew) => ("brew", vec!["uninstall", name]),
            _ => return ToolResult::err("no supported package manager found"),
        };

        let output = tokio::process::Command::new("sudo")
            .arg(cmd).args(&args)
            .output().await;
        match output {
            Ok(o) => ToolResult::ok(serde_json::json!({
                "package": name,
                "success": o.status.success(),
            })),
            Err(e) => ToolResult::err(format!("uninstall failed: {e}"))
        }
    }
}

pub fn register(reg: &mut ToolRegistry) {
    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        // RED
        (ToolDef {
            name: "clean_temp_files".into(), description: "Delete old temporary files".into(),
            category: "disk".into(), default_tier: RiskLevel::Red, min_tier: "lite",
            parameters: vec![param("older_than_days", "integer", "Only delete files older than N days (default 7)", false)],
        }, Arc::new(CleanTempFiles)),
        (ToolDef {
            name: "install_application".into(), description: "Install an application via the system package manager".into(),
            category: "disk".into(), default_tier: RiskLevel::Red, min_tier: "standard",
            parameters: vec![param("name", "string", "Package name", true)],
        }, Arc::new(InstallApplication)),
        (ToolDef {
            name: "uninstall_application".into(), description: "Uninstall an application via the system package manager".into(),
            category: "disk".into(), default_tier: RiskLevel::Red, min_tier: "standard",
            parameters: vec![param("name", "string", "Package name", true)],
        }, Arc::new(UninstallApplication)),
    ];
    for (def, handler) in tools { reg.register(def, handler); }
}
