use std::sync::Arc;
use std::collections::HashMap;
use async_trait::async_trait;
use crate::infra::ToolResult;
use crate::safety::RiskLevel;
use crate::tools::registry::{ToolRegistry, ToolDef, ToolHandler, ParamDef};

// ─── Built-in translation dictionaries (lightweight, no LLM needed) ───

fn supported_languages() -> Vec<(&'static str, &'static str)> {
    vec![
        ("en", "English"),
        ("es", "Español"),
        ("de", "Deutsch"),
        ("fr", "Français"),
        ("zh", "中文"),
        ("ar", "العربية"),
        ("hi", "हिन्दी"),
    ]
}

// ─── Handlers ───

struct ListLanguages;
#[async_trait]
impl ToolHandler for ListLanguages {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        let langs: Vec<serde_json::Value> = supported_languages()
            .iter()
            .map(|(code, name)| serde_json::json!({ "code": code, "label": name }))
            .collect();
        ToolResult::ok(serde_json::json!({ "languages": langs }))
    }
}

struct DetectLanguage;
#[async_trait]
impl ToolHandler for DetectLanguage {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
        if text.is_empty() {
            return ToolResult::err("text parameter is required");
        }

        // Simple heuristic detection based on Unicode script ranges
        let mut scripts: HashMap<&str, usize> = HashMap::new();
        for ch in text.chars() {
            let script = match ch {
                '\u{0600}'..='\u{06FF}' => "ar",
                '\u{0900}'..='\u{097F}' => "hi",
                '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}' => "zh",
                '\u{00C0}'..='\u{00FF}' => "latin_extended",
                'a'..='z' | 'A'..='Z' => "latin",
                _ => continue,
            };
            *scripts.entry(script).or_insert(0) += 1;
        }

        let detected = if scripts.contains_key("ar") {
            "ar"
        } else if scripts.contains_key("hi") {
            "hi"
        } else if scripts.contains_key("zh") {
            "zh"
        } else if scripts.contains_key("latin_extended") {
            // Could be es, de, fr — just mark as "latin_extended"
            "es" // best guess for accented Latin
        } else {
            "en"
        };

        ToolResult::ok(serde_json::json!({
            "detected_language": detected,
            "confidence": "heuristic",
        }))
    }
}

struct GetAccessibilitySettings;
#[async_trait]
impl ToolHandler for GetAccessibilitySettings {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        ToolResult::ok(serde_json::json!({
            "available_options": {
                "high_contrast": "Enable high-contrast color scheme for better visibility",
                "reduce_motion": "Reduce animations for users with motion sensitivity",
                "font_scale": "Adjust font size multiplier (0.8 to 2.0)",
                "language": "Set interface language (en, es, de, fr, zh, ar, hi)",
            }
        }))
    }
}

// ─── Registration ───

pub fn register(reg: &mut ToolRegistry) {
    reg.register(
        ToolDef {
            name: "list_languages".into(),
            description: "List all supported interface languages.".into(),
            category: "i18n".into(),
            parameters: vec![],
            default_tier: RiskLevel::Green,
            min_tier: "lite",
        },
        Arc::new(ListLanguages),
    );

    reg.register(
        ToolDef {
            name: "detect_language".into(),
            description: "Detect the language of a text snippet using heuristics.".into(),
            category: "i18n".into(),
            parameters: vec![ParamDef {
                name: "text".into(),
                param_type: "string".into(),
                description: "The text to analyze for language detection.".into(),
                required: true,
                default: None,
            }],
            default_tier: RiskLevel::Green,
            min_tier: "lite",
        },
        Arc::new(DetectLanguage),
    );

    reg.register(
        ToolDef {
            name: "get_accessibility_settings".into(),
            description: "Get available accessibility options and their descriptions.".into(),
            category: "i18n".into(),
            parameters: vec![],
            default_tier: RiskLevel::Green,
            min_tier: "lite",
        },
        Arc::new(GetAccessibilitySettings),
    );
}
