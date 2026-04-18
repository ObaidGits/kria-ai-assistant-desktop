/// Phase 11 — Developer Power Tools Tests
/// Tests developer tool registration, git tools, project analysis, diff, and database tools.

// ── Registration tests ──

#[test]
fn developer_tools_registered() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    let dev_tools = reg.list_by_category("developer");
    assert!(
        dev_tools.len() >= 11,
        "expected at least 11 developer tools, got {}",
        dev_tools.len()
    );
}

#[test]
fn git_status_registered() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    assert!(reg.get_def("git_status").is_some());
    assert!(reg.get_handler("git_status").is_some());
}

#[test]
fn git_log_registered() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    assert!(reg.get_def("git_log").is_some());
}

#[test]
fn git_diff_registered() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    assert!(reg.get_def("git_diff").is_some());
}

#[test]
fn git_commit_is_red_tier() {
    use kria_core::safety::RiskLevel;
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    let def = reg.get_def("git_commit").unwrap();
    assert_eq!(def.default_tier, RiskLevel::Red);
}

#[test]
fn git_commit_requires_message() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    let def = reg.get_def("git_commit").unwrap();
    let msg_param = def.parameters.iter().find(|p| p.name == "message").unwrap();
    assert!(msg_param.required);
}

#[test]
fn git_branch_list_registered() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    assert!(reg.get_def("git_branch_list").is_some());
}

#[test]
fn git_checkout_is_yellow_tier() {
    use kria_core::safety::RiskLevel;
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    let def = reg.get_def("git_checkout").unwrap();
    assert_eq!(def.default_tier, RiskLevel::Yellow);
}

#[test]
fn git_stash_registered() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    assert!(reg.get_def("git_stash").is_some());
    let def = reg.get_def("git_stash").unwrap();
    let pop_param = def.parameters.iter().find(|p| p.name == "pop");
    assert!(pop_param.is_some());
}

#[test]
fn analyze_project_registered() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    let def = reg.get_def("analyze_project").unwrap();
    assert_eq!(def.category, "developer");
}

#[test]
fn diff_files_unified_registered() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    let def = reg.get_def("diff_files_unified").unwrap();
    assert_eq!(def.parameters.len(), 2);
    assert!(def.parameters.iter().all(|p| p.required));
}

#[test]
fn query_sqlite_registered() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    let def = reg.get_def("query_sqlite").unwrap();
    assert_eq!(def.parameters.len(), 2);
}

#[test]
fn describe_database_registered() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    assert!(reg.get_def("describe_database").is_some());
}

// ── Functional tests ──

#[tokio::test]
async fn query_sqlite_rejects_write() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    let handler = reg.get_handler("query_sqlite").unwrap();
    let result = handler
        .execute(serde_json::json!({
            "db_path": "/tmp/test.db",
            "sql": "DROP TABLE users"
        }))
        .await;
    assert!(!result.success);
    assert!(result.error.unwrap().contains("read-only"));
}

#[tokio::test]
async fn query_sqlite_rejects_insert() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    let handler = reg.get_handler("query_sqlite").unwrap();
    let result = handler
        .execute(serde_json::json!({
            "db_path": "/tmp/test.db",
            "sql": "INSERT INTO users VALUES (1, 'test')"
        }))
        .await;
    assert!(!result.success);
}

#[tokio::test]
async fn query_sqlite_rejects_update() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    let handler = reg.get_handler("query_sqlite").unwrap();
    let result = handler
        .execute(serde_json::json!({
            "db_path": "/tmp/test.db",
            "sql": "UPDATE users SET name='test'"
        }))
        .await;
    assert!(!result.success);
}

#[tokio::test]
async fn diff_files_unified_requires_both_params() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    let handler = reg.get_handler("diff_files_unified").unwrap();
    let result = handler
        .execute(serde_json::json!({ "file_a": "/tmp/a.txt" }))
        .await;
    assert!(!result.success);
    assert!(result.error.unwrap().contains("required"));
}

#[tokio::test]
async fn git_status_on_current_repo() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    let handler = reg.get_handler("git_status").unwrap();
    let result = handler.execute(serde_json::json!({ "path": "." })).await;
    // This may pass or fail depending on CI, but should not panic
    if result.success {
        assert!(result.data["branch"].is_string());
    }
}

#[tokio::test]
async fn analyze_project_current_dir() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    let handler = reg.get_handler("analyze_project").unwrap();
    let result = handler.execute(serde_json::json!({ "path": "." })).await;
    assert!(result.success);
    assert!(result.data["total_files"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn analyze_project_invalid_path() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    let handler = reg.get_handler("analyze_project").unwrap();
    let result = handler
        .execute(serde_json::json!({ "path": "/nonexistent/path/xyz" }))
        .await;
    assert!(!result.success);
}

#[test]
fn developer_tools_standard_tier() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    // Developer tools should be filtered away for lite tier
    let lite_tools = reg.list_for_tier("lite");
    let has_dev = lite_tools.iter().any(|t| t.name == "git_status");
    assert!(!has_dev, "git_status should not appear on lite tier");
}

#[test]
fn developer_tools_available_standard() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    let std_tools = reg.list_for_tier("standard");
    let has_dev = std_tools.iter().any(|t| t.name == "git_status");
    assert!(has_dev, "git_status should appear on standard tier");
}

#[test]
fn total_tool_count_after_phase11() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    // Phase 10 had at least 56 tools total, now +11 developer = 67+
    assert!(
        reg.len() >= 67,
        "expected at least 67 tools, got {}",
        reg.len()
    );
}
