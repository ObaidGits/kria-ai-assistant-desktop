use kria_core::config::{KriaConfig, UiConfig};
use kria_core::tools::registry::build_default_registry;

// ──────────────────────────────────────────────
// Config – i18n & Accessibility fields
// ──────────────────────────────────────────────

#[test]
fn ui_config_defaults_language_en() {
    let cfg = UiConfig::default();
    assert_eq!(cfg.language, "en");
}

#[test]
fn ui_config_defaults_high_contrast_false() {
    let cfg = UiConfig::default();
    assert!(!cfg.high_contrast);
}

#[test]
fn ui_config_defaults_reduce_motion_false() {
    let cfg = UiConfig::default();
    assert!(!cfg.reduce_motion);
}

#[test]
fn ui_config_defaults_font_scale_1() {
    let cfg = UiConfig::default();
    assert!((cfg.font_scale - 1.0).abs() < f32::EPSILON);
}

#[test]
fn ui_config_serialization_roundtrip() {
    let mut ui = UiConfig::default();
    ui.language = "fr".into();
    ui.high_contrast = true;
    ui.reduce_motion = true;
    ui.font_scale = 1.5;

    let toml_str = toml::to_string_pretty(&ui).unwrap();
    let parsed: UiConfig = toml::from_str(&toml_str).unwrap();

    assert_eq!(parsed.language, "fr");
    assert!(parsed.high_contrast);
    assert!(parsed.reduce_motion);
    assert!((parsed.font_scale - 1.5).abs() < f32::EPSILON);
}

#[test]
fn kria_config_includes_ui_language() {
    let cfg = KriaConfig::default();
    assert_eq!(cfg.ui.language, "en");
    assert!(!cfg.ui.high_contrast);
}

// ──────────────────────────────────────────────
// i18n Tools – Registry
// ──────────────────────────────────────────────

#[test]
fn registry_has_list_languages() {
    let reg = build_default_registry();
    assert!(reg.get_def("list_languages").is_some());
}

#[test]
fn registry_has_detect_language() {
    let reg = build_default_registry();
    assert!(reg.get_def("detect_language").is_some());
}

#[test]
fn registry_has_get_accessibility_settings() {
    let reg = build_default_registry();
    assert!(reg.get_def("get_accessibility_settings").is_some());
}

#[test]
fn list_languages_tool_metadata() {
    let reg = build_default_registry();
    let def = reg.get_def("list_languages").unwrap();
    assert_eq!(def.category, "i18n");
    assert_eq!(def.min_tier, "lite");
    assert!(def.parameters.is_empty());
}

#[test]
fn detect_language_requires_text() {
    let reg = build_default_registry();
    let def = reg.get_def("detect_language").unwrap();
    assert_eq!(def.parameters.len(), 1);
    assert!(def.parameters[0].required);
    assert_eq!(def.parameters[0].name, "text");
}

#[test]
fn i18n_tools_in_category() {
    let reg = build_default_registry();
    let i18n_tools = reg.list_by_category("i18n");
    assert_eq!(i18n_tools.len(), 3);
}

// ──────────────────────────────────────────────
// i18n Tools – Execution
// ──────────────────────────────────────────────

#[tokio::test]
async fn list_languages_returns_all() {
    let reg = build_default_registry();
    let handler = reg.get_handler("list_languages").unwrap();
    let result = handler.execute(serde_json::json!({})).await;
    assert!(result.success);
    let langs = result.data["languages"].as_array().unwrap();
    assert!(langs.len() >= 7);
    let codes: Vec<&str> = langs.iter().map(|l| l["code"].as_str().unwrap()).collect();
    assert!(codes.contains(&"en"));
    assert!(codes.contains(&"es"));
    assert!(codes.contains(&"zh"));
    assert!(codes.contains(&"ar"));
    assert!(codes.contains(&"hi"));
}

