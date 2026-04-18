//! Intelligent package management tools.
//!
//! Provides 4 GREEN query tools + 2 RED action tools that the ReAct agent loop
//! chains together to give intelligent, safe install/uninstall behaviour:
//!
//!   search_package          → find packages across all available sources
//!   check_package_installed → is it already installed? what version?
//!   check_package_updates   → is a newer version available?
//!   get_package_info        → detailed metadata (maintainer, size, homepage …)
//!   install_package         → actually install  (RED — requires HITL approval)
//!   uninstall_package       → actually remove   (RED — requires HITL approval)

use crate::infra::ToolResult;
use crate::platform::detect::{
    get_available_package_managers, get_package_manager, PackageManager,
};
use crate::safety::RiskLevel;
use crate::tools::registry::{ParamDef, ToolDef, ToolHandler, ToolRegistry};
use async_trait::async_trait;
use std::sync::Arc;
use tracing::{error, info, warn};

fn param(name: &str, ty: &str, desc: &str, required: bool) -> ParamDef {
    ParamDef {
        name: name.into(),
        param_type: ty.into(),
        description: desc.into(),
        required,
        default: None,
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Validate a package name: alphanumeric, dash, dot, underscore, plus, slash (for flatpak refs).
fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("package name is required".into());
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || "-._+/@:".contains(c))
    {
        return Err("invalid package name: only alphanumeric and - . _ + / @ : are allowed".into());
    }
    Ok(())
}

/// Run a command and capture its stdout as a String.
async fn run_cmd(program: &str, args: &[&str]) -> Result<String, String> {
    let out = tokio::process::Command::new(program)
        .args(args)
        .output()
        .await
        .map_err(|e| format!("failed to run {program}: {e}"))?;

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    if out.status.success() || !stdout.is_empty() {
        // Many tools (snap, flatpak, apt) write their output to stderr only.
        // When stdout is empty, return stderr so callers can see what actually happened.
        let output = if stdout.trim().is_empty() {
            stderr.trim().to_string()
        } else {
            stdout
        };
        Ok(output)
    } else {
        Err(if stderr.is_empty() { stdout } else { stderr })
    }
}

/// Resolve the package manager to use, preferring an explicit `source` param.
fn resolve_pm(source: Option<&str>) -> Result<PackageManager, String> {
    if let Some(s) = source {
        let pm = match s.to_lowercase().as_str() {
            "apt" | "apt-get"  => PackageManager::Apt,
            "dnf" | "yum"      => PackageManager::Dnf,
            "pacman"           => PackageManager::Pacman,
            "zypper"           => PackageManager::Zypper,
            "brew" | "homebrew"=> PackageManager::Brew,
            "winget"           => PackageManager::Winget,
            "choco" | "chocolatey" => PackageManager::Choco,
            "snap"             => PackageManager::Snap,
            "flatpak"          => PackageManager::Flatpak,
            _ => return Err(format!("unknown package source '{s}'. Valid: apt, dnf, pacman, zypper, brew, winget, choco, snap, flatpak")),
        };
        return Ok(pm);
    }
    get_package_manager().ok_or_else(|| "no supported package manager found on this system".into())
}

/// Privilege escalation strategy for the current environment.
#[derive(Debug, Clone, Copy)]
enum PrivEsc {
    /// No elevation needed (Brew, Winget, Flatpak user-install).
    None,
    /// pkexec — shows a native graphical auth dialog (best for desktop Tauri app).
    Pkexec,
    /// sudo with cached credentials (NOPASSWD or already authenticated TTY session).
    Sudo,
}

/// Determine the best privilege escalation method available.
/// Priority: None (for PMs that don't need it) → pkexec → sudo -n.
async fn get_priv_esc(pm: PackageManager) -> Result<PrivEsc, String> {
    // Managers that don't need root at all.
    if matches!(
        pm,
        PackageManager::Brew | PackageManager::Winget | PackageManager::Flatpak
    ) {
        info!("[packages] PM {:?} needs no privilege escalation", pm);
        return Ok(PrivEsc::None);
    }

    // Try pkexec first — works in a desktop session even without TTY.
    let pkexec_check = tokio::process::Command::new("pkexec")
        .arg("--version")
        .output()
        .await;
    if pkexec_check.map(|o| o.status.success()).unwrap_or(false) {
        info!("[packages] pkexec available — will use for privilege escalation");
        return Ok(PrivEsc::Pkexec);
    }
    warn!("[packages] pkexec not found or failed, falling back to sudo");

    // Fall back to sudo -n (only works if NOPASSWD or credentials are cached).
    let sudo_check = tokio::process::Command::new("sudo")
        .args(["-n", "true"])
        .output()
        .await;
    match sudo_check {
        Ok(o) if o.status.success() => {
            info!("[packages] sudo -n succeeded — credentials cached or NOPASSWD configured");
            Ok(PrivEsc::Sudo)
        }
        Ok(_) => {
            error!("[packages] sudo requires a password and pkexec is unavailable");
            Err("Cannot escalate privileges: pkexec is not installed on this system and 'sudo' requires a password. \
                 Install policykit-1 (Ubuntu/Debian: sudo apt install policykit-1) to enable graphical privilege escalation, \
                 or run 'sudo -v' in a terminal to cache credentials.".into())
        }
        Err(e) => {
            error!("[packages] sudo check failed: {e}");
            Err(format!("Cannot escalate privileges: {e}"))
        }
    }
}

