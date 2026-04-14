"""
Code Snippet Library (GREEN tier)
===================================
CRUD operations for reusable code/text snippets stored as files.
"""
import json
import logging
from pathlib import Path

from kria.infra.config import settings
from kria.infra.isolation import ToolResult, isolated
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.tools.snippet_lib")

_SNIPPETS_DIR = Path(settings.snippets_dir).expanduser()
_SNIPPETS_DIR.mkdir(parents=True, exist_ok=True)


@isolated
async def save_snippet(name: str, content: str, language: str = "", tags: str = "") -> dict:
    """Save a code/text snippet to the library."""
    snippet = {
        "name": name,
        "content": content,
        "language": language,
        "tags": [t.strip() for t in tags.split(",") if t.strip()],
    }
    path = _SNIPPETS_DIR / f"{name}.json"
    path.write_text(json.dumps(snippet, indent=2))
    return {"saved": True, "name": name, "path": str(path)}


@isolated
async def get_snippet(name: str) -> dict:
    """Retrieve a snippet by name."""
    path = _SNIPPETS_DIR / f"{name}.json"
    if not path.exists():
        return {"error": f"Snippet '{name}' not found"}
    return json.loads(path.read_text())


@isolated
async def list_snippets(tag: str = "") -> dict:
    """List all saved snippets, optionally filtered by tag."""
    snippets = []
    for f in _SNIPPETS_DIR.glob("*.json"):
        try:
            data = json.loads(f.read_text())
            if tag and tag.lower() not in [t.lower() for t in data.get("tags", [])]:
                continue
            snippets.append({
                "name": data.get("name", f.stem),
                "language": data.get("language", ""),
                "tags": data.get("tags", []),
            })
        except Exception:
            continue
    return {"snippets": snippets, "count": len(snippets)}


@isolated
async def delete_snippet(name: str) -> dict:
    """Delete a snippet by name."""
    path = _SNIPPETS_DIR / f"{name}.json"
    if path.exists():
        path.unlink()
        return {"deleted": True, "name": name}
    return {"error": f"Snippet '{name}' not found"}


# ── Register ──────────────────────────────────────────────────────

tool_registry.register("save_snippet", save_snippet,
    description="Save a code/text snippet to the library.",
    parameters_schema={
        "name": {"type": "string", "description": "Snippet name"},
        "content": {"type": "string", "description": "Snippet content"},
        "language": {"type": "string", "description": "Programming language", "default": ""},
        "tags": {"type": "string", "description": "Comma-separated tags", "default": ""},
    })

tool_registry.register("get_snippet", get_snippet,
    description="Retrieve a code snippet by name.",
    parameters_schema={
        "name": {"type": "string", "description": "Snippet name"},
    })

tool_registry.register("list_snippets", list_snippets,
    description="List all saved snippets, optionally filtered by tag.",
    parameters_schema={
        "tag": {"type": "string", "default": ""},
    })

tool_registry.register("delete_snippet", delete_snippet,
    description="Delete a snippet by name.",
    parameters_schema={
        "name": {"type": "string", "description": "Snippet name"},
    })
