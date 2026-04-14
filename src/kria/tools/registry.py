"""
Tool Registry
=============
Central catalogue of all tools callable by the ReAct loop.

Each tool must be an async function decorated with @isolated (from
kria.infra.isolation) and registered via ``tool_registry.register()``.
The loop calls ``execute(name, params)`` which:
  1. Looks up the function
  2. Filters params to the function's actual signature (safe injection)
  3. Calls the function, returns ToolResult

Auto-discovery:
  Importing this module side-loads every tools/sub-module; each sub-module
  calls tool_registry.register() for its own functions.  Registration order
  does not matter.
"""
import asyncio
import importlib
import inspect
import logging
import pkgutil
from typing import Any, Callable, Optional

from kria.infra.isolation import ToolResult, isolated

logger = logging.getLogger("kria.tools.registry")

_TOOL_MODULES = [
    "kria.tools.system_info",
    "kria.tools.app_control",
    "kria.tools.file_ops",
    "kria.tools.system_config",
    "kria.tools.process_mgmt",
    "kria.tools.code_executor",
    "kria.tools.web_tools",
    # Phase 5 — Internet & Connectivity
    "kria.tools.download_mgr",
    "kria.tools.rss_reader",
    "kria.tools.network_mgmt",
    "kria.tools.api_tools",
    # Phase 6 — Document Intelligence
    "kria.tools.document_parser",
    "kria.tools.document_convert",
    "kria.tools.file_organizer",
    "kria.tools.file_watcher",
    # Phase 7 — OS-Level Management
    "kria.tools.service_mgmt",
    "kria.tools.disk_mgmt",
    "kria.tools.power_mgmt",
    "kria.tools.env_mgmt",
    "kria.tools.task_scheduler",
    # Phase 8 — Application Lifecycle
    "kria.tools.app_lifecycle",
    # Phase 9 — Notification & Communication
    "kria.tools.notification",
    "kria.tools.email_composer",
    "kria.tools.clipboard_mgr",
    "kria.tools.reminder",
    # Phase 10 — Knowledge Base
    "kria.tools.knowledge_tools",
    "kria.tools.doc_ingest",
    "kria.tools.snippet_lib",
    # Phase 10 — User Preferences
    "kria.memory.user_prefs",
    # Phase 12 — Plugin Manager
    "kria.plugins.manager",
    # Interaction (ask_user)
    "kria.tools.interaction_tools",
]


class ToolRegistry:
    def __init__(self) -> None:
        self._tools: dict[str, dict] = {}
        self._loaded = False

    # ── Registration ──────────────────────────────────────────────

    def register(
        self,
        name: str,
        func: Callable,
        description: str = "",
        parameters_schema: Optional[dict] = None,
    ) -> None:
        if name in self._tools:
            logger.warning("Tool %r already registered — overwriting", name)
        self._tools[name] = {
            "func": func,
            "description": description,
            "parameters_schema": parameters_schema or {},
        }
        logger.debug("Tool registered: %s", name)

    # ── Discovery ─────────────────────────────────────────────────

    def load_all(self) -> None:
        """Import every tool sub-module, triggering their register() calls."""
        if self._loaded:
            return
        for module_path in _TOOL_MODULES:
            try:
                importlib.import_module(module_path)
                logger.debug("Loaded tool module: %s", module_path)
            except Exception as exc:
                logger.error("Failed to load tool module %s: %s", module_path, exc)
        self._loaded = True
        logger.info("Tool registry loaded: %d tools available", len(self._tools))

    # ── Execution ─────────────────────────────────────────────────

    async def execute(self, name: str, params: dict) -> ToolResult:
        tool = self._tools.get(name)
        if tool is None:
            return ToolResult(
                success=False,
                data=None,
                error=f"Unknown tool: '{name}'. Available: {self.list_names()}",
            )

        func = tool["func"]
        # Filter params to only what the function actually accepts
        sig = inspect.signature(func)
        accepted = {
            k: v for k, v in params.items()
            if k in sig.parameters
        }

        try:
            result = await func(**accepted)
            # If the function already returns ToolResult (wrapped by @isolated), pass through
            if isinstance(result, ToolResult):
                return result
            return ToolResult(success=True, data=result)
        except Exception as exc:
            logger.error("Tool execution error for %s: %s", name, exc)
            return ToolResult(success=False, data=None, error=str(exc))

    # ── Introspection ─────────────────────────────────────────────

    def list_names(self) -> list[str]:
        return sorted(self._tools.keys())

    def describe_all(self) -> list[dict]:
        return [
            {
                "name": name,
                "description": spec["description"],
                "parameters": spec["parameters_schema"],
            }
            for name, spec in self._tools.items()
        ]

    def describe(self, name: str) -> Optional[dict]:
        spec = self._tools.get(name)
        if spec is None:
            return None
        return {
            "name": name,
            "description": spec["description"],
            "parameters": spec["parameters_schema"],
        }

    _LITE_TOOLS = {
        "open_application", "close_application", "get_cpu_usage",
        "get_memory_info", "get_disk_space", "get_time",
        "get_battery_status", "web_search", "get_weather",
        "execute_shell", "list_directory", "read_file",
        "search_files", "write_file",
        "send_notification", "get_clipboard", "set_clipboard",
        "remember_fact", "recall_fact", "search_knowledge",
        "lock_screen", "get_news", "get_public_ip",
        "schedule_reminder", "download_file",
        "ping_host", "fetch_webpage", "deep_search",
        "rss_feed_read",
        "ask_user",
    }

    def _build_schema(self, name: str, spec: dict) -> dict:
        raw = spec["parameters_schema"] or {}
        if raw.get("type") == "object" or not raw:
            parameters = raw or {"type": "object", "properties": {}}
        else:
            parameters = {"type": "object", "properties": raw}
        return {
            "type": "function",
            "function": {
                "name": name,
                "description": spec["description"],
                "parameters": parameters,
            },
        }

    def get_openai_schemas(self) -> list[dict]:
        """Return tool schemas in OpenAI function-calling format."""
        return [self._build_schema(n, s) for n, s in self._tools.items()]

    def get_openai_schemas_filtered(self, names: list[str]) -> list[dict]:
        """Return schemas for only the specified tool names."""
        return [
            self._build_schema(n, s)
            for n, s in self._tools.items()
            if n in names
        ]

    def get_openai_schemas_lite(self) -> list[dict]:
        """Return only the most common tool schemas (smaller payload for DIRECT_TOOL)."""
        return [
            self._build_schema(n, s)
            for n, s in self._tools.items()
            if n in self._LITE_TOOLS
        ]

    def __len__(self) -> int:
        return len(self._tools)


tool_registry = ToolRegistry()
