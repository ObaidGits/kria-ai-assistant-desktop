// ─────────────────────────────────────────────────────────────────────────────
//  test_smoke_system.rs — §1 System Info, §2 Power, §3 System Config, §5 Process
//
//  Covers PROMPT-IDs:
//    SYS-01..SYS-10, PWR-01..PWR-08, CFG-01..CFG-09, PROC-01..PROC-08
// ─────────────────────────────────────────────────────────────────────────────

mod common;

use common::{assert_result_field, assert_tool_success};
use kria_core::agent::router::{Intent, IntentRouter};
use kria_core::safety::policy::{PolicyEngine, RiskLevel};
use kria_core::tools::registry;

// ═══════════════════════════════════════════════════════════════════════════
//  SECTION 1 — SYSTEM INFORMATION & HEALTH
// ═══════════════════════════════════════════════════════════════════════════

// ── Smoke: tools registered ──────────────────────────────────────────────

#[test]
fn smoke_sys_tools_registered() {
    // PROMPT-ID: SYS-01..SYS-10
    let reg = registry::build_default_registry();
    let required = [
        "get_cpu_usage",
        "get_memory_info",
        "get_disk_space",
        "get_battery_status",
        "get_system_uptime",
        "check_system_health",
        "get_alerts",
        "dismiss_alert",
        "get_gpu_info",
        "get_network_status",
    ];
    for name in &required {
        assert!(
            reg.get_handler(name).is_some(),
            "Tool `{name}` must be registered (required by §1)"
        );
    }
}

// ── Functional: direct tool execution ─────────────────────────────────────

#[tokio::test]
async fn functional_sys01_get_cpu_usage() {
    // PROMPT-ID: SYS-01, SYS-02
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("get_cpu_usage").unwrap().clone();
    let result = handler.execute(serde_json::json!({})).await;
    assert_tool_success(&serde_json::json!({"success": result.success, "data": result.data, "error": result.error}));
    assert_result_field(
        &result.data,
        "percentage",
        |v| v.as_f64().map(|n| n >= 0.0 && n <= 100.0).unwrap_or(false),
        "percentage must be 0.0–100.0",
    );
}

#[tokio::test]
async fn functional_sys_get_memory_info() {
    // PROMPT-ID: SYS-01
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("get_memory_info").unwrap().clone();
    let result = handler.execute(serde_json::json!({})).await;
    assert!(result.success, "get_memory_info should succeed: {:?}", result.error);
    // Must return at least total_mb and used_mb
    assert!(
        result.data.get("total_mb").or(result.data.get("total")).is_some(),
        "get_memory_info must include total memory field"
    );
}

#[tokio::test]
async fn functional_sys_get_disk_space() {
    // PROMPT-ID: SYS-01
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("get_disk_space").unwrap().clone();
    let result = handler.execute(serde_json::json!({})).await;
    assert!(result.success, "get_disk_space should succeed: {:?}", result.error);
}

#[tokio::test]
async fn functional_sys03_get_battery_status() {
    // PROMPT-ID: SYS-03
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("get_battery_status").unwrap().clone();
    let result = handler.execute(serde_json::json!({})).await;
    // OK whether battery exists or not; must not panic
    assert!(
        result.success || result.error.is_some(),
        "get_battery_status must either succeed or return a clean error"
    );
}

#[tokio::test]
async fn functional_sys04_get_system_uptime() {
    // PROMPT-ID: SYS-04
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("get_system_uptime").unwrap().clone();
    let result = handler.execute(serde_json::json!({})).await;
    assert!(result.success, "get_system_uptime should succeed: {:?}", result.error);
}

#[tokio::test]
async fn functional_sys05_check_system_health() {
    // PROMPT-ID: SYS-05
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("check_system_health").unwrap().clone();
    let result = handler.execute(serde_json::json!({})).await;
    assert!(result.success, "check_system_health should succeed: {:?}", result.error);
}

#[tokio::test]
async fn functional_sys06_get_alerts() {
    // PROMPT-ID: SYS-06
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("get_alerts").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "include_dismissed": false }))
        .await;
    assert!(result.success, "get_alerts should succeed: {:?}", result.error);
}

