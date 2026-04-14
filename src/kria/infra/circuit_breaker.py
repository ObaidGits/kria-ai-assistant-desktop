"""
Circuit Breaker
===============
Wraps any async callable with automatic failure detection and recovery.

States:
  CLOSED   → Normal operation. All calls go through.
  OPEN     → Too many failures. Calls are short-circuited and return fallback.
  HALF_OPEN→ Recovery probe. One call allowed; success → CLOSED, fail → OPEN.

Usage::

    cb = CircuitBreaker("my-service", failure_threshold=3, recovery_timeout=30.0)
    result = await cb.call(my_async_function, arg1, kwarg=val)
"""
import asyncio
import logging
import time
from enum import Enum
from typing import Any, Callable

logger = logging.getLogger("kria.infra.circuit_breaker")


class CircuitState(Enum):
    CLOSED = "closed"
    OPEN = "open"
    HALF_OPEN = "half_open"


class CircuitBreaker:
    def __init__(
        self,
        name: str,
        failure_threshold: int = 3,
        recovery_timeout: float = 30.0,
        fallback: Any = None,
    ) -> None:
        self.name = name
        self.failure_threshold = failure_threshold
        self.recovery_timeout = recovery_timeout
        self.fallback = fallback

        self._state = CircuitState.CLOSED
        self._failure_count = 0
        self._last_failure_time = 0.0
        self._lock = asyncio.Lock()

    @property
    def state(self) -> CircuitState:
        """Read-only snapshot (best-effort without lock). Use _check_state() under lock for mutations."""
        return self._state

    def _check_state(self) -> CircuitState:
        """Must be called while holding self._lock."""
        if self._state == CircuitState.OPEN:
            if time.monotonic() - self._last_failure_time >= self.recovery_timeout:
                self._state = CircuitState.HALF_OPEN
                logger.info("[circuit:%s] OPEN → HALF_OPEN (recovery probe)", self.name)
        return self._state

    async def call(self, func: Callable[..., Any], *args: Any, **kwargs: Any) -> Any:
        """
        Execute *func* guarded by the circuit.
        Returns *fallback* immediately when the circuit is OPEN.
        """
        async with self._lock:
            current = self._check_state()

        if current == CircuitState.OPEN:
            logger.debug("[circuit:%s] OPEN — returning fallback", self.name)
            return self.fallback

        try:
            result = await func(*args, **kwargs)
            async with self._lock:
                # Successful probe or normal call — reset
                if self._state != CircuitState.CLOSED:
                    logger.info("[circuit:%s] %s → CLOSED (success)", self.name, self._state.value)
                self._failure_count = 0
                self._state = CircuitState.CLOSED
            return result
        except Exception as exc:
            async with self._lock:
                self._failure_count += 1
                self._last_failure_time = time.monotonic()
                if self._failure_count >= self.failure_threshold:
                    logger.warning(
                        "[circuit:%s] CLOSED → OPEN after %d failures: %r",
                        self.name, self._failure_count, exc,
                    )
                    self._state = CircuitState.OPEN
                else:
                    logger.warning(
                        "[circuit:%s] failure %d/%d: %r",
                        self.name, self._failure_count, self.failure_threshold, exc,
                    )
            return self.fallback
