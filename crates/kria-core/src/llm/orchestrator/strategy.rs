//! Layer strategy calculator — determines optimal (ngl, context, vision)
//! parameters given available VRAM and model profile.

use crate::config::ModelProfile;
use super::GpuBackend;

/// Degradation level representing how much the model is operating below
/// its optimal configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DegradationLevel {
    /// All layers on GPU, full context, vision enabled.
    Full,
    /// All layers on GPU but context is reduced.
    ReducedContext,
    /// Some layers offloaded to CPU.
    PartialOffload,
    /// Heavy CPU offload, reduced context.
    HeavyOffload,
    /// Full CPU inference (ngl=0).
    CpuOnly,
}

impl DegradationLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::ReducedContext => "reduced_context",
            Self::PartialOffload => "partial_offload",
            Self::HeavyOffload => "heavy_offload",
            Self::CpuOnly => "cpu_only",
        }
    }
}

impl std::fmt::Display for DegradationLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Calculated target parameters for a llama-server spawn.
#[derive(Debug, Clone)]
pub struct TargetParams {
    /// Number of GPU layers to offload.
    pub ngl: u32,
    /// Context window size in tokens.
    pub context: u32,
    /// Whether to enable vision (load mmproj).
    pub enable_vision: bool,
    /// Current degradation level.
    pub degradation: DegradationLevel,
}

/// Calculate optimal llama-server parameters given available VRAM.
///
/// Algorithm:
/// 1. Reserve `safety_margin_mb` from available VRAM
/// 2. Subtract base overhead (CUDA context + embeddings)
/// 3. Maximize ngl from remaining budget
/// 4. Allocate remaining VRAM to context window
/// 5. Disable vision below ngl=15 (insufficient GPU capacity)
pub fn calculate_target_params(
    profile: &ModelProfile,
    free_vram_mb: u64,
    safety_margin_mb: u64,
    backend: GpuBackend,
) -> TargetParams {
    // macOS Metal: all layers are always offloaded (unified memory).
    // Only context is adjusted based on free RAM.
    if backend == GpuBackend::Metal {
        return calculate_metal_params(profile, free_vram_mb);
    }

    // CPU-only: no GPU layers, max context, no vision
    if backend == GpuBackend::CpuOnly {
        return TargetParams {
            ngl: 0,
            context: profile.max_context,
            enable_vision: false,
            degradation: DegradationLevel::CpuOnly,
        };
    }

    // CUDA path: VRAM budget calculation
    let available = free_vram_mb.saturating_sub(safety_margin_mb);

    // Reserve VRAM for vision projector (mmproj) when present
    let mmproj_cost = if profile.has_vision_projector {
        profile.mmproj_vram_mb as u64
    } else {
        0
    };

    // Not enough for even base overhead + mmproj → CPU only
    if available < profile.base_vram_overhead_mb as u64 + mmproj_cost {
        return TargetParams {
            ngl: 0,
            context: profile.min_context,
            enable_vision: false,
            degradation: DegradationLevel::CpuOnly,
        };
    }

    let budget_after_base = available - profile.base_vram_overhead_mb as u64 - mmproj_cost;

    // Calculate max layers that fit
    let max_layers_from_budget = if profile.per_layer_vram_mb > 0 {
        (budget_after_base / profile.per_layer_vram_mb as u64) as u32
    } else {
        profile.total_layers
    };
    let ngl = max_layers_from_budget.min(profile.total_layers);

    // Remaining VRAM after layers → context
    let vram_used_by_layers = ngl as u64 * profile.per_layer_vram_mb as u64;
    let remaining_for_ctx = budget_after_base.saturating_sub(vram_used_by_layers);

    let context = if profile.kv_per_1k_ctx_mb > 0 {
        let ctx_from_vram = ((remaining_for_ctx * 1024) / profile.kv_per_1k_ctx_mb as u64) as u32;
        ctx_from_vram
            .max(profile.min_context)
            .min(profile.max_context)
    } else {
        profile.max_context
    };

    // Vision requires enough GPU capacity — disable below ngl=15
    let enable_vision = profile.has_vision_projector && ngl >= profile.vision_min_ngl;

    let degradation = degradation_level(ngl, context, profile);

    TargetParams {
        ngl,
        context,
        enable_vision,
        degradation,
    }
}

