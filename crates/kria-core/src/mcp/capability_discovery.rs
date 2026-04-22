use crate::tools::registry::ToolRegistry;

fn normalize_colab_operation_name(tool_name: &str, server_name: &str) -> String {
    let prefix = format!("mcp_{server_name}_");
    tool_name
        .strip_prefix(&prefix)
        .unwrap_or(tool_name)
        .to_string()
}

/// Build a Colab-focused capability summary from dynamically registered MCP tools.
///
/// This inspects the runtime tool registry category `mcp_<server_name>` and infers
/// feature coverage from discovered tool names/descriptions.
pub fn build_colab_capability_summary(
    tool_registry: &ToolRegistry,
    server_name: &str,
) -> serde_json::Value {
    let category = format!("mcp_{server_name}");
    let tool_defs = tool_registry.list_by_category(&category);

    let indexed: Vec<(String, String)> = tool_defs
        .iter()
        .map(|tool| {
            let operation = normalize_colab_operation_name(&tool.name, server_name);
            let parameter_hints = tool
                .parameters
                .iter()
                .map(|param| param.name.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let haystack = format!("{} {} {}", operation, tool.description, parameter_hints)
                .to_ascii_lowercase();
            (operation, haystack)
        })
        .collect();

    let has_keywords = |keywords: &[&str]| {
        indexed
            .iter()
            .any(|(_, haystack)| keywords.iter().any(|kw| haystack.contains(kw)))
    };

    let notebook_discovery = has_keywords(&[
        "list_notebook",
        "list notebooks",
        "notebook_list",
        "notebooks.list",
        "get_notebook",
        "search_notebook",
        "available notebooks",
        "list_open_notebooks",
    ]);
    let notebook_selection = has_keywords(&[
        "select_notebook",
        "set_notebook",
        "open_notebook",
        "attach_notebook",
        "switch_notebook",
        "active_notebook",
        "notebook_id",
    ]);
    let cell_execution = has_keywords(&[
        "execute",
        "execute_cell",
        "run_cell",
        "run code",
        "run_notebook",
        "exec",
        "execute_code",
        "run_code",
        "run_python",
        "python",
        "code_cell",
        "code",
    ]);
    let artifact_io = has_keywords(&[
        "upload",
        "download",
        "artifact",
        "file",
        "read_file",
        "write_file",
        "save_file",
        "export",
    ]);
    let runtime_lifecycle = has_keywords(&[
        "connect",
        "disconnect",
        "keepalive",
        "heartbeat",
        "runtime",
        "kernel",
        "session",
        "restart",
        "interrupt",
    ]);
    let package_management = has_keywords(&[
        "pip",
        "conda",
        "apt",
        "install",
        "requirements",
        "package",
        "dependency",
    ]);
    let checkpointing = has_keywords(&[
        "checkpoint",
        "save_checkpoint",
        "snapshot",
        "resume_checkpoint",
    ]);

    let ready_satisfied = cell_execution && (notebook_selection || notebook_discovery);
    let mut missing: Vec<String> = Vec::new();
    if !cell_execution {
        missing.push("cell_execution".into());
    }
    if !(notebook_selection || notebook_discovery) {
        missing.push("notebook_selection_or_discovery".into());
    }

    let discovered_tools: Vec<serde_json::Value> = tool_defs
        .iter()
        .zip(indexed.iter())
        .map(|(tool, (operation, _))| {
            serde_json::json!({
                "name": tool.name,
                "operation": operation,
                "description": tool.description,
                "parameter_count": tool.parameters.len(),
            })
        })
        .collect();

    serde_json::json!({
        "category": category,
        "tool_count": discovered_tools.len(),
        "discovered_tools": discovered_tools,
        "features": {
            "notebook_discovery": notebook_discovery,
            "notebook_selection": notebook_selection,
            "cell_execution": cell_execution,
            "artifact_io": artifact_io,
            "runtime_lifecycle": runtime_lifecycle,
            "package_management": package_management,
            "checkpointing": checkpointing,
        },
        "ready_requirements": {
            "requires": ["cell_execution", "notebook_selection_or_discovery"],
            "satisfied": ready_satisfied,
            "missing": missing,
        }
    })
}
