"""
Policy Engine
=============
Implements the 4-tier risk classification from SAFETY_SPECIFICATION.md.

Evaluation order (first match wins):
  1. Emergency mode  → only GREEN allowed
  2. BLACK patterns (hardcoded regex) → hard deny, cannot be overridden
  3. Protected path check → escalate any file op to RED
  4. Action name lookup  → GREEN / YELLOW / RED table
  5. Unknown action      → default RED (fail-safe)

The engine is purely code-driven — the LLM has NO ability to modify
risk levels or bypass the BLACK list.
"""
import re
import logging
from dataclasses import dataclass
from enum import Enum
from typing import Optional

from kria.infra.config import settings

logger = logging.getLogger("kria.safety.policy")


class RiskLevel(Enum):
    GREEN = "GREEN"
    YELLOW = "YELLOW"
    RED = "RED"
    BLACK = "BLACK"


@dataclass
class PolicyDecision:
    risk_level: RiskLevel
    allowed: bool
    reason: str
    requires_approval: bool = False
    requires_rollback: bool = False


# ── Classification tables (mirrors SAFETY_SPECIFICATION.md) ──────

GREEN_ACTIONS = frozenset({
    # ── Phase 0-4 originals ──
    "open_application", "list_running_apps", "focus_window",
    "get_cpu_usage", "get_memory_info", "get_disk_space",
    "get_network_status", "get_battery_status", "get_clipboard",
    "read_file", "search_files", "web_search", "fetch_webpage",
    "get_weather", "get_time", "screenshot", "list_directory",
    "deep_search",
    # ── Phase 5: Internet ──
    "rss_feed_read", "get_news", "ping_host", "dns_lookup",
    "get_public_ip", "check_url_status", "http_request", "get_stock_price",
    # ── Phase 6: Document (read-only) ──
    "parse_pdf", "parse_docx", "parse_xlsx", "parse_csv",
    "summarize_document", "list_watched_directories",
    # ── Phase 7: OS (read-only) ──
    "list_services", "find_large_files", "find_duplicate_files",
    "calculate_dir_size", "get_environment_variable",
    "list_environment_variables", "list_scheduled_tasks",
    "search_package", "check_updates_available",
    # ── Phase 8b: Snap/Flatpak (read-only) ──
    "snap_list", "snap_search", "flatpak_list", "flatpak_search",
    # ── Phase 9: Communication ──
    "send_notification", "compose_email", "open_email_draft",
    "get_clipboard", "clipboard_history", "schedule_reminder",
    # ── Phase 10: Knowledge (read) ──
    "recall_fact", "list_remembered", "search_knowledge",
    "get_snippet", "list_snippets", "get_preference", "list_preferences",
    # ── Phase 12: Plugins (read) ──
    "list_plugins",
    # ── Interaction (meta-tool, zero risk) ──
    "ask_user",
})

YELLOW_ACTIONS = frozenset({
    # ── Phase 0-4 originals ──
    "close_application", "kill_process", "set_volume", "set_brightness",
    "toggle_wifi", "set_power_plan", "write_file", "set_clipboard",
    "type_text", "install_package", "create_directory", "rename_file",
    # ── Phase 5: Internet (write) ──
    "download_file",
    # ── Phase 6: Document (write) ──
    "convert_document", "organize_files", "watch_directory",
    "unwatch_directory",
    # ── Phase 7: OS (limited write) ──
    "lock_screen", "suspend_system", "create_scheduled_task",
    "cancel_scheduled_task",
    # ── Phase 9: Communication (write) ──
    "set_clipboard",
    # ── Phase 10: Knowledge (write) ──
    "remember_fact", "ingest_document", "save_snippet", "delete_snippet",
    "set_preference",
    # ── Phase 12: Plugins ──
    "load_plugin", "unload_plugin",
})

RED_ACTIONS = frozenset({
    # ── Phase 0-4 originals ──
    "delete_file", "delete_directory", "move_file",
    "write_registry", "modify_service", "change_network_config",
    "execute_powershell", "execute_python", "execute_shell",
    "uninstall_package", "set_process_priority",
    "modify_scheduled_task", "change_environment_variable",
    # ── Phase 7: OS (destructive) ──
    "manage_service", "shutdown_system", "reboot_system",
    # ── Phase 8: App lifecycle (destructive) ──
    "install_application", "uninstall_application",
    # ── Phase 8b: Snap/Flatpak (destructive) ──
    "snap_install", "snap_remove", "flatpak_install", "flatpak_remove",
})

# Hardcoded — these patterns are NEVER permitted regardless of user approval
_BLACK_PATTERNS_RAW = [
    r"format\s+[a-z]:",
    r"diskpart.*clean",
    r"cipher\s+/w",
    r"bcdedit",
    r"bootrec",
    r"sfc\s*/scannow.*delete",
    r"netsh.*firewall.*disable",
    r"Set-MpPreference.*Disable.*True",
    r"Set-MpPreference.*DisableRealtimeMonitoring.*True",
    r"net\s+stop\s+WinDefend",
    r"del\s.*/[sq].*system32",
    r"rmdir.*windows",
    r"rm\s+-rf\s+/",
    r"Remove-Item.*-Recurse.*C:\\\\Windows",
    r"mimikatz",
    r"\blsass\b",
    r"SAM.*dump",
    r"sekurlsa",
]

