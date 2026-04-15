/// RAG (Retrieval-Augmented Generation) — document chunking, hybrid retrieval, citations.
use crate::memory::store::{MemoryStore, DocumentChunk};
use crate::memory::vectors::VectorIndex;
use crate::memory::embeddings::EmbeddingModel;
use chrono::Utc;
use std::sync::Arc;

/// Configuration for document chunking.
pub struct ChunkConfig {
    pub chunk_size: usize,
    pub overlap: usize,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self { chunk_size: 512, overlap: 64 }
    }
}

/// A retrieved chunk with citation metadata.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RagResult {
    pub content: String,
    pub doc_name: String,
    pub doc_id: String,
    pub chunk_index: i32,
    pub score: f64,
}

/// RAG engine: ingest documents, retrieve with hybrid scoring.
pub struct RagEngine {
    store: Arc<MemoryStore>,
    vectors: Arc<VectorIndex>,
    embeddings: Arc<EmbeddingModel>,
}

impl RagEngine {
    pub fn new(store: Arc<MemoryStore>, vectors: Arc<VectorIndex>, embeddings: Arc<EmbeddingModel>) -> Self {
        Self { store, vectors, embeddings }
    }

    /// Ingest text into the knowledge base: chunk → embed → store.
    /// Returns the doc_id and number of chunks created.
    pub fn ingest(&self, name: &str, doc_type: &str, text: &str, config: &ChunkConfig) -> anyhow::Result<(String, usize)> {
        let doc_id = format!("doc_{}", uuid::Uuid::new_v4().to_string().replace('-', "")[..12].to_string());

        // Delete existing chunks for same doc name (re-ingest)
        let existing = self.store.list_documents()?;
        for (eid, ename, _, _) in &existing {
            if ename == name {
                self.delete_document(eid)?;
            }
        }

        let chunks = chunk_text(text, config.chunk_size, config.overlap);
        let now = Utc::now();
        let mut count = 0;

        for (i, (offset, chunk_text)) in chunks.iter().enumerate() {
            let chunk = DocumentChunk {
                id: None,
                doc_id: doc_id.clone(),
                doc_name: name.to_string(),
                doc_type: doc_type.to_string(),
                chunk_index: i as i32,
                content: chunk_text.clone(),
                char_offset: *offset as i64,
                created_at: now,
            };

            let chunk_id = self.store.store_chunk(&chunk)?;

            // Embed and index
            let vec = self.embeddings.embed(chunk_text)?;
            // Use negative IDs to separate chunk vectors from fact vectors
            let vector_id = -(chunk_id);
            self.vectors.add(vector_id, vec)?;
            count += 1;
        }

        tracing::info!(doc_id = %doc_id, name, chunks = count, "document ingested");
        Ok((doc_id, count))
    }

    /// Hybrid RAG retrieval: vector similarity + keyword search.
    pub fn retrieve(&self, query: &str, limit: usize) -> anyhow::Result<Vec<RagResult>> {
        let query_vec = self.embeddings.embed(query)?;

        // Vector search over chunk vectors (negative IDs)
        let all_candidates = self.vectors.search(&query_vec, limit * 3);
        let vector_hits: Vec<(i64, f32)> = all_candidates.into_iter()
            .filter(|(id, _)| *id < 0) // Only chunk vectors
            .map(|(id, sim)| (-id, sim)) // Convert back to chunk ID
            .collect();

        // Keyword search
        // Build FTS5-safe query: quote each word
        let fts_query = query.split_whitespace()
            .map(|w| {
                let clean: String = w.chars().filter(|c| c.is_alphanumeric()).collect();
                if clean.is_empty() { String::new() } else { format!("\"{}\"", clean) }
            })
            .filter(|w| !w.is_empty())
            .collect::<Vec<_>>()
            .join(" OR ");

        let keyword_hits = if !fts_query.is_empty() {
            self.store.search_chunks(&fts_query, limit * 2).unwrap_or_default()
        } else {
            Vec::new()
        };

        // Merge scores: vector (0.6) + keyword (0.3) + recency (0.1)
        let mut scored: std::collections::HashMap<i64, (f64, Option<DocumentChunk>)> = std::collections::HashMap::new();

        for (chunk_id, sim) in &vector_hits {
            let entry = scored.entry(*chunk_id).or_insert((0.0, None));
            entry.0 += *sim as f64 * 0.6;
            if entry.1.is_none() {
                entry.1 = self.store.get_chunk(*chunk_id).ok().flatten();
            }
        }

        for chunk in &keyword_hits {
            if let Some(id) = chunk.id {
                let entry = scored.entry(id).or_insert((0.0, None));
                entry.0 += 0.3; // keyword match bonus
                if entry.1.is_none() {
                    entry.1 = Some(chunk.clone());
                }
            }
        }

        // Add recency bonus
        let now = Utc::now();
        for (_id, (score, chunk_opt)) in scored.iter_mut() {
            if let Some(chunk) = chunk_opt {
                let hours = (now - chunk.created_at).num_hours().max(0) as f64;
                *score += 0.1 / (1.0 + hours / 168.0);
            }
        }

        let mut results: Vec<RagResult> = scored.into_iter()
            .filter_map(|(_id, (score, chunk_opt))| {
                chunk_opt.map(|c| RagResult {
                    content: c.content,
                    doc_name: c.doc_name,
                    doc_id: c.doc_id,
                    chunk_index: c.chunk_index,
                    score,
                })
            })
            .collect();

        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);
        Ok(results)
    }

    /// List all ingested documents.
    pub fn list_documents(&self) -> anyhow::Result<Vec<(String, String, String, i64)>> {
        self.store.list_documents()
    }

    /// Delete a document and its vectors.
    pub fn delete_document(&self, doc_id: &str) -> anyhow::Result<usize> {
        // Get chunk IDs to remove from vector index
        let chunks = self.store.get_chunks_by_doc(doc_id)?;
        for chunk in &chunks {
            if let Some(id) = chunk.id {
                self.vectors.remove(-id);
            }
        }
        self.store.delete_document_chunks(doc_id)
    }
}

/// Split text into overlapping chunks. Returns (char_offset, text) pairs.
pub fn chunk_text(text: &str, chunk_size: usize, overlap: usize) -> Vec<(usize, String)> {
    if text.is_empty() {
        return Vec::new();
    }
    if text.len() <= chunk_size {
        return vec![(0, text.to_string())];
    }

    let mut chunks = Vec::new();
    let mut start = 0;
    let bytes = text.as_bytes();

    while start < text.len() {
        let end = (start + chunk_size).min(text.len());
        // Try to break at a sentence or word boundary
        let actual_end = if end < text.len() {
            // Look back for a period, newline, or space
            let search_start = if end > 50 { end - 50 } else { start };
            let mut best = end;
            for i in (search_start..end).rev() {
                if i < bytes.len() && (bytes[i] == b'.' || bytes[i] == b'\n') {
                    best = i + 1;
                    break;
                }
                if i < bytes.len() && bytes[i] == b' ' && best == end {
                    best = i + 1;
                }
            }
            best
        } else {
            end
        };

        let chunk = &text[start..actual_end];
        if !chunk.trim().is_empty() {
            chunks.push((start, chunk.to_string()));
        }

        if actual_end >= text.len() { break; }

        // Advance with overlap
        start = if actual_end > overlap { actual_end - overlap } else { actual_end };
    }

    chunks
}
