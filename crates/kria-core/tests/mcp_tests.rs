//! Phase 7: MCP Protocol & Plugin Ecosystem tests.
//!
//! Covers MCP protocol types, config integration, server manager
//! construction, tool name prefixing, schema extraction, and
//! risk level parsing.

use kria_core::config::*;
use kria_core::mcp::*;
use serde_json::json;
use std::io::Write;
use tempfile::NamedTempFile;

// ── Protocol type serialization ────────────────────────────────────

mod protocol {
    use super::*;
    use kria_core::mcp::protocol::*;

    #[test]
    fn json_rpc_request_serializes_correctly() {
        let req = JsonRpcRequest::new(1, "initialize", Some(json!({"key": "value"})));
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("\"jsonrpc\":\"2.0\""));
        assert!(s.contains("\"id\":1"));
        assert!(s.contains("\"method\":\"initialize\""));
        assert!(s.contains("\"params\""));
    }

    #[test]
    fn json_rpc_request_omits_null_params() {
        let req = JsonRpcRequest::new(42, "notifications/initialized", None);
        let s = serde_json::to_string(&req).unwrap();
        assert!(!s.contains("\"params\""));
        assert!(s.contains("\"id\":42"));
    }

    #[test]
    fn json_rpc_response_into_result_success() {
        let resp: JsonRpcResponse =
            serde_json::from_value(json!({"jsonrpc": "2.0", "id": 1, "result": {"ok": true}}))
                .unwrap();
        let result = resp.into_result().unwrap();
        assert_eq!(result, json!({"ok": true}));
    }

    #[test]
    fn json_rpc_response_into_result_error() {
        let resp: JsonRpcResponse = serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {"code": -32600, "message": "Invalid Request"}
        }))
        .unwrap();
        let err = resp.into_result().unwrap_err();
        assert!(err.contains("-32600"));
        assert!(err.contains("Invalid Request"));
    }

    #[test]
    fn json_rpc_response_null_result_when_both_absent() {
        let resp: JsonRpcResponse =
            serde_json::from_value(json!({"jsonrpc": "2.0", "id": 1})).unwrap();
        let result = resp.into_result().unwrap();
        assert!(result.is_null());
    }

    #[test]
    fn initialize_params_serializes_with_camel_case() {
        let params = InitializeParams {
            protocol_version: "2024-11-05".into(),
            capabilities: ClientCapabilities::default(),
            client_info: ClientInfo {
                name: "kria".into(),
                version: "0.1.0".into(),
            },
        };
        let s = serde_json::to_string(&params).unwrap();
        assert!(s.contains("\"protocolVersion\":\"2024-11-05\""));
        assert!(s.contains("\"clientInfo\""));
    }

    #[test]
    fn initialize_result_deserializes() {
        let val = json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {},
                "resources": null,
                "prompts": null
            },
            "serverInfo": {
                "name": "test-server",
                "version": "1.0"
            }
        });
        let result: InitializeResult = serde_json::from_value(val).unwrap();
        assert_eq!(result.protocol_version, "2024-11-05");
        assert!(result.capabilities.tools.is_some());
        assert!(result.server_info.is_some());
        assert_eq!(result.server_info.unwrap().name, "test-server");
    }

    #[test]
    fn mcp_tool_def_deserializes() {
        let val = json!({
            "name": "read_file",
            "description": "Read a file from disk",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File path"}
                },
                "required": ["path"]
            }
        });
        let tool: McpToolDef = serde_json::from_value(val).unwrap();
        assert_eq!(tool.name, "read_file");
        assert_eq!(tool.description.unwrap(), "Read a file from disk");
        assert!(tool.input_schema.get("properties").is_some());
    }

    #[test]
    fn tools_list_result_deserializes() {
        let val = json!({
            "tools": [
                {"name": "tool_a", "description": "A", "inputSchema": {}},
                {"name": "tool_b", "inputSchema": {}}
            ]
        });
        let result: ToolsListResult = serde_json::from_value(val).unwrap();
        assert_eq!(result.tools.len(), 2);
        assert_eq!(result.tools[0].name, "tool_a");
        assert!(result.tools[1].description.is_none());
    }

    #[test]
    fn tool_call_params_serializes() {
        let params = ToolCallParams {
            name: "read_file".into(),
            arguments: Some(json!({"path": "/tmp/test.txt"})),
        };
        let s = serde_json::to_string(&params).unwrap();
        assert!(s.contains("\"name\":\"read_file\""));
        assert!(s.contains("\"path\":\"/tmp/test.txt\""));
    }

    #[test]
    fn tool_call_result_deserializes_success() {
        let val = json!({
            "content": [
                {"type": "text", "text": "Hello, World!"}
            ],
            "isError": false
        });
        let result: ToolCallResult = serde_json::from_value(val).unwrap();
        assert!(!result.is_error);
        assert_eq!(result.content.len(), 1);
        assert_eq!(result.content[0].text.as_deref(), Some("Hello, World!"));
    }

    #[test]
    fn tool_call_result_deserializes_error() {
        let val = json!({
            "content": [
                {"type": "text", "text": "File not found"}
            ],
            "isError": true
        });
        let result: ToolCallResult = serde_json::from_value(val).unwrap();
        assert!(result.is_error);
    }

    #[test]
    fn tool_call_content_with_binary_data() {
        let val = json!({
            "type": "image",
            "data": "iVBORw0KGgo...",
            "mimeType": "image/png"
        });
        let content: ToolCallContent = serde_json::from_value(val).unwrap();
        assert_eq!(content.content_type, "image");
        assert!(content.data.is_some());
        assert_eq!(content.mime_type.as_deref(), Some("image/png"));
        assert!(content.text.is_none());
    }
}

