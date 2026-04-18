use serde::{Deserialize, Serialize};

/// Macro recorder for recording and replaying sequences of tool calls.
pub struct MacroRecorder {
    macros: std::collections::HashMap<String, Macro>,
    recording: Option<(String, Vec<MacroStep>)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Macro {
    pub name: String,
    pub description: String,
    pub steps: Vec<MacroStep>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MacroStep {
    pub tool_name: String,
    pub args: serde_json::Value,
}

impl MacroRecorder {
    pub fn new() -> Self {
        Self {
            macros: std::collections::HashMap::new(),
            recording: None,
        }
    }

    /// Start recording a new macro.
    pub fn start_recording(&mut self, name: &str) {
        self.recording = Some((name.to_string(), Vec::new()));
    }

    /// Record a tool call step.
    pub fn record_step(&mut self, tool_name: &str, args: serde_json::Value) {
        if let Some((_, ref mut steps)) = self.recording {
            steps.push(MacroStep {
                tool_name: tool_name.to_string(),
                args,
            });
        }
    }

    /// Stop recording and save the macro.
    pub fn stop_recording(&mut self, description: &str) -> Option<Macro> {
        if let Some((name, steps)) = self.recording.take() {
            let m = Macro {
                name: name.clone(),
                description: description.to_string(),
                steps,
                created_at: chrono::Utc::now().to_rfc3339(),
            };
            self.macros.insert(name, m.clone());
            Some(m)
        } else {
            None
        }
    }

    /// Get a recorded macro by name.
    pub fn get(&self, name: &str) -> Option<&Macro> {
        self.macros.get(name)
    }

    /// List all macros.
    pub fn list(&self) -> Vec<&Macro> {
        self.macros.values().collect()
    }

    /// Delete a macro.
    pub fn delete(&mut self, name: &str) -> bool {
        self.macros.remove(name).is_some()
    }

    /// Save macros to a JSON file.
    pub fn save_to_file(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let data = serde_json::to_string_pretty(&self.macros)?;
        std::fs::write(path, data)?;
        Ok(())
    }

    /// Load macros from a JSON file.
    pub fn load_from_file(&mut self, path: &std::path::Path) -> anyhow::Result<()> {
        if path.exists() {
            let data = std::fs::read_to_string(path)?;
            self.macros = serde_json::from_str(&data)?;
        }
        Ok(())
    }
}

impl Default for MacroRecorder {
    fn default() -> Self {
        Self::new()
    }
}
