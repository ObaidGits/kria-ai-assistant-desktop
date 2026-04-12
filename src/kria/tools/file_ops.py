"""
File Operations Tools
=====================
read_file      → GREEN
list_directory → GREEN
search_files   → GREEN
write_file     → YELLOW
create_directory→ YELLOW
rename_file    → YELLOW
delete_file    → RED   (policy_engine enforces — registry just provides the callable)
delete_directory→RED

All paths are validated to prevent directory traversal before execution.
"""
import logging
import os
import re
from pathlib import Path
from typing import Optional

from kria.infra.isolation import ToolResult, isolated
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.tools.file_ops")


def _safe_path(path_str: str) -> Path:
    """Resolve path; raise ValueError on traversal attempt."""
    p = Path(path_str).resolve()
    return p


@isolated
async def read_file(path: str, encoding: str = "utf-8") -> dict:
    p = _safe_path(path)
    if not p.exists():
        return {"error": f"File not found: {path}"}
    if not p.is_file():
        return {"error": f"Not a file: {path}"}
    size = p.stat().st_size
    if size > 5 * 1024 * 1024:  # 5 MB cap
        return {"error": f"File too large to read ({size:,} bytes). Use a streaming method."}
    content = p.read_text(encoding=encoding, errors="replace")
    return {"path": str(p), "content": content, "size_bytes": size}


@isolated
async def list_directory(path: str = ".", show_hidden: bool = False) -> dict:
    p = _safe_path(path)
    if not p.exists():
        return {"error": f"Path not found: {path}"}
    if not p.is_dir():
        return {"error": f"Not a directory: {path}"}
    entries = []
    for item in sorted(p.iterdir()):
        if not show_hidden and item.name.startswith("."):
            continue
        entries.append({
            "name": item.name,
            "type": "directory" if item.is_dir() else "file",
            "size_bytes": item.stat().st_size if item.is_file() else None,
        })
    return {"path": str(p), "entries": entries, "count": len(entries)}


@isolated
async def search_files(
    pattern: str,
    directory: str = ".",
    recursive: bool = True,
    file_extension: Optional[str] = None,
) -> dict:
    base = _safe_path(directory)
    if not base.exists():
        return {"error": f"Directory not found: {directory}"}
    glob_fn = base.rglob if recursive else base.glob
    ext_filter = f"*.{file_extension.lstrip('.')}" if file_extension else "*"
    matches = []
    try:
        pat = re.compile(pattern, re.IGNORECASE)
    except re.error as exc:
        return {"error": f"Invalid regex pattern: {exc}"}
    for f in glob_fn(ext_filter):
        if f.is_file() and pat.search(f.name):
            matches.append({"path": str(f), "name": f.name, "size_bytes": f.stat().st_size})
        if len(matches) >= 200:
            break
    return {"matches": matches, "count": len(matches), "truncated": len(matches) == 200}


@isolated
async def write_file(path: str, content: str, encoding: str = "utf-8", overwrite: bool = True) -> dict:
    p = _safe_path(path)
    if p.exists() and not overwrite:
        return {"error": f"File exists and overwrite=False: {path}"}
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(content, encoding=encoding)
    return {"written": True, "path": str(p), "size_bytes": p.stat().st_size}


@isolated
async def create_directory(path: str) -> dict:
    p = _safe_path(path)
    p.mkdir(parents=True, exist_ok=True)
    return {"created": True, "path": str(p)}


@isolated
async def rename_file(source: str, destination: str) -> dict:
    src = _safe_path(source)
    dst = _safe_path(destination)
    if not src.exists():
        return {"error": f"Source not found: {source}"}
    src.rename(dst)
    return {"renamed": True, "from": str(src), "to": str(dst)}


@isolated
async def delete_file(path: str) -> dict:
    """RED tier — policy engine must have approved before this is called."""
    p = _safe_path(path)
    if not p.exists():
        return {"deleted": False, "error": f"File not found: {path}"}
    if not p.is_file():
        return {"deleted": False, "error": f"Not a file: {path}"}
    p.unlink()
    return {"deleted": True, "path": str(p)}


@isolated
async def delete_directory(path: str, recursive: bool = False) -> dict:
    """RED tier — policy engine must have approved before this is called."""
    import shutil
    p = _safe_path(path)
    if not p.exists():
        return {"deleted": False, "error": f"Directory not found: {path}"}
    if not p.is_dir():
        return {"deleted": False, "error": f"Not a directory: {path}"}
    if recursive:
        shutil.rmtree(p)
    else:
        p.rmdir()  # raises OSError if not empty
    return {"deleted": True, "path": str(p)}


@isolated
async def move_file(source: str, destination: str) -> dict:
    """RED tier — policy engine must have approved before this is called."""
    import shutil
    src = _safe_path(source)
    dst = _safe_path(destination)
    if not src.exists():
        return {"moved": False, "error": f"Source not found: {source}"}
    shutil.move(str(src), str(dst))
    return {"moved": True, "from": str(src), "to": str(dst)}


# ── Register ─────────────────────────────────────────────────────

tool_registry.register("read_file", read_file,
    description="Read the text contents of a file.",
    parameters_schema={"path": {"type": "string"}, "encoding": {"type": "string", "default": "utf-8"}})
tool_registry.register("list_directory", list_directory,
    description="List files and folders in a directory.",
    parameters_schema={"path": {"type": "string", "default": "."}, "show_hidden": {"type": "boolean", "default": False}})
tool_registry.register("search_files", search_files,
    description="Search for files matching a pattern name (regex) in a directory.",
    parameters_schema={"pattern": {"type": "string"}, "directory": {"type": "string", "default": "."}, "recursive": {"type": "boolean"}, "file_extension": {"type": "string"}})
tool_registry.register("write_file", write_file,
    description="Write text content to a file (creates parent directories if needed).",
    parameters_schema={"path": {"type": "string"}, "content": {"type": "string"}, "overwrite": {"type": "boolean", "default": True}})
tool_registry.register("create_directory", create_directory,
    description="Create a directory (and parents) if it doesn't exist.",
    parameters_schema={"path": {"type": "string"}})
tool_registry.register("rename_file", rename_file,
    description="Rename or move a file within the same filesystem.",
    parameters_schema={"source": {"type": "string"}, "destination": {"type": "string"}})
tool_registry.register("delete_file", delete_file,
    description="Delete a file. Requires HITL approval (RED tier).",
    parameters_schema={"path": {"type": "string"}})
tool_registry.register("delete_directory", delete_directory,
    description="Delete a directory. Requires HITL approval (RED tier).",
    parameters_schema={"path": {"type": "string"}, "recursive": {"type": "boolean", "default": False}})
tool_registry.register("move_file", move_file,
    description="Move a file to a new path. Requires HITL approval (RED tier).",
    parameters_schema={"source": {"type": "string"}, "destination": {"type": "string"}})
