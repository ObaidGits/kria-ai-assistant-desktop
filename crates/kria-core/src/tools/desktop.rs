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

// ── Active Window Detection ──

struct GetActiveWindow;
#[async_trait]
impl ToolHandler for GetActiveWindow {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        if cfg!(target_os = "linux") {
            // Try xdotool first
            let output = tokio::process::Command::new("xdotool")
                .args(["getactivewindow", "getwindowname"])
                .output()
                .await;
            match output {
                Ok(o) if o.status.success() => {
                    let title = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    // Also get the PID
                    let pid_out = tokio::process::Command::new("xdotool")
                        .args(["getactivewindow", "getwindowpid"])
                        .output()
                        .await;
                    let pid = pid_out
                        .ok()
                        .filter(|o| o.status.success())
                        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                        .unwrap_or_default();
                    // Get WM_CLASS for app identification
                    let wid_out = tokio::process::Command::new("xdotool")
                        .args(["getactivewindow"])
                        .output()
                        .await;
                    let wm_class = if let Ok(wid) = wid_out {
                        let wid_str = String::from_utf8_lossy(&wid.stdout).trim().to_string();
                        let xprop = tokio::process::Command::new("xprop")
                            .args(["-id", &wid_str, "WM_CLASS"])
                            .output()
                            .await;
                        xprop
                            .ok()
                            .filter(|o| o.status.success())
                            .map(|o| {
                                let s = String::from_utf8_lossy(&o.stdout);
                                s.split('=')
                                    .nth(1)
                                    .map(|v| v.trim().replace('"', ""))
                                    .unwrap_or_default()
                            })
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    ToolResult::ok(serde_json::json!({
                        "title": title,
                        "pid": pid,
                        "wm_class": wm_class,
                    }))
                }
                _ => ToolResult::err(
                    "xdotool not available — install it with: sudo apt install xdotool",
                ),
            }
        } else {
            ToolResult::err("get_active_window not implemented for this OS")
        }
    }
}

// ── Window Management ──

struct MoveWindow;
#[async_trait]
impl ToolHandler for MoveWindow {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let title = params["title"].as_str().unwrap_or("");
        let x = params["x"].as_i64().unwrap_or(0);
        let y = params["y"].as_i64().unwrap_or(0);
        if cfg!(target_os = "linux") {
            let result = tokio::process::Command::new("wmctrl")
                .args(["-r", title, "-e", &format!("0,{x},{y},-1,-1")])
                .output()
                .await;
            match result {
                Ok(o) if o.status.success() => {
                    ToolResult::ok(serde_json::json!({ "moved": title, "x": x, "y": y }))
                }
                _ => ToolResult::err(format!("failed to move window '{title}' (wmctrl required)")),
            }
        } else {
            ToolResult::err("move_window not implemented for this OS")
        }
    }
}

struct ResizeWindow;
#[async_trait]
impl ToolHandler for ResizeWindow {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let title = params["title"].as_str().unwrap_or("");
        let w = params["width"].as_i64().unwrap_or(800);
        let h = params["height"].as_i64().unwrap_or(600);
        if cfg!(target_os = "linux") {
            let result = tokio::process::Command::new("wmctrl")
                .args(["-r", title, "-e", &format!("0,-1,-1,{w},{h}")])
                .output()
                .await;
            match result {
                Ok(o) if o.status.success() => {
                    ToolResult::ok(serde_json::json!({ "resized": title, "width": w, "height": h }))
                }
                _ => ToolResult::err(format!(
                    "failed to resize window '{title}' (wmctrl required)"
                )),
            }
        } else {
            ToolResult::err("resize_window not implemented for this OS")
        }
    }
}

struct MaximizeWindow;
#[async_trait]
impl ToolHandler for MaximizeWindow {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let title = params["title"].as_str().unwrap_or("");
        if cfg!(target_os = "linux") {
            let result = tokio::process::Command::new("wmctrl")
                .args(["-r", title, "-b", "add,maximized_vert,maximized_horz"])
                .output()
                .await;
            match result {
                Ok(o) if o.status.success() => {
                    ToolResult::ok(serde_json::json!({ "maximized": title }))
                }
                _ => ToolResult::err(format!("failed to maximize '{title}'")),
            }
        } else {
            ToolResult::err("maximize_window not implemented for this OS")
        }
    }
}

