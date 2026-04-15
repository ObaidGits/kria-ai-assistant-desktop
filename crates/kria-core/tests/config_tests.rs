//! Feature tests for the KRIA config system.
//!
//! Tests config loading, default values, env-var overrides,
//! merge behaviour, and auto model selection — covering both
//! happy paths and edge cases.

use kria_core::config::*;
use kria_core::platform::HardwareTier;
use std::io::Write;
use tempfile::NamedTempFile;

// ── Default values ──────────────────────────────────────────────────

#[test]
fn default_config_has_expected_values() {
    let cfg = KriaConfig::default();

    assert_eq!(cfg.llm.active_model, "phi-4-mini");
    assert_eq!(cfg.llm.local_api_url, "http://127.0.0.1:8080/v1");
    assert_eq!(cfg.llm.routing_mode, "local");
    assert_eq!(cfg.llm.context_window, 4096);
    assert_eq!(cfg.llm.max_tokens, 2048);
    assert!((cfg.llm.temperature - 0.6).abs() < f32::EPSILON);
    assert_eq!(cfg.llm.gpu_layers, -1);
    assert_eq!(cfg.llm.max_iterations, 10);

    assert!(!cfg.voice.enabled);
    assert_eq!(cfg.voice.mode, "push_to_talk");
    assert_eq!(cfg.voice.stt_model, "ggml-base.en.bin");

    assert_eq!(cfg.memory.max_context_turns, 20);
    assert_eq!(cfg.memory.max_facts, 1000);
    assert_eq!(cfg.memory.embedding_dim, 384);

    assert_eq!(cfg.safety.hitl_timeout_secs, 30);
    assert!(!cfg.safety.emergency_mode);
    assert_eq!(cfg.safety.max_concurrent_tools, 3);

    assert_eq!(cfg.server.port, 8088);
    assert!(!cfg.server.enable_auth);

    assert_eq!(cfg.ui.theme, "dark");
    assert_eq!(cfg.ui.window_width, 1200);
}

// ── Loading from TOML ───────────────────────────────────────────────

#[test]
fn load_config_from_valid_toml() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(
        f,
        r#"
[llm]
active_model = "custom-model"
context_window = 8192

[server]
port = 9999
"#
    )
    .unwrap();

    let cfg = load_config(f.path(), None).unwrap();
    assert_eq!(cfg.llm.active_model, "custom-model");
    assert_eq!(cfg.llm.context_window, 8192);
    assert_eq!(cfg.server.port, 9999);
    // Untouched fields remain default
    assert_eq!(cfg.ui.theme, "dark");
}

#[test]
fn load_config_falls_back_to_defaults_if_file_missing() {
    let path = std::path::Path::new("/tmp/kria_test_nonexistent_38472.toml");
    assert!(!path.exists());

    let cfg = load_config(path, None).unwrap();
    assert_eq!(cfg.llm.active_model, "phi-4-mini");
}

// ── Merge override ──────────────────────────────────────────────────

#[test]
fn override_file_merges_into_base() {
    let mut base_f = NamedTempFile::new().unwrap();
    writeln!(
        base_f,
        r#"
[llm]
active_model = "base-model"
routing_mode = "local"
"#
    )
    .unwrap();

    let mut override_f = NamedTempFile::new().unwrap();
    writeln!(
        override_f,
        r#"
[llm]
active_model = "override-model"
routing_mode = "gemini"
cloud_api_key = "sk-test-key"
"#
    )
    .unwrap();

    let cfg = load_config(base_f.path(), Some(override_f.path())).unwrap();
    // Override values applied
    assert_eq!(cfg.llm.routing_mode, "gemini");
    assert_eq!(cfg.llm.cloud_api_key, "sk-test-key");
    assert_eq!(cfg.llm.active_model, "override-model");
}

// ── Env-var overrides ───────────────────────────────────────────────

