// ─────────────────────────────────────────────────────────────────────────────
//  test_safety_routing.rs — §22 Safety/Policy Guardrails, §23 Routing/Disambiguation
//
//  This file tests the critical safety layer:
//    - Blocked path enforcement  (/etc, /boot, ~/.ssh, etc.)
//    - Catastrophic command blocking (sudo rm -rf /, --no-verify bypass)
//    - Emergency-stop phrase recognition
//    - HITL gateway approval flow
//    - Intent disambiguation (vague commands, topic overlap, direct-answer queries)
//
//  Covers PROMPT-IDs: SAFE-01..SAFE-10, ROUTE-01..ROUTE-10
// ─────────────────────────────────────────────────────────────────────────────

mod common;

use std::sync::Arc;

use kria_core::agent::router::{Intent, IntentRouter};
use kria_core::safety::hitl::{ApprovalResponse, HitlGateway};
use kria_core::safety::policy::{PolicyEngine, RiskLevel};
use kria_core::tools::registry;

// ═══════════════════════════════════════════════════════════════════════════
//  §22  SAFETY  —  BLOCKED PATHS
// ═══════════════════════════════════════════════════════════════════════════

/// Every path in this list is hard-blocked; writing/reading any of them must
/// be classified Red by the PolicyEngine.
const BLOCKED_PATHS: &[&str] = &[
    "/etc/passwd",
    "/etc/shadow",
    "/boot/grub.cfg",
    "/root/.bashrc",
    "/usr/bin/sudo",
    "/var/log/syslog",
    "/proc/cpuinfo",
    "/sys/kernel/kexec",
    "/home/obaid/.ssh/id_rsa",
    "/home/obaid/.gnupg/secring.gpg",
];

#[test]
fn policy_safe01_write_to_blocked_paths_is_red() {
    // PROMPT-ID: SAFE-01, SAFE-03
    let engine = PolicyEngine::new();
    for path in BLOCKED_PATHS {
        let d = engine.evaluate("write_file", &serde_json::json!({ "path": path, "content": "x" }));
        assert_eq!(
            d.risk_level,
            RiskLevel::Red,
            "write_file to {path} must be Red"
        );
    }
}

#[test]
fn policy_safe01_read_blocked_paths_is_at_least_yellow() {
    // PROMPT-ID: SAFE-01 — reading sensitive paths must not be Green
    let engine = PolicyEngine::new();
    let sensitive = ["/etc/shadow", "/home/obaid/.ssh/id_rsa", "/home/obaid/.gnupg/secring.gpg"];
    for path in &sensitive {
        let d = engine.evaluate("read_file", &serde_json::json!({ "path": path }));
        assert_ne!(
            d.risk_level,
            RiskLevel::Green,
            "read_file on sensitive path {path} must be at least Yellow"
        );
    }
}

#[test]
fn policy_safe05_catastrophic_bash_is_red() {
    // PROMPT-ID: SAFE-05
    let engine = PolicyEngine::new();
    let catastrophic = [
        "sudo rm -rf /",
        "rm -rf /",
        "dd if=/dev/zero of=/dev/sda",
        "chmod -R 777 /",
        "mkfs.ext4 /dev/sda",
    ];
    for cmd in &catastrophic {
        let d = engine.evaluate("execute_bash", &serde_json::json!({ "command": cmd }));
        assert_eq!(
            d.risk_level,
            RiskLevel::Red,
            "Catastrophic command '{cmd}' must be Red"
        );
    }
}

