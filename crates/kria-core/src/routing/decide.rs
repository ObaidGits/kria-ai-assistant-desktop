//! Core routing decision algorithm.
//!
//! Consumes:
//! - Per-domain cosine similarities (from fastembed centroids)
//! - OOD calibration distribution
//! - Verb modality result (from verbs.rs)
//! - Segment count (from segment.rs)
//!
//! Produces a `RouteDecision` with no static numeric thresholds for
//! OOD or multi-intent detection.


use super::domain::Domain;
use super::ood::{self, OodContext};
use super::verbs::ModalityResult;
use crate::config::RoutingConfig;

/// Output of the routing decision algorithm.
#[derive(Debug, Clone)]
pub enum RouteDecision {
    /// Conversational / OOD — no tools; LLM answers freely.
    Conversation,
    /// Clearly maps to a single domain.
    SingleDomain(Domain),
    /// Two or more domains detected (multi-intent or multi-segment).
    MultiDomain(Vec<Domain>),
    /// Three or more domains tied with low margin — hand off to Planner.
    Ambiguous { top: Vec<Domain> },
}

/// Input to the routing decision function.
pub struct DecideInput<'a> {
    /// Sorted desc: (Domain, cosine_similarity).
    pub domain_sims: &'a [(Domain, f32)],
    /// OOD calibration top-1 distribution.
    pub ood_distribution: &'a [f32],
    /// Verb classifier result.
    pub modality: &'a ModalityResult,
    /// Segments produced by the segmenter.
    pub segments: &'a [String],
    /// Per-segment domain similarities: segment_idx → sorted (Domain, sim).
    pub segment_sims: &'a [Vec<(Domain, f32)>],
    /// Routing config thresholds.
    pub config: &'a RoutingConfig,
}

pub fn decide(input: &DecideInput<'_>) -> RouteDecision {
    let sims = input.domain_sims;
    if sims.is_empty() {
        return RouteDecision::Conversation;
    }

    // ── Step 1: OOD check ────────────────────────────────────────────────
    let sim_values: Vec<f32> = sims.iter().map(|(_, s)| *s).collect();
    let ood_ctx = OodContext {
        sims: &sim_values,
        ood_distribution: input.ood_distribution,
        z_threshold: input.config.ood_z_threshold,
        entropy_threshold: input.config.ood_entropy_threshold,
    };
    if ood::is_ood(&ood_ctx) {
        return RouteDecision::Conversation;
    }

    let (_s1, s2, margin) = ood::top2_and_margin(sims);

    // ── Step 2: Multi-segment hard signal ────────────────────────────────
    if input.segments.len() >= 2 && !input.segment_sims.is_empty() {
        let mut domains: Vec<Domain> = Vec::new();
        for seg_sims in input.segment_sims {
            if let Some((d, _)) = seg_sims.first() {
                if !domains.contains(d) {
                    domains.push(*d);
                }
            }
        }
        if domains.len() >= 2 {
            return RouteDecision::MultiDomain(domains);
        }
    }

    // ── Step 3: Soft multi-intent — margin too small AND multiple modalities
    let top1_domain = sims[0].0;
    let top2_domain = if sims.len() > 1 { Some(sims[1].0) } else { None };

    let has_multi_modality = input.modality.all.len() >= 2;
    let margin_is_small = margin < input.config.multi_intent_margin;

    if margin_is_small && has_multi_modality {
        // Both top domains pass a basic quality gate (s2 not near zero)
        if s2 > 0.15 {
            let mut domains = vec![top1_domain];
            if let Some(d2) = top2_domain {
                domains.push(d2);
            }
            return RouteDecision::MultiDomain(domains);
        }
    }

    // ── Step 4: Ambiguous (3+ domains with similar scores) ───────────────
    if margin < input.config.multi_intent_margin {
        let top_k: Vec<Domain> = sims
            .iter()
            .take(3)
            .filter(|(_, s)| *s > 0.15)
            .map(|(d, _)| *d)
            .collect();
        if top_k.len() >= 3 {
            return RouteDecision::Ambiguous { top: top_k };
        }
    }

    // ── Step 5: Clear single-domain ──────────────────────────────────────
    RouteDecision::SingleDomain(top1_domain)
}
