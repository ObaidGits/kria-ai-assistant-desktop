use std::sync::Arc;
use async_trait::async_trait;
use crate::infra::ToolResult;
use crate::memory::store::MemoryStore;
use crate::safety::RiskLevel;
use crate::tools::registry::{ToolRegistry, ToolDef, ToolHandler, ParamDef};
use chrono::Utc;

fn param(name: &str, ty: &str, desc: &str, required: bool) -> ParamDef {
    ParamDef { name: name.into(), param_type: ty.into(), description: desc.into(), required, default: None }
}

/// Shared handle to MemoryStore, injected into each handler.
#[derive(Clone)]
struct StoreHandle(Arc<MemoryStore>);

struct RememberFact(StoreHandle);
#[async_trait] impl ToolHandler for RememberFact {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let key = params["key"].as_str().unwrap_or("").to_string();
        let value = params["value"].as_str().unwrap_or("").to_string();
        let fact = crate::memory::store::MemoryFact {
            id: None,
            text: format!("{}: {}", key, value),
            category: key.clone(),
            source: "user_tool".into(),
            created_at: Utc::now(),
            last_accessed: Utc::now(),
            access_count: 0,
            decay_score: 1.0,
        };
        match self.0 .0.store_fact(&fact) {
            Ok(id) => ToolResult::ok(serde_json::json!({
                "stored": true, "key": key, "value": value, "fact_id": id,
            })),
            Err(e) => ToolResult { success: false, data: serde_json::Value::Null, error: Some(e.to_string()) },
        }
    }
}

struct RecallFact(StoreHandle);
#[async_trait] impl ToolHandler for RecallFact {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let query = params["query"].as_str().unwrap_or("");
        match self.0 .0.search_facts(query, 10) {
            Ok(facts) => {
                // Update access timestamps for returned facts
                for f in &facts {
                    if let Some(id) = f.id {
                        let _ = self.0 .0.update_fact_access(id);
                    }
                }
                let results: Vec<serde_json::Value> = facts.iter().map(|f| {
                    serde_json::json!({
                        "id": f.id, "text": f.text, "category": f.category,
                        "source": f.source, "access_count": f.access_count,
                    })
                }).collect();
                ToolResult::ok(serde_json::json!({ "query": query, "results": results, "count": results.len() }))
            }
            Err(e) => ToolResult { success: false, data: serde_json::Value::Null, error: Some(e.to_string()) },
        }
    }
}

struct SearchKnowledge(StoreHandle);
#[async_trait] impl ToolHandler for SearchKnowledge {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let query = params["query"].as_str().unwrap_or("");
        let max = params["max_results"].as_u64().unwrap_or(10) as usize;

        // Hybrid search: FTS facts + conversation search
        let facts = self.0 .0.search_facts(query, max).unwrap_or_default();
        let convos = self.0 .0.search_conversations(query, max).unwrap_or_default();

        let fact_results: Vec<serde_json::Value> = facts.iter().map(|f| {
            serde_json::json!({ "type": "fact", "id": f.id, "text": f.text, "category": f.category })
        }).collect();
        let conv_results: Vec<serde_json::Value> = convos.iter().map(|c| {
            serde_json::json!({ "type": "conversation", "session": c.session_id, "role": c.role, "content": c.content })
        }).collect();

        let mut all = fact_results;
        all.extend(conv_results);
        let total = all.len();
        ToolResult::ok(serde_json::json!({ "query": query, "results": all, "count": total }))
    }
}

struct ListRemembered(StoreHandle);
#[async_trait] impl ToolHandler for ListRemembered {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        match self.0 .0.all_facts_with_decay(0.0) {
            Ok(facts) => {
                let results: Vec<serde_json::Value> = facts.iter().map(|f| {
                    serde_json::json!({
                        "id": f.id, "text": f.text, "category": f.category,
                        "decay_score": f.decay_score, "access_count": f.access_count,
                    })
                }).collect();
                ToolResult::ok(serde_json::json!({ "facts": results, "count": results.len() }))
            }
            Err(e) => ToolResult { success: false, data: serde_json::Value::Null, error: Some(e.to_string()) },
        }
    }
}

struct SaveSnippet(StoreHandle);
#[async_trait] impl ToolHandler for SaveSnippet {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let name = params["name"].as_str().unwrap_or("");
        let content = params["content"].as_str().unwrap_or("");
        let language = params["language"].as_str().unwrap_or("text");
        match self.0 .0.save_snippet(name, content, language, &[]) {
            Ok(()) => ToolResult::ok(serde_json::json!({
                "saved": true, "name": name, "language": language, "size": content.len(),
            })),
            Err(e) => ToolResult { success: false, data: serde_json::Value::Null, error: Some(e.to_string()) },
        }
    }
}

struct GetSnippet(StoreHandle);
#[async_trait] impl ToolHandler for GetSnippet {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let name = params["name"].as_str().unwrap_or("");
        match self.0 .0.get_snippet(name) {
            Ok(Some((content, language, tags))) => ToolResult::ok(serde_json::json!({
                "name": name, "content": content, "language": language, "tags": tags,
            })),
            Ok(None) => ToolResult::ok(serde_json::json!({ "name": name, "found": false })),
            Err(e) => ToolResult { success: false, data: serde_json::Value::Null, error: Some(e.to_string()) },
        }
    }
}

struct ListSnippets(StoreHandle);
#[async_trait] impl ToolHandler for ListSnippets {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let tag = params["tag"].as_str();
        match self.0 .0.list_snippets(tag) {
            Ok(names) => ToolResult::ok(serde_json::json!({ "snippets": names, "count": names.len() })),
            Err(e) => ToolResult { success: false, data: serde_json::Value::Null, error: Some(e.to_string()) },
        }
    }
}