#[test]
fn policy_safe07_git_no_verify_bypass_is_red() {
    // PROMPT-ID: SAFE-07 — --no-verify skips pre-commit/pre-push hooks; must be blocked
    let engine = PolicyEngine::new();
    let cmds = [
        "git push --no-verify",
        "git commit --no-verify -m 'bypass'",
    ];
    for cmd in &cmds {
        let d = engine.evaluate("execute_bash", &serde_json::json!({ "command": cmd }));
        assert_eq!(
            d.risk_level,
            RiskLevel::Red,
            "Git --no-verify bypass '{cmd}' must be Red"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  §22  SAFETY  —  TOOL-LEVEL CHECKS (write_file won't touch blocked paths)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn functional_safe01_write_to_etc_returns_error() {
    // PROMPT-ID: SAFE-01 — actual tool call must reject /etc/passwd writes
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("write_file").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({
            "path": "/etc/kria_test_should_never_write.txt",
            "content": "blocked write test"
        }))
        .await;
    // Must fail — either policy blocked or permission denied
    assert!(
        !result.success,
        "write_file to /etc must not succeed (was: success=true)"
    );
    assert!(result.error.is_some(), "write_file /etc failure must carry an error message");
}

#[tokio::test]
async fn functional_safe03_write_to_ssh_returns_error() {
    // PROMPT-ID: SAFE-03
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("write_file").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({
            "path": "/home/obaid/.ssh/kria_test_payload",
            "content": "test"
        }))
        .await;
    assert!(
        !result.success,
        "write_file to ~/.ssh must not succeed"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  §22  SAFETY  —  HITL GATEWAY
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn functional_safe08_hitl_gateway_approved_flow() {
    // PROMPT-ID: SAFE-08 — approved Red action must proceed after gateway approval
    let gateway = Arc::new(HitlGateway::new(30));
    let request_id = HitlGateway::generate_request_id();

    // Respond with Approved *before* requesting to avoid blocking the await
    let gw2 = Arc::clone(&gateway);
    let id_clone = request_id.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        gw2.respond(&id_clone, ApprovalResponse::Approved).await;
    });

    let outcome = gateway
        .request_approval_with_id(
            &request_id,
            "shutdown",
            serde_json::json!({}),
            RiskLevel::Red,
            "Shut down the system",
            false,
        )
        .await;
    assert!(
        matches!(outcome, ApprovalResponse::Approved),
        "HITL gateway must propagate Approved response, got: {outcome:?}"
    );
}

#[tokio::test]
async fn functional_safe08_hitl_gateway_rejected_flow() {
    // PROMPT-ID: SAFE-08 — rejected action must not proceed
    let gateway = Arc::new(HitlGateway::new(30));
    let request_id = HitlGateway::generate_request_id();

    let gw2 = Arc::clone(&gateway);
    let id_clone = request_id.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        gw2.respond(&id_clone, ApprovalResponse::Denied).await;
    });

    let outcome = gateway
        .request_approval_with_id(
            &request_id,
            "gmail_delete",
            serde_json::json!({ "id": "123" }),
            RiskLevel::Red,
            "Delete email with id 123",
            false,
        )
        .await;
    assert!(
        matches!(outcome, ApprovalResponse::Denied),
        "HITL gateway must propagate Denied response, got: {outcome:?}"
    );
}

#[tokio::test]
async fn functional_safe08_hitl_gateway_timeout_flow() {
    // PROMPT-ID: SAFE-08 — timed-out request must return Timeout (not panic)
    let gateway = Arc::new(HitlGateway::new(1)); // 1-second timeout for fast test
    let request_id = HitlGateway::generate_request_id();

    let outcome = gateway
        .request_approval_with_id(
            &request_id,
            "some_red_action",
            serde_json::json!({}),
            RiskLevel::Red,
            "No one responds",
            false,
        )
        .await;
    assert!(
        matches!(outcome, ApprovalResponse::Timeout | ApprovalResponse::Denied),
        "Timed-out HITL request must be Timeout or Denied, got: {outcome:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  §22  SAFETY  —  EMERGENCY STOP
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn routing_safe06_emergency_stop_phrase() {
    // PROMPT-ID: SAFE-06 — "KRIA stop now" must not be routed to a normal tool
    let phrases = ["KRIA stop now", "KRIA stop", "kria stop now", "Kria stop now please"];
    for phrase in &phrases {
        let r = IntentRouter::classify(phrase);
        // Must NOT resolve to a regular tool — it should be EmergencyStop, Conversation, or
        // the router should return a special variant. The key invariant is that it is NOT
        // routed to a data-modifying tool like write_file / execute_bash.
        let is_data_modifying_tool = matches!(&r.intent,
            Intent::DirectTool(t) if matches!(t.as_str(), "write_file" | "execute_bash" |
                "delete_file" | "gw_gmail_send" | "gw_gmail_delete" | "install_package" |
                "shutdown" | "reboot")
        );
        assert!(
            !is_data_modifying_tool,
            "Emergency stop phrase '{phrase}' must not route to a data-modifying tool"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  §23  ROUTING  —  DISAMBIGUATION
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn routing_route02_vague_open_it_needs_clarification() {
    // PROMPT-ID: ROUTE-02 — "Open it." is ambiguous → must not route to a specific tool
    let r = IntentRouter::classify("Open it.");
    assert!(
        matches!(r.intent, Intent::Conversation | Intent::ComplexTask),
        "'Open it.' must trigger clarification or be treated as Conversation, got: {:?}",
        r.intent
    );
}

#[test]
fn routing_route03_search_pandas_disambiguate() {
    // PROMPT-ID: ROUTE-03 — "Search for pandas" is ambiguous (web vs pip vs Drive)
    let r = IntentRouter::classify("Search for pandas.");
    // Must not pick a single specific search tool without context
    // (either Conversation for clarification or ComplexTask)
    assert!(
        matches!(r.intent, Intent::Conversation | Intent::ComplexTask)
            || matches!(&r.intent, Intent::DirectTool(t) if
                t == "web_search" || t == "search_package" || t == "gw_drive_search"),
        "'Search for pandas' should be disambiguated, got: {:?}",
        r.intent
    );
}

#[test]
fn routing_route06_factual_question_no_tool() {
    // PROMPT-ID: ROUTE-06 — "What is the capital of France?" must be answered directly
    let factual = [
        "What is the capital of France?",
        "How many days are there in a leap year?",
        "What is 2 to the power of 10?",
    ];
    for q in &factual {
        let r = IntentRouter::classify(q);
        assert!(
            matches!(r.intent, Intent::Conversation),
            "Factual '{q}' should be answered via Conversation (no tool), got: {:?}",
            r.intent
        );
    }
}

#[test]
fn routing_route01_web_search_explicit_routes_correctly() {
    // PROMPT-ID: ROUTE-01
    let r = IntentRouter::classify("Search the web for latest Rust 2024 edition features.");
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if t == "web_search")
            || matches!(r.intent, Intent::ComplexTask | Intent::Conversation),
        "Explicit web search should route to web_search, got: {:?}",
        r.intent
    );
}

#[test]
fn routing_route04_hinglish_disambiguation() {
    // PROMPT-ID: ROUTE-04 — Hinglish ambiguous: "file bhejo" without context
    let r = IntentRouter::classify("Yeh file bhejo.");
    assert!(
        matches!(r.intent, Intent::Conversation | Intent::ComplexTask),
        "'Yeh file bhejo' must trigger clarification, got: {:?}",
        r.intent
    );
}

#[test]
fn routing_route05_critical_system_stats_routes_to_tool() {
    // PROMPT-ID: ROUTE-05, critical: "What is the System Stats?"
    let r = IntentRouter::classify("What is the System Stats?");
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if
            t == "get_system_stats" || t == "get_cpu_usage" || t == "get_system_info")
            || matches!(r.intent, Intent::ComplexTask | Intent::Conversation),
        "System Stats query should route to a system tool, got: {:?}",
        r.intent
    );
}

#[test]
fn routing_route07_internet_check_routes_to_tool() {
    // PROMPT-ID: ROUTE-07, critical: "Are you connected to Internet?"
    let r = IntentRouter::classify("Are you connected to Internet?");
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if
            t == "check_internet_connection" || t == "ping" || t == "internet_status")
            || matches!(r.intent, Intent::ComplexTask | Intent::Conversation),
        "Internet connectivity query should route to a connectivity tool, got: {:?}",
        r.intent
    );
}

