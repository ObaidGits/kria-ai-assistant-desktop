use chrono::Utc;
use kria_core::memory::embeddings::EmbeddingModel;
use kria_core::memory::facts::FactManager;
use kria_core::memory::retrieval::ContextBuilder;
/// Phase 1 — Persistent Memory & Chat History tests
///
/// Validates: MemoryStore CRUD, session management, fact extraction,
/// knowledge tool wiring, embeddings (fallback), vector index, and
/// the ContextBuilder retrieval pipeline.
use kria_core::memory::store::{ConversationTurn, MemoryFact, MemoryStore};
use kria_core::memory::vectors::VectorIndex;
use kria_core::tools::registry;
use std::sync::Arc;

fn tmp_store() -> MemoryStore {
    MemoryStore::open(std::path::Path::new(":memory:")).expect("in-memory store")
}

// ── MemoryStore: Conversation persistence ──────────────────────

#[test]
fn store_and_retrieve_turns() {
    let store = tmp_store();
    let sid = "test-session-1";
    let turn = ConversationTurn {
        id: None,
        session_id: sid.into(),
        role: "user".into(),
        content: "Hello KRIA".into(),
        tool_name: None,
        tool_result: None,
        tokens_used: Some(5),
        timestamp: Utc::now(),
    };
    let id = store.store_turn(&turn).unwrap();
    assert!(id > 0);

    let turns = store.get_recent_turns(sid, 10).unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].content, "Hello KRIA");
    assert_eq!(turns[0].role, "user");
}

#[test]
fn multiple_turns_ordered_correctly() {
    let store = tmp_store();
    let sid = "session-order";
    for i in 0..5 {
        store
            .store_turn(&ConversationTurn {
                id: None,
                session_id: sid.into(),
                role: if i % 2 == 0 { "user" } else { "assistant" }.into(),
                content: format!("msg-{}", i),
                tool_name: None,
                tool_result: None,
                tokens_used: None,
                timestamp: Utc::now(),
            })
            .unwrap();
    }
    let turns = store.get_recent_turns(sid, 10).unwrap();
    assert_eq!(turns.len(), 5);
    // Should be in chronological order (oldest first)
    assert_eq!(turns[0].content, "msg-0");
    assert_eq!(turns[4].content, "msg-4");
}

#[test]
fn list_sessions_returns_all() {
    let store = tmp_store();
    for sid in &["s1", "s2", "s3"] {
        store
            .store_turn(&ConversationTurn {
                id: None,
                session_id: sid.to_string(),
                role: "user".into(),
                content: "hi".into(),
                tool_name: None,
                tool_result: None,
                tokens_used: None,
                timestamp: Utc::now(),
            })
            .unwrap();
    }
    let sessions = store.list_sessions().unwrap();
    assert_eq!(sessions.len(), 3);
}

#[test]
fn delete_session_removes_all_turns() {
    let store = tmp_store();
    let sid = "doomed-session";
    for _ in 0..3 {
        store
            .store_turn(&ConversationTurn {
                id: None,
                session_id: sid.into(),
                role: "user".into(),
                content: "temp".into(),
                tool_name: None,
                tool_result: None,
                tokens_used: None,
                timestamp: Utc::now(),
            })
            .unwrap();
    }
    let deleted = store.delete_session(sid).unwrap();
    assert_eq!(deleted, 3);
    assert!(store.get_recent_turns(sid, 10).unwrap().is_empty());
}

#[test]
fn search_conversations_fts() {
    let store = tmp_store();
    store
        .store_turn(&ConversationTurn {
            id: None,
            session_id: "s".into(),
            role: "user".into(),
            content: "Tell me about quantum computing".into(),
            tool_name: None,
            tool_result: None,
            tokens_used: None,
            timestamp: Utc::now(),
        })
        .unwrap();
    store
        .store_turn(&ConversationTurn {
            id: None,
            session_id: "s".into(),
            role: "assistant".into(),
            content: "Quantum computing uses qubits".into(),
            tool_name: None,
            tool_result: None,
            tokens_used: None,
            timestamp: Utc::now(),
        })
        .unwrap();

    let results = store.search_conversations("quantum", 10).unwrap();
    assert!(!results.is_empty(), "FTS should find 'quantum'");
}

// ── MemoryStore: Facts ─────────────────────────────────────────

#[test]
fn store_and_search_facts() {
    let store = tmp_store();
    let fact = MemoryFact {
        id: None,
        text: "User prefers dark mode".into(),
        category: "preference".into(),
        source: "conversation".into(),
        created_at: Utc::now(),
        last_accessed: Utc::now(),
        access_count: 0,
        decay_score: 1.0,
    };
    let id = store.store_fact(&fact).unwrap();
    assert!(id > 0);

    let found = store.search_facts("dark mode", 5).unwrap();
    assert!(!found.is_empty());
    assert!(found[0].text.contains("dark mode"));
}

