//! Feature tests for the KRIA memory subsystem.
//!
//! Uses an in-memory SQLite database so tests are fully idempotent.
//! Covers conversation storage, fact management, vector search,
//! and decay pruning.

use chrono::Utc;
use kria_core::memory::store::{ConversationTurn, MemoryFact, MemoryStore};
use kria_core::memory::vectors::VectorIndex;
use kria_core::memory::embeddings::EmbeddingModel;
use std::path::Path;

// ── MemoryStore — conversation turns ────────────────────────────────

fn make_turn(session: &str, role: &str, content: &str) -> ConversationTurn {
    ConversationTurn {
        id: None,
        session_id: session.into(),
        role: role.into(),
        content: content.into(),
        tool_name: None,
        tool_result: None,
        tokens_used: Some(5),
        timestamp: Utc::now(),
    }
}

fn make_fact(text: &str, category: &str) -> MemoryFact {
    MemoryFact {
        id: None,
        text: text.into(),
        category: category.into(),
        source: "test".into(),
        created_at: Utc::now(),
        last_accessed: Utc::now(),
        access_count: 0,
        decay_score: 1.0,
    }
}

#[test]
fn store_and_retrieve_conversation_turns() {
    let store = MemoryStore::open(Path::new(":memory:")).unwrap();

    store.store_turn(&make_turn("sess-1", "user", "Hello KRIA")).unwrap();
    store.store_turn(&make_turn("sess-1", "assistant", "Hi there!")).unwrap();

    let turns = store.get_recent_turns("sess-1", 10).unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].role, "user");
    assert_eq!(turns[1].role, "assistant");
    assert_eq!(turns[1].content, "Hi there!");
}

#[test]
fn get_recent_turns_respects_limit() {
    let store = MemoryStore::open(Path::new(":memory:")).unwrap();

    for i in 0..20 {
        store.store_turn(&make_turn("sess-2", "user", &format!("msg {i}"))).unwrap();
    }

    let turns = store.get_recent_turns("sess-2", 5).unwrap();
    assert_eq!(turns.len(), 5);
}

#[test]
fn list_sessions_returns_stored_sessions() {
    let store = MemoryStore::open(Path::new(":memory:")).unwrap();

    store.store_turn(&make_turn("sess-a", "user", "hello")).unwrap();
    store.store_turn(&make_turn("sess-b", "user", "world")).unwrap();

    let sessions = store.list_sessions().unwrap();
    assert!(sessions.len() >= 2);
}

#[test]
fn delete_session_removes_all_turns() {
    let store = MemoryStore::open(Path::new(":memory:")).unwrap();

    store.store_turn(&make_turn("sess-del", "user", "bye")).unwrap();
    store.delete_session("sess-del").unwrap();

    let turns = store.get_recent_turns("sess-del", 10).unwrap();
    assert!(turns.is_empty());
}

// ── MemoryStore — facts ─────────────────────────────────────────────

#[test]
fn store_and_search_facts() {
    let store = MemoryStore::open(Path::new(":memory:")).unwrap();

    store.store_fact(&make_fact("Rust was created by Mozilla", "tech")).unwrap();
    store.store_fact(&make_fact("The user prefers dark themes", "preference")).unwrap();

    let results = store.search_facts("Rust", 10).unwrap();
    assert!(!results.is_empty());
    assert!(results[0].text.contains("Rust"));
}

// ── MemoryStore — preferences ───────────────────────────────────────

#[test]
fn set_and_get_preferences() {
    let store = MemoryStore::open(Path::new(":memory:")).unwrap();

    store.set_preference("theme", "dark").unwrap();
    let val = store.get_preference("theme").unwrap();
    assert_eq!(val.as_deref(), Some("dark"));
}

#[test]
fn overwrite_preference() {
    let store = MemoryStore::open(Path::new(":memory:")).unwrap();

    store.set_preference("lang", "en").unwrap();
    store.set_preference("lang", "fr").unwrap();
    let val = store.get_preference("lang").unwrap();
    assert_eq!(val.as_deref(), Some("fr"));
}

#[test]
fn missing_preference_returns_none() {
    let store = MemoryStore::open(Path::new(":memory:")).unwrap();
    let val = store.get_preference("nonexistent").unwrap();
    assert!(val.is_none());
}

// ── VectorIndex ─────────────────────────────────────────────────────

#[test]
fn vector_index_add_and_search() {
    let idx = VectorIndex::in_memory(3);

    idx.add(1, vec![1.0, 0.0, 0.0]).unwrap();
    idx.add(2, vec![0.0, 1.0, 0.0]).unwrap();
    idx.add(3, vec![0.0, 0.0, 1.0]).unwrap();

    let results = idx.search(&[0.9, 0.1, 0.0], 2);
    assert_eq!(results.len(), 2);
    // The first result should be closest to [1, 0, 0]
    assert_eq!(results[0].0, 1);
}

#[test]
fn vector_index_remove() {
    let idx = VectorIndex::in_memory(3);
    idx.add(1, vec![1.0, 0.0, 0.0]).unwrap();
    idx.add(2, vec![0.0, 1.0, 0.0]).unwrap();

    idx.remove(1);
    assert_eq!(idx.len(), 1);

    let results = idx.search(&[1.0, 0.0, 0.0], 5);
    // Only id=2 should remain
    assert!(results.iter().all(|(id, _)| *id != 1));
}

#[test]
fn vector_index_empty_search_returns_empty() {
    let idx = VectorIndex::in_memory(3);
    let results = idx.search(&[1.0, 0.0, 0.0], 5);
    assert!(results.is_empty());
}

// ── EmbeddingModel ──────────────────────────────────────────────────

#[test]
fn embedding_produces_correct_dimension() {
    let model = EmbeddingModel::load(384).unwrap();
    let vec = model.embed("hello world").unwrap();
    assert_eq!(vec.len(), 384);
}

#[test]
fn embedding_is_deterministic() {
    let model = EmbeddingModel::load(384).unwrap();
    let a = model.embed("same text").unwrap();
    let b = model.embed("same text").unwrap();
    assert_eq!(a, b);
}

#[test]
fn embedding_differs_for_different_input() {
    let model = EmbeddingModel::load(384).unwrap();
    let a = model.embed("text a").unwrap();
    let b = model.embed("text b").unwrap();
    assert_ne!(a, b);
}