/// Calculate parameters for Apple Silicon (Metal backend).
/// All layers are always on GPU (unified memory), context adapts to free RAM.
fn calculate_metal_params(profile: &ModelProfile, free_ram_mb: u64) -> TargetParams {
    let ngl = profile.total_layers; // Always full offload on Metal

    // Context scales with available RAM
    let context = if profile.kv_per_1k_ctx_mb > 0 {
        // Reserve ~2GB for system use
        let usable = free_ram_mb.saturating_sub(2048);
        let ctx = ((usable * 1024) / profile.kv_per_1k_ctx_mb as u64) as u32;
        ctx.max(profile.min_context).min(profile.max_context)
    } else {
        profile.max_context
    };

    let enable_vision = profile.has_vision_projector;
    let degradation = if context >= profile.max_context {
        DegradationLevel::Full
    } else {
        DegradationLevel::ReducedContext
    };

    TargetParams {
        ngl,
        context,
        enable_vision,
        degradation,
    }
}

/// Determine the degradation level from current ngl and context.
pub fn degradation_level(ngl: u32, context: u32, profile: &ModelProfile) -> DegradationLevel {
    if ngl == 0 {
        DegradationLevel::CpuOnly
    } else if ngl < profile.total_layers / 2 {
        DegradationLevel::HeavyOffload
    } else if ngl < profile.total_layers {
        if context < profile.max_context / 2 {
            DegradationLevel::HeavyOffload
        } else {
            DegradationLevel::PartialOffload
        }
    } else if context < profile.max_context {
        DegradationLevel::ReducedContext
    } else {
        DegradationLevel::Full
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_profile() -> ModelProfile {
        ModelProfile {
            total_layers: 35,
            per_layer_vram_mb: 128,
            base_vram_overhead_mb: 200,
            kv_per_1k_ctx_mb: 100,
            min_context: 2048,
            max_context: 8192,
            has_vision_projector: true,
            mmproj_vram_mb: 0,
            vision_min_ngl: 15,
        }
    }

    #[test]
    fn full_vram_gives_full_params() {
        let p = test_profile();
        // 6GB free = 6144 MB
        let result = calculate_target_params(&p, 6144, 256, GpuBackend::Cuda);
        assert_eq!(result.ngl, 35); // All layers fit: (6144-256-200)/128 = 44 > 35
        assert!(result.context >= p.min_context);
        assert!(result.enable_vision);
        assert_eq!(result.degradation, DegradationLevel::Full);
    }

    #[test]
    fn low_vram_forces_cpu_only() {
        let p = test_profile();
        let result = calculate_target_params(&p, 300, 256, GpuBackend::Cuda);
        assert_eq!(result.ngl, 0);
        assert_eq!(result.context, p.min_context);
        assert!(!result.enable_vision);
        assert_eq!(result.degradation, DegradationLevel::CpuOnly);
    }

    #[test]
    fn moderate_vram_gives_partial_offload() {
        let p = test_profile();
        // 3GB = 3072 MB. Budget = 3072-256-200 = 2616. Layers = 2616/128 = 20
        let result = calculate_target_params(&p, 3072, 256, GpuBackend::Cuda);
        assert!(result.ngl > 0 && result.ngl < 35);
        assert!(result.ngl >= 15); // Vision should be on
        assert!(result.enable_vision);
    }

    #[test]
    fn vision_disabled_below_ngl_15() {
        let p = test_profile();
        // ~2GB = 2048 MB. Budget = 2048-256-200 = 1592. Layers = 1592/128 = 12
        let result = calculate_target_params(&p, 2048, 256, GpuBackend::Cuda);
        assert!(result.ngl < 15);
        assert!(!result.enable_vision);
    }

    #[test]
    fn metal_always_full_layers() {
        let p = test_profile();
        let result = calculate_target_params(&p, 4096, 256, GpuBackend::Metal);
        assert_eq!(result.ngl, p.total_layers);
        assert!(result.enable_vision);
    }

    #[test]
    fn cpu_only_backend() {
        let p = test_profile();
        let result = calculate_target_params(&p, 8192, 256, GpuBackend::CpuOnly);
        assert_eq!(result.ngl, 0);
        assert!(!result.enable_vision);
        assert_eq!(result.degradation, DegradationLevel::CpuOnly);
    }

    #[test]
    fn context_floor_enforced() {
        let p = test_profile();
        // Very low VRAM — context should still be >= min_context
        let result = calculate_target_params(&p, 512, 256, GpuBackend::Cuda);
        assert!(result.context >= p.min_context);
    }

    #[test]
    fn mmproj_vram_reduces_available_layers() {
        let mut p = test_profile();
        p.mmproj_vram_mb = 1300;
        // 6GB free = 6144 MB.
        // Without mmproj: budget = 6144-256-200 = 5688. Layers = 5688/128 = 44 → capped at 35
        // With mmproj:    budget = 6144-256-200-1300 = 4388. Layers = 4388/128 = 34
        let result = calculate_target_params(&p, 6144, 256, GpuBackend::Cuda);
        assert_eq!(result.ngl, 34);
        assert!(result.enable_vision);
    }
}
