"""
Plugin API
==========
Defines the interface that plugins can use to interact with K.R.I.A.
Plugins import from this module to register tools, subscribe to events, etc.
"""
from kria.automation.event_bus import event_bus
from kria.infra.isolation import ToolResult, isolated
from kria.tools.registry import tool_registry

__all__ = [
    "tool_registry",
    "event_bus",
    "isolated",
    "ToolResult",
    "register_tool",
]


def register_tool(
    name: str,
    func,
    description: str = "",
    parameters_schema: dict | None = None,
):
    """Convenience wrapper for plugins to register tools."""
    tool_registry.register(
        name=name,
        func=func,
        description=description,
        parameters_schema=parameters_schema,
    )
