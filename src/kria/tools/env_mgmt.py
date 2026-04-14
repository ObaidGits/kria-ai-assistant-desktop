"""
Environment Variable Tools (GREEN tier — read-only)
=====================================================
Read and list environment variables with secret redaction.
"""
import logging
import os

from kria.infra.isolation import ToolResult, isolated
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.tools.env_mgmt")

_SENSITIVE_PATTERNS = ["key", "secret", "password", "token", "auth", "credential", "private"]


@isolated
async def get_environment_variable(name: str) -> dict:
    """Read an environment variable value."""
    value = os.environ.get(name)
    # Redact sensitive variables
    if value and any(p in name.lower() for p in _SENSITIVE_PATTERNS):
        value = "****REDACTED****"
    return {"name": name, "value": value, "exists": os.environ.get(name) is not None}


@isolated
async def list_environment_variables(filter: str = "") -> dict:
    """List environment variables (filtered, secrets redacted)."""
    result = {}
    for k, v in sorted(os.environ.items()):
        if filter and filter.lower() not in k.lower():
            continue
        if any(p in k.lower() for p in _SENSITIVE_PATTERNS):
            result[k] = "****REDACTED****"
        else:
            result[k] = v
    return {"variables": result, "count": len(result)}


# ── Register ──────────────────────────────────────────────────────

tool_registry.register("get_environment_variable", get_environment_variable,
    description="Read an environment variable value (secrets redacted).",
    parameters_schema={
        "name": {"type": "string", "description": "Variable name"},
    })

tool_registry.register("list_environment_variables", list_environment_variables,
    description="List environment variables (filtered, secrets redacted).",
    parameters_schema={
        "filter": {"type": "string", "default": ""},
    })