#[test]
fn env_vars_override_config_values() {
    // Use a serial test approach — set, load, unset
    let path = std::path::Path::new("/tmp/kria_test_nonexistent_env.toml");

    std::env::set_var("KRIA_LLM_MODE", "cloud");
    std::env::set_var("KRIA_CLOUD_API_KEY", "env-secret-key");

    let cfg = load_config(path, None).unwrap();

    // Clean up immediately
    std::env::remove_var("KRIA_LLM_MODE");
    std::env::remove_var("KRIA_CLOUD_API_KEY");

    assert_eq!(cfg.llm.routing_mode, "cloud");
    assert_eq!(cfg.llm.cloud_api_key, "env-secret-key");
}

// ── Auto model selection ────────────────────────────────────────────

#[test]
fn auto_select_model_maps_tiers_correctly() {
    assert_eq!(auto_select_model(HardwareTier::Lite), "qwen2.5-3b");
    assert_eq!(auto_select_model(HardwareTier::Standard), "phi-4-mini");
    assert_eq!(auto_select_model(HardwareTier::Performance), "qwen2.5-vl-7b");
    assert_eq!(auto_select_model(HardwareTier::High), "qwen2.5-vl-7b");
}

// ── Serialization roundtrip ─────────────────────────────────────────

#[test]
fn config_survives_json_roundtrip() {
    let original = KriaConfig::default();
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: KriaConfig = serde_json::from_str(&json).unwrap();

    assert_eq!(original.llm.active_model, deserialized.llm.active_model);
    assert_eq!(original.server.port, deserialized.server.port);
    assert_eq!(original.ui.theme, deserialized.ui.theme);
}

// ── Edge case: invalid TOML ─────────────────────────────────────────

#[test]
fn load_config_returns_error_for_invalid_toml() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(f, "this is {{ not valid TOML").unwrap();

    let result = load_config(f.path(), None);
    assert!(result.is_err());
}

// ── Phase 6: Config save / TOML roundtrip ───────────────────────────

#[test]
fn config_survives_toml_roundtrip() {
    let original = KriaConfig::default();
    let toml_str = toml::to_string_pretty(&original).unwrap();
    let deserialized: KriaConfig = toml::from_str(&toml_str).unwrap();

    assert_eq!(original.llm.active_model, deserialized.llm.active_model);
    assert_eq!(original.llm.routing_mode, deserialized.llm.routing_mode);
    assert_eq!(original.server.port, deserialized.server.port);
    assert_eq!(original.ui.theme, deserialized.ui.theme);
    assert_eq!(original.voice.tts_voice, deserialized.voice.tts_voice);
    assert_eq!(original.safety.hitl_timeout_secs, deserialized.safety.hitl_timeout_secs);
    assert_eq!(original.search.engine, deserialized.search.engine);
}

#[test]
fn config_save_writes_valid_toml_to_disk() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.toml");

    let mut cfg = KriaConfig::default();
    cfg.llm.active_model = "test-model".into();
    cfg.ui.theme = "light".into();
    cfg.safety.emergency_mode = true;
    cfg.voice.enabled = true;

    // Write manually (since save() uses KriaPaths)
    let toml_str = toml::to_string_pretty(&cfg).unwrap();
    std::fs::write(&config_path, &toml_str).unwrap();

    // Read back and verify
    let loaded = load_config(&config_path, None).unwrap();
    assert_eq!(loaded.llm.active_model, "test-model");
    assert_eq!(loaded.ui.theme, "light");
    assert!(loaded.safety.emergency_mode);
    assert!(loaded.voice.enabled);
}

