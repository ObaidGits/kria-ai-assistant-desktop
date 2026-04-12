"""
Conversation Memory
===================
Persist and retrieve conversation turns from SQLite.
Each turn has: session_id, role ('user'|'assistant'|'system'|'tool'),
content, and optional tool metadata.

The ``conversations`` table was created by persistent.py SCHEMA_SQL.
Full-text search across content is powered by the FTS5 trigger index
also defined there.
"""
import json
import logging
from typing import Optional

from kria.memory.persistent import sqlite_manager

logger = logging.getLogger("kria.memory.conversation")


class ConversationMemory:
    # ── Write ─────────────────────────────────────────────────────

    async def add_turn(
        self,
        session_id: str,
        role: str,
        content: str,
        tool_name: Optional[str] = None,
        tool_result: Optional[dict] = None,
        model_used: Optional[str] = None,
        tokens_used: Optional[int] = None,
    ) -> None:
        await sqlite_manager.execute(
            """INSERT INTO conversations
               (session_id, role, content, tool_name, tool_result, model_used, tokens_used)
               VALUES (?, ?, ?, ?, ?, ?, ?)""",
            (
                session_id, role, content,
                tool_name,
                json.dumps(tool_result) if tool_result else None,
                model_used, tokens_used,
            ),
            fetch=False,
        )

    # ── Read ──────────────────────────────────────────────────────

    async def get_recent(
        self,
        session_id: str,
        limit: int = 20,
        roles: Optional[list[str]] = None,
    ) -> list[dict]:
        """Return the *limit* most recent turns for a session (oldest first)."""
        if roles:
            placeholders = ",".join("?" * len(roles))
            rows = await sqlite_manager.execute(
                f"""SELECT * FROM conversations
                    WHERE session_id = ? AND role IN ({placeholders})
                    ORDER BY id DESC LIMIT ?""",
                (session_id, *roles, limit),
            )
        else:
            rows = await sqlite_manager.execute(
                """SELECT * FROM conversations
                   WHERE session_id = ? ORDER BY id DESC LIMIT ?""",
                (session_id, limit),
            )
        return list(reversed(rows))  # chronological order

    async def search(self, query: str, limit: int = 10) -> list[dict]:
        """Full-text search across all conversation content."""
        return await sqlite_manager.execute(
            """SELECT c.* FROM conversations c
               JOIN conversations_fts f ON c.rowid = f.rowid
               WHERE conversations_fts MATCH ?
               ORDER BY rank LIMIT ?""",
            (query, limit),
        )

    async def get_sessions(self) -> list[str]:
        """Return distinct session IDs ordered by most recent first."""
        rows = await sqlite_manager.execute(
            "SELECT DISTINCT session_id FROM conversations ORDER BY id DESC",
            (),
        )
        return [r["session_id"] for r in rows]

    async def delete_session(self, session_id: str) -> int:
        """Delete all turns for a session. Returns how many rows were deleted."""
        rows = await sqlite_manager.execute(
            "SELECT COUNT(*) AS cnt FROM conversations WHERE session_id = ?",
            (session_id,),
        )
        count = rows[0]["cnt"] if rows else 0
        await sqlite_manager.execute(
            "DELETE FROM conversations WHERE session_id = ?",
            (session_id,),
            fetch=False,
        )
        return count

    # ── LLM message formatting ────────────────────────────────────

    async def build_messages(
        self,
        session_id: str,
        system_prompt: str,
        limit: int = 20,
    ) -> list[dict]:
        """
        Return a list of OpenAI-style message dicts ready to send to the LLM.
        Ordering: [system] + [recent turns]
        """
        turns = await self.get_recent(session_id, limit=limit, roles=["user", "assistant"])
        messages: list[dict] = [{"role": "system", "content": system_prompt}]
        for t in turns:
            messages.append({"role": t["role"], "content": t["content"]})
        return messages


conversation_memory = ConversationMemory()
