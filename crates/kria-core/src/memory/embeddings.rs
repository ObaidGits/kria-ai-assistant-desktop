use ort::session::Session;
use ort::value::Tensor;
/// Local sentence embeddings.
///
/// Uses ONNX Runtime (`ort`) for real embedding inference with
/// `all-MiniLM-L6-v2`. Falls back to deterministic hash-based vectors
/// if the ONNX model is not available.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub struct EmbeddingModel {
    dim: usize,
    session: Option<Arc<Mutex<Session>>>,
}

impl EmbeddingModel {
    /// Load the embedding model from an ONNX file.
    /// If no model path is found, falls back to hash-based placeholder.
    pub fn load(dim: usize) -> anyhow::Result<Self> {
        // Try to find the ONNX model in standard locations
        let model_paths = [
            dirs::home_dir()
                .unwrap_or_default()
                .join(".kria/models/embeddings/all-MiniLM-L6-v2.onnx"),
            PathBuf::from("models/embeddings/all-MiniLM-L6-v2.onnx"),
        ];

        for path in &model_paths {
            if path.exists() {
                match Session::builder()?.commit_from_file(path) {
                    Ok(session) => {
                        tracing::info!(path = %path.display(), "embedding model loaded (ONNX)");
                        return Ok(Self {
                            dim,
                            session: Some(Arc::new(Mutex::new(session))),
                        });
                    }
                    Err(e) => {
                        tracing::warn!(path = %path.display(), error = %e, "failed to load ONNX model, using fallback");
                    }
                }
            }
        }

        tracing::info!(dim, "embedding model initialized (hash-based fallback)");
        Ok(Self { dim, session: None })
    }

    /// Generate an embedding vector for the given text.
    pub fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        if let Some(ref session) = self.session {
            let mut guard = session
                .lock()
                .map_err(|e| anyhow::anyhow!("mutex poisoned: {e}"))?;
            self.embed_onnx(&mut guard, text)
        } else {
            self.embed_fallback(text)
        }
    }

    /// Real ONNX inference for sentence embedding.
    fn embed_onnx(&self, session: &mut Session, text: &str) -> anyhow::Result<Vec<f32>> {
        use ndarray::Array2;

        let tokens = self.simple_tokenize(text);
        let seq_len = tokens.len();

        let input_ids =
            Array2::from_shape_vec((1, seq_len), tokens.iter().map(|&t| t as i64).collect())?;
        let attention_mask = Array2::from_shape_vec((1, seq_len), vec![1i64; seq_len])?;
        let token_type_ids = Array2::from_shape_vec((1, seq_len), vec![0i64; seq_len])?;

        let input_ids_val = Tensor::from_array(input_ids)?;
        let attention_mask_val = Tensor::from_array(attention_mask)?;
        let token_type_ids_val = Tensor::from_array(token_type_ids)?;

        let outputs = session.run(ort::inputs![
            input_ids_val,
            attention_mask_val,
            token_type_ids_val,
        ])?;

        // try_extract_tensor returns (&Shape, &[f32])
        // Shape derefs to &[i64], shape is [batch=1, seq_len, hidden_dim]
        let (shape, data) = outputs[0].try_extract_tensor::<f32>()?;
        let hidden_dim = if shape.len() >= 3 {
            shape[2] as usize
        } else {
            self.dim
        };
        let seq_len_out = if shape.len() >= 3 {
            shape[1] as usize
        } else {
            1
        };

        // Mean pool over sequence dimension
        let mut pooled = vec![0.0f32; hidden_dim];
        for s in 0..seq_len_out {
            let offset = s * hidden_dim;
            for d in 0..hidden_dim {
                if offset + d < data.len() {
                    pooled[d] += data[offset + d];
                }
            }
        }
        if seq_len_out > 0 {
            for v in &mut pooled {
                *v /= seq_len_out as f32;
            }
        }

        // L2 normalize
        let norm: f32 = pooled.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for v in &mut pooled {
                *v /= norm;
            }
        }

        Ok(pooled)
    }

    /// Simple word-piece-like tokenizer placeholder.
    /// For production, use a proper tokenizer (tokenizers crate).
    fn simple_tokenize(&self, text: &str) -> Vec<u32> {
        let mut tokens = vec![101u32]; // [CLS]
        for word in text.split_whitespace().take(510) {
            // Simple hash to vocab range (30522 for BERT-like models)
            let hash = word
                .bytes()
                .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
            tokens.push(hash % 30520 + 2); // avoid special tokens 0,1
        }
        tokens.push(102); // [SEP]
        tokens
    }

    /// Deterministic hash-based fallback embedding (no model needed).
    fn embed_fallback(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let mut vec = vec![0.0f32; self.dim];
        for (i, byte) in text.bytes().enumerate() {
            vec[i % self.dim] += (byte as f32 - 96.0) / 128.0;
        }
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

    /// Whether the real ONNX model is loaded.
    pub fn is_onnx_loaded(&self) -> bool {
        self.session.is_some()
    }
}
