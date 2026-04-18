//! Phase 0.5 integration tests — Sidecar Bridge & Pre-Cognitive tools.

use kria_core::sidecar::health::SidecarHealth;
use kria_core::sidecar::protocol::{JsonRpcRequest, JsonRpcResponse};
use kria_core::sidecar::SidecarBridge;
use kria_core::tools::precognitive;
use kria_core::tools::registry;
use std::sync::Arc;

// ── Protocol Tests ──────────────────────────────────────────

#[test]
fn json_rpc_request_serializes_correctly() {
    let req = JsonRpcRequest::new(1, "ping", serde_json::json!({}));
    let json = serde_json::to_string(&req).unwrap();
    assert!(json.contains("\"jsonrpc\":\"2.0\""));
    assert!(json.contains("\"id\":1"));
    assert!(json.contains("\"method\":\"ping\""));
}

#[test]
fn json_rpc_response_ok_parses() {
    let raw = r#"{"jsonrpc":"2.0","id":1,"result":{"pong":true}}"#;
    let resp: JsonRpcResponse = serde_json::from_str(raw).unwrap();
    assert_eq!(resp.id, Some(1));
    assert!(resp.error.is_none());
    let result = resp.into_result().unwrap();
    assert_eq!(result["pong"], true);
}

#[test]
fn json_rpc_response_error_parses() {
    let raw = r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"Method not found"}}"#;
    let resp: JsonRpcResponse = serde_json::from_str(raw).unwrap();
    assert!(resp.error.is_some());
    let err = resp.into_result().unwrap_err();
    assert!(err.contains("-32601"));
    assert!(err.contains("Method not found"));
}

#[test]
fn json_rpc_response_null_result() {
    let raw = r#"{"jsonrpc":"2.0","id":3,"result":null}"#;
    let resp: JsonRpcResponse = serde_json::from_str(raw).unwrap();
    let result = resp.into_result().unwrap();
    assert!(result.is_null());
}

// ── Health Tests ────────────────────────────────────────────

#[test]
fn sidecar_health_starts_not_alive() {
    let health = SidecarHealth::new(3);
    assert!(!health.is_alive());
}

#[test]
fn sidecar_health_mark_alive() {
    let health = SidecarHealth::new(3);
    health.mark_alive();
    assert!(health.is_alive());
}

#[test]
fn sidecar_health_stop() {
    let health = SidecarHealth::new(3);
    health.mark_alive();
    health.stop();
    // stop doesn't change alive status, just signals the loop to exit
    assert!(health.is_alive());
}

// ── Bridge Tests ────────────────────────────────────────────

#[test]
fn sidecar_bridge_creates() {
    let bridge = SidecarBridge::new("python3", Some("/tmp/test-env"));
    assert!(!bridge.is_alive());
}

#[test]
fn sidecar_bridge_debug_format() {
    let bridge = SidecarBridge::new("python3", None);
    let debug = format!("{:?}", bridge);
    assert!(debug.contains("SidecarBridge"));
    assert!(debug.contains("python3"));
}

// ── Precognitive Tool Registration ──────────────────────────

#[test]
fn precognitive_tools_register_into_registry() {
    let registry = registry::build_default_registry();
    let base_count = registry.len();

    let bridge = Arc::new(SidecarBridge::new("python3", None));
    precognitive::register(&registry, bridge);

    assert_eq!(
        registry.len(),
        base_count + 6,
        "Should add 6 precognitive tools"
    );

    // Verify each tool exists
    assert!(registry.get_def("image_analyze").is_some());
    assert!(registry.get_def("document_extract").is_some());
    assert!(registry.get_def("code_analyze_ast").is_some());
    assert!(registry.get_def("web_extract_article").is_some());
    assert!(registry.get_def("embeddings_generate").is_some());
    assert!(registry.get_def("audio_preprocess").is_some());
}

#[test]
fn precognitive_tools_in_correct_category() {
    let registry = registry::build_default_registry();
    let bridge = Arc::new(SidecarBridge::new("python3", None));
    precognitive::register(&registry, bridge);

    let precog_tools = registry.list_by_category("precognitive");
    assert_eq!(precog_tools.len(), 6);
}

#[test]
fn precognitive_tools_have_handlers() {
    let registry = registry::build_default_registry();
    let bridge = Arc::new(SidecarBridge::new("python3", None));
    precognitive::register(&registry, bridge);

    assert!(registry.get_handler("image_analyze").is_some());
    assert!(registry.get_handler("document_extract").is_some());
    assert!(registry.get_handler("code_analyze_ast").is_some());
    assert!(registry.get_handler("web_extract_article").is_some());
    assert!(registry.get_handler("embeddings_generate").is_some());
    assert!(registry.get_handler("audio_preprocess").is_some());
}

#[test]
fn precognitive_tools_generate_function_schemas() {
    let registry = registry::build_default_registry();
    let bridge = Arc::new(SidecarBridge::new("python3", None));
    precognitive::register(&registry, bridge);

    let schemas = registry.function_schemas("standard");
    let precog_names: Vec<&str> = vec![
        "image_analyze",
        "document_extract",
        "code_analyze_ast",
        "web_extract_article",
        "embeddings_generate",
        "audio_preprocess",
    ];

    for name in precog_names {
        let found = schemas
            .iter()
            .any(|s| s["function"]["name"].as_str() == Some(name));
        assert!(found, "Schema missing for {}", name);
    }
}

#[test]
fn precognitive_tools_tier_filtering() {
    let registry = registry::build_default_registry();
    let bridge = Arc::new(SidecarBridge::new("python3", None));
    precognitive::register(&registry, bridge);

    // "lite" tier should see document_extract and code_analyze_ast (min_tier: "lite")
    let lite_tools = registry.list_for_tier("lite");
    let lite_names: Vec<&str> = lite_tools.iter().map(|d| d.name.as_str()).collect();
    assert!(
        lite_names.contains(&"document_extract"),
        "document_extract should be available on lite"
    );
    assert!(
        lite_names.contains(&"code_analyze_ast"),
        "code_analyze_ast should be available on lite"
    );
    // image_analyze requires "standard"
    assert!(
        !lite_names.contains(&"image_analyze"),
        "image_analyze should NOT be on lite"
    );

    // "standard" tier should see all 6
    let standard_tools = registry.list_for_tier("standard");
    let std_names: Vec<&str> = standard_tools.iter().map(|d| d.name.as_str()).collect();
    assert!(std_names.contains(&"image_analyze"));
    assert!(std_names.contains(&"web_extract_article"));
    assert!(std_names.contains(&"embeddings_generate"));
    assert!(std_names.contains(&"audio_preprocess"));
}
