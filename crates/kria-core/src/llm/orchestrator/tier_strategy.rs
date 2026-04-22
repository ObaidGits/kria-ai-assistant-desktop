//! Adaptive model + profile selection for the hardware orchestrator.
//!
//! Bridges [`HardwareTier`](crate::platform::detect::HardwareTier) with the
//! list of `[[llm.models]]` entries from `config.toml`, producing:
//!
//! * the **best-fitting model** for the detected hardware (or the user's
//!   explicit override), and
//! * a **derived [`ModelProfile`]** sized to that model's metadata
//!   (`vram_estimate_gb`, `context_window`, vision capability …).
//!
//! Used by `init_runtime` in `kria-desktop` to:
//!   1. avoid loading a 4.7 GB Qwen2.5-VL on a 6 GB-RAM laptop, and
//!   2. avoid the orchestrator silently disabling itself when the
//!      `active_model` doesn't exist on disk.
//!
//! Selection rules (in priority order):
//!   1. **Explicit override** — if `config.llm.active_model` matches a
//!      model in `models[]` *and* the file exists on disk, honour it.
//!   2. **Tier match** — pick the largest model whose
//!      `vram_estimate_gb` fits the tier's effective memory budget.
//!   3. **Smallest available** — fall back to the smallest known model.
//!
//! The selector is **pure** — it never touches the filesystem itself; the
//! caller passes a `model_exists` closure so this module can be unit-tested
//! without needing real GGUF files on disk.

use crate::config::{LocalModelDef, ModelProfile};
use crate::platform::detect::HardwareTier;

/// Effective memory budget (MiB) for a given tier when picking models.
///
/// On CPU-only tiers we use ~60 % of system RAM as the budget so the model
/// plus KV cache + OS headroom fits comfortably.
/// On GPU tiers we use ~85 % of VRAM (CUDA driver and other apps need slack).
pub fn tier_memory_budget_mb(tier: HardwareTier, ram_mb: u64, vram_mb: Option<u64>) -> u64 {
    match (tier, vram_mb) {
        (HardwareTier::Performance | HardwareTier::High, Some(v)) if v > 0 => {
            (v as f64 * 0.85) as u64
        }
        // No GPU or pure-CPU tier: budget against RAM.
        _ => (ram_mb as f64 * 0.60) as u64,
    }
}

/// Result of a tier-aware model selection.
#[derive(Debug, Clone)]
pub struct TierModelChoice {
    /// The chosen model (clone of an entry in `config.llm.models`).
    pub model: LocalModelDef,
    /// Why this model was picked — for logging & UI surfacing.
    pub reason: SelectionReason,
    /// Whether vision was disabled because this tier can't support it.
    pub vision_disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionReason {
    /// User's `active_model` matched and the file exists.
    UserOverride,
    /// Largest model whose VRAM estimate fits the tier budget.
    TierFit { budget_mb: u64 },
    /// Nothing fit — picked the smallest model as a last resort.
    SmallestFallback,
    /// No models defined in config; caller must handle.
    NoModels,
}

/// Pick the best model for the current hardware tier.
///
/// `model_exists(file)` should return `true` if the GGUF file is present on
/// disk under any of the search paths the orchestrator knows about.
pub fn select_model_for_tier<F>(
    tier: HardwareTier,
    ram_mb: u64,
    vram_mb: Option<u64>,
    active_model_name: &str,
    models: &[LocalModelDef],
    model_exists: F,
) -> Option<TierModelChoice>
where
    F: Fn(&str) -> bool,
{
    if models.is_empty() {
        return None;
    }

    // 1. Explicit override.
    if !active_model_name.is_empty() {
        if let Some(m) = models
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case(active_model_name))
        {
            if model_exists(&m.file) {
                return Some(TierModelChoice {
                    model: m.clone(),
                    reason: SelectionReason::UserOverride,
                    vision_disabled: !tier.has_vision()
                        && m.capabilities.iter().any(|c| c == "vision"),
                });
            }
        }
    }

    let budget_mb = tier_memory_budget_mb(tier, ram_mb, vram_mb);

    // 2. Filter to models that exist on disk and fit the budget.
    //    Vision models are deprioritised on tiers without vision.
    let mut candidates: Vec<&LocalModelDef> = models
        .iter()
        .filter(|m| model_exists(&m.file))
        .filter(|m| {
            let estimate_mb = (m.vram_estimate_gb as f64 * 1024.0) as u64;
            estimate_mb <= budget_mb
        })
        .collect();

