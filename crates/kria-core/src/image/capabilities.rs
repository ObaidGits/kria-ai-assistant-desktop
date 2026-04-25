//! Tier-aware capability resolution for image generation.
//!
//! **Single source of truth** for `(QualityProfile, ImageTier) → ResolvedWorkflow`.
//! All code that needs sampler / steps / model selection must call `resolve()` —
//! no conditional logic lives anywhere else.
//!
//! # Invariants (verified by unit tests)
//! * Flux-schnell paths: `cfg == 1.0` (distilled consistency model).
//! * Negative prompts: only populated for SDXL — caller ignores for Schnell.
//! * Hires-fix: **always false** below Tier S.
//! * SDXL Lightning: used only for `Photorealistic` style on GPU tiers when
//!   checkpoint is available.
//! * High on Tier B → silent downgrade to Balanced.

use serde::{Deserialize, Serialize};
use crate::image::styles::ImageStyle;
use crate::platform::vram::ImageTier;

// ─── Quality profile ──────────────────────────────────────────────────────────

/// User-visible quality preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum QualityProfile {
    /// 4-step Schnell native — current legacy behaviour. Fastest.
    Fast,
    /// Improved sampler / step count (tier-dependent). **Default.**
    #[default]
    Balanced,
    /// Maximum quality. SDXL Juggernaut XL Lightning on Tier S (model must be
    /// present); best-effort Schnell on Tier A/B (same as Balanced on Tier B).
    High,
}

impl QualityProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Balanced => "balanced",
            Self::High => "high",
        }
    }
}

impl std::fmt::Display for QualityProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for QualityProfile {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "fast" => Self::Fast,
            "high" | "hq" | "best" => Self::High,
            _ => Self::Balanced,
        })
    }
}

// ─── Resolved workflow ────────────────────────────────────────────────────────

/// Fully resolved generation parameters for one job.
///
/// This is the *only* struct that `build_workflow` / cloud dispatch read.
/// No conditional logic is needed downstream.
#[derive(Debug, Clone)]
pub struct ResolvedWorkflow {
    /// GGUF unet filename (`comfy_models_dir/unet/`) or SDXL checkpoint filename
    /// (`comfy_models_dir/checkpoints/`).
    pub model_file: &'static str,
    /// ComfyUI sampler name.
    pub sampler: &'static str,
    /// ComfyUI scheduler name.
    pub scheduler: &'static str,
    /// Denoising steps.
    pub steps: u32,
    /// CFG scale. **Invariant: 1.0 for Flux-schnell.**
    pub cfg: f32,
    /// True only for the SDXL checkpoint workflow (Tier S + model present).
    pub use_sdxl: bool,
    /// Whether to emit a hires-fix latent upscale + refine node.
    /// **Invariant: false below Tier S.**
    pub use_hires_fix: bool,
    /// Denoise for the hires-fix refine pass.
    pub hires_denoise: f32,
    /// The profile actually applied (may differ from requested after downgrade).
    pub effective_profile: QualityProfile,
}

// ─── Resolver ─────────────────────────────────────────────────────────────────

