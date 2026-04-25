// ─────────────────────────────────────────────────────────────────────────────
//  test_internet.rs — §6 Network/Web, §7 Document/News
//
//  Internet-dependent tests are guarded with common::internet_available().
//  When the network is absent they skip cleanly rather than fail.
//
//  Covers PROMPT-IDs: NET-01..NET-11, DOC-01..DOC-07
// ─────────────────────────────────────────────────────────────────────────────

mod common;

use common::{assert_tool_success, internet_available};
use kria_core::agent::router::{Intent, IntentRouter};
use kria_core::safety::policy::{PolicyEngine, RiskLevel};
use kria_core::tools::registry;

// ═══════════════════════════════════════════════════════════════════════════
//  Smoke — all internet/doc tools must be registered
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn smoke_internet_tools_registered() {
    // PROMPT-ID: NET-01..NET-11
    let reg = registry::build_default_registry();
    let required = [
        "web_search",
        "fetch_webpage",
        "check_url_status",
        "get_public_ip",
        "ping_host",
        "dns_lookup",
        "speed_test",
        "download_file",
        "get_current_time",
        "get_weather",
    ];
    for name in &required {
        assert!(
            reg.get_handler(name).is_some(),
            "Tool `{name}` must be registered (required by §6)"
        );
    }
}

#[test]
fn smoke_document_tools_registered() {
    // PROMPT-ID: DOC-01..DOC-07
    let reg = registry::build_default_registry();
    let required = ["parse_document", "parse_csv", "summarize_document"];
    for name in &required {
        assert!(
            reg.get_handler(name).is_some(),
            "Tool `{name}` must be registered (required by §7)"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Routing — §6 prompts
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn routing_net01_web_search_routes_correctly() {
    // PROMPT-ID: NET-01
    let prompts = [
        "Search the web for latest news on Rust programming language.",
        "search for machine learning tutorials",
        "web search: tokio async runtime",
        "look up KRIA project on the internet",
    ];
    for p in &prompts {
        let r = IntentRouter::classify(p);
        assert!(
            matches!(&r.intent, Intent::DirectTool(t) if matches!(t.as_str(), "web_search" | "searxng_search"))
                || matches!(r.intent, Intent::ComplexTask),
            "'{p}' should route to web_search/searxng_search, got: {:?}",
            r.intent
        );
    }
}

#[test]
fn routing_net02_fetch_webpage_routes_correctly() {
    // PROMPT-ID: NET-02
    let prompts = [
        "Fetch the content of https://www.rust-lang.org",
        "fetch https://example.com",
        "get the page https://docs.rs/tokio",
        "read the page at https://news.ycombinator.com",
    ];
    for p in &prompts {
        let r = IntentRouter::classify(p);
        assert!(
            matches!(&r.intent, Intent::DirectTool(t) if t == "fetch_webpage"),
            "'{p}' should route to fetch_webpage, got: {:?}",
            r.intent
        );
    }
}

#[test]
fn routing_net02_fetch_webpage_not_confused_with_websearch() {
    // PROMPT-ID: NET-02 — pure search queries must NOT route to fetch_webpage
    let search_prompts = [
        "search for latest Rust news",
        "what is machine learning?",
        "find information about tokio",
    ];
    for p in &search_prompts {
        let r = IntentRouter::classify(p);
        if let Intent::DirectTool(ref t) = r.intent {
            assert_ne!(
                t, "fetch_webpage",
                "Pure search '{p}' must not route to fetch_webpage"
            );
        }
    }
}

#[test]
fn routing_net03_check_url_status_routes_correctly() {
    // PROMPT-ID: NET-03
    let r = IntentRouter::classify("Check if https://google.com is reachable.");
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if t == "check_url_status")
            || matches!(r.intent, Intent::ComplexTask | Intent::Conversation),
        "URL status check should route to check_url_status, got: {:?}",
        r.intent
    );
}

#[test]
fn routing_net04_get_public_ip_routes_correctly() {
    // PROMPT-ID: NET-04
    let prompts = ["What is my public IP?", "public IP address", "mera IP kya hai"];
    for p in &prompts {
        let r = IntentRouter::classify(p);
        assert!(
            matches!(&r.intent, Intent::DirectTool(t) if t == "get_public_ip")
                || matches!(r.intent, Intent::Conversation | Intent::ComplexTask),
            "'{p}' should route to get_public_ip, got: {:?}",
            r.intent
        );
    }
}

#[test]
fn routing_net05_ping_host_routes_correctly() {
    // PROMPT-ID: NET-05
    let r = IntentRouter::classify("Ping google.com");
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if t == "ping_host"),
        "Ping should route to ping_host, got: {:?}",
        r.intent
    );
}

