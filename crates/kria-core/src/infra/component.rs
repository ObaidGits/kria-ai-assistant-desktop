//! Component version tracking and update checking.
//!
//! Tracks installed versions of managed components (llama-server, models, sidecar deps)
//! and checks for available updates.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// A managed component with version tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentInfo {
    pub name: String,
    pub version: String,
    pub installed_at: String,
    pub path: PathBuf,
    pub component_type: ComponentType,
}

/// Types of managed components.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentType {
    LlamaServer,
    Model,
    SttModel,
    TtsVoice,
    PythonSidecar,
}

/// Persisted manifest of all installed components.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ComponentManifest {
    /// Map from component name → info
    pub components: HashMap<String, ComponentInfo>,
    /// Last time we checked for updates (ISO 8601)
    pub last_update_check: Option<String>,
}

impl ComponentManifest {
    /// Load from disk, or create empty if not found.
    pub fn load() -> Self {
        let path = manifest_path();
        match std::fs::read_to_string(&path) {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Persist to disk.
    pub fn save(&self) -> anyhow::Result<()> {
        let path = manifest_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)?;
        Ok(())
    }

    /// Register or update a component.
    pub fn register(&mut self, info: ComponentInfo) {
        self.components.insert(info.name.clone(), info);
    }

    /// Remove a component.
    pub fn unregister(&mut self, name: &str) {
        self.components.remove(name);
    }

    /// Check if a component is installed.
    pub fn is_installed(&self, name: &str) -> bool {
        self.components.contains_key(name)
    }

    /// Get installed version of a component.
    pub fn version_of(&self, name: &str) -> Option<&str> {
        self.components.get(name).map(|c| c.version.as_str())
    }
}

/// Path to the component manifest file.
fn manifest_path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".kria").join("components.json")
}
