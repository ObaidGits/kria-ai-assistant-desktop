use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

/// Embedded HNSW vector index for approximate nearest-neighbor search.
///
/// Wraps a simple in-memory index with serde-based persistence.
/// When the `usearch` crate is available, replace internals with usearch HNSW.
/// For now: brute-force cosine similarity (sufficient up to ~50k vectors).
pub struct VectorIndex {
    dim: usize,
    vectors: Mutex<HashMap<i64, Vec<f32>>>,
    path: Option<std::path::PathBuf>,
}

impl VectorIndex {
    /// Create/open an index at the given path.
    pub fn open(path: &Path, dim: usize) -> anyhow::Result<Self> {
        let vectors = if path.exists() {
            let data = std::fs::read(path)?;
            bincode_deserialize(&data).unwrap_or_default()
        } else {
            HashMap::new()
        };

        Ok(Self {
            dim,
            vectors: Mutex::new(vectors),
            path: Some(path.to_path_buf()),
        })
    }

    /// In-memory only index (for testing).
    pub fn in_memory(dim: usize) -> Self {
        Self {
            dim,
            vectors: Mutex::new(HashMap::new()),
            path: None,
        }
    }

    /// Add a vector.
    pub fn add(&self, id: i64, vector: Vec<f32>) -> anyhow::Result<()> {
        anyhow::ensure!(
            vector.len() == self.dim,
            "vector dimension mismatch: expected {}, got {}",
            self.dim,
            vector.len()
        );
        self.vectors.lock().unwrap().insert(id, vector);
        Ok(())
    }

    /// Remove a vector.
    pub fn remove(&self, id: i64) {
        self.vectors.lock().unwrap().remove(&id);
    }

    /// Search for k nearest neighbors by cosine similarity.
    /// Returns Vec<(id, similarity)> sorted by descending similarity.
    pub fn search(&self, query: &[f32], k: usize) -> Vec<(i64, f32)> {
        let vecs = self.vectors.lock().unwrap();
        let mut scored: Vec<(i64, f32)> = vecs
            .iter()
            .map(|(&id, v)| (id, cosine_similarity(query, v)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(k);
        scored
    }

    /// Number of stored vectors.
    pub fn len(&self) -> usize {
        self.vectors.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Persist to disk.
    pub fn save(&self) -> anyhow::Result<()> {
        if let Some(ref path) = self.path {
            let vecs = self.vectors.lock().unwrap();
            let data = bincode_serialize(&*vecs)?;
            std::fs::write(path, data)?;
        }
        Ok(())
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

// Simple bincode-like serialization using serde_json (replace with bincode crate for production)
fn bincode_serialize(map: &HashMap<i64, Vec<f32>>) -> anyhow::Result<Vec<u8>> {
    Ok(serde_json::to_vec(map)?)
}

fn bincode_deserialize(data: &[u8]) -> anyhow::Result<HashMap<i64, Vec<f32>>> {
    Ok(serde_json::from_slice(data)?)
}

impl Drop for VectorIndex {
    fn drop(&mut self) {
        let _ = self.save();
    }
}
