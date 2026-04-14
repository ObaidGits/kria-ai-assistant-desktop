"""
Plugin Manager
===============
Tool-level interface for managing plugins (list, enable, disable, load, unload).
"""
import logging

from kria.infra.isolation import ToolResult, isolated
from kria.plugins.loader import plugin_loader
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.plugins.manager")


@isolated
async def list_plugins() -> dict:
    """List all available and loaded plugins."""
    available = plugin_loader.discover()
    loaded = plugin_loader.list_loaded()
    return {
        "available": available,
        "loaded": loaded,
        "available_count": len(available),
        "loaded_count": len(loaded),
    }


@isolated
async def load_plugin(name: str) -> dict:
    """Load a plugin by name."""
    success = plugin_loader.load(name)
    return {"plugin": name, "loaded": success}


@isolated
async def unload_plugin(name: str) -> dict:
    """Unload a plugin by name."""
    success = plugin_loader.unload(name)
    return {"plugin": name, "unloaded": success}


# ── Register ──────────────────────────────────────────────────────

tool_registry.register("list_plugins", list_plugins,
    description="List all available and loaded plugins.")

tool_registry.register("load_plugin", load_plugin,
    description="Load a plugin by name.",
    parameters_schema={
        "name": {"type": "string", "description": "Plugin name"},
    })

tool_registry.register("unload_plugin", unload_plugin,
    description="Unload a plugin by name.",
    parameters_schema={
        "name": {"type": "string", "description": "Plugin name"},
    })
