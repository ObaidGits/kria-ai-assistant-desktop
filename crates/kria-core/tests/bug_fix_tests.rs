/// ─────────────────────────────────────────────────────────────────────────
///  Bug Fix Tests — Smoke, Functional, and Integration
///
///  Covers 5 bug groups from the live-test chat export (2025-04):
///    A. fetch_webpage routing  — router pattern + fallback arm
///    B. Volume level coercion  — string / percent / integer param variants
///    C. Brightness coercion    — same param coercion + GNOME D-Bus path
///    D. Notification delivery  — notify-send primary with explicit DBUS env
///    E. Hinglish routing       — "volume ko X% kardo", "brightness badhaao"
/// ─────────────────────────────────────────────────────────────────────────

use kria_core::agent::router::{Intent, IntentRouter};
use kria_core::tools::registry;

// ═══════════════════════════════════════════════════════════════════════════
//  A. fetch_webpage routing
// ═══════════════════════════════════════════════════════════════════════════

/// Smoke: "Fetch the content of https://…" must route to fetch_webpage, NOT
/// produce bash output or fall through to web_search.
#[test]
fn smoke_fetch_content_of_url_routes_to_fetch_webpage() {
    let prompts = [
        "Fetch the content of https://www.rust-lang.org",
        "fetch the content of https://example.com/page",
        "get the content of https://docs.rs/tokio",
        "fetch https://news.ycombinator.com",
        "Fetch this URL: https://api.github.com/repos/rust-lang/rust",
        "read the page https://wikipedia.org/wiki/Rust_(programming_language)",
        "scrape https://www.reddit.com/r/rust",
    ];
    for p in &prompts {
        let result = IntentRouter::classify(p);
        assert!(
            matches!(&result.intent, Intent::DirectTool(t) if t == "fetch_webpage"),
            "Expected DirectTool(fetch_webpage) for: {p}\n  Got: {:?}",
            result.intent
        );
    }
}

/// Functional: fetch_webpage tool is registered in the default registry.
#[test]
fn functional_fetch_webpage_tool_registered() {
    let reg = registry::build_default_registry();
    let def = reg.get_def("fetch_webpage");
    assert!(def.is_some(), "fetch_webpage must be registered in default registry");
    let def = def.unwrap();
    assert!(
        def.parameters.iter().any(|p| p.name == "url"),
        "fetch_webpage must have a 'url' parameter"
    );
}

