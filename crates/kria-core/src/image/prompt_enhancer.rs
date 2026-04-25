//! Deterministic template-based prompt enhancement for image generation.
//!
//! The template enhancer appends curated, per-style boilerplate tokens to the
//! raw user prompt. It is:
//! * **Zero-latency** — pure Rust, no I/O, no network calls.
//! * **Idempotent** — skips any token already present in the prompt.
//! * **Tier-safe** — the only enhancer available on Tier B/C (no LLM swap cost).
//!
//! Negative prompts are only populated for the SDXL workflow path; Flux-schnell
//! is a distilled model and does not meaningfully respond to negative prompts.

use crate::image::capabilities::QualityProfile;
use crate::image::styles::ImageStyle;
use crate::platform::vram::ImageTier;

const PHOTOREALISTIC_CLINICAL_NEGATIVE: &str = "plastic, CGI, artificial, smooth skin, bad anatomy, deformed eyes, extra fingers, cartoon, illustration, low resolution, artifacts";

/// Output of the enhancement pass.
#[derive(Debug, Clone)]
pub struct EnhancedPrompt {
    /// Final positive prompt (raw + injected boilerplate).
    pub positive: String,
    /// Negative prompt — **empty for Schnell**; populated only for SDXL.
    pub negative: String,
    /// Style that was resolved for this job.
    pub used_style: ImageStyle,
    /// True if any boilerplate tokens were injected.
    pub was_enhanced: bool,
    /// Enhancement mode used ("template" or "passthrough").
    pub mode: &'static str,
}

// ─── Per-style boilerplate banks ─────────────────────────────────────────────

struct StyleBoilerplate {
    positive: &'static [&'static str],
    negative: &'static [&'static str],
}

fn boilerplate(style: ImageStyle) -> StyleBoilerplate {
    match style {
        ImageStyle::Photorealistic => StyleBoilerplate {
            positive: &[
                "shot on Sony A7IV",
                "50mm f/1.4",
                "soft natural light",
                "sharp focus",
                "professional color grading",
                "high detail",
                "8k resolution",
            ],
            negative: &[],
        },
        ImageStyle::Anime => StyleBoilerplate {
            positive: &[
                "vibrant anime illustration",
                "cel-shaded",
                "clean line art",
                "studio-quality lighting",
                "expressive composition",
                "highly detailed",
            ],
            negative: &[
                "realistic",
                "photo",
                "3d render",
                "blurry",
                "watermark",
                "low quality",
                "deformed",
            ],
        },
        ImageStyle::Cartoon => StyleBoilerplate {
            positive: &[
                "stylized cartoon",
                "bold outlines",
                "flat saturated colors",
                "expressive shapes",
                "clean vector art",
            ],
            negative: &[
                "photo-realistic",
                "grainy",
                "blurry",
                "watermark",
                "deformed proportions",
            ],
        },
        ImageStyle::LineArt => StyleBoilerplate {
            positive: &[
                "clean ink line art",
                "high contrast",
                "precise linework",
                "white background",
                "detailed sketch",
            ],
            negative: &["color", "shading", "blurry", "messy lines", "watermark"],
        },
        ImageStyle::TextHeavy => StyleBoilerplate {
            positive: &[
                "crisp legible typography",
                "balanced layout",
                "high-contrast lettering",
                "professional design",
            ],
            negative: &[
                "illegible text",
                "blurry",
                "watermark",
                "distorted letters",
            ],
        },
    }
}

// ─── Idempotency guard ────────────────────────────────────────────────────────

/// Returns `true` if `prompt_lower` already contains this boilerplate token.
/// Prevents stuttering when the user writes an already-detailed prompt.
fn already_present(prompt_lower: &str, token: &str) -> bool {
    // Take just the first significant word of multi-word tokens.
    // For short tokens (< 4 chars, e.g. "8k") require an exact match.
    let first_word = token
        .split_whitespace()
        .next()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if first_word.len() < 4 {
        return prompt_lower.contains(&token.to_ascii_lowercase());
    }
    prompt_lower.contains(&token.to_ascii_lowercase())
}

// ─── Public API ───────────────────────────────────────────────────────────────

