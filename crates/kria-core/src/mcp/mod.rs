/// MCP (Model Context Protocol) client implementation.
///
/// Connects to MCP servers via stdio transport, discovers their tools,
/// and registers them in the KRIA tool registry.
pub mod capability_discovery;
pub mod client;
pub mod payload_shaper;
pub mod protocol;
pub mod server_manager;
pub mod tool_bridge;

pub use capability_discovery::build_colab_capability_summary;
pub use client::McpClient;
pub use payload_shaper::{shape_for_llm, ShapedPayload};
pub use server_manager::McpServerManager;
pub use tool_bridge::McpToolHandler;
