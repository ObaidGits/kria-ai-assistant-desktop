"""
System Information Tools (GREEN tier — read-only)
==================================================
All functions wrapped with @isolated so they never raise into the agent loop.
Each function registers itself in tool_registry upon module import.
"""
import platform
import logging

try:
    import psutil
    _HAS_PSUTIL = True
except ImportError:
    _HAS_PSUTIL = False

from kria.infra.isolation import ToolResult, isolated
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.tools.system_info")


@isolated
async def get_cpu_usage() -> dict:
    if not _HAS_PSUTIL:
        return {"error": "psutil not available"}
    return {
        "cpu_percent": psutil.cpu_percent(interval=0.1),
        "cpu_count_logical": psutil.cpu_count(logical=True),
        "cpu_count_physical": psutil.cpu_count(logical=False),
        "cpu_freq_mhz": getattr(psutil.cpu_freq(), "current", None),
    }


@isolated
async def get_memory_info() -> dict:
    if not _HAS_PSUTIL:
        return {"error": "psutil not available"}
    vm = psutil.virtual_memory()
    swap = psutil.swap_memory()
    return {
        "total_gb": round(vm.total / 1024**3, 2),
        "available_gb": round(vm.available / 1024**3, 2),
        "used_percent": vm.percent,
        "swap_total_gb": round(swap.total / 1024**3, 2),
        "swap_used_percent": swap.percent,
    }


@isolated
async def get_disk_space(path: str = "/") -> dict:
    if not _HAS_PSUTIL:
        return {"error": "psutil not available"}
    usage = psutil.disk_usage(path)
    return {
        "path": path,
        "total_gb": round(usage.total / 1024**3, 2),
        "free_gb": round(usage.free / 1024**3, 2),
        "used_percent": usage.percent,
    }


@isolated
async def get_network_status() -> dict:
    if not _HAS_PSUTIL:
        return {"error": "psutil not available"}
    counters = psutil.net_io_counters()
    return {
        "bytes_sent": counters.bytes_sent,
        "bytes_recv": counters.bytes_recv,
        "packets_sent": counters.packets_sent,
        "packets_recv": counters.packets_recv,
        "connections": len(psutil.net_connections()),
    }


@isolated
async def get_battery_status() -> dict:
    if not _HAS_PSUTIL:
        return {"error": "psutil not available"}
    bat = psutil.sensors_battery()
    if bat is None:
        return {"battery": "No battery detected (desktop)"}
    return {
        "percent": bat.percent,
        "plugged_in": bat.power_plugged,
        "seconds_left": bat.secsleft if bat.secsleft > 0 else None,
    }


@isolated
async def get_time() -> dict:
    from datetime import datetime, timezone
    now = datetime.now()
    now_utc = datetime.now(timezone.utc)
    return {
        "local_time": now.isoformat(),
        "utc_time": now_utc.isoformat(),
        "platform": platform.system(),
    }


@isolated
async def screenshot(save_path: str = "") -> dict:
    """Take a screenshot. Requires Pillow."""
    try:
        from PIL import ImageGrab
        img = ImageGrab.grab()
        if save_path:
            img.save(save_path)
            return {"saved": save_path, "size": img.size}
        return {"size": img.size, "mode": img.mode, "saved": False}
    except ImportError:
        return {"error": "Pillow not installed. Run: pip install pillow"}


@isolated
async def list_running_apps() -> dict:
    if not _HAS_PSUTIL:
        return {"error": "psutil not available"}
    apps = []
    for proc in psutil.process_iter(["pid", "name", "status", "memory_percent"]):
        try:
            apps.append(proc.info)
        except (psutil.NoSuchProcess, psutil.AccessDenied):
            pass
    return {"processes": apps, "count": len(apps)}


# ── Register all ─────────────────────────────────────────────────

tool_registry.register("get_cpu_usage", get_cpu_usage,
    description="Get current CPU usage, core count, and frequency.")
tool_registry.register("get_memory_info", get_memory_info,
    description="Get RAM and swap usage statistics.")
tool_registry.register("get_disk_space", get_disk_space,
    description="Get disk space usage for a path.",
    parameters_schema={"path": {"type": "string", "default": "/"}})
tool_registry.register("get_network_status", get_network_status,
    description="Get network I/O counters and connection count.")
tool_registry.register("get_battery_status", get_battery_status,
    description="Get battery level and charging status.")
tool_registry.register("get_time", get_time,
    description="Get current local and UTC time.")
tool_registry.register("screenshot", screenshot,
    description="Take a screenshot of the desktop.",
    parameters_schema={"save_path": {"type": "string", "default": ""}})
tool_registry.register("list_running_apps", list_running_apps,
    description="List all running processes with PID, name, and memory usage.")
