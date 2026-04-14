"""
ReAct Agent Loop
================
Implements the Think → Act → Observe cycle at the core of K.R.I.A.

Each iteration:
  1. Send conversation messages (with tool descriptions embedded in the
     system prompt) to the LLM.
  2. If the LLM returns <tool_call> blocks in the text: run each through
     the safety policy, execute (or block), append the tool result.
  3. If the LLM returns plain content: that is the final answer — return it.
  4. Hard stop at MAX_ITERATIONS to prevent infinite loops.

Tool calling approach:
  llama.cpp silently drops API-level tools for chat templates that lack
  tool-call formatting (e.g. Phi-4).  Instead we embed tool descriptions
  in the system prompt and parse <tool_call> JSON blocks from the text.

Safety gate integration:
  - GREEN  → execute immediately, log silently.
  - YELLOW → execute immediately, publish Redis notification.
  - RED    → request HITL approval; create rollback snapshot before executing.
  - BLACK  → hard deny, log with ``decided_by=HARDCODED``.

All exceptions inside tool execution are caught by the @isolated wrapper
and returned as ToolResult(success=False) — they never crash the loop.
"""
import asyncio
import json
import logging
import re
import time
from dataclasses import dataclass, field

from kria.agent.model_router import model_router
from kria.agent.prompts import build_system_prompt
from kria.agent.response_validator import ResponseValidator
from kria.infra.isolation import ToolResult
from kria.infra.redis_bus import redis_bus

logger = logging.getLogger("kria.agent")

_SUMMARY_HINT = "Now give me the answer only. Be brief and direct."
_validator = ResponseValidator(model_router.config.validation)

# ── Emergency stop signal ─────────────────────────────────────────
_terminate = asyncio.Event()


def signal_terminate() -> None:
    """Set the emergency stop flag — the loop exits at the next iteration."""
    _terminate.set()
    logger.warning("[agent] Emergency terminate signal received")


def clear_terminate() -> None:
    """Reset the emergency stop flag."""
    _terminate.clear()


async def _broadcast_stage(stage: str, detail: str = "") -> None:
    """Broadcast the current ReAct stage to the dashboard."""
    try:
        from kria.api.websocket import broadcast
        await broadcast({"type": "agent_stage", "stage": stage, "detail": detail})
    except Exception:
        pass