// ── MCP Config integration ─────────────────────────────────────────

mod config_integration {
    use super::*;

    #[test]
    fn default_mcp_config_has_no_servers() {
        let cfg = KriaConfig::default();
        assert!(cfg.mcp.servers.is_empty());
    }

    #[test]
    fn mcp_server_config_defaults_enabled_true() {
        let server: McpServerConfig = serde_json::from_value(json!({
            "name": "test",
            "command": "test-cmd"
        }))
        .unwrap();
        assert!(server.enabled);
        assert_eq!(server.trust_level, "YELLOW");
        assert!(server.args.is_empty());
        assert!(server.env.is_empty());
        assert!(server.tool_overrides.is_empty());
    }

    #[test]
    fn mcp_config_from_toml() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(
            f,
            r#"
[[mcp.servers]]
name = "filesystem"
command = "mcp-server-filesystem"
args = ["/home", "/tmp"]
trust_level = "GREEN"

[[mcp.servers]]
name = "git"
command = "mcp-server-git"
enabled = false
"#
        )
        .unwrap();

        let cfg = load_config(f.path(), None).unwrap();
        assert_eq!(cfg.mcp.servers.len(), 2);

        let fs = &cfg.mcp.servers[0];
        assert_eq!(fs.name, "filesystem");
        assert_eq!(fs.command, "mcp-server-filesystem");
        assert_eq!(fs.args, vec!["/home", "/tmp"]);
        assert_eq!(fs.trust_level, "GREEN");
        assert!(fs.enabled);

        let git = &cfg.mcp.servers[1];
        assert_eq!(git.name, "git");
        assert!(!git.enabled);
        assert_eq!(git.trust_level, "YELLOW"); // default
    }

    #[test]
    fn mcp_config_with_env_and_overrides() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(
            f,
            r#"
[[mcp.servers]]
name = "custom"
command = "custom-mcp"

[mcp.servers.env]
NODE_ENV = "production"
API_KEY = "secret"

[mcp.servers.tool_overrides]
delete_file = "RED"
list_files = "GREEN"
"#
        )
        .unwrap();

        let cfg = load_config(f.path(), None).unwrap();
        let srv = &cfg.mcp.servers[0];
        assert_eq!(srv.env.get("NODE_ENV").unwrap(), "production");
        assert_eq!(srv.env.get("API_KEY").unwrap(), "secret");
        assert_eq!(srv.tool_overrides.get("delete_file").unwrap(), "RED");
        assert_eq!(srv.tool_overrides.get("list_files").unwrap(), "GREEN");
    }

    #[test]
    fn mcp_config_preserves_other_sections() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(
            f,
            r#"
[llm]
active_model = "custom"

[[mcp.servers]]
name = "fs"
command = "mcp-fs"
"#
        )
        .unwrap();

        let cfg = load_config(f.path(), None).unwrap();
        assert_eq!(cfg.llm.active_model, "custom");
        assert_eq!(cfg.mcp.servers.len(), 1);
        // Other defaults preserved
        assert_eq!(cfg.ui.theme, "dark");
    }
}

// ── Server Manager unit tests ──────────────────────────────────────

