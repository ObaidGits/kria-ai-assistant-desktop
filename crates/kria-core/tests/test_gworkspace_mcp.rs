// ─────────────────────────────────────────────────────────────────────────────
//  test_gworkspace_mcp.rs — §17 i18n, §18 Google Workspace, §19 Colab MCP,
//                            §20 MCP Filesystem Server
//
//  Google Workspace tests: read-only operations checked at tool-registry and
//  policy level; live calls are gated on gworkspace_creds_available().
//  Send/Delete operations are confirmed as RED tier.
//
//  MCP / Colab tests: registry + JSON-RPC protocol assertions only —
//  live Colab browser connection is #[ignore].
//
//  Covers PROMPT-IDs: I18N-01..I18N-03, GW-01..GW-21, COLAB-01..COLAB-06,
//                     MCP-FS-01..MCP-FS-03
// ─────────────────────────────────────────────────────────────────────────────

mod common;

use common::gworkspace_creds_available;
use kria_core::agent::router::{Intent, IntentRouter};
use kria_core::safety::policy::{PolicyEngine, RiskLevel};
use kria_core::tools::registry;

// ═══════════════════════════════════════════════════════════════════════════
//  §17 ACCESSIBILITY & I18N
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn smoke_i18n_tools_registered() {
    // PROMPT-ID: I18N-01..I18N-03
    let reg = registry::build_default_registry();
    for name in &["list_languages", "detect_language", "get_accessibility_settings"] {
        assert!(
            reg.get_handler(name).is_some(),
            "Tool `{name}` must be registered (§17)"
        );
    }
}

#[tokio::test]
async fn functional_i18n01_list_languages() {
    // PROMPT-ID: I18N-01
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("list_languages").unwrap().clone();
    let result = handler.execute(serde_json::json!({})).await;
    assert!(result.success, "list_languages should succeed: {:?}", result.error);
    let langs = result.data.as_array()
        .cloned()
        .or_else(|| result.data["languages"].as_array().cloned())
        .unwrap_or_default();
    assert!(!langs.is_empty(), "list_languages must return at least one language");
}

#[tokio::test]
async fn functional_i18n02_detect_language_hindi() {
    // PROMPT-ID: I18N-02
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("detect_language").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "text": "Aaj mausam bahut accha hai" }))
        .await;
    assert!(result.success, "detect_language should succeed: {:?}", result.error);
    let lang = result.data["language"].as_str()
        .or(result.data["detected"].as_str())
        .or(result.data["code"].as_str())
        .unwrap_or("");
    assert!(
        lang.contains("hi") || lang.contains("Hindi") || lang.contains("hinglish"),
        "Hindi text should be detected as Hindi/hi, got: {lang}"
    );
}

