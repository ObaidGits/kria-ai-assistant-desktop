use std::sync::Arc;
use async_trait::async_trait;
use crate::infra::ToolResult;
use crate::safety::RiskLevel;
use crate::tools::registry::{ToolRegistry, ToolDef, ToolHandler, ParamDef};

fn param(name: &str, ty: &str, desc: &str, required: bool) -> ParamDef {
    ParamDef { name: name.into(), param_type: ty.into(), description: desc.into(), required, default: None }
}

struct RememberFact;
#[async_trait] impl ToolHandler for RememberFact {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let key = params["key"].as_str().unwrap_or("");
        let value = params["value"].as_str().unwrap_or("");
        // Actual persistence delegated to memory::FactManager at the agent layer
        ToolResult::ok(serde_json::json!({
            "stored": true, "key": key, "value": value,
        }))
    }
}

struct RecallFact;
#[async_trait] impl ToolHandler for RecallFact {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let query = params["query"].as_str().unwrap_or("");
        // Actual retrieval delegated to memory::ContextBuilder at the agent layer
        ToolResult::ok(serde_json::json!({
            "query": query,
            "note": "recall delegated to memory layer",
        }))
    }
}

struct SearchKnowledge;
#[async_trait] impl ToolHandler for SearchKnowledge {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let query = params["query"].as_str().unwrap_or("");
        let max = params["max_results"].as_u64().unwrap_or(10);
        ToolResult::ok(serde_json::json!({
            "query": query, "max_results": max,
            "note": "search delegated to memory layer",
        }))
    }
}

struct SaveSnippet;
#[async_trait] impl ToolHandler for SaveSnippet {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let name = params["name"].as_str().unwrap_or("");
        let content = params["content"].as_str().unwrap_or("");
        let language = params["language"].as_str().unwrap_or("");
        ToolResult::ok(serde_json::json!({
            "saved": true, "name": name, "language": language,
            "size": content.len(),
        }))
    }
}

struct GetSnippet;
#[async_trait] impl ToolHandler for GetSnippet {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let name = params["name"].as_str().unwrap_or("");
        ToolResult::ok(serde_json::json!({ "name": name, "note": "delegated to memory layer" }))
    }
}

struct ListSnippets;
#[async_trait] impl ToolHandler for ListSnippets {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        ToolResult::ok(serde_json::json!({ "note": "delegated to memory layer" }))
    }
}

struct ListRemembered;
#[async_trait] impl ToolHandler for ListRemembered {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        ToolResult::ok(serde_json::json!({ "note": "delegated to memory layer" }))
    }
}

struct IngestDocument;
#[async_trait] impl ToolHandler for IngestDocument {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path = params["path"].as_str().unwrap_or("");
        ToolResult::ok(serde_json::json!({
            "path": path,
            "note": "document ingestion delegated to preprocessing + memory layer",
        }))
    }
}

pub fn register(reg: &mut ToolRegistry) {
    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        (ToolDef {
            name: "remember_fact".into(), description: "Store a fact in long-term memory".into(),
            category: "knowledge".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![
                param("key", "string", "Fact key/topic", true),
                param("value", "string", "Fact content", true),
            ],
        }, Arc::new(RememberFact)),
        (ToolDef {
            name: "recall_fact".into(), description: "Recall facts matching a query".into(),
            category: "knowledge".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![param("query", "string", "Search query", true)],
        }, Arc::new(RecallFact)),
        (ToolDef {
            name: "search_knowledge".into(), description: "Search all stored knowledge".into(),
            category: "knowledge".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![
                param("query", "string", "Search query", true),
                param("max_results", "integer", "Max results (default 10)", false),
            ],
        }, Arc::new(SearchKnowledge)),
        (ToolDef {
            name: "list_remembered".into(), description: "List all remembered facts".into(),
            category: "knowledge".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![],
        }, Arc::new(ListRemembered)),
        (ToolDef {
            name: "save_snippet".into(), description: "Save a code snippet to memory".into(),
            category: "knowledge".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![
                param("name", "string", "Snippet name", true),
                param("content", "string", "Code content", true),
                param("language", "string", "Programming language", false),
            ],
        }, Arc::new(SaveSnippet)),
        (ToolDef {
            name: "get_snippet".into(), description: "Retrieve a saved code snippet".into(),
            category: "knowledge".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![param("name", "string", "Snippet name", true)],
        }, Arc::new(GetSnippet)),
        (ToolDef {
            name: "list_snippets".into(), description: "List all saved snippets".into(),
            category: "knowledge".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![],
        }, Arc::new(ListSnippets)),
        (ToolDef {
            name: "ingest_document".into(), description: "Ingest a document into knowledge base".into(),
            category: "knowledge".into(), default_tier: RiskLevel::Green, min_tier: "standard",
            parameters: vec![param("path", "string", "Document path", true)],
        }, Arc::new(IngestDocument)),
    ];
    for (def, handler) in tools { reg.register(def, handler); }
}
