use std::sync::Arc;

use kria_core::infra::ToolResult;
use kria_core::mcp::build_colab_capability_summary;
use kria_core::safety::RiskLevel;
use kria_core::tools::registry::{ParamDef, ToolDef, ToolHandler, ToolRegistry};

struct NoopTool;

#[async_trait::async_trait]
impl ToolHandler for NoopTool {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        ToolResult::ok(serde_json::json!({ "ok": true }))
    }
}

fn register_mock_mcp_tool(
    registry: &ToolRegistry,
    server_name: &str,
    operation: &str,
    description: &str,
) {
    register_mock_mcp_tool_with_params(registry, server_name, operation, description, &[]);
}

fn register_mock_mcp_tool_with_params(
    registry: &ToolRegistry,
    server_name: &str,
    operation: &str,
    description: &str,
    params: &[&str],
) {
    let prefixed_name = format!("mcp_{server_name}_{operation}");
    registry.register(
        ToolDef {
            name: prefixed_name,
            description: description.to_string(),
            category: format!("mcp_{server_name}"),
            parameters: params
                .iter()
                .map(|name| ParamDef {
                    name: (*name).to_string(),
                    param_type: "string".to_string(),
                    description: String::new(),
                    required: false,
                    default: None,
                })
                .collect(),
            default_tier: RiskLevel::Yellow,
            min_tier: "standard",
        },
        Arc::new(NoopTool),
    );
}

#[test]
fn colab_capability_summary_detects_ready_requirements() {
    let registry = ToolRegistry::new();
    let server_name = "colab-mcp";

    register_mock_mcp_tool(
        &registry,
        server_name,
        "list_notebooks",
        "List available notebooks in the current Colab workspace",
    );
    register_mock_mcp_tool(
        &registry,
        server_name,
        "execute_cell",
        "Execute one notebook cell and return output",
    );
    register_mock_mcp_tool(
        &registry,
        server_name,
        "download_artifact",
        "Download generated artifacts from Colab runtime",
    );

    let summary = build_colab_capability_summary(&registry, server_name);

    assert_eq!(summary["tool_count"].as_u64(), Some(3));
    assert_eq!(
        summary["features"]["notebook_discovery"].as_bool(),
        Some(true)
    );
    assert_eq!(summary["features"]["cell_execution"].as_bool(), Some(true));
    assert_eq!(summary["features"]["artifact_io"].as_bool(), Some(true));
    assert_eq!(
        summary["ready_requirements"]["satisfied"].as_bool(),
        Some(true)
    );

    let missing = summary["ready_requirements"]["missing"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(missing.is_empty());
}

#[test]
fn colab_capability_summary_reports_missing_requirements() {
    let registry = ToolRegistry::new();
    let server_name = "colab-mcp";

    register_mock_mcp_tool(
        &registry,
        server_name,
        "upload_artifact",
        "Upload a local artifact into Colab runtime",
    );

    let summary = build_colab_capability_summary(&registry, server_name);

    assert_eq!(summary["tool_count"].as_u64(), Some(1));
    assert_eq!(
        summary["ready_requirements"]["satisfied"].as_bool(),
        Some(false)
    );

    let missing: Vec<String> = summary["ready_requirements"]["missing"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| value.as_str().map(|s| s.to_string()))
        .collect();

    assert!(missing.contains(&"cell_execution".to_string()));
    assert!(missing.contains(&"notebook_selection_or_discovery".to_string()));
}

#[test]
fn colab_capability_summary_detects_execution_via_modern_keyword_variants() {
    let registry = ToolRegistry::new();
    let server_name = "colab-mcp";

    register_mock_mcp_tool(
        &registry,
        server_name,
        "run_python",
        "Run Python in the active notebook and return output",
    );
    register_mock_mcp_tool(
        &registry,
        server_name,
        "set_active_notebook",
        "Set the active notebook id for subsequent calls",
    );

    let summary = build_colab_capability_summary(&registry, server_name);
    assert_eq!(summary["features"]["cell_execution"].as_bool(), Some(true));
    assert_eq!(summary["features"]["notebook_selection"].as_bool(), Some(true));
    assert_eq!(
        summary["ready_requirements"]["satisfied"].as_bool(),
        Some(true)
    );
}

#[test]
fn colab_capability_summary_uses_parameter_hints_for_execution_signal() {
    let registry = ToolRegistry::new();
    let server_name = "colab-mcp";

    register_mock_mcp_tool_with_params(
        &registry,
        server_name,
        "proxy_action",
        "Proxy action against connected notebook runtime",
        &["code", "language"],
    );
    register_mock_mcp_tool(
        &registry,
        server_name,
        "list_open_notebooks",
        "List notebooks currently open in Colab",
    );

    let summary = build_colab_capability_summary(&registry, server_name);
    assert_eq!(summary["features"]["cell_execution"].as_bool(), Some(true));
    assert_eq!(summary["features"]["notebook_discovery"].as_bool(), Some(true));
    assert_eq!(
        summary["ready_requirements"]["satisfied"].as_bool(),
        Some(true)
    );
}
