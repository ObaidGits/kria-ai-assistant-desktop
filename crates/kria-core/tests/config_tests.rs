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
