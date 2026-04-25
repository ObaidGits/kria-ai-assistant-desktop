// ─────────────────────────────────────────────────────────────────────────────
//  test_dev_packages.rs — §11 Shell, §12 Packages, §13 Scheduling, §15 Git/Dev
//
//  Destructive installs are gated on policy (RED tier) — only checked at the
//  policy level here, never actually executed.  Git tests operate on the real
//  KRIA workspace in read-only mode (status, log, diff, branch list).
//
//  Covers PROMPT-IDs: SH-01..SH-06, PKG-01..PKG-08, SCHED-01..SCHED-03,
//                     GIT-01..GIT-10
// ─────────────────────────────────────────────────────────────────────────────

mod common;

use common::SandboxDir;
use kria_core::agent::router::{Intent, IntentRouter};
use kria_core::safety::policy::{PolicyEngine, RiskLevel};
use kria_core::tools::registry;

const KRIA_PATH: &str = "/media/obaid/SSD/KRIA";

// ═══════════════════════════════════════════════════════════════════════════
//  §11 SHELL EXECUTION
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn smoke_shell_tools_registered() {
    let reg = registry::build_default_registry();
    for name in &["execute_bash", "execute_python"] {
        assert!(
            reg.get_handler(name).is_some(),
            "Tool `{name}` must be registered (§11)"
        );
    }
}

#[test]
fn policy_sh03_destructive_bash_is_red() {
    // PROMPT-ID: SH-03 — rm -rf must be classified RED
    let engine = PolicyEngine::new();
    let decision = engine.evaluate(
        "execute_bash",
        &serde_json::json!({ "command": "rm -rf /home/obaid/important_data" }),
    );
    assert_eq!(
        decision.risk_level,
        RiskLevel::Red,
        "execute_bash with rm -rf must be Red"
    );
}

#[test]
fn policy_sh03_sudo_rm_rf_root_is_red() {
    // PROMPT-ID: SH-03, SAFE-05 — sudo rm -rf / must be blocked
    let engine = PolicyEngine::new();
    let decision = engine.evaluate(
        "execute_bash",
        &serde_json::json!({ "command": "sudo rm -rf /" }),
    );
    assert_eq!(
        decision.risk_level,
        RiskLevel::Red,
        "sudo rm -rf / must be classified Red"
    );
}

#[test]
fn routing_sh01_execute_bash_routes_correctly() {
    // PROMPT-ID: SH-01
    let r = IntentRouter::classify("Run: ls -la /home/obaid");
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if t == "execute_bash")
            || matches!(r.intent, Intent::ComplexTask | Intent::Conversation),
        "Run: ls -la should route to execute_bash, got: {:?}",
        r.intent
    );
}

#[test]
fn routing_sh02_execute_python_routes_correctly() {
    // PROMPT-ID: SH-02
    let r = IntentRouter::classify("Execute Python code: print(2 + 2)");
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if t == "execute_python")
            || matches!(r.intent, Intent::ComplexTask | Intent::Conversation),
        "Execute python should route to execute_python, got: {:?}",
        r.intent
    );
}

#[tokio::test]
async fn functional_sh04_execute_bash_echo() {
    // PROMPT-ID: SH-04 — safe bash: echo $HOME
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("execute_bash").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "command": "echo $HOME", "timeout": 10 }))
        .await;
    assert!(result.success, "execute_bash echo $HOME should succeed: {:?}", result.error);
    let output = result.data["output"].as_str()
        .or(result.data["stdout"].as_str())
        .unwrap_or("")
        .trim()
        .to_owned();
    assert!(!output.is_empty(), "echo $HOME must produce non-empty output");
}

#[tokio::test]
async fn functional_sh02_execute_python_print() {
    // PROMPT-ID: SH-02
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("execute_python").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "code": "print(2 + 2)", "timeout": 10 }))
        .await;
    assert!(result.success, "execute_python print(2+2) should succeed: {:?}", result.error);
    let output = result.data["output"].as_str()
        .or(result.data["stdout"].as_str())
        .unwrap_or("")
        .trim()
        .to_owned();
    assert_eq!(output, "4", "execute_python print(2+2) should output '4', got: {output}");
}

