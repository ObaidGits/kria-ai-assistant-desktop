"""
File Organizer (YELLOW tier — moves/renames files)
====================================================
Rule-based file organization by extension, date, or size.
"""
import logging
import os
import shutil
from collections import defaultdict
from datetime import datetime
from pathlib import Path

from kria.infra.isolation import ToolResult, isolated
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.tools.file_organizer")

# Default extension-to-category mapping
_EXT_CATEGORIES = {
    "Documents": {".pdf", ".doc", ".docx", ".txt", ".rtf", ".odt", ".md", ".tex"},
    "Spreadsheets": {".xlsx", ".xls", ".csv", ".ods"},
    "Images": {".jpg", ".jpeg", ".png", ".gif", ".bmp", ".svg", ".webp", ".tiff", ".ico"},
    "Videos": {".mp4", ".mkv", ".avi", ".mov", ".wmv", ".flv", ".webm"},
    "Audio": {".mp3", ".wav", ".flac", ".aac", ".ogg", ".wma", ".m4a"},
    "Archives": {".zip", ".tar", ".gz", ".bz2", ".rar", ".7z", ".xz"},
    "Code": {".py", ".js", ".ts", ".java", ".c", ".cpp", ".h", ".rs", ".go", ".rb", ".sh"},
    "Data": {".json", ".xml", ".yaml", ".yml", ".toml", ".ini", ".cfg", ".sql", ".db"},
    "Executables": {".exe", ".msi", ".deb", ".rpm", ".AppImage", ".dmg"},
}


def _get_category(ext: str) -> str:
    ext = ext.lower()
    for category, extensions in _EXT_CATEGORIES.items():
        if ext in extensions:
            return category
    return "Other"


@isolated
async def organize_files(
    directory: str,
    strategy: str = "extension",
    dry_run: bool = True,
) -> dict:
    """Organize files in a directory by extension, date, or size category."""
    src_dir = Path(directory)
    if not src_dir.is_dir():
        return {"error": f"Not a directory: {directory}"}

    moves = []
    for item in src_dir.iterdir():
        if item.is_dir() or item.name.startswith("."):
            continue

        if strategy == "extension":
            category = _get_category(item.suffix)
        elif strategy == "date":
            mtime = datetime.fromtimestamp(item.stat().st_mtime)
            category = mtime.strftime("%Y-%m")
        elif strategy == "size":
            size = item.stat().st_size
            if size < 1024 * 100:
                category = "Small (<100KB)"
            elif size < 1024 * 1024 * 10:
                category = "Medium (100KB-10MB)"
            else:
                category = "Large (>10MB)"
        else:
            return {"error": f"Unknown strategy: {strategy}. Use: extension, date, size"}

        dest_dir = src_dir / category
        dest_path = dest_dir / item.name

        moves.append({
            "file": item.name,
            "from": str(item),
            "to": str(dest_path),
            "category": category,
        })

        if not dry_run:
            dest_dir.mkdir(exist_ok=True)
            shutil.move(str(item), str(dest_path))

    return {
        "directory": directory,
        "strategy": strategy,
        "dry_run": dry_run,
        "files_to_move": len(moves),
        "moves": moves[:50],  # cap output
    }


# ── Register ──────────────────────────────────────────────────────

tool_registry.register("organize_files", organize_files,
    description="Organize files in a directory by extension, date, or size. Use dry_run=true to preview.",
    parameters_schema={
        "directory": {"type": "string", "description": "Directory to organize"},
        "strategy": {"type": "string", "description": "extension | date | size", "default": "extension"},
        "dry_run": {"type": "boolean", "description": "Preview mode (no actual moves)", "default": True},
    })
