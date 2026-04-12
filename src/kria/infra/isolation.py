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
import logging
import traceback
from dataclasses import dataclass
from functools import wraps
from typing import Any, Callable, Optional

logger = logging.getLogger("kria.isolation")


@dataclass
class ToolResult:
    success: bool
    data: Any = None
    error: Optional[str] = None

    def __bool__(self) -> bool:
        return self.success


def isolated(func: Callable) -> Callable:
    """
    Decorator that guarantees the wrapped *async* function never raises.

    On any uncaught exception it logs the traceback at ERROR level and
    returns ``ToolResult(success=False, error=<message>)``.
    """
    @wraps(func)
    async def wrapper(*args: Any, **kwargs: Any) -> ToolResult:
        try:
            result = await func(*args, **kwargs)
            # Functions may already return a ToolResult — pass it through.
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
