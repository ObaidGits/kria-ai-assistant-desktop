"""
Service Health Registry
=======================
A lightweight, in-process registry that tracks the health of every external
service K.R.I.A. depends on (Redis, LLM, STT, TTS, ChromaDB, SQLite).

Modules call ``health_registry.update()`` after each connectivity check.
Other modules call ``health_registry.is_healthy()`` before deciding whether
to use a service or fall back to an alternative.

The singleton ``health_registry`` is imported by all modules.
"""
import time
from dataclasses import dataclass, field
from enum import Enum
from typing import Optional


class ServiceStatus(Enum):
    HEALTHY = "healthy"
    DEGRADED = "degraded"
    DOWN = "down"
    UNKNOWN = "unknown"


@dataclass
class ServiceHealth:
    name: str
    status: ServiceStatus = ServiceStatus.UNKNOWN
    last_check: float = field(default_factory=time.monotonic)
    error: Optional[str] = None


class HealthRegistry:
    """Global service health registry."""

    def __init__(self) -> None:
        self._services: dict[str, ServiceHealth] = {}

    def register(self, name: str) -> None:
        """Register a service (call once at startup)."""
        if name not in self._services:
            self._services[name] = ServiceHealth(name=name)

    def update(
        self,
        name: str,
        status: ServiceStatus,
        error: Optional[str] = None,
    ) -> None:
        """Update health status for a service."""
        if name not in self._services:
            self.register(name)
        self._services[name].status = status
        self._services[name].last_check = time.monotonic()
        self._services[name].error = error

    def is_healthy(self, name: str) -> bool:
        svc = self._services.get(name)
        return svc is not None and svc.status == ServiceStatus.HEALTHY

    def get_all(self) -> dict[str, ServiceHealth]:
        return dict(self._services)

    def summary(self) -> dict[str, str]:
        return {name: svc.status.value for name, svc in self._services.items()}


# Singleton — imported by all modules
health_registry = HealthRegistry()
