"""
Interaction Gateway
===================
Multi-choice decision gateway for the ReAct loop.

Unlike HITL (binary approve/deny for dangerous actions), this gateway
presents the user with multiple options and auto-selects the recommended
choice on timeout — ensuring forward progress.

Flow:
  1. LLM calls the ``ask_user`` tool with a question and options.
  2. Gateway creates an InteractionRequest with a Future and stores it.
  3. Gateway broadcasts a WebSocket message to the dashboard.
  4. Gateway awaits the Future with a configurable timeout (default 20s).
  5. The dashboard (or REST endpoint) calls ``submit_choice(id, index)``.
  6. If timeout → auto-select ``options[recommended]``, log, continue.
  7. Cleanup: pending request removed regardless of outcome.
"""
import asyncio
import logging
import uuid
from dataclasses import dataclass, field
from typing import Any, Callable, Optional

from kria.infra.config import settings

logger = logging.getLogger("kria.agent.interaction")


@dataclass
class InteractionRequest:
    id: str = field(default_factory=lambda: f"ixn_{uuid.uuid4().hex[:12]}")
    question: str = ""
    options: list[str] = field(default_factory=list)
    recommended: int = 0        # 0-based index into options
    context: str = ""
    timeout_seconds: float = 20.0
    _future: Optional[asyncio.Future] = field(default=None, repr=False)

    def __post_init__(self) -> None:
        try:
            self._future = asyncio.get_running_loop().create_future()
        except RuntimeError:
            self._future = None


class InteractionGateway:
    def __init__(self) -> None:
        self._pending: dict[str, InteractionRequest] = {}
        self._broadcast: Optional[Callable] = None
        self._ws_clients_count: Callable = lambda: 0

    def set_broadcast_handler(
        self, handler: Callable, clients_count: Optional[Callable] = None,
    ) -> None:
        """Wire up the WebSocket broadcaster (called during FastAPI startup)."""
        self._broadcast = handler
        if clients_count:
            self._ws_clients_count = clients_count

    # ── Ask the user ──────────────────────────────────────────────

    async def ask_user(
        self,
        question: str,
        options: list[str],
        recommended: int = 0,
        context: str = "",
        timeout: Optional[float] = None,
    ) -> dict:
        """
        Present a multi-choice question to the user.

        Returns:
            {"chosen": "option text", "index": int, "source": "user"|"timeout"}
        """
        recommended = max(0, min(recommended, len(options) - 1)) if options else 0
        timeout_s = timeout or settings.interaction_timeout_seconds

        req = InteractionRequest(
            question=question,
            options=options,
            recommended=recommended,
            context=context,
            timeout_seconds=timeout_s,
        )
        self._pending[req.id] = req

        logger.info(
            "[interaction] Question posed: %r — %d options, req_id=%s",
            question[:80], len(options), req.id,
        )

        # Notify dashboard via WebSocket
        if self._broadcast:
            try:
                await self._broadcast({
                    "type": "interaction_request",
                    "id": req.id,
                    "question": question,
                    "options": options,
                    "recommended": recommended,
                    "context": context,
                    "timeout_seconds": timeout_s,
                })
            except Exception as exc:
                logger.warning("[interaction] WebSocket broadcast failed: %s", exc)

        # Console fallback
        print(
            f"\n[interaction] Question for user:\n"
            f"  {question}\n"
            + "".join(f"  [{i}] {opt}{'  ← recommended' if i == recommended else ''}\n"
                      for i, opt in enumerate(options))
            + f"  Request ID: {req.id}\n"
            f"  Timeout:    {timeout_s}s\n"
            f"  → POST /api/v1/interaction/decide  "
            f"{{\"request_id\":\"{req.id}\", \"choice_index\":0}}\n"
        )

        # Terminal-mode: if no WebSocket clients, prompt stdin
        if settings.hitl_terminal_mode and self._ws_clients_count() == 0:
            try:
                loop = asyncio.get_running_loop()
                prompt = f"[interaction] Choose [0-{len(options)-1}] (default {recommended}): "
                answer = await asyncio.wait_for(
                    loop.run_in_executor(None, lambda: input(prompt).strip()),
                    timeout=timeout_s,
                )
                idx = int(answer) if answer.isdigit() else recommended
                idx = max(0, min(idx, len(options) - 1))
                chosen = options[idx]
                logger.info("[interaction] Terminal choice for %s: [%d] %s", req.id, idx, chosen)
                return {"chosen": chosen, "index": idx, "source": "user"}
            except (asyncio.TimeoutError, EOFError, OSError, ValueError):
                chosen = options[recommended] if options else ""
                logger.info("[interaction] Terminal fallback for %s: auto-select [%d]", req.id, recommended)
                return {"chosen": chosen, "index": recommended, "source": "timeout"}
            finally:
                self._pending.pop(req.id, None)

        # WebSocket mode: await Future
        try:
            if req._future is None:
                logger.error("[interaction] Future not initialized — auto-select recommended")
                chosen = options[recommended] if options else ""
                return {"chosen": chosen, "index": recommended, "source": "timeout"}

            choice_index: int = await asyncio.wait_for(
                asyncio.shield(req._future),
                timeout=timeout_s,
            )
            choice_index = max(0, min(choice_index, len(options) - 1))
            chosen = options[choice_index] if options else ""
            logger.info("[interaction] User chose [%d] %s for %s", choice_index, chosen, req.id)
            return {"chosen": chosen, "index": choice_index, "source": "user"}

        except asyncio.TimeoutError:
            chosen = options[recommended] if options else ""
            logger.info("[interaction] Timeout for %s — auto-select [%d] %s", req.id, recommended, chosen)

            # Notify dashboard of timeout
            if self._broadcast:
                try:
                    await self._broadcast({
                        "type": "interaction_timeout",
                        "id": req.id,
                        "chosen": chosen,
                        "index": recommended,
                    })
                except Exception:
                    pass

            return {"chosen": chosen, "index": recommended, "source": "timeout"}
        finally:
            self._pending.pop(req.id, None)

    # ── Submit choice ─────────────────────────────────────────────

    async def submit_choice(self, request_id: str, choice_index: int) -> bool:
        """
        Called by the WebSocket handler or REST endpoint when the user picks.
        Returns True if the request was found and resolved.
        """
        req = self._pending.get(request_id)
        if req and req._future and not req._future.done():
            req._future.set_result(choice_index)
            return True
        logger.warning("[interaction] submit_choice: %s not found or already resolved", request_id)
        return False

    # ── Inspection ────────────────────────────────────────────────

    def get_pending(self) -> list[dict]:
        return [
            {
                "id": r.id,
                "question": r.question,
                "options": r.options,
                "recommended": r.recommended,
                "context": r.context,
                "timeout_seconds": r.timeout_seconds,
            }
            for r in self._pending.values()
        ]

    def cancel_all(self) -> None:
        """Emergency stop — resolve all pending futures with recommended index."""
        for req in list(self._pending.values()):
            if req._future and not req._future.done():
                req._future.set_result(req.recommended)
        self._pending.clear()
        logger.warning("[interaction] All pending requests cancelled")


interaction_gateway = InteractionGateway()