struct MinimizeWindow;
#[async_trait]
impl ToolHandler for MinimizeWindow {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let title = params["title"].as_str().unwrap_or("");
        if cfg!(target_os = "linux") {
            let result = tokio::process::Command::new("xdotool")
                .args(["search", "--name", title, "windowminimize"])
                .output()
                .await;
            match result {
                Ok(o) if o.status.success() => {
                    ToolResult::ok(serde_json::json!({ "minimized": title }))
                }
                _ => ToolResult::err(format!("failed to minimize '{title}'")),
            }
        } else {
            ToolResult::err("minimize_window not implemented for this OS")
        }
    }
}

struct TileWindows;
#[async_trait]
impl ToolHandler for TileWindows {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let layout = params["layout"].as_str().unwrap_or("side-by-side");
        let windows: Vec<String> = params["windows"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        if cfg!(target_os = "linux") {
            // Get screen dimensions
            let xdp = tokio::process::Command::new("xdpyinfo").output().await;
            let (sw, sh) = xdp
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| {
                    let text = String::from_utf8_lossy(&o.stdout).to_string();
                    text.lines()
                        .find(|l| l.contains("dimensions:"))
                        .and_then(|l| {
                            let parts: Vec<&str> = l.split_whitespace().collect();
                            parts.get(1).and_then(|dim| {
                                let xy: Vec<&str> = dim.split('x').collect();
                                Some((
                                    xy.first()?.parse::<i64>().ok()?,
                                    xy.get(1)?.parse::<i64>().ok()?,
                                ))
                            })
                        })
                })
                .unwrap_or((1920, 1080));

            if windows.len() < 2 {
                return ToolResult::err("at least 2 window titles required for tiling");
            }

            match layout {
                "side-by-side" => {
                    let half_w = sw / 2;
                    // Left half
                    let _ = tokio::process::Command::new("wmctrl")
                        .args([
                            "-r",
                            &windows[0],
                            "-b",
                            "remove,maximized_vert,maximized_horz",
                        ])
                        .output()
                        .await;
                    let _ = tokio::process::Command::new("wmctrl")
                        .args(["-r", &windows[0], "-e", &format!("0,0,0,{half_w},{sh}")])
                        .output()
                        .await;
                    // Right half
                    let _ = tokio::process::Command::new("wmctrl")
                        .args([
                            "-r",
                            &windows[1],
                            "-b",
                            "remove,maximized_vert,maximized_horz",
                        ])
                        .output()
                        .await;
                    let _ = tokio::process::Command::new("wmctrl")
                        .args([
                            "-r",
                            &windows[1],
                            "-e",
                            &format!("0,{half_w},0,{half_w},{sh}"),
                        ])
                        .output()
                        .await;
                    ToolResult::ok(
                        serde_json::json!({ "layout": "side-by-side", "windows": windows }),
                    )
                }
                "grid" => {
                    let half_w = sw / 2;
                    let half_h = sh / 2;
                    let positions = [(0, 0), (half_w, 0), (0, half_h), (half_w, half_h)];
                    for (i, win) in windows.iter().enumerate().take(4) {
                        let (px, py) = positions.get(i).copied().unwrap_or((0, 0));
                        let _ = tokio::process::Command::new("wmctrl")
                            .args(["-r", win, "-b", "remove,maximized_vert,maximized_horz"])
                            .output()
                            .await;
                        let _ = tokio::process::Command::new("wmctrl")
                            .args(["-r", win, "-e", &format!("0,{px},{py},{half_w},{half_h}")])
                            .output()
                            .await;
                    }
                    ToolResult::ok(serde_json::json!({ "layout": "grid", "windows": windows }))
                }
                _ => ToolResult::err(format!(
                    "unknown layout '{layout}'. Supported: side-by-side, grid"
                )),
            }
        } else {
            ToolResult::err("tile_windows not implemented for this OS")
        }
    }
}

// ── Browser / URL ──

struct OpenUrl;
#[async_trait]
impl ToolHandler for OpenUrl {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let url = params["url"].as_str().unwrap_or("");
        if url.is_empty() {
            return ToolResult::err("url parameter is required");
        }
        // Validate URL
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return ToolResult::err("url must start with http:// or https://");
        }
        let result = if cfg!(target_os = "linux") {
            tokio::process::Command::new("xdg-open").arg(url).spawn()
        } else if cfg!(target_os = "macos") {
            tokio::process::Command::new("open").arg(url).spawn()
        } else {
            tokio::process::Command::new("cmd")
                .args(["/C", "start", "", url])
                .spawn()
        };
        match result {
            Ok(_) => ToolResult::ok(serde_json::json!({ "opened": url })),
            Err(e) => ToolResult::err(format!("failed to open URL: {e}")),
        }
    }
}

