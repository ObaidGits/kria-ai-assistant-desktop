"""
MCP Client Manager
==================
Manages async connections to external MCP servers, discovers their
tools, and registers proxy functions into the KRIA ToolRegistry.

Each server connection is wrapped in a SupervisedTask for resilient
auto-reconnect with exponential backoff.  Tool execution is proxied
through async closures that translate MCP CallToolResult into KRIA
ToolResult, so the ReAct loop and safety pipeline see MCP tools as
ordinary registered functions.

Design constraints:
  - Zero VRAM impact (pure async I/O, no model loading)
  - Zero-trust default: all MCP tools registered as RED unless
    server config explicitly downgrades
  - Graceful degradation: MCP failures never block KRIA startup
"""
import asyncio
import json
import logging
from typing import Any, Optional

from kria.infra.config import settings
from kria.infra.isolation import ToolResult
from kria.mcp.config import MCPServerConfig, load_mcp_config

logger = logging.getLogger("kria.mcp.client")

# Conditional import — mcp SDK is an optional dependency
try:
    from mcp import ClientSession
    from mcp.client.stdio import StdioServerParameters, stdio_client
    from mcp.client.sse import sse_client

    _MCP_AVAILABLE = True
except ImportError:
    _MCP_AVAILABLE = False


class MCPServerConnection:
    """Manages a single MCP server connection lifecycle."""

    def __init__(self, config: MCPServerConfig) -> None:
        self.config = config
        self.name = config.name
        self._session: Optional[ClientSession] = None
        self._read_stream: Any = None
        self._write_stream: Any = None
        self._transport_ctx: Any = None
        self._session_ctx: Any = None
        self._connected = False
        self._tools: list[dict] = []

    @property
    def is_connected(self) -> bool:
        return self._connected

    async def connect(self) -> None:
        """Establish transport and initialize the MCP session."""
        if not _MCP_AVAILABLE:
            raise RuntimeError("mcp SDK not installed — run: pip install 'kria[mcp]'")

        if self.config.transport == "stdio":
            server_params = StdioServerParameters(
                command=self.config.command[0],
                args=self.config.command[1:] if len(self.config.command) > 1 else [],
                env={**self.config.env} if self.config.env else None,
            )
            self._transport_ctx = stdio_client(server_params)
        else:
            self._transport_ctx = sse_client(
                url=self.config.url,
                headers=self.config.headers if self.config.headers else None,
            )

        self._read_stream, self._write_stream = await self._transport_ctx.__aenter__()
        self._session_ctx = ClientSession(self._read_stream, self._write_stream)
        self._session = await self._session_ctx.__aenter__()
        await self._session.initialize()
        self._connected = True
        logger.info("[MCP] Connected to server %r (%s)", self.name, self.config.transport)

    async def discover_tools(self) -> list[dict]:
        """List tools from the MCP server, return normalized dicts."""
        if not self._session:
            raise RuntimeError(f"MCP server {self.name!r} not connected")

        result = await self._session.list_tools()
        self._tools = []
        for tool in result.tools:
            self._tools.append({
                "name": tool.name,
                "description": tool.description or "",
                "input_schema": tool.inputSchema if hasattr(tool, "inputSchema") else {},
            })
        logger.info("[MCP] Discovered %d tools from server %r", len(self._tools), self.name)
        return self._tools

    async def call_tool(self, tool_name: str, arguments: dict) -> ToolResult:
        """Execute a tool on the MCP server, return KRIA ToolResult."""
        if not self._session:
            return ToolResult(success=False, error=f"MCP server {self.name!r} not connected")

        try:
            result = await asyncio.wait_for(
                self._session.call_tool(tool_name, arguments),
                timeout=settings.mcp_tool_timeout,
            )
            # MCP CallToolResult has .content (list of content blocks) and .isError
            if result.isError:
                error_text = _extract_text(result.content)
                return ToolResult(success=False, error=error_text)

            data = _extract_text(result.content)
            return ToolResult(success=True, data=data)

        except asyncio.TimeoutError:
            return ToolResult(
                success=False,
                error=f"MCP tool {tool_name!r} timed out after {settings.mcp_tool_timeout}s",
            )
        except Exception as exc:
            logger.error("[MCP] Tool call %s.%s failed: %s", self.name, tool_name, exc)
            return ToolResult(success=False, error=str(exc))

    async def disconnect(self) -> None:
        """Cleanly shut down the session and transport."""
        self._connected = False
        if self._session_ctx:
            try:
                await self._session_ctx.__aexit__(None, None, None)
            except Exception:
                pass
            self._session_ctx = None
            self._session = None
        if self._transport_ctx:
            try:
                await self._transport_ctx.__aexit__(None, None, None)
            except Exception:
                pass
            self._transport_ctx = None
        self._read_stream = None
        self._write_stream = None
        logger.info("[MCP] Disconnected from server %r", self.name)


