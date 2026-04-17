//! Pre-Cognitive tools — delegate heavy pre-processing to the Python sidecar.
//!
//! These tools extract structured data from files, images, web pages, and code
//! *before* the LLM sees them, reducing token cost and improving accuracy.

use std::sync::Arc;
use async_trait::async_trait;
use crate::infra::ToolResult;
use crate::sidecar::SidecarBridge;
use crate::tools::registry::{ToolDef, ToolHandler, ToolRegistry, ParamDef};
use crate::safety::RiskLevel;

/// Shared handle to the sidecar bridge, injected into each handler.
#[derive(Clone)]
struct SidecarHandle(Arc<SidecarBridge>);

// ── Image Analyze ───────────────────────────────────────────

struct ImageAnalyzeHandler(SidecarHandle);

#[async_trait]
impl ToolHandler for ImageAnalyzeHandler {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        match self.0 .0.request("image.analyze", params).await {
            Ok(v) => ToolResult { success: true, data: v, error: None },
            Err(e) => ToolResult { success: false, data: serde_json::Value::Null, error: Some(e.to_string()) },
        }
    }
}

// ── Document Extract ────────────────────────────────────────

struct DocumentExtractHandler(SidecarHandle);

#[async_trait]
impl ToolHandler for DocumentExtractHandler {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        match self.0 .0.request("document.extract", params).await {
            Ok(v) => ToolResult { success: true, data: v, error: None },
            Err(e) => ToolResult { success: false, data: serde_json::Value::Null, error: Some(e.to_string()) },
        }
    }
}

// ── Code Analyze AST ────────────────────────────────────────

struct CodeAnalyzeHandler(SidecarHandle);

#[async_trait]
impl ToolHandler for CodeAnalyzeHandler {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        match self.0 .0.request("code.analyze", params).await {
            Ok(v) => ToolResult { success: true, data: v, error: None },
            Err(e) => ToolResult { success: false, data: serde_json::Value::Null, error: Some(e.to_string()) },
        }
    }
}

// ── Web Extract Article ─────────────────────────────────────

struct WebExtractHandler(SidecarHandle);

#[async_trait]
impl ToolHandler for WebExtractHandler {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        match self.0 .0.request("web.extract", params).await {
            Ok(v) => ToolResult { success: true, data: v, error: None },
            Err(e) => ToolResult { success: false, data: serde_json::Value::Null, error: Some(e.to_string()) },
        }
    }
}

// ── Embeddings Generate ─────────────────────────────────────

struct EmbeddingsHandler(SidecarHandle);

#[async_trait]
impl ToolHandler for EmbeddingsHandler {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        match self.0 .0.request("embeddings.embed_text", params).await {
            Ok(v) => ToolResult { success: true, data: v, error: None },
            Err(e) => ToolResult { success: false, data: serde_json::Value::Null, error: Some(e.to_string()) },
        }
    }
}

// ── Audio Preprocess ────────────────────────────────────────

struct AudioPreprocessHandler(SidecarHandle);

#[async_trait]
impl ToolHandler for AudioPreprocessHandler {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        match self.0 .0.request("audio.preprocess", params).await {
            Ok(v) => ToolResult { success: true, data: v, error: None },
            Err(e) => ToolResult { success: false, data: serde_json::Value::Null, error: Some(e.to_string()) },
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
                ParamDef { name: "file_path".into(), param_type: "string".into(), description: "Path to document file".into(), required: true, default: None },
                ParamDef { name: "max_chars".into(), param_type: "integer".into(), description: "Maximum characters to extract".into(), required: false, default: None },
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
            description: "Analyze source code: extract functions, classes, imports via AST parsing.".into(),
            category: "precognitive".into(),
            parameters: vec![
                ParamDef { name: "file_path".into(), param_type: "string".into(), description: "Path to source file".into(), required: true, default: None },
                ParamDef { name: "language".into(), param_type: "string".into(), description: "Override language detection".into(), required: false, default: None },
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
                ParamDef { name: "url".into(), param_type: "string".into(), description: "URL to fetch and extract".into(), required: true, default: None },
                ParamDef { name: "max_chars".into(), param_type: "integer".into(), description: "Maximum characters to extract".into(), required: false, default: None },
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
            parameters: vec![
                ParamDef { name: "text".into(), param_type: "string".into(), description: "Text to embed".into(), required: true, default: None },
            ],
            default_tier: RiskLevel::Green,
            min_tier: "standard",
        },
        Arc::new(EmbeddingsHandler(handle.clone())),
    );

    // audio.preprocess
    registry.register(
        ToolDef {
            name: "audio_preprocess".into(),
            description: "Preprocess audio: noise reduction, silence trimming, normalization.".into(),
            category: "precognitive".into(),
            parameters: vec![
                ParamDef { name: "file_path".into(), param_type: "string".into(), description: "Path to audio file".into(), required: true, default: None },
                ParamDef { name: "sample_rate".into(), param_type: "integer".into(), description: "Target sample rate (default 16000)".into(), required: false, default: None },
            ],
            default_tier: RiskLevel::Green,
            min_tier: "standard",
        },
        Arc::new(AudioPreprocessHandler(handle)),
    );

    tracing::info!("registered 6 pre-cognitive tools (sidecar-backed)");
}
