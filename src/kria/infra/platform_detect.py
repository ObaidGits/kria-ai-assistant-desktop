"""
Platform Detection Utility
===========================
Cross-platform tools branch by OS. Centralize detection here.
"""
import platform
import shutil
from enum import Enum


class OSType(Enum):
    LINUX = "linux"
    WINDOWS = "windows"
    MACOS = "macos"
    UNKNOWN = "unknown"


def get_os() -> OSType:
    system = platform.system().lower()
    if system == "linux":
        return OSType.LINUX
    elif system == "windows":
        return OSType.WINDOWS
    elif system == "darwin":
        return OSType.MACOS
    return OSType.UNKNOWN


def has_command(cmd: str) -> bool:
    """Check if a command-line tool is available on PATH."""
    return shutil.which(cmd) is not None


def get_package_manager() -> str | None:
    """Detect the system package manager."""
    os_type = get_os()
    if os_type == OSType.LINUX:
        for pm in ["apt", "dnf", "pacman", "zypper", "apk"]:
            if has_command(pm):
                return pm
    elif os_type == OSType.WINDOWS:
        if has_command("winget"):
            return "winget"
        if has_command("choco"):
            return "choco"
    elif os_type == OSType.MACOS:
        if has_command("brew"):
            return "brew"
    return None


OS = get_os()
PACKAGE_MANAGER = get_package_manager()
