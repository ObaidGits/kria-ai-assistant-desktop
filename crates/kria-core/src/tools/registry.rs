use std::collections::HashMap;
use std::sync::Arc;
use async_trait::async_trait;
use crate::infra::ToolResult;
use crate::safety::RiskLevel;

/// Tool parameter schema for LLM function-calling format.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ParamDef {
    pub name: String,
    #[serde(rename = "type")]
    pub param_type: String,
    pub description: String,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
}

/// Full tool definition including name, description, parameter schema, and tier.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub category: String,
    pub parameters: Vec<ParamDef>,
    pub default_tier: RiskLevel,
    /// Minimum hardware tier ("lite" tools available on all hardware).
    pub min_tier: &'static str,
}

impl ToolDef {
    /// Convert to OpenAI-compatible function schema for LLM.
    pub fn to_function_schema(&self) -> serde_json::Value {
        let mut props = serde_json::Map::new();
        let mut required = Vec::new();

        for p in &self.parameters {
            props.insert(p.name.clone(), serde_json::json!({
                "type": p.param_type,
                "description": p.description,
            }));
            if p.required {
                required.push(serde_json::Value::String(p.name.clone()));
            }
        }

        serde_json::json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": {
                    "type": "object",
                    "properties": props,
                    "required": required,
                }
            }
        })
    }
}

/// Trait for tool execution handlers.
#[async_trait]
pub trait ToolHandler: Send + Sync {
    async fn execute(&self, params: serde_json::Value) -> ToolResult;
}

/// Central tool registry. Holds all tool definitions and their handlers.
pub struct ToolRegistry {
    defs: HashMap<String, ToolDef>,
    handlers: HashMap<String, Arc<dyn ToolHandler>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            defs: HashMap::new(),
            handlers: HashMap::new(),
        }
    }

    /// Register a tool with its definition and handler.
    pub fn register(&mut self, def: ToolDef, handler: Arc<dyn ToolHandler>) {
        let name = def.name.clone();
        self.defs.insert(name.clone(), def);
        self.handlers.insert(name, handler);
    }

    /// Get a tool definition by name.
    pub fn get_def(&self, name: &str) -> Option<&ToolDef> {
        self.defs.get(name)
    }

    /// Get a tool handler by name.
    pub fn get_handler(&self, name: &str) -> Option<&Arc<dyn ToolHandler>> {
        self.handlers.get(name)
    }

    /// List all tool definitions (for LLM system prompt).
    pub fn list_defs(&self) -> Vec<&ToolDef> {
        self.defs.values().collect()
    }

    /// List tool definitions filtered by hardware tier.
    pub fn list_for_tier(&self, hw_tier: &str) -> Vec<&ToolDef> {
        let tier_rank = |t: &str| -> u8 {
            match t {
                "lite" => 0,
                "standard" => 1,
                "performance" => 2,
                "high" => 3,
                _ => 0,
            }
        };
        let rank = tier_rank(hw_tier);
        self.defs.values().filter(|d| tier_rank(d.min_tier) <= rank).collect()
    }

    /// List tools by category.
    pub fn list_by_category(&self, category: &str) -> Vec<&ToolDef> {
        self.defs.values().filter(|d| d.category == category).collect()
    }

    /// Get all category names.
    pub fn categories(&self) -> Vec<String> {
        let mut cats: Vec<String> = self.defs.values().map(|d| d.category.clone()).collect();
        cats.sort();
        cats.dedup();
        cats
    }

    /// Generate the function schemas array for the LLM.
    pub fn function_schemas(&self, hw_tier: &str) -> Vec<serde_json::Value> {
        self.list_for_tier(hw_tier).iter().map(|d| d.to_function_schema()).collect()
    }

    /// Total number of registered tools.
    pub fn len(&self) -> usize {
        self.defs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }
}

/// Build the full tool registry with all built-in tools.
pub fn build_default_registry() -> ToolRegistry {
    let mut reg = ToolRegistry::new();

    super::system_info::register(&mut reg);
    super::file_ops::register(&mut reg);
    super::app_lifecycle::register(&mut reg);
    super::shell::register(&mut reg);
    super::internet::register(&mut reg);
    super::knowledge::register(&mut reg);
    super::system_config::register(&mut reg);
    super::power::register(&mut reg);
    super::process::register(&mut reg);
    super::documents::register(&mut reg);
    super::communication::register(&mut reg);
    super::interaction::register(&mut reg);
    super::disk::register(&mut reg);
    super::scheduler::register(&mut reg);

    tracing::info!(count = reg.len(), "tool registry built");
    reg
}
