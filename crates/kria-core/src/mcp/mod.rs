/// MCP (Model Context Protocol) client implementation.
///
/// Connects to MCP servers via stdio transport, discovers their tools,
/// and registers them in the KRIA tool registry.
pub mod protocol;
pub mod client;
pub mod server_manager;
pub mod tool_bridge;

pub use client::McpClient;
pub use server_manager::McpServerManager;
pub use tool_bridge::McpToolHandler;