/// Build a command that runs `program args` with the given privilege escalation.
fn priv_cmd(priv_esc: PrivEsc, program: &str, args: &[&str]) -> tokio::process::Command {
    match priv_esc {
        PrivEsc::None => {
            let mut cmd = tokio::process::Command::new(program);
            cmd.args(args);
            cmd
        }
        PrivEsc::Pkexec => {
            let mut cmd = tokio::process::Command::new("pkexec");
            // Preserve DEBIAN_FRONTEND so apt doesn't prompt interactively.
            cmd.env("DEBIAN_FRONTEND", "noninteractive");
            cmd.arg(program);
            cmd.args(args);
            cmd
        }
        PrivEsc::Sudo => {
            let mut cmd = tokio::process::Command::new("sudo");
            cmd.env("DEBIAN_FRONTEND", "noninteractive");
            cmd.arg(program);
            cmd.args(args);
            cmd
        }
    }
}

/// Run a privileged command and return stdout, logging all details.
async fn run_priv_cmd(priv_esc: PrivEsc, program: &str, args: &[&str]) -> Result<String, String> {
    let display_cmd = format!("{program} {}", args.join(" "));
    info!("[packages] Running (priv={priv_esc:?}): {display_cmd}");

    let mut cmd = priv_cmd(priv_esc, program, args);
    let out = cmd.output().await.map_err(|e| {
        error!("[packages] Failed to spawn '{display_cmd}': {e}");
        format!("failed to spawn '{program}': {e}")
    })?;

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let code = out.status.code().unwrap_or(-1);

    info!("[packages] Command '{display_cmd}' exited with code {code}");
    if !stdout.is_empty() {
        info!("[packages] stdout: {}", &stdout[..stdout.len().min(500)]);
    }
    if !stderr.is_empty() {
        warn!("[packages] stderr: {}", &stderr[..stderr.len().min(500)]);
    }

    if out.status.success() {
        Ok(stdout)
    } else {
        let err_output = if !stderr.is_empty() { stderr } else { stdout };
        Err(format!("command exited with code {code}: {err_output}"))
    }
}

// ─── search_package ───────────────────────────────────────────────────────────

struct SearchPackage;

#[async_trait]
impl ToolHandler for SearchPackage {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let query = params
            .get("query")
            .and_then(|v| v.as_str())
            .or_else(|| params.get("name").and_then(|v| v.as_str()))
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .map(|q| q.to_string())
            .unwrap_or_default();
        if query.is_empty() {
            return ToolResult::err("query parameter is required (or provide name as alias)");
        }
        let source = params["source"].as_str();

        let pms: Vec<PackageManager> = if let Some(s) = source {
            match resolve_pm(Some(s)) {
                Ok(pm) => vec![pm],
                Err(e) => return ToolResult::err(e),
            }
        } else {
            get_available_package_managers()
        };

        if pms.is_empty() {
            return ToolResult::err("no package managers found on this system");
        }

        let mut all_results: Vec<serde_json::Value> = Vec::new();

        for pm in pms {
            let results = search_with_pm(pm, &query).await;
            all_results.extend(results);
        }

        ToolResult::ok(serde_json::json!({
            "query": query,
            "count": all_results.len(),
            "results": all_results,
        }))
    }
}