# Regex to find <tool_call>{"name": "...", "arguments": {...}}</tool_call>
_TOOL_CALL_RE = re.compile(
    r'<tool_call>\s*(\{.*?\})\s*</tool_call>',
    re.DOTALL,
)
# Regex to detect bracket-style tool calls: [tool_name("arg", key="val")]
# Some models (Qwen) emit this non-standard format instead of <tool_call> XML.
_BRACKET_CALL_RE = re.compile(
    r'\[([a-zA-Z_][a-zA-Z0-9_]*)\s*\(([^)]*)\)\]'
)


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
        tool_hint: list[str] | None = None,
    ) -> AgentResponse:
        # Lazy imports to avoid circular dependencies at module load time
        from kria.safety.audit import audit_logger
        from kria.safety.hitl import hitl_gateway
        from kria.safety.policy_engine import RiskLevel, policy_engine
        from kria.safety.rollback import rollback_manager
        from kria.tools.registry import tool_registry

        # Pick the inference backend once for this entire request.
        # route() inspects messages for vision content in auto mode.
        _llm = model_router.route(intent=intent, messages=[
            *conversation_history,
            {"role": "user", "content": user_message},
        ])
        _uses_native_tools = _llm.tool_calling_mode == "native_api"
        _max_iter = _llm.max_iterations
        logger.info("[agent] using backend=%s mode=%s intent=%s native_tools=%s max_iter=%d",
                    _llm.health_key,
                    model_router.mode, intent, _uses_native_tools, _max_iter)

        # DIRECT_TOOL: small tool set + no thinking (fast)
        # AGENT_LOOP: full tool set + thinking (thorough)
        use_think = intent != "direct_tool"

        # Select tool schemas to embed in the system prompt
        if intent == "direct_tool" and tool_hint:
            tool_schemas = tool_registry.get_openai_schemas_filtered(tool_hint)
        elif intent == "direct_tool":
            tool_schemas = tool_registry.get_openai_schemas_lite()
        else:
            tool_schemas = tool_registry.get_openai_schemas_lite()

        # Build system prompt — for native tool-calling backends, omit tool descriptions
        # from the prompt since tools are sent via the API. For prompt-based, embed them.
        prompt_tool_schemas = None if _uses_native_tools else tool_schemas

        # Fetch relevant Mem0 facts for personalization (non-blocking)
        user_facts: list[dict] = []
        try:
            from kria.memory.mem0_memory import mem0_memory
            user_facts = await mem0_memory.search(query=user_message, limit=5)
        except Exception:
            pass

        messages = [
            {"role": "system", "content": build_system_prompt(
                think=use_think,
                model_label=_llm.model_label,
                tool_schemas=prompt_tool_schemas,
                user_facts=user_facts,
            )},
            *conversation_history,
            {"role": "user", "content": user_message},
        ]

        tool_log: list[dict] = []

        for iteration in range(_max_iter):
            # ── Emergency stop check ──
            if _terminate.is_set():
                clear_terminate()
                logger.warning("[agent] Terminated by emergency signal at iter=%d", iteration)
                return AgentResponse(
                    text="Task terminated by user.",
                    tool_calls=tool_log,
                    iterations=iteration,
                    success=False,
                )

            await _broadcast_stage("thinking", f"iteration {iteration + 1}")

            # For DIRECT_TOOL after first tool call: rebuild prompt WITHOUT
            # tool schemas so the LLM produces a text summary.
            _send_api_tools = _uses_native_tools and tool_schemas
            if intent == "direct_tool" and tool_log:
                messages[0]["content"] = build_system_prompt(
                    think=False,
                    model_label=_llm.model_label,
                    tool_schemas=None,
                )
                _ensure_no_think(messages)
                _send_api_tools = False  # No more tool calls for summary
                if not any(m.get("content") == _SUMMARY_HINT for m in messages if m.get("role") == "user"):
                    messages.append({"role": "user", "content": _SUMMARY_HINT})

            logger.info(
                "[agent] iter=%d intent=%s msg_count=%d",
                iteration, intent, len(messages),
            )

            # Native API tool calling: send tools param in request.
            # Prompt-based: tools are embedded in the system prompt text.
            chat_kwargs: dict = {"messages": messages}
            if _send_api_tools:
                chat_kwargs["tools"] = tool_schemas
            result = await _llm.chat(
                **chat_kwargs,
            )

            if result is None:
                return AgentResponse(
                    text="I'm having trouble reaching my reasoning engine right now.",
                    tool_calls=tool_log,
                    iterations=iteration + 1,
                    success=False,
                )

            choice = result["choices"][0]["message"]
            content = choice.get("content") or ""

            # Qwen3 reasoning_content fallback
            if not content:
                rc = (choice.get("reasoning_content") or "").strip()
                if rc:
                    content = _extract_from_reasoning(rc)

            logger.info(
                "[agent] iter=%d tool_calls_api=%s content_len=%d content_preview=%r",
                iteration,
                bool(choice.get("tool_calls")),
                len(content),
                content[:120],
            )

            # ── Case 1: API-level tool_calls (model natively supports it) ──
            if api_tool_calls := choice.get("tool_calls"):
                messages.append({"role": "assistant", **_strip_content(choice)})

                for tc in api_tool_calls:
                    func_name = tc["function"]["name"]
                    await _broadcast_stage("acting", func_name)
                    try:
                        func_args = json.loads(tc["function"]["arguments"])
                    except json.JSONDecodeError:
                        logger.warning("Malformed tool args for %s: %s", func_name, tc["function"]["arguments"][:200])
                        func_args = {}

                    call_id = tc["id"]
                    tool_result = await self._execute_tool(
                        func_name, func_args, session_id,
                        tool_registry, policy_engine, hitl_gateway,
                        rollback_manager, audit_logger,
                    )
                    tool_log.append({"tool": func_name, "args": func_args, "call_id": call_id})
                    await _broadcast_stage("observing", func_name)

                    messages.append({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "name": func_name,
                        "content": json.dumps({
                            "success": tool_result.success,
                            "data": tool_result.data,
                            "error": tool_result.error,
                        }),
                    })
                continue

            # ── Case 2a: Bracket-style tool calls [func_name("arg")] ──
            # Convert to <tool_call> format then fall through to Case 2.
            if not _TOOL_CALL_RE.search(content) and _BRACKET_CALL_RE.search(content):
                content = _convert_bracket_calls(content)

            # ── Case 2: Text-based tool calls (<tool_call> blocks) ──
            parsed_calls = _TOOL_CALL_RE.findall(content)
            if parsed_calls:
                messages.append({"role": "assistant", "content": content})

                for i, tc_json_str in enumerate(parsed_calls):
                    try:
                        tc = json.loads(tc_json_str)
                        func_name = tc.get("name", "")
                        func_args = tc.get("arguments", {})
                        if isinstance(func_args, str):
                            func_args = json.loads(func_args)
                    except (json.JSONDecodeError, KeyError) as e:
                        logger.warning("Malformed text tool call: %s — raw=%s", e, tc_json_str[:200])
                        messages.append({
                            "role": "user",
                            "content": f'<tool_result>\n{{"error": "Malformed tool call: {e}"}}\n</tool_result>',
                        })
                        continue

                    call_id = f"text_{iteration}_{i}"
                    await _broadcast_stage("acting", func_name)
                    tool_result = await self._execute_tool(
                        func_name, func_args, session_id,
                        tool_registry, policy_engine, hitl_gateway,
                        rollback_manager, audit_logger,
                    )
                    tool_log.append({"tool": func_name, "args": func_args, "call_id": call_id})
                    await _broadcast_stage("observing", func_name)

                    result_data = json.dumps({
                        "success": tool_result.success,
                        "data": tool_result.data,
                        "error": tool_result.error,
                    })
                    logger.info(
                        "[agent] TEXT_TOOL_RESULT: %s success=%s result=%s",
                        func_name, tool_result.success, result_data[:300],
                    )
                    messages.append({
                        "role": "user",
                        "content": f"<tool_result>\n{result_data}\n</tool_result>",
                    })
                continue

            # ── Case 3: Final text response (no tool calls) ──
            clean = _strip_tool_tags(content)
            if clean:
                await _broadcast_stage("complete")
                return AgentResponse(
                    text=clean,
                    tool_calls=tool_log,
                    iterations=iteration + 1,
                    success=True,
                )

        # Max iterations exceeded
        logger.warning("[agent] max iterations (%d) reached for: %s", _max_iter, user_message[:80])
        return AgentResponse(
            text=(
                "I've been working on this for a while and couldn't complete it. "
                "Could you try breaking your request into smaller steps?"
            ),
            tool_calls=tool_log,
            iterations=_max_iter,
            success=False,
        )

    async def _execute_tool(
        self,
        func_name: str,
        func_args: dict,
        session_id: str,
        tool_registry,
        policy_engine,
        hitl_gateway,
        rollback_manager,
        audit_logger,
    ) -> ToolResult:
        """Execute a tool call through the safety pipeline. Shared by API and text-based paths."""
        from kria.safety.policy_engine import RiskLevel

        # Short-circuit: ask_user bypasses safety pipeline entirely
        if func_name == "ask_user":
            from kria.agent.interaction import interaction_gateway
            result = await interaction_gateway.ask_user(
                question=func_args.get("question", ""),
                options=func_args.get("options", []),
                recommended=func_args.get("recommended", 0),
                context=func_args.get("context", ""),
            )
            return ToolResult(success=True, data=result)

        t_start = time.monotonic()

        policy = await policy_engine.evaluate(func_name, func_args)

        if policy.risk_level == RiskLevel.BLACK:
            tool_result = ToolResult(
                success=False,
                error=f"Blocked by safety policy: {policy.reason}",
            )
            await audit_logger.log(
                session_id=session_id, action=func_name, parameters=func_args,
                risk_level="BLACK", decision="BLOCKED", decided_by="HARDCODED",
                result="FAILED", error_msg=policy.reason,
            )

        elif policy.risk_level == RiskLevel.RED:
            desc = f"Execute {func_name} with args {json.dumps(func_args)[:120]}"
            approved = await hitl_gateway.request_approval(
                action=func_name, parameters=func_args,
                risk_level="RED", description=desc,
            )
            if approved:
                rollback_id = None
                file_paths = _extract_file_paths(func_args)
                if file_paths:
                    rollback_id = await rollback_manager.create_snapshot(
                        session_id=session_id, action=func_name,
                        risk_level="RED", files=file_paths,
                    )
                tool_result = await tool_registry.execute(func_name, func_args)
                duration = int((time.monotonic() - t_start) * 1000)
                await audit_logger.log(
                    session_id=session_id, action=func_name, parameters=func_args,
                    risk_level="RED", decision="APPROVED", decided_by="USER_GUI",
                    result="SUCCESS" if tool_result.success else "FAILED",
                    error_msg=tool_result.error, rollback_id=rollback_id,
                    duration_ms=duration,
                )
            else:
                tool_result = ToolResult(success=False, error="Action denied by user.")
                await audit_logger.log(
                    session_id=session_id, action=func_name, parameters=func_args,
                    risk_level="RED", decision="DENIED", decided_by="USER_GUI",
                    result="FAILED",
                )

        else:
            # GREEN or YELLOW — execute immediately
            tool_result = await tool_registry.execute(func_name, func_args)
            duration = int((time.monotonic() - t_start) * 1000)
            await audit_logger.log(
                session_id=session_id, action=func_name, parameters=func_args,
                risk_level=policy.risk_level.value, decision="AUTO_EXECUTED",
                decided_by="POLICY",
                result="SUCCESS" if tool_result.success else "FAILED",
                error_msg=tool_result.error, duration_ms=duration,
            )
            if policy.risk_level.value == "YELLOW":
                await redis_bus.publish("tool.executed", {
                    "action": func_name,
                    "risk_level": "YELLOW",
                    "success": tool_result.success,
                })

        logger.info(
            "[agent] TOOL_CALL: %s args=%s success=%s duration=%dms",
            func_name, json.dumps(func_args)[:200],
            tool_result.success, int((time.monotonic() - t_start) * 1000),
        )
        return tool_result