class MCPClientManager:
    """
    Singleton manager for all MCP server connections.

    Responsibilities:
      - Load config, connect to each enabled server
      - Discover tools and register proxy functions into ToolRegistry
      - Feed risk levels into PolicyEngine
      - Wrap each connection in a SupervisedTask for auto-reconnect
      - Provide health status for the /health endpoint
    """

    def __init__(self) -> None:
        self._connections: dict[str, MCPServerConnection] = {}
        self._supervised_tasks: dict[str, Any] = {}
        self._configs: list[MCPServerConfig] = []
        self._tool_registry: Any = None
        self._started = False

    async def start(self, tool_registry: Any, health_registry: Any) -> None:
        """
        Initialize all configured MCP servers.

        Called from main.py lifespan after tool_registry.load_all().
        """
        if not _MCP_AVAILABLE:
            logger.warning("[MCP] mcp SDK not installed — MCP integration disabled")
            return

        self._tool_registry = tool_registry
        self._configs = load_mcp_config(settings.mcp_config_path)

        if not self._configs:
            logger.info("[MCP] No MCP servers configured")
            return

        from kria.infra.health import ServiceStatus

        total_tools = 0
        for cfg in self._configs:
            if not cfg.enabled:
                logger.info("[MCP] Server %r disabled — skipping", cfg.name)
                continue

            conn = MCPServerConnection(cfg)
            self._connections[cfg.name] = conn

            try:
                await asyncio.wait_for(
                    conn.connect(),
                    timeout=settings.mcp_connection_timeout,
                )
                tools = await conn.discover_tools()
                count = self._register_server_tools(cfg, conn, tool_registry)
                total_tools += count
                health_registry.update(f"mcp_{cfg.name}", ServiceStatus.HEALTHY)
                logger.info("[MCP] Server %r: %d tools registered", cfg.name, count)

            except asyncio.TimeoutError:
                health_registry.update(
                    f"mcp_{cfg.name}", ServiceStatus.DOWN,
                    f"Connection timeout ({settings.mcp_connection_timeout}s)",
                )
                logger.error("[MCP] Server %r: connection timed out", cfg.name)

            except Exception as exc:
                health_registry.update(f"mcp_{cfg.name}", ServiceStatus.DOWN, str(exc))
                logger.error("[MCP] Server %r: failed to connect — %s", cfg.name, exc)

        # Start supervised reconnect tasks for down servers
        for name, conn in self._connections.items():
            if not conn.is_connected:
                self._start_supervised_reconnect(name, conn, tool_registry, health_registry)

        self._started = True
        logger.info("[MCP] Client manager started — %d server(s), %d total tools",
                     len(self._connections), total_tools)

    def _register_server_tools(
        self,
        config: MCPServerConfig,
        connection: MCPServerConnection,
        tool_registry: Any,
    ) -> int:
        """
        Register all discovered MCP tools into the ToolRegistry.

        Each tool becomes an async closure that proxies to
        ``connection.call_tool()``.  Risk levels are fed into
        the PolicyEngine.
        """
        from kria.safety.policy_engine import policy_engine

        risk_map: dict[str, str] = {}
        count = 0

        for tool_info in connection._tools:
            original_name = tool_info["name"]
            namespaced = f"mcp_{config.name}_{original_name}"

            # Determine risk level: per-tool override > server default
            risk = config.tool_overrides.get(original_name, config.trust_level)

            # Create an async proxy closure — captures tool_name and connection
            # by value via default args to avoid late-binding issues
            async def _proxy(
                _conn: MCPServerConnection = connection,
                _tool: str = original_name,
                **kwargs: Any,
            ) -> ToolResult:
                return await _conn.call_tool(_tool, kwargs)

            tool_registry.register(
                name=namespaced,
                func=_proxy,
                description=tool_info["description"],
                parameters_schema=_normalize_schema(tool_info.get("input_schema", {})),
            )

            risk_map[namespaced] = risk
            count += 1

        # Feed risk levels into PolicyEngine
        if risk_map:
            policy_engine.register_mcp_risk_levels(risk_map)

        return count

    def _start_supervised_reconnect(
        self,
        name: str,
        connection: MCPServerConnection,
        tool_registry: Any,
        health_registry: Any,
    ) -> None:
        """Wrap a failed connection in a SupervisedTask for auto-reconnect."""
        from kria.infra.supervisor import SupervisedTask
        from kria.infra.health import ServiceStatus

        async def _reconnect() -> None:
            """Attempt to connect, discover, and register tools."""
            await connection.connect()
            await connection.discover_tools()
            cfg = connection.config
            count = self._register_server_tools(cfg, connection, tool_registry)
            health_registry.update(f"mcp_{name}", ServiceStatus.HEALTHY)
            logger.info("[MCP] Reconnected to server %r — %d tools registered", name, count)
            # Stay alive — if this returns, SupervisedTask restarts it
            # Wait indefinitely (until cancelled or connection drops)
            while connection.is_connected:
                await asyncio.sleep(60)

        task = SupervisedTask(
            name=f"mcp_reconnect_{name}",
            coro_factory=_reconnect,
            max_retries=10,
            base_delay=2.0,
            max_delay=120.0,
        )
        self._supervised_tasks[name] = task
        task.start()

    async def stop(self) -> None:
        """Shut down all MCP connections and supervised tasks."""
        for name, task in self._supervised_tasks.items():
            await task.stop()
        self._supervised_tasks.clear()

        for name, conn in self._connections.items():
            await conn.disconnect()
        self._connections.clear()

        self._started = False
        logger.info("[MCP] Client manager stopped")

    def get_server_status(self) -> dict[str, dict]:
        """Return status summary for each MCP server."""
        status = {}
        for name, conn in self._connections.items():
            status[name] = {
                "connected": conn.is_connected,
                "transport": conn.config.transport,
                "tools_count": len(conn._tools),
                "trust_level": conn.config.trust_level,
            }
        return status

    @property
    def is_available(self) -> bool:
        return _MCP_AVAILABLE

    @property
    def server_count(self) -> int:
        return len(self._connections)

    @property
    def tool_count(self) -> int:
        return sum(len(c._tools) for c in self._connections.values())


# ── Helpers ───────────────────────────────────────────────────────

def _extract_text(content: list) -> str:
    """Extract text from MCP content blocks."""
    parts = []
    for block in content:
        if hasattr(block, "text"):
            parts.append(block.text)
        elif isinstance(block, dict) and "text" in block:
            parts.append(block["text"])
        else:
            parts.append(str(block))
    return "\n".join(parts) if parts else ""


def _normalize_schema(input_schema: dict) -> dict:
    """
    Normalize MCP inputSchema to KRIA ToolRegistry format.

    MCP tools declare inputSchema as a JSON Schema object (``type: object``
    with ``properties``).  KRIA's registry expects the same format, so
    this is mostly a pass-through with a safety wrapper.
    """
    if not input_schema:
        return {"type": "object", "properties": {}}

    if input_schema.get("type") == "object":
        return input_schema

    # Wrap flat schemas in an object envelope
    return {"type": "object", "properties": input_schema}