async fn search_with_pm(pm: PackageManager, query: &str) -> Vec<serde_json::Value> {
    let source = pm.as_str();
    match pm {
        PackageManager::Apt => match run_cmd("apt-cache", &["search", query]).await {
            Ok(out) => out
                .lines()
                .filter(|l| !l.is_empty())
                .take(20)
                .map(|line| {
                    let parts: Vec<&str> = line.splitn(2, " - ").collect();
                    serde_json::json!({
                        "name": parts.first().unwrap_or(&"").trim(),
                        "description": parts.get(1).unwrap_or(&"").trim(),
                        "source": source,
                    })
                })
                .collect(),
            Err(_) => vec![],
        },
        PackageManager::Dnf => match run_cmd("dnf", &["search", query]).await {
            Ok(out) => out
                .lines()
                .filter(|l| l.contains('.') && !l.starts_with('=') && !l.starts_with(' '))
                .take(20)
                .map(|line| {
                    let parts: Vec<&str> = line.splitn(2, " : ").collect();
                    serde_json::json!({
                        "name": parts.first().unwrap_or(&"").trim()
                            .split_once('.').map(|(n,_)| n).unwrap_or(parts.first().unwrap_or(&"")),
                        "description": parts.get(1).unwrap_or(&"").trim(),
                        "source": source,
                    })
                })
                .collect(),
            Err(_) => vec![],
        },
        PackageManager::Pacman => match run_cmd("pacman", &["-Ss", query]).await {
            Ok(out) => {
                let mut results = Vec::new();
                let mut lines = out.lines();
                while let Some(pkg_line) = lines.next() {
                    let desc = lines.next().map(|l| l.trim()).unwrap_or("");
                    if let Some(name_part) = pkg_line.split_whitespace().next() {
                        let name = name_part.split('/').last().unwrap_or(name_part);
                        results.push(serde_json::json!({
                            "name": name,
                            "description": desc,
                            "source": source,
                        }));
                    }
                    if results.len() >= 20 {
                        break;
                    }
                }
                results
            }
            Err(_) => vec![],
        },
        PackageManager::Zypper => {
            match run_cmd("zypper", &["search", query]).await {
                Ok(out) => out
                    .lines()
                    .filter(|l| l.starts_with('|') || l.starts_with('+'))
                    .skip(1) // header
                    .take(20)
                    .map(|line| {
                        let cols: Vec<&str> = line.split('|').map(|s| s.trim()).collect();
                        serde_json::json!({
                            "name": cols.get(2).unwrap_or(&"").trim(),
                            "description": cols.get(4).unwrap_or(&"").trim(),
                            "source": source,
                        })
                    })
                    .collect(),
                Err(_) => vec![],
            }
        }
        PackageManager::Brew => {
            // brew search returns formulae and casks separated by headers
            match run_cmd("brew", &["search", "--formula", query]).await {
                Ok(formula_out) => {
                    let mut results: Vec<serde_json::Value> = formula_out
                        .lines()
                        .filter(|l| !l.is_empty() && !l.starts_with('='))
                        .take(10)
                        .map(|name| {
                            serde_json::json!({
                                "name": name.trim(),
                                "description": "",
                                "source": "brew-formula",
                            })
                        })
                        .collect();
                    // also check casks
                    if let Ok(cask_out) = run_cmd("brew", &["search", "--cask", query]).await {
                        let casks: Vec<serde_json::Value> = cask_out
                            .lines()
                            .filter(|l| !l.is_empty() && !l.starts_with('='))
                            .take(10)
                            .map(|name| {
                                serde_json::json!({
                                    "name": name.trim(),
                                    "description": "",
                                    "source": "brew-cask",
                                })
                            })
                            .collect();
                        results.extend(casks);
                    }
                    results
                }
                Err(_) => vec![],
            }
        }
        PackageManager::Winget => {
            match run_cmd("winget", &["search", query, "--accept-source-agreements"]).await {
                Ok(out) => out
                    .lines()
                    .skip(2) // skip header + separator
                    .filter(|l| !l.is_empty() && !l.chars().all(|c| c == '-' || c == ' '))
                    .take(20)
                    .map(|line| {
                        let cols: Vec<&str> = line.split_whitespace().collect();
                        serde_json::json!({
                            "name": cols.first().unwrap_or(&"").to_string(),
                            "description": cols.get(1).map(|s| s.to_string()).unwrap_or_default(),
                            "source": source,
                        })
                    })
                    .collect(),
                Err(_) => vec![],
            }
        }
        PackageManager::Choco => match run_cmd("choco", &["search", query]).await {
            Ok(out) => out
                .lines()
                .filter(|l| {
                    !l.is_empty() && !l.starts_with("Chocolatey") && !l.contains("packages found")
                })
                .take(20)
                .map(|line| {
                    let parts: Vec<&str> = line.splitn(2, ' ').collect();
                    serde_json::json!({
                        "name": parts.first().unwrap_or(&"").trim(),
                        "description": parts.get(1).unwrap_or(&"").trim(),
                        "source": source,
                    })
                })
                .collect(),
            Err(_) => vec![],
        },
        PackageManager::Snap => {
            match run_cmd("snap", &["find", query]).await {
                Ok(out) => out
                    .lines()
                    .skip(1) // skip header
                    .filter(|l| !l.is_empty())
                    .take(20)
                    .map(|line| {
                        let cols: Vec<&str> = line.splitn(4, '\t').collect();
                        let cols_ws: Vec<&str> = line.split_whitespace().collect();
                        serde_json::json!({
                            "name": cols.first().or_else(|| cols_ws.first()).unwrap_or(&"").trim(),
                            "description": cols.get(3).unwrap_or(&"").trim(),
                            "source": source,
                        })
                    })
                    .collect(),
                Err(_) => vec![],
            }
        }
        PackageManager::Flatpak => {
            match run_cmd(
                "flatpak",
                &["search", "--columns=name,application,description", query],
            )
            .await
            {
                Ok(out) => out
                    .lines()
                    .skip(1)
                    .filter(|l| !l.is_empty())
                    .take(20)
                    .map(|line| {
                        let cols: Vec<&str> = line.splitn(3, '\t').collect();
                        serde_json::json!({
                            "name": cols.first().unwrap_or(&"").trim(),
                            "description": cols.get(2).unwrap_or(&"").trim(),
                            "source": source,
                        })
                    })
                    .collect(),
                Err(_) => vec![],
            }
        }
    }
}

// ─── check_package_installed ──────────────────────────────────────────────────

struct CheckPackageInstalled;

#[async_trait]
impl ToolHandler for CheckPackageInstalled {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let name = match params["name"].as_str() {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => return ToolResult::err("name parameter is required"),
        };

        if let Err(e) = validate_name(&name) {
            return ToolResult::err(e);
        }

        let pms = get_available_package_managers();
        if pms.is_empty() {
            // Fallback: check if binary exists in PATH
            let in_path = tokio::process::Command::new("which")
                .arg(&name)
                .output()
                .await
                .map(|o| o.status.success())
                .unwrap_or(false);
            return ToolResult::ok(serde_json::json!({
                "name": name,
                "installed": in_path,
                "version": serde_json::Value::Null,
                "source": if in_path { serde_json::json!("PATH") } else { serde_json::Value::Null },
            }));
        }

        for pm in &pms {
            if let Some(result) = check_installed_with_pm(*pm, &name).await {
                return ToolResult::ok(result);
            }
        }

        // Final fallback: which/where
        let in_path =
            tokio::process::Command::new(if cfg!(windows) { "where.exe" } else { "which" })
                .arg(&name)
                .output()
                .await
                .map(|o| o.status.success())
                .unwrap_or(false);

        ToolResult::ok(serde_json::json!({
            "name": name,
            "installed": in_path,
            "version": null,
        "source": if in_path { serde_json::Value::String("PATH".into()) } else { serde_json::Value::Null },
        }))
    }
}

