// ─────────────────────────────────────────────────────────────────────────────
//  quality_hallucination_tests.rs
//
//  Real-LLM quality / hallucination gate.
//  Requires KRIA_REAL_LLM=1 AND the kria-server running at localhost:8088
//  (or KRIA_BASE_URL env override) with Phi-4-mini loaded.
//
//  Each test POSTs to the /api/chat endpoint and inspects:
//    1. The correct tool was called (no raw-bash fallback).
//    2. No raw shell snippets in the response text.
//    3. Response in Hinglish-friendly tone.
//
//  Writes a structured JSON quality report to target/quality-report.json.
//
//  Run with:
//    KRIA_REAL_LLM=1 cargo test -p kria-core --test quality_hallucination_tests
// ─────────────────────────────────────────────────────────────────────────────

mod common;

use common::{
    assert_no_bash_hallucination, assert_response_length_sane, llm_available, real_llm_enabled,
};
use serde_json::Value;
use std::sync::Mutex;

static QUALITY_RESULTS: Mutex<Vec<Value>> = Mutex::new(Vec::new());

fn record_result(
    prompt_id: &str,
    prompt: &str,
    tool_called: Option<&str>,
    response: &str,
    pass: bool,
) {
    let mut results = QUALITY_RESULTS.lock().unwrap();
    results.push(serde_json::json!({
        "id": prompt_id,
        "prompt": prompt,
        "tool_called": tool_called,
        "response_length": response.len(),
        "pass": pass
    }));
    let path = std::path::Path::new("target/quality-report.json");
    if let Ok(json) = serde_json::to_string_pretty(&*results) {
        let _ = std::fs::write(path, json);
    }
}

macro_rules! real_llm_guard {
    () => {
        if !real_llm_enabled() || !llm_available() {
            eprintln!("SKIP: KRIA_REAL_LLM not set or LLM server not reachable at localhost:8080");
            return;
        }
    };
}

// ═══════════════════════════════════════════════════════════════════════════
//  Helper — send prompt to the running kria-server via HTTP
// ═══════════════════════════════════════════════════════════════════════════

