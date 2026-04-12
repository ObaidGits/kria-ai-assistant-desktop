"""
Supervised Task Runner
======================
Runs an async coroutine factory in a restart loop.  If the coroutine
crashes, the supervisor logs the error and restarts it after an
exponential back-off delay (capped at *max_delay* seconds).

This is used for all long-lived background tasks:
  - Wake-word listener
  - Redis pub/sub subscriber
  - Periodic health-check probe
  - Rollback cleanup scheduler

Usage::

    task = SupervisedTask(
        name="wake_word",
        coro_factory=my_wake_word_listener,   # zero-arg async callable
        max_retries=20,
    )
    task.start()   # non-blocking; returns asyncio.Task
    ...
    await task.stop()
"""
import asyncio
import logging
from typing import Callable

logger = logging.getLogger("kria.supervisor")


class SupervisedTask:
    def __init__(
        self,
        name: str,
        coro_factory: Callable,
        max_retries: int = 10,
        base_delay: float = 1.0,
        max_delay: float = 60.0,
    ) -> None:
        self.name = name
        self.coro_factory = coro_factory
        self.max_retries = max_retries
        self.base_delay = base_delay
        self.max_delay = max_delay
        self._task: asyncio.Task | None = None
        self._retries: int = 0
        self._stopped: bool = False

    async def _run_loop(self) -> None:
        while self._retries <= self.max_retries and not self._stopped:
            try:
                logger.info("[%s] Starting (attempt %d)", self.name, self._retries + 1)
                await self.coro_factory()
                if not self._stopped:
                    logger.warning("[%s] Exited cleanly — restarting", self.name)
            except asyncio.CancelledError:
                logger.info("[%s] Cancelled", self.name)
                return
            except Exception as exc:
                self._retries += 1
                delay = min(self.base_delay * (2 ** self._retries), self.max_delay)
                logger.error(
                    "[%s] Crashed: %s. Retry %d/%d in %.1fs",
                    self.name, exc, self._retries, self.max_retries, delay,
                )
                await asyncio.sleep(delay)

        if not self._stopped:
            logger.critical("[%s] Exceeded max retries (%d). Task disabled.", self.name, self.max_retries)

    def start(self) -> asyncio.Task:
        self._stopped = False
        self._retries = 0
        self._task = asyncio.create_task(self._run_loop(), name=f"supervised:{self.name}")
        return self._task

    async def stop(self) -> None:
        self._stopped = True
        if self._task and not self._task.done():
            self._task.cancel()
            try:
                await self._task
            except asyncio.CancelledError:
                pass
