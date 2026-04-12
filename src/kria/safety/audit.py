"""
Audit Logger
============
Writes every tool action — approved, denied, or auto-executed — to the
append-only ``audit_log`` table in SQLite (schema in persistent.py).

Design rules:
  - ``log()`` NEVER raises. A write failure is swallowed and logged at ERROR.
    The calling agent loop must never crash because the audit log is unavailable.
  - All parameters are JSON-serialized so the raw string can be searched.
  - ``query_recent()`` supports filtering by risk level and session.
"""
import json
import logging
import time
from typing import Optional

from kria.memory.persistent import sqlite_manager

logger = logging.getLogger("kria.safety.audit")


class AuditLogger:
    async def log(
        self,
        session_id: str,
        action: str,
        parameters: dict,
        risk_level: str,
        decision: str,
        decided_by: str,
        result: Optional[str] = None,
        error_msg: Optional[str] = None,
        rollback_id: Optional[str] = None,
        duration_ms: Optional[int] = None,
    ) -> None:
        """Write one audit record.  Exceptions are caught internally."""
        try:
            await sqlite_manager.execute(
                """INSERT INTO audit_log
                   (session_id, action, parameters, risk_level,
                    decision, decided_by, result, error_msg, rollback_id, duration_ms)
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)""",
                (
                    session_id,
                    action,
                    json.dumps(parameters),
                    risk_level,
                    decision,
                    decided_by,
                    result,
                    error_msg,
                    rollback_id,
                    duration_ms,
                ),
                fetch=False,
            )
        except Exception as exc:
            logger.error("Audit log write failed (non-fatal): %s", exc)

    async def query_recent(
        self,
        limit: int = 50,
        risk_level: Optional[str] = None,
        session_id: Optional[str] = None,
    ) -> list[dict]:
        """Return recent audit records, newest first."""
        conditions: list[str] = []
        params: list = []

        if risk_level:
            conditions.append("risk_level = ?")
            params.append(risk_level)
        if session_id:
            conditions.append("session_id = ?")
            params.append(session_id)

        where = ("WHERE " + " AND ".join(conditions)) if conditions else ""
        params.append(limit)

        return await sqlite_manager.execute(
            f"SELECT * FROM audit_log {where} ORDER BY timestamp DESC LIMIT ?",
            tuple(params),
        )

    async def get_by_id(self, record_id: int) -> Optional[dict]:
        rows = await sqlite_manager.execute(
            "SELECT * FROM audit_log WHERE id = ?", (record_id,)
        )
        return rows[0] if rows else None


audit_logger = AuditLogger()
