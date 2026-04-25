//! Image-generation pipeline for K.R.I.A.
//!
//! Modules:
//! - `capabilities`   — tier-aware quality profile resolver (single source of truth)
//! - `prompt_enhancer`— deterministic template-based prompt enrichment
//! - `styles`         — style presets, LoRA catalog, prompt classifier
//! - `comfy`          — headless ComfyUI sidecar lifecycle
//! - `ws_bridge`      — ComfyUI WebSocket progress bridge
//! - `swap`           — Tier B drop-and-swap coordinator
//! - `cloud`          — Tier C cloud fallback (Pollinations.ai / HF Inference)
//! - `orchestrator`   — top-level facade wiring all pieces together

pub mod capabilities;
pub mod cloud;
pub mod comfy;
pub mod mode;
pub mod orchestrator;
pub mod prompt_enhancer;
pub mod styles;
pub mod swap;
pub mod ws_bridge;

pub use orchestrator::{FailureReport, FailureStage, ImageError, ImageOrchestrator, ImageRequest, ImageResult};
pub use capabilities::QualityProfile;
pub use mode::{ImageMode, ModeError, ResolvedMode, resolve_image_mode};
