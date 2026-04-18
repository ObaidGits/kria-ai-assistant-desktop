//! Pre-Cognitive tools — delegate heavy pre-processing to the Python sidecar.
//!
//! These tools extract structured data from files, images, web pages, and code
//! *before* the LLM sees them, reducing token cost and improving accuracy.

use crate::infra::ToolResult;
use crate::safety::RiskLevel;
use crate::sidecar::SidecarBridge;
use crate::tools::registry::{ParamDef, ToolDef, ToolHandler, ToolRegistry};
use async_trait::async_trait;
use std::sync::Arc;

const DEFAULT_CONTEXT_WINDOW: u64 = 4096;
const DEFAULT_RESPONSE_RESERVE: u64 = 700;
const DEFAULT_SYSTEM_RESERVE: u64 = 900;
const DEFAULT_HISTORY_RESERVE: u64 = 1400;

/// Shared handle to the sidecar bridge, injected into each handler.
#[derive(Clone)]
struct SidecarHandle(Arc<SidecarBridge>);

fn infer_image_intent_from_ops(map: &serde_json::Map<String, serde_json::Value>) -> &'static str {
    let Some(ops) = map.get("operations").and_then(|v| v.as_array()) else {
        return "scene_understanding";
    };

    let has_ocr = ops.iter().any(|v| v.as_str() == Some("ocr"));
    let has_features = ops.iter().any(|v| v.as_str() == Some("features"));

    if has_ocr && has_features {
        "mixed"
    } else if has_ocr {
        "text_reading"
    } else {
        "scene_understanding"
    }
}

fn enrich_image_analyze_params(params: serde_json::Value) -> serde_json::Value {
    let mut map = match params {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };

    if !map.contains_key("intent") {
        let inferred = infer_image_intent_from_ops(&map);
        map.insert("intent".into(), serde_json::json!(inferred));
    }
    if !map.contains_key("context_window") {
        map.insert(
            "context_window".into(),
            serde_json::json!(DEFAULT_CONTEXT_WINDOW),
        );
    }
    if !map.contains_key("response_reserve") {
        map.insert(
            "response_reserve".into(),
            serde_json::json!(DEFAULT_RESPONSE_RESERVE),
        );
    }
    if !map.contains_key("system_reserve") {
        map.insert(
            "system_reserve".into(),
            serde_json::json!(DEFAULT_SYSTEM_RESERVE),
        );
    }
    if !map.contains_key("history_reserve") {
        map.insert(
            "history_reserve".into(),
            serde_json::json!(DEFAULT_HISTORY_RESERVE),
        );
    }

    serde_json::Value::Object(map)
}

// ── Image Analyze ───────────────────────────────────────────

struct ImageAnalyzeHandler(SidecarHandle);

#[async_trait]
impl ToolHandler for ImageAnalyzeHandler {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let params = enrich_image_analyze_params(params);
        match self.0 .0.request("image.analyze", params).await {
            Ok(v) => ToolResult {
                success: true,
                data: v,
                error: None,
            },
            Err(e) => ToolResult {
                success: false,
                data: serde_json::Value::Null,
                error: Some(e.to_string()),
            },
        }
    }
}

// ── Document Extract ────────────────────────────────────────

struct DocumentExtractHandler(SidecarHandle);

#[async_trait]
impl ToolHandler for DocumentExtractHandler {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        match self.0 .0.request("document.extract", params).await {
            Ok(v) => ToolResult {
                success: true,
                data: v,
                error: None,
            },
            Err(e) => ToolResult {
                success: false,
                data: serde_json::Value::Null,
                error: Some(e.to_string()),
            },
        }
    }
}

// ── Code Analyze AST ────────────────────────────────────────

struct CodeAnalyzeHandler(SidecarHandle);

#[async_trait]
impl ToolHandler for CodeAnalyzeHandler {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        match self.0 .0.request("code.analyze", params).await {
            Ok(v) => ToolResult {
                success: true,
                data: v,
                error: None,
            },
            Err(e) => ToolResult {
                success: false,
                data: serde_json::Value::Null,
                error: Some(e.to_string()),
            },
        }
    }
}

// ── Web Extract Article ─────────────────────────────────────

struct WebExtractHandler(SidecarHandle);

#[async_trait]
impl ToolHandler for WebExtractHandler {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        match self.0 .0.request("web.extract", params).await {
            Ok(v) => ToolResult {
                success: true,
                data: v,
                error: None,
            },
            Err(e) => ToolResult {
                success: false,
                data: serde_json::Value::Null,
                error: Some(e.to_string()),
            },
        }
    }
}

// ── Embeddings Generate ─────────────────────────────────────

struct EmbeddingsHandler(SidecarHandle);

#[async_trait]
impl ToolHandler for EmbeddingsHandler {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        match self.0 .0.request("embeddings.embed_text", params).await {
            Ok(v) => ToolResult {
                success: true,
                data: v,
                error: None,
            },
            Err(e) => ToolResult {
                success: false,
                data: serde_json::Value::Null,
                error: Some(e.to_string()),
            },
        }
    }
}

// ── Audio Preprocess ────────────────────────────────────────

struct AudioPreprocessHandler(SidecarHandle);

#[async_trait]
impl ToolHandler for AudioPreprocessHandler {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        match self.0 .0.request("audio.preprocess", params).await {
            Ok(v) => ToolResult {
                success: true,
                data: v,
                error: None,
            },
            Err(e) => ToolResult {
                success: false,
                data: serde_json::Value::Null,
                error: Some(e.to_string()),
            },
        }
    }
}

// ── Registration ────────────────────────────────────────────

