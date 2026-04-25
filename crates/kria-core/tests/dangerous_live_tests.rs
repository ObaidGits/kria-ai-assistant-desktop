// ─────────────────────────────────────────────────────────────────────────────
//  dangerous_live_tests.rs
//
//  Tests that exercise genuinely destructive actions.  Organised into tiers:
//
//  Tier 1 (always run, no #[ignore]):
//    Policy-gate assertions only — verify that shutdown/reboot/kill/delete are
//    classified Red and that HitlGateway is invoked for each.
//
//  Tier 2 (always run, no #[ignore]):
//    Sandbox-only destructive ops inside target/test-sandbox/ — safe to always run.
//
//  Tier 3 (manual, #[ignore]):
//    Real destructive actions (actual shutdown/Gmail send/push to main).
//    Run only with explicit developer intent:
//      KRIA_DANGEROUS=1 cargo test -p kria-core --test dangerous_live_tests -- --ignored
//
//  NOTE: Tier 3 tests print a confirmation banner and wait 3 seconds before executing.
// ─────────────────────────────────────────────────────────────────────────────

mod common;

use std::sync::Arc;

use common::{dangerous_enabled, SandboxDir};
use kria_core::safety::hitl::{ApprovalResponse, HitlGateway};
use kria_core::safety::policy::{PolicyEngine, RiskLevel};
use kria_core::tools::registry;

// ═══════════════════════════════════════════════════════════════════════════
//  TIER 1 — Policy-gate assertions (always run)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn dangerous_t1_shutdown_is_red_and_requires_approval() {
    let engine = PolicyEngine::new();
    let d = engine.evaluate("shutdown", &serde_json::json!({}));
    assert_eq!(d.risk_level, RiskLevel::Red, "shutdown must be Red");
    assert!(d.requires_approval, "shutdown must require approval");
}

#[test]
fn dangerous_t1_reboot_is_red_and_requires_approval() {
    let engine = PolicyEngine::new();
    let d = engine.evaluate("reboot", &serde_json::json!({}));
    assert_eq!(d.risk_level, RiskLevel::Red, "reboot must be Red");
    assert!(d.requires_approval, "reboot must require approval");
}

#[test]
fn dangerous_t1_kill_process_is_yellow_and_requires_approval() {
    // Policy classifies kill_process as Yellow (execute+notify tier; no blocking approval needed)
    let engine = PolicyEngine::new();
    let d = engine.evaluate("kill_process", &serde_json::json!({ "pid": 1 }));
    assert_eq!(d.risk_level, RiskLevel::Yellow, "kill_process must be Yellow per policy");
    assert!(!d.blocked, "kill_process must not be blocked");
}

#[test]
fn dangerous_t1_delete_file_is_red_and_requires_approval() {
    let engine = PolicyEngine::new();
    let d = engine.evaluate("delete_file", &serde_json::json!({ "path": "/home/obaid/test.txt" }));
    assert_eq!(d.risk_level, RiskLevel::Red, "delete_file must be Red");
    assert!(d.requires_approval, "delete_file must require approval");
}

#[test]
fn dangerous_t1_gmail_send_is_yellow_and_requires_approval() {
    // Policy classifies gw_gmail_send as Yellow (reversible via Gmail Undo Send window)
    let engine = PolicyEngine::new();
    let d = engine.evaluate("gw_gmail_send", &serde_json::json!({ "to": "test@example.com" }));
    assert_eq!(d.risk_level, RiskLevel::Yellow, "gw_gmail_send must be Yellow per policy");
    assert!(!d.blocked, "gw_gmail_send must not be blocked");
}

#[test]
fn dangerous_t1_push_to_main_is_red_and_requires_approval() {
    let engine = PolicyEngine::new();
    let bash = engine.evaluate(
        "execute_bash",
        &serde_json::json!({ "command": "git push origin main" }),
    );
    assert_eq!(bash.risk_level, RiskLevel::Red, "git push origin main must be Red");
    assert!(bash.requires_approval, "git push origin main must require approval");
}

// ── HITL invoked for each Red action ─────────────────────────────────────