/// Enhance a prompt using the tier-appropriate strategy.
///
/// * Tier B / C → [`enhance_template`] (deterministic, zero latency).
/// * Tier S / A → [`enhance_template`] for now (LLM path is future work; the
///   LLM-assisted variant will call this as fallback anyway).
///
/// `use_sdxl` gates whether a negative prompt is emitted.
pub fn enhance(
    raw_prompt: &str,
    style: ImageStyle,
    _quality: QualityProfile,
    _tier: ImageTier,
    use_sdxl: bool,
) -> EnhancedPrompt {
    enhance_template(raw_prompt, style, use_sdxl)
}

/// Pure template-based enhancer. Sub-millisecond. Deterministic.
pub fn enhance_template(raw_prompt: &str, style: ImageStyle, use_sdxl: bool) -> EnhancedPrompt {
    let bp = boilerplate(style);
    let prompt_lower = raw_prompt.to_ascii_lowercase();

    // Collect positive tokens not already in the prompt.
    let mut to_add: Vec<&'static str> = bp
        .positive
        .iter()
        .copied()
        .filter(|t| !already_present(&prompt_lower, t))
        .collect();

    let positive = if to_add.is_empty() {
        raw_prompt.to_string()
    } else {
        let base = raw_prompt.trim_end_matches([',', '.', ' ']);
        // Build up to 480 chars, trimming tokens from the tail if needed.
        let mut result = base.to_string();
        to_add.retain(|token| {
            let candidate = format!("{result}, {token}");
            if candidate.len() <= 480 {
                result = candidate;
                true
            } else {
                false
            }
        });
        result
    };

    // Negative prompt only for SDXL (Schnell ignores it by design).
    let negative = if use_sdxl {
        if matches!(style, ImageStyle::Photorealistic) {
            PHOTOREALISTIC_CLINICAL_NEGATIVE.to_string()
        } else {
            bp.negative.join(", ")
        }
    } else {
        String::new()
    };

    let was_enhanced = !to_add.is_empty() || (use_sdxl && !negative.is_empty());

    EnhancedPrompt {
        positive,
        negative,
        used_style: style,
        was_enhanced,
        mode: "template",
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enriches_weak_prompt() {
        let ep = enhance_template("a cat", ImageStyle::Photorealistic, false);
        assert!(ep.positive.contains("sharp focus"), "should inject boilerplate");
        assert!(ep.was_enhanced);
        assert_eq!(ep.mode, "template");
    }

    #[test]
    fn idempotent_on_already_detailed_prompt() {
        let ep = enhance_template(
            "a cat, shot on Sony A7IV, 50mm f/1.4, soft natural light, sharp focus, \
             professional color grading, high detail, 8k resolution",
            ImageStyle::Photorealistic,
            false,
        );
        // No duplication.
        let count = ep.positive.matches("shot on Sony A7IV").count();
        assert_eq!(count, 1);
    }

    #[test]
    fn negative_empty_for_schnell() {
        let ep = enhance_template("a landscape", ImageStyle::Photorealistic, false);
        assert!(ep.negative.is_empty(), "Schnell must have empty negative");
    }

    #[test]
    fn negative_populated_for_sdxl() {
        let ep = enhance_template("a landscape", ImageStyle::Photorealistic, true);
        assert!(!ep.negative.is_empty(), "SDXL must have populated negative");
    }

    #[test]
    fn photorealistic_uses_fixed_clinical_negative() {
        let ep = enhance_template("a portrait", ImageStyle::Photorealistic, true);
        assert_eq!(ep.negative, PHOTOREALISTIC_CLINICAL_NEGATIVE);
    }

    #[test]
    fn respects_480_char_budget() {
        let long_prompt = "a ".repeat(200);
        let ep = enhance_template(&long_prompt, ImageStyle::Photorealistic, false);
        assert!(ep.positive.len() <= 480);
    }

    #[test]
    fn all_styles_return_non_empty_positive() {
        for style in [
            ImageStyle::Photorealistic,
            ImageStyle::Anime,
            ImageStyle::Cartoon,
            ImageStyle::LineArt,
            ImageStyle::TextHeavy,
        ] {
            let ep = enhance_template("test", style, false);
            assert!(!ep.positive.is_empty());
        }
    }
}