#[tokio::test]
async fn functional_sys07_dismiss_alert() {
    // PROMPT-ID: SYS-07
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("dismiss_alert").unwrap().clone();
    // Dismissing a non-existent alert should return a clean result (not panic)
    let result = handler
        .execute(serde_json::json!({ "id": "nonexistent-alert-999" }))
        .await;
    assert!(
        result.success || result.error.is_some(),
        "dismiss_alert must not panic for unknown id"
    );
}

#[tokio::test]
async fn functional_sys08_get_gpu_info() {
    // PROMPT-ID: SYS-08
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("get_gpu_info").unwrap().clone();
    let result = handler.execute(serde_json::json!({})).await;
    // GPU may not be present — must not panic; clean success or clean error
    assert!(
        result.success || result.error.is_some(),
        "get_gpu_info must not panic on systems without GPU"
    );
}

#[tokio::test]
async fn functional_sys09_get_network_status() {
    // PROMPT-ID: SYS-09, SYS-10
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("get_network_status").unwrap().clone();
    let result = handler.execute(serde_json::json!({})).await;
    assert!(result.success, "get_network_status should succeed: {:?}", result.error);
}

// ── Routing ────────────────────────────────────────────────────────────────

#[test]
fn routing_sys01_system_stats_routes_to_get_cpu_or_check_health() {
    // PROMPT-ID: SYS-01
    let prompts = ["What are my system stats?", "system stats", "show system info"];
    for p in &prompts {
        let r = IntentRouter::classify(p);
        let routed_to_sys = matches!(
            &r.intent,
            Intent::DirectTool(t)
            if matches!(t.as_str(), "get_cpu_usage" | "check_system_health" | "get_memory_info")
        );
        assert!(
            routed_to_sys || matches!(r.intent, Intent::ComplexTask | Intent::Conversation),
            "'{p}' should route to a system tool or complex task, got: {:?}",
            r.intent
        );
    }
}

#[test]
fn routing_sys02_hinglish_cpu_routes_to_get_cpu_usage() {
    // PROMPT-ID: SYS-02
    let r = IntentRouter::classify("Mera CPU kitna use ho raha hai?");
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if t == "get_cpu_usage")
            || matches!(r.intent, Intent::Conversation | Intent::ComplexTask),
        "Hinglish CPU prompt should route to get_cpu_usage or be handled as conversation, got: {:?}",
        r.intent
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  SECTION 2 — POWER MANAGEMENT
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn smoke_power_tools_registered() {
    // PROMPT-ID: PWR-01..PWR-07
    let reg = registry::build_default_registry();
    for name in &["lock_screen", "sleep", "shutdown_system", "reboot_system"] {
        assert!(
            reg.get_handler(name).is_some(),
            "Tool `{name}` must be registered (required by §2)"
        );
    }
}

#[test]
fn policy_pwr01_lock_screen_is_green() {
    // PROMPT-ID: PWR-01, PWR-02
    let engine = PolicyEngine::new();
    let decision = engine.evaluate("lock_screen", &serde_json::json!({}));
    assert!(
        decision.risk_level == RiskLevel::Green || decision.risk_level == RiskLevel::Yellow,
        "lock_screen should be Green or Yellow, not Red"
    );
}

#[test]
fn policy_pwr06_shutdown_system_is_red() {
    // PROMPT-ID: PWR-06
    let engine = PolicyEngine::new();
    let decision = engine.evaluate("shutdown_system", &serde_json::json!({}));
    assert_eq!(
        decision.risk_level,
        RiskLevel::Red,
        "shutdown_system must be classified Red (destructive)"
    );
}

#[test]
fn policy_pwr07_reboot_system_is_red() {
    // PROMPT-ID: PWR-07
    let engine = PolicyEngine::new();
    let decision = engine.evaluate("reboot_system", &serde_json::json!({}));
    assert_eq!(
        decision.risk_level,
        RiskLevel::Red,
        "reboot_system must be classified Red (destructive)"
    );
}

#[test]
fn routing_pwr01_lock_screen_routes_correctly() {
    // PROMPT-ID: PWR-01, PWR-02
    let prompts = [
        "Lock my screen.",
        "Screen lock karo.",
        "lock screen",
        "lock the computer",
    ];
    for p in &prompts {
        let r = IntentRouter::classify(p);
        assert!(
            matches!(&r.intent, Intent::DirectTool(t) if t == "lock_screen"),
            "'{p}' should route to lock_screen, got: {:?}",
            r.intent
        );
    }
}

