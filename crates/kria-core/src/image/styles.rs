//! Image style presets, LoRA catalog, and automatic style classification.
//!
//! Flux.1-schnell is the foundation model. Each style maps to a LoRA file
//! (or none for TextHeavy, where Flux's native text rendering is used directly).

use serde::{Deserialize, Serialize};

/// Supported image styles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageStyle {
    /// Photorealistic photos, portraits, product shots.
    Photorealistic,
    /// Anime / manga / cel-shaded illustration.
    Anime,
    /// Western cartoon, flat colour, toon-shaded.
    Cartoon,
    /// Clean line art, sketch, technical illustration.
    LineArt,
    /// Posters, infographics, typography-heavy compositions.
    /// Uses **no LoRA** — Flux's native text rendering is superior.
    TextHeavy,
}

impl ImageStyle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Photorealistic => "photorealistic",
            Self::Anime => "anime",
            Self::Cartoon => "cartoon",
            Self::LineArt => "line_art",
            Self::TextHeavy => "text_heavy",
        }
    }

    /// LoRA filename on disk under `comfy_models_dir/loras/`, or `None` for no LoRA.
    pub fn lora_filename(self) -> Option<&'static str> {
        match self {
            Self::Photorealistic => Some("realism-flux-v2.safetensors"),
            Self::Anime => Some("anime-flux-v3.safetensors"),
            Self::Cartoon => Some("cartoon-toon-flux.safetensors"),
            Self::LineArt => Some("lineart-flux-v1.safetensors"),
            Self::TextHeavy => None,
        }
    }

    /// ComfyUI workflow template filename (under the embedded `assets/comfy_workflows/` dir).
    pub fn workflow_template(self) -> &'static str {
        match self {
            Self::TextHeavy => "text_heavy.json",
            _ => "standard_lora.json",
        }
    }

    /// Suggested step count for Flux-schnell (4-step model; override per style).
    pub fn steps(self) -> u32 {
        match self {
            Self::TextHeavy => 4,
            _ => 4,
        }
    }
}

impl std::str::FromStr for ImageStyle {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "anime" | "manga" | "cel" => Self::Anime,
            "cartoon" | "toon" | "illustrated" => Self::Cartoon,
            "lineart" | "line_art" | "sketch" | "drawing" => Self::LineArt,
            "text" | "text_heavy" | "typography" | "poster" | "infographic" => Self::TextHeavy,
            _ => Self::Photorealistic,
        })
    }
}

// ─── Style auto-classifier ────────────────────────────────────────────────────

/// Classify a prompt into an `ImageStyle` using keyword heuristics.
///
/// Returns `None` if the prompt is ambiguous and caller should either use a
/// default or invoke the LLM classifier.
pub fn classify_style_from_prompt(prompt: &str) -> Option<ImageStyle> {
    let lower = prompt.to_ascii_lowercase();

    // TextHeavy keywords.
    if lower.contains("poster")
        || lower.contains("infographic")
        || lower.contains("typography")
        || lower.contains("headline")
        || lower.contains("banner")
        || lower.contains("sign ")
        || lower.contains(" sign")
        || lower.contains("text that says")
        || lower.contains("caption")
    {
        return Some(ImageStyle::TextHeavy);
    }

    // Anime keywords.
    let anime_score = score_keywords(
        &lower,
        &[
            "anime", "manga", "chibi", "cel shad", "cel-shad", "sakura",
            "kawaii", "moe", "shounen", "shonen", "shojo", "shoujou",
            "studio ghibli", "ghibli", "dragon ball", "naruto",
        ],
    );

    // Cartoon keywords.
    let cartoon_score = score_keywords(
        &lower,
        &[
            "cartoon", "toon", "pixar", "animated", "flat colour", "flat color",
            "vector art", "vector", "caricature", "comic", "children's book",
        ],
    );

    // LineArt keywords.
    let lineart_score = score_keywords(
        &lower,
        &[
            "line art", "lineart", "sketch", "pencil drawing", "ink drawing",
            "technical illustration", "blueprint", "schematic", "outline",
            "black and white line",
        ],
    );

    // Photorealistic keywords.
    let photo_score = score_keywords(
        &lower,
        &[
            "photo", "photograph", "photorealistic", "realistic", "portrait",
            "cinematic", "4k", "8k", "raw photo", "dslr", "lens", "bokeh",
            "studio lighting", "natural light", "hyperrealistic",
        ],
    );

    let scores = [
        (ImageStyle::Photorealistic, photo_score),
        (ImageStyle::Anime, anime_score),
        (ImageStyle::Cartoon, cartoon_score),
        (ImageStyle::LineArt, lineart_score),
    ];

    let max = scores.iter().copied().max_by_key(|&(_, s)| s)?;
    if max.1 == 0 {
        return None; // Ambiguous.
    }

    // Require strict winner — if two styles score equally, treat as ambiguous.
    let second = scores.iter().filter(|&&(s, _)| s != max.0).map(|&(_, v)| v).max().unwrap_or(0);
    if max.1 == second && max.1 > 0 {
        return None;
    }

    Some(max.0)
}

fn score_keywords(lower: &str, keywords: &[&str]) -> u32 {
    keywords.iter().filter(|&&kw| lower.contains(kw)).count() as u32
}

// ─── Aspect ratios ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AspectRatio {
    Square,        // 1:1  → 1024×1024
    Landscape,     // 16:9 → 1024×576
    Portrait,      // 9:16 → 576×1024
    Wide,          // 2.39:1 cinematic → 1024×428
}

impl AspectRatio {
    pub fn dimensions(self) -> (u32, u32) {
        match self {
            Self::Square => (1024, 1024),
            Self::Landscape => (1024, 576),
            Self::Portrait => (576, 1024),
            Self::Wide => (1024, 428),
        }
    }
}

impl Default for AspectRatio {
    fn default() -> Self {
        Self::Square
    }
}

impl std::str::FromStr for AspectRatio {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "landscape" | "16:9" | "wide_screen" => Self::Landscape,
            "portrait" | "9:16" => Self::Portrait,
            "wide" | "cinema" | "cinematic" | "2.39:1" => Self::Wide,
            _ => Self::Square,
        })
    }
}

// ─── Prompt validation / length check ────────────────────────────────────────

/// Estimate token count for style-routing decisions.
/// Rough heuristic: ~1.3 chars per token on average for English.
pub fn estimate_token_count(prompt: &str) -> usize {
    (prompt.split_whitespace().count() as f32 * 1.3).ceil() as usize
}

/// Whether this prompt is short enough to skip T5-XXL (CLIP-only safe).
pub fn is_clip_only_safe(prompt: &str) -> bool {
    estimate_token_count(prompt) <= 50
}