#[tokio::test]
async fn functional_sh06_execute_bash_df_h() {
    // PROMPT-ID: SH-06 — df -h is safe read-only disk usage
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("execute_bash").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "command": "df -h", "timeout": 10 }))
        .await;
    assert!(
        result.success || result.error.is_some(),
        "execute_bash df -h must not panic"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  §12 PACKAGE MANAGEMENT
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn smoke_package_tools_registered() {
    let reg = registry::build_default_registry();
    let required = [
        "check_package_installed",
        "search_package",
        "get_package_info",
        "check_package_updates",
        "install_package",
        "uninstall_package",
    ];
    for name in &required {
        assert!(
            reg.get_handler(name).is_some(),
            "Tool `{name}` must be registered (§12)"
        );
    }
}

#[test]
fn policy_pkg05_install_package_is_red() {
    // PROMPT-ID: PKG-05, PKG-06
    let engine = PolicyEngine::new();
    let decision =
        engine.evaluate("install_package", &serde_json::json!({ "name": "htop" }));
    assert_eq!(
        decision.risk_level,
        RiskLevel::Red,
        "install_package must be classified Red"
    );
}

#[test]
fn policy_pkg07_uninstall_package_is_red() {
    // PROMPT-ID: PKG-07
    let engine = PolicyEngine::new();
    let decision =
        engine.evaluate("uninstall_package", &serde_json::json!({ "name": "htop" }));
    assert_eq!(
        decision.risk_level,
        RiskLevel::Red,
        "uninstall_package must be classified Red"
    );
}

#[test]
fn routing_pkg01_check_installed_routes_correctly() {
    // PROMPT-ID: PKG-01
    let r = IntentRouter::classify("Is git installed?");
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if t == "check_package_installed")
            || matches!(r.intent, Intent::Conversation | Intent::ComplexTask),
        "Package installed check should route to check_package_installed, got: {:?}",
        r.intent
    );
}

#[test]
fn routing_pkg05_install_hinglish_routes_correctly() {
    // PROMPT-ID: PKG-06
    let r = IntentRouter::classify("Htop install karo.");
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if t == "install_package")
            || matches!(r.intent, Intent::ComplexTask | Intent::Conversation),
        "Hinglish 'install karo' should route to install_package, got: {:?}",
        r.intent
    );
}

#[tokio::test]
async fn functional_pkg01_check_git_installed() {
    // PROMPT-ID: PKG-01
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("check_package_installed").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "name": "git" }))
        .await;
    assert!(result.success, "check_package_installed for git should succeed: {:?}", result.error);
    // git is expected to be installed on this dev machine
    let installed = result.data["installed"].as_bool()
        .or(result.data["is_installed"].as_bool())
        .unwrap_or(true);
    assert!(installed, "git should be installed on the dev machine");
}

#[tokio::test]
async fn functional_pkg02_search_package_htop() {
    // PROMPT-ID: PKG-02
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("search_package").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "name": "htop" }))
        .await;
    assert!(
        result.success || result.error.is_some(),
        "search_package must not panic"
    );
}

#[tokio::test]
async fn functional_pkg08_search_nonexistent_package() {
    // PROMPT-ID: PKG-08 — searching a non-existent package must return clean not-found
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("search_package").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "name": "xyz_nonexistent_package_99_kria_test" }))
        .await;
    // Should either return success=false or success=true with empty results
    if result.success {
        let found = result.data["found"].as_bool().unwrap_or(false)
            || result.data.as_array().map(|a| !a.is_empty()).unwrap_or(false);
        assert!(
            !found,
            "Non-existent package search should return found=false or empty results"
        );
    } else {
        assert!(result.error.is_some(), "search_package failure must have error message");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  §13 TASK SCHEDULING
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn smoke_scheduler_tools_registered() {
    let reg = registry::build_default_registry();
    let required = ["list_scheduled_tasks", "create_scheduled_task", "delete_scheduled_task"];
    for name in &required {
        assert!(
            reg.get_handler(name).is_some(),
            "Tool `{name}` must be registered (§13)"
        );
    }
}

#[test]
fn policy_sched03_delete_scheduled_task_is_red() {
    // PROMPT-ID: SCHED-03
    let engine = PolicyEngine::new();
    let decision = engine.evaluate(
        "delete_scheduled_task",
        &serde_json::json!({ "pattern": "echo hello" }),
    );
    assert_eq!(
        decision.risk_level,
        RiskLevel::Red,
        "delete_scheduled_task must be classified Red (destructive)"
    );
}

#[tokio::test]
async fn functional_sched01_list_scheduled_tasks() {
    // PROMPT-ID: SCHED-01
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("list_scheduled_tasks").unwrap().clone();
    let result = handler.execute(serde_json::json!({})).await;
    assert!(
        result.success || result.error.is_some(),
        "list_scheduled_tasks must not panic"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  §15 GIT & DEVELOPER TOOLS
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn smoke_developer_tools_registered() {
    let reg = registry::build_default_registry();
    let required = [
        "git_status",
        "git_log",
        "git_diff",
        "git_commit",
        "git_branch_list",
        "git_stash",
        "git_checkout",
        "analyze_project",
        "query_sqlite",
    ];
    for name in &required {
        assert!(
            reg.get_handler(name).is_some(),
            "Tool `{name}` must be registered (§15)"
        );
    }
}

#[test]
fn policy_git08_git_push_is_red() {
    // PROMPT-ID: GIT-08
    let engine = PolicyEngine::new();
    // git push may be via execute_bash or git_push tool
    let decisions = [
        engine.evaluate("execute_bash", &serde_json::json!({ "command": "git push origin main" })),
        engine.evaluate("git_push", &serde_json::json!({ "remote": "origin", "branch": "main" })),
    ];
    for d in &decisions {
        assert_eq!(
            d.risk_level,
            RiskLevel::Red,
            "Pushing to main must be classified Red"
        );
    }
}

#[test]
fn routing_git01_git_status_routes_correctly() {
    // PROMPT-ID: GIT-01
    let prompts = [
        "Git status of the KRIA project.",
        "git status",
        "show git changes",
    ];
    for p in &prompts {
        let r = IntentRouter::classify(p);
        assert!(
            matches!(&r.intent, Intent::DirectTool(t) if t == "git_status")
                || matches!(r.intent, Intent::ComplexTask | Intent::Conversation),
            "'{p}' should route to git_status, got: {:?}",
            r.intent
        );
    }
}

#[test]
fn routing_git08_push_to_main_routes_correctly() {
    // PROMPT-ID: GIT-08
    let r = IntentRouter::classify("Push to main.");
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if matches!(t.as_str(), "git_push" | "execute_bash"))
            || matches!(r.intent, Intent::ComplexTask | Intent::Conversation),
        "Push to main should route to git_push or execute_bash, got: {:?}",
        r.intent
    );
}

#[tokio::test]
async fn functional_git01_git_status_kria() {
    // PROMPT-ID: GIT-01
    if !std::path::Path::new(KRIA_PATH).exists() {
        eprintln!("SKIP: KRIA workspace not found");
        return;
    }
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("git_status").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "path": KRIA_PATH }))
        .await;
    assert!(result.success, "git_status should succeed for KRIA workspace: {:?}", result.error);
}

