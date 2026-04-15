/// Phase 12 — RAG, Document Chat & Deep Knowledge Tests
/// Tests document chunking, RAG ingestion, retrieval, and knowledge base management.

use std::sync::Arc;

fn make_test_rag() -> (
    Arc<kria_core::memory::store::MemoryStore>,
    Arc<kria_core::memory::vectors::VectorIndex>,
    Arc<kria_core::memory::embeddings::EmbeddingModel>,
    Arc<kria_core::memory::rag::RagEngine>,
) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store = Arc::new(kria_core::memory::store::MemoryStore::open(tmp.path()).unwrap());
    let vectors = Arc::new(kria_core::memory::vectors::VectorIndex::in_memory(384));
    let embeddings = Arc::new(kria_core::memory::embeddings::EmbeddingModel::load(384).unwrap());
    let rag = Arc::new(kria_core::memory::rag::RagEngine::new(store.clone(), vectors.clone(), embeddings.clone()));
    (store, vectors, embeddings, rag)
}

// ── Chunking ──

#[test]
fn chunk_text_empty() {
    let chunks = kria_core::memory::rag::chunk_text("", 512, 64);
    assert!(chunks.is_empty());
}

#[test]
fn chunk_text_short() {
    let chunks = kria_core::memory::rag::chunk_text("hello world", 512, 64);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].0, 0);
    assert_eq!(chunks[0].1, "hello world");
}

#[test]
fn chunk_text_splits_long() {
    let text = "a ".repeat(500); // 1000 chars
    let chunks = kria_core::memory::rag::chunk_text(&text, 200, 20);
    assert!(chunks.len() >= 3, "expected at least 3 chunks, got {}", chunks.len());
}

#[test]
fn chunk_text_overlap() {
    let text = "word ".repeat(200); // 1000 chars
    let chunks = kria_core::memory::rag::chunk_text(&text, 300, 50);
    assert!(chunks.len() >= 2);
    // Verify the second chunk starts before the first one ends (overlap)
    if chunks.len() >= 2 {
        let end_first = chunks[0].0 + chunks[0].1.len();
        let start_second = chunks[1].0;
        assert!(start_second < end_first, "expected overlap: end_first={}, start_second={}", end_first, start_second);
    }
}

#[test]
fn chunk_text_preserves_content() {
    let text = "The quick brown fox jumps over the lazy dog. This is a test of the chunking system.";
    let chunks = kria_core::memory::rag::chunk_text(text, 50, 10);
    assert!(!chunks.is_empty());
    // All chunks should be non-empty
    for (_, c) in &chunks {
        assert!(!c.trim().is_empty());
    }
}

// ── Document storage ──

#[test]
fn store_and_retrieve_chunks() {
    let (store, _, _, _) = make_test_rag();
    let chunk = kria_core::memory::store::DocumentChunk {
        id: None,
        doc_id: "test_doc_1".to_string(),
        doc_name: "test.txt".to_string(),
        doc_type: "text".to_string(),
        chunk_index: 0,
        content: "Hello world this is a test chunk".to_string(),
        char_offset: 0,
        created_at: chrono::Utc::now(),
    };
    let id = store.store_chunk(&chunk).unwrap();
    assert!(id > 0);

    let retrieved = store.get_chunk(id).unwrap();
    assert!(retrieved.is_some());
    assert_eq!(retrieved.unwrap().content, "Hello world this is a test chunk");
}

#[test]
fn list_documents_empty() {
    let (store, _, _, _) = make_test_rag();
    let docs = store.list_documents().unwrap();
    assert!(docs.is_empty());
}

#[test]
fn list_documents_after_store() {
    let (store, _, _, _) = make_test_rag();
    let chunk = kria_core::memory::store::DocumentChunk {
        id: None,
        doc_id: "doc_abc123".to_string(),
        doc_name: "readme.md".to_string(),
        doc_type: "md".to_string(),
        chunk_index: 0,
        content: "content here".to_string(),
        char_offset: 0,
        created_at: chrono::Utc::now(),
    };
    store.store_chunk(&chunk).unwrap();
    let docs = store.list_documents().unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].1, "readme.md");
}

#[test]
fn delete_document_chunks() {
    let (store, _, _, _) = make_test_rag();
    for i in 0..3 {
        let chunk = kria_core::memory::store::DocumentChunk {
            id: None,
            doc_id: "doc_del".to_string(),
            doc_name: "file.txt".to_string(),
            doc_type: "text".to_string(),
            chunk_index: i,
            content: format!("chunk {i}"),
            char_offset: 0,
            created_at: chrono::Utc::now(),
        };
        store.store_chunk(&chunk).unwrap();
    }
    let deleted = store.delete_document_chunks("doc_del").unwrap();
    assert_eq!(deleted, 3);
    let docs = store.list_documents().unwrap();
    assert!(docs.is_empty());
}

// ── RAG Engine ──

#[test]
fn rag_ingest_and_list() {
    let (_, _, _, rag) = make_test_rag();
    let text = "Rust is a systems programming language. It provides memory safety without garbage collection.";
    let config = kria_core::memory::rag::ChunkConfig::default();
    let (doc_id, chunks) = rag.ingest("test.txt", "text", text, &config).unwrap();
    assert!(!doc_id.is_empty());
    assert!(chunks >= 1);

    let docs = rag.list_documents().unwrap();
    assert_eq!(docs.len(), 1);
}

