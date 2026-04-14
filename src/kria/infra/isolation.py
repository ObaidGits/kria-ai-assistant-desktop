"""
Fault Isolation Decorator
=========================
Every tool function and every external service call is wrapped with
``@isolated`` to guarantee that exceptions never propagate to the caller.

``ToolResult`` is the standard return envelope shared across all tool
executions and service clients.

Usage::

    @isolated
    async def my_tool(path: str) -> str:
        ...  # Any exception here → ToolResult(success=False, error=...)

    result: ToolResult = await my_tool("/some/path")
    if result.success:
        use(result.data)
    else:
        handle_error(result.error)
"""
import asyncio
import logging
import traceback
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from functools import wraps
from typing import Any, Callable, Optional

logger = logging.getLogger("kria.isolation")

# Dedicated thread pool for tool execution so blocking I/O
# (file ops, subprocess, psutil) never freezes the async event loop.
_tool_pool = ThreadPoolExecutor(max_workers=4, thread_name_prefix="kria-tool")


@dataclass
class ToolResult:
    success: bool
    data: Any = None
    error: Optional[str] = None

    def __bool__(self) -> bool:
        return self.success


def isolated(func: Callable) -> Callable:
    """
    Decorator that guarantees the wrapped *async* function never raises
    and offloads execution to a thread pool.

    Most tool functions are ``async def`` but perform blocking I/O
    (file system, subprocess, psutil).  Running them in a worker thread
    keeps the main event loop responsive for health checks, WebSocket,
    and other concurrent HTTP requests.
    """
    @wraps(func)
    async def wrapper(*args: Any, **kwargs: Any) -> ToolResult:
        try:
            loop = asyncio.get_running_loop()
            coro = func(*args, **kwargs)
            result = await loop.run_in_executor(_tool_pool, asyncio.run, coro)
            if isinstance(result, ToolResult):
                return result
            return ToolResult(success=True, data=result)
        except Exception as exc:
            logger.error(
                "[%s] Unhandled exception: %s\n%s",
                func.__name__,
                exc,
                traceback.format_exc(),
            )
            return ToolResult(success=False, error=str(exc))

    return wrapper
