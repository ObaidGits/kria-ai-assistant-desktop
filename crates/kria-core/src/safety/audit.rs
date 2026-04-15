use rusqlite::{Connection, params};
use std::sync::Mutex;
use crate::safety::RiskLevel;

/// Decision made by the policy/HITL system.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub enum Decision {
    AutoExecuted,
    Approved,
    Denied,
    Blocked,
    Timeout,
}

impl Decision {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AutoExecuted => "AUTO_EXECUTED",
            Self::Approved => "APPROVED",
            Self::Denied => "DENIED",
            Self::Blocked => "BLOCKED",
            Self::Timeout => "TIMEOUT",
        }
    }
}

/// Who made the decision.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub enum DecidedBy {
    Policy,
    UserVoice,
    UserGui,
    Timeout,
    Hardcoded,
}

impl DecidedBy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Policy => "POLICY",
            Self::UserVoice => "USER_VOICE",
            Self::UserGui => "USER_GUI",
            Self::Timeout => "TIMEOUT",
            Self::Hardcoded => "HARDCODED",
        }
    }
}

/// Execution result status.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub enum ExecResult {
    Success,
    Failed,
    RolledBack,
}

impl ExecResult {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Success => "SUCCESS",
            Self::Failed => "FAILED",
            Self::RolledBack => "ROLLED_BACK",
        }
    }
}

/// Structured audit logger backed by SQLite.
pub struct AuditLogger {
    conn: Mutex<Connection>,
}

impl AuditLogger {
    pub fn new(conn: Connection) -> Self {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS audit_log (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                session_id  TEXT    NOT NULL,
                action      TEXT    NOT NULL,
                parameters  TEXT    NOT NULL,
                risk_level  TEXT    NOT NULL CHECK (risk_level IN ('GREEN', 'YELLOW', 'RED', 'BLACK')),
                decision    TEXT    NOT NULL CHECK (decision IN ('AUTO_EXECUTED', 'APPROVED', 'DENIED', 'BLOCKED', 'TIMEOUT')),
                decided_by  TEXT    NOT NULL CHECK (decided_by IN ('POLICY', 'USER_VOICE', 'USER_GUI', 'TIMEOUT', 'HARDCODED')),
                result      TEXT             CHECK (result IN ('SUCCESS', 'FAILED', 'ROLLED_BACK', NULL)),
                error_msg   TEXT,
                rollback_id TEXT,
                duration_ms INTEGER,
                network_url TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_audit_timestamp ON audit_log(timestamp);
            CREATE INDEX IF NOT EXISTS idx_audit_session   ON audit_log(session_id);
            CREATE INDEX IF NOT EXISTS idx_audit_risk      ON audit_log(risk_level);
            CREATE INDEX IF NOT EXISTS idx_audit_action    ON audit_log(action);"
        ).expect("failed to create audit_log table");

        Self { conn: Mutex::new(conn) }
    }

    /// Log a tool action with its policy decision.
    pub fn log(
        &self,
        session_id: &str,
        action: &str,
        parameters: &serde_json::Value,
        risk_level: RiskLevel,
        decision: Decision,
        decided_by: DecidedBy,
    ) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT INTO audit_log (session_id, action, parameters, risk_level, decision, decided_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                session_id,
                action,
                parameters.to_string(),
                risk_level.as_str(),
                decision.as_str(),
                decided_by.as_str(),
            ],
        );
    }

    /// Update the result of an already-logged action (post-execution).
    pub fn update_result(
        &self,
        action_id: i64,
        result: ExecResult,
        error_msg: Option<&str>,
        rollback_id: Option<&str>,
        duration_ms: Option<i64>,
    ) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE audit_log SET result = ?1, error_msg = ?2, rollback_id = ?3, duration_ms = ?4
             WHERE id = ?5",
            params![
                result.as_str(),
                error_msg,
                rollback_id,
                duration_ms,
                action_id,
            ],
        );
    }

    /// Query audit log entries.
    pub fn query(
        &self,
        session_id: Option<&str>,
        risk_level: Option<RiskLevel>,
        limit: usize,
    ) -> Vec<serde_json::Value> {
        let conn = self.conn.lock().unwrap();
        let mut sql = "SELECT id, timestamp, session_id, action, risk_level, decision, decided_by, result, error_msg
                       FROM audit_log WHERE 1=1".to_string();
        let mut bind_values: Vec<String> = Vec::new();

        if let Some(sid) = session_id {
            sql.push_str(" AND session_id = ?");
            bind_values.push(sid.to_string());
        }
        if let Some(rl) = risk_level {
            sql.push_str(" AND risk_level = ?");
            bind_values.push(rl.as_str().to_string());
        }
        sql.push_str(" ORDER BY timestamp DESC LIMIT ?");
        bind_values.push(limit.to_string());

        let mut stmt = conn.prepare(&sql).unwrap();
        let param_refs: Vec<&dyn rusqlite::types::ToSql> = bind_values
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();

        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, i64>(0)?,
                "timestamp": row.get::<_, String>(1)?,
                "session_id": row.get::<_, String>(2)?,
                "action": row.get::<_, String>(3)?,
                "risk_level": row.get::<_, String>(4)?,
                "decision": row.get::<_, String>(5)?,
                "decided_by": row.get::<_, String>(6)?,
                "result": row.get::<_, Option<String>>(7)?,
                "error_msg": row.get::<_, Option<String>>(8)?,
            }))
        }).unwrap();

        rows.filter_map(|r| r.ok()).collect()
    }

    /// Count entries per risk level (for dashboard stats).
    pub fn stats(&self) -> serde_json::Value {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT risk_level, COUNT(*) FROM audit_log GROUP BY risk_level"
        ).unwrap();
        let rows: Vec<_> = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        }).unwrap().filter_map(|r| r.ok()).collect();

        let mut m = serde_json::Map::new();
        for (level, count) in rows {
            m.insert(level, serde_json::Value::Number(count.into()));
        }
        serde_json::Value::Object(m)
    }
}
