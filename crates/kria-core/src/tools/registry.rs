use crate::infra::ToolResult;
use crate::safety::RiskLevel;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

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
            props.insert(
                p.name.clone(),
                serde_json::json!({
                    "type": p.param_type,
                    "description": p.description,
                }),
            );
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
/// Thread-safe for dynamic registration (e.g. MCP servers connecting in background).
pub struct ToolRegistry {
    defs: RwLock<HashMap<String, ToolDef>>,
    handlers: RwLock<HashMap<String, Arc<dyn ToolHandler>>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            defs: RwLock::new(HashMap::new()),
            handlers: RwLock::new(HashMap::new()),
        }
    }

    /// Register a tool with its definition and handler.
    /// Thread-safe: can be called concurrently from background tasks.
    pub fn register(&self, def: ToolDef, handler: Arc<dyn ToolHandler>) {
        let name = def.name.clone();
        self.defs
            .write()
            .expect("tool registry defs lock poisoned")
            .insert(name.clone(), def);
        self.handlers
            .write()
            .expect("tool registry handlers lock poisoned")
            .insert(name, handler);
    }

    /// Get a tool definition by name.
    pub fn get_def(&self, name: &str) -> Option<ToolDef> {
        self.defs
            .read()
            .expect("tool registry defs lock poisoned")
            .get(name)
            .cloned()
    }

    /// Get a tool handler by name.
    pub fn get_handler(&self, name: &str) -> Option<Arc<dyn ToolHandler>> {
        self.handlers
            .read()
            .expect("tool registry handlers lock poisoned")
            .get(name)
            .cloned()
    }

    /// List all tool definitions (for LLM system prompt).
    pub fn list_defs(&self) -> Vec<ToolDef> {
        self.defs
            .read()
            .expect("tool registry defs lock poisoned")
            .values()
            .cloned()
            .collect()
    }

    /// List tool definitions filtered by hardware tier.
    pub fn list_for_tier(&self, hw_tier: &str) -> Vec<ToolDef> {
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
        self.defs
            .read()
            .expect("tool registry defs lock poisoned")
            .values()
            .filter(|d| tier_rank(d.min_tier) <= rank)
            .cloned()
            .collect()
    }

    /// List tools by category.
    pub fn list_by_category(&self, category: &str) -> Vec<ToolDef> {
        self.defs
            .read()
            .expect("tool registry defs lock poisoned")
            .values()
            .filter(|d| d.category == category)
            .cloned()
            .collect()
    }

    /// Get all category names.
    pub fn categories(&self) -> Vec<String> {
        let defs = self.defs.read().expect("tool registry defs lock poisoned");
        let mut cats: Vec<String> = defs.values().map(|d| d.category.clone()).collect();
        cats.sort();
        cats.dedup();
        cats
    }

    /// Generate the function schemas array for the LLM.
    pub fn function_schemas(&self, hw_tier: &str) -> Vec<serde_json::Value> {
        self.list_for_tier(hw_tier)
            .iter()
            .map(|d| d.to_function_schema())
            .collect()
    }

    /// Total number of registered tools.
    pub fn len(&self) -> usize {
        self.defs
            .read()
            .expect("tool registry defs lock poisoned")
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.defs
            .read()
            .expect("tool registry defs lock poisoned")
            .is_empty()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the full tool registry with all built-in tools.
pub fn build_default_registry() -> ToolRegistry {
    build_registry_with_store(None)
}

/// Build with MemoryStore only (no RAG).
pub fn build_registry_with_store(
    store: Option<std::sync::Arc<crate::memory::store::MemoryStore>>,
) -> ToolRegistry {
    build_registry_full(store, None, None)
}

/// Build the full tool registry with a MemoryStore, optional RagEngine, and optional ProactiveEngine.
pub fn build_registry_full(
    store: Option<std::sync::Arc<crate::memory::store::MemoryStore>>,
    rag: Option<std::sync::Arc<crate::memory::rag::RagEngine>>,
    proactive: Option<std::sync::Arc<crate::automation::proactive::ProactiveEngine>>,
) -> ToolRegistry {
    let reg = ToolRegistry::new();

    super::system_info::register(&reg);
    super::file_ops::register(&reg);
    super::app_lifecycle::register(&reg);
    super::shell::register(&reg);
    super::internet::register(&reg);
    if let Some(s) = store {
        super::knowledge::register(&reg, s);
    } else {
        // Register without memory backing (stubs for testing)
        super::knowledge::register_stubs(&reg);
    }
    super::system_config::register(&reg);
    super::power::register(&reg);
    super::process::register(&reg);
    super::documents::register(&reg);
    super::communication::register(&reg);
    super::interaction::register(&reg);
    super::disk::register(&reg);
    super::packages::register(&reg);
    super::scheduler::register(&reg);
    super::vision::register(&reg, None);
    super::desktop::register(&reg);
    super::developer::register(&reg);
    super::i18n::register(&reg);
    if let Some(rag_engine) = rag {
        super::rag::register(&reg, rag_engine);
    }
    if let Some(proactive_engine) = proactive {
        super::proactive::register(&reg, proactive_engine);
    }

    tracing::info!(count = reg.len(), "tool registry built");
    reg
}