async fn check_installed_with_pm(pm: PackageManager, name: &str) -> Option<serde_json::Value> {
    match pm {
        PackageManager::Apt => {
            let out = run_cmd("dpkg-query", &["-W", "-f=${Status} ${Version}", name])
                .await
                .ok()?;
            if out.contains("install ok installed") {
                let version = out.split_whitespace().last().map(|s| s.to_string());
                Some(serde_json::json!({
                    "name": name,
                    "installed": true,
                    "version": version,
                    "source": "apt",
                }))
            } else {
                None
            }
        }
        PackageManager::Dnf => {
            let out = run_cmd("dnf", &["list", "installed", name]).await.ok()?;
            if out.lines().any(|l| l.starts_with(name)) {
                let version = out
                    .lines()
                    .find(|l| l.starts_with(name))
                    .and_then(|l| l.split_whitespace().nth(1))
                    .map(|s| s.to_string());
                Some(serde_json::json!({
                    "name": name,
                    "installed": true,
                    "version": version,
                    "source": "dnf",
                }))
            } else {
                None
            }
        }
        PackageManager::Pacman => {
            let out = run_cmd("pacman", &["-Q", name]).await.ok()?;
            if !out.is_empty() && !out.contains("was not found") {
                let version = out.split_whitespace().nth(1).map(|s| s.to_string());
                Some(serde_json::json!({
                    "name": name,
                    "installed": true,
                    "version": version,
                    "source": "pacman",
                }))
            } else {
                None
            }
        }
        PackageManager::Zypper => {
            let out = run_cmd("rpm", &["-q", name]).await.ok()?;
            if out.contains(name) && !out.contains("not installed") {
                Some(serde_json::json!({
                    "name": name,
                    "installed": true,
                    "version": out.trim().to_string(),
                    "source": "zypper",
                }))
            } else {
                None
            }
        }
        PackageManager::Brew => {
            let out = run_cmd("brew", &["list", "--versions", name]).await.ok()?;
            if !out.trim().is_empty() {
                let version = out.split_whitespace().nth(1).map(|s| s.to_string());
                Some(serde_json::json!({
                    "name": name,
                    "installed": true,
                    "version": version,
                    "source": "brew",
                }))
            } else {
                None
            }
        }
        PackageManager::Winget => {
            let out = run_cmd("winget", &["list", name]).await.ok()?;
            if out
                .lines()
                .skip(2)
                .any(|l| l.to_lowercase().contains(&name.to_lowercase()))
            {
                Some(serde_json::json!({
                    "name": name,
                    "installed": true,
                    "version": null,
                    "source": "winget",
                }))
            } else {
                None
            }
        }
        PackageManager::Choco => {
            let out = run_cmd("choco", &["list", "--local-only", name])
                .await
                .ok()?;
            if out
                .lines()
                .any(|l| l.to_lowercase().starts_with(&name.to_lowercase()))
            {
                let version = out
                    .lines()
                    .find(|l| l.to_lowercase().starts_with(&name.to_lowercase()))
                    .and_then(|l| l.split_whitespace().nth(1))
                    .map(|s| s.to_string());
                Some(serde_json::json!({
                    "name": name,
                    "installed": true,
                    "version": version,
                    "source": "choco",
                }))
            } else {
                None
            }
        }
        PackageManager::Snap => {
            let out = run_cmd("snap", &["list", name]).await.ok()?;
            if out.lines().skip(1).any(|l| l.starts_with(name)) {
                let version = out
                    .lines()
                    .nth(1)
                    .and_then(|l| l.split_whitespace().nth(1))
                    .map(|s| s.to_string());
                Some(serde_json::json!({
                    "name": name,
                    "installed": true,
                    "version": version,
                    "source": "snap",
                }))
            } else {
                None
            }
        }
        PackageManager::Flatpak => {
            let out = run_cmd("flatpak", &["list", "--columns=name,version"])
                .await
                .ok()?;
            let found = out
                .lines()
                .find(|l| l.to_lowercase().contains(&name.to_lowercase()));
            if let Some(line) = found {
                let cols: Vec<&str> = line.splitn(2, '\t').collect();
                Some(serde_json::json!({
                    "name": cols.first().unwrap_or(&name).trim(),
                    "installed": true,
                    "version": cols.get(1).map(|s| s.trim().to_string()),
                    "source": "flatpak",
                }))
            } else {
                None
            }
        }
    }
}

// ─── check_package_updates ────────────────────────────────────────────────────

struct CheckPackageUpdates;

#[async_trait]
impl ToolHandler for CheckPackageUpdates {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let name = match params["name"].as_str() {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => return ToolResult::err("name parameter is required"),
        };

        if let Err(e) = validate_name(&name) {
            return ToolResult::err(e);
        }

        let pm = match get_package_manager() {
            Some(p) => p,
            None => return ToolResult::err("no package manager found"),
        };

        let result = check_updates_with_pm(pm, &name).await;
        ToolResult::ok(result)
    }
}

