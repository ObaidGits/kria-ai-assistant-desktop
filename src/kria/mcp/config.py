"""
MCP Server Configuration Loader
================================
Reads ``~/.kria/mcp_servers.json`` (or KRIA_MCP_CONFIG_PATH) and
returns a list of validated MCPServerConfig objects.

Missing config file → empty list (graceful degradation).
"""
import json
import logging
from dataclasses import dataclass, field
from pathlib import Path
from typing import Literal

logger = logging.getLogger("kria.mcp.config")


@dataclass
class MCPServerConfig:
    """Definition of a single external MCP server."""

    name: str
    transport: Literal["stdio", "sse"]
    command: list[str] = field(default_factory=list)   # stdio transport
    url: str = ""                                       # sse transport
    env: dict[str, str] = field(default_factory=dict)  # env vars for stdio subprocess
    enabled: bool = True
    trust_level: str = "RED"                           # default risk tier for all tools
    tool_overrides: dict[str, str] = field(default_factory=dict)  # per-tool risk
    headers: dict[str, str] = field(default_factory=dict)         # custom headers (SSE)


def load_mcp_config(path: str) -> list[MCPServerConfig]:
    """
    Load and validate MCP server definitions from a JSON file.

    Returns an empty list if the file does not exist or is invalid,
    so KRIA never fails to start due to MCP misconfiguration.
    """
    config_path = Path(path).expanduser()

    if not config_path.is_file():
        logger.info("MCP config not found at %s — no MCP servers configured", config_path)
        return []

    try:
        raw = json.loads(config_path.read_text(encoding="utf-8"))
    except (json.JSONDecodeError, OSError) as exc:
        logger.error("Failed to read MCP config %s: %s", config_path, exc)
        return []

    servers_raw = raw.get("servers", [])
    if not isinstance(servers_raw, list):
        logger.error("MCP config 'servers' must be an array")
        return []

    configs: list[MCPServerConfig] = []
    seen_names: set[str] = set()

    for idx, entry in enumerate(servers_raw):
        if not isinstance(entry, dict):
            logger.warning("MCP config entry #%d is not an object — skipping", idx)
            continue

        name = entry.get("name", "").strip()
        if not name:
            logger.warning("MCP config entry #%d missing 'name' — skipping", idx)
            continue

        if name in seen_names:
            logger.warning("MCP config duplicate server name %r — skipping", name)
            continue

        transport = entry.get("transport", "").strip()
        if transport not in ("stdio", "sse"):
            logger.warning("MCP server %r: invalid transport %r — skipping", name, transport)
            continue

        if transport == "stdio" and not entry.get("command"):
            logger.warning("MCP server %r: stdio transport requires 'command' — skipping", name)
            continue

        if transport == "sse" and not entry.get("url"):
            logger.warning("MCP server %r: sse transport requires 'url' — skipping", name)
            continue

        trust_level = entry.get("trust_level", "RED").upper()
        if trust_level not in ("GREEN", "YELLOW", "RED"):
            logger.warning("MCP server %r: invalid trust_level %r — defaulting to RED", name, trust_level)
            trust_level = "RED"

        tool_overrides = entry.get("tool_overrides", {})
        if isinstance(tool_overrides, dict):
            # Validate override values
            tool_overrides = {
                k: v.upper()
                for k, v in tool_overrides.items()
                if isinstance(v, str) and v.upper() in ("GREEN", "YELLOW", "RED")
            }
        else:
            tool_overrides = {}

        cfg = MCPServerConfig(
            name=name,
            transport=transport,
            command=entry.get("command", []),
            url=entry.get("url", ""),
            env=entry.get("env", {}),
            enabled=entry.get("enabled", True),
            trust_level=trust_level,
            tool_overrides=tool_overrides,
            headers=entry.get("headers", {}),
        )
        configs.append(cfg)
        seen_names.add(name)

    logger.info("Loaded %d MCP server config(s) from %s", len(configs), config_path)
    return configs
