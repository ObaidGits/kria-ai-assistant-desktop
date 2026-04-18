use crate::infra::ToolResult;
use crate::safety::RiskLevel;
use crate::sidecar::SidecarBridge;
use crate::tools::registry::{ParamDef, ToolDef, ToolHandler, ToolRegistry};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const DEFAULT_CONTEXT_WINDOW: u64 = 4096;
const DEFAULT_RESPONSE_RESERVE: u64 = 700;
const DEFAULT_SYSTEM_RESERVE: u64 = 900;
const DEFAULT_HISTORY_RESERVE: u64 = 1400;

fn param(name: &str, ty: &str, desc: &str, required: bool) -> ParamDef {
    ParamDef {
        name: name.into(),
        param_type: ty.into(),
        description: desc.into(),
        required,
        default: None,
    }
}

fn trim_trailing_path_noise(input: &str) -> String {
    let mut s = input.trim().to_string();
    while let Some(ch) = s.chars().last() {
        if matches!(ch, ',' | ';' | '.' | '!' | '?') {
            s.pop();
            continue;
        }
        break;
    }
    s
}

fn decode_common_file_uri_escapes(input: &str) -> String {
    input
        .replace("%20", " ")
        .replace("%23", "#")
        .replace("%5B", "[")
        .replace("%5D", "]")
        .replace("%28", "(")
        .replace("%29", ")")
}

fn normalize_input_path(raw: &str) -> String {
    let mut s = raw
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim_matches('`')
        .to_string();

    s = trim_trailing_path_noise(&s);

    // Handle markdown link-like wrappers: [label](path)
    if s.ends_with(')') {
        if let (Some(open), Some(close)) = (s.rfind('('), s.rfind(')')) {
            if open < close {
                let candidate = s[open + 1..close].trim();
                if !candidate.is_empty() {
                    s = candidate.to_string();
                }
            }
        }
    }

    // Handle explicit image placeholder wrappers: [image: path]
    if let Some(inner) = s.strip_prefix("[image:").and_then(|v| v.strip_suffix(']')) {
        s = inner.trim().to_string();
    }

    if let Some(stripped) = s.strip_prefix("file://") {
        let no_localhost = stripped.strip_prefix("localhost/").unwrap_or(stripped);
        s = decode_common_file_uri_escapes(no_localhost);
    }

    // Support "path: /foo/bar.png" style values.
    if let Some(stripped) = s.strip_prefix("path:") {
        s = stripped.trim().to_string();
    }

    // Use first line when model outputs extra text after path.
    if let Some(first_line) = s.lines().next() {
        s = first_line.trim().to_string();
    }

    // Expand ~/ for local home paths.
    if s == "~" {
        if let Some(home) = dirs::home_dir() {
            s = home.to_string_lossy().to_string();
        }
    } else if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            s = home.join(rest).to_string_lossy().to_string();
        }
    }

    trim_trailing_path_noise(&s)
}

fn resolve_image_path(raw: &str) -> Option<PathBuf> {
    let normalized = normalize_input_path(raw);
    if normalized.is_empty() {
        return None;
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    let given = PathBuf::from(&normalized);
    candidates.push(given.clone());

    let kria_paths = crate::platform::paths::KriaPaths::resolve();
    let attachments_dir = kria_paths.data_dir.join("attachments");

    if !given.is_absolute() {
        candidates.push(kria_paths.data_dir.join(&normalized));
        candidates.push(attachments_dir.join(&normalized));

        if let Some(stripped) = normalized.strip_prefix("attachments/") {
            candidates.push(attachments_dir.join(stripped));
        }
    }

    for candidate in &candidates {
        if candidate.exists() {
            return Some(candidate.clone());
        }
    }

    // Final fallback: if only a filename was provided, look for exact filename in attachments.
    if let Some(file_name) = Path::new(&normalized).file_name() {
        if let Ok(entries) = std::fs::read_dir(&attachments_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() && path.file_name() == Some(file_name) {
                    return Some(path);
                }
            }
        }
    }

    None
}

fn infer_image_intent(operations: &[&str]) -> &'static str {
    let has_ocr = operations.iter().any(|op| *op == "ocr");
    let has_features = operations.iter().any(|op| *op == "features");

    if has_ocr && has_features {
        "mixed"
    } else if has_ocr {
        "text_reading"
    } else {
        "scene_understanding"
    }
}