#[test]
fn all_facts_with_decay_filters() {
    let store = tmp_store();
    store
        .store_fact(&MemoryFact {
            id: None,
            text: "high decay".into(),
            category: "test".into(),
            source: "test".into(),
            created_at: Utc::now(),
            last_accessed: Utc::now(),
            access_count: 0,
            decay_score: 0.9,
        })
        .unwrap();
    store
        .store_fact(&MemoryFact {
            id: None,
            text: "low decay".into(),
            category: "test".into(),
            source: "test".into(),
            created_at: Utc::now(),
            last_accessed: Utc::now(),
            access_count: 0,
            decay_score: 0.1,
        })
        .unwrap();

    let high = store.all_facts_with_decay(0.5).unwrap();
    assert_eq!(high.len(), 1);
    assert!(high[0].text.contains("high decay"));
}

#[test]
fn update_fact_access_increments() {
    let store = tmp_store();
    let id = store
        .store_fact(&MemoryFact {
            id: None,
            text: "access test".into(),
            category: "t".into(),
            source: "t".into(),
            created_at: Utc::now(),
            last_accessed: Utc::now(),
            access_count: 0,
            decay_score: 1.0,
        })
        .unwrap();

    store.update_fact_access(id).unwrap();
    store.update_fact_access(id).unwrap();
    let fact = store.get_fact(id).unwrap().expect("fact exists");
    assert_eq!(fact.access_count, 2);
}

// ── MemoryStore: Snippets ──────────────────────────────────────

#[test]
fn save_and_get_snippet() {
    let store = tmp_store();
    store
        .save_snippet(
            "hello_world",
            "fn main() { println!(\"Hello\"); }",
            "rust",
            &[],
        )
        .unwrap();

    let (content, lang, _tags) = store
        .get_snippet("hello_world")
        .unwrap()
        .expect("snippet exists");
    assert_eq!(lang, "rust");
    assert!(content.contains("println"));
}

#[test]
fn list_snippets_returns_names() {
    let store = tmp_store();
    store.save_snippet("a", "code", "py", &[]).unwrap();
    store.save_snippet("b", "code", "rs", &[]).unwrap();
    let names = store.list_snippets(None).unwrap();
    assert_eq!(names.len(), 2);
}

// ── MemoryStore: Preferences ───────────────────────────────────

#[test]
fn set_and_get_preference() {
    let store = tmp_store();
    store.set_preference("theme", "dark").unwrap();
    assert_eq!(store.get_preference("theme").unwrap().unwrap(), "dark");
}

#[test]
fn preference_overwrite() {
    let store = tmp_store();
    store.set_preference("lang", "en").unwrap();
    store.set_preference("lang", "de").unwrap();
    assert_eq!(store.get_preference("lang").unwrap().unwrap(), "de");
}

// ── EmbeddingModel ─────────────────────────────────────────────

#[test]
fn embedding_fallback_produces_correct_dim() {
    let model = EmbeddingModel::load(384).unwrap();
    assert!(!model.is_onnx_loaded(), "no ONNX model in test env");
    let vec = model.embed("hello world").unwrap();
    assert_eq!(vec.len(), 384);
}

#[test]
fn embedding_fallback_is_normalized() {
    let model = EmbeddingModel::load(128).unwrap();
    let vec = model.embed("test embedding").unwrap();
    let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!(
        (norm - 1.0).abs() < 0.01,
        "should be L2-normalized, got norm={}",
        norm
    );
}

#[test]
fn embedding_different_texts_differ() {
    let model = EmbeddingModel::load(128).unwrap();
    let v1 = model.embed("rust programming").unwrap();
    let v2 = model.embed("chocolate cake recipe").unwrap();
    let sim: f32 = v1.iter().zip(&v2).map(|(a, b)| a * b).sum();
    assert!(
        sim < 0.99,
        "different texts should produce different embeddings, sim={}",
        sim
    );
}

// ── VectorIndex ────────────────────────────────────────────────

#[test]
fn vector_add_and_search() {
    let idx = VectorIndex::in_memory(3);
    idx.add(1, vec![1.0, 0.0, 0.0]).unwrap();
    idx.add(2, vec![0.0, 1.0, 0.0]).unwrap();
    idx.add(3, vec![0.9, 0.1, 0.0]).unwrap();

    let results = idx.search(&[1.0, 0.0, 0.0], 2);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].0, 1, "exact match should be first");
    assert_eq!(results[1].0, 3, "closest should be second");
}

#[test]
fn vector_remove_excludes_from_search() {
    let idx = VectorIndex::in_memory(3);
    idx.add(1, vec![1.0, 0.0, 0.0]).unwrap();
    idx.add(2, vec![1.0, 0.0, 0.0]).unwrap();
    idx.remove(1);
    let results = idx.search(&[1.0, 0.0, 0.0], 5);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, 2);
}

// ── FactManager ────────────────────────────────────────────────

#[test]
fn fact_manager_deduplicates() {
    let store = tmp_store();
    let vectors = VectorIndex::in_memory(128);
    let embeddings = EmbeddingModel::load(128).unwrap();
    let mgr = FactManager::new(&store, &vectors, &embeddings);

    let id1 = mgr
        .add_fact("user likes rust", "preference", "test")
        .unwrap();
    assert!(id1.is_some(), "first insert should succeed");

    // Exact same text → should deduplicate (similarity ≥ 0.92)
    let id2 = mgr
        .add_fact("user likes rust", "preference", "test")
        .unwrap();
    assert!(id2.is_none(), "duplicate should be skipped");
}

