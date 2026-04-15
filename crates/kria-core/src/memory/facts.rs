use crate::memory::store::{MemoryStore, MemoryFact};
use crate::memory::vectors::VectorIndex;
use crate::memory::embeddings::EmbeddingModel;
use chrono::Utc;

/// LLM-driven fact extraction and management.
/// Replaces Mem0: extracts user facts from conversation, stores in SQLite + vector index.
pub struct FactManager<'a> {
    store: &'a MemoryStore,
    vectors: &'a VectorIndex,
    embeddings: &'a EmbeddingModel,
    similarity_threshold: f32,
}

impl<'a> FactManager<'a> {
    pub fn new(
        store: &'a MemoryStore,
        vectors: &'a VectorIndex,
        embeddings: &'a EmbeddingModel,
    ) -> Self {
        Self {
            store,
            vectors,
            embeddings,
            similarity_threshold: 0.92,
        }
    }

    /// Add a new fact. Deduplicates against existing facts.
    pub fn add_fact(&self, text: &str, category: &str, source: &str) -> anyhow::Result<Option<i64>> {
        // Check for duplicates via vector similarity
        let vec = self.embeddings.embed(text)?;
        let similar = self.vectors.search(&vec, 3);

        for (existing_id, sim) in &similar {
            if *sim >= self.similarity_threshold {
                tracing::debug!(
                    existing_id,
                    similarity = sim,
                    "duplicate fact detected, skipping"
                );
                // Update access on existing instead
                let _ = self.store.update_fact_access(*existing_id);
                return Ok(None);
            }
        }

        let now = Utc::now();
        let fact = MemoryFact {
            id: None,
            text: text.to_string(),
            category: category.to_string(),
            source: source.to_string(),
            created_at: now,
            last_accessed: now,
            access_count: 0,
            decay_score: 1.0,
        };

        let id = self.store.store_fact(&fact)?;
        self.vectors.add(id, vec)?;

        tracing::info!(id, category, "stored new fact");
        Ok(Some(id))
    }

    /// Update an existing fact's text.
    pub fn update_fact(&self, id: i64, new_text: &str) -> anyhow::Result<()> {
        // Remove old vector
        self.vectors.remove(id);

        // Delete and re-insert (simpler than UPDATE for FTS)
        self.store.delete_fact(id)?;

        let vec = self.embeddings.embed(new_text)?;
        let now = Utc::now();
        let fact = MemoryFact {
            id: None,
            text: new_text.to_string(),
            category: "general".to_string(),
            source: "updated".to_string(),
            created_at: now,
            last_accessed: now,
            access_count: 0,
            decay_score: 1.0,
        };
        let new_id = self.store.store_fact(&fact)?;
        self.vectors.add(new_id, vec)?;

        tracing::info!(old_id = id, new_id, "updated fact");
        Ok(())
    }

    /// Delete a fact and its vector.
    pub fn delete_fact(&self, id: i64) -> anyhow::Result<()> {
        self.vectors.remove(id);
        self.store.delete_fact(id)?;
        tracing::info!(id, "deleted fact");
        Ok(())
    }

    /// Extract facts from a conversation turn (placeholder for LLM-driven extraction).
    pub fn extract_from_turn(&self, user_message: &str, _assistant_response: &str) -> anyhow::Result<Vec<i64>> {
        // TODO: Call LLM with extraction prompt to identify user facts
        // For now, simple heuristic: detect "I prefer", "I like", "my name is", etc.
        let mut added = Vec::new();
        let lower = user_message.to_lowercase();

        let patterns = [
            "i prefer ", "i like ", "my name is ", "i am a ", "i work ",
            "i use ", "my favorite ", "i always ", "i never ", "i live ",
        ];

        for pattern in &patterns {
            if lower.contains(pattern) {
                if let Some(id) = self.add_fact(user_message, "user_preference", "conversation")? {
                    added.push(id);
                }
                break;
            }
        }

        Ok(added)
    }
}
