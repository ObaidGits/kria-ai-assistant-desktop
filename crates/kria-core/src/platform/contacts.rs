/// SQLite-backed contact resolver with NFC normalization, diacritic folding,
/// case-folding, and Double Metaphone phonetic matching.
///
/// # Matching pipeline (in priority order)
/// 1. NFC normalization + case-fold + diacritic-fold (via `deunicode`)
/// 2. Exact match on normalized name
/// 3. Prefix match on normalized name
/// 4. Double Metaphone phonetic match
///
/// Steps 2-4 produce scored candidates. If a single candidate has confidence ≥ 0.95
/// AND the second-best is < 0.5, it is auto-resolved. Otherwise `Ambiguous` is returned.
///
/// # Storage
/// Contacts are stored in K.R.I.A.'s SQLite database under a `contacts` table.
/// The user populates this via import or manual entry — no automatic harvesting
/// of system address books (which may require additional OS permissions).
use std::sync::Arc;

use rusqlite::{Connection, Result as SqlResult};
use tokio::sync::Mutex;
use tracing::{debug, warn};
use unicode_normalization::UnicodeNormalization;

use crate::platform::intent::resolution::{
    Candidate, ContactId, ContactResolver, MessagingApp, ResolutionError,
};

// ─── ContactRecord ────────────────────────────────────────────────────────────

struct ContactRecord {
    display_name: String,
    phone_e164: Option<String>,
    email: Option<String>,
    telegram_handle: Option<String>,
    signal_phone: Option<String>,
}

// ─── Normalization ────────────────────────────────────────────────────────────