async fn check_updates_with_pm(pm: PackageManager, name: &str) -> serde_json::Value {
    match pm {
        PackageManager::Apt => {
            // apt list --upgradable shows packages with available upgrades
            let upgradable = run_cmd("apt", &["list", "--upgradable"])
                .await
                .unwrap_or_default();
            let line = upgradable.lines().find(|l| l.starts_with(name));
            if let Some(l) = line {
                // format: "name/repo version arch [upgradable from: old_ver]"
                let parts: Vec<&str> = l.split_whitespace().collect();
                let latest = parts.get(1).map(|s| s.to_string());
                let installed = l
                    .split("upgradable from: ")
                    .nth(1)
                    .and_then(|s| s.strip_suffix(']'))
                    .map(|s| s.to_string());
                serde_json::json!({
                    "name": name,
                    "update_available": true,
                    "installed_version": installed,
                    "latest_version": latest,
                    "source": "apt",
                })
            } else {
                // Package is up to date (or not installed)
                let installed = run_cmd("dpkg-query", &["-W", "-f=${Version}", name])
                    .await
                    .ok()
                    .filter(|s| !s.is_empty());
                serde_json::json!({
                    "name": name,
                    "update_available": false,
                    "installed_version": installed,
                    "latest_version": null,
                    "source": "apt",
                })
            }
        }
        PackageManager::Dnf => {
            let out = run_cmd("dnf", &["check-update", name])
                .await
                .unwrap_or_default();
            let update_line = out.lines().find(|l| l.starts_with(name));
            let update_available = update_line.is_some();
            let latest = update_line
                .and_then(|l| l.split_whitespace().nth(1))
                .map(|s| s.to_string());
            serde_json::json!({
                "name": name,
                "update_available": update_available,
                "installed_version": null,
                "latest_version": latest,
                "source": "dnf",
            })
        }
        PackageManager::Pacman => {
            // checkupdates from pacman-contrib if available, otherwise pacman -Qu
            let out = run_cmd("pacman", &["-Qu", name]).await.unwrap_or_default();
            let update_line = out.lines().find(|l| l.starts_with(name));
            let update_available = update_line.is_some();
            // pacman -Qu: "name old_ver -> new_ver"
            let (installed, latest) = if let Some(l) = update_line {
                let parts: Vec<&str> = l.split_whitespace().collect();
                (
                    parts.get(1).map(|s| s.to_string()),
                    parts.get(3).map(|s| s.to_string()),
                )
            } else {
                (None, None)
            };
            serde_json::json!({
                "name": name,
                "update_available": update_available,
                "installed_version": installed,
                "latest_version": latest,
                "source": "pacman",
            })
        }
        PackageManager::Zypper => {
            let out = run_cmd("zypper", &["list-updates"])
                .await
                .unwrap_or_default();
            let update_line = out.lines().find(|l| l.contains(name));
            let update_available = update_line.is_some();
            serde_json::json!({
                "name": name,
                "update_available": update_available,
                "installed_version": null,
                "latest_version": null,
                "source": "zypper",
            })
        }
        PackageManager::Brew => {
            let out = run_cmd("brew", &["outdated", "--verbose"])
                .await
                .unwrap_or_default();
            let update_line = out.lines().find(|l| l.starts_with(name));
            if let Some(l) = update_line {
                // "name (installed_ver) < latest_ver"
                let installed = l
                    .split('(')
                    .nth(1)
                    .and_then(|s| s.split(')').next())
                    .map(|s| s.to_string());
                let latest = l.split("< ").nth(1).map(|s| s.trim().to_string());
                serde_json::json!({
                    "name": name,
                    "update_available": true,
                    "installed_version": installed,
                    "latest_version": latest,
                    "source": "brew",
                })
            } else {
                serde_json::json!({
                    "name": name,
                    "update_available": false,
                    "installed_version": null,
                    "latest_version": null,
                    "source": "brew",
                })
            }
        }
        PackageManager::Winget => {
            let out = run_cmd("winget", &["upgrade", name])
                .await
                .unwrap_or_default();
            let update_available =
                !out.contains("No applicable upgrade found") && out.lines().count() > 3;
            serde_json::json!({
                "name": name,
                "update_available": update_available,
                "installed_version": null,
                "latest_version": null,
                "source": "winget",
            })
        }
        PackageManager::Choco => {
            let out = run_cmd("choco", &["outdated"]).await.unwrap_or_default();
            let update_line = out
                .lines()
                .find(|l| l.to_lowercase().starts_with(&name.to_lowercase()));
            let update_available = update_line.is_some();
            let (installed, latest) = if let Some(l) = update_line {
                let cols: Vec<&str> = l.split('|').collect();
                (
                    cols.get(1).map(|s| s.trim().to_string()),
                    cols.get(2).map(|s| s.trim().to_string()),
                )
            } else {
                (None, None)
            };
            serde_json::json!({
                "name": name,
                "update_available": update_available,
                "installed_version": installed,
                "latest_version": latest,
                "source": "choco",
            })
        }
        PackageManager::Snap => {
            let out = run_cmd("snap", &["refresh", "--list"])
                .await
                .unwrap_or_default();
            let update_available = out.lines().any(|l| l.starts_with(name));
            serde_json::json!({
                "name": name,
                "update_available": update_available,
                "installed_version": null,
                "latest_version": null,
                "source": "snap",
            })
        }
        PackageManager::Flatpak => {
            let out = run_cmd("flatpak", &["remote-ls", "--updates"])
                .await
                .unwrap_or_default();
            let update_available = out
                .lines()
                .any(|l| l.to_lowercase().contains(&name.to_lowercase()));
            serde_json::json!({
                "name": name,
                "update_available": update_available,
                "installed_version": null,
                "latest_version": null,
                "source": "flatpak",
            })
        }
    }
}

// ─── get_package_info ─────────────────────────────────────────────────────────

struct GetPackageInfo;

#[async_trait]
impl ToolHandler for GetPackageInfo {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let name = match params["name"].as_str() {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => return ToolResult::err("name parameter is required"),
        };
        let source = params["source"].as_str();

        if let Err(e) = validate_name(&name) {
            return ToolResult::err(e);
        }

        let pm = match resolve_pm(source) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(e),
        };

        let info = get_info_with_pm(pm, &name).await;
        ToolResult::ok(info)
    }
}