#[test]
fn fact_manager_extract_from_turn() {
    let store = tmp_store();
    let vectors = VectorIndex::in_memory(128);
    let embeddings = EmbeddingModel::load(128).unwrap();
    let mgr = FactManager::new(&store, &vectors, &embeddings);

    // Message with a "I prefer" pattern → should extract
    let ids = mgr
        .extract_from_turn("I prefer dark mode for coding", "OK, noted!")
        .unwrap();
    assert!(
        !ids.is_empty(),
        "should extract fact from 'I prefer' pattern"
    );

    // Message without any pattern → should not extract
    let ids2 = mgr
        .extract_from_turn("What's the weather?", "I can't check weather yet.")
        .unwrap();
    assert!(ids2.is_empty(), "no pattern → no extraction");
}

// ── Knowledge tools wired to store ─────────────────────────────

#[test]
fn knowledge_tools_register_with_store() {
    let store = Arc::new(tmp_store());
    let reg = registry::build_registry_with_store(Some(store));
    assert!(reg.get_def("remember_fact").is_some());
    assert!(reg.get_def("recall_fact").is_some());
    assert!(reg.get_def("search_knowledge").is_some());
    assert!(reg.get_def("save_snippet").is_some());
    assert!(reg.get_def("get_snippet").is_some());
    assert!(reg.get_def("list_snippets").is_some());
    assert!(reg.get_def("list_remembered").is_some());
    assert!(reg.get_def("ingest_document").is_some());
}

#[test]
fn knowledge_stubs_register_without_store() {
    let reg = registry::build_default_registry();
    assert!(reg.get_def("remember_fact").is_some());
    assert!(reg.get_def("recall_fact").is_some());
}

#[tokio::test]
async fn remember_fact_tool_persists() {
    let store = Arc::new(tmp_store());
    let reg = registry::build_registry_with_store(Some(store.clone()));
    let handler = reg.get_handler("remember_fact").unwrap().clone();

    let result = handler
        .execute(serde_json::json!({
            "key": "food",
            "value": "user loves sushi"
        }))
        .await;

    assert!(result.success);
    assert_eq!(result.data["stored"], true);
    let fact_id = result.data["fact_id"].as_i64().unwrap();
    assert!(fact_id > 0);

    // Verify it's actually in the store
    let fact = store.get_fact(fact_id).unwrap().expect("fact persisted");
    assert!(fact.text.contains("sushi"));
}

#[tokio::test]
async fn recall_fact_tool_searches() {
    let store = Arc::new(tmp_store());
    store
        .store_fact(&MemoryFact {
            id: None,
            text: "User enjoys hiking in mountains".into(),
            category: "hobby".into(),
            source: "test".into(),
            created_at: Utc::now(),
            last_accessed: Utc::now(),
            access_count: 0,
            decay_score: 1.0,
        })
        .unwrap();

    let reg = registry::build_registry_with_store(Some(store));
    let handler = reg.get_handler("recall_fact").unwrap().clone();

    let result = handler
        .execute(serde_json::json!({ "query": "hiking" }))
        .await;
    assert!(result.success);
    assert!(result.data["count"].as_u64().unwrap() >= 1);
}

#[tokio::test]
async fn save_and_get_snippet_tools() {
    let store = Arc::new(tmp_store());
    let reg = registry::build_registry_with_store(Some(store));

    let save = reg.get_handler("save_snippet").unwrap().clone();
    let get = reg.get_handler("get_snippet").unwrap().clone();

    let res = save
        .execute(serde_json::json!({
            "name": "greet", "content": "print('hello')", "language": "python"
        }))
        .await;
    assert!(res.success);

    let res2 = get.execute(serde_json::json!({ "name": "greet" })).await;
    assert!(res2.success);
    assert_eq!(res2.data["language"], "python");
    assert!(res2.data["content"].as_str().unwrap().contains("hello"));
}

// ── ContextBuilder retrieval ───────────────────────────────────

#[test]
fn context_builder_retrieves_relevant_facts() {
    let store = tmp_store();
    let vectors = VectorIndex::in_memory(128);
    let embeddings = EmbeddingModel::load(128).unwrap();

    // Add some facts with vectors
    for text in ["Rust is fast", "Python is flexible", "KRIA uses local LLMs"]
        .iter()
    {
        let id = store
            .store_fact(&MemoryFact {
                id: None,
                text: text.to_string(),
                category: "tech".into(),
                source: "test".into(),
                created_at: Utc::now(),
                last_accessed: Utc::now(),
                access_count: 0,
                decay_score: 1.0,
            })
            .unwrap();
        let vec = embeddings.embed(text).unwrap();
        vectors.add(id, vec).unwrap();
    }

    let builder = ContextBuilder::new(&store, &vectors, &embeddings);
    let context = builder.retrieve("Tell me about Rust", 5).unwrap();
    assert!(!context.is_empty(), "should retrieve some context");
}