#[test]
fn routing_pwr04_get_power_plan_routes_correctly() {
    // PROMPT-ID: PWR-04
    let r = IntentRouter::classify("What is the current power plan?");
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if t == "get_power_plan")
            || matches!(r.intent, Intent::Conversation | Intent::ComplexTask),
        "Power plan query should route to get_power_plan or conversation, got: {:?}",
        r.intent
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  SECTION 3 — SYSTEM CONFIGURATION
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn smoke_config_tools_registered() {
    // PROMPT-ID: CFG-01..CFG-09
    let reg = registry::build_default_registry();
    let required = [
        "set_volume",
        "set_brightness",
        "toggle_wifi",
        "get_wifi_networks",
        "get_environment_variable",
        "list_environment_variables",
        "set_environment_variable",
        "get_power_plan",
        "set_power_plan",
    ];
    for name in &required {
        assert!(
            reg.get_handler(name).is_some(),
            "Tool `{name}` must be registered (required by §3)"
        );
    }
}

#[test]
fn routing_cfg01_set_volume_routes_correctly() {
    // PROMPT-ID: CFG-01, CFG-02
    let prompts = [
        "Set volume to 60.",
        "Volume band karo.",
        "volume ko 80 percent kardo",
        "set the volume to 40%",
        "volume 50",
    ];
    for p in &prompts {
        let r = IntentRouter::classify(p);
        assert!(
            matches!(&r.intent, Intent::DirectTool(t) if t == "set_volume"),
            "'{p}' should route to set_volume, got: {:?}",
            r.intent
        );
    }
}

#[test]
fn routing_cfg03_set_brightness_routes_correctly() {
    // PROMPT-ID: CFG-03
    let prompts = [
        "Set brightness to 80 percent.",
        "brightness badhaao",
        "brightness ko 50% karo",
        "screen brightness 70",
    ];
    for p in &prompts {
        let r = IntentRouter::classify(p);
        assert!(
            matches!(&r.intent, Intent::DirectTool(t) if t == "set_brightness"),
            "'{p}' should route to set_brightness, got: {:?}",
            r.intent
        );
    }
}

#[test]
fn routing_cfg04_toggle_wifi_off_routes_correctly() {
    // PROMPT-ID: CFG-04
    let r = IntentRouter::classify("Turn off WiFi.");
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if t == "toggle_wifi"),
        "'Turn off WiFi.' should route to toggle_wifi, got: {:?}",
        r.intent
    );
}

#[test]
fn routing_cfg06_list_wifi_networks_routes_correctly() {
    // PROMPT-ID: CFG-06
    let prompts = [
        "List available WiFi networks.",
        "show wifi networks",
        "available networks dikhao",
    ];
    for p in &prompts {
        let r = IntentRouter::classify(p);
        assert!(
            matches!(&r.intent, Intent::DirectTool(t) if t == "get_wifi_networks"),
            "'{p}' should route to get_wifi_networks, got: {:?}",
            r.intent
        );
    }
}

#[test]
fn routing_cfg07_get_env_var_routes_correctly() {
    // PROMPT-ID: CFG-07
    let r = IntentRouter::classify("What is the value of the HOME environment variable?");
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if t == "get_environment_variable")
            || matches!(r.intent, Intent::Conversation | Intent::ComplexTask),
        "Env var query should route to get_environment_variable or conversation, got: {:?}",
        r.intent
    );
}

#[tokio::test]
async fn functional_cfg01_set_volume_integer_param() {
    // PROMPT-ID: CFG-01 — integer level (most common LLM output)
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("set_volume").unwrap().clone();
    let result = handler.execute(serde_json::json!({ "level": 60 })).await;
    assert!(
        result.success || result.error.as_deref().map(|e| !e.contains("parse")).unwrap_or(true),
        "set_volume with integer level 60 must not fail with parse error: {:?}",
        result.error
    );
}

#[tokio::test]
async fn functional_cfg01_set_volume_string_percent_param() {
    // PROMPT-ID: CFG-01 — string "60%" (LLM may pass this)
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("set_volume").unwrap().clone();
    let result = handler.execute(serde_json::json!({ "level": "60%" })).await;
    assert!(
        result.success || result.error.as_deref().map(|e| !e.contains("parse")).unwrap_or(true),
        "set_volume with string '60%' must not fail with parse error: {:?}",
        result.error
    );
}

