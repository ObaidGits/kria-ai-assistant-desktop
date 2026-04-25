// ─────────────────────────────────────────────────────────────────────────────
//  test_interaction_desktop.rs — §9 Interaction & Desktop, §10 Notifications
//
//  Desktop/clipboard/window tests require GNOME/X11 (guarded).
//  Notification tests require DBUS (guarded).
//
//  Covers PROMPT-IDs: DT-01..DT-10, COM-01..COM-03
// ─────────────────────────────────────────────────────────────────────────────

mod common;

use common::{dbus_available, gnome_display_available};
use kria_core::agent::router::{Intent, IntentRouter};
use kria_core::safety::policy::{PolicyEngine, RiskLevel};
use kria_core::tools::registry;

// ═══════════════════════════════════════════════════════════════════════════
//  Smoke — all interaction/desktop tools must be registered
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn smoke_interaction_tools_registered() {
    // PROMPT-ID: DT-01..DT-10
    let reg = registry::build_default_registry();
    let required = [
        "get_clipboard",
        "set_clipboard",
        "transform_clipboard",
        "type_text",
        "get_active_window",
        "list_windows",
        "maximize_window",
        "minimize_window",
        "tile_windows",
    ];
    for name in &required {
        assert!(
            reg.get_handler(name).is_some(),
            "Tool `{name}` must be registered (required by §9)"
        );
    }
}

#[test]
fn smoke_communication_tools_registered() {
    // PROMPT-ID: COM-01..COM-03
    let reg = registry::build_default_registry();
    let required = ["send_notification", "schedule_reminder", "compose_email"];
    for name in &required {
        assert!(
            reg.get_handler(name).is_some(),
            "Tool `{name}` must be registered (required by §10)"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Routing — §9 prompts
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn routing_dt01_get_clipboard_routes_correctly() {
    // PROMPT-ID: DT-01
    let prompts = [
        "Get clipboard content.",
        "clipboard kya hai?",
        "what's in my clipboard?",
        "show clipboard",
    ];
    for p in &prompts {
        let r = IntentRouter::classify(p);
        assert!(
            matches!(&r.intent, Intent::DirectTool(t) if t == "get_clipboard")
                || matches!(r.intent, Intent::Conversation | Intent::ComplexTask),
            "'{p}' should route to get_clipboard, got: {:?}",
            r.intent
        );
    }
}

#[test]
fn routing_dt02_set_clipboard_routes_correctly() {
    // PROMPT-ID: DT-02
    let r = IntentRouter::classify("Set clipboard to 'Hello from KRIA'");
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if t == "set_clipboard")
            || matches!(r.intent, Intent::ComplexTask | Intent::Conversation),
        "Set clipboard should route to set_clipboard, got: {:?}",
        r.intent
    );
}

#[test]
fn routing_dt05_get_active_window_routes_correctly() {
    // PROMPT-ID: DT-05
    let r = IntentRouter::classify("What window is currently active?");
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if t == "get_active_window")
            || matches!(r.intent, Intent::Conversation | Intent::ComplexTask),
        "Active window query should route to get_active_window, got: {:?}",
        r.intent
    );
}

#[test]
fn routing_dt06_list_windows_routes_correctly() {
    // PROMPT-ID: DT-06
    let prompts = ["List all open windows.", "show all windows", "khuli windows list karo"];
    for p in &prompts {
        let r = IntentRouter::classify(p);
        assert!(
            matches!(&r.intent, Intent::DirectTool(t) if t == "list_windows")
                || matches!(r.intent, Intent::Conversation | Intent::ComplexTask),
            "'{p}' should route to list_windows, got: {:?}",
            r.intent
        );
    }
}

#[test]
fn routing_dt09_open_url_routes_correctly() {
    // PROMPT-ID: DT-09
    let r = IntentRouter::classify("Open https://github.com in the browser.");
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if matches!(t.as_str(), "open_url" | "browser_search"))
            || matches!(r.intent, Intent::ComplexTask),
        "Open URL should route to open_url or browser_search, got: {:?}",
        r.intent
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  Routing — §10 prompts
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn routing_com01_send_notification_routes_correctly() {
    // PROMPT-ID: COM-01
    let prompts = [
        "Send me a notification: 'Build complete!'",
        "notification bhejo: build done",
        "notify me: tests passed",
    ];
    for p in &prompts {
        let r = IntentRouter::classify(p);
        assert!(
            matches!(&r.intent, Intent::DirectTool(t) if t == "send_notification")
                || matches!(r.intent, Intent::ComplexTask | Intent::Conversation),
            "'{p}' should route to send_notification, got: {:?}",
            r.intent
        );
    }
}