#[tokio::test]
async fn detect_language_english() {
    let reg = build_default_registry();
    let handler = reg.get_handler("detect_language").unwrap();
    let result = handler.execute(serde_json::json!({ "text": "Hello world, this is a test" })).await;
    assert!(result.success);
    assert_eq!(result.data["detected_language"].as_str().unwrap(), "en");
}

#[tokio::test]
async fn detect_language_arabic() {
    let reg = build_default_registry();
    let handler = reg.get_handler("detect_language").unwrap();
    let result = handler.execute(serde_json::json!({ "text": "مرحبا بالعالم هذا اختبار" })).await;
    assert!(result.success);
    assert_eq!(result.data["detected_language"].as_str().unwrap(), "ar");
}

#[tokio::test]
async fn detect_language_chinese() {
    let reg = build_default_registry();
    let handler = reg.get_handler("detect_language").unwrap();
    let result = handler.execute(serde_json::json!({ "text": "你好世界这是一个测试" })).await;
    assert!(result.success);
    assert_eq!(result.data["detected_language"].as_str().unwrap(), "zh");
}

#[tokio::test]
async fn detect_language_hindi() {
    let reg = build_default_registry();
    let handler = reg.get_handler("detect_language").unwrap();
    let result = handler.execute(serde_json::json!({ "text": "नमस्ते दुनिया यह एक परीक्षण है" })).await;
    assert!(result.success);
    assert_eq!(result.data["detected_language"].as_str().unwrap(), "hi");
}

#[tokio::test]
async fn detect_language_empty_text_error() {
    let reg = build_default_registry();
    let handler = reg.get_handler("detect_language").unwrap();
    let result = handler.execute(serde_json::json!({ "text": "" })).await;
    assert!(!result.success);
}

#[tokio::test]
async fn get_accessibility_settings_returns_options() {
    let reg = build_default_registry();
    let handler = reg.get_handler("get_accessibility_settings").unwrap();
    let result = handler.execute(serde_json::json!({})).await;
    assert!(result.success);
    let opts = &result.data["available_options"];
    assert!(opts["high_contrast"].is_string());
    assert!(opts["reduce_motion"].is_string());
    assert!(opts["font_scale"].is_string());
    assert!(opts["language"].is_string());
}

// ──────────────────────────────────────────────
// i18n Tools – tier filtering
// ──────────────────────────────────────────────

#[test]
fn i18n_tools_available_on_lite_tier() {
    let reg = build_default_registry();
    let lite_tools = reg.list_for_tier("lite");
    let names: Vec<&str> = lite_tools.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"list_languages"));
    assert!(names.contains(&"detect_language"));
    assert!(names.contains(&"get_accessibility_settings"));
}

// ──────────────────────────────────────────────
// Config – edge cases
// ──────────────────────────────────────────────

#[test]
fn ui_config_deserializes_without_new_fields() {
    // Old config without new fields should still deserialize with defaults
    let toml_str = r#"
theme = "dark"
window_width = 1200
window_height = 800
"#;
    let ui: UiConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(ui.language, "en");
    assert!(!ui.high_contrast);
    assert!(!ui.reduce_motion);
    assert!((ui.font_scale - 1.0).abs() < f32::EPSILON);
}

#[test]
fn ui_config_custom_font_scale() {
    let toml_str = r#"
theme = "light"
window_width = 800
window_height = 600
language = "de"
high_contrast = true
reduce_motion = false
font_scale = 2.0
"#;
    let ui: UiConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(ui.language, "de");
    assert!(ui.high_contrast);
    assert!(!ui.reduce_motion);
    assert!((ui.font_scale - 2.0).abs() < f32::EPSILON);
}

#[test]
fn function_schema_for_detect_language() {
    let reg = build_default_registry();
    let def = reg.get_def("detect_language").unwrap();
    let schema = def.to_function_schema();
    let func = &schema["function"];
    assert_eq!(func["name"].as_str().unwrap(), "detect_language");
    let params = &func["parameters"];
    assert!(params["properties"]["text"].is_object());
    let required = params["required"].as_array().unwrap();
    assert!(required.contains(&serde_json::json!("text")));
}