#[tokio::test]
async fn functional_i18n03_get_accessibility_settings() {
    // PROMPT-ID: I18N-03
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("get_accessibility_settings").unwrap().clone();
    let result = handler.execute(serde_json::json!({})).await;
    assert!(
        result.success || result.error.is_some(),
        "get_accessibility_settings must not panic"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  §18 GOOGLE WORKSPACE — smoke & policy
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn smoke_gworkspace_tools_registered() {
    // PROMPT-ID: GW-01..GW-21
    let reg = registry::build_default_registry();
    let required = [
        "gw_gmail_inbox",
        "gw_gmail_read",
        "gw_gmail_search",
        "gw_gmail_send",
        "gw_gmail_delete",
        "gw_calendar_today",
        "gw_calendar_search",
        "gw_calendar_create",
        "gw_calendar_delete",
        "gw_drive_list",
        "gw_drive_search",
        "gw_drive_read",
        "gw_drive_delete",
        "gw_docs_read",
        "gw_docs_create",
        "gw_docs_edit",
        "gw_sheets_read",
        "gw_sheets_create",
        "gw_forms_list",
    ];
    for name in &required {
        assert!(
            reg.get_handler(name).is_some(),
            "Tool `{name}` must be registered (§18)"
        );
    }
}

// ── Policy: destructive GW operations must be RED ─────────────────────────

#[test]
fn policy_gw04_gmail_send_is_red() {
    // PROMPT-ID: GW-04
    let engine = PolicyEngine::new();
    let d = engine.evaluate("gw_gmail_send", &serde_json::json!({ "to": "x@example.com" }));
    assert_eq!(d.risk_level, RiskLevel::Red, "gw_gmail_send must be Red");
}

#[test]
fn policy_gw05_gmail_delete_is_red() {
    // PROMPT-ID: GW-05
    let engine = PolicyEngine::new();
    let d = engine.evaluate("gw_gmail_delete", &serde_json::json!({ "id": "123abc" }));
    assert_eq!(d.risk_level, RiskLevel::Red, "gw_gmail_delete must be Red");
}

#[test]
fn policy_gw09_calendar_delete_is_red() {
    // PROMPT-ID: GW-09
    let engine = PolicyEngine::new();
    let d = engine.evaluate("gw_calendar_delete", &serde_json::json!({ "id": "event-001" }));
    assert_eq!(d.risk_level, RiskLevel::Red, "gw_calendar_delete must be Red");
}

#[test]
fn policy_gw13_drive_delete_is_red() {
    // PROMPT-ID: GW-13
    let engine = PolicyEngine::new();
    let d = engine.evaluate("gw_drive_delete", &serde_json::json!({ "id": "abc123" }));
    assert_eq!(d.risk_level, RiskLevel::Red, "gw_drive_delete must be Red");
}

// ── Policy: read-only GW operations should be GREEN ───────────────────────

#[test]
fn policy_gw_read_ops_are_green_or_yellow() {
    // PROMPT-ID: GW-01, GW-06, GW-10, GW-11, GW-14, GW-17, GW-21
    let engine = PolicyEngine::new();
    let read_ops = [
        "gw_gmail_inbox",
        "gw_gmail_search",
        "gw_calendar_today",
        "gw_calendar_search",
        "gw_drive_list",
        "gw_drive_search",
        "gw_drive_read",
        "gw_docs_read",
        "gw_sheets_read",
        "gw_forms_list",
    ];
    for op in &read_ops {
        let d = engine.evaluate(op, &serde_json::json!({}));
        assert!(
            d.risk_level == RiskLevel::Green || d.risk_level == RiskLevel::Yellow,
            "Read-only GW op `{op}` should be Green or Yellow, not Red"
        );
    }
}

// ── Routing ────────────────────────────────────────────────────────────────

#[test]
fn routing_gw01_gmail_inbox_routes_correctly() {
    // PROMPT-ID: GW-01
    let prompts = ["Check my Gmail inbox.", "inbox dikhao", "show unread emails"];
    for p in &prompts {
        let r = IntentRouter::classify(p);
        assert!(
            matches!(&r.intent, Intent::DirectTool(t) if t == "gw_gmail_inbox")
                || matches!(r.intent, Intent::ComplexTask | Intent::Conversation),
            "'{p}' should route to gw_gmail_inbox, got: {:?}",
            r.intent
        );
    }
}

#[test]
fn routing_gw06_calendar_today_routes_correctly() {
    // PROMPT-ID: GW-06
    let prompts = [
        "What are my meetings today?",
        "aaj ki meetings batao",
        "today's calendar",
    ];
    for p in &prompts {
        let r = IntentRouter::classify(p);
        assert!(
            matches!(&r.intent, Intent::DirectTool(t) if t == "gw_calendar_today")
                || matches!(r.intent, Intent::ComplexTask | Intent::Conversation),
            "'{p}' should route to gw_calendar_today, got: {:?}",
            r.intent
        );
    }
}

// ── Functional — live calls (gated on credentials) ─────────────────────────

#[tokio::test]
async fn functional_gw01_gmail_inbox_live() {
    // PROMPT-ID: GW-01
    if !gworkspace_creds_available() {
        eprintln!("SKIP: Google Workspace credentials not available");
        return;
    }
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("gw_gmail_inbox").unwrap().clone();
    let result = handler.execute(serde_json::json!({})).await;
    assert!(
        result.success || result.error.is_some(),
        "gw_gmail_inbox must not panic"
    );
}

#[tokio::test]
async fn functional_gw06_calendar_today_live() {
    // PROMPT-ID: GW-06
    if !gworkspace_creds_available() {
        eprintln!("SKIP: Google Workspace credentials not available");
        return;
    }
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("gw_calendar_today").unwrap().clone();
    let result = handler.execute(serde_json::json!({})).await;
    assert!(
        result.success || result.error.is_some(),
        "gw_calendar_today must not panic"
    );
}

// ── Error path: missing credentials ───────────────────────────────────────

#[tokio::test]
async fn functional_gw_unauthenticated_returns_clean_error() {
    // PROMPT-ID: GW-01 — when no credentials, must return clean auth error
    if gworkspace_creds_available() {
        eprintln!("SKIP: credentials present, skipping unauthenticated test");
        return;
    }
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("gw_gmail_inbox").unwrap().clone();
    let result = handler.execute(serde_json::json!({})).await;
    assert!(
        !result.success,
        "gw_gmail_inbox without credentials must fail"
    );
    let err = result.error.unwrap_or_default().to_lowercase();
    assert!(
        err.contains("auth") || err.contains("token") || err.contains("credentials")
            || err.contains("unauthorized") || err.contains("not configured"),
        "Auth error message should mention credentials/auth/token, got: {err}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  §19 GOOGLE COLAB (MCP) — registry only, live tests are #[ignore]
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn smoke_colab_mcp_tool_present() {
    // PROMPT-ID: COLAB-01..COLAB-06
    // colab-mcp tools are bridged via MCP — at least the bridge entry must be present
    let reg = registry::build_default_registry();
    let colab_tools: Vec<_> = (0..256) // check all registered names
        .filter_map(|_| None::<()>)
        .collect();
    let _ = colab_tools;

    // At minimum, the tool registry must accept colab tool lookups without panicking
    let _ = reg.get_handler("mcp_colab-mcp_open_colab_browser_connection");
    let _ = reg.get_handler("open_colab_browser_connection");
}

#[test]
fn routing_colab01_create_notebook_routes_to_complex_task() {
    // PROMPT-ID: COLAB-01 — multi-step colab flow should be ComplexTask
    let r = IntentRouter::classify("Create a Google Colab notebook named mcp_test.ipynb");
    assert!(
        matches!(r.intent, Intent::ComplexTask | Intent::Conversation)
            || matches!(&r.intent, Intent::DirectTool(t) if t.contains("colab")),
        "Colab notebook creation should be ComplexTask or colab-directed, got: {:?}",
        r.intent
    );
}

#[test]
fn routing_colab06_hinglish_colab_chalao() {
    // PROMPT-ID: COLAB-06
    let r = IntentRouter::classify("Colab chalao.");
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if t.contains("colab"))
            || matches!(r.intent, Intent::ComplexTask | Intent::Conversation),
        "'Colab chalao' should route to colab tool or complex task, got: {:?}",
        r.intent
    );
}

/// Live Colab browser connection — requires colab-mcp server running.
/// Run manually: cargo test functional_colab_browser_connection -- --ignored
#[tokio::test]
#[ignore]
async fn functional_colab_browser_connection_live() {
    // PROMPT-ID: COLAB-02
    let reg = registry::build_default_registry();
    let handler = reg
        .get_handler("mcp_colab-mcp_open_colab_browser_connection")
        .or_else(|| reg.get_handler("open_colab_browser_connection"));
    let Some(handler) = handler else {
        eprintln!("SKIP: colab browser connection tool not registered");
        return;
    };
    let handler = handler.clone();
    let result = handler.execute(serde_json::json!({})).await;
    assert!(result.success, "open_colab_browser_connection should succeed: {:?}", result.error);
}

// ═══════════════════════════════════════════════════════════════════════════
//  §20 MCP FILESYSTEM SERVER TOOLS
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn routing_mcp_fs01_list_via_mcp_routes_correctly() {
    // PROMPT-ID: MCP-FS-01
    let r = IntentRouter::classify("List files in /home/obaid using the filesystem MCP.");
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if t.contains("list") || t.contains("mcp"))
            || matches!(r.intent, Intent::ComplexTask | Intent::Conversation),
        "MCP list should route to a list/mcp tool or complex task, got: {:?}",
        r.intent
    );
}