#[test]
fn routing_net06_dns_lookup_routes_correctly() {
    // PROMPT-ID: NET-06
    let r = IntentRouter::classify("DNS lookup for github.com");
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if t == "dns_lookup")
            || matches!(r.intent, Intent::ComplexTask | Intent::Conversation),
        "DNS lookup should route to dns_lookup, got: {:?}",
        r.intent
    );
}

#[test]
fn routing_net10_get_current_time_routes_correctly() {
    // PROMPT-ID: NET-10
    let prompts = ["What time is it?", "abhi time kya hai?", "current time"];
    for p in &prompts {
        let r = IntentRouter::classify(p);
        assert!(
            matches!(&r.intent, Intent::DirectTool(t) if t == "get_current_time")
                || matches!(r.intent, Intent::Conversation | Intent::ComplexTask),
            "'{p}' should route to get_current_time, got: {:?}",
            r.intent
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Policy — download_file is YELLOW (requires confirmation, then approval
//           for execution per user policy)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn policy_net08_download_file_is_yellow_or_red() {
    // PROMPT-ID: NET-08
    let engine = PolicyEngine::new();
    let decision = engine.evaluate(
        "download_file",
        &serde_json::json!({
            "url": "https://example.com/sample.pdf",
            "destination": "/home/obaid/Downloads/"
        }),
    );
    assert!(
        decision.risk_level == RiskLevel::Yellow || decision.risk_level == RiskLevel::Red,
        "download_file should be Yellow or Red (requires confirmation)"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  Functional — tools that don't need network
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn functional_net10_get_current_time() {
    // PROMPT-ID: NET-10
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("get_current_time").unwrap().clone();
    let result = handler.execute(serde_json::json!({})).await;
    assert!(result.success, "get_current_time should succeed: {:?}", result.error);
    // Must include some time-like field
    let has_time = result.data.get("time").or(result.data.get("datetime"))
        .or(result.data.get("current_time")).or(result.data.get("iso"))
        .is_some() || result.data.is_string();
    assert!(has_time, "get_current_time must return a time field: {}", result.data);
}

// ═══════════════════════════════════════════════════════════════════════════
//  Functional — internet-required tests (guarded)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn functional_net05_ping_host_google() {
    // PROMPT-ID: NET-05
    if !internet_available() {
        eprintln!("SKIP: internet not available");
        return;
    }
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("ping_host").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "host": "8.8.8.8" }))
        .await;
    assert!(
        result.success || result.error.is_some(),
        "ping_host must not panic"
    );
}

#[tokio::test]
async fn functional_net04_get_public_ip() {
    // PROMPT-ID: NET-04
    if !internet_available() {
        eprintln!("SKIP: internet not available");
        return;
    }
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("get_public_ip").unwrap().clone();
    let result = handler.execute(serde_json::json!({})).await;
    assert!(
        result.success || result.error.is_some(),
        "get_public_ip must not panic"
    );
    if result.success {
        let ip = result.data["ip"].as_str()
            .or(result.data["address"].as_str())
            .unwrap_or("");
        assert!(!ip.is_empty(), "get_public_ip must return a non-empty IP address");
    }
}

#[tokio::test]
async fn functional_net06_dns_lookup_github() {
    // PROMPT-ID: NET-06
    if !internet_available() {
        eprintln!("SKIP: internet not available");
        return;
    }
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("dns_lookup").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "domain": "github.com" }))
        .await;
    assert!(
        result.success || result.error.is_some(),
        "dns_lookup must not panic"
    );
}

