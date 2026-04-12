"""
Process Management Tools (YELLOW/RED tier)
==========================================
set_process_priority → RED
modify_scheduled_task → RED
Other process queries → GREEN
"""
import logging
import platform
import subprocess

from kria.infra.isolation import ToolResult, isolated
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.tools.process_mgmt")

_OS = platform.system()

try:
    import psutil
    _HAS_PSUTIL = True
except ImportError:
    _HAS_PSUTIL = False


@isolated
async def get_process_info(pid: int) -> dict:
    if not _HAS_PSUTIL:
        return {"error": "psutil not available"}
    try:
        proc = psutil.Process(pid)
        return {
            "pid": proc.pid,
            "name": proc.name(),
            "status": proc.status(),
            "cpu_percent": proc.cpu_percent(interval=0.1),
            "memory_mb": round(proc.memory_info().rss / 1024**2, 2),
            "cmdline": proc.cmdline(),
            "username": proc.username(),
        }
    except psutil.NoSuchProcess:
        return {"error": f"Process {pid} not found"}
    except psutil.AccessDenied:
        return {"error": f"Access denied to process {pid}"}


@isolated
async def list_processes_by_name(name_pattern: str) -> dict:
    if not _HAS_PSUTIL:
        return {"error": "psutil not available"}
    import re
    pat = re.compile(name_pattern, re.IGNORECASE)
    results = []
    for proc in psutil.process_iter(["pid", "name", "cpu_percent", "memory_percent"]):
        try:
            if pat.search(proc.info["name"] or ""):
                results.append(proc.info)
        except (psutil.NoSuchProcess, psutil.AccessDenied):
            pass
    return {"matches": results, "count": len(results)}


@isolated
async def set_process_priority(pid: int, priority: str) -> dict:
    """
    Set process CPU priority. RED tier.
    priority: 'low' | 'below_normal' | 'normal' | 'above_normal' | 'high' | 'realtime'
    """
    if not _HAS_PSUTIL:
        return {"error": "psutil not available"}

    PRIORITY_MAP = {
        "low": psutil.IDLE_PRIORITY_CLASS if _OS == "Windows" else 19,
        "below_normal": psutil.BELOW_NORMAL_PRIORITY_CLASS if _OS == "Windows" else 10,
        "normal": psutil.NORMAL_PRIORITY_CLASS if _OS == "Windows" else 0,
        "above_normal": psutil.ABOVE_NORMAL_PRIORITY_CLASS if _OS == "Windows" else -5,
        "high": psutil.HIGH_PRIORITY_CLASS if _OS == "Windows" else -10,
        "realtime": psutil.REALTIME_PRIORITY_CLASS if _OS == "Windows" else -20,
    }
    p_val = PRIORITY_MAP.get(priority.lower())
    if p_val is None:
        return {"error": f"Unknown priority: {priority!r}. Use: {list(PRIORITY_MAP)}"}
    try:
        proc = psutil.Process(pid)
        if _OS == "Windows":
            proc.nice(p_val)
        else:
            proc.nice(p_val)
        return {"pid": pid, "priority": priority, "success": True}
    except Exception as exc:
        return {"success": False, "error": str(exc)}


@isolated
async def modify_scheduled_task(
    task_name: str,
    action: str,
    schedule: str = "",
    command: str = "",
) -> dict:
    """
    Manage Windows Scheduled Tasks. RED tier.
    action: 'create' | 'delete' | 'enable' | 'disable' | 'run'
    """
    if _OS != "Windows":
        return {"error": "modify_scheduled_task is Windows-only"}

    if action == "delete":
        result = subprocess.run(
            ["schtasks", "/Delete", "/TN", task_name, "/F"],
            capture_output=True, text=True
        )
    elif action == "enable":
        result = subprocess.run(
            ["schtasks", "/Change", "/TN", task_name, "/ENABLE"],
            capture_output=True, text=True
        )
    elif action == "disable":
        result = subprocess.run(
            ["schtasks", "/Change", "/TN", task_name, "/DISABLE"],
            capture_output=True, text=True
        )
    elif action == "run":
        result = subprocess.run(
            ["schtasks", "/Run", "/TN", task_name],
            capture_output=True, text=True
        )
    elif action == "create" and command and schedule:
        result = subprocess.run(
            ["schtasks", "/Create", "/TN", task_name, "/TR", command, "/SC", schedule, "/F"],
            capture_output=True, text=True
        )
    else:
        return {"error": f"Invalid action '{action}' or missing required parameters."}

    return {
        "task": task_name,
        "action": action,
        "success": result.returncode == 0,
        "output": result.stdout.strip(),
    }


@isolated
async def install_package(package: str, manager: str = "pip") -> dict:
    """Install a software package. YELLOW tier."""
    if manager == "pip":
        result = subprocess.run(
            ["pip", "install", package],
            capture_output=True, text=True
        )
    elif manager == "winget":
        result = subprocess.run(
            ["winget", "install", "--silent", package],
            capture_output=True, text=True
        )
    elif manager == "choco":
        result = subprocess.run(
            ["choco", "install", "-y", package],
            capture_output=True, text=True
        )
    else:
        return {"error": f"Unknown package manager: {manager!r}. Use: pip, winget, choco"}
    return {
        "package": package,
        "manager": manager,
        "success": result.returncode == 0,
        "output": (result.stdout + result.stderr).strip()[-2000:],
    }


@isolated
async def uninstall_package(package: str, manager: str = "pip") -> dict:
    """Uninstall a package. RED tier."""
    if manager == "pip":
        result = subprocess.run(
            ["pip", "uninstall", "-y", package],
            capture_output=True, text=True
        )
    elif manager == "winget":
        result = subprocess.run(
            ["winget", "uninstall", "--silent", package],
            capture_output=True, text=True
        )
    else:
        return {"error": f"Uninstall not supported for manager: {manager!r}"}
    return {
        "package": package,
        "manager": manager,
        "success": result.returncode == 0,
        "output": result.stdout.strip(),
    }


# ── Register ─────────────────────────────────────────────────────

tool_registry.register("get_process_info", get_process_info,
    description="Get detailed information about a process by PID.",
    parameters_schema={"pid": {"type": "integer"}})
tool_registry.register("list_processes_by_name", list_processes_by_name,
    description="Search for running processes by name (regex).",
    parameters_schema={"name_pattern": {"type": "string"}})
tool_registry.register("set_process_priority", set_process_priority,
    description="Change a process CPU priority. Requires approval (RED).",
    parameters_schema={"pid": {"type": "integer"}, "priority": {"type": "string"}})
tool_registry.register("modify_scheduled_task", modify_scheduled_task,
    description="Create/delete/enable/disable/run Windows Scheduled Tasks. Requires approval (RED).",
    parameters_schema={"task_name": {"type": "string"}, "action": {"type": "string"}, "schedule": {"type": "string"}, "command": {"type": "string"}})
tool_registry.register("install_package", install_package,
    description="Install a package via pip, winget, or choco.",
    parameters_schema={"package": {"type": "string"}, "manager": {"type": "string", "default": "pip"}})
tool_registry.register("uninstall_package", uninstall_package,
    description="Uninstall a package. Requires approval (RED).",
    parameters_schema={"package": {"type": "string"}, "manager": {"type": "string", "default": "pip"}})
