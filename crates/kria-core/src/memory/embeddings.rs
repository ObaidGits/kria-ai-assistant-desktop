/// Local sentence embeddings.
///
/// Placeholder: generates random vectors until fastembed-rs or candle integration.
/// Replace `embed()` body with actual model inference.
pub struct EmbeddingModel {
    dim: usize,
}

impl EmbeddingModel {
    /// Load the embedding model (lazy, first call may be slow).
    pub fn load(dim: usize) -> anyhow::Result<Self> {
        // TODO: Load all-MiniLM-L6-v2 ONNX via ort crate
        tracing::info!(dim, "embedding model initialized (placeholder)");
        Ok(Self { dim })
    }

    /// Generate an embedding vector for the given text.
    pub fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        // TODO: Replace with actual model inference
        // Deterministic hash-based placeholder for consistent behavior
        let mut vec = vec![0.0f32; self.dim];
        for (i, byte) in text.bytes().enumerate() {
            vec[i % self.dim] += (byte as f32 - 96.0) / 128.0;
        }
        // Normalize
        let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for v in &mut vec {
                *v /= norm;
            }
        }
        Ok(vec)
    }

    pub fn dimension(&self) -> usize {
        self.dim
    }
}
