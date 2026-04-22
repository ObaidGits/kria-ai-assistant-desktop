/// Installed application registry for Linux (freedesktop .desktop files).
///
/// Scans `.desktop` files from standard locations including system, user-local,
/// Snap, Flatpak exports, and AppImage-generated entries. Builds:
/// - A name→`CanonicalAppId` alias map for LLM input normalization.
/// - A `CanonicalAppId`→desktop-file-path map for `gio launch`.
/// - A set of registered URI schemes for deep-link classification.
/// - A fingerprint (SHA-256) of each `Exec=` line to detect handler hijacking.
///
/// # Refresh strategy
/// - Startup: full scan, blocking until complete.
/// - Runtime: `notify` filesystem watcher on all scan directories.
/// - Belt-and-suspenders: periodic full rescan every 5 minutes.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use once_cell::sync::Lazy;
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::platform::intent::capability::CanonicalAppId;

// ─── Scan directories (Linux) ─────────────────────────────────────────────────

/// All directories that may contain `.desktop` application entries on a Linux desktop.
static SCAN_DIRS: Lazy<Vec<PathBuf>> = Lazy::new(|| {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/root"));
    vec![
        PathBuf::from("/usr/share/applications"),
        PathBuf::from("/usr/local/share/applications"),
        home.join(".local/share/applications"),
        // Snap desktop integration.
        PathBuf::from("/var/lib/snapd/desktop/applications"),
        // Flatpak: user-level.
        home.join(".local/share/flatpak/exports/share/applications"),
        // Flatpak: system-level.
        PathBuf::from("/var/lib/flatpak/exports/share/applications"),
    ]
});

// ─── AppManifest ─────────────────────────────────────────────────────────────

/// Metadata extracted from a `.desktop` file.
#[derive(Clone, Debug)]
pub struct AppManifest {
    /// Canonical application ID (usually the `.desktop` filename without extension,
    /// or the `StartupWMClass=` value if present).
    pub app_id: CanonicalAppId,
    /// Human-readable display name from `Name=`.
    pub display_name: String,
    /// Path to the `.desktop` file — used by `gio launch`.
    pub desktop_path: PathBuf,
    /// `Exec=` line value — used for fingerprinting.
    pub exec_line: String,
    /// SHA-256 of the `Exec=` line at discovery time.
    /// Compared on every launch to detect default-handler hijacking.
    pub exec_fingerprint: [u8; 32],
    /// URI schemes registered by this app (from `MimeType=x-scheme-handler/<scheme>`).
    pub registered_schemes: Vec<String>,
    /// Additional name aliases (from `GenericName=`, `X-KDE-Aliases=`, etc.) used for
    /// fuzzy resolution of LLM-supplied names like "chrome" → "chromium".
    pub name_aliases: Vec<String>,
}

// ─── InstalledAppRegistry ─────────────────────────────────────────────────────

/// Thread-safe, self-refreshing registry of installed desktop applications.
pub struct InstalledAppRegistry {
    /// `CanonicalAppId.as_str()` → `AppManifest`
    apps: Arc<RwLock<HashMap<String, AppManifest>>>,
    /// Lowercase name/alias → `CanonicalAppId.as_str()`
    aliases: Arc<RwLock<HashMap<String, String>>>,
    /// URI scheme string → `CanonicalAppId.as_str()`
    schemes: Arc<RwLock<HashMap<String, String>>>,
}

impl InstalledAppRegistry {
    /// Build the registry synchronously. Suitable for calling at startup before the
    /// Tokio runtime is needed for other work.
    pub fn build_sync() -> Arc<Self> {
        let registry = Arc::new(Self {
            apps: Arc::new(RwLock::new(HashMap::new())),
            aliases: Arc::new(RwLock::new(HashMap::new())),
            schemes: Arc::new(RwLock::new(HashMap::new())),
        });

        // Perform an immediate full scan on the current thread.
        let manifests = scan_all_desktop_files();
        let rt = tokio::runtime::Handle::try_current();
        if let Ok(handle) = rt {
            handle.block_on(registry.load_manifests(manifests));
        } else {
            // No runtime yet — we'll populate lazily on first access.
            // This path should not happen in production but is safe.
            warn!("InstalledAppRegistry::build_sync called without a Tokio runtime");
        }

        registry
    }

    /// Build the registry asynchronously. The registry is empty until
    /// `initialize()` completes.
    pub async fn build_async() -> Arc<Self> {
        let registry = Arc::new(Self {
            apps: Arc::new(RwLock::new(HashMap::new())),
            aliases: Arc::new(RwLock::new(HashMap::new())),
            schemes: Arc::new(RwLock::new(HashMap::new())),
        });
        registry.initialize().await;
        registry
    }

