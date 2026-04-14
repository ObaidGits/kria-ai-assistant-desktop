"""
Desktop Notification Tools (GREEN tier)
========================================
Cross-platform desktop notifications.
"""
import asyncio
import logging

from kria.infra.isolation import ToolResult, isolated
from kria.infra.platform_detect import OS, OSType
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.tools.notification")


@isolated
async def send_notification(title: str, body: str, urgency: str = "normal") -> dict:
    """Send a desktop notification."""
    try:
        if OS == OSType.LINUX:
            urgency_map = {"low": "low", "normal": "normal", "critical": "critical"}
            await asyncio.create_subprocess_exec(
                "notify-send", "-u", urgency_map.get(urgency, "normal"), title, body
            )
            return {"sent": True, "title": title}
        else:
            try:
                from plyer import notification as plyer_notify
                plyer_notify.notify(title=title, message=body, timeout=10)
                return {"sent": True, "title": title}
            except ImportError:
                return {"sent": False, "error": "plyer not installed for notifications"}
    except Exception as e:
        logger.error("Notification failed: %s", e)
        return {"sent": False, "error": str(e)}


# ── Register ──────────────────────────────────────────────────────

tool_registry.register("send_notification", send_notification,
    description="Send a desktop notification.",
    parameters_schema={
        "title": {"type": "string", "description": "Notification title"},
        "body": {"type": "string", "description": "Notification body"},
        "urgency": {"type": "string", "description": "low | normal | critical", "default": "normal"},
    })
