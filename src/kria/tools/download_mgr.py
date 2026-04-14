"""
Download Manager (YELLOW tier — modifies filesystem)
=====================================================
Streaming file downloads with progress tracking and size limits.
"""
import logging
from pathlib import Path
from urllib.parse import urlparse

import httpx

from kria.infra.config import settings
from kria.infra.isolation import ToolResult, isolated
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.tools.download_mgr")


@isolated
async def download_file(url: str, filename: str = "", directory: str = "") -> dict:
    """Download a file from a URL to local disk with size limits."""
    save_dir = Path(directory or settings.downloads_dir).expanduser()
    save_dir.mkdir(parents=True, exist_ok=True)

    max_bytes = settings.max_download_size_mb * 1024 * 1024

    async with httpx.AsyncClient(timeout=120.0, follow_redirects=True) as client:
        # HEAD request for metadata
        try:
            head = await client.head(url)
            content_length = int(head.headers.get("content-length", 0))
            content_type = head.headers.get("content-type", "")
        except Exception:
            content_length = 0
            content_type = ""

        if content_length > max_bytes:
            return {
                "error": f"File too large ({content_length} bytes). Max: {max_bytes}",
            }

        # Determine filename
        if not filename:
            filename = Path(urlparse(url).path).name or "download"

        save_path = save_dir / filename

        # Stream download
        downloaded = 0
        async with client.stream("GET", url) as resp:
            resp.raise_for_status()
            with open(save_path, "wb") as f:
                async for chunk in resp.aiter_bytes(chunk_size=65536):
                    downloaded += len(chunk)
                    if downloaded > max_bytes:
                        return {"error": f"Download exceeded max size ({max_bytes} bytes)"}
                    f.write(chunk)

    return {
        "path": str(save_path),
        "size_bytes": downloaded,
        "content_type": content_type,
        "url": url,
    }


# ── Register ──────────────────────────────────────────────────────

tool_registry.register("download_file", download_file,
    description="Download a file from a URL to local disk. Reports progress.",
    parameters_schema={
        "url": {"type": "string", "description": "URL to download"},
        "filename": {"type": "string", "description": "Save as filename", "default": ""},
        "directory": {"type": "string", "description": "Save directory", "default": ""},
    })
