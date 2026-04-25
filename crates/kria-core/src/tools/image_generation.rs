//! `generate_image` tool — routes through `ImageOrchestrator`.

use std::sync::Arc;

use async_trait::async_trait;

use crate::image::{ImageOrchestrator, ImageRequest, QualityProfile};
use crate::image::orchestrator::FailureReport;
use crate::image::ws_bridge::EventEmitter;
use crate::image::styles::{AspectRatio, ImageStyle};
use crate::infra::ToolResult;
use crate::llm::orchestrator::Orchestrator;
use crate::safety::RiskLevel;
use crate::tools::registry::{ParamDef, ToolDef, ToolHandler, ToolRegistry};

fn param(name: &str, ty: &str, desc: &str, required: bool) -> ParamDef {
    ParamDef {
        name: name.into(),
        param_type: ty.into(),
        description: desc.into(),
        required,
        default: None,
    }
}

// ─── Handler ─────────────────────────────────────────────────────────────────

struct GenerateImageHandler {
    orchestrator: Arc<ImageOrchestrator>,
    /// Closure that forwards image/voice events to the UI layer.
    /// Built by the caller (kria-desktop) as `move |name, payload| app.emit(name, payload)`.
    emit_fn: Arc<dyn Fn(&str, serde_json::Value) + Send + Sync + 'static>,
    /// LLM hardware orchestrator — used to get the current llama-server API URL
    /// and NGL so the image orchestrator can pause it during Tier B VRAM swap.
    llm_orch: Arc<tokio::sync::RwLock<Option<Arc<Orchestrator>>>>,
}

#[async_trait]
impl ToolHandler for GenerateImageHandler {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let prompt = match params["prompt"].as_str() {
            Some(p) if !p.trim().is_empty() => p.trim().to_string(),
            _ => {
                return ToolResult::err(
                    "generate_image: 'prompt' parameter is required and must be non-empty",
                );
            }
        };

        let style: Option<ImageStyle> = params["style"].as_str().map(|s| s.parse().unwrap());
        let aspect: AspectRatio = params["aspect"]
            .as_str()
            .map(|a| a.parse().unwrap())
            .unwrap_or_default();
        let count = params["count"].as_u64().unwrap_or(1).clamp(1, 4) as u32;
        let seed = params["seed"].as_u64();
        let force_cloud = params["force_cloud"].as_bool().unwrap_or(false);

        // New optional params.
        let quality: Option<QualityProfile> = params["quality"]
            .as_str()
            .and_then(|s| s.parse().ok());
        let negative: Option<String> = params["negative"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let enhance: Option<bool> = params["enhance"].as_bool();

        let req = ImageRequest {
            prompt,
            style,
            aspect,
            count,
            seed,
            force_cloud,
            quality,
            negative,
            enhance,
        };

        // Build event emitter that forwards to the UI via the injected emit_fn.
        let emit_fn = self.emit_fn.clone();
        let emitter: EventEmitter = Arc::new(move |name, payload| {
            emit_fn(name, payload);
        });

        // Resolve the LLM hardware orchestrator into a trait object so the
        // image swap path can hard-restart llama-server in CPU mode (Tier B).
        // We pass `Orchestrator` itself (it implements `LlmEvictionController`)
        // rather than a raw `(api_url, ngl)` tuple, because modern llama.cpp
        // does not support dynamic `n_gpu_layers` mutation via `/props`.
        let llm_evictor: Option<Arc<dyn crate::image::swap::LlmEvictionController>> = {
            let guard = self.llm_orch.read().await;
            guard
                .as_ref()
                .map(|o| o.clone() as Arc<dyn crate::image::swap::LlmEvictionController>)
        };

        match self.orchestrator.generate(req, Some(emitter), llm_evictor).await {
            Ok(result) => {
                ToolResult::ok(serde_json::json!({
                    "images": result.images.iter().map(|img| serde_json::json!({
                        "path": img.path.display().to_string(),
                        "sha256": img.sha256,
                        "width": img.width,
                        "height": img.height,
                        "style": img.style,
                        "provenance": img.provenance,
                        "seed": img.seed,
                        "quality": img.quality,
                        "steps": img.steps,
                        "sampler": img.sampler,
                        "cfg_scale": img.cfg_scale,
                        "enhance_mode": img.enhance_mode,
                        "final_prompt": img.final_prompt,
                    })).collect::<Vec<_>>(),
                    "elapsed_ms": result.elapsed_ms,
                    "tier_used": result.tier_used,
                    "swap_count": result.swap_count,
                }))
            }
            Err(e) => {
                let report = FailureReport::from_error(&e);
                ToolResult::err_with_data(
                    format!("Image generation failed: {e}"),
                    serde_json::json!({
                        "failure_report": {
                            "stage": format!("{:?}", report.stage),
                            "provider": report.provider,
                            "http_status": report.http_status,
                            "attempt": report.attempt,
                            "message": report.message,
                            "hint": report.hint,
                        }
                    }),
                )
            }
        }
    }
}