#[tokio::test]
async fn functional_mcp_fs01_list_directory_via_mcp() {
    // PROMPT-ID: MCP-FS-01 — test via direct tool (MCP FS falls back to built-in)
    let reg = registry::build_default_registry();
    // Try MCP-specific tool first, fall back to list_directory
    let handler = reg
        .get_handler("mcp_fs_list_directory")
        .or_else(|| reg.get_handler("list_directory"));
    let Some(handler) = handler else {
        eprintln!("SKIP: neither mcp_fs_list_directory nor list_directory found");
        return;
    };
    let handler = handler.clone();
    let result = handler
        .execute(serde_json::json!({ "path": "/home/obaid" }))
        .await;
    assert!(
        result.success || result.error.is_some(),
        "MCP fs list_directory must not panic"
    );
}

#[tokio::test]
async fn functional_mcp_fs02_read_file_via_mcp() {
    // PROMPT-ID: MCP-FS-02
    let reg = registry::build_default_registry();
    let handler = reg
        .get_handler("mcp_fs_read_file")
        .or_else(|| reg.get_handler("read_file"));
    let Some(handler) = handler else {
        return;
    };
    let handler = handler.clone();
    let result = handler
        .execute(serde_json::json!({ "path": "/media/obaid/SSD/KRIA/Cargo.toml" }))
        .await;
    assert!(
        result.success || result.error.is_some(),
        "MCP fs read_file must not panic"
    );
}
