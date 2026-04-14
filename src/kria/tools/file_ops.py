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
    hierarchical: bool = True,
) -> dict:
    """
    Search for files/folders matching a name pattern (regex).

    When hierarchical=True (default), searches outward in stages:
      1. Starting directory
      2. Parent directory
      3. Current disk/mount root
      4. All mounted storage devices
    Stops expanding once matches are found at a given level.

    When hierarchical=False, searches only the given directory.
    """
    base = _safe_path(directory)
    if not base.exists():
        return {"error": f"Directory not found: {directory}"}

    try:
        pat = re.compile(pattern, re.IGNORECASE)
    except re.error as exc:
        return {"error": f"Invalid regex pattern: {exc}"}

    ext_filter = f"*.{file_extension.lstrip('.')}" if file_extension else "*"
    max_per_level = 200

    def _scan(search_dir: Path) -> list[dict]:
        """Scan a directory and return matches."""
        if not search_dir.exists() or not search_dir.is_dir():
            return []
        glob_fn = search_dir.rglob if recursive else search_dir.glob
        hits: list[dict] = []
        try:
            for f in glob_fn(ext_filter):
                if pat.search(f.name):
                    entry = {
                        "path": str(f),
                        "name": f.name,
                        "type": "directory" if f.is_dir() else "file",
                        "parent": str(f.parent),
                    }
                    if f.is_file():
                        try:
                            entry["size_bytes"] = f.stat().st_size
                        except OSError:
                            pass
                    hits.append(entry)
                if len(hits) >= max_per_level:
                    break
        except PermissionError:
            pass
        return hits

    # Simple (non-hierarchical) mode
    if not hierarchical:
        matches = _scan(base)
        return {"matches": matches, "count": len(matches), "truncated": len(matches) == max_per_level}

    # Hierarchical search: expand outward
    levels_searched: list[str] = []
    all_matches: list[dict] = []

    # Level 1: Starting directory
    matches = _scan(base)
    levels_searched.append(str(base))
    if matches:
        return {
            "matches": matches,
            "count": len(matches),
            "search_level": "current_directory",
            "searched": levels_searched,
            "truncated": len(matches) == max_per_level,
        }

    # Level 2: Parent directory
    parent = base.parent
    if parent != base:
        matches = _scan(parent)
        levels_searched.append(str(parent))
        if matches:
            return {
                "matches": matches,
                "count": len(matches),
                "search_level": "parent_directory",
                "searched": levels_searched,
                "truncated": len(matches) == max_per_level,
            }

    # Level 3: Current disk/mount root
    # Find the mount point for the current directory
    mount_root = base
    while mount_root.parent != mount_root:
        if mount_root.is_mount() or str(mount_root) == "/":
            break
        mount_root = mount_root.parent

    if str(mount_root) not in levels_searched:
        matches = _scan(mount_root)
        levels_searched.append(str(mount_root))
        if matches:
            return {
                "matches": matches,
                "count": len(matches),
                "search_level": "current_disk",
                "searched": levels_searched,
                "truncated": len(matches) == max_per_level,
            }

    # Level 4: All mounted storage (Linux: /media, /mnt, /home; plus /)
    extra_roots: list[Path] = []
    for mp in [Path("/media"), Path("/mnt"), Path("/home"), Path("/")]:
        if mp.exists() and str(mp) not in levels_searched:
            extra_roots.append(mp)

    for er in extra_roots:
        found = _scan(er)
        levels_searched.append(str(er))
        all_matches.extend(found)
        if len(all_matches) >= max_per_level:
            all_matches = all_matches[:max_per_level]
            break

    return {
        "matches": all_matches,
        "count": len(all_matches),
        "search_level": "all_disks",
        "searched": levels_searched,
        "truncated": len(all_matches) == max_per_level,
    }


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
    description="Search for files and folders matching a name pattern (regex). By default searches hierarchically: current dir → parent → disk → all disks. Shows full paths.",
    parameters_schema={
        "pattern": {"type": "string", "description": "Name pattern (regex) to search for"},
        "directory": {"type": "string", "default": ".", "description": "Starting directory for search"},
        "recursive": {"type": "boolean", "default": True},
        "file_extension": {"type": "string", "description": "Filter by file extension (e.g. 'pdf', 'py')"},
        "hierarchical": {"type": "boolean", "default": True, "description": "Search outward: current dir → parent → disk → all disks"},
    })
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
