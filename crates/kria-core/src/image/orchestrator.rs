//! `ImageOrchestrator` — top-level façade for image generation.
//!
//! Coordinates: tier admission → ComfyUI sidecar → WebSocket progress →
//! Tier B swap if needed → cloud fallback on Tier C.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};
use rand::Rng;
use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, Semaphore};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::config::ImageGenerationConfig;
use crate::image::capabilities::{QualityProfile, ResolvedWorkflow, resolve as resolve_workflow};
use crate::image::cloud::{CloudFallback, CloudError};
use crate::image::comfy::{ComfyError, ComfySidecar, ComfyLaunchConfig};
use crate::image::mode::{ResolvedMode, resolve_image_mode};
use crate::image::prompt_enhancer::{EnhancedPrompt, enhance};
use crate::image::styles::{AspectRatio, ImageStyle, classify_style_from_prompt};
use crate::image::swap::{EvictionToken, SwapCoordinator, SwapError};
use crate::image::ws_bridge::{spawn_ws_listener, EventEmitter};
use crate::platform::vram::{build_profiler, ImageTier, VramProfiler};

// ─── Public request / response types ─────────────────────────────────────────

/// Input parameters for a single image generation request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageRequest {
    /// Text prompt.
    pub prompt: String,
    /// Style hint. If `None`, auto-classified from prompt.
    pub style: Option<ImageStyle>,
    /// Aspect ratio.
    #[serde(default)]
    pub aspect: AspectRatio,
    /// Number of images to generate (1–4).
    #[serde(default = "default_count")]
    pub count: u32,
    /// Optional fixed seed. When `None`, a random seed is generated per image.
    pub seed: Option<u64>,
    /// Force cloud fallback even on capable tiers.
    #[serde(default)]
    pub force_cloud: bool,
    /// Quality preset. `None` → use `config.default_quality`.
    pub quality: Option<QualityProfile>,
    /// User-supplied negative prompt (only effective on SDXL High path).
    pub negative: Option<String>,
    /// Whether to run the prompt enhancer. `None` → true.
    pub enhance: Option<bool>,
}

fn default_count() -> u32 { 1 }

/// Output from a successful generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedImage {
    /// Absolute path on disk.
    pub path: PathBuf,
    /// SHA-256 hex digest.
    pub sha256: String,
    pub width: u32,
    pub height: u32,
    pub style: String,
    pub provenance: String,
    /// The seed actually used (always set — caller can reproduce by passing it back).
    pub seed: u64,
    /// Quality profile used.
    pub quality: String,
    /// Denoising steps used.
    pub steps: u32,
    /// Sampler used.
    pub sampler: String,
    /// CFG scale used.
    pub cfg_scale: f32,
    /// Enhancement mode: "template", "passthrough", or "cloud".
    pub enhance_mode: String,
    /// Final positive prompt sent to the model (after enhancement).
    pub final_prompt: String,
}

/// Full result payload returned as the tool result data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageResult {
    pub images: Vec<GeneratedImage>,
    pub elapsed_ms: u64,
    pub tier_used: String,
    pub swap_count: usize,
}

/// Fine-grained stage identifier for failure reports.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureStage {
    PromptEnhance,
    TierAdmission,
    Sidecar,
    Workflow,
    Sampler,
    VaeDecode,
    OutputCopy,
    CloudHttp,
    CloudDecode,
    Validation,
    ModelMissing,
    Oom,
    Unknown,
}

impl FailureStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PromptEnhance => "PromptEnhance",
            Self::TierAdmission => "TierAdmission",
            Self::Sidecar => "Sidecar",
            Self::Workflow => "Workflow",
            Self::Sampler => "Sampler",
            Self::VaeDecode => "VaeDecode",
            Self::OutputCopy => "OutputCopy",
            Self::CloudHttp => "CloudHttp",
            Self::CloudDecode => "CloudDecode",
            Self::Validation => "Validation",
            Self::ModelMissing => "ModelMissing",
            Self::Oom => "OOM",
            Self::Unknown => "Unknown",
        }
    }
}

/// Structured failure report surfaced to the tool result and UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureReport {
    pub stage: FailureStage,
    pub provider: String,
    pub http_status: Option<u16>,
    pub attempt: u32,
    pub message: String,
    pub hint: String,
}

impl std::fmt::Display for FailureReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{:?}] {} — {}", self.stage, self.message, self.hint)
    }
}

