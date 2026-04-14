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

from kria.agent.model_router import model_router
from kria.infra.redis_bus import redis_bus

logger = logging.getLogger("kria.router")

_SYSTEM = """\
You are an intent classifier for an OS-control voice assistant.
Given the user's message classify it with exactly ONE word:

  DIRECT_TOOL  — single, clear OS action (open app, get info, search files, \
set volume, read document, check system, download, install, etc.)
  AGENT_LOOP   — complex, multi-step, or requires reasoning / planning
  CONVERSATION — pure chat, greeting, or no action needed

Reply with ONLY the classification word. No punctuation, no explanation.
/no_think\
"""


class IntentType(Enum):
    DIRECT_TOOL = "direct_tool"
    AGENT_LOOP = "agent_loop"
    CONVERSATION = "conversation"


# ── Tool categories: map regex-detected verb to a small set of tools ──
# This helps the small model by only showing 3-5 relevant tools.
_TOOL_CATEGORIES: dict[str, list[str]] = {
    "search_web": ["web_search", "deep_search", "fetch_webpage", "get_news"],
    "search_files": ["search_files", "list_directory", "read_file"],
    "system_info": ["get_cpu_usage", "get_memory_info", "get_disk_space", "get_battery_status", "get_time", "get_network_status"],
    "app_control": ["open_application", "close_application", "list_running_apps"],
    "download": ["download_file", "fetch_webpage"],
    "shell": ["execute_shell"],
    "read_doc": ["read_file", "search_files", "list_directory"],
    "write_file": ["write_file", "read_file"],
    "weather": ["get_weather"],
    "news": ["get_news", "deep_search", "web_search"],
    "network": ["ping_host", "get_public_ip", "web_search"],
    "clipboard": ["get_clipboard", "set_clipboard"],
    "power": ["lock_screen"],
    "reminder": ["schedule_reminder", "send_notification"],
    "knowledge": ["remember_fact", "recall_fact", "search_knowledge"],
    "install": ["execute_shell"],
    "notify": ["send_notification"],
    "snap": ["snap_install", "snap_remove", "snap_list", "snap_search"],
    "flatpak": ["flatpak_install", "flatpak_remove", "flatpak_list", "flatpak_search"],
}

# Map regex match groups to tool categories
_VERB_TO_CATEGORY: dict[str, str] = {
    "open": "app_control", "launch": "app_control", "start": "app_control",
    "run": "shell", "close": "app_control", "kill": "app_control", "stop": "app_control",
    "find": "search_files", "search": "search_web", "look": "search_files",
    "locate": "search_files", "where": "search_files",
    "get": "system_info", "show": "read_doc", "check": "system_info",
    "what": "system_info",
    "read": "read_doc", "parse": "read_doc", "summarize": "read_doc", "convert": "read_doc",
    "download": "download", "install": "install", "uninstall": "install", "update": "install",
    "set": "clipboard",
    "lock": "power", "shutdown": "power", "reboot": "power", "restart": "power", "suspend": "power",
    "ping": "network", "dns": "network", "curl": "network", "fetch": "network",
    "remind": "reminder", "schedule": "reminder", "notify": "notify",
    "remember": "knowledge", "recall": "knowledge", "save": "knowledge",
    "copy": "clipboard", "paste": "clipboard", "clipboard": "clipboard",
    "organize": "search_files", "watch": "search_files", "monitor": "search_files",
    "list": "search_files",
    "snap": "snap", "flatpak": "flatpak",
}


def _detect_tool_hint(message: str) -> Optional[list[str]]:
    """Detect which tool category fits the message and return tool names."""
    text = message.lower().strip()

    # Special keyword-based overrides (check before generic verb matching)
    if re.search(r"\b(gold|stock|price|bitcoin|crypto|forex)\b", text):
        return _TOOL_CATEGORIES["search_web"]

    if re.search(r"\bweather|forecast\b", text):
        return _TOOL_CATEGORIES["weather"]

    if re.search(r"\bnews|headline\b", text):
        return _TOOL_CATEGORIES["news"]

    # Current events / real-time info → force web tools (prevent hallucination)
    if re.search(r"\b(war|conflict|election|latest|breaking|today'?s?|current|recent|update|happening|ongoing|crisis|attack|ceasefire|treaty|summit)\b", text):
        return _TOOL_CATEGORIES["search_web"]

    if re.search(r"\b(folders?|director(?:y|ies)|files?)\b.*\b(named?|called)\b", text):
        return _TOOL_CATEGORIES["search_files"]

    if re.search(r"\b(search|find|look)\b.*\b(files?|folders?|director(?:y|ies))\b", text):
        return _TOOL_CATEGORIES["search_files"]

    if re.search(r"\blist\b.*\b(files?|folders?|director(?:y|ies)|contents?)\b", text):
        return _TOOL_CATEGORIES["search_files"]

    if re.search(r"\blist\b.*\b(in\s+/|in\s+~|in\s+the)\b", text):
        return _TOOL_CATEGORIES["search_files"]

    # "show/display the content(s) of X", "show me X folder/directory/file"
    if re.search(r"\b(show|display|print|cat|view)\b.*\b(content|contents|inside|folder|directory|file|tree)\b", text):
        return _TOOL_CATEGORIES["read_doc"]

    if re.search(r"\b(read|open)\b.*\b(file|document|log|config|txt|json|yaml|yml|md|csv)\b", text):
        return _TOOL_CATEGORIES["read_doc"]

    if re.search(r"\b(web|internet|online|google|search the|search\s+(?:for|about)\s+(?!files?|folders?|director))\b", text):
        return _TOOL_CATEGORIES["search_web"]

    if re.search(r"\b(time|clock|date today)\b", text):
        return _TOOL_CATEGORIES["system_info"]

    if re.search(r"\b(cpu|memory|ram|disk|battery|storage|space)\b", text):
        return _TOOL_CATEGORIES["system_info"]

    if re.search(r"\b(ip address|public ip|ping|dns)\b", text):
        return _TOOL_CATEGORIES["network"]

    if re.search(r"\b(clipboard|copy|paste)\b", text):
        return _TOOL_CATEGORIES["clipboard"]

    if re.search(r"\bsnap\b", text):
        return _TOOL_CATEGORIES["snap"]

    if re.search(r"\bflatpak\b", text):
        return _TOOL_CATEGORIES["flatpak"]

    if re.search(r"\b(remind|reminder|alarm)\b", text):
        return _TOOL_CATEGORIES["reminder"]

    # Verb-based matching
    m = _DIRECT_TOOL_RE.match(text)
    if m:
        verb = m.group(1).split()[0].lower()
        cat = _VERB_TO_CATEGORY.get(verb)
        if cat:
            return _TOOL_CATEGORIES.get(cat)

    return None


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

