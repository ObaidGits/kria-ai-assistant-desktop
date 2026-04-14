"""
Event Bus
=========
Pub/sub event bus for system events (file changes, app launches, etc.).
"""
import asyncio
import logging
from typing import Callable
from collections import defaultdict

logger = logging.getLogger("kria.automation.events")


class EventBus:
    """Pub/sub event bus for system events."""

    def __init__(self):
        self._handlers: dict[str, list[Callable]] = defaultdict(list)

    def subscribe(self, event_type: str, handler: Callable):
        self._handlers[event_type].append(handler)
        logger.debug("Subscribed to '%s'", event_type)

    def unsubscribe(self, event_type: str, handler: Callable):
        try:
            self._handlers[event_type].remove(handler)
        except ValueError:
            pass

    def emit(self, event_type: str, data: dict):
        for handler in self._handlers.get(event_type, []):
            try:
                if asyncio.iscoroutinefunction(handler):
                    asyncio.ensure_future(handler(event_type, data))
                else:
                    handler(event_type, data)
            except Exception as e:
                logger.error("Event handler error for '%s': %s", event_type, e)


event_bus = EventBus()
