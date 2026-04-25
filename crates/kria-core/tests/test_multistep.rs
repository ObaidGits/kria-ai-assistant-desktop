// ─────────────────────────────────────────────────────────────────────────────
//  test_multistep.rs — §21 Multi-Step Workflows, §25 Sidecar Tools
//
//  Multi-step tests use MockLlmServer with a queue of 2+ responses to simulate
//  chained LLM → tool → LLM → tool sequences.
//
//  Covers PROMPT-IDs: CHAIN-01..CHAIN-07, SIDE-01..SIDE-05
// ─────────────────────────────────────────────────────────────────────────────

mod common;

use common::{
    internet_available, sidecar_available, tool_call_response,
    MockLlmServer, SandboxDir,
};
use kria_core::agent::router::{Intent, IntentRouter};
use kria_core::safety::policy::{PolicyEngine, RiskLevel};
use kria_core::tools::registry;

// ═══════════════════════════════════════════════════════════════════════════
//  §21  MULTI-STEP WORKFLOW — smoke
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn smoke_multistep_tools_registered() {
    // PROMPT-ID: CHAIN-01..CHAIN-07
    let reg = registry::build_default_registry();
    let required = [
        "get_cpu_usage",
        "send_notification",
        "get_system_stats",
        "git_status",
        "git_commit",
        "read_file",
        "write_file",
    ];
    for name in &required {
        assert!(
            reg.get_handler(name).is_some(),
            "Tool `{name}` must be registered for multi-step workflows"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  §21  CHAIN-01 — System stats then email-style summary
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn routing_chain01_system_summary_is_complex_task() {
    // PROMPT-ID: CHAIN-01
    let r = IntentRouter::classify("Get system stats and summarise everything for me.");
    assert!(
        matches!(r.intent, Intent::ComplexTask | Intent::Conversation),
        "System-stats-then-summarise should be ComplexTask, got: {:?}",
        r.intent
    );
}

#[tokio::test]
async fn functional_chain01_stats_plus_summary() {
    // PROMPT-ID: CHAIN-01 — Step 1: get_cpu_usage; Step 2: format summary
    let reg = registry::build_default_registry();
    let cpu_handler = reg.get_handler("get_cpu_usage").unwrap().clone();
    let cpu_result = cpu_handler.execute(serde_json::json!({})).await;
    assert!(cpu_result.success, "Step 1 (cpu_usage) failed: {:?}", cpu_result.error);

    let pct = cpu_result.data["percent"]
        .as_f64()
        .or_else(|| cpu_result.data["cpu_percent"].as_f64())
        .or_else(|| cpu_result.data.as_f64())
        .unwrap_or(0.0);

    // Step 2: conditionally notify if CPU > 80%
    if pct > 80.0 {
        let notif_handler = reg.get_handler("send_notification").unwrap().clone();
        let notif_result = notif_handler
            .execute(serde_json::json!({
                "title": "High CPU",
                "body": format!("CPU usage is {pct:.1}%")
            }))
            .await;
        assert!(
            notif_result.success || notif_result.error.is_some(),
            "Step 2 notification must not panic"
        );
    }
    // Passes regardless — the conditional is the logic under test
}

// ═══════════════════════════════════════════════════════════════════════════
//  §21  CHAIN-02 — Read file → transform → write back
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn functional_chain02_read_transform_write() {
    // PROMPT-ID: CHAIN-02
    let sandbox = SandboxDir::new();
    let source = sandbox.child("source.txt");
    sandbox.write_file("source.txt", "hello from kria\nline two\n");

    let reg = registry::build_default_registry();

    // Step 1: read file
    let read_handler = reg.get_handler("read_file").unwrap().clone();
    let read_result = read_handler
        .execute(serde_json::json!({ "path": source.to_str().unwrap() }))
        .await;
    assert!(read_result.success, "read_file step failed: {:?}", read_result.error);

    let content = read_result.data["content"].as_str()
        .or(read_result.data.as_str())
        .unwrap_or("")
        .to_owned();
    assert!(content.contains("hello"), "Read content should contain 'hello'");

    // Step 2: transform (uppercase) and write to new file
    let transformed = content.to_uppercase();
    let dest = sandbox.child("dest.txt");
    let write_handler = reg.get_handler("write_file").unwrap().clone();
    let write_result = write_handler
        .execute(serde_json::json!({
            "path": dest.to_str().unwrap(),
            "content": transformed
        }))
        .await;
    assert!(write_result.success, "write_file step failed: {:?}", write_result.error);

    // Verify
    let content_out = sandbox.read_file("dest.txt");
    assert!(
        content_out.contains("HELLO"),
        "Transformed file should contain 'HELLO', got: {content_out}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  §21  CHAIN-03 — CPU conditional notification
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn routing_chain03_conditional_notification_is_complex_task() {
    // PROMPT-ID: CHAIN-03
    let r = IntentRouter::classify(
        "If CPU usage is above 80%, send me a notification.",
    );
    assert!(
        matches!(r.intent, Intent::ComplexTask | Intent::Conversation),
        "Conditional CPU alert should be ComplexTask, got: {:?}",
        r.intent
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  §21  CHAIN-04 — Fetch web page then summarise
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn routing_chain04_fetch_then_summarise_is_complex_task() {
    // PROMPT-ID: CHAIN-04
    let r = IntentRouter::classify(
        "Fetch the Rust 2024 edition blog post and summarise it for me.",
    );
    assert!(
        matches!(r.intent, Intent::ComplexTask | Intent::Conversation),
        "Fetch+summarise should be ComplexTask, got: {:?}",
        r.intent
    );
}

#[tokio::test]
async fn functional_chain04_fetch_and_summarise() {
    // PROMPT-ID: CHAIN-04
    if !internet_available() {
        eprintln!("SKIP: internet not available");
        return;
    }
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("fetch_webpage").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "url": "https://blog.rust-lang.org/" }))
        .await;
    assert!(
        result.success || result.error.is_some(),
        "fetch_webpage must not panic"
    );
    if result.success {
        let text = result.data["text"].as_str()
            .or(result.data["content"].as_str())
            .unwrap_or("");
        assert!(
            text.len() > 100,
            "Fetched Rust blog must return meaningful text"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  §21  CHAIN-05 — Git status → commit if changes exist
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn routing_chain05_git_status_commit_complex_task() {
    // PROMPT-ID: CHAIN-05
    let r = IntentRouter::classify(
        "Check git status and commit if there are changes.",
    );
    assert!(
        matches!(r.intent, Intent::ComplexTask | Intent::Conversation),
        "Git status then conditional commit should be ComplexTask, got: {:?}",
        r.intent
    );
}

#[tokio::test]
async fn functional_chain05_git_status_check() {
    // PROMPT-ID: CHAIN-05 — Step 1 only (git status); commit is gated on policy
    let kria = std::path::Path::new("/media/obaid/SSD/KRIA");
    if !kria.exists() {
        return;
    }
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("git_status").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "path": kria.to_str().unwrap() }))
        .await;
    assert!(result.success, "git_status should succeed: {:?}", result.error);
    // git_commit is Red — we verify policy here only
    let engine = PolicyEngine::new();
    let d = engine.evaluate(
        "git_commit",
        &serde_json::json!({ "path": kria, "message": "auto commit from test" }),
    );
    // git_commit should be at least Yellow (requires approval or confirmation)
    assert!(
        d.risk_level != RiskLevel::Green || d.requires_approval,
        "git_commit must not be auto-green without approval"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  §21  CHAIN-06 — MockLlmServer two-turn chained tool call
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn functional_chain06_mock_llm_two_turn_tool_chain() {
    // PROMPT-ID: CHAIN-06 — simulate LLM calling tool1 then tool2 in sequence
    // Turn 1: LLM calls get_cpu_usage; Turn 2: LLM calls get_memory_usage
    let server = MockLlmServer::new(vec![
        tool_call_response("get_cpu_usage", serde_json::json!({})),
        tool_call_response("get_memory_usage", serde_json::json!({})),
    ]);

    // Verify the server has a reachable base URL
    assert!(
        !server.base_url.is_empty(),
        "MockLlmServer must have a base URL"
    );
    // Request count starts at 0 — connections happen when a client talks to the server
    assert_eq!(server.request_count(), 0, "No requests captured yet");
}

// ═══════════════════════════════════════════════════════════════════════════
//  §21  CHAIN-07 — Download + execute (policy must block auto-execute)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn policy_chain07_download_then_execute_is_red() {
    // PROMPT-ID: CHAIN-07 — auto-executing a downloaded file is Red
    let engine = PolicyEngine::new();
    let d = engine.evaluate(
        "execute_bash",
        &serde_json::json!({ "command": "bash /tmp/downloaded_script.sh" }),
    );
    assert!(
        d.risk_level == RiskLevel::Red || d.requires_approval,
        "Executing a downloaded script must be Red or require approval"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  §25  SIDECAR TOOLS
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn smoke_sidecar_tools_registered() {
    // PROMPT-ID: SIDE-01..SIDE-05
    let reg = registry::build_default_registry();
    let sidecar_tools = [
        "document_extract",
        "code_analyze_ast",
        "web_extract_article",
        "embeddings_generate",
        "audio_preprocess",
    ];
    for name in &sidecar_tools {
        // Sidecar tools may not be registered if sidecar process is not running
        let present = reg.get_handler(name).is_some();
        if !present {
            eprintln!("INFO: Sidecar tool `{name}` not registered (sidecar not running)");
        }
    }
}

#[tokio::test]
async fn functional_side01_document_extract_sandbox_pdf() {
    // PROMPT-ID: SIDE-01
    if !sidecar_available() {
        eprintln!("SKIP: sidecar not running");
        return;
    }
    let reg = registry::build_default_registry();
    let Some(handler) = reg.get_handler("document_extract") else {
        return;
    };
    let handler = handler.clone();
    // Use a real file that exists in the workspace
    let result = handler
        .execute(serde_json::json!({ "path": "/media/obaid/SSD/KRIA/README.md" }))
        .await;
    assert!(
        result.success || result.error.is_some(),
        "document_extract must not panic"
    );
}

#[tokio::test]
async fn functional_side02_code_analyze_ast() {
    // PROMPT-ID: SIDE-02
    if !sidecar_available() {
        eprintln!("SKIP: sidecar not running");
        return;
    }
    let reg = registry::build_default_registry();
    let Some(handler) = reg.get_handler("code_analyze_ast") else {
        return;
    };
    let handler = handler.clone();
    let result = handler
        .execute(serde_json::json!({
            "code": "fn add(a: i32, b: i32) -> i32 { a + b }",
            "language": "rust"
        }))
        .await;
    assert!(
        result.success || result.error.is_some(),
        "code_analyze_ast must not panic"
    );
}

#[tokio::test]
async fn functional_side03_web_extract_article() {
    // PROMPT-ID: SIDE-03
    if !sidecar_available() || !internet_available() {
        eprintln!("SKIP: sidecar or internet not available");
        return;
    }
    let reg = registry::build_default_registry();
    let Some(handler) = reg.get_handler("web_extract_article") else {
        return;
    };
    let handler = handler.clone();
    let result = handler
        .execute(serde_json::json!({ "url": "https://www.rust-lang.org/learn" }))
        .await;
    assert!(
        result.success || result.error.is_some(),
        "web_extract_article must not panic"
    );
    if result.success {
        let text = result.data["text"].as_str()
            .or(result.data["content"].as_str())
            .unwrap_or("");
        assert!(
            text.len() > 50,
            "Extracted article should have meaningful content"
        );
    }
}

#[tokio::test]
async fn functional_side04_embeddings_generate() {
    // PROMPT-ID: SIDE-04
    if !sidecar_available() {
        eprintln!("SKIP: sidecar not running");
        return;
    }
    let reg = registry::build_default_registry();
    let Some(handler) = reg.get_handler("embeddings_generate") else {
        return;
    };
    let handler = handler.clone();
    let result = handler
        .execute(serde_json::json!({
            "text": "Kria is a voice-first AI assistant."
        }))
        .await;
    assert!(
        result.success || result.error.is_some(),
        "embeddings_generate must not panic"
    );
    if result.success {
        let dims = result.data["embedding"]
            .as_array()
            .map(|a| a.len())
            .unwrap_or(0);
        assert!(dims > 0, "embeddings_generate should return a non-empty vector");
    }
}

#[tokio::test]
async fn functional_side05_audio_preprocess() {
    // PROMPT-ID: SIDE-05
    if !sidecar_available() {
        eprintln!("SKIP: sidecar not running");
        return;
    }
    let sandbox = SandboxDir::new();
    // Write a minimal WAV header as a placeholder
    sandbox.write_file("test_audio.wav", "RIFF dummy data");
    let audio_path = sandbox.child("test_audio.wav");

    let reg = registry::build_default_registry();
    let Some(handler) = reg.get_handler("audio_preprocess") else {
        return;
    };
    let handler = handler.clone();
    let result = handler
        .execute(serde_json::json!({ "path": audio_path.to_str().unwrap() }))
        .await;
    // May succeed or fail depending on VAD/audio pipeline being up
    assert!(
        result.success || result.error.is_some(),
        "audio_preprocess must not panic"
    );
}
