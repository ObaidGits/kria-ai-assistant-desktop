use crate::safety::RiskLevel;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::sync::Mutex;

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
                network_url TEXT,
                prev_hash   TEXT,
                row_hash    TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_audit_timestamp ON audit_log(timestamp);
            CREATE INDEX IF NOT EXISTS idx_audit_session   ON audit_log(session_id);
            CREATE INDEX IF NOT EXISTS idx_audit_risk      ON audit_log(risk_level);
            CREATE INDEX IF NOT EXISTS idx_audit_action    ON audit_log(action);"
        ).expect("failed to create audit_log table");

        // Hash-chain migration: silently add columns if they don't exist yet
        // (for databases created before this version). SQLite does NOT support
        // `ALTER TABLE … ADD COLUMN IF NOT EXISTS`, so we probe pragma_table_info.
        let has_column = |name: &str| -> bool {
            conn.prepare("SELECT 1 FROM pragma_table_info('audit_log') WHERE name = ?1")
                .and_then(|mut stmt| stmt.exists([name]))
                .unwrap_or(false)
        };
        if !has_column("prev_hash") {
            let _ = conn.execute("ALTER TABLE audit_log ADD COLUMN prev_hash TEXT", []);
        }
        if !has_column("row_hash") {
            let _ = conn.execute("ALTER TABLE audit_log ADD COLUMN row_hash TEXT", []);
        }

        Self {
            conn: Mutex::new(conn),
        }
    }

    /// Log a tool action with its policy decision.
    ///
    /// Each row is hash-chained: `row_hash = SHA-256(prev_hash || timestamp || session_id || action || parameters || decision)`.
    /// The first row in the log has `prev_hash = "GENESIS"`.
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

        // Fetch the hash of the previous row to chain to.
        let prev_hash: String = conn
            .query_row(
                "SELECT COALESCE(row_hash, 'GENESIS') FROM audit_log ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap_or_else(|_| "GENESIS".to_string());

        // Timestamp for this row (use the same value we'll insert).
        let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        let params_str = parameters.to_string();

        // Compute row_hash = SHA-256(prev_hash || timestamp || session_id || action || params || decision).
        let row_hash = {
            let mut h = Sha256::new();
            h.update(prev_hash.as_bytes());
            h.update(b"|");
            h.update(timestamp.as_bytes());
            h.update(b"|");
            h.update(session_id.as_bytes());
            h.update(b"|");
            h.update(action.as_bytes());
            h.update(b"|");
            h.update(params_str.as_bytes());
            h.update(b"|");
            h.update(decision.as_str().as_bytes());
            hex::encode(h.finalize())
        };

        let _ = conn.execute(
            "INSERT INTO audit_log (timestamp, session_id, action, parameters, risk_level, decision, decided_by, prev_hash, row_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                timestamp,
                session_id,
                action,
                params_str,
                risk_level.as_str(),
                decision.as_str(),
                decided_by.as_str(),
                prev_hash,
                row_hash,
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

        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
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
            })
            .unwrap();

        rows.filter_map(|r| r.ok()).collect()
    }

    /// Count entries per risk level (for dashboard stats).
    pub fn stats(&self) -> serde_json::Value {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT risk_level, COUNT(*) FROM audit_log GROUP BY risk_level")
            .unwrap();
        let rows: Vec<_> = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        let mut m = serde_json::Map::new();
        for (level, count) in rows {
            m.insert(level, serde_json::Value::Number(count.into()));
        }
        serde_json::Value::Object(m)
    }

    /// Verify the hash-chain integrity of the entire audit log.
    ///
    /// Walks every row in insertion order and recomputes `row_hash`.
    /// Returns `Ok(rows_verified)` on success, or `Err(first_broken_id)` on tamper detection.
    pub fn verify_chain(&self) -> Result<usize, i64> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, timestamp, session_id, action, parameters, decision, prev_hash, row_hash \
                 FROM audit_log ORDER BY id ASC",
            )
            .map_err(|_| -1i64)?;

        struct Row {
            id: i64,
            timestamp: String,
            session_id: String,
            action: String,
            parameters: String,
            decision: String,
            prev_hash: String,
            row_hash: String,
        }

        let rows: Vec<Row> = stmt
            .query_map([], |r| {
                Ok(Row {
                    id: r.get(0)?,
                    timestamp: r.get(1)?,
                    session_id: r.get(2)?,
                    action: r.get(3)?,
                    parameters: r.get(4)?,
                    decision: r.get(5)?,
                    prev_hash: r.get::<_, Option<String>>(6)?.unwrap_or_else(|| "GENESIS".into()),
                    row_hash: r.get::<_, Option<String>>(7)?.unwrap_or_default(),
                })
            })
            .map_err(|_| -1i64)?
            .filter_map(|r| r.ok())
            .collect();

        let mut count = 0;
        for row in &rows {
            // Rows without a hash were logged by the pre-hash-chain version — skip.
            if row.row_hash.is_empty() {
                count += 1;
                continue;
            }

            let mut h = Sha256::new();
            h.update(row.prev_hash.as_bytes());
            h.update(b"|");
            h.update(row.timestamp.as_bytes());
            h.update(b"|");
            h.update(row.session_id.as_bytes());
            h.update(b"|");
            h.update(row.action.as_bytes());
            h.update(b"|");
            h.update(row.parameters.as_bytes());
            h.update(b"|");
            h.update(row.decision.as_bytes());
            let expected = hex::encode(h.finalize());

            if expected != row.row_hash {
                return Err(row.id);
            }
            count += 1;
        }
        Ok(count)
    }
}