async fn get_info_with_pm(pm: PackageManager, name: &str) -> serde_json::Value {
    match pm {
        PackageManager::Apt => {
            let out = run_cmd("apt-cache", &["show", name])
                .await
                .unwrap_or_default();
            if out.is_empty() {
                return serde_json::json!({"name": name, "found": false, "source": "apt"});
            }
            let field = |key: &str| -> Option<String> {
                out.lines()
                    .find(|l| l.starts_with(key))
                    .map(|l| l.splitn(2, ": ").nth(1).unwrap_or("").trim().to_string())
            };
            serde_json::json!({
                "name": name,
                "found": true,
                "version": field("Version"),
                "description": field("Description"),
                "maintainer": field("Maintainer"),
                "homepage": field("Homepage"),
                "size": field("Installed-Size").map(|s| format!("{s} kB")),
                "section": field("Section"),
                "source": "apt",
            })
        }
        PackageManager::Dnf => {
            let out = run_cmd("dnf", &["info", name]).await.unwrap_or_default();
            if out.is_empty() || out.contains("No matching Packages") {
                return serde_json::json!({"name": name, "found": false, "source": "dnf"});
            }
            let field = |key: &str| -> Option<String> {
                out.lines()
                    .find(|l| l.starts_with(key))
                    .map(|l| l.splitn(2, ": ").nth(1).unwrap_or("").trim().to_string())
            };
            serde_json::json!({
                "name": name,
                "found": true,
                "version": field("Version"),
                "description": field("Summary"),
                "maintainer": field("Packager"),
                "homepage": field("URL"),
                "source": "dnf",
            })
        }
        PackageManager::Pacman => {
            let out = run_cmd("pacman", &["-Si", name])
                .await
                .unwrap_or_else(|_e| String::new());
            let out = if out.is_empty() {
                run_cmd("pacman", &["-Qi", name]).await.unwrap_or_default()
            } else {
                out
            };
            if out.is_empty() {
                return serde_json::json!({"name": name, "found": false, "source": "pacman"});
            }
            let field = |key: &str| -> Option<String> {
                out.lines()
                    .find(|l| l.starts_with(key))
                    .map(|l| l.splitn(2, ": ").nth(1).unwrap_or("").trim().to_string())
            };
            serde_json::json!({
                "name": name,
                "found": true,
                "version": field("Version"),
                "description": field("Description"),
                "homepage": field("URL"),
                "source": "pacman",
            })
        }
        PackageManager::Zypper => {
            let out = run_cmd("zypper", &["info", name]).await.unwrap_or_default();
            let field = |key: &str| -> Option<String> {
                out.lines()
                    .find(|l| l.contains(key))
                    .map(|l| l.splitn(2, ": ").nth(1).unwrap_or("").trim().to_string())
            };
            serde_json::json!({
                "name": name,
                "found": !out.is_empty(),
                "version": field("Version"),
                "description": field("Summary"),
                "homepage": field("Homepage"),
                "source": "zypper",
            })
        }
        PackageManager::Brew => {
            let out = run_cmd("brew", &["info", "--json=v2", name])
                .await
                .unwrap_or_default();
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&out) {
                // Try formulae first, then casks
                let formula = json["formulae"]
                    .as_array()
                    .and_then(|a| a.first())
                    .or_else(|| json["casks"].as_array().and_then(|a| a.first()));
                if let Some(f) = formula {
                    return serde_json::json!({
                        "name": name,
                        "found": true,
                        "version": f["versions"]["stable"].as_str().or_else(|| f["version"].as_str()),
                        "description": f["desc"].as_str(),
                        "homepage": f["homepage"].as_str(),
                        "source": "brew",
                    });
                }
            }
            serde_json::json!({"name": name, "found": false, "source": "brew"})
        }
        PackageManager::Winget => {
            let out = run_cmd("winget", &["show", name]).await.unwrap_or_default();
            serde_json::json!({
                "name": name,
                "found": !out.is_empty(),
                "raw_info": &out[..out.len().min(2000)],
                "source": "winget",
            })
        }
        PackageManager::Choco => {
            let out = run_cmd("choco", &["info", name]).await.unwrap_or_default();
            serde_json::json!({
                "name": name,
                "found": !out.is_empty() && !out.contains("0 packages found"),
                "raw_info": &out[..out.len().min(2000)],
                "source": "choco",
            })
        }
        PackageManager::Snap => {
            let out = run_cmd("snap", &["info", name]).await.unwrap_or_default();
            let field = |key: &str| -> Option<String> {
                out.lines()
                    .find(|l| l.starts_with(key))
                    .map(|l| l.splitn(2, ": ").nth(1).unwrap_or("").trim().to_string())
            };
            serde_json::json!({
                "name": name,
                "found": !out.is_empty() && !out.contains("snap not found"),
                "version": field("snap-id"),
                "description": field("summary"),
                "publisher": field("publisher"),
                "homepage": field("contact"),
                "source": "snap",
            })
        }
        PackageManager::Flatpak => {
            let out = run_cmd(
                "flatpak",
                &[
                    "search",
                    "--columns=name,application,version,description",
                    name,
                ],
            )
            .await
            .unwrap_or_default();
            let line = out
                .lines()
                .skip(1)
                .find(|l| l.to_lowercase().contains(&name.to_lowercase()));
            if let Some(l) = line {
                let cols: Vec<&str> = l.splitn(4, '\t').collect();
                serde_json::json!({
                    "name": cols.first().unwrap_or(&name).trim(),
                    "found": true,
                    "app_id": cols.get(1).map(|s| s.trim()),
                    "version": cols.get(2).map(|s| s.trim()),
                    "description": cols.get(3).map(|s| s.trim()),
                    "source": "flatpak",
                })
            } else {
                serde_json::json!({"name": name, "found": false, "source": "flatpak"})
            }
        }
    }
}

