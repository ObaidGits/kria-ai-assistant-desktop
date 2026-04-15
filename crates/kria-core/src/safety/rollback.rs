use std::path::{Path, PathBuf};
use chrono::Utc;
use sha2::{Sha256, Digest};

/// Manifest for a rollback snapshot.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RollbackManifest {
    pub timestamp: String,
    pub session_id: String,
    pub action: String,
    pub risk_level: String,
    pub changes: Vec<ChangeRecord>,
    pub rollback_command: String,
    pub expires: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChangeRecord {
    pub change_type: String,
    pub original_path: String,
    pub backup_path: String,
    pub hash_sha256: String,
}

/// Manages rollback snapshots for reversible safety.
///
/// Layout: {rollback_dir}/{timestamp}/manifest.json + files/
pub struct RollbackManager {
    rollback_dir: PathBuf,
    retention_hours: u64,
    max_storage_bytes: u64,
}

impl RollbackManager {
    pub fn new(rollback_dir: PathBuf, retention_hours: u64, max_storage_mb: u64) -> Self {
        let _ = std::fs::create_dir_all(&rollback_dir);
        Self {
            rollback_dir,
            retention_hours,
            max_storage_bytes: max_storage_mb * 1024 * 1024,
        }
    }

    /// Create a restore point before a destructive action.
    /// Returns the rollback ID (timestamp string).
    pub fn create_snapshot(
        &self,
        session_id: &str,
        action: &str,
        risk_level: &str,
        files_to_backup: &[&Path],
    ) -> anyhow::Result<String> {
        let ts = Utc::now().format("%Y-%m-%dT%H-%M-%S").to_string();
        let snapshot_dir = self.rollback_dir.join(&ts);
        let files_dir = snapshot_dir.join("files");
        std::fs::create_dir_all(&files_dir)?;

        let mut changes = Vec::new();

        for path in files_to_backup {
            if !path.exists() {
                continue;
            }
            let filename = path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            // Copy file to backup
            let backup_name = format!("{}_{}", changes.len(), &filename);
            let backup_path = files_dir.join(&backup_name);
            std::fs::copy(path, &backup_path)?;

            // Compute hash
            let data = std::fs::read(path)?;
            let mut hasher = Sha256::new();
            hasher.update(&data);
            let hash = format!("{:x}", hasher.finalize());

            changes.push(ChangeRecord {
                change_type: "file_backup".into(),
                original_path: path.to_string_lossy().into(),
                backup_path: backup_name,
                hash_sha256: hash,
            });
        }

        let expires = Utc::now() + chrono::Duration::hours(self.retention_hours as i64);
        let manifest = RollbackManifest {
            timestamp: ts.clone(),
            session_id: session_id.to_string(),
            action: action.to_string(),
            risk_level: risk_level.to_string(),
            changes,
            rollback_command: "restore_files".into(),
            expires: expires.to_rfc3339(),
        };

        let manifest_path = snapshot_dir.join("manifest.json");
        let json = serde_json::to_string_pretty(&manifest)?;
        std::fs::write(&manifest_path, json)?;

        tracing::info!(snapshot = %ts, action, "rollback snapshot created");
        Ok(ts)
    }

    /// Restore files from a snapshot.
    pub fn restore(&self, rollback_id: &str) -> anyhow::Result<Vec<String>> {
        let snapshot_dir = self.rollback_dir.join(rollback_id);
        let manifest_path = snapshot_dir.join("manifest.json");

        if !manifest_path.exists() {
            anyhow::bail!("rollback snapshot not found: {rollback_id}");
        }

        let json = std::fs::read_to_string(&manifest_path)?;
        let manifest: RollbackManifest = serde_json::from_str(&json)?;

        let mut restored = Vec::new();
        let files_dir = snapshot_dir.join("files");

        for change in &manifest.changes {
            let backup = files_dir.join(&change.backup_path);
            let original = Path::new(&change.original_path);

            if backup.exists() {
                // Ensure parent directory exists
                if let Some(parent) = original.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                std::fs::copy(&backup, original)?;
                restored.push(change.original_path.clone());
            }
        }

        tracing::info!(snapshot = %rollback_id, count = restored.len(), "rollback restored");
        Ok(restored)
    }

    /// List all available snapshots.
    pub fn list_snapshots(&self) -> Vec<RollbackManifest> {
        let mut manifests: Vec<RollbackManifest> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.rollback_dir) {
            for entry in entries.flatten() {
                let manifest_path = entry.path().join("manifest.json");
                if manifest_path.exists() {
                    if let Ok(json) = std::fs::read_to_string(&manifest_path) {
                        if let Ok(m) = serde_json::from_str(&json) {
                            manifests.push(m);
                        }
                    }
                }
            }
        }
        manifests.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        manifests
    }

    /// Prune expired snapshots and enforce storage limits.
    pub fn prune(&self) -> anyhow::Result<usize> {
        let now = Utc::now();
        let mut removed = 0;

        if let Ok(entries) = std::fs::read_dir(&self.rollback_dir) {
            let mut snapshots: Vec<_> = entries.flatten().collect();
            snapshots.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

            for entry in &snapshots {
                let manifest_path = entry.path().join("manifest.json");
                if manifest_path.exists() {
                    if let Ok(json) = std::fs::read_to_string(&manifest_path) {
                        if let Ok(m) = serde_json::from_str::<RollbackManifest>(&json) {
                            if let Ok(expires) = chrono::DateTime::parse_from_rfc3339(&m.expires) {
                                if expires < now {
                                    std::fs::remove_dir_all(entry.path())?;
                                    removed += 1;
                                }
                            }
                        }
                    }
                }
            }
        }

        // Enforce storage limit
        let total_size = self.total_size();
        if total_size > self.max_storage_bytes {
            // Remove oldest until under limit
            if let Ok(entries) = std::fs::read_dir(&self.rollback_dir) {
                let mut snapshots: Vec<_> = entries.flatten().collect();
                snapshots.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
                for entry in snapshots {
                    if self.total_size() <= self.max_storage_bytes {
                        break;
                    }
                    std::fs::remove_dir_all(entry.path())?;
                    removed += 1;
                }
            }
        }

        Ok(removed)
    }

    fn total_size(&self) -> u64 {
        walkdir::WalkDir::new(&self.rollback_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.metadata().ok())
            .map(|m| m.len())
            .sum()
    }
}
