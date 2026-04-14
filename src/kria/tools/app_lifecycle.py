"""
Application Lifecycle Management (GREEN search / RED install/uninstall)
========================================================================
Install, uninstall, update, and query packages via system package managers.
"""
import asyncio
import logging
import os

from kria.infra.isolation import ToolResult, isolated
from kria.infra.platform_detect import OS, OSType, PACKAGE_MANAGER
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.tools.app_lifecycle")


@isolated
async def search_package(query: str) -> dict:
    """Search for an installable package in system repositories."""
    if not PACKAGE_MANAGER:
        return {"error": "No supported package manager found"}

    if PACKAGE_MANAGER == "apt":
        cmd = ["apt", "search", query]
    elif PACKAGE_MANAGER == "dnf":
        cmd = ["dnf", "search", query]
    elif PACKAGE_MANAGER == "pacman":
        cmd = ["pacman", "-Ss", query]
    elif PACKAGE_MANAGER == "winget":
        cmd = ["winget", "search", query]
    elif PACKAGE_MANAGER == "brew":
        cmd = ["brew", "search", query]
    else:
        return {"error": f"Unsupported package manager: {PACKAGE_MANAGER}"}

    proc = await asyncio.create_subprocess_exec(
        *cmd, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE
    )
    stdout, stderr = await asyncio.wait_for(proc.communicate(), timeout=30.0)
    return {
        "query": query,
        "package_manager": PACKAGE_MANAGER,
        "results": stdout.decode(errors="replace")[:5000],
    }


@isolated
async def install_application(package_name: str) -> dict:
    """Install an application using the system package manager. Requires approval."""
    if not PACKAGE_MANAGER:
        return {"error": "No supported package manager found"}

    if PACKAGE_MANAGER == "apt":
        cmd = ["sudo", "apt", "install", "-y", package_name]
    elif PACKAGE_MANAGER == "dnf":
        cmd = ["sudo", "dnf", "install", "-y", package_name]
    elif PACKAGE_MANAGER == "pacman":
        cmd = ["sudo", "pacman", "-S", "--noconfirm", package_name]
    elif PACKAGE_MANAGER == "winget":
        cmd = ["winget", "install", "--accept-source-agreements", "--accept-package-agreements", package_name]
    elif PACKAGE_MANAGER == "brew":
        cmd = ["brew", "install", package_name]
    else:
        return {"error": f"Unsupported package manager: {PACKAGE_MANAGER}"}

    proc = await asyncio.create_subprocess_exec(
        *cmd, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE
    )
    stdout, stderr = await asyncio.wait_for(proc.communicate(), timeout=300.0)
    return {
        "package": package_name,
        "success": proc.returncode == 0,
        "output": stdout.decode(errors="replace")[:3000],
        "error": stderr.decode(errors="replace")[:1000] if proc.returncode != 0 else None,
    }


@isolated
async def uninstall_application(package_name: str) -> dict:
    """Uninstall an application. Requires approval."""
    if not PACKAGE_MANAGER:
        return {"error": "No supported package manager found"}

    if PACKAGE_MANAGER == "apt":
        cmd = ["sudo", "apt", "remove", "-y", package_name]
    elif PACKAGE_MANAGER == "dnf":
        cmd = ["sudo", "dnf", "remove", "-y", package_name]
    elif PACKAGE_MANAGER == "pacman":
        cmd = ["sudo", "pacman", "-R", "--noconfirm", package_name]
    elif PACKAGE_MANAGER == "winget":
        cmd = ["winget", "uninstall", package_name]
    elif PACKAGE_MANAGER == "brew":
        cmd = ["brew", "uninstall", package_name]
    else:
        return {"error": f"Unsupported package manager: {PACKAGE_MANAGER}"}

    proc = await asyncio.create_subprocess_exec(
        *cmd, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE
    )
    stdout, stderr = await asyncio.wait_for(proc.communicate(), timeout=120.0)
    return {
        "package": package_name,
        "success": proc.returncode == 0,
        "output": stdout.decode(errors="replace")[:3000],
    }


@isolated
async def check_updates_available() -> dict:
    """List available package updates."""
    if not PACKAGE_MANAGER:
        return {"error": "No supported package manager found"}

    if PACKAGE_MANAGER == "apt":
        # Update index first
        await asyncio.create_subprocess_exec(
            "sudo", "apt", "update",
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE,
        )
        cmd = ["apt", "list", "--upgradable"]
    elif PACKAGE_MANAGER == "dnf":
        cmd = ["dnf", "check-update"]
    elif PACKAGE_MANAGER == "pacman":
        cmd = ["pacman", "-Qu"]
    elif PACKAGE_MANAGER == "winget":
        cmd = ["winget", "upgrade"]
    elif PACKAGE_MANAGER == "brew":
        cmd = ["brew", "outdated"]
    else:
        return {"error": f"Unsupported package manager: {PACKAGE_MANAGER}"}

    proc = await asyncio.create_subprocess_exec(
        *cmd, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE
    )
    stdout, _ = await asyncio.wait_for(proc.communicate(), timeout=60.0)
    return {
        "package_manager": PACKAGE_MANAGER,
        "updates": stdout.decode(errors="replace")[:5000],
    }


# ── Register ──────────────────────────────────────────────────────

tool_registry.register("search_package", search_package,
    description="Search for an installable package in system repositories.",
    parameters_schema={
        "query": {"type": "string", "description": "Package name to search"},
    })