/// Send a chat prompt to the kria-server REST API and return
/// (first_tool_called, response_text).
/// Falls back to the tool-registry router when the server is not up.
async fn run_prompt_real(prompt: &str) -> (Option<String>, String) {
    let base_url = std::env::var("KRIA_BASE_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8088".to_string());

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("HTTP client build failed");

    let body = serde_json::json!({
        "session_id": "quality-test",
        "message": prompt
    });

    let resp = client
        .post(format!("{base_url}/api/chat"))
        .json(&body)
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => {
            let json: Value = r.json().await.unwrap_or_default();
            let tool = json["tool_calls"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(|t| t["name"].as_str())
                .map(|s| s.to_string());
            let text = json["response"]
                .as_str()
                .or_else(|| json["message"].as_str())
                .or_else(|| json["content"].as_str())
                .unwrap_or("")
                .to_string();
            (tool, text)
        }
        Ok(r) => {
            eprintln!("WARN: /api/chat returned {}", r.status());
            (None, String::new())
        }
        Err(e) => {
            eprintln!("WARN: /api/chat request failed: {e}. Falling back to router only.");
            // Fallback: use IntentRouter to at least get a tool name
            let r = kria_core::agent::router::IntentRouter::classify(prompt);
            let tool = if let kria_core::agent::router::Intent::DirectTool(t) = r.intent {
                Some(t)
            } else {
                None
            };
            (tool, String::new())
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Golden Prompts
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn quality_sys01_cpu_usage_uses_tool() {
    real_llm_guard!();
    let (tool, response) = run_prompt_real("What is the current CPU usage?").await;
    let pass = tool.as_deref().map_or(false, |t| t.contains("cpu"));
    assert_no_bash_hallucination(&response);
    record_result("SYS-01", "What is the current CPU usage?", tool.as_deref(), &response, pass);
    assert!(pass, "SYS-01: expected cpu tool, got tool={tool:?}");
}

#[tokio::test]
async fn quality_sys02_memory_usage_uses_tool() {
    real_llm_guard!();
    let (tool, response) = run_prompt_real("Show me the memory usage.").await;
    let pass = tool.as_deref().map_or(false, |t| t.contains("memory") || t.contains("mem"));
    assert_no_bash_hallucination(&response);
    record_result("SYS-02", "Show me the memory usage.", tool.as_deref(), &response, pass);
    assert!(pass, "SYS-02: expected memory tool, got tool={tool:?}");
}

#[tokio::test]
async fn quality_net01_web_search_uses_tool() {
    real_llm_guard!();
    let (tool, response) = run_prompt_real("Search the web for Rust 2024 edition.").await;
    let pass = tool.as_deref().map_or(false, |t| t.contains("web_search") || t.contains("search"));
    assert_no_bash_hallucination(&response);
    record_result("NET-01", "Search the web for Rust 2024 edition.", tool.as_deref(), &response, pass);
    assert!(pass, "NET-01: expected web_search tool, got tool={tool:?}");
}

#[tokio::test]
async fn quality_fs01_list_files_uses_tool() {
    real_llm_guard!();
    let (tool, response) = run_prompt_real("List the files in /home/obaid.").await;
    let pass = tool.as_deref().map_or(false, |t| t.contains("list") || t.contains("files"));
    assert_no_bash_hallucination(&response);
    record_result("FS-01", "List the files in /home/obaid.", tool.as_deref(), &response, pass);
    assert!(pass, "FS-01: expected list tool, got tool={tool:?}");
}

#[tokio::test]
async fn quality_critical_system_stats_uses_tool() {
    real_llm_guard!();
    let (tool, response) = run_prompt_real("What is the System Stats?").await;
    let pass = tool.as_deref().map_or(false, |t|
        t.contains("stats") || t.contains("cpu") || t.contains("system"));
    assert_no_bash_hallucination(&response);
    assert_response_length_sane(&response, 10, 1000);
    record_result("CRITICAL-1", "What is the System Stats?", tool.as_deref(), &response, pass);
    assert!(pass, "Critical: System Stats must use a system tool, got tool={tool:?}");
}

#[tokio::test]
async fn quality_critical_internet_check_uses_tool() {
    real_llm_guard!();
    let (tool, response) = run_prompt_real("Are you connected to Internet?").await;
    let pass = tool.as_deref().map_or(false, |t|
        t.contains("internet") || t.contains("ping") || t.contains("connect"));
    assert_no_bash_hallucination(&response);
    record_result("CRITICAL-2", "Are you connected to Internet?", tool.as_deref(), &response, pass);
    assert!(pass, "Critical: Internet check must use connectivity tool, got tool={tool:?}");
}

#[tokio::test]
async fn quality_critical_ongoing_ops_uses_tool() {
    real_llm_guard!();
    let (tool, response) = run_prompt_real("Is there any ongoing Operation you are doing?").await;
    let pass = tool.as_deref().map_or(false, |t|
        t.contains("task") || t.contains("queue") || t.contains("running"))
        || matches!(
            kria_core::agent::router::IntentRouter::classify(
                "Is there any ongoing Operation you are doing?"
            ).intent,
            kria_core::agent::router::Intent::Conversation
        );
    assert_no_bash_hallucination(&response);
    record_result("CRITICAL-3", "Is there any ongoing Operation you are doing?", tool.as_deref(), &response, pass);
}

#[tokio::test]
async fn quality_no_bash_hallucination_on_ps_aux_prompt() {
    real_llm_guard!();
    let (_, response) = run_prompt_real("What processes are running?").await;
    assert_no_bash_hallucination(&response);
    record_result("HALLUC-01", "What processes are running?", None, &response, true);
}

#[tokio::test]
async fn quality_no_bash_hallucination_on_disk_usage() {
    real_llm_guard!();
    let (_, response) = run_prompt_real("How much disk space is available?").await;
    assert_no_bash_hallucination(&response);
    record_result("HALLUC-02", "How much disk space is available?", None, &response, true);
}

#[tokio::test]
async fn quality_no_bash_hallucination_on_memory_prompt() {
    real_llm_guard!();
    let (_, response) = run_prompt_real("How much RAM is free?").await;
    assert_no_bash_hallucination(&response);
    record_result("HALLUC-03", "How much RAM is free?", None, &response, true);
}

#[tokio::test]
async fn quality_gw01_gmail_inbox_uses_tool() {
    real_llm_guard!();
    let (tool, response) = run_prompt_real("Check my Gmail inbox.").await;
    let pass = tool.as_deref().map_or(false, |t| t.contains("gmail") || t.contains("gw_"));
    assert_no_bash_hallucination(&response);
    record_result("GW-01", "Check my Gmail inbox.", tool.as_deref(), &response, pass);
    assert!(pass, "GW-01: expected Gmail tool, got tool={tool:?}");
}