_PROTECTED_PATH_PATTERNS = [
    r"C:[/\\]Windows[/\\]",
    r"C:[/\\]Program Files[/\\]",
    r"C:[/\\]Program Files \(x86\)[/\\]",
    r"C:[/\\]ProgramData[/\\]",
    r"C:[/\\]Users[/\\][^/\\]+[/\\]AppData[/\\]Local[/\\]Microsoft[/\\]",
    r"C:[/\\]Boot[/\\]",
    r"[/\\]System32[/\\]",
    r"[/\\]SysWOW64[/\\]",
    r"^/etc/",
    r"^/usr/",
    r"^/var/",
    r"^/boot/",
]


class PolicyEngine:
    def __init__(self) -> None:
        self._black = [re.compile(p, re.IGNORECASE) for p in _BLACK_PATTERNS_RAW]
        self._protected = [re.compile(p, re.IGNORECASE) for p in _PROTECTED_PATH_PATTERNS]
        # Emergency mode: only GREEN allowed until manually cleared
        self._emergency: bool = settings.emergency_mode
        # MCP tool risk levels (populated by MCPClientManager after discovery)
        self._mcp_risk_levels: dict[str, RiskLevel] = {}

    # ── MCP risk registration ─────────────────────────────────────

    def register_mcp_risk_levels(self, tool_risk_map: dict[str, str]) -> None:
        """
        Register risk levels for MCP tools.

        Called by MCPClientManager after discovering tools from each server.
        Accepts {"mcp_server_tool": "RED", ...} — string values are
        converted to RiskLevel enum.  Invalid values default to RED.
        """
        level_map = {"GREEN": RiskLevel.GREEN, "YELLOW": RiskLevel.YELLOW, "RED": RiskLevel.RED}
        for name, level_str in tool_risk_map.items():
            self._mcp_risk_levels[name] = level_map.get(level_str.upper(), RiskLevel.RED)
        logger.info("Registered MCP risk levels for %d tools", len(tool_risk_map))

    # ── Public API ────────────────────────────────────────────────

    def set_emergency_mode(self, enabled: bool) -> None:
        self._emergency = enabled
        logger.warning("Emergency mode %s", "ENABLED" if enabled else "disabled")

    @property
    def in_emergency_mode(self) -> bool:
        return self._emergency

    async def evaluate(self, action: str, params: dict) -> PolicyDecision:
        # 1. Emergency mode
        if self._emergency and action not in GREEN_ACTIONS:
            return PolicyDecision(
                risk_level=RiskLevel.RED,
                allowed=False,
                reason="Emergency mode is active — only GREEN actions are permitted.",
            )

        # 2. BLACK list (hardcoded, cannot be bypassed)
        serialized = self._serialize(action, params)
        for pattern in self._black:
            if pattern.search(serialized):
                return PolicyDecision(
                    risk_level=RiskLevel.BLACK,
                    allowed=False,
                    reason=f"Permanently blocked — matches BLACK pattern: {pattern.pattern!r}",
                )

        # 3. Protected path escalation
        if self._targets_protected_path(params):
            return PolicyDecision(
                risk_level=RiskLevel.RED,
                allowed=False,
                reason="Action targets a protected system path.",
                requires_approval=True,
                requires_rollback=True,
            )

        # 3b. MCP tool risk lookup (set by MCPClientManager)
        mcp_level = self._mcp_risk_levels.get(action)
        if mcp_level is not None:
            if mcp_level == RiskLevel.GREEN:
                return PolicyDecision(
                    risk_level=RiskLevel.GREEN,
                    allowed=True,
                    reason=f"MCP tool — configured as GREEN",
                )
            if mcp_level == RiskLevel.YELLOW:
                return PolicyDecision(
                    risk_level=RiskLevel.YELLOW,
                    allowed=True,
                    reason=f"MCP tool — configured as YELLOW",
                )
            return PolicyDecision(
                risk_level=RiskLevel.RED,
                allowed=False,
                reason=f"MCP tool — configured as RED (requires approval)",
                requires_approval=True,
                requires_rollback=False,
            )

        # 4. Action table lookup
        if action in GREEN_ACTIONS:
            return PolicyDecision(
                risk_level=RiskLevel.GREEN,
                allowed=True,
                reason="Auto-execute: GREEN action",
            )
        if action in YELLOW_ACTIONS:
            return PolicyDecision(
                risk_level=RiskLevel.YELLOW,
                allowed=True,
                reason="Execute + notify: YELLOW action",
            )
        if action in RED_ACTIONS:
            return PolicyDecision(
                risk_level=RiskLevel.RED,
                allowed=False,
                reason="Requires explicit approval: RED action",
                requires_approval=True,
                requires_rollback=True,
            )

        # 5. Unknown → fail-safe RED
        return PolicyDecision(
            risk_level=RiskLevel.RED,
            allowed=False,
            reason=f"Unknown action '{action}' — defaulting to RED (fail-safe).",
            requires_approval=True,
        )

    # ── Helpers ───────────────────────────────────────────────────

    def _serialize(self, action: str, params: dict) -> str:
        parts = [action]
        for v in params.values():
            if isinstance(v, str):
                parts.append(v)
            elif isinstance(v, list):
                parts.extend(str(i) for i in v)
        return " ".join(parts)

    def _targets_protected_path(self, params: dict) -> bool:
        for v in params.values():
            targets = [v] if isinstance(v, str) else (v if isinstance(v, list) else [])
            for item in targets:
                if isinstance(item, str):
                    for pattern in self._protected:
                        if pattern.search(item):
                            return True
        return False


policy_engine = PolicyEngine()