fn read_hint_u64(params: Option<&serde_json::Value>, key: &str, default: u64) -> serde_json::Value {
    let value = params
        .and_then(|p| p.get(key))
        .and_then(|v| v.as_u64())
        .unwrap_or(default);
    serde_json::json!(value)
}

fn build_sidecar_hints(
    params: Option<&serde_json::Value>,
    default_intent: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let mut hints = serde_json::Map::new();

    let intent = params
        .and_then(|p| p.get("intent"))
        .and_then(|v| v.as_str())
        .unwrap_or(default_intent);

    hints.insert("intent".into(), serde_json::json!(intent));
    hints.insert(
        "context_window".into(),
        read_hint_u64(params, "context_window", DEFAULT_CONTEXT_WINDOW),
    );
    hints.insert(
        "response_reserve".into(),
        read_hint_u64(params, "response_reserve", DEFAULT_RESPONSE_RESERVE),
    );
    hints.insert(
        "system_reserve".into(),
        read_hint_u64(params, "system_reserve", DEFAULT_SYSTEM_RESERVE),
    );
    hints.insert(
        "history_reserve".into(),
        read_hint_u64(params, "history_reserve", DEFAULT_HISTORY_RESERVE),
    );

    for key in [
        "ocr_token_cap",
        "metadata_token_cap",
        "hard_visual_token_cap",
    ] {
        if let Some(value) = params.and_then(|p| p.get(key)).and_then(|v| v.as_u64()) {
            hints.insert(key.to_string(), serde_json::json!(value));
        }
    }

    if let Some(model_name) = params
        .and_then(|p| p.get("model_name"))
        .and_then(|v| v.as_str())
    {
        hints.insert("model_name".into(), serde_json::json!(model_name));
    }

    if let Some(model_profile) = params.and_then(|p| p.get("model_profile")) {
        if model_profile.is_object() {
            hints.insert("model_profile".into(), model_profile.clone());
        }
    }

    hints
}

/// Wrapper for optional sidecar access.
struct VisionSidecar(Option<Arc<tokio::sync::Mutex<Arc<SidecarBridge>>>>);

impl VisionSidecar {
    async fn try_ocr(
        &self,
        path: &str,
        hints: &serde_json::Map<String, serde_json::Value>,
    ) -> Option<String> {
        let guard = self.0.as_ref()?;
        let bridge = guard.lock().await;
        let mut payload = serde_json::Map::new();
        payload.insert("file".into(), serde_json::json!(path));
        payload.insert("operations".into(), serde_json::json!(["ocr"]));
        for (key, value) in hints {
            payload.insert(key.clone(), value.clone());
        }

        let result = bridge
            .request("image.analyze", serde_json::Value::Object(payload))
            .await
            .ok()?;
        result["ocr_text"].as_str().map(|s| s.to_string())
    }

    async fn try_analyze(
        &self,
        path: &str,
        operations: &[&str],
        hints: &serde_json::Map<String, serde_json::Value>,
    ) -> Option<serde_json::Value> {
        let guard = self.0.as_ref()?;
        let bridge = guard.lock().await;
        let mut payload = serde_json::Map::new();
        payload.insert("file".into(), serde_json::json!(path));
        payload.insert("operations".into(), serde_json::json!(operations));
        for (key, value) in hints {
            payload.insert(key.clone(), value.clone());
        }

        bridge
            .request("image.analyze", serde_json::Value::Object(payload))
            .await
            .ok()
    }
}

struct OcrImage {
    sidecar: Arc<VisionSidecar>,
}
#[async_trait]
impl ToolHandler for OcrImage {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path_input = match params["path"].as_str() {
            Some(p) => p,
            None => return ToolResult::err("missing required parameter: path"),
        };

        let resolved = match resolve_image_path(path_input) {
            Some(p) => p,
            None => {
                return ToolResult::err(format!(
                    "file not found: {path_input}. Provide an absolute path or an attachment filename from ~/.kria/attachments"
                ));
            }
        };
        let resolved_str = resolved.to_string_lossy().to_string();
        let hints = build_sidecar_hints(Some(&params), "text_reading");

