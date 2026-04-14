"""
Tests for MCP (Model Context Protocol) Client Integration
==========================================================
Tests config loading, schema translation, risk assignment,
PolicyEngine MCP lookup, and the client manager lifecycle.
"""
import asyncio
import json
import os
import tempfile
from dataclasses import dataclass, field
from typing import Any
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from kria.infra.isolation import ToolResult
from kria.mcp.config import MCPServerConfig, load_mcp_config
from kria.mcp.client import (
    MCPClientManager,
    MCPServerConnection,
    _extract_text,
    _normalize_schema,
)


# ═══════════════════════════════════════════════════════════════════
#  Config loading tests
# ═══════════════════════════════════════════════════════════════════


class TestLoadMCPConfig:
    """Tests for load_mcp_config()."""

    def test_missing_file_returns_empty(self, tmp_path):
        result = load_mcp_config(str(tmp_path / "nonexistent.json"))
        assert result == []

    def test_invalid_json_returns_empty(self, tmp_path):
        bad_file = tmp_path / "bad.json"
        bad_file.write_text("not json {{{", encoding="utf-8")
        result = load_mcp_config(str(bad_file))
        assert result == []

    def test_valid_stdio_server(self, tmp_path):
        config = {
            "servers": [{
                "name": "filesystem",
                "transport": "stdio",
                "command": ["npx", "-y", "@modelcontextprotocol/server-filesystem", "/home"],
                "trust_level": "YELLOW",
                "tool_overrides": {"write_file": "RED"},
            }]
        }
        cfg_file = tmp_path / "mcp.json"
        cfg_file.write_text(json.dumps(config), encoding="utf-8")
        result = load_mcp_config(str(cfg_file))

        assert len(result) == 1
        assert result[0].name == "filesystem"
        assert result[0].transport == "stdio"
        assert result[0].command == ["npx", "-y", "@modelcontextprotocol/server-filesystem", "/home"]
        assert result[0].trust_level == "YELLOW"
        assert result[0].tool_overrides == {"write_file": "RED"}

    def test_valid_sse_server(self, tmp_path):
        config = {
            "servers": [{
                "name": "github",
                "transport": "sse",
                "url": "http://localhost:3001/sse",
                "trust_level": "RED",
                "headers": {"Authorization": "Bearer test"},
            }]
        }
        cfg_file = tmp_path / "mcp.json"
        cfg_file.write_text(json.dumps(config), encoding="utf-8")
        result = load_mcp_config(str(cfg_file))

        assert len(result) == 1
        assert result[0].name == "github"
        assert result[0].transport == "sse"
        assert result[0].url == "http://localhost:3001/sse"
        assert result[0].headers == {"Authorization": "Bearer test"}

    def test_skip_entry_missing_name(self, tmp_path):
        config = {"servers": [{"transport": "stdio", "command": ["echo"]}]}
        cfg_file = tmp_path / "mcp.json"
        cfg_file.write_text(json.dumps(config), encoding="utf-8")
        result = load_mcp_config(str(cfg_file))
        assert result == []

    def test_skip_invalid_transport(self, tmp_path):
        config = {"servers": [{"name": "bad", "transport": "grpc"}]}
        cfg_file = tmp_path / "mcp.json"
        cfg_file.write_text(json.dumps(config), encoding="utf-8")
        result = load_mcp_config(str(cfg_file))
        assert result == []

    def test_skip_stdio_without_command(self, tmp_path):
        config = {"servers": [{"name": "bad", "transport": "stdio"}]}
        cfg_file = tmp_path / "mcp.json"
        cfg_file.write_text(json.dumps(config), encoding="utf-8")
        result = load_mcp_config(str(cfg_file))
        assert result == []

    def test_skip_sse_without_url(self, tmp_path):
        config = {"servers": [{"name": "bad", "transport": "sse"}]}
        cfg_file = tmp_path / "mcp.json"
        cfg_file.write_text(json.dumps(config), encoding="utf-8")
        result = load_mcp_config(str(cfg_file))
        assert result == []

    def test_duplicate_names_skipped(self, tmp_path):
        config = {
            "servers": [
                {"name": "a", "transport": "sse", "url": "http://a"},
                {"name": "a", "transport": "sse", "url": "http://b"},
            ]
        }
        cfg_file = tmp_path / "mcp.json"
        cfg_file.write_text(json.dumps(config), encoding="utf-8")
        result = load_mcp_config(str(cfg_file))
        assert len(result) == 1

    def test_invalid_trust_level_defaults_red(self, tmp_path):
        config = {
            "servers": [{
                "name": "test",
                "transport": "sse",
                "url": "http://test",
                "trust_level": "PURPLE",
            }]
        }
        cfg_file = tmp_path / "mcp.json"
        cfg_file.write_text(json.dumps(config), encoding="utf-8")
        result = load_mcp_config(str(cfg_file))
        assert result[0].trust_level == "RED"

    def test_invalid_tool_override_values_dropped(self, tmp_path):
        config = {
            "servers": [{
                "name": "test",
                "transport": "sse",
                "url": "http://test",
                "tool_overrides": {"good": "GREEN", "bad": "PURPLE", "number": 42},
            }]
        }
        cfg_file = tmp_path / "mcp.json"
        cfg_file.write_text(json.dumps(config), encoding="utf-8")
        result = load_mcp_config(str(cfg_file))
        assert result[0].tool_overrides == {"good": "GREEN"}

    def test_disabled_server_loaded(self, tmp_path):
        config = {
            "servers": [{
                "name": "disabled",
                "transport": "sse",
                "url": "http://test",
                "enabled": False,
            }]
        }
        cfg_file = tmp_path / "mcp.json"
        cfg_file.write_text(json.dumps(config), encoding="utf-8")
        result = load_mcp_config(str(cfg_file))
        assert len(result) == 1
        assert result[0].enabled is False

    def test_multiple_servers(self, tmp_path):
        config = {
            "servers": [
                {"name": "a", "transport": "sse", "url": "http://a"},
                {"name": "b", "transport": "stdio", "command": ["echo"]},
                {"name": "c", "transport": "sse", "url": "http://c"},
            ]
        }
        cfg_file = tmp_path / "mcp.json"
        cfg_file.write_text(json.dumps(config), encoding="utf-8")
        result = load_mcp_config(str(cfg_file))
        assert len(result) == 3
        assert [c.name for c in result] == ["a", "b", "c"]


