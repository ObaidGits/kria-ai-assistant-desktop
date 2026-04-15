/// ────────────────────────────────────────────────────────────────
///  Feature tests for the 7 reported issues
///  1. LLM health – no longer hardcoded
///  2-3. Model detection – router stores api_url, detect_server_model available
///  4. Voice live transcription – PartialTranscript event exists
///  5-6. HITL request_id – UUID instead of "pending"
///  7. Vision / image routing – multimodal content format + fallback
/// ────────────────────────────────────────────────────────────────

use kria_core::config::KriaConfig;
use kria_core::llm::{ChatMessage, ImageAttachment, ModelRouter};
use kria_core::safety::hitl::{HitlGateway, ApprovalResponse};
use kria_core::safety::PolicyEngine;
use kria_core::voice::pipeline::VoicePipelineEvent;

// ═══════════════════════════════════════════════════════════════
//  Issue 5-6: HITL request_id must be a real UUID, not "pending"
// ═══════════════════════════════════════════════════════════════

#[test]
fn hitl_generate_request_id_is_uuid() {
    let id = HitlGateway::generate_request_id();
    // UUID v4 format: 8-4-4-4-12 hex chars
    assert_eq!(id.len(), 36, "request ID should be UUID format (36 chars)");
    assert!(id.contains('-'), "should contain dashes");
    // Parse as UUID to verify format
    assert!(uuid::Uuid::parse_str(&id).is_ok(), "should be a valid UUID");
}

#[test]
fn hitl_generate_unique_ids() {
    let id1 = HitlGateway::generate_request_id();
    let id2 = HitlGateway::generate_request_id();
    assert_ne!(id1, id2, "each call should produce a unique UUID");
}

#[tokio::test]
async fn hitl_request_approval_with_id_responds_correctly() {
    let gateway = HitlGateway::new(5);
    let request_id = HitlGateway::generate_request_id();
    let rid = request_id.clone();

    // Spawn approval request in background
    let gw = std::sync::Arc::new(gateway);
    let gw2 = gw.clone();
    let handle = tokio::spawn(async move {
        gw2.request_approval_with_id(
            &rid,
            "test_action",
            serde_json::json!({}),
            kria_core::safety::RiskLevel::Red,
            "test",
            false,
        ).await
    });

    // Small delay to let the request register
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Respond with the same UUID
    let responded = gw.respond(&request_id, ApprovalResponse::Approved).await;
    assert!(responded, "respond with correct UUID should succeed");

    let result = handle.await.unwrap();
    assert!(matches!(result, ApprovalResponse::Approved));
}

#[tokio::test]
async fn hitl_respond_with_wrong_id_fails() {
    let gateway = HitlGateway::new(2);
    // Responding to a non-existent ID should return false
    let ok = gateway.respond("non-existent-id", ApprovalResponse::Approved).await;
    assert!(!ok, "responding to unknown ID should fail");
}