// ─── Registration ─────────────────────────────────────────────────────────────

/// Register the `generate_image` tool.
///
/// - `orchestrator` — image generation orchestrator (ComfyUI + cloud).
/// - `emit_fn` — closure that forwards `(event_name, payload)` to the UI layer.
///   Typically wraps `app_handle.emit(...)` from kria-desktop.
/// - `llm_orch` — hardware orchestrator cell; may be `None` initially (before
///   llama-server starts). The handler reads it lazily at execution time.
pub fn register(
    reg: &ToolRegistry,
    orchestrator: Arc<ImageOrchestrator>,
    emit_fn: Arc<dyn Fn(&str, serde_json::Value) + Send + Sync + 'static>,
    llm_orch: Arc<tokio::sync::RwLock<Option<Arc<Orchestrator>>>>,
) {
    reg.register(
        ToolDef {
            name: "generate_image".into(),
            description: concat!(
                "Generate one or more images from a text prompt using Flux.1-schnell. ",
                "Supports photorealistic, anime, cartoon, line_art, and text_heavy styles. ",
                "Automatically selects the optimal generation tier based on available GPU memory. ",
                "Returns file paths to the generated images on disk."
            ).into(),
            category: "image".into(),
            default_tier: RiskLevel::Yellow,
            min_tier: "standard",
            parameters: vec![
                param(
                    "prompt",
                    "string",
                    "Text description of the image to generate. Be descriptive for best results.",
                    true,
                ),
                param(
                    "style",
                    "string",
                    "Style preset: photorealistic | anime | cartoon | line_art | text_heavy. Omit for auto-detection.",
                    false,
                ),
                param(
                    "aspect",
                    "string",
                    "Aspect ratio: square (1024×1024) | landscape (16:9) | portrait (9:16) | wide (cinema). Default: square.",
                    false,
                ),
                param(
                    "count",
                    "integer",
                    "Number of images to generate (1–4). Tier B always produces 1. Default: 1.",
                    false,
                ),
                param(
                    "seed",
                    "integer",
                    "Random seed for reproducibility. Omit for random.",
                    false,
                ),
                param(
                    "quality",
                    "string",
                    "Quality profile: fast | balanced | high. Default: balanced. High requires Tier S + SDXL model.",
                    false,
                ),
                param(
                    "negative",
                    "string",
                    "Negative prompt (what to avoid). Only effective on Tier S with SDXL High profile; ignored otherwise.",
                    false,
                ),
                param(
                    "enhance",
                    "boolean",
                    "Apply template-based prompt enhancement (adds style-specific keywords). Default: true when prompt is short.",
                    false,
                ),
            ],
        },
        Arc::new(GenerateImageHandler { orchestrator, emit_fn, llm_orch }),
    );
}