struct ListWindows;
#[async_trait]
impl ToolHandler for ListWindows {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        if cfg!(target_os = "linux") {
            let output = tokio::process::Command::new("wmctrl")
                .args(["-l"])
                .output()
                .await;
            match output {
                Ok(o) if o.status.success() => {
                    let text = String::from_utf8_lossy(&o.stdout);
                    let windows: Vec<serde_json::Value> = text
                        .lines()
                        .filter(|l| !l.is_empty())
                        .map(|line| {
                            let parts: Vec<&str> = line.splitn(4, char::is_whitespace).collect();
                            serde_json::json!({
                                "id": parts.first().unwrap_or(&""),
                                "desktop": parts.get(1).unwrap_or(&""),
                                "title": parts.get(3).unwrap_or(&"").trim(),
                            })
                        })
                        .collect();
                    ToolResult::ok(
                        serde_json::json!({ "windows": windows, "count": windows.len() }),
                    )
                }
                _ => ToolResult::err(
                    "wmctrl not available — install it with: sudo apt install wmctrl",
                ),
            }
        } else {
            ToolResult::err("list_windows not implemented for this OS")
        }
    }
}

pub fn register(reg: &ToolRegistry) {
    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        (
            ToolDef {
                name: "get_active_window".into(),
                description: "Get the currently focused window title, PID, and application class"
                    .into(),
                category: "desktop".into(),
                default_tier: RiskLevel::Green,
                min_tier: "standard",
                parameters: vec![],
            },
            Arc::new(GetActiveWindow),
        ),
        (
            ToolDef {
                name: "list_windows".into(),
                description: "List all open windows with their titles and desktop numbers".into(),
                category: "desktop".into(),
                default_tier: RiskLevel::Green,
                min_tier: "standard",
                parameters: vec![],
            },
            Arc::new(ListWindows),
        ),
        (
            ToolDef {
                name: "move_window".into(),
                description: "Move a window to a specific position on screen".into(),
                category: "desktop".into(),
                default_tier: RiskLevel::Yellow,
                min_tier: "standard",
                parameters: vec![
                    param("title", "string", "Window title (partial match)", true),
                    param("x", "integer", "X position in pixels", true),
                    param("y", "integer", "Y position in pixels", true),
                ],
            },
            Arc::new(MoveWindow),
        ),
        (
            ToolDef {
                name: "resize_window".into(),
                description: "Resize a window to specific dimensions".into(),
                category: "desktop".into(),
                default_tier: RiskLevel::Yellow,
                min_tier: "standard",
                parameters: vec![
                    param("title", "string", "Window title (partial match)", true),
                    param("width", "integer", "Width in pixels", true),
                    param("height", "integer", "Height in pixels", true),
                ],
            },
            Arc::new(ResizeWindow),
        ),
        (
            ToolDef {
                name: "maximize_window".into(),
                description: "Maximize a window by title".into(),
                category: "desktop".into(),
                default_tier: RiskLevel::Yellow,
                min_tier: "standard",
                parameters: vec![param(
                    "title",
                    "string",
                    "Window title (partial match)",
                    true,
                )],
            },
            Arc::new(MaximizeWindow),
        ),
        (
            ToolDef {
                name: "minimize_window".into(),
                description: "Minimize a window by title".into(),
                category: "desktop".into(),
                default_tier: RiskLevel::Yellow,
                min_tier: "standard",
                parameters: vec![param(
                    "title",
                    "string",
                    "Window title (partial match)",
                    true,
                )],
            },
            Arc::new(MinimizeWindow),
        ),
        (
            ToolDef {
                name: "tile_windows".into(),
                description: "Arrange windows in a tiled layout (side-by-side or grid)".into(),
                category: "desktop".into(),
                default_tier: RiskLevel::Yellow,
                min_tier: "standard",
                parameters: vec![
                    param("windows", "array", "Window titles to tile", true),
                    param(
                        "layout",
                        "string",
                        "Layout: 'side-by-side' or 'grid'",
                        false,
                    ),
                ],
            },
            Arc::new(TileWindows),
        ),
        (
            ToolDef {
                name: "open_url".into(),
                description: "Open a URL in the default web browser".into(),
                category: "desktop".into(),
                default_tier: RiskLevel::Green,
                min_tier: "lite",
                parameters: vec![param(
                    "url",
                    "string",
                    "URL to open (must start with http:// or https://)",
                    true,
                )],
            },
            Arc::new(OpenUrl),
        ),
    ];
    for (def, handler) in tools {
        reg.register(def, handler);
    }
}
