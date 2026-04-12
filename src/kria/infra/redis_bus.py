"""
Redis Bus
=========
Provides two services to the rest of K.R.I.A.:

1. **Pub/Sub message bus** — fire-and-forget event broadcasting between modules
   (e.g., ``tool.executed``, ``hitl.requested``, ``voice.wake_detected``).

2. **Key-value cache with TTL** — avoids redundant tool calls for short-lived
   read-only data (CPU usage, battery status, etc.).

Resilience design:
  - Circuit breaker prevents thrashing a dead Redis with connection attempts.
  - An in-memory dict acts as a fallback cache so tool output caching always works.
  - Pub/Sub publish is **best-effort** — it never blocks or raises to callers.
  - The health registry is updated after every connect attempt.
"""
import asyncio
import json
import logging
from typing import Any, Callable, Optional

from kria.infra.circuit_breaker import CircuitBreaker
from kria.infra.config import settings
from kria.infra.health import ServiceStatus, health_registry

logger = logging.getLogger("kria.redis")


class RedisBus:
    def __init__(self) -> None:
        self._client: Any = None          # redis.asyncio.Redis
        self._fallback_cache: dict[str, Any] = {}
        self._subscribers: dict[str, list[Callable]] = {}
        self._circuit = CircuitBreaker(
            name="redis",
            failure_threshold=3,
            recovery_timeout=15.0,
        )
        health_registry.register("redis")

    # ── Connection ────────────────────────────────────────────────

    async def connect(self) -> None:
        try:
            import redis.asyncio as aioredis  # lazy import — optional dep

            self._client = aioredis.from_url(
                settings.redis_url,
                decode_responses=True,
                socket_connect_timeout=5.0,
                retry_on_timeout=True,
            )
            await self._client.ping()
            health_registry.update("redis", ServiceStatus.HEALTHY)
            logger.info("Redis connected at %s", settings.redis_url)
        except ImportError:
            health_registry.update("redis", ServiceStatus.DOWN, "redis package not installed")
            logger.warning("redis[hiredis] not installed — in-memory cache only")
        except Exception as exc:
            health_registry.update("redis", ServiceStatus.DOWN, str(exc))
            logger.warning("Redis unavailable: %s — using in-memory fallback", exc)

    # ── Pub / Sub ─────────────────────────────────────────────────

    async def publish(self, channel: str, data: dict) -> None:
        """Publish an event.  Non-blocking, best-effort."""
        if not self._client:
            return
        try:
            await self._circuit.call(
                self._client.publish, channel, json.dumps(data)
            )
        except Exception:
            pass  # Intentionally swallowed — pub/sub is advisory

    async def subscribe(self, channel: str, handler: Callable) -> None:
        """Register a coroutine handler for a channel."""
        if channel not in self._subscribers:
            self._subscribers[channel] = []
        self._subscribers[channel].append(handler)

    # ── Cache ─────────────────────────────────────────────────────

    async def cache_get(self, key: str) -> Optional[Any]:
        """Read from Redis cache; fall back to in-memory."""
        if self._client and health_registry.is_healthy("redis"):
            try:
                raw = await self._circuit.call(self._client.get, key)
                if raw:
                    return json.loads(raw)
            except Exception:
                pass
        return self._fallback_cache.get(key)

    async def cache_set(
        self,
        key: str,
        value: Any,
        ttl: Optional[int] = None,
    ) -> None:
        """Write to Redis cache and in-memory fallback simultaneously."""
        ttl = ttl or settings.redis_cache_ttl_seconds
        self._fallback_cache[key] = value  # Always update in-memory

        if self._client and health_registry.is_healthy("redis"):
            try:
                await self._circuit.call(
                    self._client.setex, key, ttl, json.dumps(value)
                )
            except Exception:
                pass

    async def cache_delete(self, key: str) -> None:
        self._fallback_cache.pop(key, None)
        if self._client:
            try:
                await self._circuit.call(self._client.delete, key)
            except Exception:
                pass

    # ── Cleanup ───────────────────────────────────────────────────

    async def close(self) -> None:
        if self._client:
            await self._client.aclose()
            self._client = None
        health_registry.update("redis", ServiceStatus.DOWN)


# Singleton
redis_bus = RedisBus()
