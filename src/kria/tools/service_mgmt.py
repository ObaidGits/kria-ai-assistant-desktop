"""
Service Management Tools (GREEN read / RED manage)
====================================================
Cross-platform service listing and control via systemctl/PowerShell.
"""
import asyncio
import logging

from kria.infra.isolation import ToolResult, isolated
from kria.infra.platform_detect import OS, OSType
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.tools.service_mgmt")


@isolated
async def list_services(filter: str = "") -> dict:
    """List system services and their status."""
    if OS == OSType.LINUX:
        cmd = ["systemctl", "list-units", "--type=service", "--no-pager", "--plain"]
    elif OS == OSType.WINDOWS:
        ps_cmd = "Get-Service"
        if filter:
            ps_cmd += f" | Where-Object Name -like '*{filter}*'"
        ps_cmd += " | Format-Table Name, Status, DisplayName -AutoSize"
        cmd = ["powershell", "-Command", ps_cmd]
    else:
        return {"error": f"Unsupported OS: {OS.value}"}

    proc = await asyncio.create_subprocess_exec(
        *cmd, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE
    )
    stdout, _ = await asyncio.wait_for(proc.communicate(), timeout=15.0)
    output = stdout.decode(errors="replace")

    # Apply filter for Linux (post-filter)
    if OS == OSType.LINUX and filter:
        lines = output.splitlines()
        header = lines[0] if lines else ""
        filtered = [l for l in lines[1:] if filter.lower() in l.lower()]
        output = header + "\n" + "\n".join(filtered)

    return {"services": output, "os": OS.value}


@isolated
async def manage_service(name: str, action: str) -> dict:
    """Start, stop, restart, or check status of a system service."""
    if action not in ("start", "stop", "restart", "status"):
        return {"error": f"Invalid action: {action}. Use: start, stop, restart, status"}

    if OS == OSType.LINUX:
        cmd = ["systemctl", action, name]
    elif OS == OSType.WINDOWS:
        actions_map = {"start": "Start", "stop": "Stop", "restart": "Restart", "status": "Get"}
        cmd = ["powershell", "-Command", f"{actions_map[action]}-Service -Name '{name}'"]
    else:
        return {"error": f"Unsupported OS: {OS.value}"}

    proc = await asyncio.create_subprocess_exec(
        *cmd, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE
    )
    stdout, stderr = await asyncio.wait_for(proc.communicate(), timeout=30.0)
    return {
        "service": name,
        "action": action,
        "success": proc.returncode == 0,
        "output": stdout.decode(errors="replace"),
        "error": stderr.decode(errors="replace") if proc.returncode != 0 else None,
    }


# ── Register ──────────────────────────────────────────────────────

tool_registry.register("list_services", list_services,
    description="List system services and their status.",
    parameters_schema={
        "filter": {"type": "string", "description": "Filter by name (optional)", "default": ""},
    })

tool_registry.register("manage_service", manage_service,
    description="Start, stop, restart, or check status of a system service. Requires approval.",
    parameters_schema={
        "name": {"type": "string", "description": "Service name"},
        "action": {"type": "string", "description": "start | stop | restart | status"},
    })