impl FailureReport {
    pub fn from_error(e: &ImageError) -> Self {
        match e {
            ImageError::Disabled => Self {
                stage: FailureStage::TierAdmission,
                provider: "local".into(),
                http_status: None,
                attempt: 0,
                message: "Image generation is disabled in config".into(),
                hint: "Set image_generation.enabled = true in config/default.toml".into(),
            },
            ImageError::CloudDeclined => Self {
                stage: FailureStage::CloudHttp,
                provider: "cloud".into(),
                http_status: None,
                attempt: 0,
                message: "Cloud fallback declined by user policy".into(),
                hint: "Set image_generation.cloud_fallback to \"always\" or \"auto_offer\"".into(),
            },
            ImageError::Comfy(ce) => {
                let msg = ce.to_string();
                let lower = msg.to_lowercase();
                let is_oom = lower.contains("out of memory") || lower.contains("oom");
                let is_install = matches!(ce, ComfyError::InstallMissing(_));
                let is_early_exit = matches!(ce, ComfyError::EarlyExit { .. });
                let is_health_timeout = matches!(ce, ComfyError::HealthTimeout { .. });

                let stage = if is_oom {
                    FailureStage::Oom
                } else if is_install {
                    FailureStage::ModelMissing
                } else {
                    FailureStage::Sidecar
                };

                let hint = if is_oom {
                    "GPU ran out of memory. Try quality=fast or cloud generation.".into()
                } else if is_install {
                    "ComfyUI is not installed. Run `bash scripts/setup_comfyui.sh` to install it, \
                     or set image_generation.comfy_venv_dir to your existing install.".into()
                } else if is_early_exit {
                    "ComfyUI exited at startup — see the captured Python traceback above. \
                     Most common causes: missing pip dependency, CUDA driver mismatch, or port \
                     already in use. Run `bash scripts/setup_comfyui.sh` to repair the venv.".into()
                } else if is_health_timeout {
                    "ComfyUI did not become healthy in time. The captured log tail above shows \
                     where it stalled (typical: slow custom-node import on cold disk, model \
                     scan, or stuck CUDA init). Either raise image_generation.health_check_timeout_secs \
                     or fix the underlying issue.".into()
                } else {
                    "ComfyUI sidecar error. Check that ComfyUI is installed correctly.".into()
                };

                Self {
                    stage,
                    provider: "local:comfyui".into(),
                    http_status: None,
                    attempt: 0,
                    message: msg,
                    hint,
                }
            }
            ImageError::Cloud(ce) => {
                let msg = ce.to_string();
                let is_http = msg.contains("HTTP") || msg.contains("status");
                Self {
                    stage: if is_http { FailureStage::CloudHttp } else { FailureStage::CloudDecode },
                    provider: "cloud".into(),
                    http_status: None,
                    attempt: 0,
                    message: msg,
                    hint: "All cloud providers failed. Check internet connection or add HF token in Settings.".into(),
                }
            }
            ImageError::Swap(se) => Self {
                stage: FailureStage::Sampler,
                provider: "local".into(),
                http_status: None,
                attempt: 0,
                message: se.to_string(),
                hint: "VRAM swap failed. Try closing other GPU applications.".into(),
            },
            ImageError::WsBridge(msg) => Self {
                stage: FailureStage::VaeDecode,
                provider: "local:comfyui".into(),
                http_status: None,
                attempt: 0,
                message: msg.clone(),
                hint: "ComfyUI WebSocket disconnected. The job may have completed — check ~/.kria/uploads/generated.".into(),
            },
            ImageError::Reported(r) => *r.clone(),
            _ => Self {
                stage: FailureStage::Unknown,
                provider: "unknown".into(),
                http_status: None,
                attempt: 0,
                message: e.to_string(),
                hint: "Check the KRIA logs for details.".into(),
            },
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    #[error("Image generation disabled")]
    Disabled,
    #[error("Cloud fallback declined by user policy")]
    CloudDeclined,
    #[error("Swap failed: {0}")]
    Swap(#[from] SwapError),
    #[error("ComfyUI error: {0}")]
    Comfy(#[from] ComfyError),
    #[error("Cloud error: {0}")]
    Cloud(#[from] CloudError),
    #[error("WebSocket bridge error: {0}")]
    WsBridge(String),
    #[error("Output directory error: {0}")]
    OutputDir(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Image generation failed: {0}")]
    Reported(Box<FailureReport>),
}

// ─── Resolved-job context (internal) ─────────────────────────────────────────

/// All parameters resolved before dispatching to local/cloud generation.
struct ResolvedJob {
    positive_prompt: String,
    negative_prompt: String,
    style: ImageStyle,
    width: u32,
    height: u32,
    base_seed: u64,
    count: u32,
    wf: ResolvedWorkflow,
    enhance_mode: String,
}

// ─── Orchestrator ─────────────────────────────────────────────────────────────

/// Manages the full image generation pipeline.
pub struct ImageOrchestrator {
    cfg: ImageGenerationConfig,
    sidecar: Arc<ComfySidecar>,
    cloud: Arc<CloudFallback>,
    swap_coord: Arc<SwapCoordinator>,
    profiler: Arc<dyn VramProfiler>,
    job_sem: Arc<Semaphore>,
    output_dir: PathBuf,
    models_dir: PathBuf,
    lora_strength: f32,
    /// Tier B hang counter — incremented each time VRAM barrier times out + retries fail.
    hang_count: AtomicU32,
    /// When true, all Tier B requests are rerouted to cloud for the rest of the session.
    session_degraded: AtomicBool,
    /// Optional callback to pause the audio/STT pipeline during GPU swaps.
    audio_pause_fn: OnceLock<Arc<dyn Fn() + Send + Sync>>,
    /// Optional callback to resume the audio/STT pipeline after GPU swaps.
    audio_resume_fn: OnceLock<Arc<dyn Fn() + Send + Sync>>,
    /// Cache directory for KV-cache snapshots and conditioning blobs.
    cache_dir: PathBuf,
}

impl ImageOrchestrator {
    /// Build an orchestrator from config.
    ///
    /// `kria_data_dir` is the resolved `~/.kria/` directory.
    pub fn new(cfg: ImageGenerationConfig, kria_data_dir: &Path) -> Arc<Self> {
        let resolve = |s: &str, default: &str| -> PathBuf {
            if s.is_empty() {
                kria_data_dir.join(default)
            } else if Path::new(s).is_relative() {
                kria_data_dir.join(s)
            } else {
                PathBuf::from(s)
            }
        };

        let output_dir = resolve(&cfg.output_dir, "uploads/generated");
        let comfy_venv = resolve(&cfg.comfy_venv_dir, "comfyui/.venv");
        let models_dir = resolve(&cfg.comfy_models_dir, "comfyui/models");
        let models_dir_owned = models_dir.clone();

        let comfy_cfg = ComfyLaunchConfig {
            port: cfg.comfy_port,
            venv_dir: comfy_venv,
            models_dir,
            output_dir: output_dir.clone(),
            extra_args: Vec::new(),
            health_check_timeout_secs: cfg.health_check_timeout_secs,
        };

        let sidecar = ComfySidecar::new(comfy_cfg);
        let cloud = CloudFallback::new(
            &cfg.pollinations_base_url,
            cfg.cloud_fallback != "off",
        );
        let swap_coord = SwapCoordinator::new(cfg.defrag_every_n_swaps);
        let profiler = build_profiler();
        let job_sem = Arc::new(Semaphore::new(cfg.max_concurrent_jobs.max(1)));
        let lora_strength = cfg.default_lora_strength;

        let cache_dir = kria_data_dir.join("cache");

        Arc::new(Self {
            cfg,
            sidecar,
            cloud,
            swap_coord,
            profiler,
            job_sem,
            output_dir,
            models_dir: models_dir_owned,
            lora_strength,
            hang_count: AtomicU32::new(0),
            session_degraded: AtomicBool::new(false),
            audio_pause_fn: OnceLock::new(),
            audio_resume_fn: OnceLock::new(),
            cache_dir,
        })
    }

    /// Wire optional audio pause/resume callbacks so the orchestrator can
    /// mute the STT pipeline during Tier B VRAM swaps.
    ///
    /// Safe to call multiple times — only the first call takes effect.
    pub fn wire_audio_hooks(
        &self,
        pause: impl Fn() + Send + Sync + 'static,
        resume: impl Fn() + Send + Sync + 'static,
    ) {
        let _ = self.audio_pause_fn.set(Arc::new(pause));
        let _ = self.audio_resume_fn.set(Arc::new(resume));
    }

    /// Returns `true` if two or more consecutive VRAM swap hangs degraded
    /// this session to cloud-only routing.
    pub fn is_session_degraded(&self) -> bool {
        self.session_degraded.load(Ordering::Acquire)
    }

    /// Reset the sticky degradation flag (useful for testing).
    #[cfg(test)]
    pub fn reset_session_degradation(&self) {
        self.session_degraded.store(false, Ordering::Release);
        self.hang_count.store(0, Ordering::Release);
    }

    // ─── Public API ────────────────────────────────────────────────────────────

    /// Generate images per the given request. Handles all tier routing.
    pub async fn generate(
        self: &Arc<Self>,
        req: ImageRequest,
        emitter: Option<EventEmitter>,
        llm_evictor: Option<Arc<dyn crate::image::swap::LlmEvictionController>>,
    ) -> Result<ImageResult, ImageError> {
        if !self.cfg.enabled {
            return Err(ImageError::Disabled);
        }

        let start = Instant::now();

        // ── Step 1: resolve style ────────────────────────────────────────────
        let style = req.style.unwrap_or_else(|| {
            classify_style_from_prompt(&req.prompt)
                .unwrap_or(ImageStyle::Photorealistic)
        });

        // ── Step 2: resolve aspect ───────────────────────────────────────────
        let (width, height) = req.aspect.dimensions();

        // ── Step 3: resolve tier ─────────────────────────────────────────────
        // Read env var FIRST so we can decide whether to honour req.force_cloud.
        // KRIA_IMAGE_MODE=local_only must win over the LLM's force_cloud hint.
        let env_image_mode = std::env::var("KRIA_IMAGE_MODE")
            .ok()
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|v| !v.is_empty());
        let env_forces_local = matches!(env_image_mode.as_deref(), Some("local_only"));

        let tier = if req.force_cloud && !env_forces_local {
            // LLM requested cloud — signal Tier C only when env doesn't say local_only.
            ImageTier::CRejectOrCloud
        } else if !self.cfg.tier_override.is_empty() {
            self.cfg.tier_override.parse().unwrap_or(ImageTier::CRejectOrCloud)
        } else {
            let snapshot = self.profiler.snapshot().await;
            ImageTier::from_snapshot(&snapshot)
        };

        // ── Step 3b: resolve routing mode (env > force_cloud > config > tier) ─
        // Resolution order (highest → lowest priority):
        //   1. KRIA_IMAGE_MODE env var — explicit user/operator override; always wins
        //   2. req.force_cloud — LLM hint to prefer cloud; ignored when env says local_only
        //   3. config image_mode + tier-based default
        let mut resolved_mode = if env_forces_local {
            // Env var says local_only — force_cloud is ignored; tier is correctly resolved above.
            if tier == crate::platform::vram::ImageTier::CRejectOrCloud {
                return Err(ImageError::Reported(Box::new(FailureReport {
                    stage: FailureStage::TierAdmission,
                    provider: "mode_resolver".into(),
                    http_status: None,
                    attempt: 0,
                    message: "local generation requires a GPU (current tier is C — no discrete GPU detected)".into(),
                    hint: "Set tier_override=\"b\" in config/default.toml [image_generation].".into(),
                })));
            }
            ResolvedMode::LocalOnly
        } else if req.force_cloud {
            // LLM hint: prefer cloud. Only reached when env doesn't say local_only.
            ResolvedMode::CloudOnly
        } else {
            match resolve_image_mode(
                &self.cfg.image_mode,
                &self.cfg.cloud_fallback,
                tier,
            ) {
                Ok(m) => m,
                Err(e) => {
                    return Err(ImageError::Reported(Box::new(FailureReport {
                        stage: FailureStage::TierAdmission,
                        provider: "mode_resolver".into(),
                        http_status: None,
                        attempt: 0,
                        message: e.to_string(),
                        hint: "Check image_mode in config or KRIA_IMAGE_MODE env var.".into(),
                    })));
                }
            }
        };

        // ── Step 3c: sticky session degradation ─────────────────────────────
        if self.session_degraded.load(Ordering::Acquire) {
            if matches!(
                resolved_mode,
                ResolvedMode::LocalThenCloud
                    | ResolvedMode::CloudOnly
                    | ResolvedMode::CloudThenLocal
            ) {
                info!("ImageOrchestrator: session degraded — forcing CloudOnly routing");
                resolved_mode = ResolvedMode::CloudOnly;
            } else {
                return Err(ImageError::Reported(Box::new(FailureReport {
                    stage: FailureStage::TierAdmission,
                    provider: "local".into(),
                    http_status: None,
                    attempt: 0,
                    message: "Local image generation is degraded this session (VRAM swap timeouts).".into(),
                    hint: "Restart KRIA to reset, or set image_mode = \"local_then_cloud\".".into(),
                })));
            }
        }

        // ── Step 4: resolve quality profile ─────────────────────────────────
        let profile = req.quality
            .unwrap_or_else(|| self.cfg.default_quality.parse().unwrap_or_default());

        // ── Step 5: check SDXL model availability ───────────────────────────
        let sdxl_available = self.sdxl_model_available();

        // ── Step 6: resolve workflow (single source of truth) ────────────────
        let wf = resolve_workflow(profile, tier, style, sdxl_available);

        // ── Step 7: resolve base seed (always randomize if not provided) ─────
        let base_seed = req.seed.unwrap_or_else(|| rand::thread_rng().gen::<u64>() % 0xFFFF_FFFF);

        // ── Step 8: prompt enhancement (template-only; zero cost on Tier B/C) ─
        let should_enhance = req.enhance.unwrap_or(true);
        let enhanced = if should_enhance {
            let user_neg = req.negative.clone().unwrap_or_default();
            let mut ep = enhance(&req.prompt, style, profile, tier, wf.use_sdxl);
            // Merge user-supplied negative on top.
            if !user_neg.is_empty() && wf.use_sdxl {
                ep.negative = if ep.negative.is_empty() {
                    user_neg
                } else {
                    format!("{}, {}", ep.negative, user_neg)
                };
            }
            ep
        } else {
            EnhancedPrompt {
                positive: req.prompt.clone(),
                negative: req.negative.clone().unwrap_or_default(),
                used_style: style,
                was_enhanced: false,
                mode: "passthrough",
            }
        };

        info!(
            tier = tier.as_str(),
            style = style.as_str(),
            quality = wf.effective_profile.as_str(),
            steps = wf.steps,
            sampler = wf.sampler,
            enhanced = enhanced.was_enhanced,
            width,
            height,
            "ImageOrchestrator: generate"
        );

        // ── Step 9: build resolved job context ───────────────────────────────
        let job = ResolvedJob {
            positive_prompt: enhanced.positive,
            negative_prompt: enhanced.negative,
            style,
            width,
            height,
            base_seed,
            count: req.count.clamp(1, 4),
            wf,
            enhance_mode: enhanced.mode.to_string(),
        };

        // ── Step 10: dispatch (mode-aware) ────────────────────────────────────
        // Phase 13F: keep a clone for post-dispatch TTS announcement.
        let announce_emitter = emitter.clone();
        let result: Result<ImageResult, ImageError> = match resolved_mode {
            ResolvedMode::LocalOnly => match tier {
                ImageTier::SHighRes | ImageTier::AStandard => {
                    self.generate_local(&job, emitter).await
                }
                ImageTier::BDropSwap => {
                    self.generate_with_swap(&job, emitter, llm_evictor.clone()).await
                }
                ImageTier::CRejectOrCloud => {
                    // resolve_image_mode already rejected this combination;
                    // this arm is unreachable in practice.
                    Err(ImageError::Reported(Box::new(FailureReport {
                        stage: FailureStage::TierAdmission,
                        provider: "mode_resolver".into(),
                        http_status: None,
                        attempt: 0,
                        message: "LocalOnly requested but no GPU detected".into(),
                        hint: "Set KRIA_IMAGE_MODE=cloud_only or install a GPU.".into(),
                    })))
                }
            },

            ResolvedMode::CloudOnly => self.generate_cloud(&job).await,

            ResolvedMode::LocalThenCloud => {
                let local_result = match tier {
                    ImageTier::SHighRes | ImageTier::AStandard => {
                        self.generate_local(&job, emitter).await
                    }
                    ImageTier::BDropSwap => {
                        self.generate_with_swap(&job, emitter, llm_evictor.clone()).await
                    }
                    ImageTier::CRejectOrCloud => Err(ImageError::Reported(Box::new(FailureReport {
                        stage: FailureStage::TierAdmission,
                        provider: "tier".into(),
                        http_status: None,
                        attempt: 0,
                        message: "no GPU; cannot attempt local generation".into(),
                        hint: "Falling back to cloud.".into(),
                    }))),
                };
                match local_result {
                    Ok(r) => Ok(r),
                    Err(e) => {
                        warn!(error = %e, "LocalThenCloud: local failed, trying cloud fallback");
                        self.generate_cloud(&job).await.map(|mut r| {
                            r.tier_used = format!("cloud_fallback:{}", tier.as_str());
                            r
                        })
                    }
                }
            }

            ResolvedMode::CloudThenLocal => {
                match self.generate_cloud(&job).await {
                    Ok(r) => Ok(r),
                    Err(e) => {
                        warn!(error = %e, "CloudThenLocal: cloud failed, trying local fallback");
                        match tier {
                            ImageTier::SHighRes | ImageTier::AStandard => {
                                self.generate_local(&job, emitter).await
                            }
                            ImageTier::BDropSwap => {
                                self.generate_with_swap(&job, emitter, llm_evictor.clone()).await
                            }
                            ImageTier::CRejectOrCloud => Err(e),
                        }
                    }
                }
            }
        };

        let elapsed_ms = start.elapsed().as_millis() as u64;
        match result {
            Ok(mut r) => {
                // Phase 13F: voice announce — "Image ready."
                if let Some(emit) = &announce_emitter {
                    emit("voice:speak", serde_json::json!({
                        "text": "Image ready.",
                        "priority": "normal"
                    }));
                }
                r.elapsed_ms = elapsed_ms;
                r.tier_used = tier.as_str().to_string();
                r.swap_count = self.swap_coord.count();
                Ok(r)
            }
            Err(e) => Err(e),
        }
    }

    /// Returns true if the SDXL Lightning checkpoint is present on disk.
    fn sdxl_model_available(&self) -> bool {
        const SDXL_LIGHTNING_MODEL: &str = "juggernautXL_v9Lightning.safetensors";
        self.models_dir
            .join("checkpoints")
            .join(SDXL_LIGHTNING_MODEL)
            .exists()
    }

    // ─── Local generation (Tier S / A) ───────────────────────────────────────

    async fn generate_local(
        &self,
        job: &ResolvedJob,
        emitter: Option<EventEmitter>,
    ) -> Result<ImageResult, ImageError> {
        // Acquire job semaphore.
        let _permit = self.job_sem.acquire().await
            .map_err(|_| ImageError::Comfy(ComfyError::NotRunning))?;

        // Ensure sidecar is running.
        self.sidecar.ensure_running().await?;

        let mut all_images = Vec::new();

        for i in 0..job.count {
            let img_seed = job.base_seed.wrapping_add(u64::from(i));

            // Build workflow for this image.
            let workflow = if job.wf.use_sdxl {
                self.build_sdxl_lightning_graph(
                    &job.positive_prompt,
                    &job.negative_prompt,
                    job.width,
                    job.height,
                    img_seed,
                )?
            } else {
                self.build_schnell_workflow(
                    &job.positive_prompt,
                    job.style,
                    job.width,
                    job.height,
                    img_seed,
                    &job.wf,
                )?
            };

            // Submit job.
            let queued = self.sidecar.submit_workflow(workflow).await?;
            info!(prompt_id = %queued.prompt_id, img_index = i, "ComfyUI job queued");

            // Wait for completion.
            let outputs = self.wait_for_job(queued, if i == 0 { emitter.clone() } else { None }).await?;

            // Collect output files.
            let imgs = self.collect_outputs(outputs, job, img_seed).await?;
            all_images.extend(imgs);
        }

        Ok(ImageResult {
            images: all_images,
            elapsed_ms: 0,
            tier_used: String::new(),
            swap_count: 0,
        })
    }

    // ─── Tier B drop-and-swap ─────────────────────────────────────────────────

    async fn generate_with_swap(
        &self,
        job: &ResolvedJob,
        emitter: Option<EventEmitter>,
        llm_evictor: Option<Arc<dyn crate::image::swap::LlmEvictionController>>,
    ) -> Result<ImageResult, ImageError> {
        let evictor = match llm_evictor {
            Some(c) => c,
            None => {
                // No LLM running — treat as Tier A (no eviction needed).
                return self.generate_local(job, emitter).await;
            }
        };

        // VRAM needed for Flux.1-schnell GGUF Q4_K_S.
        //
        // On 6 GB / 8 GB GPUs, ComfyUI can already hold part of the model
        // resident between jobs. A fixed 4500 MB barrier then becomes
        // unreachable after LLM eviction even though generation can proceed.
        // Use a dynamic threshold for smaller cards while keeping the
        // existing conservative default for larger VRAM tiers.
        let snap = self.profiler.snapshot().await;
        let required_mb = if snap.total_mb > 0 && snap.total_mb <= 8_192 {
            // ~38% of total VRAM, clamped to a safe floor for Flux sampler headroom.
            let dynamic = ((snap.total_mb as f64) * 0.38_f64).round() as u64;
            dynamic.max(2_000)
        } else {
            ImageTier::BDropSwap.required_free_mb()
        };
        info!(
            total_vram_mb = snap.total_mb,
            free_vram_mb = snap.free_mb,
            required_mb,
            "tier-b swap barrier target resolved"
        );

        // Local emit helper — borrows a clone so `emitter` is still available later.
        let emit_event = {
            let emitter_clone = emitter.clone();
            move |name: &'static str, val: serde_json::Value| {
                if let Some(e) = &emitter_clone {
                    e(name, val);
                }
            }
        };

        // ── Phase 1: announce swap + freeze audio ─────────────────────────────
        emit_event("image:tier_blackout", serde_json::json!({
            "free_mb": 0_u64,
            "required_mb": required_mb,
            "stage": "evicting_llm"
        }));
        // Hinglish voice announce — low-priority TTS (fire-and-forget).
        emit_event("voice:speak", serde_json::json!({
            "text": "Generating your image, ek moment.",
            "priority": "low"
        }));

        // Pause voice/STT capture during GPU reallocation.
        if let Some(pause) = self.audio_pause_fn.get() {
            pause();
        }

        // ── Phase 2: hard-restart LLM in CPU mode ────────────────────────────
        // The previous revision tried `POST /props {"n_gpu_layers": 0}` here,
        // which modern llama.cpp rejects with HTTP 501. The eviction
        // controller now performs a SIGTERM → wait → respawn(--n-gpu-layers 0)
        // cycle, with slot-0 KV cache preserved across the restart.
        let token = match EvictionToken::acquire(
            evictor.clone(),
            self.profiler.clone(),
            required_mb,
        ).await {
            Ok(t) => t,
            Err(SwapError::VramTimeout { free_mb, required_mb: req_mb }) => {
                warn!(
                    free_mb,
                    required_mb = req_mb,
                    "VramBarrier timeout after CPU-mode restart — retrying once"
                );
                emit_event("image:tier_blackout", serde_json::json!({
                    "free_mb": free_mb,
                    "required_mb": req_mb,
                    "stage": "retry_after_interrupt"
                }));

                // Brief pause for driver-side cleanup (NVML often lags
                // SIGKILL by 100-500 ms even after the process is reaped).
                tokio::time::sleep(Duration::from_millis(600)).await;

                // Single retry — the controller is idempotent.
                match EvictionToken::acquire(
                    evictor.clone(),
                    self.profiler.clone(),
                    required_mb,
                ).await {
                    Ok(t) => t,
                    Err(e) => {
                        // Both attempts failed — increment hang counter.
                        let hangs = self.hang_count.fetch_add(1, Ordering::AcqRel) + 1;
                        warn!(hangs, "EvictionToken: swap retry also failed");
                        if hangs >= 2 {
                            self.session_degraded.store(true, Ordering::Release);
                            warn!("ImageOrchestrator: 2 hang timeouts — session degraded to cloud-only");
                            emit_event("image:session_degraded", serde_json::json!({
                                "level": "cloud_only",
                                "hang_count": hangs
                            }));
                        }
                        if let Some(resume) = self.audio_resume_fn.get() {
                            resume();
                        }
                        return Err(e.into());
                    }
                }
            }
            Err(e) => {
                if let Some(resume) = self.audio_resume_fn.get() {
                    resume();
                }
                return Err(e.into());
            }
        };

        // VRAM cleared and stable.
        emit_event("image:tier_blackout", serde_json::json!({
            "free_mb": required_mb,
            "required_mb": required_mb,
            "stage": "ready"
        }));

        // ── Phase 3: run image generation ─────────────────────────────────────
        let result = self.generate_local(job, emitter).await;

        // ── Phase 4: restore LLM ──────────────────────────────────────────────
        token.restore().await;

        // Resume voice capture.
        if let Some(resume) = self.audio_resume_fn.get() {
            resume();
        }

        // Eager warmup is handled implicitly: the controller's GPU respawn
        // runs llama-server's built-in warmup pass before reporting healthy,
        // and the slot KV-cache restore re-imports the prior conversation's
        // tokens. No additional `/completion` poke is needed here.
        emit_event("image:tier_blackout", serde_json::json!({ "stage": "restored" }));

        // ── Phase 5: defrag check ─────────────────────────────────────────────
        if self.swap_coord.tick() {
            let sidecar = self.sidecar.clone();
            tokio::spawn(async move {
                info!("SwapCoordinator: defrag — restarting ComfySidecar");
                sidecar.stop().await;
                if let Err(e) = sidecar.start().await {
                    error!(error = %e, "ComfySidecar defrag restart failed");
                }
            });
        }

        result
    }

    // ─── Cloud fallback ───────────────────────────────────────────────────────

    async fn generate_cloud(
        &self,
        job: &ResolvedJob,
    ) -> Result<ImageResult, ImageError> {
        // Build a style-prefixed prompt for cloud providers.
        let styled_prompt = format!("{} {}", job.style.as_str(), job.positive_prompt);

        let mut all_images = Vec::new();

        for i in 0..job.count {
            // Always use a per-image seed — eliminates "same image every run" bug.
            let img_seed = job.base_seed.wrapping_add(u64::from(i));

            let result = self.cloud.generate(&styled_prompt, job.width, job.height, Some(img_seed)).await?;

            let ext = if result.png_bytes.starts_with(b"\x89PNG\r\n\x1a\n") { "png" } else { "jpg" };
            let out_path = self.save_bytes(&result.png_bytes, ext).await?;
            let sha = sha256_hex(&result.png_bytes);

            all_images.push(GeneratedImage {
                path: out_path,
                sha256: sha,
                width: job.width,
                height: job.height,
                style: job.style.as_str().to_string(),
                provenance: result.provenance,
                seed: img_seed,
                quality: job.wf.effective_profile.as_str().to_string(),
                steps: job.wf.steps,
                sampler: job.wf.sampler.to_string(),
                cfg_scale: job.wf.cfg,
                enhance_mode: "cloud".to_string(),
                final_prompt: styled_prompt.clone(),
            });
        }

        Ok(ImageResult {
            images: all_images,
            elapsed_ms: 0,
            tier_used: String::new(),
            swap_count: 0,
        })
    }

    // ─── Helpers ──────────────────────────────────────────────────────────────

    /// Build a Flux-schnell (GGUF) ComfyUI workflow graph.
    ///
    /// Uses the `ResolvedWorkflow` for sampler/steps/cfg — no hard-coded values.
    /// CFG is always 1.0 for Schnell (enforced by capabilities resolver).
    fn build_schnell_workflow(
        &self,
        prompt: &str,
        style: ImageStyle,
        width: u32,
        height: u32,
        seed: u64,
        wf: &ResolvedWorkflow,
    ) -> Result<serde_json::Value, ImageError> {
        // Flux requires the T5 encoder for correct conditioning. Using CLIP-L
        // for both DualCLIP slots can produce near-black outputs.
        if !self.t5_model_available() {
            return Err(ImageError::Comfy(ComfyError::InstallMissing(
                "Flux T5 encoder missing: clip/t5xxl_fp8_e4m3fn.safetensors. Run `KRIA_DOWNLOAD_T5=1 python scripts/download_models.py --comfyui`.".into(),
            )));
        }
        let clip2 = "t5xxl_fp8_e4m3fn.safetensors";

        let lora = style.lora_filename().and_then(|lora_file| {
            let lora_path = self.models_dir.join("loras").join(lora_file);
            if lora_path.exists() {
                Some(lora_file)
            } else {
                warn!(
                    style = style.as_str(),
                    lora_file,
                    path = %lora_path.display(),
                    "Style LoRA not found on disk — building Flux workflow without it"
                );
                None
            }
        });
        let lora_strength = self.lora_strength;

        // Node routing:
        //   1 = UnetLoaderGGUF / UnetLoaderNF4  → [MODEL]
        //   2 = DualCLIPLoader                  → [CLIP]
        //   3 = VAELoader                       → [VAE]
        //   4 = CLIPTextEncode (positive)        → [CONDITIONING]
        //   5 = CLIPTextEncode (negative/empty)  → [CONDITIONING]
        //   6 = EmptySD3LatentImage              → [LATENT]
        //   7 = KSampler                         → [LATENT]
        //   8 = VAEDecodeTiled                   → [IMAGE]
        //   9 = SaveImage
        //  10 = LoraLoader (optional)            → [MODEL, CLIP]

        // FP8 model uses a different loader node.
        let unet_loader = if wf.model_file.contains("fp8") {
            "UnetLoaderNF4"
        } else {
            "UnetLoaderGGUF"
        };

        let model_input: serde_json::Value = if lora.is_some() {
            serde_json::json!(["10", 0])
        } else {
            serde_json::json!(["1", 0])
        };
        let clip_input: serde_json::Value = if lora.is_some() {
            serde_json::json!(["10", 1])
        } else {
            serde_json::json!(["2", 0])
        };

        let mut graph = serde_json::json!({
            "1": {
                "class_type": unet_loader,
                "inputs": { "unet_name": wf.model_file }
            },
            "2": {
                "class_type": "DualCLIPLoader",
                "inputs": {
                    "clip_name1": "clip_l.safetensors",
                    "clip_name2": clip2,
                    "type": "flux"
                }
            },
            "3": {
                "class_type": "VAELoader",
                "inputs": { "vae_name": "ae.safetensors" }
            },
            "4": {
                "class_type": "CLIPTextEncode",
                "inputs": { "text": prompt, "clip": clip_input.clone() }
            },
            "5": {
                // Schnell: negative prompt is always empty (model ignores it).
                "class_type": "CLIPTextEncode",
                "inputs": { "text": "", "clip": clip_input }
            },
            "6": {
                "class_type": "EmptySD3LatentImage",
                "inputs": { "width": width, "height": height, "batch_size": 1 }
            },
            "7": {
                "class_type": "KSampler",
                "inputs": {
                    "model": model_input,
                    "positive": ["4", 0],
                    "negative": ["5", 0],
                    "latent_image": ["6", 0],
                    "seed": seed,
                    "steps": wf.steps,
                    "cfg": wf.cfg,
                    "sampler_name": wf.sampler,
                    "scheduler": wf.scheduler,
                    "denoise": 1.0
                }
            },
            "8": {
                "class_type": "VAEDecodeTiled",
                "inputs": {
                    "samples": ["7", 0],
                    "vae": ["3", 0],
                    "tile_size": 512,
                    "overlap": 64,
                    "temporal_size": 64,
                    "temporal_overlap": 8
                }
            },
            "9": {
                "class_type": "SaveImage",
                "inputs": { "images": ["8", 0], "filename_prefix": "kria" }
            }
        });

        if let Some(lora_file) = lora {
            graph["10"] = serde_json::json!({
                "class_type": "LoraLoader",
                "inputs": {
                    "model": ["1", 0],
                    "clip": ["2", 0],
                    "lora_name": lora_file,
                    "strength_model": lora_strength,
                    "strength_clip": lora_strength
                }
            });
        }

        Ok(graph)
    }

    /// Build the SDXL Lightning (JuggernautXL v9) ComfyUI workflow graph.
    ///
    /// Strict 6GB profile:
    ///   - sampler: euler
    ///   - scheduler: sgm_uniform
    ///   - steps: 6
    ///   - cfg: 2.0
    ///   - decode: VAEDecodeTiled with tile_size=512
    fn build_sdxl_lightning_graph(
        &self,
        positive: &str,
        negative: &str,
        width: u32,
        height: u32,
        seed: u64,
    ) -> Result<serde_json::Value, ImageError> {
        const SDXL_LIGHTNING_MODEL: &str = "juggernautXL_v9Lightning.safetensors";

        // SDXL Lightning graph:
        //  1 = CheckpointLoaderSimple → [MODEL, CLIP, VAE]
        //  2 = CLIPTextEncode (positive conditioning)
        //  3 = CLIPTextEncode (negative conditioning)
        //  4 = EmptyLatentImage
        //  5 = KSampler
        //  6 = VAEDecodeTiled
        //  7 = SaveImage
        Ok(serde_json::json!({
            "1": {
                "class_type": "CheckpointLoaderSimple",
                "inputs": { "ckpt_name": SDXL_LIGHTNING_MODEL }
            },
            "2": {
                "class_type": "CLIPTextEncode",
                "inputs": { "text": positive, "clip": ["1", 1] }
            },
            "3": {
                "class_type": "CLIPTextEncode",
                // Explicit negative prompt wiring for SDXL conditioning.
                "inputs": { "text": negative, "clip": ["1", 1] }
            },
            "4": {
                "class_type": "EmptyLatentImage",
                "inputs": { "width": width, "height": height, "batch_size": 1 }
            },
            "5": {
                "class_type": "KSampler",
                "inputs": {
                    "model": ["1", 0],
                    "positive": ["2", 0],
                    "negative": ["3", 0],
                    "latent_image": ["4", 0],
                    "seed": seed,
                    "steps": 6,
                    "cfg": 2.0,
                    "sampler_name": "euler",
                    "scheduler": "sgm_uniform",
                    "denoise": 1.0
                }
            },
            "6": {
                "class_type": "VAEDecodeTiled",
                "inputs": {
                    "samples": ["5", 0],
                    "vae": ["1", 2],
                    "tile_size": 512,
                    "overlap": 64,
                    "temporal_size": 64,
                    "temporal_overlap": 8
                }
            },
            "7": {
                "class_type": "SaveImage",
                "inputs": { "images": ["6", 0], "filename_prefix": "kria" }
            }
        }))
    }

    /// Wait for a ComfyUI job to complete via WebSocket.
    async fn wait_for_job(
        &self,
        job: crate::image::comfy::QueuedJob,
        emitter: Option<EventEmitter>,
    ) -> Result<Vec<crate::image::ws_bridge::ComfyOutput>, ImageError> {
        let (tx, rx) = oneshot::channel();
        let cancel = CancellationToken::new();
        let timeout_secs = if self.cfg.local_timeout_secs > 0 {
            self.cfg.local_timeout_secs
        } else {
            300 // 5 min hard cap
        };
        let timeout = Duration::from_secs(timeout_secs);

        if emitter.is_some() {
            spawn_ws_listener(
                self.cfg.comfy_port,
                job.client_id,
                job.prompt_id.clone(),
                emitter,
                tx,
                cancel.clone(),
                timeout,
            );
        } else {
            // Poll /history instead.
            let port = self.cfg.comfy_port;
            let pid = job.prompt_id.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let result = crate::image::ws_bridge::recover_from_history_pub(port, &pid).await;
                let _ = tx.send(result);
            });
        }

        rx.await
            .map_err(|_| ImageError::WsBridge("channel dropped".into()))?
            .map_err(|e| ImageError::WsBridge(e.to_string()))
    }

    /// Move ComfyUI output files to `output_dir`, compute hashes, and tag with generation metadata.
    async fn collect_outputs(
        &self,
        outputs: Vec<crate::image::ws_bridge::ComfyOutput>,
        job: &ResolvedJob,
        img_seed: u64,
    ) -> Result<Vec<GeneratedImage>, ImageError> {
        tokio::fs::create_dir_all(&self.output_dir).await
            .map_err(|e| ImageError::OutputDir(e.to_string()))?;

        let comfy_output_dir = self.output_dir.clone();
        let mut results = Vec::new();

        for out in &outputs {
            let src = comfy_output_dir.join(&out.filename);
            let dst = self.output_dir.join(&out.filename);

            let bytes = tokio::fs::read(&src).await.unwrap_or_default();
            let sha = sha256_hex(&bytes);

            results.push(GeneratedImage {
                path: dst,
                sha256: sha,
                width: job.width,
                height: job.height,
                style: job.style.as_str().to_string(),
                provenance: "local:comfyui".to_string(),
                seed: img_seed,
                quality: job.wf.effective_profile.as_str().to_string(),
                steps: job.wf.steps,
                sampler: job.wf.sampler.to_string(),
                cfg_scale: job.wf.cfg,
                enhance_mode: job.enhance_mode.clone(),
                final_prompt: job.positive_prompt.clone(),
            });
        }
        Ok(results)
    }

    /// Save raw bytes to a timestamped file in `output_dir`.
    async fn save_bytes(&self, bytes: &[u8], ext: &str) -> Result<PathBuf, ImageError> {
        tokio::fs::create_dir_all(&self.output_dir).await
            .map_err(|e| ImageError::OutputDir(e.to_string()))?;

        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let filename = format!("kria_{}.{}", ts, ext);
        let path = self.output_dir.join(&filename);
        tokio::fs::write(&path, bytes).await?;
        Ok(path)
    }

    // ─── Private helpers ──────────────────────────────────────────────────────

    /// Returns `true` if the T5-XXL FP8 text encoder model is present on disk.
    fn t5_model_available(&self) -> bool {
        self.models_dir
            .join("clip")
            .join("t5xxl_fp8_e4m3fn.safetensors")
            .exists()
    }

    /// Compute the conditioning-cache file path for a given prompt + encoder.
    #[allow(dead_code)]
    fn conditioning_cache_path(&self, prompt: &str, clip2: &str) -> PathBuf {
        let key = sha256_hex(format!("{}:{}", prompt, clip2).as_bytes());
        self.cache_dir.join("conditioning").join(format!("{}.bin", key))
    }

    /// LRU eviction: remove oldest conditioning-cache blobs until total ≤ 500 MB.
    #[allow(dead_code)]
    async fn evict_conditioning_cache_if_needed(&self) {
        const MAX_BYTES: u64 = 500 * 1024 * 1024;
        let cache_dir = self.cache_dir.join("conditioning");
        if !cache_dir.exists() {
            return;
        }
        let mut entries: Vec<(PathBuf, u64, std::time::SystemTime)> = Vec::new();
        let mut rd = match tokio::fs::read_dir(&cache_dir).await {
            Ok(rd) => rd,
            Err(_) => return,
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            if let Ok(meta) = entry.metadata().await {
                let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                entries.push((entry.path(), meta.len(), mtime));
            }
        }
        let total: u64 = entries.iter().map(|(_, sz, _)| *sz).sum();
        if total <= MAX_BYTES {
            return;
        }
        entries.sort_by_key(|(_, _, mtime)| *mtime);
        let mut remaining = total;
        for (path, sz, _) in entries {
            if remaining <= MAX_BYTES {
                break;
            }
            if tokio::fs::remove_file(&path).await.is_ok() {
                remaining = remaining.saturating_sub(sz);
            } else {
                warn!(path = %path.display(), "conditioning cache eviction failed");
            }
        }
    }

    /// Idle cleanup: unload Flux from VRAM.
    pub async fn on_idle(&self) {
        if let Err(e) = self.sidecar.unload_models().await {
            warn!(error = %e, "ImageOrchestrator: idle unload failed");
        }
    }

    /// Shut down sidecar cleanly.
    pub async fn shutdown(&self) {
        self.sidecar.stop().await;
    }
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Sha256, Digest};
    let mut h = Sha256::new();
    h.update(data);
    format!("{:x}", h.finalize())
}
