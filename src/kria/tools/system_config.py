"""
System Configuration Tools (YELLOW/RED tier — Windows-focused)
==============================================================
set_volume        → YELLOW
set_brightness    → YELLOW
toggle_wifi       → YELLOW
set_power_plan    → YELLOW
write_registry    → RED
change_environment_variable → RED
"""
import logging
import platform
import subprocess

from kria.infra.isolation import ToolResult, isolated
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.tools.system_config")

_OS = platform.system()


@isolated
async def set_volume(level: int) -> dict:
    """Set system volume (0-100)."""
    level = max(0, min(100, level))
    if _OS == "Windows":
        script = f"(New-Object -ComObject WScript.Shell).SendKeys([char]173); $v={level}; $wm = New-Object -ComObject WScript.Shell"
        # Use nircmd or PowerShell audio API
        result = subprocess.run(
            [
                "powershell", "-Command",
                f"$volume = {level} / 100;"
                "$obj = New-Object -ComObject WScript.Shell;"
                "[audio]::Volume = $volume 2>$null; "
                f"Write-Output 'Volume set to {level}'"
            ],
            capture_output=True, text=True
        )
        return {"volume": level, "success": result.returncode == 0, "note": result.stdout.strip()}
    elif _OS == "Linux":
        result = subprocess.run(
            ["amixer", "-D", "pulse", "sset", "Master", f"{level}%"],
            capture_output=True, text=True
        )
        return {"volume": level, "success": result.returncode == 0}
    return {"error": f"set_volume not implemented for {_OS}"}


@isolated
async def set_brightness(level: int) -> dict:
    """Set screen brightness (0-100). Windows-only via PowerShell WMI."""
    level = max(0, min(100, level))
    if _OS != "Windows":
        return {"error": "set_brightness is Windows-only via WMI"}
    result = subprocess.run(
        [
            "powershell", "-Command",
            f"(Get-WmiObject -Namespace root/WMI -Class WmiMonitorBrightnessMethods).WmiSetBrightness(1, {level})"
        ],
        capture_output=True, text=True
    )
    return {"brightness": level, "success": result.returncode == 0}


@isolated
async def toggle_wifi(enabled: bool) -> dict:
    """Enable or disable Wi-Fi adapter."""
    state = "enable" if enabled else "disable"
    if _OS == "Windows":
        result = subprocess.run(
            ["netsh", "interface", "set", "interface", "Wi-Fi", state],
            capture_output=True, text=True
        )
        return {"wifi_enabled": enabled, "success": result.returncode == 0, "output": result.stdout.strip()}
    elif _OS == "Linux":
        cmd = "nmcli radio wifi on" if enabled else "nmcli radio wifi off"
        result = subprocess.run(cmd.split(), capture_output=True, text=True)
        return {"wifi_enabled": enabled, "success": result.returncode == 0}
    return {"error": f"toggle_wifi not implemented for {_OS}"}


@isolated
async def set_power_plan(plan: str) -> dict:
    """
    Set Windows power plan.
    plan: 'balanced' | 'high_performance' | 'power_saver'
    """
    PLANS = {
        "balanced": "381b4222-f694-41f0-9685-ff5bb260df2e",
        "high_performance": "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c",
        "power_saver": "a1841308-3541-4fab-bc81-f71556f20b4a",
    }
    if _OS != "Windows":
        return {"error": "set_power_plan is Windows-only"}
    guid = PLANS.get(plan.lower())
    if not guid:
        return {"error": f"Unknown plan: {plan!r}. Use: {list(PLANS.keys())}"}
    result = subprocess.run(
        ["powercfg", "/setactive", guid],
        capture_output=True, text=True
    )
    return {"plan": plan, "guid": guid, "success": result.returncode == 0}


@isolated
async def write_registry(key: str, value_name: str, value_data: str, value_type: str = "REG_SZ") -> dict:
    """
    Write a Windows registry value. RED tier — requires HITL approval.
    key example: 'HKCU\\\\Software\\\\MyApp'
    """
    if _OS != "Windows":
        return {"error": "write_registry is Windows-only"}
    result = subprocess.run(
        ["reg", "add", key, "/v", value_name, "/t", value_type, "/d", value_data, "/f"],
        capture_output=True, text=True
    )
    return {"key": key, "value": value_name, "success": result.returncode == 0, "output": result.stdout.strip()}


@isolated
async def change_environment_variable(name: str, value: str, scope: str = "User") -> dict:
    """
    Set an environment variable. RED tier — requires HITL approval.
    scope: 'User' | 'Machine' (Machine requires admin)
    """
    if _OS != "Windows":
        return {"error": "change_environment_variable (via setx) is Windows-only"}
    if scope.lower() == "machine":
        cmd = ["setx", name, value, "/M"]
    else:
        cmd = ["setx", name, value]
    result = subprocess.run(cmd, capture_output=True, text=True)
    return {"name": name, "scope": scope, "success": result.returncode == 0, "output": result.stdout.strip()}


# ── Register ─────────────────────────────────────────────────────

tool_registry.register("set_volume", set_volume,
    description="Set system audio volume (0–100).",
    parameters_schema={"level": {"type": "integer"}})
tool_registry.register("set_brightness", set_brightness,
    description="Set screen brightness (0–100). Windows WMI only.",
    parameters_schema={"level": {"type": "integer"}})
tool_registry.register("toggle_wifi", toggle_wifi,
    description="Enable or disable the Wi-Fi adapter.",
    parameters_schema={"enabled": {"type": "boolean"}})
tool_registry.register("set_power_plan", set_power_plan,
    description="Set Windows power plan: balanced, high_performance, or power_saver.",
    parameters_schema={"plan": {"type": "string"}})
tool_registry.register("write_registry", write_registry,
    description="Write a Windows registry value. Requires approval (RED).",
    parameters_schema={"key": {"type": "string"}, "value_name": {"type": "string"}, "value_data": {"type": "string"}, "value_type": {"type": "string"}})
tool_registry.register("change_environment_variable", change_environment_variable,
    description="Set a Windows environment variable. Requires approval (RED).",
    parameters_schema={"name": {"type": "string"}, "value": {"type": "string"}, "scope": {"type": "string"}})