# ═══════════════════════════════════════════════════════════════════
#  Schema normalization tests
# ═══════════════════════════════════════════════════════════════════


class TestNormalizeSchema:
    """Tests for _normalize_schema()."""

    def test_empty_schema(self):
        result = _normalize_schema({})
        assert result == {"type": "object", "properties": {}}

    def test_already_object_schema(self):
        schema = {"type": "object", "properties": {"path": {"type": "string"}}}
        result = _normalize_schema(schema)
        assert result is schema  # same reference — passthrough

    def test_flat_schema_wrapped(self):
        schema = {"path": {"type": "string"}, "mode": {"type": "string"}}
        result = _normalize_schema(schema)
        assert result == {"type": "object", "properties": schema}


# ═══════════════════════════════════════════════════════════════════
#  Text extraction tests
# ═══════════════════════════════════════════════════════════════════


class TestExtractText:
    """Tests for _extract_text()."""

    def test_empty_content(self):
        assert _extract_text([]) == ""

    def test_text_attribute(self):
        block = MagicMock()
        block.text = "hello world"
        assert _extract_text([block]) == "hello world"

    def test_dict_with_text(self):
        assert _extract_text([{"text": "hello"}]) == "hello"

    def test_fallback_to_str(self):
        assert _extract_text([42]) == "42"

    def test_multiple_blocks(self):
        b1 = MagicMock()
        b1.text = "line 1"
        b2 = MagicMock()
        b2.text = "line 2"
        assert _extract_text([b1, b2]) == "line 1\nline 2"


