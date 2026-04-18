//! Phase 0 integration test: verify the agent loop wiring.
//!
//! Tests that the AgentLoop properly:
//! - Accepts messages and returns StreamEvents
//! - Routes tool calls through the safety policy
//! - Emits ToolStart / ToolEnd events
//! - Ends with a Done event
//!
//! Uses a mock LLM backend that returns scripted responses.

use kria_core::agent::loop_engine::StreamEvent;
use kria_core::agent::AgentLoop;
use kria_core::infra::EventBus;
use kria_core::llm::{ChatMessage, ModelRouter};
use kria_core::safety::hitl::HitlGateway;
use kria_core::safety::{AuditLogger, PolicyEngine, RollbackManager};
use kria_core::tools::registry;
use std::sync::Arc;
use tokio::sync::mpsc;

fn spawn_mock_chat_server(
    responses: Vec<serde_json::Value>,
) -> (String, std::thread::JoinHandle<()>) {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn read_one_http_request(stream: &mut std::net::TcpStream) -> bool {
        let mut buf = Vec::<u8>::new();
        let mut tmp = [0u8; 2048];

        // Read headers
        loop {
            match stream.read(&mut tmp) {
                Ok(0) => return false,
                Ok(n) => {
                    buf.extend_from_slice(&tmp[..n]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                Err(_) => return false,
            }
        }

        let header_end = match buf.windows(4).position(|w| w == b"\r\n\r\n") {
            Some(i) => i + 4,
            None => return false,
        };

        let header_text = String::from_utf8_lossy(&buf[..header_end]);
        let mut content_len = 0usize;
        for line in header_text.lines() {
            let lower = line.to_ascii_lowercase();
            if let Some(v) = lower.strip_prefix("content-length:") {
                content_len = v.trim().parse::<usize>().unwrap_or(0);
                break;
            }
        }

        // Read body if needed
        let needed = header_end + content_len;
        while buf.len() < needed {
            match stream.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
                Err(_) => break,
            }
        }

        true
    }

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock chat server");
    let addr = listener.local_addr().expect("local addr");
    let handle = std::thread::spawn(move || {
        for body in responses {
            let (mut stream, _) = listener.accept().expect("accept connection");
            if !read_one_http_request(&mut stream) {
                continue;
            }

            let payload = body.to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.flush();
        }
    });

    (format!("http://{}/v1", addr), handle)
}

fn mock_tool_call_response(tool_name: &str, arguments: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": tool_name,
                        "arguments": arguments.to_string()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {
            "prompt_tokens": 1,
            "completion_tokens": 1,
            "total_tokens": 2
        }
    })
}

fn mock_text_response(text: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": text
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 1,
            "completion_tokens": 1,
            "total_tokens": 2
        }
    })
}

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
        Arc::new(tokio::sync::RwLock::new(
            kria_core::tools::mount_manager::ToolMountManager::new(),
        )),
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

    let has_error_or_done = events
        .iter()
        .any(|e| matches!(e, StreamEvent::Error(_) | StreamEvent::Done(_)));
    assert!(
        has_error_or_done,
        "should end with Error (no backend) or Done"
    );
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
        kria_core::infra::event_bus::KriaEvent::MessageReceived {
            session_id,
            content,
        } => {
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

    assert!(matches!(
        e1,
        kria_core::infra::event_bus::KriaEvent::SidecarReady
    ));
    assert!(matches!(
        e2,
        kria_core::infra::event_bus::KriaEvent::SidecarReady
    ));
}

#[test]
fn policy_engine_classifies_tool_tiers() {
    let engine = PolicyEngine::new();

    // GREEN: read-only
    let d = engine.evaluate("get_cpu_usage", &serde_json::json!({}));
    assert!(!d.requires_approval);
    assert!(!d.blocked);

    // YELLOW: user-level mutation
    let d = engine.evaluate(
        "write_file",
        &serde_json::json!({"path": "/home/user/test.txt"}),
    );
    assert!(!d.requires_approval); // YELLOW auto-executes with notify
    assert!(!d.blocked);

    // RED: system mutation
    let d = engine.evaluate(
        "delete_file",
        &serde_json::json!({"path": "/home/user/test.txt"}),
    );
    assert!(d.requires_approval);
    assert!(!d.blocked);

    // BLACK: dangerous pattern (mkfs matches blacklist)
    let d = engine.evaluate(
        "execute_bash",
        &serde_json::json!({"command": "mkfs.ext4 /dev/sda"}),
    );
    assert!(d.blocked);
}

#[test]
fn tool_registry_generates_function_schemas() {
    let reg = registry::build_default_registry();
    let schemas = reg.function_schemas("standard");

    assert!(!schemas.is_empty(), "should produce schemas for LLM");

    // Each schema should have the OpenAI function-calling shape
    for schema in &schemas {
        assert!(
            schema.get("function").is_some(),
            "schema missing 'function' key"
        );
        let func = &schema["function"];
        assert!(func.get("name").is_some(), "function missing 'name'");
        assert!(
            func.get("description").is_some(),
            "function missing 'description'"
        );
        assert!(
            func.get("parameters").is_some(),
            "function missing 'parameters'"
        );
    }
}

#[test]
fn system_prompt_includes_tools_and_datetime() {
    let prompt = kria_core::agent::prompts::build_system_prompt(
        "### get_cpu_usage\nGet current CPU usage.\nParameters:\n  (none)",
        "TestUser",
        "linux",
        "standard",
        "apt (also available: snap)",
        "- User prefers dark theme",
    );

    assert!(prompt.contains("K.R.I.A."), "should mention KRIA");
    assert!(prompt.contains("TestUser"), "should include user name");
    assert!(prompt.contains("linux"), "should include OS name");
    assert!(prompt.contains("standard"), "should include hardware tier");
    assert!(
        prompt.contains("get_cpu_usage"),
        "should include tool descriptions"
    );
    assert!(
        prompt.contains("dark theme"),
        "should include memory context"
    );
    assert!(
        prompt.contains("Current Date/Time:"),
        "should include date/time"
    );
    assert!(
        prompt.contains("<tool_call>"),
        "should include tool call format"
    );
}

