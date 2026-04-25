// ─────────────────────────────────────────────────────────────────────────────
//  test_files.rs — §4 File System Tools (sandboxed)
//
//  All destructive / write operations run inside SandboxDir
//  (target/test-sandbox/<uuid>/) and are auto-cleaned on test exit.
//
//  Covers PROMPT-IDs: FS-01..FS-19
// ─────────────────────────────────────────────────────────────────────────────

mod common;

use common::{assert_tool_success, SandboxDir};
use kria_core::agent::router::{Intent, IntentRouter};
use kria_core::safety::policy::{PolicyEngine, RiskLevel};
use kria_core::tools::registry;

// ═══════════════════════════════════════════════════════════════════════════
//  Smoke — all file tools must be registered
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn smoke_fs_tools_registered() {
    // PROMPT-ID: FS-01..FS-19
    let reg = registry::build_default_registry();
    let required = [
        "read_file",
        "write_file",
        "delete_file",
        "copy_file",
        "rename_file",
        "move_file",
        "get_file_info",
        "calculate_dir_size",
        "list_directory",
        "search_files",
        "search_file_contents",
        "find_files_by_pattern",
        "get_project_structure",
        "count_lines_of_code",
        "diff_files",
        "find_todos",
        "analyze_code",
        "clean_temp_files",
    ];
    for name in &required {
        assert!(
            reg.get_handler(name).is_some(),
            "Tool `{name}` must be registered (required by §4)"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Routing — §4 prompts
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn routing_fs01_read_file_routes_correctly() {
    // PROMPT-ID: FS-01
    let prompts = [
        "Read the file /home/obaid/README.md",
        "read file /tmp/test.txt",
        "show the file /home/obaid/notes.txt",
    ];
    for p in &prompts {
        let r = IntentRouter::classify(p);
        assert!(
            matches!(&r.intent, Intent::DirectTool(t) if t == "read_file"),
            "'{p}' should route to read_file, got: {:?}",
            r.intent
        );
    }
}

#[test]
fn routing_fs02_list_directory_routes_correctly() {
    // PROMPT-ID: FS-02
    let prompts = [
        "List files in /home/obaid/Documents",
        "list the files in /tmp",
        "list folder /home/obaid",
    ];
    for p in &prompts {
        let r = IntentRouter::classify(p);
        assert!(
            matches!(&r.intent, Intent::DirectTool(t) if t == "list_directory"),
            "'{p}' should route to list_directory, got: {:?}",
            r.intent
        );
    }
}

#[test]
fn routing_fs03_search_files_routes_correctly() {
    // PROMPT-ID: FS-03
    let prompts = [
        "find files matching *.pdf in /home/obaid",
        "find all .rs files in /media/obaid/SSD/KRIA",
        "search files matching *.log in /var/log",
    ];
    for p in &prompts {
        let r = IntentRouter::classify(p);
        assert!(
            matches!(&r.intent, Intent::DirectTool(t) if matches!(t.as_str(), "search_files" | "find_files_by_pattern"))
                || matches!(r.intent, Intent::ComplexTask),
            "'{p}' should route to a file-search tool, got: {:?}",
            r.intent
        );
    }
}

#[test]
fn routing_fs13_search_file_contents_routes_correctly() {
    // PROMPT-ID: FS-13
    let r = IntentRouter::classify("search inside .py files for the pattern 'import os' under /home/obaid/projects");
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if t == "search_file_contents")
            || matches!(r.intent, Intent::ComplexTask),
        "Content search should route to search_file_contents, got: {:?}",
        r.intent
    );
}

#[test]
fn routing_fs14_get_project_structure_routes_correctly() {
    // PROMPT-ID: FS-14
    let r = IntentRouter::classify("Show me the project structure of /media/obaid/SSD/KRIA");
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if t == "get_project_structure")
            || matches!(r.intent, Intent::ComplexTask),
        "Project structure should route to get_project_structure, got: {:?}",
        r.intent
    );
}