/// Register all pre-cognitive tools that delegate to the Python sidecar.
pub fn register(registry: &ToolRegistry, sidecar: Arc<SidecarBridge>) {
    let handle = SidecarHandle(sidecar);

    // image.analyze
    registry.register(
        ToolDef {
            name: "image_analyze".into(),
            description: "Analyze an image: extract metadata, OCR text, visual features, thumbnail.".into(),
            category: "precognitive".into(),
            parameters: vec![
                ParamDef { name: "file_path".into(), param_type: "string".into(), description: "Path to image file".into(), required: true, default: None },
                ParamDef { name: "operations".into(), param_type: "array".into(), description: "Operations: metadata, ocr, features, thumbnail".into(), required: false, default: None },
                ParamDef { name: "intent".into(), param_type: "string".into(), description: "Intent hint: text_reading, mixed, scene_understanding, ui_error_reading, document_scan".into(), required: false, default: None },
                ParamDef { name: "context_window".into(), param_type: "integer".into(), description: "Context window for token budgeting (default 4096)".into(), required: false, default: None },
                ParamDef { name: "response_reserve".into(), param_type: "integer".into(), description: "Reserved completion tokens (default 700)".into(), required: false, default: None },
                ParamDef { name: "system_reserve".into(), param_type: "integer".into(), description: "Reserved system prompt tokens (default 900)".into(), required: false, default: None },
                ParamDef { name: "history_reserve".into(), param_type: "integer".into(), description: "Reserved chat history tokens (default 1400)".into(), required: false, default: None },
                ParamDef { name: "model_name".into(), param_type: "string".into(), description: "Optional vision model name hint".into(), required: false, default: None },
                ParamDef { name: "model_profile".into(), param_type: "object".into(), description: "Optional model profile override (patch_size, patch_merge, effective_patch, token caps)".into(), required: false, default: None },
            ],
            default_tier: RiskLevel::Green,
            min_tier: "standard",
        },
        Arc::new(ImageAnalyzeHandler(handle.clone())),
    );

    // document.extract
    registry.register(
        ToolDef {
            name: "document_extract".into(),
            description: "Extract text and structure from PDF, DOCX, CSV, or text files.".into(),
            category: "precognitive".into(),
            parameters: vec![
                ParamDef {
                    name: "file_path".into(),
                    param_type: "string".into(),
                    description: "Path to document file".into(),
                    required: true,
                    default: None,
                },
                ParamDef {
                    name: "max_chars".into(),
                    param_type: "integer".into(),
                    description: "Maximum characters to extract".into(),
                    required: false,
                    default: None,
                },
            ],
            default_tier: RiskLevel::Green,
            min_tier: "lite",
        },
        Arc::new(DocumentExtractHandler(handle.clone())),
    );

    // code.analyze
    registry.register(
        ToolDef {
            name: "code_analyze_ast".into(),
            description:
                "Analyze source code: extract functions, classes, imports via AST parsing.".into(),
            category: "precognitive".into(),
            parameters: vec![
                ParamDef {
                    name: "file_path".into(),
                    param_type: "string".into(),
                    description: "Path to source file".into(),
                    required: true,
                    default: None,
                },
                ParamDef {
                    name: "language".into(),
                    param_type: "string".into(),
                    description: "Override language detection".into(),
                    required: false,
                    default: None,
                },
            ],
            default_tier: RiskLevel::Green,
            min_tier: "lite",
        },
        Arc::new(CodeAnalyzeHandler(handle.clone())),
    );

    // web.extract
    registry.register(
        ToolDef {
            name: "web_extract_article".into(),
            description: "Extract clean article text and metadata from a web page URL.".into(),
            category: "precognitive".into(),
            parameters: vec![
                ParamDef {
                    name: "url".into(),
                    param_type: "string".into(),
                    description: "URL to fetch and extract".into(),
                    required: true,
                    default: None,
                },
                ParamDef {
                    name: "max_chars".into(),
                    param_type: "integer".into(),
                    description: "Maximum characters to extract".into(),
                    required: false,
                    default: None,
                },
            ],
            default_tier: RiskLevel::Green,
            min_tier: "standard",
        },
        Arc::new(WebExtractHandler(handle.clone())),
    );

    // embeddings.embed_text
    registry.register(
        ToolDef {
            name: "embeddings_generate".into(),
            description: "Generate semantic embedding vector for a text string.".into(),
            category: "precognitive".into(),
            parameters: vec![ParamDef {
                name: "text".into(),
                param_type: "string".into(),
                description: "Text to embed".into(),
                required: true,
                default: None,
            }],
            default_tier: RiskLevel::Green,
            min_tier: "standard",
        },
        Arc::new(EmbeddingsHandler(handle.clone())),
    );

    // audio.preprocess
    registry.register(
        ToolDef {
            name: "audio_preprocess".into(),
            description: "Preprocess audio: noise reduction, silence trimming, normalization."
                .into(),
            category: "precognitive".into(),
            parameters: vec![
                ParamDef {
                    name: "file_path".into(),
                    param_type: "string".into(),
                    description: "Path to audio file".into(),
                    required: true,
                    default: None,
                },
                ParamDef {
                    name: "sample_rate".into(),
                    param_type: "integer".into(),
                    description: "Target sample rate (default 16000)".into(),
                    required: false,
                    default: None,
                },
            ],
            default_tier: RiskLevel::Green,
            min_tier: "standard",
        },
        Arc::new(AudioPreprocessHandler(handle)),
    );

    tracing::info!("registered 6 pre-cognitive tools (sidecar-backed)");
}