# ── Helpers ───────────────────────────────────────────────────────

def _ensure_no_think(messages: list[dict]) -> None:
    """Append /no_think hint so Qwen3 returns content (not reasoning_content)."""
    for msg in messages:
        if msg["role"] == "system" and "/think" in msg["content"] and "/no_think" not in msg["content"]:
            msg["content"] = msg["content"].replace("/think", "/no_think")
            break


def _strip_content(msg: dict) -> dict:
    """Return a copy of the message dict without None content."""
    return {k: v for k, v in msg.items() if v is not None}


def _strip_tool_tags(text: str) -> str:
    """Remove leftover <tool_call>/<tool_result> blocks and bracket-style calls."""
    cleaned = _TOOL_CALL_RE.sub("", text).strip()
    cleaned = re.sub(r'</?tool_call>', '', cleaned).strip()
    cleaned = re.sub(r'</?tool_result>', '', cleaned).strip()
    # Remove any bracket-style calls that weren't converted/executed
    cleaned = _BRACKET_CALL_RE.sub("", cleaned).strip()
    return cleaned


def _convert_bracket_calls(text: str) -> str:
    """Convert bracket-style [func(args)] calls to <tool_call> XML format."""
    def _bracket_to_xml(m: re.Match) -> str:
        func_name = m.group(1)
        raw_args = m.group(2).strip()
        # Try to extract the first positional string arg -> {"query": value}
        # e.g. [deep_search("current president of India")]
        str_match = re.match(r'^["\'](.+?)["\']$', raw_args)
        kv_matches = re.findall(r'([a-zA-Z_]\w*)\s*=\s*["\']([^"\']*)["\']', raw_args)
        if kv_matches:
            args_dict = {k: v for k, v in kv_matches}
        elif str_match:
            # Positional string — infer param name from tool
            param_name_map = {
                "deep_search": "query", "web_search": "query",
                "fetch_webpage": "url", "get_news": "query",
                "get_weather": "location", "execute_shell": "command",
                "search_files": "pattern", "read_file": "path",
            }
            pname = param_name_map.get(func_name, "query")
            args_dict = {pname: str_match.group(1)}
        else:
            args_dict = {}
        import json as _json
        return f'<tool_call>{{"name": "{func_name}", "arguments": {_json.dumps(args_dict)}}}</tool_call>'
    return _BRACKET_CALL_RE.sub(_bracket_to_xml, text)