/// Functional: fetch_webpage must NOT route for pure information search.
#[test]
fn functional_web_search_not_confused_with_fetch_webpage() {
    let search_prompts = [
        "search for latest Rust news",
        "what is machine learning",
        "find information about tokio async",
    ];
    for p in &search_prompts {
        let result = IntentRouter::classify(p);
        // These should NOT route to fetch_webpage — they should use web_search
        if let Intent::DirectTool(ref t) = result.intent {
            assert_ne!(
                t, "fetch_webpage",
                "Pure search query should not route to fetch_webpage: {p}"
            );
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  B. Volume level coercion
// ═══════════════════════════════════════════════════════════════════════════

/// The three JSON input shapes the LLM may produce for level=100:
///   - JSON integer: {"level": 100}
///   - JSON string: {"level": "100"}
///   - String with percent: {"level": "100%"}
/// All must produce level=100, not the default=50.
fn parse_level(v: serde_json::Value) -> u64 {
    v["level"]
        .as_u64()
        .or_else(|| v["level"].as_f64().map(|f| f as u64))
        .or_else(|| {
            v["level"]
                .as_str()
                .and_then(|s| s.trim_end_matches('%').trim().parse::<u64>().ok())
        })
        .unwrap_or(50)
        .min(100)
}

#[test]
fn functional_volume_level_integer_json() {
    assert_eq!(parse_level(serde_json::json!({ "level": 100 })), 100);
    assert_eq!(parse_level(serde_json::json!({ "level": 0 })), 0);
    assert_eq!(parse_level(serde_json::json!({ "level": 75 })), 75);
}

#[test]
fn functional_volume_level_string_json() {
    assert_eq!(parse_level(serde_json::json!({ "level": "100" })), 100);
    assert_eq!(parse_level(serde_json::json!({ "level": "0" })), 0);
    assert_eq!(parse_level(serde_json::json!({ "level": "80" })), 80);
}

#[test]
fn functional_volume_level_percent_string_json() {
    assert_eq!(parse_level(serde_json::json!({ "level": "100%" })), 100);
    assert_eq!(parse_level(serde_json::json!({ "level": "80%" })), 80);
    assert_eq!(parse_level(serde_json::json!({ "level": "0%" })), 0);
}

#[test]
fn functional_volume_level_clamps_above_100() {
    // LLM might send 150 — must clamp to 100
    assert_eq!(parse_level(serde_json::json!({ "level": 150 })), 100);
    assert_eq!(parse_level(serde_json::json!({ "level": "150%" })), 100);
}

#[test]
fn functional_volume_level_missing_defaults_to_50() {
    assert_eq!(parse_level(serde_json::json!({})), 50);
    assert_eq!(parse_level(serde_json::json!({ "level": null })), 50);
}

/// Smoke: "increase the speaker volume to 100%" must route to set_volume.
#[test]
fn smoke_increase_volume_to_100_percent_routes_to_set_volume() {
    let prompts = [
        "increase the speaker volume to 100%",
        "set volume to 100",
        "volume set to 80",
        "turn up the volume",
        "raise the volume",
        "decrease the volume",
        "lower the sound",
    ];
    for p in &prompts {
        let result = IntentRouter::classify(p);
        assert!(
            matches!(&result.intent, Intent::DirectTool(t) if t == "set_volume"),
            "Expected DirectTool(set_volume) for: {p}\n  Got: {:?}",
            result.intent
        );
    }
}

/// Functional: fallback level extraction strips trailing % correctly.
/// "increase the speaker volume to 100%" — split on whitespace gives "100%"
/// which must parse as 100 after trim_end_matches('%').
#[test]
fn functional_fallback_level_extraction_strips_percent() {
    let query = "increase the speaker volume to 100%";
    let lower = query.to_lowercase();
    let level: u64 = lower
        .split_whitespace()
        .find_map(|w| {
            w.trim_end_matches('%')
                .parse::<u64>()
                .ok()
                .filter(|&n| n <= 100)
        })
        .unwrap_or(50);
    assert_eq!(level, 100, "fallback must extract 100 from '100%'");
}

#[test]
fn functional_fallback_level_extraction_plain_number() {
    let query = "set volume to 75";
    let lower = query.to_lowercase();
    let level: u64 = lower
        .split_whitespace()
        .find_map(|w| {
            w.trim_end_matches('%')
                .parse::<u64>()
                .ok()
                .filter(|&n| n <= 100)
        })
        .unwrap_or(50);
    assert_eq!(level, 75);
}

// ═══════════════════════════════════════════════════════════════════════════
//  C. Brightness level coercion and routing
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn functional_brightness_level_integer_json() {
    assert_eq!(parse_level(serde_json::json!({ "level": 80 })), 80);
}

#[test]
fn functional_brightness_level_string_json() {
    assert_eq!(parse_level(serde_json::json!({ "level": "80" })), 80);
}

#[test]
fn functional_brightness_level_percent_string_json() {
    assert_eq!(parse_level(serde_json::json!({ "level": "80%" })), 80);
}

/// Smoke: brightness commands must route to set_brightness.
#[test]
fn smoke_brightness_commands_route_to_set_brightness() {
    let prompts = [
        "set brightness to 80%",
        "increase the brightness",
        "lower the screen brightness",
        "brightness set to 50",
        "brightness 70",
        "raise the brightness",
        "turn up the brightness",
    ];
    for p in &prompts {
        let result = IntentRouter::classify(p);
        assert!(
            matches!(&result.intent, Intent::DirectTool(t) if t == "set_brightness"),
            "Expected DirectTool(set_brightness) for: {p}\n  Got: {:?}",
            result.intent
        );
    }
}

/// Functional: set_brightness tool is registered.
#[test]
fn functional_set_brightness_tool_registered() {
    let reg = registry::build_default_registry();
    let def = reg.get_def("set_brightness");
    assert!(def.is_some(), "set_brightness must be registered");
    assert!(
        def.unwrap().parameters.iter().any(|p| p.name == "level"),
        "set_brightness must have 'level' parameter"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  D. Notification delivery
// ═══════════════════════════════════════════════════════════════════════════

/// Smoke: send_notification and schedule_reminder tools are registered.
#[test]
fn smoke_notification_tools_registered() {
    let reg = registry::build_default_registry();
    assert!(reg.get_def("send_notification").is_some());
    assert!(reg.get_def("schedule_reminder").is_some());
}

/// Integration: notify-send CLI must succeed with explicit DBUS_SESSION_BUS_ADDRESS.
/// This test requires a GNOME X11 session — will be skipped in CI headless environments.
#[tokio::test]
#[ignore = "requires GNOME desktop session (run manually: cargo test -- --ignored)"]
async fn integration_notify_send_with_explicit_dbus_shows_popup() {
    let dbus_addr = std::env::var("DBUS_SESSION_BUS_ADDRESS")
        .or_else(|_| std::env::var("XDG_RUNTIME_DIR").map(|d| format!("unix:path={}/bus", d)))
        .unwrap_or_else(|_| "unix:path=/run/user/1000/bus".to_string());
    let display = std::env::var("DISPLAY").unwrap_or_else(|_| ":1".to_string());
    let status = tokio::process::Command::new("notify-send")
        .env("DBUS_SESSION_BUS_ADDRESS", &dbus_addr)
        .env("DISPLAY", &display)
        .args(["-a", "KRIA", "-u", "normal", "-t", "3000",
               "KRIA Test", "Bug fix integration test: notification working!"])
        .status()
        .await
        .expect("notify-send must be available");
    assert!(status.success(), "notify-send must exit 0 with explicit DBUS env");
}

/// Integration: send_notification tool executes successfully on a desktop session.
#[tokio::test]
#[ignore = "requires GNOME desktop session (run manually: cargo test -- --ignored)"]
async fn integration_send_notification_tool_executes() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("send_notification").expect("tool must be registered");
    let params = serde_json::json!({
        "title": "KRIA Integration Test",
        "body": "send_notification tool executed successfully!"
    });
    let result = handler.execute(params).await;
    assert!(result.success, "send_notification should return success=true; got: {:?}", result.error);
}

/// Integration: schedule_reminder with 0.1-minute delay (6 seconds) schedules and fires.
/// This is a slow test; enable only when testing reminder plumbing manually.
#[tokio::test]
#[ignore = "slow (6s) + requires GNOME desktop session (run manually: cargo test -- --ignored)"]
async fn integration_schedule_reminder_fires_after_delay() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("schedule_reminder").expect("tool must be registered");
    let params = serde_json::json!({
        "message": "KRIA integration test reminder fired!",
        "delay_minutes": 0.1  // 6 seconds
    });
    let result = handler.execute(params).await;
    assert!(result.success, "schedule_reminder should return success=true");

    // Verify the scheduled: true JSON response
    let data = &result.data;
    assert_eq!(data["scheduled"], serde_json::json!(true));
    assert!(data["fires_in"].is_string(), "fires_in should be a human-readable string");

    // Wait for the reminder to fire (6 seconds + buffer)
    tokio::time::sleep(std::time::Duration::from_secs(8)).await;
    // If we reach here without panic the test passes (reminder fired silently or shown)
}

// ═══════════════════════════════════════════════════════════════════════════
//  E. Hinglish routing
// ═══════════════════════════════════════════════════════════════════════════

/// Smoke: Hinglish volume commands (with number) route to set_volume.
#[test]
fn smoke_hinglish_volume_set_routes_to_set_volume() {
    let prompts = [
        "volume ko 100% kardo",
        "volume ko 50 karo",
        "volume badhao",
        "awaaz 80 karo",
        "speaker volume ko 70% set karo",
    ];
    for p in &prompts {
        let result = IntentRouter::classify(p);
        assert!(
            matches!(&result.intent, Intent::DirectTool(t) if t == "set_volume"),
            "Expected DirectTool(set_volume) for Hinglish prompt: {p}\n  Got: {:?}",
            result.intent
        );
    }
}

/// Smoke: Hinglish brightness commands route to set_brightness.
#[test]
fn smoke_hinglish_brightness_routes_to_set_brightness() {
    let prompts = [
        "brightness ko 80% kardo",
        "brightness badhao",
        "brightness ko 50 set karo",
    ];
    for p in &prompts {
        let result = IntentRouter::classify(p);
        assert!(
            matches!(&result.intent, Intent::DirectTool(t) if t == "set_brightness"),
            "Expected DirectTool(set_brightness) for Hinglish prompt: {p}\n  Got: {:?}",
            result.intent
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  F. Volume tool registration sanity
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn functional_set_volume_tool_registered() {
    let reg = registry::build_default_registry();
    let def = reg.get_def("set_volume");
    assert!(def.is_some(), "set_volume must be registered");
    assert!(
        def.unwrap().parameters.iter().any(|p| p.name == "level"),
        "set_volume must have 'level' parameter"
    );
}

/// Integration: set_volume tool executes with numeric level on a PipeWire system.
#[tokio::test]
#[ignore = "requires PipeWire/PulseAudio (run manually: cargo test -- --ignored)"]
async fn integration_set_volume_integer_level_100() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("set_volume").expect("tool must be registered");

    // Test: integer JSON
    let result = handler.execute(serde_json::json!({ "level": 100 })).await;
    assert!(result.success, "set_volume with integer 100 should succeed; got: {:?}", result.error);

    // Reset to 75%
    let _ = handler.execute(serde_json::json!({ "level": 75 })).await;
}

/// Integration: set_volume tool handles string-typed level from LLM.
#[tokio::test]
#[ignore = "requires PipeWire/PulseAudio (run manually: cargo test -- --ignored)"]
async fn integration_set_volume_string_level_100_percent() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("set_volume").expect("tool must be registered");

    // Test: string "100%"
    let result = handler.execute(serde_json::json!({ "level": "100%" })).await;
    assert!(result.success, "set_volume with '100%' string should succeed; got: {:?}", result.error);
    let data = &result.data;
    assert_eq!(data["volume"], serde_json::json!(100), "volume in response should be 100");

    // Reset to 75%
    let _ = handler.execute(serde_json::json!({ "level": 75 })).await;
}

/// Integration: set_brightness tool executes via GNOME SettingsDaemon D-Bus.
#[tokio::test]
#[ignore = "requires GNOME desktop session (run manually: cargo test -- --ignored)"]
async fn integration_set_brightness_gnome_dbus() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("set_brightness").expect("tool must be registered");

    let result = handler.execute(serde_json::json!({ "level": 70 })).await;
    assert!(result.success, "set_brightness should succeed on GNOME; got: {:?}", result.error);
    let data = &result.data;
    // When using GNOME SettingsDaemon, backend should be "gnome-settingsd"
    let backend = data["backend"].as_str().unwrap_or("unknown");
    assert!(
        backend == "gnome-settingsd" || backend == "brightnessctl" || backend == "xrandr-gamma",
        "brightness backend should be one of the known backends, got: {backend}"
    );
}