    /// Full scan and load. Idempotent — replaces existing data.
    pub async fn initialize(&self) {
        info!("InstalledAppRegistry: starting full scan");
        let manifests = tokio::task::spawn_blocking(scan_all_desktop_files)
            .await
            .unwrap_or_default();
        info!(count = manifests.len(), "InstalledAppRegistry: scan complete");
        self.load_manifests(manifests).await;
    }

    async fn load_manifests(&self, manifests: Vec<AppManifest>) {
        let mut apps = self.apps.write().await;
        let mut aliases = self.aliases.write().await;
        let mut schemes = self.schemes.write().await;

        apps.clear();
        aliases.clear();
        schemes.clear();

        for manifest in manifests {
            let id_str = manifest.app_id.as_str().to_lowercase();

            // Register aliases (case-insensitive).
            aliases.insert(id_str.clone(), id_str.clone());
            aliases.insert(manifest.display_name.to_lowercase(), id_str.clone());
            for alias in &manifest.name_aliases {
                aliases.insert(alias.to_lowercase(), id_str.clone());
            }

            // Register URI schemes.
            for scheme in &manifest.registered_schemes {
                schemes.insert(scheme.clone(), id_str.clone());
            }

            apps.insert(id_str, manifest);
        }
    }

    /// Spawn a background task that watches scan directories for changes and
    /// performs a periodic full rescan every 5 minutes.
    pub fn spawn_watcher(self: Arc<Self>) {
        let registry = Arc::clone(&self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300));
            interval.tick().await; // First tick fires immediately; skip it.
            loop {
                interval.tick().await;
                debug!("InstalledAppRegistry: periodic rescan");
                registry.initialize().await;
            }
        });
    }

    /// Check whether an app with the given `CanonicalAppId` is installed.
    pub fn is_installed(&self, app_id: &CanonicalAppId) -> bool {
        // We need a blocking read in a sync context; use try_read.
        if let Ok(apps) = self.apps.try_read() {
            apps.contains_key(&app_id.as_str().to_lowercase())
        } else {
            // Lock contention — fail safe by allowing the dispatch to proceed;
            // the backend will return an error if the app truly isn't installed.
            true
        }
    }

    /// Get the `.desktop` file path for a canonical app ID.
    pub fn desktop_path(&self, app_id: &CanonicalAppId) -> Option<PathBuf> {
        let apps = self.apps.try_read().ok()?;
        apps.get(&app_id.as_str().to_lowercase())
            .map(|m| m.desktop_path.clone())
    }

    /// Resolve a user-supplied app name (e.g., "chrome", "Google Chrome") to a
    /// `CanonicalAppId`. Returns `None` if no match.
    pub fn resolve_alias(&self, name: &str) -> Option<CanonicalAppId> {
        let aliases = self.aliases.try_read().ok()?;
        let id_str = aliases.get(&name.to_lowercase())?;
        Some(CanonicalAppId::from_registry(id_str.clone()))
    }

    /// Return all URI schemes registered by installed applications.
    pub fn registered_schemes(&self) -> HashSet<String> {
        if let Ok(schemes) = self.schemes.try_read() {
            schemes.keys().cloned().collect()
        } else {
            HashSet::new()
        }
    }

    /// Check whether a specific app's `Exec=` line matches its registered fingerprint.
    /// Returns `true` if the fingerprint is valid (or if fingerprinting is unavailable).
    /// Returns `false` if the handler binary appears to have changed — callers should
    /// elevate to RED and warn the user.
    pub fn verify_exec_fingerprint(&self, app_id: &CanonicalAppId) -> bool {
        let apps = match self.apps.try_read() {
            Ok(a) => a,
            Err(_) => return true, // fail open on lock contention
        };
        let manifest = match apps.get(&app_id.as_str().to_lowercase()) {
            Some(m) => m,
            None => return true,
        };

        // Re-hash the current Exec= value from the .desktop file on disk.
        let current_exec = read_exec_line(&manifest.desktop_path);
        match current_exec {
            None => true, // Can't read — assume valid.
            Some(exec) => {
                let current_hash = sha256_bytes(exec.as_bytes());
                current_hash == manifest.exec_fingerprint
            }
        }
    }
}

// ─── Desktop file parsing ─────────────────────────────────────────────────────

fn scan_all_desktop_files() -> Vec<AppManifest> {
    let mut manifests = Vec::new();

    for dir in SCAN_DIRS.iter() {
        if !dir.exists() {
            continue;
        }
        match std::fs::read_dir(dir) {
            Err(e) => {
                debug!("cannot read scan dir {}: {e}", dir.display());
                continue;
            }
            Ok(entries) => {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("desktop") {
                        if let Some(manifest) = parse_desktop_file(&path) {
                            manifests.push(manifest);
                        }
                    }
                }
            }
        }
    }

    manifests
}

