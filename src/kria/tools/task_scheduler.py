"""
Task Scheduler Tools (GREEN read / YELLOW create / RED delete)
===============================================================
Interface to the KriaScheduler for creating and managing scheduled tasks.
"""
import logging
from datetime import datetime, timedelta

from kria.infra.isolation import ToolResult, isolated
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.tools.task_scheduler")


@isolated
async def create_scheduled_task(
    tool_name: str,
    params: dict | None = None,
    minutes_from_now: int = 0,
    cron: str = "",
    name: str = "",
) -> dict:
    """Create a scheduled task (one-shot or cron-based)."""
    from kria.automation.scheduler import scheduler

    params = params or {}

    if cron:
        job_id = scheduler.add_cron(cron, tool_name, params, name=name)
        return {"job_id": job_id, "type": "cron", "cron": cron, "tool": tool_name}
    elif minutes_from_now > 0:
        trigger_time = datetime.now() + timedelta(minutes=minutes_from_now)
        job_id = scheduler.add_one_shot(trigger_time, tool_name, params)
        return {
            "job_id": job_id,
            "type": "one_shot",
            "trigger_at": trigger_time.isoformat(),
            "tool": tool_name,
        }
    else:
        return {"error": "Provide either 'cron' expression or 'minutes_from_now'"}


@isolated
async def list_scheduled_tasks() -> dict:
    """List all scheduled tasks."""
    from kria.automation.scheduler import scheduler
    jobs = scheduler.list_jobs()
    return {"tasks": jobs, "count": len(jobs)}


@isolated
async def cancel_scheduled_task(job_id: str) -> dict:
    """Cancel a scheduled task by ID."""
    from kria.automation.scheduler import scheduler
    removed = scheduler.remove(job_id)
    return {"job_id": job_id, "cancelled": removed}


# ── Register ──────────────────────────────────────────────────────

tool_registry.register("create_scheduled_task", create_scheduled_task,
    description="Create a scheduled task (one-shot or cron-based) to run a tool automatically.",
    parameters_schema={
        "tool_name": {"type": "string", "description": "Name of the tool to run"},
        "params": {"type": "object", "description": "Parameters to pass to the tool", "default": {}},
        "minutes_from_now": {"type": "integer", "description": "Minutes from now (one-shot)", "default": 0},
        "cron": {"type": "string", "description": "Cron expression (5 fields: min hour day month dow)", "default": ""},
        "name": {"type": "string", "description": "Optional task name", "default": ""},
    })

tool_registry.register("list_scheduled_tasks", list_scheduled_tasks,
    description="List all scheduled tasks.")

tool_registry.register("cancel_scheduled_task", cancel_scheduled_task,
    description="Cancel a scheduled task by ID.",
    parameters_schema={
        "job_id": {"type": "string", "description": "Task/job ID to cancel"},
    })
