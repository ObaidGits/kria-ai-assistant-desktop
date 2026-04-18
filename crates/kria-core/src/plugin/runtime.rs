use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Plugin metadata discovered from manifest.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    /// Tool names provided by this plugin.
    pub tools: Vec<String>,
}

/// Plugin runtime: discovers, loads and manages plugins.
pub struct PluginRuntime {
    plugin_dir: PathBuf,
    loaded: HashMap<String, PluginManifest>,
}

impl PluginRuntime {
    pub fn new(plugin_dir: PathBuf) -> Self {
        Self {
            plugin_dir,
            loaded: HashMap::new(),
        }
    }

    /// Discover plugins from the plugin directory.
    pub fn discover(&mut self) -> anyhow::Result<Vec<PluginManifest>> {
        let mut found = Vec::new();

        if !self.plugin_dir.exists() {
            return Ok(found);
        }

        for entry in std::fs::read_dir(&self.plugin_dir)? {
            let entry = entry?;
            let path = entry.path();

            if !path.is_dir() {
                continue;
            }

            let manifest_path = path.join("manifest.json");
            if manifest_path.exists() {
                match self.load_manifest(&manifest_path) {
                    Ok(manifest) => {
                        tracing::info!(
                            "discovered plugin: {} v{}",
                            manifest.name,
                            manifest.version
                        );
                        found.push(manifest);
                    }
                    Err(e) => {
                        tracing::warn!("invalid plugin manifest at {:?}: {e}", manifest_path);
                    }
                }
            }
        }

        Ok(found)
    }

    fn load_manifest(&self, path: &Path) -> anyhow::Result<PluginManifest> {
        let data = std::fs::read_to_string(path)?;
        let manifest: PluginManifest = serde_json::from_str(&data)?;
        Ok(manifest)
    }

    /// Register a discovered plugin.
    pub fn register(&mut self, manifest: PluginManifest) {
        self.loaded.insert(manifest.name.clone(), manifest);
    }

    /// List loaded plugins.
    pub fn list(&self) -> Vec<&PluginManifest> {
        self.loaded.values().collect()
    }

    /// Check if a plugin provides a specific tool.
    pub fn find_tool_provider(&self, tool_name: &str) -> Option<&PluginManifest> {
        self.loaded
            .values()
            .find(|m| m.tools.iter().any(|t| t == tool_name))
    }

    /// Unload a plugin by name.
    pub fn unload(&mut self, name: &str) -> bool {
        self.loaded.remove(name).is_some()
    }
}
