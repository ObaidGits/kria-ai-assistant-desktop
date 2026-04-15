use crate::memory::store::{MemoryStore, MemoryFact};
use crate::memory::vectors::VectorIndex;
use crate::memory::embeddings::EmbeddingModel;

/// Hybrid context builder: vector similarity + relational scoring.
pub struct ContextBuilder<'a> {
    store: &'a MemoryStore,
    vectors: &'a VectorIndex,
    embeddings: &'a EmbeddingModel,
}

#[derive(Debug, Clone)]
pub struct RetrievedFact {
    pub fact: MemoryFact,
    pub score: f64,
}

impl<'a> ContextBuilder<'a> {
    pub fn new(store: &'a MemoryStore, vectors: &'a VectorIndex, embeddings: &'a EmbeddingModel) -> Self {
        Self { store, vectors, embeddings }
    }

    /// Build context for a user message: retrieve relevant facts.
    ///
    /// 1. Embed the query
    /// 2. ANN search (k=20 candidates)
    /// 3. Fetch facts from SQLite
    /// 4. Score: similarity*0.5 + recency*0.25 + frequency*0.15 + link_strength*0.1
    /// 5. Return top `limit` results
    pub fn retrieve(&self, query: &str, limit: usize) -> anyhow::Result<Vec<RetrievedFact>> {
        let query_vec = self.embeddings.embed(query)?;
        let candidates = self.vectors.search(&query_vec, 20);

        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        let now = chrono::Utc::now();
        let mut scored: Vec<RetrievedFact> = Vec::new();

        for (fact_id, similarity) in &candidates {
            let fact = match self.store.get_fact(*fact_id)? {
                Some(f) => f,
                None => continue,
            };

            // Skip low-decay facts
            if fact.decay_score < 0.1 {
                continue;
            }

            // Recency: hours since last access, normalized
            let hours = (now - fact.last_accessed).num_hours().max(0) as f64;
            let recency = 1.0 / (1.0 + hours / 168.0); // 1-week half-life

            // Frequency: log-scaled access count
            let frequency = (fact.access_count as f64 + 1.0).ln() / 10.0;

            // Link strength: average strength of connected links
            let links = self.store.get_links(fact.id.unwrap_or(0)).unwrap_or_default();
            let link_strength = if links.is_empty() {
                0.0
            } else {
                links.iter().map(|l| l.strength).sum::<f64>() / links.len() as f64
            };

            let score = (*similarity as f64) * 0.5
                + recency * 0.25
                + frequency * 0.15
                + link_strength * 0.1;

            scored.push(RetrievedFact { fact, score });
        }

        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);

        // Update access counts for retrieved facts
        for rf in &scored {
            if let Some(id) = rf.fact.id {
                let _ = self.store.update_fact_access(id);
            }
        }

        Ok(scored)
    }
}
