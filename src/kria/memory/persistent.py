"""
SQLite Persistence Manager
==========================
Manages the K.R.I.A. SQLite database with three tables:

  conversations      — Full conversation history (user + assistant + tool turns)
  conversations_fts  — FTS5 full-text index over conversations.content
  audit_log          — Append-only security audit trail (see SAFETY_SPECIFICATION.md)
  user_preferences   — Key-value store for user config

Design decisions:
  - Uses aiosqlite for non-blocking async I/O.
  - Schema is applied via ``CREATE TABLE IF NOT EXISTS`` — safe to re-run on every boot.
  - ``execute()`` catches all DB errors and returns [] instead of raising,
    so callers are not responsible for SQLite error handling.
  - The health registry is updated so other modules can check SQLite status.
"""
import json
import logging
from pathlib import Path
from typing import Any, Optional

from kria.infra.config import settings
from kria.infra.health import ServiceStatus, health_registry

logger = logging.getLogger("kria.sqlite")

SCHEMA_SQL = """
PRAGMA journal_mode = WAL;
PRAGMA synchronous  = NORMAL;

CREATE TABLE IF NOT EXISTS conversations (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id  TEXT    NOT NULL,
    role        TEXT    NOT NULL CHECK (role IN ('user','assistant','system','tool')),
    content     TEXT    NOT NULL,
    timestamp   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    metadata    TEXT,
    tool_name   TEXT,
    tool_result TEXT,
    model_used  TEXT,
    tokens_used INTEGER
);

CREATE INDEX IF NOT EXISTS idx_conv_session ON conversations(session_id);
CREATE INDEX IF NOT EXISTS idx_conv_ts      ON conversations(timestamp);

CREATE VIRTUAL TABLE IF NOT EXISTS conversations_fts USING fts5(
    content,
    content=conversations,
    content_rowid=id,
    tokenize="porter unicode61"
);

CREATE TRIGGER IF NOT EXISTS conv_insert_fts
    AFTER INSERT ON conversations BEGIN
        INSERT INTO conversations_fts(rowid, content) VALUES (new.id, new.content);
    END;

CREATE TABLE IF NOT EXISTS audit_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    session_id  TEXT    NOT NULL,
    action      TEXT    NOT NULL,
    parameters  TEXT    NOT NULL,
    risk_level  TEXT    NOT NULL CHECK (risk_level IN ('GREEN','YELLOW','RED','BLACK')),
    decision    TEXT    NOT NULL CHECK (decision IN (
                    'AUTO_EXECUTED','APPROVED','DENIED','BLOCKED','TIMEOUT')),
    decided_by  TEXT    NOT NULL CHECK (decided_by IN (
                    'POLICY','USER_VOICE','USER_GUI','TIMEOUT','HARDCODED')),
    result      TEXT             CHECK (result IN ('SUCCESS','FAILED','ROLLED_BACK',NULL)),
    error_msg   TEXT,
    rollback_id TEXT,
    duration_ms INTEGER
);

CREATE INDEX IF NOT EXISTS idx_audit_ts      ON audit_log(timestamp);
CREATE INDEX IF NOT EXISTS idx_audit_session ON audit_log(session_id);
CREATE INDEX IF NOT EXISTS idx_audit_risk    ON audit_log(risk_level);

CREATE TABLE IF NOT EXISTS user_preferences (
    key         TEXT PRIMARY KEY,
    value       TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    updated_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
"""


class SQLiteManager:
    def __init__(self) -> None:
        self._db: Any = None  # aiosqlite.Connection
        health_registry.register("sqlite")

    async def connect(self) -> None:
        try:
            import aiosqlite

            db_path = Path(settings.sqlite_path)
            db_path.parent.mkdir(parents=True, exist_ok=True)

            self._db = await aiosqlite.connect(str(db_path))
            self._db.row_factory = aiosqlite.Row
            await self._db.executescript(SCHEMA_SQL)
            # Migrations: add columns that were missing in earlier schema versions
            for col, col_def in [
                ("tool_name",   "TEXT"),
                ("tool_result", "TEXT"),
                ("model_used",  "TEXT"),
                ("tokens_used", "INTEGER"),
            ]:
                try:
                    await self._db.execute(
                        f"ALTER TABLE conversations ADD COLUMN {col} {col_def}"
                    )
                except Exception:
                    pass  # column already exists
            # user_preferences migrations
            for col, col_def in [
                ("description", "TEXT NOT NULL DEFAULT ''"),
            ]:
                try:
                    await self._db.execute(
                        f"ALTER TABLE user_preferences ADD COLUMN {col} {col_def}"
                    )
                except Exception:
                    pass  # column already exists
            await self._db.commit()
            health_registry.update("sqlite", ServiceStatus.HEALTHY)
            logger.info("SQLite initialized at %s", db_path)
        except ImportError:
            health_registry.update("sqlite", ServiceStatus.DOWN, "aiosqlite not installed")
            logger.warning("aiosqlite not installed — persistence disabled")
        except Exception as exc:
            health_registry.update("sqlite", ServiceStatus.DOWN, str(exc))
            logger.error("SQLite init failed: %s", exc)

    async def execute(
        self,
        query: str,
        params: tuple = (),
        fetch: bool = True,
    ) -> list[Any]:
        """
        Execute a SQL statement.  Returns rows for SELECT,
        empty list for INSERT/UPDATE/DELETE, empty list on any error.
        """
        if not self._db:
            return []
        try:
            cursor = await self._db.execute(query, params)
            await self._db.commit()
            if fetch:
                rows = await cursor.fetchall()
                return [dict(r) for r in rows]
            return []
        except Exception as exc:
            logger.error("SQLite query failed: %s | query=%s", exc, query[:80])
            return []

    async def executemany(self, query: str, params_list: list[tuple]) -> None:
        if not self._db:
            return
        try:
            await self._db.executemany(query, params_list)
            await self._db.commit()
        except Exception as exc:
            logger.error("SQLite executemany failed: %s", exc)

    async def close(self) -> None:
        if self._db:
            await self._db.close()
            self._db = None
        health_registry.update("sqlite", ServiceStatus.DOWN)


# Singleton — shared by audit logger, conversation store, and preferences
sqlite_manager = SQLiteManager()