#[test]
fn routing_fs19_analyze_code_routes_correctly() {
    // PROMPT-ID: FS-19
    let r = IntentRouter::classify(
        "Analyse the code in /media/obaid/SSD/KRIA/crates/kria-core/src/agent/loop_engine.rs",
    );
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if t == "analyze_code")
            || matches!(r.intent, Intent::ComplexTask),
        "Code analysis should route to analyze_code, got: {:?}",
        r.intent
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  Functional — read operations against real filesystem
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn functional_fs01_read_existing_file() {
    // PROMPT-ID: FS-01
    // Read the KRIA Cargo.toml — always exists
    let path = "/media/obaid/SSD/KRIA/Cargo.toml";
    if !std::path::Path::new(path).exists() {
        return; // CI may not have this path
    }
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("read_file").unwrap().clone();
    let result = handler.execute(serde_json::json!({ "path": path })).await;
    assert!(result.success, "read_file should succeed for Cargo.toml: {:?}", result.error);
    let content = result.data["content"].as_str().unwrap_or("");
    assert!(!content.is_empty(), "read_file must return non-empty content");
}

#[tokio::test]
async fn functional_fs01_read_missing_file_returns_error() {
    // PROMPT-ID: FS-01 — missing file must return clean error, not panic
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("read_file").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "path": "/nonexistent_path_kria_test_xyz/file.txt" }))
        .await;
    assert!(
        !result.success,
        "read_file for missing path must return success=false"
    );
    assert!(
        result.error.is_some(),
        "read_file for missing path must include an error message"
    );
}

#[tokio::test]
async fn functional_fs02_list_directory() {
    // PROMPT-ID: FS-02
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("list_directory").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "path": "/media/obaid/SSD/KRIA" }))
        .await;
    if !result.success {
        // May be permission-denied in CI — that's fine as long as it's clean
        assert!(result.error.is_some(), "list_directory failure must have error field");
        return;
    }
    assert!(
        result.data.is_array() || result.data.get("entries").is_some(),
        "list_directory must return an array or entries object"
    );
}

#[tokio::test]
async fn functional_fs10_calculate_dir_size() {
    // PROMPT-ID: FS-10
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("calculate_dir_size").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "path": "/media/obaid/SSD/KRIA/crates" }))
        .await;
    assert!(
        result.success || result.error.is_some(),
        "calculate_dir_size must not panic"
    );
    if result.success {
        assert!(
            result.data.get("total_bytes").or(result.data.get("size_bytes")).or(result.data.get("size_mb")).is_some(),
            "calculate_dir_size must include a size field"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Functional — sandboxed write / copy / rename / move / delete operations
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn functional_fs05_write_file_sandbox() {
    // PROMPT-ID: FS-05
    let sandbox = SandboxDir::new();
    let path = sandbox.child("hello.txt");
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("write_file").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({
            "path": path.to_str().unwrap(),
            "content": "Hello KRIA",
            "overwrite": false
        }))
        .await;
    assert!(result.success, "write_file should succeed in sandbox: {:?}", result.error);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap_or_default(),
        "Hello KRIA"
    );
}

#[tokio::test]
async fn functional_fs05_write_file_overwrite_false_fails_if_exists() {
    // PROMPT-ID: FS-05 — overwrite:false on existing file must return error
    let sandbox = SandboxDir::new();
    let path = sandbox.write_file("exist.txt", "original content");
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("write_file").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({
            "path": path.to_str().unwrap(),
            "content": "new content",
            "overwrite": false
        }))
        .await;
    assert!(
        !result.success,
        "write_file with overwrite:false on existing file should fail"
    );
}

#[tokio::test]
async fn functional_fs06_copy_file_sandbox() {
    // PROMPT-ID: FS-06
    let sandbox = SandboxDir::new();
    let src = sandbox.write_file("source.txt", "copy me");
    let dst = sandbox.child("dest.txt");
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("copy_file").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({
            "source": src.to_str().unwrap(),
            "destination": dst.to_str().unwrap()
        }))
        .await;
    assert!(result.success, "copy_file should succeed in sandbox: {:?}", result.error);
    assert_eq!(
        std::fs::read_to_string(&dst).unwrap_or_default(),
        "copy me"
    );
}

#[tokio::test]
async fn functional_fs07_rename_file_sandbox() {
    // PROMPT-ID: FS-07
    let sandbox = SandboxDir::new();
    let original = sandbox.write_file("original.txt", "rename me");
    let renamed = sandbox.child("renamed.txt");
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("rename_file").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({
            "source": original.to_str().unwrap(),
            "destination": renamed.to_str().unwrap()
        }))
        .await;
    assert!(result.success, "rename_file should succeed in sandbox: {:?}", result.error);
    assert!(!original.exists(), "original file should be gone after rename");
    assert!(renamed.exists(), "renamed file should exist");
}