#[test]
fn config_preserves_all_sections_through_toml() {
    let mut cfg = KriaConfig::default();
    cfg.llm.cloud_provider = "gemini".into();
    cfg.llm.cloud_api_key = "sk-test-123".into();
    cfg.llm.temperature = 0.8;
    cfg.llm.max_tokens = 4096;
    cfg.voice.mode = "continuous".into();
    cfg.voice.vad_silence_ms = 500;
    cfg.memory.max_context_turns = 50;
    cfg.safety.tool_timeout_secs = 60;
    cfg.search.engine = "searxng".into();
    cfg.search.searxng_url = "http://my-instance:9090".into();

    let toml_str = toml::to_string_pretty(&cfg).unwrap();
    let loaded: KriaConfig = toml::from_str(&toml_str).unwrap();

    assert_eq!(loaded.llm.cloud_provider, "gemini");
    assert_eq!(loaded.llm.cloud_api_key, "sk-test-123");
    assert!((loaded.llm.temperature - 0.8).abs() < f32::EPSILON);
    assert_eq!(loaded.llm.max_tokens, 4096);
    assert_eq!(loaded.voice.mode, "continuous");
    assert_eq!(loaded.voice.vad_silence_ms, 500);
    assert_eq!(loaded.memory.max_context_turns, 50);
    assert_eq!(loaded.safety.tool_timeout_secs, 60);
    assert_eq!(loaded.search.engine, "searxng");
    assert_eq!(loaded.search.searxng_url, "http://my-instance:9090");
}

#[test]
fn config_json_to_toml_and_back() {
    // Simulates settings UI flow: frontend sends JSON → backend deserializes → saves as TOML → loads back
    let original = KriaConfig::default();
    let json = serde_json::to_string(&original).unwrap();
    let from_json: KriaConfig = serde_json::from_str(&json).unwrap();
    let toml_str = toml::to_string_pretty(&from_json).unwrap();
    let from_toml: KriaConfig = toml::from_str(&toml_str).unwrap();

    assert_eq!(from_toml.llm.active_model, original.llm.active_model);
    assert_eq!(from_toml.ui.theme, original.ui.theme);
    assert_eq!(from_toml.server.port, original.server.port);
    assert_eq!(from_toml.voice.tts_voice, original.voice.tts_voice);
}

#[test]
fn config_partial_toml_retains_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("partial.toml");
    std::fs::write(
        &path,
        r#"
[ui]
theme = "light"
"#,
    )
    .unwrap();

    let cfg = load_config(&path, None).unwrap();
    assert_eq!(cfg.ui.theme, "light");
    // Everything else remains default
    assert_eq!(cfg.llm.active_model, "phi-4-mini");
    assert_eq!(cfg.voice.mode, "push_to_talk");
    assert_eq!(cfg.safety.hitl_timeout_secs, 30);
    assert_eq!(cfg.search.engine, "duckduckgo");
}

#[test]
fn config_cloud_api_key_can_be_updated_and_saved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("api_key_test.toml");

    let mut cfg = KriaConfig::default();
    assert!(cfg.llm.cloud_api_key.is_empty());

    cfg.llm.cloud_api_key = "sk-new-key-1234".into();
    let toml_str = toml::to_string_pretty(&cfg).unwrap();
    std::fs::write(&path, &toml_str).unwrap();

    let loaded = load_config(&path, None).unwrap();
    assert_eq!(loaded.llm.cloud_api_key, "sk-new-key-1234");
}

#[test]
fn theme_toggle_persists_through_save_load() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("theme_test.toml");

    // Start dark
    let mut cfg = KriaConfig::default();
    assert_eq!(cfg.ui.theme, "dark");

    // Toggle to light
    cfg.ui.theme = "light".into();
    let toml_str = toml::to_string_pretty(&cfg).unwrap();
    std::fs::write(&path, &toml_str).unwrap();

    let loaded = load_config(&path, None).unwrap();
    assert_eq!(loaded.ui.theme, "light");

    // Toggle back to dark
    let mut cfg2 = loaded;
    cfg2.ui.theme = "dark".into();
    let toml_str2 = toml::to_string_pretty(&cfg2).unwrap();
    std::fs::write(&path, &toml_str2).unwrap();

    let loaded2 = load_config(&path, None).unwrap();
    assert_eq!(loaded2.ui.theme, "dark");
}
