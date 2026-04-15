use std::sync::Arc;
use std::path::Path;
use async_trait::async_trait;
use crate::infra::ToolResult;
use crate::safety::RiskLevel;
use crate::sidecar::SidecarBridge;
use crate::tools::registry::{ToolRegistry, ToolDef, ToolHandler, ParamDef};

fn param(name: &str, ty: &str, desc: &str, required: bool) -> ParamDef {
    ParamDef { name: name.into(), param_type: ty.into(), description: desc.into(), required, default: None }
}

/// Wrapper for optional sidecar access.
struct VisionSidecar(Option<Arc<tokio::sync::Mutex<Arc<SidecarBridge>>>>);

impl VisionSidecar {
    async fn try_ocr(&self, path: &str) -> Option<String> {
        let guard = self.0.as_ref()?;
        let bridge = guard.lock().await;
        let result = bridge.request("image.analyze", serde_json::json!({
            "file": path,
            "operations": ["ocr"],
        })).await.ok()?;
        result["ocr_text"].as_str().map(|s| s.to_string())
    }

    async fn try_analyze(&self, path: &str, operations: &[&str]) -> Option<serde_json::Value> {
        let guard = self.0.as_ref()?;
        let bridge = guard.lock().await;
        bridge.request("image.analyze", serde_json::json!({
            "file": path,
            "operations": operations,
        })).await.ok()
    }
}

struct OcrImage { sidecar: Arc<VisionSidecar> }
#[async_trait] impl ToolHandler for OcrImage {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path = match params["path"].as_str() {
            Some(p) => p,
            None => return ToolResult::err("missing required parameter: path"),
        };
        if !Path::new(path).exists() {
            return ToolResult::err(format!("file not found: {path}"));
        }

        // Try sidecar OCR first (pytesseract/easyocr)
        if let Some(text) = self.sidecar.try_ocr(path).await {
            return ToolResult::ok(serde_json::json!({
                "text": text,
                "source": "sidecar",
                "path": path,
            }));
        }

        // Fallback: tesseract CLI
        let output = tokio::process::Command::new("tesseract")
            .args([path, "stdout"])
            .output()
            .await;
        match output {
            Ok(o) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stdout).to_string();
                ToolResult::ok(serde_json::json!({
                    "text": text.trim(),
                    "source": "tesseract_cli",
                    "path": path,
                }))
            }
            Ok(o) => ToolResult::err(format!("tesseract failed: {}", String::from_utf8_lossy(&o.stderr))),
            Err(_) => ToolResult::err("OCR unavailable: install tesseract or start the Python sidecar"),
        }
    }
}

struct AnalyzeImage { sidecar: Arc<VisionSidecar> }
#[async_trait] impl ToolHandler for AnalyzeImage {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path = match params["path"].as_str() {
            Some(p) => p,
            None => return ToolResult::err("missing required parameter: path"),
        };
        if !Path::new(path).exists() {
            return ToolResult::err(format!("file not found: {path}"));
        }

        // Get image metadata from Rust
        let info = match crate::preprocessing::image::ImageProcessor::info(Path::new(path)) {
            Ok(i) => serde_json::json!({
                "width": i.width,
                "height": i.height,
                "format": i.format,
                "size_bytes": i.size_bytes,
            }),
            Err(e) => return ToolResult::err(format!("failed to read image: {e}")),
        };

        // Try sidecar for richer analysis (features, objects, etc.)
        let operations = params["operations"].as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
            .unwrap_or_else(|| vec!["metadata", "ocr"]);

        let ops_refs: Vec<&str> = operations.iter().map(|s| s.as_ref()).collect();
        if let Some(analysis) = self.sidecar.try_analyze(path, &ops_refs).await {
            return ToolResult::ok(serde_json::json!({
                "metadata": info,
                "analysis": analysis,
                "source": "sidecar",
            }));
        }

        // Fallback: metadata only + optional tesseract
        let mut result = serde_json::json!({
            "metadata": info,
            "source": "native",
        });

        if operations.contains(&"ocr") {
            let output = tokio::process::Command::new("tesseract")
                .args([path, "stdout"])
                .output()
                .await;
            if let Ok(o) = output {
                if o.status.success() {
                    result["ocr_text"] = serde_json::json!(
                        String::from_utf8_lossy(&o.stdout).trim().to_string()
                    );
                }
            }
        }

        ToolResult::ok(result)
    }
}

struct ScreenshotAnalyze { sidecar: Arc<VisionSidecar> }
#[async_trait] impl ToolHandler for ScreenshotAnalyze {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let output_path = params["output"].as_str().unwrap_or("/tmp/kria_screenshot_analyze.png");

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
                let out = tokio::process::Command::new(tool).args(&args).output().await;
                if let Ok(o) = out {
                    if o.status.success() { captured = true; break; }
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

        // Try sidecar OCR
        if let Some(text) = self.sidecar.try_ocr(output_path).await {
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
                result["ocr_text"] = serde_json::json!(
                    String::from_utf8_lossy(&o.stdout).trim().to_string()
                );
                result["source"] = serde_json::json!("tesseract_cli");
            }
        }

        ToolResult::ok(result)
    }
}

pub fn register(reg: &mut ToolRegistry, sidecar: Option<Arc<SidecarBridge>>) {
    let vision_sidecar = Arc::new(VisionSidecar(
        sidecar.map(|s| Arc::new(tokio::sync::Mutex::new(s)))
    ));

    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        (ToolDef {
            name: "ocr_image".into(),
            description: "Extract text from an image using OCR (pytesseract via sidecar, tesseract CLI fallback)".into(),
            category: "vision".into(),
            default_tier: RiskLevel::Green,
            min_tier: "standard",
            parameters: vec![
                param("path", "string", "Path to the image file", true),
            ],
        }, Arc::new(OcrImage { sidecar: vision_sidecar.clone() })),
        (ToolDef {
            name: "analyze_image".into(),
            description: "Analyze an image: metadata, OCR, features. Operations: metadata, ocr, features, thumbnail.".into(),
            category: "vision".into(),
            default_tier: RiskLevel::Green,
            min_tier: "standard",
            parameters: vec![
                param("path", "string", "Path to the image file", true),
                param("operations", "array", "Operations to perform (default: metadata, ocr)", false),
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

    for (def, handler) in tools { reg.register(def, handler); }
}