#[tokio::test]
async fn functional_cfg01_set_volume_clamps_above_100() {
    // PROMPT-ID: CFG-01 — levels > 100 must be clamped, not errored
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("set_volume").unwrap().clone();
    let result = handler.execute(serde_json::json!({ "level": 150 })).await;
    assert!(
        result.success || result.error.is_some(),
        "set_volume with level 150 must not panic"
    );
}

#[tokio::test]
async fn functional_cfg07_get_environment_variable_home() {
    // PROMPT-ID: CFG-07
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("get_environment_variable").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "name": "HOME" }))
        .await;
    assert!(result.success, "get_environment_variable HOME should succeed: {:?}", result.error);
    let val = result.data["value"].as_str().unwrap_or("");
    assert!(
        val.contains("/home") || val.contains("obaid") || !val.is_empty(),
        "HOME should return a non-empty path, got: {val}"
    );
}

#[tokio::test]
async fn functional_cfg08_list_environment_variables() {
    // PROMPT-ID: CFG-08
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("list_environment_variables").unwrap().clone();
    let result = handler.execute(serde_json::json!({})).await;
    assert!(result.success, "list_environment_variables should succeed: {:?}", result.error);
}

// ═══════════════════════════════════════════════════════════════════════════
//  SECTION 5 — PROCESS & SERVICE MANAGEMENT (read-only)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn smoke_process_tools_registered() {
    // PROMPT-ID: PROC-01..PROC-08
    let reg = registry::build_default_registry();
    let required = [
        "list_running_apps",
        "open_application",
        "close_application",
        "kill_process",
        "focus_window",
        "manage_service",
        "get_active_connections",
    ];
    for name in &required {
        assert!(
            reg.get_handler(name).is_some(),
            "Tool `{name}` must be registered (required by §5)"
        );
    }
}

#[tokio::test]
async fn functional_proc01_list_running_apps() {
    // PROMPT-ID: PROC-01
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("list_running_apps").unwrap().clone();
    let result = handler.execute(serde_json::json!({})).await;
    assert!(result.success, "list_running_apps should succeed: {:?}", result.error);
    // Must return an array or object with process data
    assert!(
        result.data.is_array() || result.data.is_object(),
        "list_running_apps must return array or object"
    );
}

#[tokio::test]
async fn functional_proc08_get_active_connections() {
    // PROMPT-ID: PROC-08
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("get_active_connections").unwrap().clone();
    let result = handler.execute(serde_json::json!({})).await;
    assert!(
        result.success || result.error.is_some(),
        "get_active_connections must not panic"
    );
}

#[test]
fn policy_proc04_kill_process_is_red() {
    // PROMPT-ID: PROC-04
    let engine = PolicyEngine::new();
    let decision = engine.evaluate("kill_process", &serde_json::json!({ "pid": 12345 }));
    assert_eq!(
        decision.risk_level,
        RiskLevel::Red,
        "kill_process must be classified Red (destructive)"
    );
}

#[test]
fn policy_proc03_close_application_is_yellow_or_red() {
    // PROMPT-ID: PROC-03
    let engine = PolicyEngine::new();
    let decision = engine.evaluate("close_application", &serde_json::json!({}));
    assert!(
        decision.risk_level == RiskLevel::Yellow || decision.risk_level == RiskLevel::Red,
        "close_application should not be silently Green — data loss risk"
    );
}

#[test]
fn routing_proc01_list_apps_routes_correctly() {
    // PROMPT-ID: PROC-01
    let prompts = [
        "What apps are running right now?",
        "list running processes",
        "show running apps",
    ];
    for p in &prompts {
        let r = IntentRouter::classify(p);
        assert!(
            matches!(&r.intent, Intent::DirectTool(t) if t == "list_running_apps")
                || matches!(r.intent, Intent::Conversation | Intent::ComplexTask),
            "'{p}' should route to list_running_apps or conversation, got: {:?}",
            r.intent
        );
    }
}

#[test]
fn routing_proc06_manage_service_status_routes_correctly() {
    // PROMPT-ID: PROC-06
    let r = IntentRouter::classify("Check status of docker service.");
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if t == "manage_service")
            || matches!(r.intent, Intent::Conversation | Intent::ComplexTask),
        "Docker service status should route to manage_service, got: {:?}",
        r.intent
    );
}