# Common tool-triggering verbs → fast-path DIRECT_TOOL (skip LLM call)
_DIRECT_TOOL_RE = re.compile(
    r"^\s*"
    r"(open|launch|start|run|close|kill|stop"
    r"|find|search|look\s+for|locate|where\s+is"
    r"|get|show|check|what('?s|\s+is)\s+(my|the|current)"
    r"|what\s+(time|date|day)"
    r"|read|parse|summarize|convert"
    r"|download|install|uninstall|update"
    r"|set\s+(volume|brightness|clipboard)"
    r"|lock|shutdown|reboot|restart|suspend"
    r"|ping|dns|curl|fetch"
    r"|remind|schedule|notify"
    r"|remember|recall|save\s+snippet"
    r"|copy|paste|clipboard"
    r"|organize|watch|monitor"
    r"|snap|flatpak"
    r"|list\s+(services|files|plugins|tasks|processes))"
    r"\b",
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
    async def classify(self, user_message: str) -> tuple[IntentType, Optional[list[str]]]:
        """
        Classify user intent. Returns (IntentType, tool_hint).
        tool_hint is a list of relevant tool names for DIRECT_TOOL, or None.
        """
        # ── Fast-path: regex pre-classification (no LLM call) ─────
        if _CONVERSATION_RE.match(user_message):
            logger.info("Router fast-path: CONVERSATION (regex match)")
            return IntentType.CONVERSATION, None

        if _DIRECT_TOOL_RE.match(user_message):
            hint = _detect_tool_hint(user_message)
            logger.info("Router fast-path: DIRECT_TOOL (regex match) tools=%s", hint)
            return IntentType.DIRECT_TOOL, hint

        # ── Keyword-based tool detection: if _detect_tool_hint finds a
        #    matching tool category, this IS a tool request regardless of
        #    the sentence structure (e.g. "how is the weather in kolkata")
        keyword_hint = _detect_tool_hint(user_message)
        if keyword_hint:
            logger.info("Router fast-path: DIRECT_TOOL (keyword hint) tools=%s", keyword_hint)
            return IntentType.DIRECT_TOOL, keyword_hint

        if _THINK_HARD_RE.search(user_message):
            logger.info("Router fast-path: AGENT_LOOP (think-hard keyword)")
            return IntentType.AGENT_LOOP, None

        # Check cache first (identical query in the same session)
        cache_key = f"intent:{hash(user_message) & 0xFFFFFFFF}"
        cached = await redis_bus.cache_get(cache_key)
        if cached:
            try:
                intent = IntentType(cached)
                hint = _detect_tool_hint(user_message) if intent == IntentType.DIRECT_TOOL else None
                return intent, hint
            except ValueError:
                pass

        intent = await self._call_llm(user_message)
        hint = _detect_tool_hint(user_message) if intent == IntentType.DIRECT_TOOL else None

        await redis_bus.cache_set(cache_key, intent.value, ttl=300)
        return intent, hint

    async def _call_llm(self, message: str) -> IntentType:
        try:
            _client = model_router.get_classification_client()
            result = await _client.chat(
                messages=[
                    {"role": "system", "content": _SYSTEM},
                    {"role": "user", "content": message},
                ],
                temperature=0.0,
                max_tokens=10,
            )
            if result:
                msg = result["choices"][0]["message"]
                content = (msg.get("content") or "").strip().upper()
                # Fallback: extract from reasoning_content (Qwen3 /think quirk)
                if not content:
                    rc = (msg.get("reasoning_content") or "").strip()
                    # Take the very last line which usually has the answer
                    content = rc.split("\n")[-1].strip().upper() if rc else ""
                mapping = {
                    "DIRECT_TOOL": IntentType.DIRECT_TOOL,
                    "AGENT_LOOP": IntentType.AGENT_LOOP,
                    "CONVERSATION": IntentType.CONVERSATION,
                }
                if content in mapping:
                    return mapping[content]
                # Handle partial matches (e.g. model adds punctuation)
                for key, val in mapping.items():
                    if key in content:
                        return val
                logger.info("Router LLM returned unrecognized: %r", content[:100])
        except Exception as exc:
            logger.warning("Intent router LLM call failed: %s — defaulting to AGENT_LOOP", exc)

        # Fail-safe: AGENT_LOOP handles everything (just slower than DIRECT_TOOL)
        return IntentType.AGENT_LOOP


intent_router = IntentRouter()
