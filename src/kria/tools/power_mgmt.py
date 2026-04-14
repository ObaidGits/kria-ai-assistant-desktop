"""
Power Management Tools (GREEN–RED tier)
========================================
Lock screen, shutdown, reboot.
"""
import asyncio
import logging

from kria.infra.isolation import ToolResult, isolated
from kria.infra.platform_detect import OS, OSType
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.tools.power_mgmt")


@isolated
async def lock_screen() -> str:
    """Lock the display/screen."""
    if OS == OSType.LINUX:
        await asyncio.create_subprocess_exec("loginctl", "lock-session")
    elif OS == OSType.WINDOWS:
        await asyncio.create_subprocess_exec(
            "powershell", "-Command", "rundll32.exe user32.dll,LockWorkStation"
        )
    elif OS == OSType.MACOS:
        await asyncio.create_subprocess_exec(
            "osascript", "-e", 'tell application "System Events" to sleep'
        )
    else:
        return "Unsupported OS for lock_screen"
    return "Screen locked"


@isolated
async def shutdown_system(delay_minutes: int = 0) -> str:
    """Shut down the computer. Requires approval."""
    if OS == OSType.LINUX:
        cmd = ["shutdown", "-h", f"+{delay_minutes}" if delay_minutes else "now"]
    elif OS == OSType.WINDOWS:
        seconds = delay_minutes * 60
        cmd = ["shutdown", "/s", "/t", str(seconds)]
    else:
        return "Unsupported OS for shutdown"
    await asyncio.create_subprocess_exec(*cmd)
    return f"Shutdown scheduled in {delay_minutes} minutes"


@isolated
async def reboot_system() -> str:
    """Reboot the computer. Requires approval."""
    if OS == OSType.LINUX:
        cmd = ["reboot"]
    elif OS == OSType.WINDOWS:
        cmd = ["shutdown", "/r", "/t", "0"]
    else:
        return "Unsupported OS for reboot"
    await asyncio.create_subprocess_exec(*cmd)
    return "Rebooting..."


@isolated
async def suspend_system() -> str:
    """Suspend/sleep the computer."""
    if OS == OSType.LINUX:
        await asyncio.create_subprocess_exec("systemctl", "suspend")
    elif OS == OSType.WINDOWS:
        await asyncio.create_subprocess_exec(
            "powershell", "-Command",
            "Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.Application]::SetSuspendState('Suspend', $false, $false)"
        )
    else:
        return "Unsupported OS for suspend"
    return "System suspended"


# ── Register ──────────────────────────────────────────────────────

tool_registry.register("lock_screen", lock_screen,
    description="Lock the display/screen.")

tool_registry.register("shutdown_system", shutdown_system,
    description="Shut down the computer. Requires approval.",
    parameters_schema={
        "delay_minutes": {"type": "integer", "default": 0},
    })

tool_registry.register("reboot_system", reboot_system,
    description="Reboot the computer. Requires approval.")

tool_registry.register("suspend_system", suspend_system,
    description="Suspend/sleep the computer. Requires approval.")