// ─── install_package (RED) ────────────────────────────────────────────────────

struct InstallPackage;

#[async_trait]
impl ToolHandler for InstallPackage {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let name = match params["name"].as_str() {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => return ToolResult::err("name parameter is required"),
        };
        let source = params["source"].as_str();

        if let Err(e) = validate_name(&name) {
            return ToolResult::err(e);
        }

        let pm = match resolve_pm(source) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(e),
        };

        info!(
            "[packages] install_package: name={name} source={source:?} pm={:?}",
            pm
        );

        let priv_esc = match get_priv_esc(pm).await {
            Ok(p) => p,
            Err(e) => return ToolResult::err(e),
        };
        info!("[packages] install_package: using priv_esc={priv_esc:?}");

        let output = run_install(pm, &name, priv_esc).await;

        match output {
            Ok(out) => {
                info!("[packages] install_package: SUCCESS for '{name}'");
                ToolResult::ok(serde_json::json!({
                    "package": name,
                    "success": true,
                    "source": pm.as_str(),
                    "output": &out[..out.len().min(2000)],
                    "message": format!("Successfully installed '{name}'"),
                }))
            }
            Err(e) => {
                error!("[packages] install_package: FAILED for '{name}': {e}");
                ToolResult::err(format!("Installation of '{name}' failed: {e}"))
            }
        }
    }
}

async fn run_install(pm: PackageManager, name: &str, priv_esc: PrivEsc) -> Result<String, String> {
    info!("[packages] run_install: pm={pm:?} name={name} priv_esc={priv_esc:?}");
    match pm {
        PackageManager::Apt => run_priv_cmd(priv_esc, "apt-get", &["install", "-y", name]).await,
        PackageManager::Dnf => run_priv_cmd(priv_esc, "dnf", &["install", "-y", name]).await,
        PackageManager::Pacman => {
            run_priv_cmd(priv_esc, "pacman", &["-S", "--noconfirm", name]).await
        }
        PackageManager::Zypper => run_priv_cmd(priv_esc, "zypper", &["install", "-y", name]).await,
        PackageManager::Brew => {
            info!("[packages] brew install: trying formula first");
            match run_cmd("brew", &["install", name]).await {
                Ok(out) => Ok(out),
                Err(e) => {
                    warn!("[packages] brew install formula failed ({e}), retrying as cask");
                    run_cmd("brew", &["install", "--cask", name]).await
                }
            }
        }
        PackageManager::Winget => {
            run_cmd(
                "winget",
                &[
                    "install",
                    "--accept-source-agreements",
                    "--accept-package-agreements",
                    name,
                ],
            )
            .await
        }
        PackageManager::Choco => run_cmd("choco", &["install", "-y", name]).await,
        PackageManager::Snap => {
            // snapd may allow install without root via the snapd socket;
            // try without sudo first, fall back to privileged if it fails.
            info!("[packages] snap install: trying without privilege escalation first");
            match run_cmd("snap", &["install", name]).await {
                Ok(out) => Ok(out),
                Err(e) => {
                    warn!("[packages] snap install without privilege failed ({e}), retrying with escalation");
                    run_priv_cmd(priv_esc, "snap", &["install", name]).await
                }
            }
        }
        PackageManager::Flatpak => {
            // Try user install (no root) first.
            info!("[packages] flatpak install: trying --user install first");
            match run_cmd("flatpak", &["install", "-y", "--user", name]).await {
                Ok(out) => Ok(out),
                Err(e) => {
                    warn!("[packages] flatpak --user install failed ({e}), retrying system-wide");
                    run_priv_cmd(priv_esc, "flatpak", &["install", "-y", name]).await
                }
            }
        }
    }
}

// ─── uninstall_package (RED) ──────────────────────────────────────────────────

struct UninstallPackage;

#[async_trait]
impl ToolHandler for UninstallPackage {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let name = match params["name"].as_str() {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => return ToolResult::err("name parameter is required"),
        };
        let source = params["source"].as_str();

        if let Err(e) = validate_name(&name) {
            return ToolResult::err(e);
        }

        let pm = match resolve_pm(source) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(e),
        };

        info!(
            "[packages] uninstall_package: name={name} source={source:?} pm={:?}",
            pm
        );

        let priv_esc = match get_priv_esc(pm).await {
            Ok(p) => p,
            Err(e) => return ToolResult::err(e),
        };
        info!("[packages] uninstall_package: using priv_esc={priv_esc:?}");

        let output = run_uninstall(pm, &name, priv_esc).await;

        match output {
            Ok(out) => {
                info!("[packages] uninstall_package: SUCCESS for '{name}'");
                ToolResult::ok(serde_json::json!({
                    "package": name,
                    "success": true,
                    "source": pm.as_str(),
                    "output": &out[..out.len().min(2000)],
                    "message": format!("Successfully uninstalled '{name}'"),
                }))
            }
            Err(e) => {
                error!("[packages] uninstall_package: FAILED for '{name}': {e}");
                ToolResult::err(format!("Uninstallation of '{name}' failed: {e}"))
            }
        }
    }
}