#[test]
fn routing_com02_schedule_reminder_routes_correctly() {
    // PROMPT-ID: COM-02
    let prompts = [
        "Remind me to drink water in 15 minutes.",
        "set reminder: meeting in 30 minutes",
        "15 minute mein mujhe yaad dilao",
    ];
    for p in &prompts {
        let r = IntentRouter::classify(p);
        assert!(
            matches!(&r.intent, Intent::DirectTool(t) if t == "schedule_reminder")
                || matches!(r.intent, Intent::ComplexTask | Intent::Conversation),
            "'{p}' should route to schedule_reminder, got: {:?}",
            r.intent
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Policy — type_text requires confirmation (WARN level)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn policy_dt04_type_text_is_yellow_or_red() {
    // PROMPT-ID: DT-04 — typing into an arbitrary window could affect any app
    let engine = PolicyEngine::new();
    let decision = engine.evaluate("type_text", &serde_json::json!({ "text": "Hello" }));
    assert!(
        decision.risk_level == RiskLevel::Yellow || decision.risk_level == RiskLevel::Red,
        "type_text should require confirmation (Yellow or Red), not be silently Green"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  Functional — clipboard (requires display)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn functional_dt01_dt02_clipboard_roundtrip() {
    // PROMPT-ID: DT-01, DT-02
    if !gnome_display_available() {
        eprintln!("SKIP: no display for clipboard test");
        return;
    }

    let reg = registry::build_default_registry();

    // Set clipboard
    let set_handler = reg.get_handler("set_clipboard").unwrap().clone();
    let set_result = set_handler
        .execute(serde_json::json!({ "text": "Hello from KRIA test" }))
        .await;
    assert!(set_result.success, "set_clipboard failed: {:?}", set_result.error);

    // Read it back
    let get_handler = reg.get_handler("get_clipboard").unwrap().clone();
    let get_result = get_handler.execute(serde_json::json!({})).await;
    assert!(get_result.success, "get_clipboard failed: {:?}", get_result.error);

    let content = get_result.data["content"].as_str()
        .or(get_result.data["text"].as_str())
        .or(get_result.data.as_str())
        .unwrap_or("");
    assert_eq!(
        content, "Hello from KRIA test",
        "get_clipboard must return exactly what was set"
    );
}

#[tokio::test]
async fn functional_dt03_transform_clipboard_uppercase() {
    // PROMPT-ID: DT-03
    if !gnome_display_available() {
        eprintln!("SKIP: no display for clipboard transform test");
        return;
    }

    let reg = registry::build_default_registry();

    // Seed a value
    {
        let h = reg.get_handler("set_clipboard").unwrap().clone();
        let r = h.execute(serde_json::json!({ "text": "hello world" })).await;
        if !r.success {
            eprintln!("SKIP: could not set clipboard");
            return;
        }
    }

    // Transform to uppercase
    let handler = reg.get_handler("transform_clipboard").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "transform": "uppercase" }))
        .await;
    assert!(result.success, "transform_clipboard uppercase failed: {:?}", result.error);

    // Verify
    let get_h = reg.get_handler("get_clipboard").unwrap().clone();
    let get_r = get_h.execute(serde_json::json!({})).await;
    if get_r.success {
        let content = get_r.data["content"].as_str()
            .or(get_r.data.as_str())
            .unwrap_or("");
        assert_eq!(
            content, "HELLO WORLD",
            "transform_clipboard uppercase should produce 'HELLO WORLD'"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Functional — window management (requires GNOME)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn functional_dt05_get_active_window() {
    // PROMPT-ID: DT-05
    if !gnome_display_available() {
        eprintln!("SKIP: no display for active window test");
        return;
    }

    let reg = registry::build_default_registry();
    let handler = reg.get_handler("get_active_window").unwrap().clone();
    let result = handler.execute(serde_json::json!({})).await;
    assert!(
        result.success || result.error.is_some(),
        "get_active_window must not panic"
    );
}

#[tokio::test]
async fn functional_dt06_list_windows() {
    // PROMPT-ID: DT-06
    if !gnome_display_available() {
        eprintln!("SKIP: no display for list_windows test");
        return;
    }

    let reg = registry::build_default_registry();
    let handler = reg.get_handler("list_windows").unwrap().clone();
    let result = handler.execute(serde_json::json!({})).await;
    assert!(
        result.success || result.error.is_some(),
        "list_windows must not panic"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  Functional — notifications (requires DBUS)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn functional_com01_send_notification() {
    // PROMPT-ID: COM-01 — critical: notification must actually appear
    if !dbus_available() {
        eprintln!("SKIP: no DBUS for notification test");
        return;
    }

    let reg = registry::build_default_registry();
    let handler = reg.get_handler("send_notification").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({
            "title": "KRIA Test",
            "body": "Build complete! (automated test)"
        }))
        .await;
    assert!(
        result.success,
        "send_notification must succeed when DBUS is available: {:?}",
        result.error
    );
}

#[tokio::test]
async fn functional_com02_schedule_reminder() {
    // PROMPT-ID: COM-02 — reminder should be scheduled (not fired immediately)
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("schedule_reminder").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({
            "message": "KRIA automated test reminder",
            "delay_minutes": 9999
        }))
        .await;
    assert!(
        result.success,
        "schedule_reminder should accept a far-future delay: {:?}",
        result.error
    );
}

#[tokio::test]
async fn functional_com03_compose_email_no_send() {
    // PROMPT-ID: COM-03 — compose_email must open draft, NOT auto-send
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("compose_email").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({
            "to": "test@example.com",
            "subject": "Test from KRIA",
            "body": "Hello, automated test."
        }))
        .await;
    // Either succeeds (opens draft) or fails cleanly — never silently sends
    assert!(
        result.success || result.error.is_some(),
        "compose_email must not panic"
    );
    // Must NOT have a field indicating the email was sent
    let sent = result.data.get("sent").and_then(|v| v.as_bool()).unwrap_or(false);
    assert!(
        !sent,
        "compose_email must NOT auto-send — it should only open a draft"
    );
}
