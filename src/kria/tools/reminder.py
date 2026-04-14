"""
Reminder System (GREEN tier)
=============================
Timed notification reminders using the scheduler.
"""
import logging
from datetime import datetime, timedelta

from kria.infra.isolation import ToolResult, isolated
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.tools.reminder")


@isolated
async def schedule_reminder(message: str, minutes_from_now: int) -> dict:
    """Set a reminder that triggers a desktop notification at a specified time."""
    from kria.automation.scheduler import scheduler

    trigger_time = datetime.now() + timedelta(minutes=minutes_from_now)
    job_id = scheduler.add_one_shot(
        trigger_time=trigger_time,
        tool_name="send_notification",
        params={"title": "Reminder", "body": message},
    )
    return {
        "reminder_set": True,
        "message": message,
        "trigger_at": trigger_time.isoformat(),
        "job_id": job_id,
    }


# ── Register ──────────────────────────────────────────────────────

tool_registry.register("schedule_reminder", schedule_reminder,
    description="Set a reminder that triggers a desktop notification at a specified time.",
    parameters_schema={
        "message": {"type": "string", "description": "Reminder message"},
        "minutes_from_now": {"type": "integer", "description": "Minutes from now to trigger"},
    })