struct IngestDocument(StoreHandle);
#[async_trait] impl ToolHandler for IngestDocument {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path = params["path"].as_str().unwrap_or("");
        // Read the file and store as a fact
        match std::fs::read_to_string(path) {
            Ok(content) => {
                let truncated = if content.len() > 10_000 { &content[..10_000] } else { &content };
                let fact = crate::memory::store::MemoryFact {
                    id: None,
                    text: format!("[document:{}] {}", path, truncated),
                    category: "document".into(),
                    source: format!("ingested:{}", path),
                    created_at: Utc::now(),
                    last_accessed: Utc::now(),
                    access_count: 0,
                    decay_score: 1.0,
                };
                match self.0 .0.store_fact(&fact) {
                    Ok(id) => ToolResult::ok(serde_json::json!({
                        "ingested": true, "path": path, "fact_id": id,
                        "size_bytes": content.len(), "truncated": content.len() > 10_000,
                    })),
                    Err(e) => ToolResult { success: false, data: serde_json::Value::Null, error: Some(e.to_string()) },
                }
            }
            Err(e) => ToolResult { success: false, data: serde_json::Value::Null, error: Some(format!("cannot read file: {e}")) },
        }
    }
}

pub fn register(reg: &mut ToolRegistry, store: Arc<MemoryStore>) {
    let h = StoreHandle(store);
    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        (ToolDef {
            name: "remember_fact".into(), description: "Store a fact in long-term memory".into(),
            category: "knowledge".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![
                param("key", "string", "Fact key/topic", true),
                param("value", "string", "Fact content", true),
            ],
        }, Arc::new(RememberFact(h.clone()))),
        (ToolDef {
            name: "recall_fact".into(), description: "Recall facts matching a query".into(),
            category: "knowledge".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![param("query", "string", "Search query", true)],
        }, Arc::new(RecallFact(h.clone()))),
        (ToolDef {
            name: "search_knowledge".into(), description: "Search all stored knowledge".into(),
            category: "knowledge".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![
                param("query", "string", "Search query", true),
                param("max_results", "integer", "Max results (default 10)", false),
            ],
        }, Arc::new(SearchKnowledge(h.clone()))),
        (ToolDef {
            name: "list_remembered".into(), description: "List all remembered facts".into(),
            category: "knowledge".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![],
        }, Arc::new(ListRemembered(h.clone()))),
        (ToolDef {
            name: "save_snippet".into(), description: "Save a code snippet to memory".into(),
            category: "knowledge".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![
                param("name", "string", "Snippet name", true),
                param("content", "string", "Code content", true),
                param("language", "string", "Programming language", false),
            ],
        }, Arc::new(SaveSnippet(h.clone()))),
        (ToolDef {
            name: "get_snippet".into(), description: "Retrieve a saved code snippet".into(),
            category: "knowledge".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![param("name", "string", "Snippet name", true)],
        }, Arc::new(GetSnippet(h.clone()))),
        (ToolDef {
            name: "list_snippets".into(), description: "List all saved snippets".into(),
            category: "knowledge".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![param("tag", "string", "Filter by tag", false)],
        }, Arc::new(ListSnippets(h.clone()))),
        (ToolDef {
            name: "ingest_document".into(), description: "Ingest a document into knowledge base".into(),
            category: "knowledge".into(), default_tier: RiskLevel::Green, min_tier: "standard",
            parameters: vec![param("path", "string", "Document path", true)],
        }, Arc::new(IngestDocument(h))),
    ];
    for (def, handler) in tools { reg.register(def, handler); }
}

/// Stub registration for tests (no MemoryStore required).
pub fn register_stubs(reg: &mut ToolRegistry) {
    struct Stub;
    #[async_trait] impl ToolHandler for Stub {
        async fn execute(&self, _params: serde_json::Value) -> ToolResult {
            ToolResult::ok(serde_json::json!({ "note": "stub — no memory store" }))
        }
    }

    let defs = vec![
        ("remember_fact", "Store a fact in long-term memory", vec![
            param("key", "string", "Fact key/topic", true),
            param("value", "string", "Fact content", true),
        ]),
        ("recall_fact", "Recall facts matching a query", vec![
            param("query", "string", "Search query", true),
        ]),
        ("search_knowledge", "Search all stored knowledge", vec![
            param("query", "string", "Search query", true),
            param("max_results", "integer", "Max results (default 10)", false),
        ]),
        ("list_remembered", "List all remembered facts", vec![]),
        ("save_snippet", "Save a code snippet to memory", vec![
            param("name", "string", "Snippet name", true),
            param("content", "string", "Code content", true),
            param("language", "string", "Programming language", false),
        ]),
        ("get_snippet", "Retrieve a saved code snippet", vec![
            param("name", "string", "Snippet name", true),
        ]),
        ("list_snippets", "List all saved snippets", vec![
            param("tag", "string", "Filter by tag", false),
        ]),
        ("ingest_document", "Ingest a document into knowledge base", vec![
            param("path", "string", "Document path", true),
        ]),
    ];
    for (name, desc, params) in defs {
        reg.register(ToolDef {
            name: name.into(), description: desc.into(),
            category: "knowledge".into(), default_tier: RiskLevel::Green,
            min_tier: if name == "ingest_document" { "standard" } else { "lite" },
            parameters: params,
        }, Arc::new(Stub));
    }
}
