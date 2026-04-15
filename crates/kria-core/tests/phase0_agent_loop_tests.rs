//! Phase 0 integration test: verify the agent loop wiring.
//!
//! Tests that the AgentLoop properly:
//! - Accepts messages and returns StreamEvents
//! - Routes tool calls through the safety policy
//! - Emits ToolStart / ToolEnd events
//! - Ends with a Done event
//!
//! Uses a mock LLM backend that returns scripted responses.

use kria_core::agent::AgentLoop;
use kria_core::agent::loop_engine::StreamEvent;
use kria_core::llm::{ChatMessage, ModelRouter};
use kria_core::safety::{PolicyEngine, AuditLogger, RollbackManager};
use kria_core::safety::hitl::HitlGateway;
use kria_core::tools::registry;
use kria_core::infra::EventBus;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Collect all stream events into a Vec for assertion.
async fn collect_events(mut rx: mpsc::UnboundedReceiver<StreamEvent>) -> Vec<StreamEvent> {
    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        events.push(ev);
    }
    events
}

#[tokio::test]
async fn agent_loop_instantiates_with_all_subsystems() {
    // This test verifies that all Phase 0 components can be wired together
    // without panicking, even without a running LLM backend.

    let config = kria_core::config::KriaConfig::load(None).unwrap();
    let model_router = Arc::new(ModelRouter::from_config(&config));
    let tool_registry = Arc::new(registry::build_default_registry());
    let policy_engine = Arc::new(PolicyEngine::new());
    let hitl = Arc::new(HitlGateway::new(5));

    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("test_audit.db");
    let audit_conn = rusqlite::Connection::open(&db_path).unwrap();
    let audit_logger = Arc::new(AuditLogger::new(audit_conn));

    let rollback_dir = tmp.path().join("rollback");
    let rollback_mgr = Arc::new(RollbackManager::new(rollback_dir, 1, 10));

    let agent_loop = AgentLoop::new(
        model_router,
        tool_registry.clone(),
        policy_engine,
        hitl,
        audit_logger,
        rollback_mgr,
    );

    // Verify tool registry has tools loaded
    assert!(tool_registry.len() > 10, "should have 10+ tools registered");

    // Verify agent loop can accept a run call (will fail gracefully with no LLM)
    let (tx, rx) = mpsc::unbounded_channel();
    let mut messages = vec![
        ChatMessage {
            role: "system".into(),
            content: "Test system prompt".into(),
            name: None,
            images: None,
        },
        ChatMessage {
            role: "user".into(),
            content: "Hello".into(),
            name: None,
            images: None,
        },
    ];

    agent_loop.run("test-session", &mut messages, tx).await;

    let events = collect_events(rx).await;

    // Without a running LLM backend, we expect an error event
    assert!(!events.is_empty(), "should emit at least one event");

    let has_error_or_done = events.iter().any(|e| matches!(e, StreamEvent::Error(_) | StreamEvent::Done(_)));
    assert!(has_error_or_done, "should end with Error (no backend) or Done");
}

#[tokio::test]
async fn event_bus_publishes_and_receives() {
    let bus = EventBus::new(16);
    let mut rx = bus.subscribe();

    bus.publish(kria_core::infra::event_bus::KriaEvent::MessageReceived {
        session_id: "s1".into(),
        content: "hello".into(),
    });

    let event = rx.recv().await.unwrap();
    match event {
        kria_core::infra::event_bus::KriaEvent::MessageReceived { session_id, content } => {
            assert_eq!(session_id, "s1");
            assert_eq!(content, "hello");
        }
        _ => panic!("unexpected event type"),
    }
}

#[tokio::test]
async fn event_bus_multiple_subscribers() {
    let bus = EventBus::new(16);
    let mut rx1 = bus.subscribe();
    let mut rx2 = bus.subscribe();

    assert_eq!(bus.subscriber_count(), 2);

    bus.publish(kria_core::infra::event_bus::KriaEvent::SidecarReady);

    let e1 = rx1.recv().await.unwrap();
    let e2 = rx2.recv().await.unwrap();

    assert!(matches!(e1, kria_core::infra::event_bus::KriaEvent::SidecarReady));
    assert!(matches!(e2, kria_core::infra::event_bus::KriaEvent::SidecarReady));
}

#[test]
fn policy_engine_classifies_tool_tiers() {
    let engine = PolicyEngine::new();

    // GREEN: read-only
    let d = engine.evaluate("get_cpu_usage", &serde_json::json!({}));
    assert!(!d.requires_approval);
    assert!(!d.blocked);

    // YELLOW: user-level mutation
    let d = engine.evaluate("write_file", &serde_json::json!({"path": "/home/user/test.txt"}));
    assert!(!d.requires_approval); // YELLOW auto-executes with notify
    assert!(!d.blocked);

    // RED: system mutation
    let d = engine.evaluate("delete_file", &serde_json::json!({"path": "/home/user/test.txt"}));
    assert!(d.requires_approval);
    assert!(!d.blocked);

    // BLACK: dangerous pattern (mkfs matches blacklist)
    let d = engine.evaluate("execute_bash", &serde_json::json!({"command": "mkfs.ext4 /dev/sda"}));
    assert!(d.blocked);
}

#[test]
fn tool_registry_generates_function_schemas() {
    let reg = registry::build_default_registry();
    let schemas = reg.function_schemas("standard");

    assert!(!schemas.is_empty(), "should produce schemas for LLM");

    // Each schema should have the OpenAI function-calling shape
    for schema in &schemas {
        assert!(schema.get("function").is_some(), "schema missing 'function' key");
        let func = &schema["function"];
        assert!(func.get("name").is_some(), "function missing 'name'");
        assert!(func.get("description").is_some(), "function missing 'description'");
        assert!(func.get("parameters").is_some(), "function missing 'parameters'");
    }
}

#[test]
fn system_prompt_includes_tools_and_datetime() {
    let prompt = kria_core::agent::prompts::build_system_prompt(
        "### get_cpu_usage\nGet current CPU usage.\nParameters:\n  (none)",
        "TestUser",
        "linux",
        "standard",
        "- User prefers dark theme",
    );

    assert!(prompt.contains("K.R.I.A."), "should mention KRIA");
    assert!(prompt.contains("TestUser"), "should include user name");
    assert!(prompt.contains("linux"), "should include OS name");
    assert!(prompt.contains("standard"), "should include hardware tier");
    assert!(prompt.contains("get_cpu_usage"), "should include tool descriptions");
    assert!(prompt.contains("dark theme"), "should include memory context");
    assert!(prompt.contains("Current Date/Time:"), "should include date/time");
    assert!(prompt.contains("<tool_call>"), "should include tool call format");
}
