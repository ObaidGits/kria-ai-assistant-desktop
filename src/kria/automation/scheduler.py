"""
Scheduler
=========
APScheduler wrapper for one-shot and cron-based job scheduling.
"""
import uuid
import logging
from datetime import datetime

from kria.infra.config import settings

logger = logging.getLogger("kria.automation.scheduler")


class KriaScheduler:
    def __init__(self):
        self._scheduler = None
        self._jobs: dict[str, dict] = {}

    def start(self):
        try:
            from apscheduler.schedulers.asyncio import AsyncIOScheduler
            self._scheduler = AsyncIOScheduler()
            self._scheduler.start()
            logger.info("Scheduler started")
        except ImportError:
            logger.warning("apscheduler not installed — scheduler disabled")
        except Exception as e:
            logger.error("Scheduler start failed: %s", e)

    def stop(self):
        if self._scheduler:
            self._scheduler.shutdown(wait=False)

    def add_one_shot(self, trigger_time: datetime, tool_name: str, params: dict) -> str:
        job_id = f"oneshot_{uuid.uuid4().hex[:8]}"
        if not self._scheduler:
            logger.warning("Scheduler not running — cannot add job")
            return job_id

        from kria.tools.registry import tool_registry

        async def _execute():
            await tool_registry.execute(tool_name, params)
            self._jobs.pop(job_id, None)

        self._scheduler.add_job(_execute, "date", run_date=trigger_time, id=job_id)
        self._jobs[job_id] = {
            "tool": tool_name,
            "params": params,
            "trigger": trigger_time.isoformat(),
            "type": "one_shot",
        }
        return job_id

    def add_cron(self, cron_expr: str, tool_name: str, params: dict, name: str = "") -> str:
        job_id = f"cron_{uuid.uuid4().hex[:8]}"
        if not self._scheduler:
            logger.warning("Scheduler not running — cannot add cron job")
            return job_id

        from kria.tools.registry import tool_registry
        parts = cron_expr.split()
        if len(parts) != 5:
            logger.error("Invalid cron expression: %s", cron_expr)
            return job_id

        async def _execute():
            await tool_registry.execute(tool_name, params)

        self._scheduler.add_job(
            _execute, "cron",
            minute=parts[0], hour=parts[1],
            day=parts[2], month=parts[3], day_of_week=parts[4],
            id=job_id,
        )
        self._jobs[job_id] = {
            "name": name, "tool": tool_name,
            "cron": cron_expr, "type": "cron",
        }
        return job_id

    def remove(self, job_id: str) -> bool:
        if not self._scheduler:
            return False
        try:
            self._scheduler.remove_job(job_id)
            self._jobs.pop(job_id, None)
            return True
        except Exception:
            return False

    def list_jobs(self) -> list[dict]:
        return [{"id": k, **v} for k, v in self._jobs.items()]


scheduler = KriaScheduler()