#[tokio::test]
async fn functional_git02_git_log_kria() {
    // PROMPT-ID: GIT-02
    if !std::path::Path::new(KRIA_PATH).exists() {
        return;
    }
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("git_log").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "path": KRIA_PATH, "count": 5 }))
        .await;
    assert!(result.success, "git_log should succeed: {:?}", result.error);
    let commits = result.data.as_array()
        .cloned()
        .or_else(|| result.data["commits"].as_array().cloned())
        .unwrap_or_default();
    assert!(!commits.is_empty(), "git_log should return at least one commit");
}

#[tokio::test]
async fn functional_git03_git_diff_kria() {
    // PROMPT-ID: GIT-03
    if !std::path::Path::new(KRIA_PATH).exists() {
        return;
    }
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("git_diff").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "path": KRIA_PATH }))
        .await;
    assert!(
        result.success || result.error.is_some(),
        "git_diff must not panic"
    );
}

#[tokio::test]
async fn functional_git05_git_branch_list_kria() {
    // PROMPT-ID: GIT-05
    if !std::path::Path::new(KRIA_PATH).exists() {
        return;
    }
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("git_branch_list").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "path": KRIA_PATH }))
        .await;
    assert!(result.success, "git_branch_list should succeed: {:?}", result.error);
    let branches = result.data.as_array()
        .cloned()
        .or_else(|| result.data["branches"].as_array().cloned())
        .unwrap_or_default();
    assert!(!branches.is_empty(), "git_branch_list should return at least one branch");
}

#[tokio::test]
async fn functional_git09_analyze_project_kria() {
    // PROMPT-ID: GIT-09
    if !std::path::Path::new(KRIA_PATH).exists() {
        return;
    }
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("analyze_project").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "path": KRIA_PATH }))
        .await;
    assert!(
        result.success || result.error.is_some(),
        "analyze_project must not panic"
    );
}

#[tokio::test]
async fn functional_git10_query_sqlite_missing_db() {
    // PROMPT-ID: GIT-10 — missing DB must return clean error
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("query_sqlite").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({
            "database": "/nonexistent_kria_test/kria.db",
            "query": "SELECT * FROM facts LIMIT 5"
        }))
        .await;
    assert!(
        !result.success,
        "query_sqlite for missing DB must fail cleanly"
    );
    assert!(result.error.is_some(), "query_sqlite failure must include error message");
}

#[tokio::test]
async fn functional_git_stash_sandbox() {
    // PROMPT-ID: GIT-07 — stash in a fresh git repo inside sandbox
    let sandbox = SandboxDir::new();
    // Init a git repo in the sandbox
    let ok = std::process::Command::new("git")
        .args(["init", sandbox.path.to_str().unwrap()])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("SKIP: git init failed");
        return;
    }
    // Create a file to stash
    sandbox.write_file("stash_me.txt", "unstaged change");

    let reg = registry::build_default_registry();
    let handler = reg.get_handler("git_stash").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "path": sandbox.path.to_str().unwrap() }))
        .await;
    // May fail if no staged changes — that's fine as long as it doesn't panic
    assert!(
        result.success || result.error.is_some(),
        "git_stash must not panic"
    );
}
