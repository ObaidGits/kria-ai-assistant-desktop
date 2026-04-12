"""
Application Control Tools (GREEN/YELLOW tier)
=============================================
open_application → GREEN
close_application / kill_process → YELLOW
focus_window → GREEN
"""
import logging
import platform
import subprocess
import sys

from kria.infra.isolation import ToolResult, isolated
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.tools.app_control")

_OS = platform.system()


@isolated
async def open_application(app_name: str, args: list[str] | None = None) -> dict:
    """Launch an application by name or full path."""
    cmd = [app_name] + (args or [])
    if _OS == "Windows":
        proc = subprocess.Popen(cmd, shell=False, creationflags=subprocess.DETACHED_PROCESS)
    else:
        proc = subprocess.Popen(cmd)
    return {"started": True, "pid": proc.pid, "app": app_name}


@isolated
async def close_application(app_name: str, force: bool = False) -> dict:
    """Close an application gracefully (or forcefully)."""
    if _OS == "Windows":
        flag = "/F" if force else ""
        result = subprocess.run(
            ["taskkill", "/IM", app_name, flag] if flag else ["taskkill", "/IM", app_name],
            capture_output=True, text=True
        )
        return {"success": result.returncode == 0, "output": result.stdout.strip()}
    else:
        signal_name = "SIGKILL" if force else "SIGTERM"
        result = subprocess.run(
            ["pkill", "-9" if force else "-15", "-f", app_name],
            capture_output=True, text=True
        )
        return {"success": result.returncode == 0, "signal": signal_name}


@isolated
async def kill_process(pid: int) -> dict:
    """Kill a process by PID."""
    try:
        import psutil
        proc = psutil.Process(pid)
        proc.kill()
        return {"killed": True, "pid": pid, "name": proc.name()}
    except Exception as exc:
        return {"killed": False, "pid": pid, "error": str(exc)}


@isolated
async def focus_window(window_title: str) -> dict:
    """Bring a window with the matching title to the foreground (Windows only)."""
    if _OS != "Windows":
        return {"error": "focus_window is Windows-only"}
    try:
        import ctypes
        user32 = ctypes.windll.user32

        def _find(hwnd, _ctx):
            length = user32.GetWindowTextLengthW(hwnd)
            if length > 0:
                buf = ctypes.create_unicode_buffer(length + 1)
                user32.GetWindowTextW(hwnd, buf, length + 1)
                if window_title.lower() in buf.value.lower():
                    user32.SetForegroundWindow(hwnd)
                    _ctx.append(hwnd)
            return True

        found: list = []
        EnumWindowsProc = ctypes.WINFUNCTYPE(ctypes.c_bool, ctypes.POINTER(ctypes.c_int), ctypes.py_object)
        user32.EnumWindows(EnumWindowsProc(_find), found)
        return {"focused": bool(found), "hwnd": found[0] if found else None}
    except Exception as exc:
        return {"focused": False, "error": str(exc)}


@isolated
async def get_clipboard() -> dict:
    """Read the current clipboard text content."""
    try:
        if _OS == "Windows":
            import subprocess
            result = subprocess.run(["powershell", "-Command", "Get-Clipboard"],
                                    capture_output=True, text=True)
            return {"text": result.stdout.strip()}
        else:
            try:
                result = subprocess.run(["xclip", "-selection", "clipboard", "-o"],
                                        capture_output=True, text=True)
                return {"text": result.stdout}
            except FileNotFoundError:
                result = subprocess.run(["xsel", "--clipboard", "--output"],
                                        capture_output=True, text=True)
                return {"text": result.stdout}
    except Exception as exc:
        return {"text": "", "error": str(exc)}


@isolated
async def set_clipboard(text: str) -> dict:
    """Write text to the clipboard."""
    if _OS == "Windows":
        import subprocess
        subprocess.run(
            ["powershell", "-Command", f"Set-Clipboard -Value '{text}'"],
            capture_output=True
        )
    else:
        try:
            p = subprocess.Popen(["xclip", "-selection", "clipboard"], stdin=subprocess.PIPE)
        except FileNotFoundError:
            p = subprocess.Popen(["xsel", "--clipboard", "--input"], stdin=subprocess.PIPE)
        p.communicate(text.encode())
    return {"set": True, "length": len(text)}


# ── Register ─────────────────────────────────────────────────────

tool_registry.register("open_application", open_application,
    description="Launch an application by name or full path.",
    parameters_schema={"app_name": {"type": "string"}, "args": {"type": "array", "items": {"type": "string"}}})
tool_registry.register("close_application", close_application,
    description="Close a running application by process name.",
    parameters_schema={"app_name": {"type": "string"}, "force": {"type": "boolean", "default": False}})
tool_registry.register("kill_process", kill_process,
    description="Kill a process by its PID.",
    parameters_schema={"pid": {"type": "integer"}})
tool_registry.register("focus_window", focus_window,
    description="Focus a window by its title.",
    parameters_schema={"window_title": {"type": "string"}})
tool_registry.register("get_clipboard", get_clipboard,
    description="Read the current clipboard content.")
tool_registry.register("set_clipboard", set_clipboard,
    description="Write text to the clipboard.",
    parameters_schema={"text": {"type": "string"}})
