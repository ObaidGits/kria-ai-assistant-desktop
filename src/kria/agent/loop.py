"""
ReAct Agent Loop
================
Implements the Think → Act → Observe cycle at the core of K.R.I.A.

Each iteration:
  1. Send conversation messages + tool schemas to the LLM.
  2. If the LLM returns tool_calls: run each through the safety policy,
     execute (or block), append the tool result for the next iteration.
  3. If the LLM returns a content string: that is the final answer — return it.
  4. Hard stop at MAX_ITERATIONS to prevent infinite loops.

Safety gate integration:
  - GREEN  → execute immediately, log silently.
  - YELLOW → execute immediately, publish Redis notification.
  - RED    → request HITL approval; create rollback snapshot before executing.
  - BLACK  → hard deny, log with ``decided_by=HARDCODED``.

All exceptions inside tool execution are caught by the @isolated wrapper
and returned as ToolResult(success=False) — they never crash the loop.
"""
import json
import logging
import time
from dataclasses import dataclass, field

from kria.agent.llm_client import llm_client
from kria.agent.prompts import build_system_prompt
from kria.infra.isolation import ToolResult
from kria.infra.redis_bus import redis_bus

logger = logging.getLogger("kria.agent")

MAX_ITERATIONS = 10


@dataclass
class AgentResponse:
    text: str
    tool_calls: list[dict] = field(default_factory=list)
    iterations: int = 0
    success: bool = True


