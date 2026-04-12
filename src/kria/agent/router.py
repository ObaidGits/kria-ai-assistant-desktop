"""
Intent Router
=============
Classifies incoming user utterances into one of three categories:

  DIRECT_TOOL  — A single, clear OS action ("open Chrome", "what's my CPU usage").
                 The agent loop executes a single tool call without a long CoT plan.

  AGENT_LOOP   — A complex, multi-step request ("find all PDFs from last week
                 and move them to Archive").  Enters the full ReAct planning loop.

  CONVERSATION — Pure conversation, question, or chitchat.  No tool needed.

The router itself calls the LLM with a tiny prompt and max_tokens=10, so it
adds ~30 ms of latency on top of a cached intent.

Resilience: if the LLM call fails (circuit open), defaults to AGENT_LOOP —
the safest option that handles everything, just more slowly.
"""
import logging
import re
from enum import Enum
from typing import Optional

from kria.agent.llm_client import llm_client
from kria.infra.redis_bus import redis_bus

logger = logging.getLogger("kria.router")

_SYSTEM = """\
You are an intent classifier for an OS-control voice assistant.
Given the user's message classify it with exactly ONE word:

  DIRECT_TOOL  — single, clear OS action (open app, get info, set volume, etc.)
  AGENT_LOOP   — complex, multi-step, or requires reasoning / planning
  CONVERSATION — chat, question, or no action needed

Reply with ONLY the classification word. No punctuation, no explanation.\
"""


class IntentType(Enum):
    DIRECT_TOOL = "direct_tool"
    AGENT_LOOP = "agent_loop"
    CONVERSATION = "conversation"


# Fast-path regex patterns — bypass LLM classification entirely
_CONVERSATION_RE = re.compile(
    r"^\s*"
    r"(h(i|ey|ye|ello|ola|owdy)"
    r"|good\s*(morning|afternoon|evening|night)"
    r"|how\s+are\s+you"
    r"|what'?s\s+up"
    r"|thanks?\s*(you)?|thank\s+you"
    r"|bye|goodbye|see\s+you"
    r"|yo|sup"
    r"|who\s+are\s+you"
    r"|what\s+is\s+your\s+name"
    r"|tell\s+me\s+(about\s+yourself|a\s+joke)"
    r")\s*[!?.]*\s*$",
    re.IGNORECASE,
)

# User-triggered deep thinking keywords → force AGENT_LOOP
_THINK_HARD_RE = re.compile(
    r"think\s+(carefully|hard|deeply|step\s+by\s+step)"
    r"|reason\s+through"
    r"|plan\s+(this|it|out)"
    r"|analyze\s+(this|it)",
    re.IGNORECASE,
)


class IntentRouter:
    async def classify(self, user_message: str) -> IntentType:
        # ── Fast-path: regex pre-classification (no LLM call) ─────
        if _CONVERSATION_RE.match(user_message):
            logger.info("Router fast-path: CONVERSATION (regex match)")
            return IntentType.CONVERSATION

        if _THINK_HARD_RE.search(user_message):
            logger.info("Router fast-path: AGENT_LOOP (think-hard keyword)")
            return IntentType.AGENT_LOOP

        # Check cache first (identical query in the same session)
        cache_key = f"intent:{hash(user_message) & 0xFFFFFFFF}"
        cached = await redis_bus.cache_get(cache_key)
        if cached:
            try:
                return IntentType(cached)
            except ValueError:
                pass

        intent = await self._call_llm(user_message)

        await redis_bus.cache_set(cache_key, intent.value, ttl=300)
        return intent

    async def _call_llm(self, message: str) -> IntentType:
        try:
            result = await llm_client.chat(
                messages=[
                    {"role": "system", "content": _SYSTEM},
                    {"role": "user", "content": message},
                ],
                temperature=0.0,
                max_tokens=10,
            )
            if result:
                msg = result["choices"][0]["message"]
                text = (msg.get("content") or msg.get("reasoning_content") or "").strip().upper()
                mapping = {
                    "DIRECT_TOOL": IntentType.DIRECT_TOOL,
                    "AGENT_LOOP": IntentType.AGENT_LOOP,
                    "CONVERSATION": IntentType.CONVERSATION,
                }
                if text in mapping:
                    return mapping[text]
                # Handle partial matches (e.g. model adds punctuation)
                for key, val in mapping.items():
                    if key in text:
                        return val
        except Exception as exc:
            logger.warning("Intent router LLM call failed: %s — defaulting to CONVERSATION", exc)

        return IntentType.CONVERSATION


intent_router = IntentRouter()
