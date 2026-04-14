"""
User Preferences Module
========================
Track and learn from user preferences over time.
"""
import logging

from kria.infra.isolation import ToolResult, isolated
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.memory.user_prefs")


@isolated
async def set_preference(key: str, value: str) -> str:
    """Set a user preference."""
    from kria.memory.persistent import sqlite_manager
    await sqlite_manager.execute(
        "INSERT OR REPLACE INTO user_preferences (key, value) VALUES (?, ?)",
        (f"pref:{key}", value),
    )
    return f"Preference set: {key} = {value}"


@isolated
async def get_preference(key: str, default: str = "") -> dict:
    """Get a user preference."""
    from kria.memory.persistent import sqlite_manager
    rows = await sqlite_manager.execute(
        "SELECT value FROM user_preferences WHERE key = ?",
        (f"pref:{key}",),
    )
    if rows:
        return {"key": key, "value": rows[0][0]}
    return {"key": key, "value": default, "note": "Using default — no preference stored"}


@isolated
async def list_preferences() -> dict:
    """List all user preferences."""
    from kria.memory.persistent import sqlite_manager
    rows = await sqlite_manager.execute(
        "SELECT key, value FROM user_preferences WHERE key LIKE 'pref:%'",
    )
    prefs = {r[0].replace("pref:", ""): r[1] for r in rows}
    return {"preferences": prefs, "count": len(prefs)}


# ── Register ──────────────────────────────────────────────────────

tool_registry.register("set_preference", set_preference,
    description="Set a user preference (e.g., preferred language, default directory).",
    parameters_schema={
        "key": {"type": "string", "description": "Preference name"},
        "value": {"type": "string", "description": "Preference value"},
    })

tool_registry.register("get_preference", get_preference,
    description="Get a user preference value.",
    parameters_schema={
        "key": {"type": "string", "description": "Preference name"},
        "default": {"type": "string", "description": "Default value if not set", "default": ""},
    })

tool_registry.register("list_preferences", list_preferences,
    description="List all user preferences.")