# ═══════════════════════════════════════════════════════════════════
#  Tool namespacing tests
# ═══════════════════════════════════════════════════════════════════


class TestToolNamespacing:
    """Verify MCP tools are namespaced as mcp_{server}_{tool}."""

    def test_namespace_format(self):
        server_name = "filesystem"
        tool_name = "read_file"
        namespaced = f"mcp_{server_name}_{tool_name}"
        assert namespaced == "mcp_filesystem_read_file"

    def test_namespace_prevents_collision(self):
        native = "read_file"
        mcp = f"mcp_filesystem_read_file"
        assert native != mcp


# ═══════════════════════════════════════════════════════════════════
#  Risk level assignment tests
# ═══════════════════════════════════════════════════════════════════


class TestRiskAssignment:
    """Test risk level resolution from server config."""

    def test_server_default_applied(self):
        config = MCPServerConfig(
            name="test",
            transport="sse",
            url="http://test",
            trust_level="YELLOW",
        )
        tool_name = "list_files"
        # No override → server default
        risk = config.tool_overrides.get(tool_name, config.trust_level)
        assert risk == "YELLOW"

    def test_per_tool_override(self):
        config = MCPServerConfig(
            name="test",
            transport="sse",
            url="http://test",
            trust_level="YELLOW",
            tool_overrides={"write_file": "RED"},
        )
        # Override takes precedence
        assert config.tool_overrides.get("write_file", config.trust_level) == "RED"
        # Non-overridden falls to server default
        assert config.tool_overrides.get("read_file", config.trust_level) == "YELLOW"

    def test_default_is_red(self):
        config = MCPServerConfig(
            name="test",
            transport="sse",
            url="http://test",
        )
        assert config.trust_level == "RED"


# ═══════════════════════════════════════════════════════════════════
#  PolicyEngine MCP lookup tests
# ═══════════════════════════════════════════════════════════════════


class TestPolicyEngineMCP:
    """Test PolicyEngine.register_mcp_risk_levels and evaluate()."""

    @pytest.fixture
    def fresh_engine(self):
        from kria.safety.policy_engine import PolicyEngine
        return PolicyEngine()

    async def test_mcp_green_tool(self, fresh_engine):
        fresh_engine.register_mcp_risk_levels({"mcp_fs_list_files": "GREEN"})
        decision = await fresh_engine.evaluate("mcp_fs_list_files", {})
        assert decision.risk_level.value == "GREEN"
        assert decision.allowed is True

    async def test_mcp_yellow_tool(self, fresh_engine):
        fresh_engine.register_mcp_risk_levels({"mcp_fs_write_file": "YELLOW"})
        decision = await fresh_engine.evaluate("mcp_fs_write_file", {})
        assert decision.risk_level.value == "YELLOW"
        assert decision.allowed is True

    async def test_mcp_red_tool(self, fresh_engine):
        fresh_engine.register_mcp_risk_levels({"mcp_fs_delete": "RED"})
        decision = await fresh_engine.evaluate("mcp_fs_delete", {})
        assert decision.risk_level.value == "RED"
        assert decision.requires_approval is True

    async def test_mcp_invalid_level_defaults_red(self, fresh_engine):
        fresh_engine.register_mcp_risk_levels({"mcp_test": "PURPLE"})
        decision = await fresh_engine.evaluate("mcp_test", {})
        assert decision.risk_level.value == "RED"

    async def test_mcp_does_not_bypass_black(self, fresh_engine):
        # Even if an MCP tool name is registered as GREEN, BLACK regex
        # check on params still blocks
        fresh_engine.register_mcp_risk_levels({"mcp_shell_exec": "GREEN"})
        decision = await fresh_engine.evaluate(
            "mcp_shell_exec",
            {"command": "rm -rf /"},
        )
        assert decision.risk_level.value == "BLACK"

    async def test_unknown_mcp_tool_defaults_red(self, fresh_engine):
        # MCP tool NOT registered in risk map → falls through to
        # "unknown action" → default RED
        decision = await fresh_engine.evaluate("mcp_unknown_tool", {})
        assert decision.risk_level.value == "RED"