def _extract_from_reasoning(rc: str) -> str:
    """Extract a usable answer from reasoning_content (Qwen3 quirk)."""
    parts = rc.split("\n\n")
    candidate = parts[-1].strip() if parts else rc
    cot_cues = ("okay", "the user", "i need", "i should",
                "let me", "alright", "so ", "now ")
    if candidate.lower().startswith(cot_cues):
        sentences = [s.strip() for s in rc.replace("\n", " ").split(". ") if s.strip()]
        for s in reversed(sentences):
            if not s.lower().startswith(cot_cues):
                return s.rstrip(".") + "."
        return sentences[-1] if sentences else rc
    return candidate


def _collapse_tool_messages(messages: list[dict]) -> list[dict]:
    """
    Rewrite the message list so that tool_calls assistant messages and
    tool-role result messages are collapsed into a single assistant
    message summarising what was done.  This avoids llama.cpp 400 errors
    when tool schemas are dropped for the summary iteration.
    """
    collapsed: list[dict] = []
    tool_summary_parts: list[str] = []

    for msg in messages:
        if msg.get("role") == "assistant" and msg.get("tool_calls"):
            for tc in msg["tool_calls"]:
                name = tc.get("function", {}).get("name", "unknown")
                args = tc.get("function", {}).get("arguments", "{}")
                tool_summary_parts.append(f"I called {name}({args})")
            continue
        if msg.get("role") == "tool":
            content = msg.get("content", "")
            tool_summary_parts.append(f"Result: {content[:500]}")
            continue
        if tool_summary_parts:
            collapsed.append({
                "role": "assistant",
                "content": "\n".join(tool_summary_parts),
            })
            tool_summary_parts = []
        collapsed.append(msg)

    if tool_summary_parts:
        collapsed.append({
            "role": "assistant",
            "content": "\n".join(tool_summary_parts),
        })

    return collapsed


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
