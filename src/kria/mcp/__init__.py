"""
MCP (Model Context Protocol) Client
====================================
Async client manager that connects to external MCP servers,
discovers their tools, and registers them into the KRIA ToolRegistry.

MCP tools flow through the same PolicyEngine → HITL → Rollback
safety pipeline as native tools.  Zero VRAM impact.
"""
from kria.mcp.client import MCPClientManager

mcp_manager = MCPClientManager()

__all__ = ["mcp_manager"]