/// NFC normalize → lowercase → strip diacritics via ASCII transliteration.
fn normalize(s: &str) -> String {
    // Step 1: NFD decomposition splits "á" → 'a' + combining accent.
    // Step 2: Lowercase.
    // Step 3: Filter out combining/diacritic code-points (General_Category=Mn)
    //         and keep only ASCII alphanumeric + whitespace.
    s.nfd()
        .flat_map(|c| c.to_lowercase())
        .filter(|c| c.is_ascii_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

// ─── Double Metaphone ─────────────────────────────────────────────────────────
//
// Minimal Double Metaphone implementation sufficient for South Asian / English names.
// A production implementation would use the `rphonetic` crate; this inline version
// avoids a new dependency and covers the "Anjali"/"Anjli"/"Anjaly" cases.

fn double_metaphone_primary(name: &str) -> String {
    // Map the first letter and common patterns.
    // This is intentionally minimal — covers the ambiguity cases in the plan.
    let n = normalize(name);
    let chars: Vec<char> = n.chars().collect();
    let mut result = String::new();

    for (i, &ch) in chars.iter().enumerate() {
        let prev = if i > 0 { chars.get(i - 1).copied() } else { None };
        let next = chars.get(i + 1).copied();

        match ch {
            'a' | 'e' | 'i' | 'o' | 'u' => {
                if i == 0 {
                    result.push('A');
                }
            }
            'b' => {
                if prev != Some('m') {
                    result.push('P');
                }
            }
            'c' => {
                if next == Some('h') {
                    result.push('X');
                } else if next == Some('i') || next == Some('e') || next == Some('y') {
                    result.push('S');
                } else {
                    result.push('K');
                }
            }
            'd' => {
                if next == Some('g') {
                    result.push('J');
                } else {
                    result.push('T');
                }
            }
            'f' => result.push('F'),
            'g' => {
                if next == Some('n') {
                    // silent G
                } else if next == Some('i') || next == Some('e') || next == Some('y') {
                    result.push('J');
                } else {
                    result.push('K');
                }
            }
            'h' => {
                if next.map(|c| "aeiou".contains(c)).unwrap_or(false) {
                    result.push('H');
                }
            }
            'j' => result.push('J'),
            'k' => {
                if prev != Some('c') {
                    result.push('K');
                }
            }
            'l' => result.push('L'),
            'm' => result.push('M'),
            'n' => result.push('N'),
            'p' => {
                if next == Some('h') {
                    result.push('F');
                } else {
                    result.push('P');
                }
            }
            'q' => result.push('K'),
            'r' => result.push('R'),
            's' => {
                if next == Some('h') || (next == Some('i') && chars.get(i + 2) == Some(&'o')) {
                    result.push('X');
                } else {
                    result.push('S');
                }
            }
            't' => {
                if next == Some('h') {
                    result.push('0'); // theta
                } else {
                    result.push('T');
                }
            }
            'v' => result.push('F'),
            'w' => {
                if next.map(|c| "aeiou".contains(c)).unwrap_or(false) {
                    result.push('W');
                }
            }
            'x' => result.push_str("KS"),
            'y' => {
                if next.map(|c| "aeiou".contains(c)).unwrap_or(false) {
                    result.push('Y');
                }
            }
            'z' => result.push('S'),
            _ => {}
        }
    }

    result
}

fn phonetic_similarity(a: &str, b: &str) -> f32 {
    let pa = double_metaphone_primary(a);
    let pb = double_metaphone_primary(b);
    if pa == pb {
        return 0.8;
    }
    // Partial match: check if one is prefix of other.
    if pa.starts_with(&pb) || pb.starts_with(&pa) {
        let min_len = pa.len().min(pb.len()) as f32;
        let max_len = pa.len().max(pb.len()) as f32;
        return 0.6 * (min_len / max_len);
    }
    0.0
}

// ─── ContactsDb ──────────────────────────────────────────────────────────────

/// SQLite-backed `ContactResolver`.
pub struct ContactsDb {
    conn: Arc<Mutex<Connection>>,
}

impl ContactsDb {
    /// Open or create the contacts database at the given path.
    pub fn open(db_path: &std::path::Path) -> SqlResult<Self> {
        let conn = Connection::open(db_path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS contacts (
                id          INTEGER PRIMARY KEY,
                display_name TEXT NOT NULL,
                phone_e164   TEXT,
                email        TEXT,
                telegram     TEXT,
                signal_phone TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_contacts_name ON contacts (display_name COLLATE NOCASE);",
        )?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Insert a contact record (for tests / import).
    pub async fn insert(
        &self,
        display_name: &str,
        phone_e164: Option<&str>,
        email: Option<&str>,
        telegram: Option<&str>,
        signal_phone: Option<&str>,
    ) -> SqlResult<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO contacts (display_name, phone_e164, email, telegram, signal_phone)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![display_name, phone_e164, email, telegram, signal_phone],
        )?;
        Ok(())
    }

    async fn fetch_all(&self) -> Vec<ContactRecord> {
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare(
            "SELECT display_name, phone_e164, email, telegram, signal_phone FROM contacts",
        ) {
            Ok(s) => s,
            Err(e) => {
                warn!("failed to prepare contacts query: {e}");
                return Vec::new();
            }
        };

        let records = match stmt.query_map([], |row| {
                Ok(ContactRecord {
                    display_name: row.get(0)?,
                    phone_e164: row.get(1)?,
                    email: row.get(2)?,
                    telegram_handle: row.get(3)?,
                    signal_phone: row.get(4)?,
                })
            }) {
                Ok(rows) => rows.flatten().collect::<Vec<_>>(),
                Err(e) => {
                    warn!("failed to query contacts: {e}");
                    Vec::new()
                }
            };

        records
    }

    fn score_record(&self, record: &ContactRecord, normalized_query: &str) -> (f32, String) {
        let normalized_name = normalize(&record.display_name);

        // Exact match.
        if normalized_name == normalized_query {
            return (1.0, "exact name match".to_string());
        }

        // Prefix match (e.g., "Anjali" matches "Anjali Sharma").
        if normalized_name.starts_with(normalized_query)
            || normalized_query.starts_with(&normalized_name)
        {
            let coverage = normalized_query.len().min(normalized_name.len()) as f32
                / normalized_query.len().max(normalized_name.len()) as f32;
            return (0.7 + 0.15 * coverage, "prefix name match".to_string());
        }

        // Check individual words in the name (first-name match).
        for word in normalized_name.split_whitespace() {
            if word == normalized_query {
                return (0.75, format!("first/last name match: '{word}'"));
            }
        }

        // Phonetic match.
        let phonetic = phonetic_similarity(&record.display_name, normalized_query);
        if phonetic > 0.0 {
            return (
                phonetic,
                format!(
                    "phonetic match: '{}' ≈ '{}'",
                    double_metaphone_primary(&record.display_name),
                    double_metaphone_primary(normalized_query)
                ),
            );
        }

        (0.0, "no match".to_string())
    }

    fn get_identifier(record: &ContactRecord, app: &MessagingApp) -> Option<String> {
        match app {
            MessagingApp::WhatsApp | MessagingApp::Signal => {
                record.phone_e164.clone().or_else(|| record.signal_phone.clone())
            }
            MessagingApp::Gmail => record.email.clone(),
            MessagingApp::Telegram => record.telegram_handle.clone().or_else(|| record.phone_e164.clone()),
        }
    }

    fn identifier_field_name(app: &MessagingApp) -> &'static str {
        match app {
            MessagingApp::WhatsApp => "phone number",
            MessagingApp::Signal => "signal phone / phone number",
            MessagingApp::Gmail => "email address",
            MessagingApp::Telegram => "telegram handle / phone number",
        }
    }
}

#[async_trait::async_trait]
impl ContactResolver for ContactsDb {
    async fn resolve(
        &self,
        name: &str,
        app: &MessagingApp,
    ) -> Result<ContactId, ResolutionError> {
        let query = normalize(name);
        let all_contacts = self.fetch_all().await;

        // Score every contact.
        let mut scored: Vec<(f32, String, &ContactRecord)> = all_contacts
            .iter()
            .map(|record| {
                let (score, reason) = self.score_record(record, &query);
                (score, reason, record)
            })
            .filter(|(score, _, _)| *score > 0.0)
            .collect();

        // Sort descending by confidence.
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        // Take top 3.
        scored.truncate(3);
        debug!(
            query = name,
            candidate_count = scored.len(),
            "contact resolution candidates"
        );

        match scored.as_slice() {
            [] => Err(ResolutionError::NotFound {
                query: name.to_string(),
            }),
            [(_top_score, _reason, record)] => {
                // Single candidate — check identifier availability.
                match Self::get_identifier(record, app) {
                    None => Err(ResolutionError::Incomplete {
                        name: record.display_name.clone(),
                        field: Self::identifier_field_name(app).to_string(),
                        app: app.clone(),
                    }),
                    Some(identifier) => Ok(ContactId {
                        display_name: record.display_name.clone(),
                        identifier,
                        app: app.clone(),
                    }),
                }
            }
            candidates => {
                let top = &candidates[0];
                let second = &candidates[1];

                // Auto-resolve only when top is very high confidence AND second is low.
                let auto_resolve = top.0 >= 0.95 && second.0 < 0.5;

                if auto_resolve {
                    let record = top.2;
                    match Self::get_identifier(record, app) {
                        None => Err(ResolutionError::Incomplete {
                            name: record.display_name.clone(),
                            field: Self::identifier_field_name(app).to_string(),
                            app: app.clone(),
                        }),
                        Some(identifier) => Ok(ContactId {
                            display_name: record.display_name.clone(),
                            identifier,
                            app: app.clone(),
                        }),
                    }
                } else {
                    // Ambiguous — return top-3 for the LLM to surface to the user.
                    let candidate_list: Vec<Candidate> = candidates
                        .iter()
                        .filter_map(|(score, reason, record)| {
                            let identifier = Self::get_identifier(record, app)?;
                            Some(Candidate {
                                contact_id: ContactId {
                                    display_name: record.display_name.clone(),
                                    identifier,
                                    app: app.clone(),
                                },
                                confidence: *score,
                                match_reason: reason.clone(),
                            })
                        })
                        .collect();

                    if candidate_list.is_empty() {
                        // All matches lack the required identifier.
                        return Err(ResolutionError::Incomplete {
                            name: name.to_string(),
                            field: Self::identifier_field_name(app).to_string(),
                            app: app.clone(),
                        });
                    }

                    Err(ResolutionError::ambiguous(name, candidate_list))
                }
            }
        }
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    /// Returns both the db and the temp file handle; the file is deleted when
    /// the handle is dropped, so tests must keep it alive for the db lifetime.
    async fn make_db() -> (ContactsDb, NamedTempFile) {
        let tmp = NamedTempFile::new().unwrap();
        let db = ContactsDb::open(tmp.path()).unwrap();
        (db, tmp)
    }

    #[tokio::test]
    async fn exact_match_resolves() {
        let (db, _tmp) = make_db().await;
        db.insert(
            "Anjali Sharma",
            Some("+919876543210"),
            None,
            None,
            None,
        )
        .await
        .unwrap();

        let result = db.resolve("Anjali Sharma", &MessagingApp::WhatsApp).await;
        assert!(result.is_ok());
        let contact = result.unwrap();
        assert_eq!(contact.display_name, "Anjali Sharma");
        assert_eq!(contact.identifier, "+919876543210");
    }

    #[tokio::test]
    async fn ambiguous_returns_error_with_candidates() {
        let (db, _tmp) = make_db().await;
        db.insert("Anjali Sharma", Some("+919876543210"), None, None, None)
            .await
            .unwrap();
        db.insert("Anjali Verma", Some("+919123456789"), None, None, None)
            .await
            .unwrap();

        let result = db.resolve("Anjali", &MessagingApp::WhatsApp).await;
        match result {
            Err(ResolutionError::Ambiguous { candidates, .. }) => {
                assert!(candidates.len() >= 2, "should have at least 2 candidates");
            }
            other => panic!("expected Ambiguous, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn not_found_returns_error() {
        let (db, _tmp) = make_db().await;
        let result = db.resolve("Rajan Kumar", &MessagingApp::WhatsApp).await;
        assert!(matches!(result, Err(ResolutionError::NotFound { .. })));
    }

    #[tokio::test]
    async fn incomplete_when_no_phone() {
        let (db, _tmp) = make_db().await;
        db.insert("Bob NoPhone", None, Some("bob@example.com"), None, None)
            .await
            .unwrap();
        let result = db.resolve("Bob NoPhone", &MessagingApp::WhatsApp).await;
        assert!(matches!(result, Err(ResolutionError::Incomplete { .. })));
    }

    #[test]
    fn normalize_strips_diacritics() {
        assert_eq!(normalize("Anjáli"), "anjali");
        assert_eq!(normalize("Müller"), "muller");
        assert_eq!(normalize("  Anjali  Sharma  "), "anjali sharma");
    }

    #[test]
    fn phonetic_anjali_variants() {
        let variants = ["Anjli", "Anjaly", "Anjali"];
        let base = double_metaphone_primary("Anjali");
        for v in &variants {
            let p = double_metaphone_primary(v);
            let sim = phonetic_similarity("Anjali", v);
            println!("Anjali={base} vs {v}={p} sim={sim}");
            assert!(sim > 0.0, "expected phonetic similarity for '{v}'");
        }
    }
}