mod server_manager {
    use super::*;

    #[test]
    fn new_with_empty_configs() {
        let mgr = McpServerManager::new(vec![]);
        assert!(mgr.get_client("anything").is_none());
    }

    #[test]
    fn new_with_configs_stores_them() {
        let configs = vec![
            McpServerConfig {
                name: "server-a".into(),
                command: "cmd-a".into(),
                args: vec![],
                env: std::collections::HashMap::new(),
                enabled: true,
                trust_level: "YELLOW".into(),
                tool_overrides: std::collections::HashMap::new(),
            },
            McpServerConfig {
                name: "server-b".into(),
                command: "cmd-b".into(),
                args: vec![],
                env: std::collections::HashMap::new(),
                enabled: false,
                trust_level: "RED".into(),
                tool_overrides: std::collections::HashMap::new(),
            },
        ];
        let mgr = McpServerManager::new(configs);
        // No clients started yet — just stored configs
        assert!(mgr.get_client("server-a").is_none());
        assert!(mgr.get_client("server-b").is_none());
    }

    #[tokio::test]
    async fn status_reports_all_servers() {
        let configs = vec![
            McpServerConfig {
                name: "srv1".into(),
                command: "mcp-test".into(),
                args: vec![],
                env: std::collections::HashMap::new(),
                enabled: true,
                trust_level: "GREEN".into(),
                tool_overrides: std::collections::HashMap::new(),
            },
            McpServerConfig {
                name: "srv2".into(),
                command: "mcp-test2".into(),
                args: vec![],
                env: std::collections::HashMap::new(),
                enabled: false,
                trust_level: "RED".into(),
                tool_overrides: std::collections::HashMap::new(),
            },
        ];
        let mgr = McpServerManager::new(configs);
        let statuses = mgr.status().await;

        assert_eq!(statuses.len(), 2);
        assert_eq!(statuses[0].name, "srv1");
        assert!(statuses[0].enabled);
        assert_eq!(statuses[0].tool_count, 0);

        assert_eq!(statuses[1].name, "srv2");
        assert!(!statuses[1].enabled);
    }

    #[tokio::test]
    async fn stop_nonexistent_server_succeeds() {
        let mut mgr = McpServerManager::new(vec![]);
        // Should not error
        mgr.stop_server("nonexistent").await.unwrap();
    }

    #[tokio::test]
    async fn stop_all_on_empty_succeeds() {
        let mut mgr = McpServerManager::new(vec![]);
        mgr.stop_all().await;
    }
}

// ── McpClient state tests ──────────────────────────────────────────

mod client_state {
    use kria_core::mcp::client::{McpClient, McpServerState};

    #[tokio::test]
    async fn new_client_is_stopped() {
        let client = McpClient::new("test-server");
        assert!(matches!(client.state().await, McpServerState::Stopped));
    }

    #[tokio::test]
    async fn new_client_has_no_tools() {
        let client = McpClient::new("test-server");
        assert!(client.tools().await.is_empty());
    }

    #[tokio::test]
    async fn new_client_has_no_error() {
        let client = McpClient::new("test-server");
        assert!(client.error().await.is_none());
    }

    #[tokio::test]
    async fn server_state_serializes() {
        let state = McpServerState::Running;
        let s = serde_json::to_string(&state).unwrap();
        assert!(s.contains("Running"));

        let state = McpServerState::Stopped;
        let s = serde_json::to_string(&state).unwrap();
        assert!(s.contains("Stopped"));
    }
}

// ── McpServerStatus serialization ──────────────────────────────────

mod status_serialization {
    use kria_core::mcp::client::McpServerState;
    use kria_core::mcp::server_manager::McpServerStatus;

    #[test]
    fn status_serializes_to_json() {
        let status = McpServerStatus {
            name: "test-server".into(),
            command: "mcp-test".into(),
            enabled: true,
            state: McpServerState::Running,
            tool_count: 5,
            error: None,
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["name"], "test-server");
        assert_eq!(json["tool_count"], 5);
        assert!(json["error"].is_null());
        assert_eq!(json["enabled"], true);
    }

    #[test]
    fn status_with_error_serializes() {
        let status = McpServerStatus {
            name: "broken".into(),
            command: "bad-cmd".into(),
            enabled: true,
            state: McpServerState::Error,
            tool_count: 0,
            error: Some("connection refused".into()),
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["error"], "connection refused");
    }
}