    // Prefer non-vision on lite/standard tiers (saves the mmproj VRAM).
    if !tier.has_vision() {
        candidates.sort_by(|a, b| {
            let a_vis = a.capabilities.iter().any(|c| c == "vision");
            let b_vis = b.capabilities.iter().any(|c| c == "vision");
            // Non-vision first, then by descending VRAM estimate.
            a_vis.cmp(&b_vis).then(
                b.vram_estimate_gb
                    .partial_cmp(&a.vram_estimate_gb)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
        });
    } else {
        // Tiers with vision: prefer the largest fitting model overall.
        candidates.sort_by(|a, b| {
            b.vram_estimate_gb
                .partial_cmp(&a.vram_estimate_gb)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    if let Some(best) = candidates.first() {
        return Some(TierModelChoice {
            model: (*best).clone(),
            reason: SelectionReason::TierFit { budget_mb },
            vision_disabled: !tier.has_vision()
                && best.capabilities.iter().any(|c| c == "vision"),
        });
    }

    // 3. Last-resort fallback: smallest model that exists on disk, regardless of fit.
    let mut existing: Vec<&LocalModelDef> =
        models.iter().filter(|m| model_exists(&m.file)).collect();
    existing.sort_by(|a, b| {
        a.vram_estimate_gb
            .partial_cmp(&b.vram_estimate_gb)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    if let Some(smallest) = existing.first() {
        return Some(TierModelChoice {
            model: (*smallest).clone(),
            reason: SelectionReason::SmallestFallback,
            vision_disabled: !tier.has_vision()
                && smallest.capabilities.iter().any(|c| c == "vision"),
        });
    }

    // No model file exists on disk at all. Return the configured choice
    // so the caller can produce an actionable error (download missing files).
    let fallback = models.first().cloned().unwrap();
    Some(TierModelChoice {
        model: fallback,
        reason: SelectionReason::NoModels,
        vision_disabled: !tier.has_vision(),
    })
}

/// Derive a [`ModelProfile`] for a given `LocalModelDef`.
///
/// Uses the model's declared `vram_estimate_gb` as the basis and reasonable
/// per-architecture defaults. Falls back to `base_profile` for any field that
/// can't be inferred from the model definition.
///
/// This lets each entry in `[[llm.models]]` work without a hand-tuned
/// `[orchestrator.model_profile]` block.
pub fn derive_model_profile(model: &LocalModelDef, base_profile: &ModelProfile) -> ModelProfile {
    let has_vision = model.capabilities.iter().any(|c| c == "vision");
    let mmproj = if model.mmproj_file.is_some() { 1300 } else { 0 };

    // Heuristic: layer count from model name. Defaults match common GGUFs.
    let total_layers = layer_count_for(&model.name).unwrap_or(base_profile.total_layers);

    // Per-layer VRAM: estimate from total estimate minus base + mmproj.
    let total_mb = (model.vram_estimate_gb as f64 * 1024.0) as u32;
    let per_layer_vram_mb = total_mb
        .saturating_sub(base_profile.base_vram_overhead_mb)
        .saturating_sub(mmproj)
        .checked_div(total_layers)
        .unwrap_or(base_profile.per_layer_vram_mb)
        .max(40); // hard floor — never report 0

    let max_context = model.context_window.max(2048) as u32;
    let min_context = base_profile.min_context.min(max_context);

    ModelProfile {
        total_layers,
        per_layer_vram_mb,
        base_vram_overhead_mb: base_profile.base_vram_overhead_mb,
        kv_per_1k_ctx_mb: base_profile.kv_per_1k_ctx_mb,
        min_context,
        max_context,
        has_vision_projector: has_vision && model.mmproj_file.is_some(),
        vision_min_ngl: base_profile.vision_min_ngl,
        mmproj_vram_mb: if has_vision && model.mmproj_file.is_some() {
            mmproj
        } else {
            0
        },
    }
}

/// Best-effort layer-count lookup keyed on substrings of the model name.
fn layer_count_for(name: &str) -> Option<u32> {
    let n = name.to_ascii_lowercase();
    if n.contains("qwen2.5-vl-7b") || n.contains("qwen2.5-7b") || n.contains("qwen3-7b") {
        Some(28)
    } else if n.contains("qwen2.5-3b") || n.contains("qwen3-3b") {
        Some(36)
    } else if n.contains("qwen3-8b") {
        Some(36)
    } else if n.contains("qwen3-0.6b") {
        Some(28)
    } else if n.contains("phi-4-mini") {
        Some(32)
    } else {
        None
    }
}

// ─────────────────────────── Tests ───────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_model(name: &str, file: &str, vram_gb: f32, vision: bool, ctx: usize) -> LocalModelDef {
        LocalModelDef {
            name: name.into(),
            file: file.into(),
            display_name: name.into(),
            context_window: ctx,
            max_tokens: 2048,
            vram_estimate_gb: vram_gb,
            capabilities: if vision {
                vec!["chat".into(), "vision".into()]
            } else {
                vec!["chat".into()]
            },
            mmproj_file: if vision {
                Some("mmproj-F16.gguf".into())
            } else {
                None
            },
        }
    }

    #[test]
    fn high_tier_picks_largest_fitting_model() {
        let models = vec![
            mk_model("qwen3-0.6b", "q06.gguf", 0.6, false, 4096),
            mk_model("phi-4-mini", "phi.gguf", 2.5, false, 4096),
            mk_model("qwen2.5-vl-7b", "qwen.gguf", 6.0, true, 8192),
        ];
        let choice = select_model_for_tier(
            HardwareTier::High,
            32 * 1024,
            Some(8192),
            "",
            &models,
            |_| true,
        )
        .unwrap();
        assert_eq!(choice.model.name, "qwen2.5-vl-7b");
        assert!(!choice.vision_disabled);
    }

    #[test]
    fn lite_tier_avoids_vision_model() {
        let models = vec![
            mk_model("qwen3-0.6b", "q06.gguf", 0.6, false, 4096),
            mk_model("qwen2.5-vl-7b", "qwen.gguf", 6.0, true, 8192),
        ];
        let choice = select_model_for_tier(
            HardwareTier::Lite,
            4 * 1024,
            None,
            "",
            &models,
            |_| true,
        )
        .unwrap();
        assert_eq!(choice.model.name, "qwen3-0.6b");
    }

    #[test]
    fn user_override_honoured_when_file_exists() {
        let models = vec![
            mk_model("qwen3-0.6b", "q06.gguf", 0.6, false, 4096),
            mk_model("qwen2.5-vl-7b", "qwen.gguf", 6.0, true, 8192),
        ];
        let choice = select_model_for_tier(
            HardwareTier::Performance,
            16 * 1024,
            Some(6000),
            "qwen3-0.6b",
            &models,
            |_| true,
        )
        .unwrap();
        assert_eq!(choice.model.name, "qwen3-0.6b");
        assert!(matches!(choice.reason, SelectionReason::UserOverride));
    }

    #[test]
    fn user_override_ignored_when_file_missing() {
        let models = vec![
            mk_model("qwen3-0.6b", "q06.gguf", 0.6, false, 4096),
            mk_model("phi-4-mini", "phi.gguf", 2.5, false, 4096),
        ];
        let choice = select_model_for_tier(
            HardwareTier::Standard,
            8 * 1024,
            None,
            "qwen3-0.6b",
            &models,
            |f| f != "q06.gguf", // override file missing
        )
        .unwrap();
        assert_ne!(choice.model.name, "qwen3-0.6b");
    }

    #[test]
    fn no_fitting_model_falls_back_to_smallest_existing() {
        let models = vec![
            mk_model("qwen2.5-vl-7b", "qwen.gguf", 6.0, true, 8192),
            mk_model("qwen3-0.6b", "q06.gguf", 2.0, false, 4096),
        ];
        // Lite tier with 2 GB RAM → 60% budget = 1228 MB. Both 6 GB and 2 GB
        // models exceed that, so neither fits → fallback to smallest existing.
        let choice = select_model_for_tier(
            HardwareTier::Lite,
            2 * 1024,
            None,
            "",
            &models,
            |_| true,
        )
        .unwrap();
        assert_eq!(choice.model.name, "qwen3-0.6b");
        assert!(matches!(choice.reason, SelectionReason::SmallestFallback));
    }

    #[test]
    fn returns_none_when_models_empty() {
        let choice =
            select_model_for_tier(HardwareTier::Standard, 8192, None, "", &[], |_| true);
        assert!(choice.is_none());
    }

    #[test]
    fn derive_profile_matches_vision_capability() {
        let m = mk_model("qwen2.5-vl-7b", "q.gguf", 6.0, true, 8192);
        let base = ModelProfile::default();
        let p = derive_model_profile(&m, &base);
        assert!(p.has_vision_projector);
        assert_eq!(p.mmproj_vram_mb, 1300);
        assert_eq!(p.max_context, 8192);
        assert!(p.per_layer_vram_mb > 50);
    }

    #[test]
    fn derive_profile_for_text_only_model_disables_vision() {
        let m = mk_model("phi-4-mini", "p.gguf", 2.5, false, 4096);
        let base = ModelProfile::default();
        let p = derive_model_profile(&m, &base);
        assert!(!p.has_vision_projector);
        assert_eq!(p.mmproj_vram_mb, 0);
    }
}