class ReActLoop:
    async def run(
        self,
        user_message: str,
        conversation_history: list[dict],
        session_id: str = "default",
        intent: str = "agent_loop",
    ) -> AgentResponse:
        # Lazy imports to avoid circular dependencies at module load time
        from kria.safety.audit import audit_logger
        from kria.safety.hitl import hitl_gateway
        from kria.safety.policy_engine import RiskLevel, policy_engine
        from kria.safety.rollback import rollback_manager
        from kria.tools.registry import tool_registry

        messages = [
            {"role": "system", "content": build_system_prompt()},
            *conversation_history,
            {"role": "user", "content": user_message},
        ]
        tool_schemas = (
            tool_registry.get_openai_schemas_lite()
            if intent == "direct_tool"
            else tool_registry.get_openai_schemas()
        )
        tool_log: list[dict] = []

        for iteration in range(MAX_ITERATIONS):
            result = await llm_client.chat(
                messages=messages,
                tools=tool_schemas if tool_schemas else None,
            )

            if result is None:
                return AgentResponse(
                    text="I'm having trouble reaching my reasoning engine right now.",
                    tool_calls=tool_log,
                    iterations=iteration + 1,
                    success=False,
                )

            choice = result["choices"][0]["message"]

            # ── Case 1: LLM wants to call tools ───────────────────
            if tool_calls := choice.get("tool_calls"):
                messages.append({"role": "assistant", **_strip_content(choice)})

                for tc in tool_calls:
                    func_name: str = tc["function"]["name"]
                    try:
                        func_args: dict = json.loads(tc["function"]["arguments"])
                    except json.JSONDecodeError:
                        logger.warning("Malformed tool args for %s: %s", func_name, tc["function"]["arguments"][:200])
                        func_args = {}

                    tool_log.append({"tool": func_name, "args": func_args, "call_id": tc["id"]})

                    t_start = time.monotonic()

                    # ── Safety evaluation ──────────────────────────
                    policy = await policy_engine.evaluate(func_name, func_args)

                    if policy.risk_level == RiskLevel.BLACK:
                        tool_result = ToolResult(
                            success=False,
                            error=f"Blocked by safety policy: {policy.reason}",
                        )
                        await audit_logger.log(
                            session_id=session_id,
                            action=func_name,
                            parameters=func_args,
                            risk_level="BLACK",
                            decision="BLOCKED",
                            decided_by="HARDCODED",
                            result="FAILED",
                            error_msg=policy.reason,
                        )

                    elif policy.risk_level == RiskLevel.RED:
                        # Voice description for HITL prompt
                        desc = f"Execute {func_name} with args {json.dumps(func_args)[:120]}"
                        approved = await hitl_gateway.request_approval(
                            action=func_name,
                            parameters=func_args,
                            risk_level="RED",
                            description=desc,
                        )
                        if approved:
                            # Create rollback snapshot for file-touching actions
                            rollback_id = None
                            file_paths = _extract_file_paths(func_args)
                            if file_paths:
                                rollback_id = await rollback_manager.create_snapshot(
                                    session_id=session_id,
                                    action=func_name,
                                    risk_level="RED",
                                    files=file_paths,
                                )
                            tool_result = await tool_registry.execute(func_name, func_args)
                            duration = int((time.monotonic() - t_start) * 1000)
                            await audit_logger.log(
                                session_id=session_id,
                                action=func_name,
                                parameters=func_args,
                                risk_level="RED",
                                decision="APPROVED",
                                decided_by="USER_GUI",
                                result="SUCCESS" if tool_result.success else "FAILED",
                                error_msg=tool_result.error,
                                rollback_id=rollback_id,
                                duration_ms=duration,
                            )
                        else:
                            tool_result = ToolResult(
                                success=False,
                                error="Action denied by user.",
                            )
                            await audit_logger.log(
                                session_id=session_id,
                                action=func_name,
                                parameters=func_args,
                                risk_level="RED",
                                decision="DENIED",
                                decided_by="USER_GUI",
                                result="FAILED",
                            )

                    else:
                        # GREEN or YELLOW — execute immediately
                        tool_result = await tool_registry.execute(func_name, func_args)
                        duration = int((time.monotonic() - t_start) * 1000)
                        decision = "AUTO_EXECUTED"
                        await audit_logger.log(
                            session_id=session_id,
                            action=func_name,
                            parameters=func_args,
                            risk_level=policy.risk_level.value,
                            decision=decision,
                            decided_by="POLICY",
                            result="SUCCESS" if tool_result.success else "FAILED",
                            error_msg=tool_result.error,
                            duration_ms=duration,
                        )
                        # Notify dashboard for YELLOW actions
                        if policy.risk_level.value == "YELLOW":
                            await redis_bus.publish("tool.executed", {
                                "action": func_name,
                                "risk_level": "YELLOW",
                                "success": tool_result.success,
                            })

                    # Append tool observation for the next LLM iteration
                    messages.append({
                        "role": "tool",
                        "tool_call_id": tc["id"],
                        "content": json.dumps({
                            "success": tool_result.success,
                            "data": tool_result.data,
                            "error": tool_result.error,
                        }),
                    })

                continue  # → next iteration: LLM observes results

            # ── Case 2: LLM produced a final text response ─────────
            content = choice.get("content") or choice.get("reasoning_content") or ""
            if content:
                return AgentResponse(
                    text=content,
                    tool_calls=tool_log,
                    iterations=iteration + 1,
                    success=True,
                )

        # Max iterations exceeded
        logger.warning("[agent] MAX_ITERATIONS reached for: %s", user_message[:80])
        return AgentResponse(
            text=(
                "I've been working on this for a while and couldn't complete it. "
                "Could you try breaking your request into smaller steps?"
            ),
            tool_calls=tool_log,
            iterations=MAX_ITERATIONS,
            success=False,
        )


# ── Helpers ───────────────────────────────────────────────────────

def _strip_content(msg: dict) -> dict:
    """Return a copy of the message dict without None content."""
    return {k: v for k, v in msg.items() if v is not None}


def _extract_file_paths(params: dict) -> list[str]:
    """Pull out any string values from params that look like file paths."""
    paths = []
    for v in params.values():
        if isinstance(v, str) and (v.startswith("C:\\") or v.startswith("/")):
            paths.append(v)
        elif isinstance(v, list):
            paths.extend(item for item in v if isinstance(item, str) and "\\" in item)
    return paths


# Singleton
react_loop = ReActLoop()