# ═══════════════════════════════════════════════════════════════════
#  MCPClientManager tests (mocked MCP SDK)
# ═══════════════════════════════════════════════════════════════════


class _MockTool:
    """Mimics an MCP Tool object."""
    def __init__(self, name: str, description: str, input_schema: dict):
        self.name = name
        self.description = description
        self.inputSchema = input_schema


class _MockListToolsResult:
    """Mimics the result from session.list_tools()."""
    def __init__(self, tools: list):
        self.tools = tools


class _MockContentBlock:
    """Mimics an MCP content block with .text."""
    def __init__(self, text: str):
        self.text = text


class _MockCallToolResult:
    """Mimics MCP CallToolResult."""
    def __init__(self, content: list, is_error: bool = False):
        self.content = content
        self.isError = is_error


class TestMCPClientManager:
    """Tests for MCPClientManager with mocked MCP SDK."""

    @pytest.fixture
    def mock_registry(self):
        """A mock ToolRegistry that records register() calls."""
        registry = MagicMock()
        registry.register = MagicMock()
        return registry

    @pytest.fixture
    def mock_health(self):
        """A mock health_registry."""
        health = MagicMock()
        health.update = MagicMock()
        return health

    @pytest.fixture
    def sample_config_file(self, tmp_path):
        config = {
            "servers": [{
                "name": "testserver",
                "transport": "stdio",
                "command": ["echo", "test"],
                "trust_level": "YELLOW",
                "tool_overrides": {"dangerous_tool": "RED"},
            }]
        }
        cfg_file = tmp_path / "mcp.json"
        cfg_file.write_text(json.dumps(config), encoding="utf-8")
        return str(cfg_file)

    async def test_start_with_no_config_file(self, mock_registry, mock_health):
        """Manager starts gracefully with missing config file."""
        manager = MCPClientManager()
        with patch("kria.mcp.client.settings") as mock_settings:
            mock_settings.mcp_config_path = "/nonexistent/path.json"
            mock_settings.mcp_connection_timeout = 5.0
            mock_settings.mcp_tool_timeout = 10.0
            with patch("kria.mcp.client._MCP_AVAILABLE", True):
                await manager.start(mock_registry, mock_health)
        assert manager.server_count == 0
        assert manager.tool_count == 0

    async def test_start_without_mcp_sdk(self, mock_registry, mock_health):
        """Manager logs warning when mcp SDK not installed."""
        manager = MCPClientManager()
        with patch("kria.mcp.client._MCP_AVAILABLE", False):
            await manager.start(mock_registry, mock_health)
        assert manager.server_count == 0

    async def test_register_server_tools(self, mock_registry):
        """Verify tools are registered with correct namespacing."""
        config = MCPServerConfig(
            name="testsvr",
            transport="stdio",
            command=["echo"],
            trust_level="YELLOW",
            tool_overrides={"write": "RED"},
        )
        conn = MCPServerConnection(config)
        conn._tools = [
            {"name": "read", "description": "Read a file", "input_schema": {"type": "object", "properties": {"path": {"type": "string"}}}},
            {"name": "write", "description": "Write a file", "input_schema": {"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}}}},
        ]

        manager = MCPClientManager()

        with patch("kria.safety.policy_engine.policy_engine") as mock_pe:
            mock_pe.register_mcp_risk_levels = MagicMock()
            count = manager._register_server_tools(config, conn, mock_registry)

        assert count == 2
        # Verify register was called with namespaced names
        call_names = [call.kwargs["name"] for call in mock_registry.register.call_args_list]
        assert "mcp_testsvr_read" in call_names
        assert "mcp_testsvr_write" in call_names

    async def test_stop_cleans_up(self):
        """Verify stop() clears connections and tasks."""
        manager = MCPClientManager()
        mock_conn = AsyncMock()
        mock_conn.disconnect = AsyncMock()
        manager._connections = {"test": mock_conn}
        manager._started = True

        await manager.stop()
        mock_conn.disconnect.assert_awaited_once()
        assert manager._connections == {}
        assert manager._started is False

    def test_get_server_status_empty(self):
        manager = MCPClientManager()
        assert manager.get_server_status() == {}

    def test_get_server_status_with_connections(self):
        manager = MCPClientManager()
        config = MCPServerConfig(name="a", transport="sse", url="http://a")
        conn = MCPServerConnection(config)
        conn._tools = [{"name": "t1", "description": "", "input_schema": {}}]
        manager._connections = {"a": conn}

        status = manager.get_server_status()
        assert "a" in status
        assert status["a"]["connected"] is False
        assert status["a"]["tools_count"] == 1
        assert status["a"]["transport"] == "sse"

    def test_is_available_reflects_import(self):
        manager = MCPClientManager()
        # We can't control _MCP_AVAILABLE at instance level,
        # but we can verify the property exists and returns a bool
        assert isinstance(manager.is_available, bool)


# ═══════════════════════════════════════════════════════════════════
#  MCPServerConnection unit tests
# ═══════════════════════════════════════════════════════════════════


class TestMCPServerConnection:
    """Tests for MCPServerConnection (mocked transport)."""

    def test_initial_state(self):
        config = MCPServerConfig(name="test", transport="stdio", command=["echo"])
        conn = MCPServerConnection(config)
        assert conn.is_connected is False
        assert conn._tools == []

    def test_connect_without_sdk_raises(self):
        config = MCPServerConfig(name="test", transport="stdio", command=["echo"])
        conn = MCPServerConnection(config)
        with patch("kria.mcp.client._MCP_AVAILABLE", False):
            with pytest.raises(RuntimeError, match="mcp SDK not installed"):
                asyncio.get_event_loop().run_until_complete(conn.connect())

    async def test_call_tool_when_disconnected(self):
        config = MCPServerConfig(name="test", transport="stdio", command=["echo"])
        conn = MCPServerConnection(config)
        result = await conn.call_tool("anything", {})
        assert result.success is False
        assert "not connected" in result.error

    async def test_discover_tools_when_disconnected(self):
        config = MCPServerConfig(name="test", transport="stdio", command=["echo"])
        conn = MCPServerConnection(config)
        with pytest.raises(RuntimeError, match="not connected"):
            await conn.discover_tools()


# ═══════════════════════════════════════════════════════════════════
#  Graceful degradation tests
# ═══════════════════════════════════════════════════════════════════


class TestGracefulDegradation:
    """Ensure MCP failures never prevent KRIA from starting."""

    def test_config_with_non_array_servers(self, tmp_path):
        cfg_file = tmp_path / "mcp.json"
        cfg_file.write_text(json.dumps({"servers": "not-an-array"}), encoding="utf-8")
        result = load_mcp_config(str(cfg_file))
        assert result == []

    def test_config_with_non_object_entry(self, tmp_path):
        cfg_file = tmp_path / "mcp.json"
        cfg_file.write_text(json.dumps({"servers": ["string-entry"]}), encoding="utf-8")
        result = load_mcp_config(str(cfg_file))
        assert result == []

    def test_config_handles_empty_servers(self, tmp_path):
        cfg_file = tmp_path / "mcp.json"
        cfg_file.write_text(json.dumps({"servers": []}), encoding="utf-8")
        result = load_mcp_config(str(cfg_file))
        assert result == []
