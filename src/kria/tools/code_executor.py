"""
Code Execution Tools (RED tier)
================================
ALL code execution requires HITL approval.  The policy engine classifies
execute_powershell / execute_python / execute_shell as RED.

Security measures:
  - 30-second timeout on all subprocesses
  - Output truncated to 8 KB
  - No shell=True for execute_python (uses explicit interpreter path)
  - Blocked commands list applied at invocation time (belt-and-suspenders
    beyond the policy engine's black-list check)
"""
import logging
import platform
import subprocess
import sys
import textwrap

from kria.infra.isolation import ToolResult, isolated
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.tools.code_executor")

_OS = platform.system()
_MAX_OUTPUT = 8 * 1024  # 8 KB
_TIMEOUT = 30  # seconds

_BLOCKED_PATTERNS_PS = [
    "format-volume", "clear-disk", "remove-item.*-recurse.*c:\\windows",
    "invoke-expression", "iex ", "downloadstring", "webclient",
    "disable.*defender", "set-mppreference",
]

_BLOCKED_PATTERNS_SH = [
    "rm -rf /", "mkfs", "dd if=/dev/zero", "> /dev/sda",
    ":(){ :|:& };:", "curl.*|.*bash", "wget.*|.*sh",
]


def _check_blocked(code: str, patterns: list[str]) -> str | None:
    import re
    code_lower = code.lower()
    for pat in patterns:
        if re.search(pat, code_lower):
            return pat
    return None


@isolated
async def execute_powershell(code: str) -> dict:
    """Execute PowerShell code. RED tier — requires HITL approval."""
    blocked = _check_blocked(code, _BLOCKED_PATTERNS_PS)
    if blocked:
        return {"success": False, "error": f"Blocked pattern detected: {blocked!r}", "output": ""}
    try:
        result = subprocess.run(
            ["powershell", "-NonInteractive", "-NoProfile", "-Command", code],
            capture_output=True, text=True, timeout=_TIMEOUT
        )
        output = (result.stdout + result.stderr)[:_MAX_OUTPUT]
        return {
            "success": result.returncode == 0,
            "exit_code": result.returncode,
            "output": output,
            "truncated": len(result.stdout + result.stderr) > _MAX_OUTPUT,
        }
    except subprocess.TimeoutExpired:
        return {"success": False, "error": f"Execution timed out after {_TIMEOUT}s", "output": ""}


@isolated
async def execute_python(code: str) -> dict:
    """Execute Python code in a subprocess. RED tier — requires HITL approval."""
    blocked = _check_blocked(code, [
        r"shutil\.rmtree.*os\.sep.*win",
        r"os\.system.*format",
        r"subprocess.*format",
    ])
    if blocked:
        return {"success": False, "error": f"Blocked pattern detected: {blocked!r}", "output": ""}
    try:
        result = subprocess.run(
            [sys.executable, "-c", code],
            capture_output=True, text=True, timeout=_TIMEOUT
        )
        output = (result.stdout + result.stderr)[:_MAX_OUTPUT]
        return {
            "success": result.returncode == 0,
            "exit_code": result.returncode,
            "output": output,
            "truncated": len(result.stdout + result.stderr) > _MAX_OUTPUT,
        }
    except subprocess.TimeoutExpired:
        return {"success": False, "error": f"Execution timed out after {_TIMEOUT}s", "output": ""}


@isolated
async def execute_shell(command: str) -> dict:
    """Execute a shell command. RED tier — requires HITL approval."""
    blocked = _check_blocked(command, _BLOCKED_PATTERNS_SH)
    if blocked:
        return {"success": False, "error": f"Blocked shell pattern detected: {blocked!r}", "output": ""}
    try:
        result = subprocess.run(
            command,
            shell=True,   # Needed for raw shell commands; blocked list mitigates risk
            capture_output=True,
            text=True,
            timeout=_TIMEOUT,
        )
        output = (result.stdout + result.stderr)[:_MAX_OUTPUT]
        return {
            "success": result.returncode == 0,
            "exit_code": result.returncode,
            "output": output,
            "truncated": len(result.stdout + result.stderr) > _MAX_OUTPUT,
        }
    except subprocess.TimeoutExpired:
        return {"success": False, "error": f"Execution timed out after {_TIMEOUT}s", "output": ""}


# ── Register ─────────────────────────────────────────────────────

tool_registry.register("execute_powershell", execute_powershell,
    description="Run PowerShell code. Requires HITL approval (RED).",
    parameters_schema={"code": {"type": "string"}})
tool_registry.register("execute_python", execute_python,
    description="Run Python code in a subprocess. Requires HITL approval (RED).",
    parameters_schema={"code": {"type": "string"}})
tool_registry.register("execute_shell", execute_shell,
    description="Run a shell command. Requires HITL approval (RED).",
    parameters_schema={"command": {"type": "string"}})