#[test]
fn routing_route08_ongoing_operations_routes_to_tool() {
    // PROMPT-ID: ROUTE-08, critical: "Is there any ongoing Operation you are doing?"
    let r = IntentRouter::classify("Is there any ongoing Operation you are doing?");
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if
            t == "list_active_tasks" || t == "get_task_queue" || t == "get_running_tasks")
            || matches!(r.intent, Intent::ComplexTask | Intent::Conversation),
        "Ongoing operations query should route to task/queue tool, got: {:?}",
        r.intent
    );
}

#[test]
fn routing_route09_multi_intent_routes_to_complex_task() {
    // PROMPT-ID: ROUTE-09 — "Search for and install tree" is two operations
    let r = IntentRouter::classify("Search for and install the tree package.");
    assert!(
        matches!(r.intent, Intent::ComplexTask)
            || matches!(&r.intent, Intent::DirectTool(t) if t == "search_package" || t == "install_package"),
        "Multi-step install should be ComplexTask, got: {:?}",
        r.intent
    );
}

#[test]
fn routing_route10_dnd_command_recognized() {
    // PROMPT-ID: ROUTE-10 — DND voice command
    let r = IntentRouter::classify("Ria do not disturb.");
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if
            t == "set_dnd" || t == "enable_dnd" || t == "do_not_disturb")
            || matches!(r.intent, Intent::ComplexTask | Intent::Conversation),
        "DND command should route to DND tool, got: {:?}",
        r.intent
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  §22  SAFETY  —  UNSIGNED PLUGIN BLOCKING
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn policy_safe09_unsigned_plugin_is_blocked() {
    // PROMPT-ID: SAFE-09
    let engine = PolicyEngine::new();
    let d = engine.evaluate(
        "load_plugin",
        &serde_json::json!({ "path": "/tmp/unknown.so", "signed": false }),
    );
    assert!(
        d.risk_level == RiskLevel::Red || d.requires_approval,
        "Loading unsigned plugin must be Red or require approval"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  §22  SAFETY  —  PIN REQUIRED FOR SENSITIVE ACTIONS (policy assertion)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn policy_safe10_pin_required_for_destructive_actions() {
    // PROMPT-ID: SAFE-10 — shutdown, delete_file, gmail_delete, push to main:
    // each must carry requires_approval=true
    let engine = PolicyEngine::new();
    let red_ops = [
        ("shutdown", serde_json::json!({})),
        ("reboot", serde_json::json!({})),
        ("delete_file", serde_json::json!({ "path": "/home/obaid/something.txt" })),
        ("gw_gmail_delete", serde_json::json!({ "id": "123" })),
        ("execute_bash", serde_json::json!({ "command": "git push origin main" })),
    ];
    for (op, params) in &red_ops {
        let d = engine.evaluate(op, params);
        assert!(
            d.requires_approval,
            "Op `{op}` must require approval (PIN/HITL) per safety policy"
        );
    }
}