#[tokio::test]
async fn functional_fs08_move_file_sandbox() {
    // PROMPT-ID: FS-08
    let sandbox = SandboxDir::new();
    let src = sandbox.write_file("moveme.txt", "move this");
    let dst_dir = sandbox.child("subdir");
    std::fs::create_dir_all(&dst_dir).ok();
    let dst = dst_dir.join("moveme.txt");
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("move_file").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({
            "source": src.to_str().unwrap(),
            "destination": dst.to_str().unwrap()
        }))
        .await;
    assert!(result.success, "move_file should succeed in sandbox: {:?}", result.error);
    assert!(!src.exists(), "source file should be gone after move");
    assert!(dst.exists(), "destination file should exist after move");
}

#[tokio::test]
async fn functional_fs09_get_file_info_sandbox() {
    // PROMPT-ID: FS-09
    let sandbox = SandboxDir::new();
    let file = sandbox.write_file("info_test.txt", "check my info");
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("get_file_info").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "path": file.to_str().unwrap() }))
        .await;
    assert!(result.success, "get_file_info should succeed: {:?}", result.error);
    // Must include at least size
    assert!(
        result.data.get("size").or(result.data.get("size_bytes")).or(result.data.get("bytes")).is_some(),
        "get_file_info must include a size field"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  Policy — destructive file operations must be RED
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn policy_fs11_delete_file_is_red() {
    // PROMPT-ID: FS-11
    let engine = PolicyEngine::new();
    let decision = engine.evaluate(
        "delete_file",
        &serde_json::json!({ "path": "/home/obaid/something.txt" }),
    );
    assert_eq!(
        decision.risk_level,
        RiskLevel::Red,
        "delete_file must be classified Red"
    );
}

#[test]
fn policy_fs11_delete_blocked_path_is_red() {
    // PROMPT-ID: FS-11, SAFE-01 — /etc paths always Red
    let engine = PolicyEngine::new();
    let decision = engine.evaluate(
        "delete_file",
        &serde_json::json!({ "path": "/etc/passwd" }),
    );
    assert_eq!(
        decision.risk_level,
        RiskLevel::Red,
        "delete_file on /etc must be Red"
    );
}

#[test]
fn policy_write_to_blocked_path_escalates_to_red() {
    // PROMPT-ID: FS-05 + SAFE-01
    let engine = PolicyEngine::new();
    let decision = engine.evaluate(
        "write_file",
        &serde_json::json!({ "path": "/etc/hosts" }),
    );
    assert_eq!(
        decision.risk_level,
        RiskLevel::Red,
        "write_file to /etc should escalate to Red"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  Functional — diff, find_todos, code analysis (read-only on real workspace)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn functional_fs17_diff_files_sandbox() {
    // PROMPT-ID: FS-17
    let sandbox = SandboxDir::new();
    let a = sandbox.write_file("a.txt", "line1\nline2\nline3\n");
    let b = sandbox.write_file("b.txt", "line1\nchanged\nline3\n");
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("diff_files").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({
            "file_a": a.to_str().unwrap(),
            "file_b": b.to_str().unwrap()
        }))
        .await;
    assert!(result.success, "diff_files should succeed: {:?}", result.error);
    let diff_text = result.data["diff"].as_str().unwrap_or("")
        .to_owned() + result.data.to_string().as_str();
    assert!(
        diff_text.contains("changed") || diff_text.contains("line2"),
        "diff output should highlight changed line"
    );
}

#[tokio::test]
async fn functional_fs16_find_todos_kria() {
    // PROMPT-ID: FS-16
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("find_todos").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "directory": "/media/obaid/SSD/KRIA/crates" }))
        .await;
    assert!(
        result.success || result.error.is_some(),
        "find_todos must not panic"
    );
}

#[tokio::test]
async fn functional_fs15_count_lines_of_code() {
    // PROMPT-ID: FS-15
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("count_lines_of_code").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "path": "/media/obaid/SSD/KRIA/crates" }))
        .await;
    assert!(
        result.success || result.error.is_some(),
        "count_lines_of_code must not panic"
    );
}

#[tokio::test]
async fn functional_fs19_analyze_code() {
    // PROMPT-ID: FS-19
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("analyze_code").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({
            "file_path": "/media/obaid/SSD/KRIA/crates/kria-core/src/agent/router.rs",
            "language": "rust"
        }))
        .await;
    assert!(
        result.success || result.error.is_some(),
        "analyze_code must not panic"
    );
}
