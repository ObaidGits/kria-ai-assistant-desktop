"""
Disk Management Tools (GREEN tier — read-only scanning)
========================================================
Find large files, duplicate files, and calculate directory sizes.
"""
import hashlib
import logging
import os
from collections import defaultdict
from pathlib import Path

from kria.infra.isolation import ToolResult, isolated
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.tools.disk_mgmt")


@isolated
async def find_large_files(directory: str, top_n: int = 20, min_size_mb: int = 10) -> list:
    """Find the largest files in a directory."""
    min_bytes = min_size_mb * 1024 * 1024
    files = []
    for root, dirs, filenames in os.walk(directory):
        for fname in filenames:
            fpath = Path(root) / fname
            try:
                size = fpath.stat().st_size
                if size >= min_bytes:
                    files.append({"path": str(fpath), "size_mb": round(size / (1024 * 1024), 2)})
            except (PermissionError, OSError):
                continue
        if len(files) > 1000:
            break
    files.sort(key=lambda x: x["size_mb"], reverse=True)
    return files[:top_n]


@isolated
async def find_duplicate_files(directory: str, min_size_kb: int = 100) -> dict:
    """Find duplicate files by hash in a directory."""
    min_bytes = min_size_kb * 1024
    size_groups: dict[int, list[str]] = defaultdict(list)

    for root, _, filenames in os.walk(directory):
        for fname in filenames:
            fpath = Path(root) / fname
            try:
                size = fpath.stat().st_size
                if size >= min_bytes:
                    size_groups[size].append(str(fpath))
            except (PermissionError, OSError):
                continue

    duplicates = []
    for size, paths in size_groups.items():
        if len(paths) < 2:
            continue
        hash_groups: dict[str, list[str]] = defaultdict(list)
        for p in paths:
            try:
                with open(p, "rb") as f:
                    h = hashlib.md5(f.read(8192)).hexdigest()  # noqa: S324
                hash_groups[h].append(p)
            except Exception:
                continue
        for h, hpaths in hash_groups.items():
            if len(hpaths) >= 2:
                duplicates.append({
                    "hash": h,
                    "size_kb": round(size / 1024, 1),
                    "files": hpaths,
                })

    return {"duplicate_groups": duplicates, "total_groups": len(duplicates)}


@isolated
async def calculate_dir_size(path: str) -> dict:
    """Calculate total size of a directory recursively."""
    total = 0
    file_count = 0
    for root, dirs, files in os.walk(path):
        for f in files:
            try:
                total += (Path(root) / f).stat().st_size
                file_count += 1
            except (PermissionError, OSError):
                continue
    return {
        "path": path,
        "size_mb": round(total / (1024 * 1024), 2),
        "size_gb": round(total / (1024 ** 3), 2),
        "file_count": file_count,
    }


# ── Register ──────────────────────────────────────────────────────

tool_registry.register("find_large_files", find_large_files,
    description="Find the largest files in a directory.",
    parameters_schema={
        "directory": {"type": "string", "description": "Directory to scan"},
        "top_n": {"type": "integer", "default": 20},
        "min_size_mb": {"type": "integer", "default": 10},
    })

tool_registry.register("find_duplicate_files", find_duplicate_files,
    description="Find duplicate files by hash in a directory.",
    parameters_schema={
        "directory": {"type": "string", "description": "Directory to scan"},
        "min_size_kb": {"type": "integer", "default": 100},
    })

tool_registry.register("calculate_dir_size", calculate_dir_size,
    description="Calculate total size of a directory recursively.",
    parameters_schema={
        "path": {"type": "string", "description": "Directory path"},
    })
