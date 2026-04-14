"""
Context Manager
===============
Assembles the full context packet fed to the agent on each request:
  1. Recent conversation history  (conversation_memory)
  2. Relevant long-term memories  (semantic_memory)
  3. User preferences             (sqlite user_preferences table)
  4. System state summary         (CPU, memory, battery — lightweight)

The context manager is called by api/routes.py and voice/pipeline.py
immediately before invoking the agent loop.
"""
import logging
from typing import Optional

from kria.memory.conversation import conversation_memory
from kria.memory.semantic import semantic_memory
from kria.memory.mem0_memory import mem0_memory

logger = logging.getLogger("kria.memory.context_manager")


class ContextManager:
    # ── User preferences ──────────────────────────────────────────

    async def get_preference(self, key: str) -> Optional[str]:
        from kria.memory.persistent import sqlite_manager
        rows = await sqlite_manager.execute(
            "SELECT value FROM user_preferences WHERE key = ?", (key,)
        )
        return rows[0]["value"] if rows else None

    async def set_preference(self, key: str, value: str, description: str = "") -> None:
        from kria.memory.persistent import sqlite_manager
        await sqlite_manager.execute(
            """INSERT INTO user_preferences (key, value, description)
               VALUES (?, ?, ?)
               ON CONFLICT(key) DO UPDATE SET value=excluded.value,
               description=excluded.description,
               updated_at=CURRENT_TIMESTAMP""",
            (key, value, description),
            fetch=False,
        )

    async def get_all_preferences(self) -> dict[str, str]:
        from kria.memory.persistent import sqlite_manager
        rows = await sqlite_manager.execute("SELECT key, value FROM user_preferences", ())
        return {r["key"]: r["value"] for r in rows}

    # ── Context assembly ──────────────────────────────────────────

    async def build_context(
        self,
        session_id: str,
        user_input: str,
        history_limit: int = 20,
        semantic_results: int = 3,
    ) -> dict:
        """
        Return a context dict used by the agent loop and API responses.

        Keys:
          conversation_history  — list[dict] OpenAI message format
          relevant_memories     — list[dict] semantic search results
          user_preferences      — dict[str, str]
          system_state          — dict (lightweight resource snapshot)
        """
        # 1. Conversation history
        from kria.agent.prompts import build_system_prompt
        messages = await conversation_memory.build_messages(
            session_id=session_id,
            system_prompt=build_system_prompt(),
            limit=history_limit,
        )

        # 2. Semantic memory (non-blocking — returns [] if unavailable)
        memories = await semantic_memory.query(
            query_text=user_input,
            n_results=semantic_results,
            session_id=session_id,
        )

        # 2b. Mem0 fact memory (non-blocking — returns [] if unavailable)
        facts = await mem0_memory.search(
            query=user_input,
            limit=5,
        )

        # 3. User preferences
        prefs = await self.get_all_preferences()

        # 4. Lightweight system state
        system_state = await self._get_system_state()

        return {
            "conversation_history": messages,
            "relevant_memories": memories,
            "user_facts": facts,
            "user_preferences": prefs,
            "system_state": system_state,
        }

    @staticmethod
    async def _get_system_state() -> dict:
        try:
            import psutil
            vm = psutil.virtual_memory()
            return {
                "cpu_percent": psutil.cpu_percent(interval=None),
                "memory_used_percent": vm.percent,
                "memory_available_gb": round(vm.available / 1024**3, 1),
            }
        except Exception:
            return {}

    # ── Post-turn storage ─────────────────────────────────────────

    async def save_turn(
        self,
        session_id: str,
        user_input: str,
        assistant_response: str,
        tool_calls: Optional[list] = None,
    ) -> None:
        """Persist a completed turn to conversation + semantic + Mem0 memories."""
        await conversation_memory.add_turn(session_id, "user", user_input)
        await conversation_memory.add_turn(session_id, "assistant", assistant_response)

        # Store a summarised chunk in semantic memory for long-term recall
        summary = f"User: {user_input}\nAssistant: {assistant_response}"
        await semantic_memory.store(summary, session_id=session_id, memory_type="conversation")

        # Feed the turn to Mem0 for automatic fact extraction (fire-and-forget)
        messages = [
            {"role": "user", "content": user_input},
            {"role": "assistant", "content": assistant_response},
        ]
        await mem0_memory.add(messages, metadata={"session_id": session_id})


context_manager = ContextManager()
