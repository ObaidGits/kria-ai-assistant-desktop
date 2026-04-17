use std::sync::Arc;
use async_trait::async_trait;
use crate::infra::ToolResult;
use crate::safety::RiskLevel;
use crate::tools::registry::{ToolRegistry, ToolDef, ToolHandler, ParamDef};
use crate::memory::rag::RagEngine;

fn param(name: &str, ty: &str, desc: &str, required: bool) -> ParamDef {
    ParamDef { name: name.into(), param_type: ty.into(), description: desc.into(), required, default: None }
}

struct IngestDocument { rag: Arc<RagEngine> }
#[async_trait] impl ToolHandler for IngestDocument {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let path = params["path"].as_str().unwrap_or("");
        if path.is_empty() {
            return ToolResult::err("path is required");
        }
        let file_path = std::path::Path::new(path);
        if !file_path.exists() {
            return ToolResult::err(format!("file not found: {path}"));
        }

        let name = file_path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(path)
            .to_string();
        let doc_type = file_path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("text")
            .to_string();

        // Read file content
        let text = match std::fs::read_to_string(file_path) {
            Ok(t) => t,
            Err(e) => return ToolResult::err(format!("failed to read file: {e}")),
        };

        if text.trim().is_empty() {
            return ToolResult::err("file is empty");
        }

        let chunk_size = params["chunk_size"].as_u64().unwrap_or(512) as usize;
        let overlap = params["overlap"].as_u64().unwrap_or(64) as usize;
        let config = crate::memory::rag::ChunkConfig { chunk_size, overlap };

        match self.rag.ingest(&name, &doc_type, &text, &config) {
            Ok((doc_id, chunks)) => ToolResult::ok(serde_json::json!({
                "doc_id": doc_id,
                "name": name,
                "chunks": chunks,
                "characters": text.len(),
            })),
            Err(e) => ToolResult::err(format!("ingestion failed: {e}")),
        }
    }
}

struct RagQuery { rag: Arc<RagEngine> }
#[async_trait] impl ToolHandler for RagQuery {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let query = params["query"].as_str().unwrap_or("");
        if query.is_empty() {
            return ToolResult::err("query is required");
        }
        let limit = params["limit"].as_u64().unwrap_or(5) as usize;

        match self.rag.retrieve(query, limit) {
            Ok(results) => {
                let citations: Vec<serde_json::Value> = results.iter().map(|r| {
                    serde_json::json!({
                        "content": r.content,
                        "source": r.doc_name,
                        "doc_id": r.doc_id,
                        "chunk_index": r.chunk_index,
                        "score": (r.score * 100.0).round() / 100.0,
                    })
                }).collect();
                // Build a context string for the LLM
                let context: String = results.iter().enumerate().map(|(i, r)| {
                    format!("[{}] (from {}, chunk {}): {}", i + 1, r.doc_name, r.chunk_index, r.content)
                }).collect::<Vec<_>>().join("\n\n");

                ToolResult::ok(serde_json::json!({
                    "results": citations,
                    "context": context,
                    "count": results.len(),
                }))
            }
            Err(e) => ToolResult::err(format!("retrieval failed: {e}")),
        }
    }
}

struct ListKnowledgeBase { rag: Arc<RagEngine> }
#[async_trait] impl ToolHandler for ListKnowledgeBase {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        match self.rag.list_documents() {
            Ok(docs) => {
                let items: Vec<serde_json::Value> = docs.iter().map(|(id, name, dtype, chunks)| {
                    serde_json::json!({
                        "doc_id": id,
                        "name": name,
                        "type": dtype,
                        "chunks": chunks,
                    })
                }).collect();
                ToolResult::ok(serde_json::json!({
                    "documents": items,
                    "count": items.len(),
                }))
            }
            Err(e) => ToolResult::err(format!("failed to list: {e}")),
        }
    }
}

struct DeleteKnowledgeItem { rag: Arc<RagEngine> }
#[async_trait] impl ToolHandler for DeleteKnowledgeItem {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let doc_id = params["doc_id"].as_str().unwrap_or("");
        if doc_id.is_empty() {
            return ToolResult::err("doc_id is required");
        }
        match self.rag.delete_document(doc_id) {
            Ok(deleted) => ToolResult::ok(serde_json::json!({ "deleted_chunks": deleted, "doc_id": doc_id })),
            Err(e) => ToolResult::err(format!("delete failed: {e}")),
        }
    }
}

pub fn register(reg: &ToolRegistry, rag: Arc<RagEngine>) {
    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        (ToolDef {
            name: "ingest_document_rag".into(), description: "Ingest a document into the knowledge base with chunking and vector embedding for RAG".into(),
            category: "knowledge".into(), default_tier: RiskLevel::Green, min_tier: "standard",
            parameters: vec![
                param("path", "string", "Path to the file to ingest", true),
                param("chunk_size", "integer", "Chunk size in characters (default: 512)", false),
                param("overlap", "integer", "Overlap between chunks (default: 64)", false),
            ],
        }, Arc::new(IngestDocument { rag: rag.clone() })),
        (ToolDef {
            name: "rag_query".into(), description: "Query the knowledge base using hybrid vector + keyword search with citations".into(),
            category: "knowledge".into(), default_tier: RiskLevel::Green, min_tier: "standard",
            parameters: vec![
                param("query", "string", "Question or search query", true),
                param("limit", "integer", "Max results to return (default: 5)", false),
            ],
        }, Arc::new(RagQuery { rag: rag.clone() })),
        (ToolDef {
            name: "list_knowledge_base".into(), description: "List all documents in the RAG knowledge base".into(),
            category: "knowledge".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![],
        }, Arc::new(ListKnowledgeBase { rag: rag.clone() })),
        (ToolDef {
            name: "delete_knowledge_item".into(), description: "Remove a document from the knowledge base".into(),
            category: "knowledge".into(), default_tier: RiskLevel::Yellow, min_tier: "standard",
            parameters: vec![
                param("doc_id", "string", "Document ID to delete", true),
            ],
        }, Arc::new(DeleteKnowledgeItem { rag })),
    ];
    for (def, handler) in tools { reg.register(def, handler); }
}
