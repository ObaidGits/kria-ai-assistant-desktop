"""
Clipboard Manager (GREEN read / YELLOW write)
===============================================
Read, write, and track clipboard history.
"""
import logging
import subprocess
from collections import deque

from kria.infra.isolation import ToolResult, isolated
from kria.infra.platform_detect import OS, OSType
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.tools.clipboard_mgr")

_clipboard_history: deque = deque(maxlen=20)


@isolated
async def get_clipboard() -> dict:
    """Read current clipboard content."""
    try:
        if OS == OSType.LINUX:
            result = subprocess.run(
                ["xclip", "-selection", "clipboard", "-o"],
                capture_output=True, text=True, timeout=5,
            )
            content = result.stdout
        elif OS == OSType.WINDOWS:
            result = subprocess.run(
                ["powershell", "-Command", "Get-Clipboard"],
                capture_output=True, text=True, timeout=5,
            )
            content = result.stdout.strip()
        elif OS == OSType.MACOS:
            result = subprocess.run(
                ["pbpaste"], capture_output=True, text=True, timeout=5,
            )
            content = result.stdout
        else:
            return {"error": f"Unsupported OS: {OS.value}"}

        _clipboard_history.append(content)
        return {"content": content[:5000], "length": len(content)}
    except FileNotFoundError:
        return {"error": "Clipboard tool not found (install xclip on Linux)"}
    except Exception as e:
        return {"error": str(e)}


@isolated
async def set_clipboard(text: str) -> str:
    """Write text to clipboard."""
    try:
        if OS == OSType.LINUX:
            proc = subprocess.Popen(
                ["xclip", "-selection", "clipboard"], stdin=subprocess.PIPE,
            )
            proc.communicate(text.encode(), timeout=5)
        elif OS == OSType.WINDOWS:
            subprocess.run(
                ["powershell", "-Command", f"Set-Clipboard -Value '{text}'"],
                timeout=5,
            )
        elif OS == OSType.MACOS:
            proc = subprocess.Popen(["pbcopy"], stdin=subprocess.PIPE)
            proc.communicate(text.encode(), timeout=5)
        else:
            return f"Unsupported OS: {OS.value}"
        _clipboard_history.append(text)
        return f"Copied {len(text)} chars to clipboard"
    except FileNotFoundError:
        return "Clipboard tool not found (install xclip on Linux)"
    except Exception as e:
        return f"Failed: {e}"


@isolated
async def clipboard_history() -> dict:
    """Get recent clipboard history (session only, last 20 entries)."""
    return {"entries": list(_clipboard_history), "count": len(_clipboard_history)}


# ── Register ──────────────────────────────────────────────────────

tool_registry.register("get_clipboard", get_clipboard,
    description="Read current clipboard content.")

tool_registry.register("set_clipboard", set_clipboard,
    description="Write text to clipboard.",
    parameters_schema={
        "text": {"type": "string", "description": "Text to copy to clipboard"},
    })

tool_registry.register("clipboard_history", clipboard_history,
    description="Get recent clipboard history (session only, last 20 entries).")