async fn run_uninstall(
    pm: PackageManager,
    name: &str,
    priv_esc: PrivEsc,
) -> Result<String, String> {
    info!("[packages] run_uninstall: pm={pm:?} name={name} priv_esc={priv_esc:?}");
    match pm {
        PackageManager::Apt => run_priv_cmd(priv_esc, "apt-get", &["remove", "-y", name]).await,
        PackageManager::Dnf => run_priv_cmd(priv_esc, "dnf", &["remove", "-y", name]).await,
        PackageManager::Pacman => {
            run_priv_cmd(priv_esc, "pacman", &["-R", "--noconfirm", name]).await
        }
        PackageManager::Zypper => run_priv_cmd(priv_esc, "zypper", &["remove", "-y", name]).await,
        PackageManager::Brew => match run_cmd("brew", &["uninstall", name]).await {
            Ok(out) => Ok(out),
            Err(e) => {
                warn!("[packages] brew uninstall formula failed ({e}), retrying as cask");
                run_cmd("brew", &["uninstall", "--cask", name]).await
            }
        },
        PackageManager::Winget => run_cmd("winget", &["uninstall", name]).await,
        PackageManager::Choco => run_cmd("choco", &["uninstall", "-y", name]).await,
        PackageManager::Snap => {
            // snap exits 0 even when the package is not installed, writing
            // "snap '<name>' is not installed" to stderr. After our run_cmd
            // fix, that message comes back as Ok(msg). Treat it as an error
            // so the caller sees the real outcome instead of silent success.
            match run_cmd("snap", &["remove", name]).await {
                Ok(out) if out.contains("is not installed") => {
                    warn!("[packages] snap remove: {out}");
                    Err(out)
                }
                Ok(out) => Ok(out),
                Err(e) => {
                    warn!("[packages] snap remove without privilege failed ({e}), retrying with escalation");
                    run_priv_cmd(priv_esc, "snap", &["remove", name]).await
                }
            }
        }
        PackageManager::Flatpak => {
            match run_cmd("flatpak", &["uninstall", "-y", "--user", name]).await {
                Ok(out) => Ok(out),
                Err(e) => {
                    warn!("[packages] flatpak --user uninstall failed ({e}), retrying system-wide");
                    run_priv_cmd(priv_esc, "flatpak", &["uninstall", "-y", name]).await
                }
            }
        }
    }
}

// ─── Register all tools ───────────────────────────────────────────────────────

pub fn register(reg: &ToolRegistry) {
    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        // ── GREEN: query tools (auto-execute, no approval needed) ──
        (ToolDef {
            name: "search_package".into(),
            description: "Search for a package/application across all available package managers (apt, dnf, snap, flatpak, brew, winget, choco). Returns matching package names, descriptions and sources. Always call this before installing.".into(),
            category: "packages".into(),
            default_tier: RiskLevel::Green,
            min_tier: "lite",
            parameters: vec![
                param("query",  "string", "Package name or keyword to search for", true),
                param("name",   "string", "Alias for query (for compatibility with older calls)", false),
                param("source", "string", "Specific source to search: apt, dnf, pacman, zypper, brew, winget, choco, snap, flatpak. Omit to search all available sources.", false),
            ],
        }, Arc::new(SearchPackage)),
        (ToolDef {
            name: "check_package_installed".into(),
            description: "Check whether a package is already installed and get its current version. Always call this before installing to avoid reinstalling existing packages.".into(),
            category: "packages".into(),
            default_tier: RiskLevel::Green,
            min_tier: "lite",
            parameters: vec![
                param("name", "string", "Exact package name to check", true),
            ],
        }, Arc::new(CheckPackageInstalled)),
        (ToolDef {
            name: "check_package_updates".into(),
            description: "Check whether a newer version is available for an installed package. Returns installed and latest version numbers.".into(),
            category: "packages".into(),
            default_tier: RiskLevel::Green,
            min_tier: "lite",
            parameters: vec![
                param("name", "string", "Package name to check for updates", true),
            ],
        }, Arc::new(CheckPackageUpdates)),
        (ToolDef {
            name: "get_package_info".into(),
            description: "Get detailed metadata about a package: version, description, maintainer, homepage, dependencies, size. Use this to verify a package before installing, especially for unfamiliar packages.".into(),
            category: "packages".into(),
            default_tier: RiskLevel::Green,
            min_tier: "lite",
            parameters: vec![
                param("name",   "string", "Package name", true),
                param("source", "string", "Package source/manager to query: apt, dnf, pacman, zypper, brew, winget, choco, snap, flatpak", false),
            ],
        }, Arc::new(GetPackageInfo)),
        // ── RED: action tools (require HITL approval) ──
        (ToolDef {
            name: "install_package".into(),
            description: "Install a package using the appropriate system package manager. Requires user approval. Call search_package and check_package_installed first.".into(),
            category: "packages".into(),
            default_tier: RiskLevel::Red,
            min_tier: "standard",
            parameters: vec![
                param("name",   "string", "Exact package name to install (as returned by search_package)", true),
                param("source", "string", "Package manager to use: apt, dnf, pacman, zypper, brew, winget, choco, snap, flatpak. Omit to use system default.", false),
            ],
        }, Arc::new(InstallPackage)),
        (ToolDef {
            name: "uninstall_package".into(),
            description: "Remove an installed package using the system package manager. Requires user approval. Call check_package_installed first to confirm the package is actually installed.".into(),
            category: "packages".into(),
            default_tier: RiskLevel::Red,
            min_tier: "standard",
            parameters: vec![
                param("name",   "string", "Exact package name to uninstall", true),
                param("source", "string", "Package manager to use: apt, dnf, pacman, zypper, brew, winget, choco, snap, flatpak. Omit to use system default.", false),
            ],
        }, Arc::new(UninstallPackage)),
    ];
    for (def, handler) in tools {
        reg.register(def, handler);
    }
}