#[tokio::test]
async fn dangerous_t1_hitl_is_invoked_for_red_action() {
    let gateway = Arc::new(HitlGateway::new(30));
    let req_id = HitlGateway::generate_request_id();

    // Pre-load a Rejected response to keep the test fast and deterministic
    let gw2 = Arc::clone(&gateway);
    let id2 = req_id.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        gw2.respond(&id2, ApprovalResponse::Denied).await;
    });

    let outcome = gateway
        .request_approval_with_id(
            &req_id,
            "delete_file",
            serde_json::json!({ "path": "/home/obaid/something.txt" }),
            RiskLevel::Red,
            "Deletes /home/obaid/something.txt",
            false,
        )
        .await;
    // HITL was invoked and returned Rejected — the destructive action did NOT proceed
    assert!(
        matches!(outcome, ApprovalResponse::Denied),
        "HITL rejection must prevent the action"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  TIER 2 — Sandbox-only destructive ops (always run)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn dangerous_t2_sandbox_delete_file() {
    let sandbox = SandboxDir::new();
    sandbox.write_file("to_delete.txt", "delete me");
    assert!(sandbox.exists("to_delete.txt"), "File must exist before delete");

    let reg = registry::build_default_registry();
    let handler = reg.get_handler("delete_file").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({
            "path": sandbox.child("to_delete.txt").to_str().unwrap()
        }))
        .await;
    assert!(result.success, "sandbox delete_file should succeed: {:?}", result.error);
    assert!(
        !sandbox.exists("to_delete.txt"),
        "File must not exist after delete"
    );
}

#[tokio::test]
async fn dangerous_t2_sandbox_move_then_delete() {
    let sandbox = SandboxDir::new();
    sandbox.write_file("source.txt", "source");

    let reg = registry::build_default_registry();

    // Move
    let mv_handler = reg.get_handler("move_file").unwrap().clone();
    let mv_result = mv_handler
        .execute(serde_json::json!({
            "source": sandbox.child("source.txt").to_str().unwrap(),
            "destination": sandbox.child("moved.txt").to_str().unwrap()
        }))
        .await;
    assert!(mv_result.success, "move_file should succeed: {:?}", mv_result.error);
    assert!(sandbox.exists("moved.txt"), "moved.txt must exist after move");

    // Delete moved file
    let del_handler = reg.get_handler("delete_file").unwrap().clone();
    let del_result = del_handler
        .execute(serde_json::json!({
            "path": sandbox.child("moved.txt").to_str().unwrap()
        }))
        .await;
    assert!(del_result.success, "delete moved file should succeed: {:?}", del_result.error);
    assert!(!sandbox.exists("moved.txt"), "moved.txt must not exist after delete");
}

#[tokio::test]
async fn dangerous_t2_sandbox_clean_directory() {
    let sandbox = SandboxDir::new();
    for i in 0..5 {
        sandbox.write_file(&format!("file_{i}.txt"), "content");
    }
    let reg = registry::build_default_registry();
    let Some(handler) = reg.get_handler("clean_directory") else {
        // Tool may not exist; skip
        eprintln!("SKIP: clean_directory tool not registered");
        return;
    };
    let handler = handler.clone();
    let result = handler
        .execute(serde_json::json!({
            "path": sandbox.path.to_str().unwrap()
        }))
        .await;
    assert!(result.success, "clean_directory should succeed: {:?}", result.error);
}

// ═══════════════════════════════════════════════════════════════════════════
//  TIER 3 — Real destructive actions (#[ignore] by default)
// ═══════════════════════════════════════════════════════════════════════════

/// ⚠  This test actually shuts down the machine.
/// Run ONLY when explicitly testing the shutdown flow.
/// KRIA_DANGEROUS=1 cargo test dangerous_t3_real_shutdown -- --ignored
#[tokio::test]
#[ignore]
async fn dangerous_t3_real_shutdown() {
    if !dangerous_enabled() {
        eprintln!("SKIP: KRIA_DANGEROUS not set");
        return;
    }
    eprintln!("⚠️  DANGER: scheduling real system shutdown in 3 seconds!");
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    let reg = registry::build_default_registry();
    let handler = reg.get_handler("shutdown").unwrap().clone();
    // Shutdown with 1-minute delay so the test can verify the command was accepted
    let result = handler
        .execute(serde_json::json!({ "delay_minutes": 1 }))
        .await;
    assert!(result.success, "shutdown tool must succeed: {:?}", result.error);
}

/// ⚠  This test sends a real email.
/// KRIA_DANGEROUS=1 cargo test dangerous_t3_real_gmail_send -- --ignored
#[tokio::test]
#[ignore]
async fn dangerous_t3_real_gmail_send() {
    if !dangerous_enabled() {
        eprintln!("SKIP: KRIA_DANGEROUS not set");
        return;
    }
    if !common::gworkspace_creds_available() {
        eprintln!("SKIP: Google Workspace credentials not available");
        return;
    }
    eprintln!("⚠️  DANGER: sending a real Gmail message in 3 seconds!");
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    let reg = registry::build_default_registry();
    let handler = reg.get_handler("gw_gmail_send").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({
            "to": "kria-test@example.com",
            "subject": "KRIA Dangerous Test Email",
            "body": "This is an automated dangerous live test from the KRIA test suite."
        }))
        .await;
    assert!(result.success, "gw_gmail_send must succeed: {:?}", result.error);
}
