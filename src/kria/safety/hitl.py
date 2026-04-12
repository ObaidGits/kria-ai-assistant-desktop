"""
Human-in-the-Loop (HITL) Approval Gateway
==========================================
Manages pending approval requests for RED-tier actions.

Flow:
  1. agent/loop.py calls ``hitl_gateway.request_approval(...)``
  2. Gateway creates an ApprovalRequest with a Future and stores it.
  3. Gateway broadcasts a WebSocket message to the dashboard (non-blocking).
  4. Gateway awaits the Future with a 30-second timeout (configurable).
  5. The dashboard (or voice handler) calls ``submit_decision(id, approved)``.
  6. If timeout → auto-deny, log, return False.
  7. Cleanup: pending request removed regardless of outcome.

Multiple concurrent RED actions are fully supported — each request has its
own Future and independent timeout.
"""
import asyncio
import logging
import uuid
from dataclasses import dataclass, field
from typing import Any, Callable, Optional

from kria.infra.config import settings

logger = logging.getLogger("kria.safety.hitl")


@dataclass
class ApprovalRequest:
    id: str = field(default_factory=lambda: f"req_{uuid.uuid4().hex[:12]}")
    action: str = ""
    parameters: dict = field(default_factory=dict)
    risk_level: str = "RED"
    description: str = ""
    timeout_seconds: float = 30.0
    _future: Optional[asyncio.Future] = field(default=None, repr=False)

    def __post_init__(self) -> None:
        # Future must be created inside a running event loop
        try:
            self._future = asyncio.get_running_loop().create_future()
        except RuntimeError:
            self._future = None


class HITLGateway:
    def __init__(self) -> None:
        self._pending: dict[str, ApprovalRequest] = {}
        self._broadcast: Optional[Callable] = None  # injected by WebSocket manager

    def set_broadcast_handler(self, handler: Callable) -> None:
        """Wire up the WebSocket broadcaster (called during FastAPI startup)."""
        self._broadcast = handler

    # ── Request approval ──────────────────────────────────────────

    async def request_approval(
        self,
        action: str,
        parameters: dict,
        risk_level: str,
        description: str,
        timeout: Optional[float] = None,
    ) -> bool:
        """
        Block until the user approves/denies, or the timeout expires.
        Returns True if approved, False otherwise.
        """
        req = ApprovalRequest(
            action=action,
            parameters=parameters,
            risk_level=risk_level,
            description=description,
            timeout_seconds=timeout or settings.hitl_timeout_seconds,
        )
        self._pending[req.id] = req

        logger.info(
            "[HITL] Approval requested: %s (%s) — req_id=%s",
            action, risk_level, req.id,
        )

        # Notify dashboard via WebSocket (non-blocking — never block agent)
        if self._broadcast:
            try:
                await self._broadcast({
                    "type": "hitl_request",
                    "id": req.id,
                    "action": action,
                    "parameters": parameters,
                    "risk_level": risk_level,
                    "description": description,
                    "timeout_seconds": req.timeout_seconds,
                    "rollback_available": True,
                })
            except Exception as exc:
                logger.warning("[HITL] WebSocket broadcast failed: %s", exc)

        # Also log to console so the developer can approve in terminal
        print(
            f"\n[HITL] Action requires approval:\n"
            f"  Action:      {action}\n"
            f"  Risk:        {risk_level}\n"
            f"  Description: {description}\n"
            f"  Request ID:  {req.id}\n"
            f"  Timeout:     {req.timeout_seconds}s\n"
            f"  → POST /api/v1/hitl/decide  {{\"request_id\":\"{req.id}\", \"approved\":true}}\n"
        )

        try:
            if req._future is None:
                logger.error("[HITL] Future not initialized — auto-deny")
                return False
            approved: bool = await asyncio.wait_for(
                asyncio.shield(req._future),
                timeout=req.timeout_seconds,
            )
            logger.info("[HITL] Decision for %s: %s", req.id, "APPROVED" if approved else "DENIED")
            return approved
        except asyncio.TimeoutError:
            logger.info("[HITL] Timeout for %s — auto-deny", req.id)
            return False
        finally:
            self._pending.pop(req.id, None)

    # ── Submit decision ───────────────────────────────────────────

    async def submit_decision(self, request_id: str, approved: bool) -> bool:
        """
        Called by the WebSocket handler or REST endpoint when the user decides.
        Returns True if the request was found and resolved, False otherwise.
        """
        req = self._pending.get(request_id)
        if req and req._future and not req._future.done():
            req._future.set_result(approved)
            return True
        logger.warning("[HITL] submit_decision: request %s not found or already resolved", request_id)
        return False

    # ── Inspection ────────────────────────────────────────────────

    def get_pending(self) -> list[dict]:
        return [
            {
                "id": r.id,
                "action": r.action,
                "risk_level": r.risk_level,
                "description": r.description,
                "timeout_seconds": r.timeout_seconds,
            }
            for r in self._pending.values()
        ]

    def cancel_all(self) -> None:
        """Emergency stop — resolve all pending futures with False."""
        for req in list(self._pending.values()):
            if req._future and not req._future.done():
                req._future.set_result(False)
        self._pending.clear()
        logger.warning("[HITL] All pending requests cancelled (emergency stop)")


hitl_gateway = HITLGateway()