        // Try sidecar OCR first (pytesseract/easyocr)
        if let Some(text) = self.sidecar.try_ocr(&resolved_str, &hints).await {
            return ToolResult::ok(serde_json::json!({
                "text": text,
                "source": "sidecar",
                "path": resolved_str,
            }));
        }

        // Fallback: tesseract CLI
        let output = tokio::process::Command::new("tesseract")
            .args([resolved_str.as_str(), "stdout"])
            .output()
            .await;
        match output {
            Ok(o) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stdout).to_string();
                ToolResult::ok(serde_json::json!({
                    "text": text.trim(),
                    "source": "tesseract_cli",
                    "path": resolved_str,
                }))
            }
            Ok(o) => ToolResult::err(format!(
                "tesseract failed: {}",
                String::from_utf8_lossy(&o.stderr)
            )),
            Err(_) => {
                ToolResult::err("OCR unavailable: install tesseract or start the Python sidecar")
            }
        }
    }
}

struct AnalyzeImage {
    sidecar: Arc<VisionSidecar>,
}
#[async_trait]
impl ToolHandler for AnalyzeImage {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path_input = match params["path"].as_str() {
            Some(p) => p,
            None => return ToolResult::err("missing required parameter: path"),
        };

        let resolved = match resolve_image_path(path_input) {
            Some(p) => p,
            None => {
                return ToolResult::err(format!(
                    "file not found: {path_input}. Provide an absolute path or an attachment filename from ~/.kria/attachments"
                ));
            }
        };
        let resolved_str = resolved.to_string_lossy().to_string();

        // Get image metadata from Rust
        let info = match crate::preprocessing::image::ImageProcessor::info(&resolved) {
            Ok(i) => serde_json::json!({
                "width": i.width,
                "height": i.height,
                "format": i.format,
                "size_bytes": i.size_bytes,
            }),
            Err(e) => return ToolResult::err(format!("failed to read image: {e}")),
        };

        // Try sidecar for richer analysis (features, objects, etc.)
        let operations = params["operations"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
            .unwrap_or_else(|| vec!["metadata", "ocr"]);

        let ops_refs: Vec<&str> = operations.iter().map(|s| s.as_ref()).collect();
        let hints = build_sidecar_hints(Some(&params), infer_image_intent(&ops_refs));
        if let Some(analysis) = self
            .sidecar
            .try_analyze(&resolved_str, &ops_refs, &hints)
            .await
        {
            return ToolResult::ok(serde_json::json!({
                "path": resolved_str,
                "metadata": info,
                "analysis": analysis,
                "source": "sidecar",
            }));
        }

        // Fallback: metadata only + optional tesseract
        let mut result = serde_json::json!({
            "path": resolved_str,
            "metadata": info,
            "source": "native",
        });

        if operations.contains(&"ocr") {
            let output = tokio::process::Command::new("tesseract")
                .args([resolved_str.as_str(), "stdout"])
                .output()
                .await;
            if let Ok(o) = output {
                if o.status.success() {
                    result["ocr_text"] =
                        serde_json::json!(String::from_utf8_lossy(&o.stdout).trim().to_string());
                }
            }
        }

        ToolResult::ok(result)
    }
}

struct ScreenshotAnalyze {
    sidecar: Arc<VisionSidecar>,
}
#[async_trait]
impl ToolHandler for ScreenshotAnalyze {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let output_path = params["output"]
            .as_str()
            .unwrap_or("/tmp/kria_screenshot_analyze.png");

        // Take screenshot first
        if cfg!(target_os = "linux") {
            let tools = ["gnome-screenshot", "scrot", "import"];
            let mut captured = false;
            for tool in &tools {
                let args = match *tool {
                    "gnome-screenshot" => vec!["-f", output_path],
                    "scrot" => vec![output_path],
                    "import" => vec!["-window", "root", output_path],
                    _ => continue,
                };
                let out = tokio::process::Command::new(tool)
                    .args(&args)
                    .output()
                    .await;
                if let Ok(o) = out {
                    if o.status.success() {
                        captured = true;
                        break;
                    }
                }
            }
            if !captured {
                return ToolResult::err("no screenshot tool available");
            }
        } else {
            return ToolResult::err("screenshot not supported on this OS");
        }