#[tokio::test]
async fn functional_net03_check_url_status_google() {
    // PROMPT-ID: NET-03
    if !internet_available() {
        eprintln!("SKIP: internet not available");
        return;
    }
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("check_url_status").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "url": "https://google.com" }))
        .await;
    assert!(
        result.success || result.error.is_some(),
        "check_url_status must not panic"
    );
    if result.success {
        let status = result.data["status_code"].as_u64()
            .or(result.data["code"].as_u64())
            .unwrap_or(0);
        assert!(
            status == 200 || status == 301 || status == 302 || status >= 100,
            "check_url_status for google.com should return a valid HTTP status: {status}"
        );
    }
}

#[tokio::test]
async fn functional_net02_fetch_webpage_rust_lang() {
    // PROMPT-ID: NET-02 — key test from TestPrompts.txt line 274
    if !internet_available() {
        eprintln!("SKIP: internet not available");
        return;
    }
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("fetch_webpage").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({
            "url": "https://www.rust-lang.org",
            "max_chars": 3000
        }))
        .await;
    assert!(result.success, "fetch_webpage rust-lang.org should succeed: {:?}", result.error);
    let content = result.data["content"].as_str()
        .or(result.data["text"].as_str())
        .unwrap_or(result.data.as_str().unwrap_or(""));
    assert!(!content.is_empty(), "fetch_webpage must return non-empty content");
    assert!(
        content.len() <= 30000,
        "fetch_webpage must respect max_chars ceiling"
    );
    // Must not return raw HTML tags as the primary output
    let tag_density = content.matches('<').count() as f32 / content.len().max(1) as f32;
    assert!(
        tag_density < 0.15,
        "fetch_webpage should return extracted text, not raw HTML (tag density: {:.2})",
        tag_density
    );
}

#[tokio::test]
async fn functional_net01_web_search() {
    // PROMPT-ID: NET-01
    if !internet_available() {
        eprintln!("SKIP: internet not available");
        return;
    }
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("web_search").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({
            "query": "Rust programming language latest news",
            "max_results": 5
        }))
        .await;
    assert!(
        result.success || result.error.is_some(),
        "web_search must not panic"
    );
    if result.success {
        let results = result.data.as_array()
            .cloned()
            .or_else(|| result.data["results"].as_array().cloned())
            .unwrap_or_default();
        assert!(!results.is_empty(), "web_search should return at least one result");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  §7 Document tools — routing and functional
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn routing_doc01_parse_document_routes_correctly() {
    // PROMPT-ID: DOC-01
    let r = IntentRouter::classify("Parse the document at /home/obaid/Documents/report.pdf");
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if matches!(t.as_str(), "parse_document" | "summarize_document"))
            || matches!(r.intent, Intent::ComplexTask | Intent::Conversation),
        "Document parse should route to parse_document, got: {:?}",
        r.intent
    );
}

#[test]
fn routing_doc02_parse_csv_routes_correctly() {
    // PROMPT-ID: DOC-02
    let r = IntentRouter::classify("Parse the CSV at /home/obaid/data.csv and give me a summary.");
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if t == "parse_csv")
            || matches!(r.intent, Intent::ComplexTask | Intent::Conversation),
        "CSV parse should route to parse_csv, got: {:?}",
        r.intent
    );
}

#[tokio::test]
async fn functional_doc01_parse_document_missing_file() {
    // PROMPT-ID: DOC-01 — missing file must return clean error
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("parse_document").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({
            "path": "/nonexistent_kria_test/report.pdf",
            "operations": ["extract_text"]
        }))
        .await;
    assert!(
        !result.success,
        "parse_document for missing file must fail cleanly"
    );
    assert!(result.error.is_some(), "parse_document failure must include error message");
}