tool_registry.register("install_application", install_application,
    description="Install an application using system package manager. Requires approval.",
    parameters_schema={
        "package_name": {"type": "string", "description": "Package to install"},
    })

tool_registry.register("uninstall_application", uninstall_application,
    description="Uninstall an application. Requires approval.",
    parameters_schema={
        "package_name": {"type": "string", "description": "Package to uninstall"},
    })

tool_registry.register("check_updates_available", check_updates_available,
    description="List available package updates.")


# ── Snap support ──────────────────────────────────────────────────

async def _run_host_cmd(cmd: list[str], timeout: float = 120.0) -> tuple[int, str, str]:
    """Run a command, preferring nsenter to host namespace if available."""
    import shutil
    # If running inside Docker with pid:host, use nsenter for host commands
    if shutil.which("nsenter") and os.path.exists("/proc/1/ns/mnt"):
        cmd = ["nsenter", "--target", "1", "--mount", "--uts", "--ipc", "--net", "--pid", "--"] + cmd
    proc = await asyncio.create_subprocess_exec(
        *cmd, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE
    )
    stdout, stderr = await asyncio.wait_for(proc.communicate(), timeout=timeout)
    return proc.returncode, stdout.decode(errors="replace"), stderr.decode(errors="replace")


@isolated
async def snap_install(package_name: str) -> dict:
    """Install a snap package. Requires approval."""
    rc, out, err = await _run_host_cmd(["snap", "install", package_name])
    return {
        "package": package_name,
        "method": "snap",
        "success": rc == 0,
        "output": out[:3000],
        "error": err[:1000] if rc != 0 else None,
    }


@isolated
async def snap_remove(package_name: str) -> dict:
    """Remove a snap package. Requires approval."""
    rc, out, err = await _run_host_cmd(["snap", "remove", package_name])
    return {
        "package": package_name,
        "method": "snap",
        "success": rc == 0,
        "output": out[:3000],
        "error": err[:1000] if rc != 0 else None,
    }


@isolated
async def snap_list() -> dict:
    """List installed snap packages."""
    rc, out, err = await _run_host_cmd(["snap", "list"], timeout=30.0)
    return {"method": "snap", "success": rc == 0, "packages": out[:5000]}


@isolated
async def snap_search(query: str) -> dict:
    """Search for snap packages."""
    rc, out, err = await _run_host_cmd(["snap", "find", query], timeout=30.0)
    return {"query": query, "method": "snap", "success": rc == 0, "results": out[:5000]}


# ── Flatpak support ───────────────────────────────────────────────

@isolated
async def flatpak_install(package_name: str) -> dict:
    """Install a flatpak application. Requires approval."""
    rc, out, err = await _run_host_cmd(["flatpak", "install", "-y", package_name])
    return {
        "package": package_name,
        "method": "flatpak",
        "success": rc == 0,
        "output": out[:3000],
        "error": err[:1000] if rc != 0 else None,
    }


@isolated
async def flatpak_remove(package_name: str) -> dict:
    """Remove a flatpak application. Requires approval."""
    rc, out, err = await _run_host_cmd(["flatpak", "uninstall", "-y", package_name])
    return {
        "package": package_name,
        "method": "flatpak",
        "success": rc == 0,
        "output": out[:3000],
        "error": err[:1000] if rc != 0 else None,
    }


@isolated
async def flatpak_list() -> dict:
    """List installed flatpak applications."""
    rc, out, err = await _run_host_cmd(["flatpak", "list", "--app"], timeout=30.0)
    return {"method": "flatpak", "success": rc == 0, "applications": out[:5000]}


@isolated
async def flatpak_search(query: str) -> dict:
    """Search for flatpak applications."""
    rc, out, err = await _run_host_cmd(["flatpak", "search", query], timeout=30.0)
    return {"query": query, "method": "flatpak", "success": rc == 0, "results": out[:5000]}


# ── Register snap/flatpak tools ───────────────────────────────────

tool_registry.register("snap_install", snap_install,
    description="Install a snap package on the host system. Requires approval.",
    parameters_schema={
        "package_name": {"type": "string", "description": "Snap package name to install"},
    })

tool_registry.register("snap_remove", snap_remove,
    description="Remove a snap package from the host system. Requires approval.",
    parameters_schema={
        "package_name": {"type": "string", "description": "Snap package name to remove"},
    })

tool_registry.register("snap_list", snap_list,
    description="List all installed snap packages on the host system.")

tool_registry.register("snap_search", snap_search,
    description="Search for available snap packages.",
    parameters_schema={
        "query": {"type": "string", "description": "Search query for snap packages"},
    })

tool_registry.register("flatpak_install", flatpak_install,
    description="Install a flatpak application on the host system. Requires approval.",
    parameters_schema={
        "package_name": {"type": "string", "description": "Flatpak app ID or name to install"},
    })

tool_registry.register("flatpak_remove", flatpak_remove,
    description="Remove a flatpak application from the host system. Requires approval.",
    parameters_schema={
        "package_name": {"type": "string", "description": "Flatpak app ID or name to remove"},
    })

tool_registry.register("flatpak_list", flatpak_list,
    description="List all installed flatpak applications on the host system.")

tool_registry.register("flatpak_search", flatpak_search,
    description="Search for available flatpak applications.",
    parameters_schema={
        "query": {"type": "string", "description": "Search query for flatpak apps"},
    })
