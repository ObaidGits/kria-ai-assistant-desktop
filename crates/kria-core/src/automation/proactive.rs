use chrono::{DateTime, Utc};
/// Proactive Intelligence — system health monitoring, file watchers, smart suggestions.
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

/// A proactive notification with category and suggested action.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProactiveAlert {
    pub id: String,
    pub category: AlertCategory,
    pub title: String,
    pub message: String,
    pub suggestion: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub dismissed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertCategory {
    Alert,
    Suggestion,
    Info,
}

/// System health thresholds.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HealthThresholds {
    pub min_disk_pct: f64,
    pub min_ram_mb: u64,
    pub min_battery_pct: u32,
}

impl Default for HealthThresholds {
    fn default() -> Self {
        Self {
            min_disk_pct: 10.0,
            min_ram_mb: 500,
            min_battery_pct: 15,
        }
    }
}

/// Watches configured directories for changes.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WatchedDir {
    pub path: PathBuf,
    pub label: String,
    pub enabled: bool,
}

/// Proactive Intelligence engine — coordinates monitoring and alerts.
pub struct ProactiveEngine {
    alerts: Arc<RwLock<Vec<ProactiveAlert>>>,
    thresholds: HealthThresholds,
    watched_dirs: Arc<RwLock<Vec<WatchedDir>>>,
}

impl ProactiveEngine {
    pub fn new(thresholds: HealthThresholds) -> Self {
        Self {
            alerts: Arc::new(RwLock::new(Vec::new())),
            thresholds,
            watched_dirs: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Add a directory to watch.
    pub async fn watch_dir(&self, path: PathBuf, label: &str) {
        let mut dirs = self.watched_dirs.write().await;
        dirs.push(WatchedDir {
            path,
            label: label.to_string(),
            enabled: true,
        });
    }

    /// Get all undismissed alerts.
    pub async fn get_alerts(&self) -> Vec<ProactiveAlert> {
        self.alerts
            .read()
            .await
            .iter()
            .filter(|a| !a.dismissed)
            .cloned()
            .collect()
    }

    /// Get all alerts including dismissed.
    pub async fn get_all_alerts(&self) -> Vec<ProactiveAlert> {
        self.alerts.read().await.clone()
    }

    /// Dismiss an alert by ID.
    pub async fn dismiss_alert(&self, id: &str) -> bool {
        let mut alerts = self.alerts.write().await;
        if let Some(alert) = alerts.iter_mut().find(|a| a.id == id) {
            alert.dismissed = true;
            true
        } else {
            false
        }
    }

    /// Push a new alert.
    pub async fn push_alert(
        &self,
        category: AlertCategory,
        title: &str,
        message: &str,
        suggestion: Option<&str>,
    ) {
        let alert = ProactiveAlert {
            id: uuid::Uuid::new_v4().to_string(),
            category,
            title: title.to_string(),
            message: message.to_string(),
            suggestion: suggestion.map(|s| s.to_string()),
            timestamp: Utc::now(),
            dismissed: false,
        };
        let mut alerts = self.alerts.write().await;
        alerts.push(alert);
        // Keep max 100 alerts
        if alerts.len() > 100 {
            let drain_to = alerts.len() - 100;
            alerts.drain(0..drain_to);
        }
    }

    /// Check system health and generate alerts if thresholds are exceeded.
    pub async fn check_system_health(&self) {
        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        sys.refresh_cpu_all();

        // Check available RAM
        let available_ram_mb = sys.available_memory() / (1024 * 1024);
        if available_ram_mb < self.thresholds.min_ram_mb {
            self.push_alert(
                AlertCategory::Alert,
                "Low Memory",
                &format!(
                    "Available RAM is {}MB (threshold: {}MB)",
                    available_ram_mb, self.thresholds.min_ram_mb
                ),
                Some("Close unused applications to free memory"),
            )
            .await;
        }

        // Check disk space
        let disks = sysinfo::Disks::new_with_refreshed_list();
        for disk in disks.list() {
            let total = disk.total_space();
            let avail = disk.available_space();
            if total > 0 {
                let pct = (avail as f64 / total as f64) * 100.0;
                if pct < self.thresholds.min_disk_pct {
                    let mount = disk.mount_point().to_string_lossy();
                    let avail_gb = avail / (1024 * 1024 * 1024);
                    self.push_alert(
                        AlertCategory::Alert,
                        "Low Disk Space",
                        &format!("{}: {:.1}% free ({} GB available)", mount, pct, avail_gb),
                        Some("Want me to find large files to clean up?"),
                    )
                    .await;
                }
            }
        }
    }

    /// Start the file watcher for configured directories.
    /// Returns a handle to the watcher (keep alive).
    pub async fn start_file_watcher(&self) -> anyhow::Result<Option<notify::RecommendedWatcher>> {
        use notify::{Config, RecursiveMode, Watcher};

        let dirs = self.watched_dirs.read().await;
        let active: Vec<WatchedDir> = dirs
            .iter()
            .filter(|d| d.enabled && d.path.exists())
            .cloned()
            .collect();
        drop(dirs);

        if active.is_empty() {
            return Ok(None);
        }

        let alerts = self.alerts.clone();
        let (tx, mut rx) = tokio::sync::mpsc::channel(100);

        let mut watcher = notify::RecommendedWatcher::new(
            move |res: Result<notify::Event, notify::Error>| {
                if let Ok(event) = res {
                    let _ = tx.blocking_send(event);
                }
            },
            Config::default(),
        )?;

        for dir in &active {
            watcher.watch(&dir.path, RecursiveMode::NonRecursive)?;
            tracing::info!(path = %dir.path.display(), label = %dir.label, "watching directory");
        }

        // Spawn event processing task
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if event.kind.is_create() || event.kind.is_modify() {
                    for path in &event.paths {
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                        let size_str = if size > 1_000_000_000 {
                            format!("{:.1} GB", size as f64 / 1_000_000_000.0)
                        } else if size > 1_000_000 {
                            format!("{:.1} MB", size as f64 / 1_000_000.0)
                        } else {
                            format!("{} KB", size / 1024)
                        };

                        let is_sensitive = name.starts_with('.')
                            && (name.contains("env")
                                || name.contains("key")
                                || name.contains("secret"));

                        let category = if is_sensitive {
                            AlertCategory::Alert
                        } else {
                            AlertCategory::Info
                        };
                        let title = if is_sensitive {
                            format!("Sensitive file detected: {}", name)
                        } else if event.kind.is_create() {
                            format!("New file: {}", name)
                        } else {
                            format!("File modified: {}", name)
                        };

                        let alert = ProactiveAlert {
                            id: uuid::Uuid::new_v4().to_string(),
                            category,
                            title,
                            message: format!("{} ({})", path.display(), size_str),
                            suggestion: if is_sensitive {
                                Some("This file may contain sensitive data. Review it.".to_string())
                            } else {
                                None
                            },
                            timestamp: Utc::now(),
                            dismissed: false,
                        };
                        let mut al = alerts.write().await;
                        al.push(alert);
                        if al.len() > 100 {
                            let drain_to = al.len() - 100;
                            al.drain(0..drain_to);
                        }
                    }
                }
            }
        });

        Ok(Some(watcher))
    }

    /// Get watched directories.
    pub async fn get_watched_dirs(&self) -> Vec<WatchedDir> {
        self.watched_dirs.read().await.clone()
    }

    /// Get thresholds.
    pub fn thresholds(&self) -> &HealthThresholds {
        &self.thresholds
    }
}