#[test]
fn rag_ingest_empty_fails() {
    let (_, _, _, rag) = make_test_rag();
    let text = "";
    let config = kria_core::memory::rag::ChunkConfig::default();
    let result = rag.ingest("empty.txt", "text", text, &config);
    // Empty text should produce 0 chunks
    let (_, chunks) = result.unwrap();
    assert_eq!(chunks, 0);
}

#[test]
fn rag_delete_document() {
    let (_, _, _, rag) = make_test_rag();
    let text = "Some content for deletion test.";
    let config = kria_core::memory::rag::ChunkConfig::default();
    let (doc_id, _) = rag.ingest("delete_me.txt", "text", text, &config).unwrap();
    assert_eq!(rag.list_documents().unwrap().len(), 1);

    rag.delete_document(&doc_id).unwrap();
    assert_eq!(rag.list_documents().unwrap().len(), 0);
}

#[test]
fn rag_retrieve_after_ingest() {
    let (_, _, _, rag) = make_test_rag();
    let text = "Rust provides memory safety. It uses a borrow checker for compile-time guarantees. The ownership system prevents data races.";
    let config = kria_core::memory::rag::ChunkConfig::default();
    rag.ingest("rust_guide.txt", "text", text, &config).unwrap();

    let results = rag.retrieve("memory safety", 5).unwrap();
    // Should find at least 1 result mentioning memory safety
    assert!(!results.is_empty(), "expected at least 1 RAG result");
    assert_eq!(results[0].doc_name, "rust_guide.txt");
}

#[test]
fn rag_retrieve_empty_query() {
    let (_, _, _, rag) = make_test_rag();
    let results = rag.retrieve("", 5).unwrap();
    assert!(results.is_empty());
}

// ── Tool registration ──

#[test]
fn rag_tools_registered() {
    use kria_core::tools::registry::build_default_registry;
    let _reg = build_default_registry();
    // RAG tools require RagEngine, so build_default_registry doesn't include them
    // But we can test with the full builder
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store = Arc::new(kria_core::memory::store::MemoryStore::open(tmp.path()).unwrap());
    let vectors = Arc::new(kria_core::memory::vectors::VectorIndex::in_memory(384));
    let embeddings = Arc::new(kria_core::memory::embeddings::EmbeddingModel::load(384).unwrap());
    let rag = Arc::new(kria_core::memory::rag::RagEngine::new(store.clone(), vectors, embeddings));
    let full_reg = kria_core::tools::registry::build_registry_full(Some(store), Some(rag), None);
    assert!(full_reg.get_def("ingest_document_rag").is_some());
    assert!(full_reg.get_def("rag_query").is_some());
    assert!(full_reg.get_def("list_knowledge_base").is_some());
    assert!(full_reg.get_def("delete_knowledge_item").is_some());
}

#[test]
fn rag_tools_in_knowledge_category() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store = Arc::new(kria_core::memory::store::MemoryStore::open(tmp.path()).unwrap());
    let vectors = Arc::new(kria_core::memory::vectors::VectorIndex::in_memory(384));
    let embeddings = Arc::new(kria_core::memory::embeddings::EmbeddingModel::load(384).unwrap());
    let rag = Arc::new(kria_core::memory::rag::RagEngine::new(store.clone(), vectors, embeddings));
    let reg = kria_core::tools::registry::build_registry_full(Some(store), Some(rag), None);
    let knowledge_tools = reg.list_by_category("knowledge");
    // Should include original 8 + 4 RAG tools = 12
    assert!(knowledge_tools.len() >= 12, "expected at least 12 knowledge tools, got {}", knowledge_tools.len());
}

#[tokio::test]
async fn rag_query_tool_requires_query() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store = Arc::new(kria_core::memory::store::MemoryStore::open(tmp.path()).unwrap());
    let vectors = Arc::new(kria_core::memory::vectors::VectorIndex::in_memory(384));
    let embeddings = Arc::new(kria_core::memory::embeddings::EmbeddingModel::load(384).unwrap());
    let rag = Arc::new(kria_core::memory::rag::RagEngine::new(store.clone(), vectors, embeddings));
    let reg = kria_core::tools::registry::build_registry_full(Some(store), Some(rag), None);
    let handler = reg.get_handler("rag_query").unwrap();
    let result = handler.execute(serde_json::json!({})).await;
    assert!(!result.success);
    assert!(result.error.unwrap().contains("required"));
}

#[tokio::test]
async fn ingest_tool_invalid_path() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store = Arc::new(kria_core::memory::store::MemoryStore::open(tmp.path()).unwrap());
    let vectors = Arc::new(kria_core::memory::vectors::VectorIndex::in_memory(384));
    let embeddings = Arc::new(kria_core::memory::embeddings::EmbeddingModel::load(384).unwrap());
    let rag = Arc::new(kria_core::memory::rag::RagEngine::new(store.clone(), vectors, embeddings));
    let reg = kria_core::tools::registry::build_registry_full(Some(store), Some(rag), None);
    let handler = reg.get_handler("ingest_document_rag").unwrap();
    let result = handler.execute(serde_json::json!({ "path": "/nonexistent/file.pdf" })).await;
    assert!(!result.success);
    assert!(result.error.unwrap().contains("not found"));
}

#[test]
fn chunk_config_defaults() {
    let config = kria_core::memory::rag::ChunkConfig::default();
    assert_eq!(config.chunk_size, 512);
    assert_eq!(config.overlap, 64);
}