        // Analyze the screenshot with OCR
        let mut result = serde_json::json!({
            "screenshot_path": output_path,
        });
        let hints = build_sidecar_hints(Some(&params), "ui_error_reading");

        // Try sidecar OCR
        if let Some(text) = self.sidecar.try_ocr(output_path, &hints).await {
            result["ocr_text"] = serde_json::json!(text);
            result["source"] = serde_json::json!("sidecar");
            return ToolResult::ok(result);
        }

        // Fallback to tesseract
        let output = tokio::process::Command::new("tesseract")
            .args([output_path, "stdout"])
            .output()
            .await;
        if let Ok(o) = output {
            if o.status.success() {
                result["ocr_text"] =
                    serde_json::json!(String::from_utf8_lossy(&o.stdout).trim().to_string());
                result["source"] = serde_json::json!("tesseract_cli");
            }
        }

        ToolResult::ok(result)
    }
}

pub fn register(reg: &ToolRegistry, sidecar: Option<Arc<SidecarBridge>>) {
    let vision_sidecar = Arc::new(VisionSidecar(
        sidecar.map(|s| Arc::new(tokio::sync::Mutex::new(s))),
    ));

    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        (ToolDef {
            name: "ocr_image".into(),
            description: "Extract text from an image using OCR (pytesseract via sidecar, tesseract CLI fallback)".into(),
            category: "vision".into(),
            default_tier: RiskLevel::Green,
            min_tier: "standard",
            parameters: vec![
                param("path", "string", "Path to the image file (absolute path, file:// URI, or attachment filename)", true),
                param("intent", "string", "Intent hint (e.g., text_reading, ui_error_reading, document_scan)", false),
                param("context_window", "integer", "Context window used for sidecar token budgeting (default 4096)", false),
                param("response_reserve", "integer", "Reserved output tokens (default 700)", false),
                param("system_reserve", "integer", "Reserved system tokens (default 900)", false),
                param("history_reserve", "integer", "Reserved history tokens (default 1400)", false),
                param("model_name", "string", "Optional vision model hint (for sidecar profile selection)", false),
                param("model_profile", "object", "Optional model profile override: patch_size, patch_merge, effective_patch and caps", false),
            ],
        }, Arc::new(OcrImage { sidecar: vision_sidecar.clone() })),
        (ToolDef {
            name: "analyze_image".into(),
            description: "Analyze an image: metadata, OCR, features. Operations: metadata, ocr, features, thumbnail.".into(),
            category: "vision".into(),
            default_tier: RiskLevel::Green,
            min_tier: "standard",
            parameters: vec![
                param("path", "string", "Path to the image file (absolute path, file:// URI, or attachment filename)", true),
                param("operations", "array", "Operations to perform (default: metadata, ocr)", false),
                param("intent", "string", "Intent hint for sidecar mode selection (text_reading, mixed, scene_understanding)", false),
                param("context_window", "integer", "Context window used for sidecar token budgeting (default 4096)", false),
                param("response_reserve", "integer", "Reserved output tokens (default 700)", false),
                param("system_reserve", "integer", "Reserved system tokens (default 900)", false),
                param("history_reserve", "integer", "Reserved history tokens (default 1400)", false),
                param("model_name", "string", "Optional vision model hint (for sidecar profile selection)", false),
                param("model_profile", "object", "Optional model profile override: patch_size, patch_merge, effective_patch and caps", false),
            ],
        }, Arc::new(AnalyzeImage { sidecar: vision_sidecar.clone() })),
        (ToolDef {
            name: "screenshot_analyze".into(),
            description: "Take a screenshot and analyze it with OCR. Returns screenshot path and extracted text.".into(),
            category: "vision".into(),
            default_tier: RiskLevel::Green,
            min_tier: "standard",
            parameters: vec![
                param("output", "string", "Output path for screenshot (default: /tmp/kria_screenshot_analyze.png)", false),
            ],
        }, Arc::new(ScreenshotAnalyze { sidecar: vision_sidecar })),
    ];

    for (def, handler) in tools {
        reg.register(def, handler);
    }
}