#[tokio::test]
async fn hitl_respond_with_pending_string_fails() {
    let gateway = HitlGateway::new(2);
    let gw = std::sync::Arc::new(gateway);
    let gw2 = gw.clone();

    // Start a request (generates a real UUID internally)
    let _handle = tokio::spawn(async move {
        gw2.request_approval(
            "test_action",
            serde_json::json!({}),
            kria_core::safety::RiskLevel::Red,
            "test",
            false,
        ).await
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Trying to respond with "pending" should fail — the real UUID is different
    let ok = gw.respond("pending", ApprovalResponse::Approved).await;
    assert!(!ok, "responding with 'pending' should NOT match any real request ID");
}

// ═══════════════════════════════════════════════════════════════
//  Issue 5-6: Policy tiers – search tools should be GREEN
// ═══════════════════════════════════════════════════════════════

#[test]
fn searxng_search_is_green() {
    let engine = PolicyEngine::new();
    let decision = engine.evaluate("searxng_search", &serde_json::json!({"query": "test"}));
    assert_eq!(decision.risk_level, kria_core::safety::RiskLevel::Green);
    assert!(!decision.requires_approval, "searxng_search should auto-execute");
    assert!(!decision.blocked);
}

#[test]
fn duckduckgo_search_is_green() {
    let engine = PolicyEngine::new();
    let decision = engine.evaluate("duckduckgo_search", &serde_json::json!({"query": "test"}));
    assert_eq!(decision.risk_level, kria_core::safety::RiskLevel::Green);
    assert!(!decision.requires_approval);
}

#[test]
fn developer_read_tools_are_green() {
    let engine = PolicyEngine::new();
    for tool in &["git_status", "git_log", "git_diff", "git_branch_list",
                   "analyze_project", "diff_files_unified", "query_sqlite", "describe_database"] {
        let d = engine.evaluate(tool, &serde_json::json!({}));
        assert_eq!(d.risk_level, kria_core::safety::RiskLevel::Green,
            "{tool} should be GREEN");
        assert!(!d.requires_approval, "{tool} should not require approval");
    }
}

#[test]
fn rag_read_tools_are_green() {
    let engine = PolicyEngine::new();
    for tool in &["rag_query", "list_knowledge_base"] {
        let d = engine.evaluate(tool, &serde_json::json!({}));
        assert_eq!(d.risk_level, kria_core::safety::RiskLevel::Green,
            "{tool} should be GREEN");
    }
}

#[test]
fn proactive_read_tools_are_green() {
    let engine = PolicyEngine::new();
    for tool in &["check_system_health", "get_alerts", "dismiss_alert",
                   "list_watched_dirs", "smart_suggest"] {
        let d = engine.evaluate(tool, &serde_json::json!({}));
        assert_eq!(d.risk_level, kria_core::safety::RiskLevel::Green,
            "{tool} should be GREEN");
    }
}

#[test]
fn i18n_tools_are_green() {
    let engine = PolicyEngine::new();
    for tool in &["list_languages", "detect_language", "get_accessibility_settings"] {
        let d = engine.evaluate(tool, &serde_json::json!({}));
        assert_eq!(d.risk_level, kria_core::safety::RiskLevel::Green,
            "{tool} should be GREEN");
    }
}

#[test]
fn vision_analysis_tools_are_green() {
    let engine = PolicyEngine::new();
    for tool in &["ocr_image", "analyze_image", "screenshot_analyze",
                   "image_analyze", "document_extract", "code_analyze_ast"] {
        let d = engine.evaluate(tool, &serde_json::json!({}));
        assert_eq!(d.risk_level, kria_core::safety::RiskLevel::Green,
            "{tool} should be GREEN");
    }
}

#[test]
fn file_read_tools_all_green() {
    let engine = PolicyEngine::new();
    for tool in &["search_file_contents", "find_files_by_pattern",
                   "get_project_structure", "count_lines_of_code",
                   "diff_files", "find_todos", "analyze_code"] {
        let d = engine.evaluate(tool, &serde_json::json!({}));
        assert_eq!(d.risk_level, kria_core::safety::RiskLevel::Green,
            "{tool} should be GREEN");
    }
}

#[test]
fn desktop_window_tools_yellow() {
    let engine = PolicyEngine::new();
    for tool in &["move_window", "resize_window", "maximize_window",
                   "minimize_window", "tile_windows"] {
        let d = engine.evaluate(tool, &serde_json::json!({}));
        assert_eq!(d.risk_level, kria_core::safety::RiskLevel::Yellow,
            "{tool} should be YELLOW");
    }
}

#[test]
fn git_destructive_tools_are_red() {
    let engine = PolicyEngine::new();
    for tool in &["git_commit", "git_checkout"] {
        let d = engine.evaluate(tool, &serde_json::json!({}));
        assert_eq!(d.risk_level, kria_core::safety::RiskLevel::Red,
            "{tool} should be RED");
        assert!(d.requires_approval, "{tool} should require approval");
    }
}

#[test]
fn install_application_is_red() {
    let engine = PolicyEngine::new();
    let d = engine.evaluate("install_application", &serde_json::json!({"name": "vim"}));
    assert_eq!(d.risk_level, kria_core::safety::RiskLevel::Red);
    assert!(d.requires_approval);
}

// ═══════════════════════════════════════════════════════════════
//  Issue 7: Vision – multimodal content format
// ═══════════════════════════════════════════════════════════════

#[test]
fn multimodal_content_format_correct() {
    let msg = ChatMessage {
        role: "user".into(),
        content: "Describe this image".into(),
        name: None,
        images: Some(vec![ImageAttachment {
            data: "iVBORw0KGgo=".into(),
            mime_type: "image/png".into(),
        }]),
    };

    assert!(msg.has_images());

    let content = msg.to_multimodal_content();
    let parts = content.as_array().expect("multimodal content should be an array");
    assert_eq!(parts.len(), 2, "should have text + image parts");

    // Text part
    assert_eq!(parts[0]["type"], "text");
    assert_eq!(parts[0]["text"], "Describe this image");

    // Image part
    assert_eq!(parts[1]["type"], "image_url");
    let url = parts[1]["image_url"]["url"].as_str().unwrap();
    assert!(url.starts_with("data:image/png;base64,"), "should be a data URI");
    assert!(url.contains("iVBORw0KGgo="), "should contain the base64 data");
}

#[test]
fn non_image_message_content_is_string() {
    let msg = ChatMessage {
        role: "user".into(),
        content: "Hello".into(),
        name: None,
        images: None,
    };

    assert!(!msg.has_images());
    let content = msg.to_multimodal_content();
    assert!(content.is_string(), "non-image message should produce a string");
    assert_eq!(content.as_str().unwrap(), "Hello");
}

// ═══════════════════════════════════════════════════════════════
//  Issue 7: Vision routing – local backend serves as vision fallback
// ═══════════════════════════════════════════════════════════════

#[test]
fn vision_available_with_default_local_config() {
    let config = KriaConfig::default();
    let router = ModelRouter::from_config(&config);
    assert!(router.has_vision(),
        "with local_api_url set, vision should be available via local fallback");
}

#[tokio::test]
async fn vision_route_returns_backend_with_default_config() {
    let config = KriaConfig::default();
    let router = ModelRouter::from_config(&config);
    let backend = router.route_vision().await;
    assert!(backend.is_some(), "route_vision should return a backend");
}

#[test]
fn no_vision_when_no_local_url() {
    let mut config = KriaConfig::default();
    config.llm.local_api_url = String::new();
    let router = ModelRouter::from_config(&config);
    assert!(!router.has_vision(),
        "without local backend, vision should not be available");
}

// ═══════════════════════════════════════════════════════════════
//  Issue 2-3: Model router has detect_server_model
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn detect_server_model_returns_none_when_server_down() {
    // Default config points to localhost:8080 which is likely not running in tests
    let config = KriaConfig::default();
    let router = ModelRouter::from_config(&config);
    let model = router.detect_server_model().await;
    // The server is not running, so it should return None (not panic)
    assert!(model.is_none(), "should gracefully return None when server is unreachable");
}

#[tokio::test]
async fn detect_server_model_none_when_no_url() {
    let mut config = KriaConfig::default();
    config.llm.local_api_url = String::new();
    let router = ModelRouter::from_config(&config);
    let model = router.detect_server_model().await;
    assert!(model.is_none());
}

// ═══════════════════════════════════════════════════════════════
//  Issue 1: Health registry uses real status (unit-level test)
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn model_router_status_reports_unhealthy_when_server_down() {
    let config = KriaConfig::default();
    let router = ModelRouter::from_config(&config);
    let status = router.status().await;

    // Server not running in test → should report unhealthy
    let healthy = status["local_healthy"].as_bool().unwrap_or(true);
    assert!(!healthy, "local_healthy should be false when llama server is not running");
}

// ═══════════════════════════════════════════════════════════════
//  Issue 4: Voice pipeline has PartialTranscript event
// ═══════════════════════════════════════════════════════════════

#[test]
fn partial_transcript_event_exists() {
    // Verify the PartialTranscript variant can be constructed
    let evt = VoicePipelineEvent::PartialTranscript("hello wor".to_string());
    match evt {
        VoicePipelineEvent::PartialTranscript(text) => {
            assert_eq!(text, "hello wor");
        }
        _ => panic!("expected PartialTranscript variant"),
    }
}

#[test]
fn partial_transcript_is_separate_from_final() {
    let partial = VoicePipelineEvent::PartialTranscript("partial".into());
    let final_t = VoicePipelineEvent::Transcript("final".into());

    // Both should match their own variant
    assert!(matches!(partial, VoicePipelineEvent::PartialTranscript(_)));
    assert!(matches!(final_t, VoicePipelineEvent::Transcript(_)));
    // They should NOT match each other's variant
    assert!(!matches!(partial, VoicePipelineEvent::Transcript(_)));
    assert!(!matches!(final_t, VoicePipelineEvent::PartialTranscript(_)));
}
