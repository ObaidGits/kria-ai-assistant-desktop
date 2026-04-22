//! OOD (out-of-distribution) detection.
//!
//! Determines whether a user prompt is conversational / off-domain rather than
//! a tool-directed command. Uses a *relative* statistical test calibrated
//! against a pre-embedded reference distribution of known OOD prompts.
//!
//! Decision requires ALL of:
//!   z-score < z_low   (similarity is low relative to the OOD baseline)
//!   entropy > h_high  (similarity vector is diffuse — no clear domain)
//!
//! This avoids both a fragile absolute threshold and the false-positive
//! that would arise from checking only one signal.

/// Context passed into `is_ood`.
pub struct OodContext<'a> {
    /// Similarity vector: one f32 per domain, in any order (sorted desc not required).
    pub sims: &'a [f32],
    /// Pre-computed top-1 sim values from the OOD calibration set.
    pub ood_distribution: &'a [f32],
    /// z-score below this → prompt similarity is unremarkably low.
    pub z_threshold: f32,
    /// Entropy above this → too diffuse to be domain-directed.
    pub entropy_threshold: f32,
}

/// Returns `true` when the prompt is considered OOD (conversational/off-domain).
pub fn is_ood(ctx: &OodContext<'_>) -> bool {
    if ctx.sims.is_empty() {
        return true; // no domains → treat as OOD
    }

    let top1 = ctx.sims.iter().cloned().fold(f32::NEG_INFINITY, f32::max);

    // ── z-score against OOD calibration ──────────────────────────────────
    let z = if ctx.ood_distribution.len() >= 2 {
        let (mean, std) = mean_std(ctx.ood_distribution);
        if std > 1e-6 { (top1 - mean) / std } else { 0.0 }
    } else {
        // No calibration data: skip z-test (only entropy matters).
        ctx.z_threshold + 1.0
    };

    // ── Entropy of softmax(sims, τ=0.1) ──────────────────────────────────
    let h = entropy_softmax(ctx.sims, 0.1);
    let max_h = (ctx.sims.len() as f32).ln(); // H_max for N uniform categories

    // OOD = low z-score AND high entropy relative to max
    let z_is_low = z < ctx.z_threshold;
    let h_is_high = h > ctx.entropy_threshold * max_h;

    z_is_low && h_is_high
}

/// Returns `(top1 sim, second sim, margin)` — useful for multi-intent check.
pub fn top2_and_margin(sims: &[(super::domain::Domain, f32)]) -> (f32, f32, f32) {
    if sims.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    let s1 = sims[0].1;
    let s2 = if sims.len() > 1 { sims[1].1 } else { 0.0 };
    (s1, s2, s1 - s2)
}

// ─── Math helpers ─────────────────────────────────────────────────────────────

fn mean_std(v: &[f32]) -> (f32, f32) {
    let n = v.len() as f32;
    let mean = v.iter().sum::<f32>() / n;
    let var = v.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / n;
    (mean, var.sqrt())
}

fn entropy_softmax(sims: &[f32], tau: f32) -> f32 {
    let scaled: Vec<f32> = sims.iter().map(|x| x / tau).collect();
    let max_val = scaled.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let exps: Vec<f32> = scaled.iter().map(|x| (x - max_val).exp()).collect();
    let sum_exp: f32 = exps.iter().sum();
    exps.iter()
        .map(|e| {
            let p = e / sum_exp;
            if p > 1e-9 { -p * p.ln() } else { 0.0 }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::domain::Domain;

    fn fake_ood_dist() -> Vec<f32> {
        // Simulate calibration: OOD prompts get top-1 sim ≈ 0.30
        vec![0.28, 0.31, 0.29, 0.32, 0.27, 0.33, 0.30, 0.29]
    }

    #[test]
    fn ood_prompt_detected() {
        // Very low, very diffuse similarities
        let sims = [0.18, 0.17, 0.19, 0.16, 0.18, 0.17, 0.16, 0.15];
        let ctx = OodContext {
            sims: &sims,
            ood_distribution: &fake_ood_dist(),
            z_threshold: 0.5,
            entropy_threshold: 0.85,
        };
        assert!(is_ood(&ctx), "expect OOD prompt to be detected");
    }

    #[test]
    fn tool_prompt_not_ood() {
        // Strong similarity to one domain (FileOps), low elsewhere
        let sims = [0.82, 0.21, 0.19, 0.18, 0.17, 0.16, 0.15, 0.14];
        let ctx = OodContext {
            sims: &sims,
            ood_distribution: &fake_ood_dist(),
            z_threshold: 0.5,
            entropy_threshold: 0.85,
        };
        assert!(!is_ood(&ctx), "expect tool prompt NOT to be OOD");
    }
}
