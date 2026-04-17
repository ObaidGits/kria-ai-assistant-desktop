use std::sync::Arc;
use async_trait::async_trait;
use crate::infra::ToolResult;
use crate::safety::RiskLevel;
use crate::tools::registry::{ToolRegistry, ToolDef, ToolHandler, ParamDef};

fn param(name: &str, ty: &str, desc: &str, required: bool) -> ParamDef {
    ParamDef { name: name.into(), param_type: ty.into(), description: desc.into(), required, default: None }
}

struct GetClipboard;
#[async_trait] impl ToolHandler for GetClipboard {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        match arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
            Ok(text) => ToolResult::ok(serde_json::json!({ "content": text })),
            Err(e) => ToolResult::err(format!("clipboard read failed: {e}"))
        }
    }
}

struct SetClipboard;
#[async_trait] impl ToolHandler for SetClipboard {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let text = params["text"].as_str().unwrap_or("");
        match arboard::Clipboard::new().and_then(|mut c| c.set_text(text.to_string())) {
            Ok(_) => ToolResult::ok(serde_json::json!({ "set": true, "length": text.len() })),
            Err(e) => ToolResult::err(format!("clipboard write failed: {e}"))
        }
    }
}

struct TransformClipboard;
#[async_trait] impl ToolHandler for TransformClipboard {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let transform = params["transform"].as_str().unwrap_or("uppercase");
        let mut clipboard = match arboard::Clipboard::new() {
            Ok(c) => c,
            Err(e) => return ToolResult::err(format!("clipboard error: {e}")),
        };
        let text = match clipboard.get_text() {
            Ok(t) => t,
            Err(e) => return ToolResult::err(format!("read failed: {e}")),
        };

        let result = match transform {
            "uppercase" => text.to_uppercase(),
            "lowercase" => text.to_lowercase(),
            "trim" => text.trim().to_string(),
            "reverse" => text.chars().rev().collect(),
            "snake_case" => text.replace(' ', "_").to_lowercase(),
            "title_case" => text.split_whitespace()
                .map(|w| {
                    let mut c = w.chars();
                    match c.next() {
                        None => String::new(),
                        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                    }
                })
                .collect::<Vec<_>>().join(" "),
            _ => return ToolResult::err(format!("unknown transform: {transform}")),
        };

        let _ = clipboard.set_text(result.clone());
        ToolResult::ok(serde_json::json!({
            "transform": transform,
            "original_length": text.len(),
            "result_length": result.len(),
        }))
    }
}

struct Screenshot;
#[async_trait] impl ToolHandler for Screenshot {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let output_path = params["output"].as_str().unwrap_or("/tmp/kria_screenshot.png");
        if cfg!(target_os = "linux") {
            // Try gnome-screenshot, then scrot, then import (ImageMagick)
            let tools = ["gnome-screenshot", "scrot", "import"];
            for tool in &tools {
                let args = match *tool {
                    "gnome-screenshot" => vec!["-f", output_path],
                    "scrot" => vec![output_path],
                    "import" => vec!["-window", "root", output_path],
                    _ => continue,
                };
                let output = tokio::process::Command::new(tool).args(&args).output().await;
                if let Ok(o) = output {
                    if o.status.success() {
                        return ToolResult::ok(serde_json::json!({
                            "path": output_path, "tool": tool,
                        }));
                    }
                }
            }
            ToolResult::err("no screenshot tool available (install gnome-screenshot, scrot, or imagemagick)")
        } else {
            ToolResult::err("screenshot not implemented for this OS yet")
        }
    }
}

struct TypeText;
#[async_trait] impl ToolHandler for TypeText {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let text = params["text"].as_str().unwrap_or("");
        if cfg!(target_os = "linux") {
            let output = tokio::process::Command::new("xdotool")
                .args(["type", "--clearmodifiers", text])
                .output().await;
            match output {
                Ok(o) if o.status.success() => ToolResult::ok(serde_json::json!({
                    "typed": true, "length": text.len(),
                })),
                _ => ToolResult::err("type_text failed (xdotool required)")
            }
        } else {
            ToolResult::err("type_text not implemented for this OS")
        }
    }
}

pub fn register(reg: &ToolRegistry) {
    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        // GREEN
        (ToolDef {
            name: "get_clipboard".into(), description: "Get clipboard text content".into(),
            category: "interaction".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![],
        }, Arc::new(GetClipboard)),
        (ToolDef {
            name: "transform_clipboard".into(), description: "Transform clipboard text (uppercase, lowercase, etc.)".into(),
            category: "interaction".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![param("transform", "string", "uppercase|lowercase|trim|reverse|snake_case|title_case", true)],
        }, Arc::new(TransformClipboard)),
        (ToolDef {
            name: "screenshot".into(), description: "Take a screenshot of the screen".into(),
            category: "interaction".into(), default_tier: RiskLevel::Green, min_tier: "standard",
            parameters: vec![param("output", "string", "Output file path (default /tmp/kria_screenshot.png)", false)],
        }, Arc::new(Screenshot)),
        // YELLOW
        (ToolDef {
            name: "set_clipboard".into(), description: "Set clipboard text content".into(),
            category: "interaction".into(), default_tier: RiskLevel::Yellow, min_tier: "lite",
            parameters: vec![param("text", "string", "Text to set", true)],
        }, Arc::new(SetClipboard)),
        (ToolDef {
            name: "type_text".into(), description: "Type text as keyboard input".into(),
            category: "interaction".into(), default_tier: RiskLevel::Yellow, min_tier: "standard",
            parameters: vec![param("text", "string", "Text to type", true)],
        }, Arc::new(TypeText)),
    ];
    for (def, handler) in tools { reg.register(def, handler); }
}
