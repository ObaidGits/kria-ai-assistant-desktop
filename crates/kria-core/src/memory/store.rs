use rusqlite::{Connection, params};
use std::path::Path;
use std::sync::Mutex;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Central SQLite storage for conversations, facts, preferences, audit log.
pub struct MemoryStore {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationTurn {
    pub id: Option<i64>,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub tool_name: Option<String>,
    pub tool_result: Option<String>,
    pub tokens_used: Option<i32>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryFact {
    pub id: Option<i64>,
    pub text: String,
    pub category: String,
    pub source: String,
    pub created_at: DateTime<Utc>,
    pub last_accessed: DateTime<Utc>,
    pub access_count: i32,
    pub decay_score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryLink {
    pub fact_a_id: i64,
    pub fact_b_id: i64,
    pub relation_type: String,
    pub strength: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: Option<i64>,
    pub session_id: String,
    pub action: String,
    pub parameters: String,
    pub risk_level: String,
    pub decision: String,
    pub decided_by: String,
    pub result: Option<String>,
    pub error_msg: Option<String>,
    pub rollback_id: Option<String>,
    pub duration_ms: Option<i64>,
    pub timestamp: DateTime<Utc>,
}

impl MemoryStore {
    /// Open (or create) the SQLite database and run migrations.
    pub fn open(db_path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open(db_path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

        let store = Self {
            conn: Mutex::new(conn),
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS conversations (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id  TEXT NOT NULL,
                role        TEXT NOT NULL,
                content     TEXT NOT NULL,
                tool_name   TEXT,
                tool_result TEXT,
                tokens_used INTEGER,
                timestamp   TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_conv_session ON conversations(session_id);

            CREATE TABLE IF NOT EXISTS memory_facts (
                id             INTEGER PRIMARY KEY AUTOINCREMENT,
                text           TEXT NOT NULL,
                category       TEXT NOT NULL DEFAULT 'general',
                source         TEXT NOT NULL DEFAULT 'inferred',
                created_at     TEXT NOT NULL DEFAULT (datetime('now')),
                last_accessed  TEXT NOT NULL DEFAULT (datetime('now')),
                access_count   INTEGER NOT NULL DEFAULT 0,
                decay_score    REAL NOT NULL DEFAULT 1.0
            );

            CREATE TABLE IF NOT EXISTS memory_links (
                fact_a_id      INTEGER NOT NULL REFERENCES memory_facts(id) ON DELETE CASCADE,
                fact_b_id      INTEGER NOT NULL REFERENCES memory_facts(id) ON DELETE CASCADE,
                relation_type  TEXT NOT NULL DEFAULT 'related',
                strength       REAL NOT NULL DEFAULT 0.5,
                PRIMARY KEY (fact_a_id, fact_b_id)
            );

            CREATE TABLE IF NOT EXISTS preferences (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS audit_log (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id  TEXT NOT NULL,
                action      TEXT NOT NULL,
                parameters  TEXT,
                risk_level  TEXT NOT NULL,
                decision    TEXT NOT NULL,
                decided_by  TEXT NOT NULL DEFAULT 'system',
                result      TEXT,
                error_msg   TEXT,
                rollback_id TEXT,
                duration_ms INTEGER,
                timestamp   TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_audit_session ON audit_log(session_id);

            CREATE TABLE IF NOT EXISTS snippets (
                name     TEXT PRIMARY KEY,
                content  TEXT NOT NULL,
                language TEXT NOT NULL DEFAULT 'text',
                tags     TEXT NOT NULL DEFAULT '[]',
                created  TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS conversations_fts USING fts5(
                content, content=conversations, content_rowid=id
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS facts_fts USING fts5(
                text, content=memory_facts, content_rowid=id
            );
            "
        )?;
        Ok(())
    }

    // ── Conversations ───────────────────────────────────────────────

    pub fn store_turn(&self, turn: &ConversationTurn) -> anyhow::Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO conversations (session_id, role, content, tool_name, tool_result, tokens_used, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                turn.session_id,
                turn.role,
                turn.content,
                turn.tool_name,
                turn.tool_result,
                turn.tokens_used,
                turn.timestamp.to_rfc3339(),
            ],
        )?;
        let id = conn.last_insert_rowid();
        // Update FTS index
        conn.execute(
            "INSERT INTO conversations_fts(rowid, content) VALUES (?1, ?2)",
            params![id, turn.content],
        )?;
        Ok(id)
    }

    pub fn get_recent_turns(&self, session_id: &str, limit: usize) -> anyhow::Result<Vec<ConversationTurn>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, role, content, tool_name, tool_result, tokens_used, timestamp
             FROM conversations WHERE session_id = ?1 ORDER BY id DESC LIMIT ?2"
        )?;
        let turns = stmt.query_map(params![session_id, limit as i64], |row| {
            Ok(ConversationTurn {
                id: Some(row.get(0)?),
                session_id: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                tool_name: row.get(4)?,
                tool_result: row.get(5)?,
                tokens_used: row.get(6)?,
                timestamp: row.get::<_, String>(7)
                    .map(|s| DateTime::parse_from_rfc3339(&s)
                        .map(|d| d.with_timezone(&Utc))
                        .unwrap_or_else(|_| Utc::now()))?,
            })
        })?.collect::<Result<Vec<_>, _>>()?;
        Ok(turns.into_iter().rev().collect())
    }

    pub fn list_sessions(&self) -> anyhow::Result<Vec<(String, i64, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT session_id, COUNT(*) as turns, MAX(timestamp) as last_active
             FROM conversations GROUP BY session_id ORDER BY last_active DESC"
        )?;
        let sessions = stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?.collect::<Result<Vec<_>, _>>()?;
        Ok(sessions)
    }

    pub fn delete_session(&self, session_id: &str) -> anyhow::Result<usize> {
        let conn = self.conn.lock().unwrap();
        let deleted = conn.execute(
            "DELETE FROM conversations WHERE session_id = ?1",
            params![session_id],
        )?;
        Ok(deleted)
    }

    pub fn search_conversations(&self, query: &str, limit: usize) -> anyhow::Result<Vec<ConversationTurn>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT c.id, c.session_id, c.role, c.content, c.tool_name, c.tool_result, c.tokens_used, c.timestamp
             FROM conversations_fts f JOIN conversations c ON f.rowid = c.id
             WHERE conversations_fts MATCH ?1 ORDER BY rank LIMIT ?2"
        )?;
        let turns = stmt.query_map(params![query, limit as i64], |row| {
            Ok(ConversationTurn {
                id: Some(row.get(0)?),
                session_id: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                tool_name: row.get(4)?,
                tool_result: row.get(5)?,
                tokens_used: row.get(6)?,
                timestamp: row.get::<_, String>(7)
                    .map(|s| DateTime::parse_from_rfc3339(&s)
                        .map(|d| d.with_timezone(&Utc))
                        .unwrap_or_else(|_| Utc::now()))?,
            })
        })?.collect::<Result<Vec<_>, _>>()?;
        Ok(turns)
    }

    // ── Facts ───────────────────────────────────────────────────────

    pub fn store_fact(&self, fact: &MemoryFact) -> anyhow::Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO memory_facts (text, category, source, created_at, last_accessed, access_count, decay_score)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                fact.text, fact.category, fact.source,
                fact.created_at.to_rfc3339(), fact.last_accessed.to_rfc3339(),
                fact.access_count, fact.decay_score,
            ],
        )?;
        let id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO facts_fts(rowid, text) VALUES (?1, ?2)",
            params![id, fact.text],
        )?;
        Ok(id)
    }

    pub fn get_fact(&self, id: i64) -> anyhow::Result<Option<MemoryFact>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, text, category, source, created_at, last_accessed, access_count, decay_score
             FROM memory_facts WHERE id = ?1"
        )?;
        let mut rows = stmt.query_map(params![id], |row| {
            Ok(MemoryFact {
                id: Some(row.get(0)?),
                text: row.get(1)?,
                category: row.get(2)?,
                source: row.get(3)?,
                created_at: parse_dt(row.get::<_, String>(4)?),
                last_accessed: parse_dt(row.get::<_, String>(5)?),
                access_count: row.get(6)?,
                decay_score: row.get(7)?,
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    pub fn search_facts(&self, query: &str, limit: usize) -> anyhow::Result<Vec<MemoryFact>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT f.id, f.text, f.category, f.source, f.created_at, f.last_accessed, f.access_count, f.decay_score
             FROM facts_fts fts JOIN memory_facts f ON fts.rowid = f.id
             WHERE facts_fts MATCH ?1 ORDER BY rank LIMIT ?2"
        )?;
        let facts = stmt.query_map(params![query, limit as i64], |row| {
            Ok(MemoryFact {
                id: Some(row.get(0)?),
                text: row.get(1)?,
                category: row.get(2)?,
                source: row.get(3)?,
                created_at: parse_dt(row.get::<_, String>(4)?),
                last_accessed: parse_dt(row.get::<_, String>(5)?),
                access_count: row.get(6)?,
                decay_score: row.get(7)?,
            })
        })?.collect::<Result<Vec<_>, _>>()?;
        Ok(facts)
    }

    pub fn all_facts_with_decay(&self, min_score: f64) -> anyhow::Result<Vec<MemoryFact>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, text, category, source, created_at, last_accessed, access_count, decay_score
             FROM memory_facts WHERE decay_score >= ?1 ORDER BY decay_score DESC"
        )?;
        let facts = stmt.query_map(params![min_score], |row| {
            Ok(MemoryFact {
                id: Some(row.get(0)?),
                text: row.get(1)?,
                category: row.get(2)?,
                source: row.get(3)?,
                created_at: parse_dt(row.get::<_, String>(4)?),
                last_accessed: parse_dt(row.get::<_, String>(5)?),
                access_count: row.get(6)?,
                decay_score: row.get(7)?,
            })
        })?.collect::<Result<Vec<_>, _>>()?;
        Ok(facts)
    }

    pub fn update_fact_access(&self, id: i64) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE memory_facts SET access_count = access_count + 1, last_accessed = datetime('now') WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    pub fn update_fact_decay(&self, id: i64, new_score: f64) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE memory_facts SET decay_score = ?2 WHERE id = ?1",
            params![id, new_score],
        )?;
        Ok(())
    }

    pub fn delete_fact(&self, id: i64) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM memory_facts WHERE id = ?1", params![id])?;
        Ok(())
    }

    // ── Links ───────────────────────────────────────────────────────

    pub fn store_link(&self, link: &MemoryLink) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO memory_links (fact_a_id, fact_b_id, relation_type, strength) VALUES (?1, ?2, ?3, ?4)",
            params![link.fact_a_id, link.fact_b_id, link.relation_type, link.strength],
        )?;
        Ok(())
    }

    pub fn get_links(&self, fact_id: i64) -> anyhow::Result<Vec<MemoryLink>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT fact_a_id, fact_b_id, relation_type, strength FROM memory_links
             WHERE fact_a_id = ?1 OR fact_b_id = ?1"
        )?;
        let links = stmt.query_map(params![fact_id], |row| {
            Ok(MemoryLink {
                fact_a_id: row.get(0)?,
                fact_b_id: row.get(1)?,
                relation_type: row.get(2)?,
                strength: row.get(3)?,
            })
        })?.collect::<Result<Vec<_>, _>>()?;
        Ok(links)
    }

    // ── Preferences ─────────────────────────────────────────────────

    pub fn set_preference(&self, key: &str, value: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO preferences (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_preference(&self, key: &str) -> anyhow::Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT value FROM preferences WHERE key = ?1")?;
        let mut rows = stmt.query_map(params![key], |row| row.get(0))?;
        Ok(rows.next().transpose()?)
    }

    pub fn list_preferences(&self) -> anyhow::Result<Vec<(String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT key, value FROM preferences ORDER BY key")?;
        let prefs = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
        Ok(prefs)
    }

    // ── Audit ───────────────────────────────────────────────────────

    pub fn log_audit(&self, entry: &AuditEntry) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO audit_log (session_id, action, parameters, risk_level, decision, decided_by, result, error_msg, rollback_id, duration_ms, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                entry.session_id, entry.action, entry.parameters,
                entry.risk_level, entry.decision, entry.decided_by,
                entry.result, entry.error_msg, entry.rollback_id,
                entry.duration_ms, entry.timestamp.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn query_audit(&self, limit: usize, risk_level: Option<&str>, session_id: Option<&str>) -> anyhow::Result<Vec<AuditEntry>> {
        let conn = self.conn.lock().unwrap();
        let sql = match (risk_level, session_id) {
            (Some(rl), Some(sid)) =>
                format!("SELECT * FROM audit_log WHERE risk_level = '{}' AND session_id = '{}' ORDER BY id DESC LIMIT {}", rl, sid, limit),
            (Some(rl), None) =>
                format!("SELECT * FROM audit_log WHERE risk_level = '{}' ORDER BY id DESC LIMIT {}", rl, limit),
            (None, Some(sid)) =>
                format!("SELECT * FROM audit_log WHERE session_id = '{}' ORDER BY id DESC LIMIT {}", sid, limit),
            (None, None) =>
                format!("SELECT * FROM audit_log ORDER BY id DESC LIMIT {}", limit),
        };
        let mut stmt = conn.prepare(&sql)?;
        let entries = stmt.query_map([], |row| {
            Ok(AuditEntry {
                id: Some(row.get(0)?),
                session_id: row.get(1)?,
                action: row.get(2)?,
                parameters: row.get(3)?,
                risk_level: row.get(4)?,
                decision: row.get(5)?,
                decided_by: row.get(6)?,
                result: row.get(7)?,
                error_msg: row.get(8)?,
                rollback_id: row.get(9)?,
                duration_ms: row.get(10)?,
                timestamp: parse_dt(row.get::<_, String>(11)?),
            })
        })?.collect::<Result<Vec<_>, _>>()?;
        Ok(entries)
    }

    // ── Snippets ────────────────────────────────────────────────────

    pub fn save_snippet(&self, name: &str, content: &str, language: &str, tags: &[String]) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        let tags_json = serde_json::to_string(tags)?;
        conn.execute(
            "INSERT OR REPLACE INTO snippets (name, content, language, tags) VALUES (?1, ?2, ?3, ?4)",
            params![name, content, language, tags_json],
        )?;
        Ok(())
    }

    pub fn get_snippet(&self, name: &str) -> anyhow::Result<Option<(String, String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT content, language, tags FROM snippets WHERE name = ?1")?;
        let mut rows = stmt.query_map(params![name], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        Ok(rows.next().transpose()?)
    }

    pub fn list_snippets(&self, tag: Option<&str>) -> anyhow::Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT name, tags FROM snippets ORDER BY name")?;
        let all: Vec<(String, String)> = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
        let filtered: Vec<String> = all.into_iter().filter(|(_, tags_json)| {
            match tag {
                Some(t) => tags_json.contains(t),
                None => true,
            }
        }).map(|(name, _)| name).collect();
        Ok(filtered)
    }

    pub fn delete_snippet(&self, name: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM snippets WHERE name = ?1", params![name])?;
        Ok(())
    }
}

fn parse_dt(s: String) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&s)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}
