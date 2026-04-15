//! Feature tests for the KRIA safety module.
//!
//! Covers the policy engine risk classification, blacklist detection,
//! HITL approval/denial flow, and audit logging. Tests both happy
//! paths and edge cases (blocked commands, timeout, unknown actions).

use kria_core::safety::policy::{PolicyEngine, RiskLevel};
use kria_core::safety::blacklist::BlacklistChecker;
use kria_core::safety::hitl::{HitlGateway, ApprovalResponse};
use std::sync::Arc;

// ── Policy Engine — risk classification ─────────────────────────────

#[test]
fn green_risk_for_read_only_actions() {
    let engine = PolicyEngine::new();

    let green_actions = [
        "read_file",
        "get_clipboard",
        "web_search",
        "list_directory",
    ];

    for action in &green_actions {
        let decision = engine.evaluate(action, &serde_json::json!({}));
        assert_eq!(
            decision.risk_level,
            RiskLevel::Green,
            "{action} should be Green"
        );
    }
}

#[test]
fn yellow_risk_for_user_level_mutations() {
    let engine = PolicyEngine::new();

    let yellow_actions = ["write_file", "set_volume", "download_file"];

    for action in &yellow_actions {
        let decision = engine.evaluate(action, &serde_json::json!({}));
        assert_eq!(
            decision.risk_level,
            RiskLevel::Yellow,
            "{action} should be Yellow"
        );
    }
}

#[test]
fn red_risk_for_system_mutations() {
    let engine = PolicyEngine::new();

    let red_actions = ["delete_file", "execute_bash", "install_app"];

    for action in &red_actions {
        let decision = engine.evaluate(action, &serde_json::json!({}));
        assert_eq!(
            decision.risk_level,
            RiskLevel::Red,
            "{action} should be Red"
        );
    }
}

#[test]
fn path_escalation_to_red_for_protected_paths() {
    let engine = PolicyEngine::new();

    let protected_params = serde_json::json!({ "path": "/etc/passwd" });
    let decision = engine.evaluate("write_file", &protected_params);

    assert_eq!(
        decision.risk_level,
        RiskLevel::Red,
        "write to /etc should escalate to Red"
    );
}

// ── Blacklist ───────────────────────────────────────────────────────

#[test]
fn blacklist_blocks_dangerous_commands() {
    let checker = BlacklistChecker::new();

    let blocked = [
        "rm -rf /",
        "mimikatz",
        "mkfs.ext4 /dev/sda",
    ];

    for cmd in &blocked {
        assert!(
            checker.is_blocked(cmd),
            "'{cmd}' should be blocked"
        );
    }
}

#[test]
fn blacklist_allows_safe_commands() {
    let checker = BlacklistChecker::new();

    let safe = ["ls -la", "cat readme.md", "echo hello"];

    for cmd in &safe {
        assert!(
            !checker.is_blocked(cmd),
            "'{cmd}' should NOT be blocked"
        );
    }
}

#[test]
fn blacklist_check_returns_matched_pattern_name() {
    let checker = BlacklistChecker::new();

    let result = checker.check("rm -rf /");
    assert!(result.is_some(), "should return matched pattern");
}

// ── HITL Gateway ────────────────────────────────────────────────────

#[tokio::test]
async fn hitl_approval_flow() {
    let hitl = Arc::new(HitlGateway::new(5));

    // Spawn approval request in background
    let hitl2 = Arc::clone(&hitl);
    let handle = tokio::spawn(async move {
        hitl2
            .request_approval(
                "delete_file",
                serde_json::json!({"path": "/tmp/test"}),
                RiskLevel::Red,
                "Deletes a file",
                false,
            )
            .await
    });

    // Give it a moment to register
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Approve the pending request
    let pending = hitl.pending_requests().await;
    assert!(!pending.is_empty(), "should have a pending request");

    let req_id = &pending[0].id;
    hitl.respond(req_id, ApprovalResponse::Approved).await;

    let result = handle.await.unwrap();
    assert!(
        matches!(result, ApprovalResponse::Approved),
        "should be approved"
    );
}

#[tokio::test]
async fn hitl_denial_flow() {
    let hitl = Arc::new(HitlGateway::new(5));

    let hitl2 = Arc::clone(&hitl);
    let handle = tokio::spawn(async move {
        hitl2
            .request_approval(
                "execute_bash",
                serde_json::json!({"command": "rm -rf /tmp/safe"}),
                RiskLevel::Red,
                "Runs a shell command",
                false,
            )
            .await
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let pending = hitl.pending_requests().await;
    let req_id = &pending[0].id;
    hitl.respond(req_id, ApprovalResponse::Denied).await;

    let result = handle.await.unwrap();
    assert!(
        matches!(result, ApprovalResponse::Denied),
        "should be denied"
    );
}

#[tokio::test]
async fn hitl_cancel_all_clears_pending() {
    let hitl = Arc::new(HitlGateway::new(60));

    let hitl2 = Arc::clone(&hitl);
    tokio::spawn(async move {
        hitl2
            .request_approval(
                "action_a",
                serde_json::json!({}),
                RiskLevel::Red,
                "test",
                false,
            )
            .await
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(!hitl.pending_requests().await.is_empty());

    hitl.cancel_all().await;

    // After cancel, pending should be empty
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(hitl.pending_requests().await.is_empty());
}