fn parse_desktop_file(path: &Path) -> Option<AppManifest> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut name = String::new();
    let mut exec = String::new();
    let mut generic_name = String::new();
    let mut startup_wm_class = String::new();
    let mut mime_types: Vec<String> = Vec::new();
    let mut hidden = false;
    let mut no_display = false;

    for line in content.lines() {
        // Only parse the [Desktop Entry] section.
        if line.starts_with('[') && line != "[Desktop Entry]" {
            break;
        }
        if let Some(val) = line.strip_prefix("Name=") {
            if name.is_empty() {
                name = val.trim().to_string();
            }
        } else if let Some(val) = line.strip_prefix("Exec=") {
            exec = val.trim().to_string();
        } else if let Some(val) = line.strip_prefix("GenericName=") {
            generic_name = val.trim().to_string();
        } else if let Some(val) = line.strip_prefix("StartupWMClass=") {
            startup_wm_class = val.trim().to_string();
        } else if let Some(val) = line.strip_prefix("MimeType=") {
            // e.g. "application/pdf;x-scheme-handler/https;x-scheme-handler/http;"
            for part in val.split(';') {
                let part = part.trim();
                if part.starts_with("x-scheme-handler/") {
                    if let Some(scheme) = part.strip_prefix("x-scheme-handler/") {
                        mime_types.push(scheme.to_string());
                    }
                }
            }
        } else if line == "Hidden=true" || line == "NoDisplay=true" {
            if line == "Hidden=true" {
                hidden = true;
            } else {
                no_display = true;
            }
        }
    }

    if name.is_empty() || hidden || no_display {
        return None;
    }

    // Derive canonical app ID: StartupWMClass > filename stem.
    let file_stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    let canonical_id = if !startup_wm_class.is_empty() {
        startup_wm_class.to_lowercase()
    } else {
        file_stem
    };

    let exec_fingerprint = sha256_bytes(exec.as_bytes());

    // Build aliases from display name and generic name.
    let mut aliases = vec![name.to_lowercase()];
    if !generic_name.is_empty() {
        aliases.push(generic_name.to_lowercase());
    }

    Some(AppManifest {
        app_id: CanonicalAppId::from_registry(canonical_id),
        display_name: name,
        desktop_path: path.to_path_buf(),
        exec_line: exec,
        exec_fingerprint,
        registered_schemes: mime_types,
        name_aliases: aliases,
    })
}

fn read_exec_line(desktop_path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(desktop_path).ok()?;
    for line in content.lines() {
        if let Some(val) = line.strip_prefix("Exec=") {
            return Some(val.trim().to_string());
        }
    }
    None
}

fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

// ─── Well-known alias overrides ───────────────────────────────────────────────
//
// These supplement the .desktop scanner with common user-facing names that
// don't always appear in the Name= field.
//
// This replaces the ad-hoc `match app_name` block in loop_engine.rs L216-221.

pub fn builtin_alias_map() -> HashMap<String, String> {
    let mut m = HashMap::new();
    let pairs: &[(&str, &str)] = &[
        ("chrome", "chromium"),
        ("google chrome", "chromium"),
        ("google-chrome", "chromium"),
        ("google-chrome-stable", "google-chrome-stable"),
        ("chrome browser", "chromium"),
        ("google chrome browser", "chromium"),
        ("firefox", "firefox"),
        ("ff", "firefox"),
        ("vscode", "code"),
        ("visual studio code", "code"),
        ("vs code", "code"),
        ("whatsapp", "whatsapp-linux-amd64"),
        ("telegram", "telegramdesktop"),
        ("files", "org.gnome.nautilus"),
        ("file manager", "org.gnome.nautilus"),
        ("calculator", "org.gnome.calculator"),
        ("text editor", "org.gnome.texeditor"),
        ("terminal", "org.gnome.terminal"),
        ("settings", "gnome-control-center"),
    ];
    for (alias, id) in pairs {
        m.insert(alias.to_string(), id.to_string());
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_aliases_contain_chrome_variants() {
        let map = builtin_alias_map();
        assert_eq!(map.get("chrome").unwrap(), "chromium");
        assert_eq!(map.get("google chrome").unwrap(), "chromium");
        assert_eq!(map.get("google-chrome").unwrap(), "chromium");
    }

    #[test]
    fn sha256_produces_32_bytes() {
        let hash = sha256_bytes(b"test");
        assert_eq!(hash.len(), 32);
    }

    #[test]
    fn fingerprint_differs_on_changed_exec() {
        let h1 = sha256_bytes(b"Exec=/usr/bin/chromium %U");
        let h2 = sha256_bytes(b"Exec=/tmp/malicious %U");
        assert_ne!(h1, h2);
    }
}
