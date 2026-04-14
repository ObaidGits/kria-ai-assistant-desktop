"""
Knowledge Tools (GREEN tier)
=============================
Store, recall, and search persistent facts and knowledge.
"""
import logging

from kria.infra.isolation import ToolResult, isolated
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.tools.knowledge_tools")


@isolated
async def remember_fact(key: str, value: str) -> str:
    """Store a fact or piece of information for later recall."""
    from kria.memory.persistent import sqlite_manager
    await sqlite_manager.execute(
        "INSERT OR REPLACE INTO user_preferences (key, value) VALUES (?, ?)",
        (f"fact:{key}", value),
    )
    return f"Remembered: {key} = {value}"


@isolated
async def recall_fact(key: str) -> dict:
    """Recall a previously stored fact."""
    from kria.memory.persistent import sqlite_manager
    rows = await sqlite_manager.execute(
        "SELECT value FROM user_preferences WHERE key = ?",
        (f"fact:{key}",),
    )
    if rows:
        return {"key": key, "value": rows[0][0]}
    return {"key": key, "value": None, "note": "No fact found with this key"}


@isolated
async def list_remembered() -> dict:
    """List all stored facts."""
    from kria.memory.persistent import sqlite_manager
    rows = await sqlite_manager.execute(
        "SELECT key, value FROM user_preferences WHERE key LIKE 'fact:%'",
    )
    facts = {r[0].replace("fact:", ""): r[1] for r in rows}
    return {"facts": facts, "count": len(facts)}


@isolated
async def search_knowledge(query: str) -> dict:
    """Semantic search across ingested documents and stored knowledge."""
    from kria.memory.semantic import semantic_memory
    results = await semantic_memory.search(query, n_results=5)
    return {"query": query, "results": results, "count": len(results)}


# ── Register ──────────────────────────────────────────────────────

tool_registry.register("remember_fact", remember_fact,
    description="Store a fact or piece of information for later recall.",
    parameters_schema={
        "key": {"type": "string", "description": "Short label (e.g., 'project_deadline')"},
        "value": {"type": "string", "description": "The information to remember"},
    })

tool_registry.register("recall_fact", recall_fact,
    description="Recall a previously stored fact.",
    parameters_schema={
        "key": {"type": "string", "description": "Fact label to recall"},
    })

tool_registry.register("list_remembered", list_remembered,
    description="List all stored facts.")

tool_registry.register("search_knowledge", search_knowledge,
    description="Semantic search across ingested documents and stored knowledge.",
    parameters_schema={
        "query": {"type": "string", "description": "Search query"},
    })