/// Resolve `(profile, tier, sdxl_available)` into a [`ResolvedWorkflow`].
///
/// Never panics. Silently downgrades if the tier cannot satisfy the profile.
pub fn resolve(
    profile: QualityProfile,
    tier: ImageTier,
    style: ImageStyle,
    sdxl_available: bool,
) -> ResolvedWorkflow {
    // Dual-engine router: SDXL Lightning for photorealistic requests on any
    // local GPU tier (S/A/B) when the checkpoint is present.
    if sdxl_available
        && matches!(style, ImageStyle::Photorealistic)
        && !matches!(tier, ImageTier::CRejectOrCloud)
    {
        return ResolvedWorkflow {
            model_file: "juggernautXL_v9Lightning.safetensors",
            sampler: "euler",
            scheduler: "sgm_uniform",
            steps: 6,
            cfg: 2.0,
            use_sdxl: true,
            use_hires_fix: false,
            hires_denoise: 0.0,
            effective_profile: profile,
        };
    }

    match tier {
        // ── Tier S (≥14 GB) ─────────────────────────────────────────────────
        ImageTier::SHighRes => match profile {
            QualityProfile::High => ResolvedWorkflow {
                // Non-photorealistic styles remain on Flux.
                model_file: "flux1-schnell-fp8.safetensors",
                sampler: "euler",
                scheduler: "simple",
                steps: 8,
                cfg: 1.0, // Schnell: cfg stays 1.0
                use_sdxl: false,
                use_hires_fix: false,
                hires_denoise: 0.0,
                effective_profile: QualityProfile::High,
            },
            QualityProfile::Balanced => ResolvedWorkflow {
                model_file: "flux1-schnell-fp8.safetensors",
                sampler: "euler",
                scheduler: "simple",
                steps: 8,
                cfg: 1.0,
                use_sdxl: false,
                use_hires_fix: false,
                hires_denoise: 0.0,
                effective_profile: QualityProfile::Balanced,
            },
            QualityProfile::Fast => ResolvedWorkflow {
                model_file: "flux1-schnell-fp8.safetensors",
                sampler: "euler",
                scheduler: "simple",
                steps: 4,
                cfg: 1.0,
                use_sdxl: false,
                use_hires_fix: false,
                hires_denoise: 0.0,
                effective_profile: QualityProfile::Fast,
            },
        },

        // ── Tier A (10–14 GB) ────────────────────────────────────────────────
        ImageTier::AStandard => match profile {
            QualityProfile::High => ResolvedWorkflow {
                model_file: "flux1-schnell-Q4_K_S.gguf",
                sampler: "dpmpp_2m",
                scheduler: "sgm_uniform",
                steps: 8,
                cfg: 1.0, // Schnell: cfg must stay 1.0
                use_sdxl: false,
                use_hires_fix: false, // hard-banned below Tier S
                hires_denoise: 0.0,
                effective_profile: QualityProfile::High,
            },
            QualityProfile::Balanced => ResolvedWorkflow {
                model_file: "flux1-schnell-Q4_K_S.gguf",
                sampler: "dpmpp_2m",
                scheduler: "sgm_uniform",
                steps: 6,
                cfg: 1.0,
                use_sdxl: false,
                use_hires_fix: false,
                hires_denoise: 0.0,
                effective_profile: QualityProfile::Balanced,
            },
            QualityProfile::Fast => ResolvedWorkflow {
                model_file: "flux1-schnell-Q4_K_S.gguf",
                sampler: "euler",
                scheduler: "simple",
                steps: 4,
                cfg: 1.0,
                use_sdxl: false,
                use_hires_fix: false,
                hires_denoise: 0.0,
                effective_profile: QualityProfile::Fast,
            },
        },

        // ── Tier B (4–10 GB, drop-and-swap) ─────────────────────────────────
        // High → silent Balanced: SDXL won't fit; Schnell cfg 2.0 illegal.
        // hires-fix hard-banned at this tier.
        ImageTier::BDropSwap => match profile {
            QualityProfile::High | QualityProfile::Balanced => ResolvedWorkflow {
                model_file: "flux1-schnell-Q4_K_S.gguf",
                sampler: "euler",
                scheduler: "simple",
                steps: 8,
                cfg: 1.0,
                use_sdxl: false,
                use_hires_fix: false,
                hires_denoise: 0.0,
                effective_profile: QualityProfile::Balanced,
            },
            QualityProfile::Fast => ResolvedWorkflow {
                model_file: "flux1-schnell-Q4_K_S.gguf",
                sampler: "euler",
                scheduler: "simple",
                steps: 4,
                cfg: 1.0,
                use_sdxl: false,
                use_hires_fix: false,
                hires_denoise: 0.0,
                effective_profile: QualityProfile::Fast,
            },
        },

        // ── Tier C — always cloud; local workflow params unused ──────────────
        ImageTier::CRejectOrCloud => ResolvedWorkflow {
            model_file: "flux1-schnell-Q4_K_S.gguf",
            sampler: "euler",
            scheduler: "simple",
            steps: 4,
            cfg: 1.0,
            use_sdxl: false,
            use_hires_fix: false,
            hires_denoise: 0.0,
            effective_profile: QualityProfile::Fast,
        },
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::styles::ImageStyle;
    use crate::platform::vram::ImageTier;

    const ALL_PROFILES: [QualityProfile; 3] = [
        QualityProfile::Fast,
        QualityProfile::Balanced,
        QualityProfile::High,
    ];

    const ALL_TIERS: [ImageTier; 4] = [
        ImageTier::SHighRes,
        ImageTier::AStandard,
        ImageTier::BDropSwap,
        ImageTier::CRejectOrCloud,
    ];

    const ALL_STYLES: [ImageStyle; 5] = [
        ImageStyle::Photorealistic,
        ImageStyle::Anime,
        ImageStyle::Cartoon,
        ImageStyle::LineArt,
        ImageStyle::TextHeavy,
    ];

    /// Schnell cfg must never deviate from 1.0 (distilled consistency model).
    #[test]
    fn schnell_cfg_invariant() {
        for &tier in &ALL_TIERS {
            for &profile in &ALL_PROFILES {
                for &style in &ALL_STYLES {
                    let r = resolve(profile, tier, style, false);
                    if !r.use_sdxl {
                        assert_eq!(
                            r.cfg, 1.0,
                            "Schnell cfg must be 1.0 for {profile:?}/{tier:?}/{style:?}"
                        );
                    }
                }
            }
        }
    }

    /// hires-fix must never be emitted below Tier S.
    #[test]
    fn hires_fix_banned_below_tier_s() {
        let non_s = [ImageTier::AStandard, ImageTier::BDropSwap, ImageTier::CRejectOrCloud];
        for &tier in &non_s {
            for &profile in &ALL_PROFILES {
                for &style in &ALL_STYLES {
                    let r = resolve(profile, tier, style, true);
                    assert!(
                        !r.use_hires_fix,
                        "hires_fix must be false below Tier S for {profile:?}/{tier:?}/{style:?}"
                    );
                }
            }
        }
    }

    /// Photorealistic requests should use SDXL Lightning on local GPU tiers.
    #[test]
    fn photorealistic_routes_to_sdxl_on_gpu_tiers() {
        for &tier in &[ImageTier::SHighRes, ImageTier::AStandard, ImageTier::BDropSwap] {
            let r = resolve(QualityProfile::Balanced, tier, ImageStyle::Photorealistic, true);
            assert!(r.use_sdxl, "photorealistic should route to SDXL on {tier:?}");
            assert_eq!(r.model_file, "juggernautXL_v9Lightning.safetensors");
            assert_eq!(r.sampler, "euler");
            assert_eq!(r.scheduler, "sgm_uniform");
            assert_eq!(r.steps, 6);
            assert_eq!(r.cfg, 2.0);
        }
    }

    /// Non-photorealistic styles stay on Flux, even when SDXL is available.
    #[test]
    fn non_photorealistic_stays_flux() {
        let r = resolve(
            QualityProfile::High,
            ImageTier::BDropSwap,
            ImageStyle::Anime,
            true,
        );
        assert!(!r.use_sdxl);
        assert_eq!(r.cfg, 1.0);
        assert!(r.model_file.contains("flux1-schnell"));
    }

    /// High on Tier B must silently degrade to Balanced.
    #[test]
    fn high_degrades_to_balanced_on_tier_b() {
        let r = resolve(
            QualityProfile::High,
            ImageTier::BDropSwap,
            ImageStyle::Anime,
            true,
        );
        assert_eq!(r.effective_profile, QualityProfile::Balanced);
    }

    /// Resolver covers all combinations without panicking.
    #[test]
    fn resolve_all_combinations() {
        for &tier in &ALL_TIERS {
            for &profile in &ALL_PROFILES {
                for &style in &ALL_STYLES {
                    let _ = resolve(profile, tier, style, false);
                    let _ = resolve(profile, tier, style, true);
                }
            }
        }
    }
}