#[tokio::test]
async fn agent_loop_blocks_tool_above_hardware_tier() {
    let (api_url, server_handle) = spawn_mock_chat_server(vec![
        mock_tool_call_response("install_package", serde_json::json!({"name": "curl"})),
        mock_text_response("done"),
    ]);

    let mut config = kria_core::config::KriaConfig::default();
    config.llm.local_api_url = api_url;
    config.llm.active_model = "mock-model".into();
    config.llm.routing_mode = "local".into();

    let model_router = Arc::new(ModelRouter::from_config(&config));
    let tool_registry = Arc::new(registry::build_default_registry());
    let policy_engine = Arc::new(PolicyEngine::new());
    let hitl = Arc::new(HitlGateway::new(3));

    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("test_audit_tier.db");
    let audit_conn = rusqlite::Connection::open(&db_path).unwrap();
    let audit_logger = Arc::new(AuditLogger::new(audit_conn));
    let rollback_mgr = Arc::new(RollbackManager::new(
        tmp.path().join("rollback_tier"),
        1,
        10,
    ));

    let mount_mgr = Arc::new(tokio::sync::RwLock::new(
        kria_core::tools::mount_manager::ToolMountManager::new(),
    ));

    let agent_loop = AgentLoop::new(
        model_router,
        tool_registry,
        mount_mgr,
        policy_engine,
        hitl,
        audit_logger,
        rollback_mgr,
    )
    .with_hardware_tier("lite")
    .with_max_tool_rounds(3);

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
            content: "please continue".into(),
            name: None,
            images: None,
        },
    ];

    agent_loop.run("test-session-tier", &mut messages, tx).await;
    let events = collect_events(rx).await;

    let tool_end = events.iter().find_map(|e| match e {
        StreamEvent::ToolEnd {
            name,
            result,
            success,
        } if name == "install_package" => Some((result.clone(), *success)),
        _ => None,
    });
    assert!(tool_end.is_some(), "expected ToolEnd for install_package");
    let (result, success) = tool_end.unwrap();
    assert!(!success, "install_package should be blocked on lite tier");
    let err = result["error"].as_str().unwrap_or_default();
    assert!(
        err.contains("not available for current hardware tier 'lite'"),
        "unexpected error: {err}"
    );

    let has_approval_prompt = events
        .iter()
        .any(|e| matches!(e, StreamEvent::ApprovalRequired { .. }));
    assert!(
        !has_approval_prompt,
        "tier-gated tool should not reach HITL approval"
    );

    let has_done = events.iter().any(|e| matches!(e, StreamEvent::Done(_)));
    assert!(has_done, "loop should complete with Done event");

    let _ = server_handle.join();
}

#[tokio::test]
async fn agent_loop_blocks_unmounted_tool_even_if_tier_allows_it() {
    let (api_url, server_handle) = spawn_mock_chat_server(vec![
        mock_tool_call_response("get_cpu_usage", serde_json::json!({})),
        mock_text_response("done"),
    ]);

    let mut config = kria_core::config::KriaConfig::default();
    config.llm.local_api_url = api_url;
    config.llm.active_model = "mock-model".into();
    config.llm.routing_mode = "local".into();

    let model_router = Arc::new(ModelRouter::from_config(&config));
    let tool_registry = Arc::new(registry::build_default_registry());
    let policy_engine = Arc::new(PolicyEngine::new());
    let hitl = Arc::new(HitlGateway::new(3));

    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("test_audit_mount.db");
    let audit_conn = rusqlite::Connection::open(&db_path).unwrap();
    let audit_logger = Arc::new(AuditLogger::new(audit_conn));
    let rollback_mgr = Arc::new(RollbackManager::new(
        tmp.path().join("rollback_mount"),
        1,
        10,
    ));

    let mut mount = kria_core::tools::mount_manager::ToolMountManager::new();
    mount.define_group("hidden_sys", vec!["get_cpu_usage".into()], false);
    let mount_mgr = Arc::new(tokio::sync::RwLock::new(mount));

    let agent_loop = AgentLoop::new(
        model_router,
        tool_registry,
        mount_mgr,
        policy_engine,
        hitl,
        audit_logger,
        rollback_mgr,
    )
    .with_hardware_tier("standard")
    .with_max_tool_rounds(3);

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
            content: "show cpu usage".into(),
            name: None,
            images: None,
        },
    ];

    agent_loop
        .run("test-session-mount", &mut messages, tx)
        .await;
    let events = collect_events(rx).await;

    let tool_end = events.iter().find_map(|e| match e {
        StreamEvent::ToolEnd {
            name,
            result,
            success,
        } if name == "get_cpu_usage" => Some((result.clone(), *success)),
        _ => None,
    });
    assert!(tool_end.is_some(), "expected ToolEnd for get_cpu_usage");
    let (result, success) = tool_end.unwrap();
    assert!(!success, "unmounted tool should be blocked");
    let err = result["error"].as_str().unwrap_or_default();
    assert!(
        err.contains("mounted tool groups"),
        "unexpected error: {err}"
    );

    let has_done = events.iter().any(|e| matches!(e, StreamEvent::Done(_)));
    assert!(has_done, "loop should complete with Done event");

    let _ = server_handle.join();
}
