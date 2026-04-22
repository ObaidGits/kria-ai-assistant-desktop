//! Embedding wrapper around fastembed-rs.
//!
//! Provides:
//! - A single shared `EmbedModel` (OnceLock — load once per process).
//! - `embed_one` / `embed_batch`: produce L2-normalised f32 vectors.
//! - `cosine_sim`: dot product of two pre-normalised vectors.

use anyhow::Result;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use once_cell::sync::OnceCell;
use std::path::PathBuf;
use std::sync::Mutex;

// fastembed v5 `embed()` takes `&mut self`, so we wrap in a Mutex.
static EMBED_MODEL: OnceCell<Mutex<TextEmbedding>> = OnceCell::new();

/// Initialise (or no-op if already done).
/// `cache_dir` is the directory where fastembed downloads/stores the model.
/// Model: multilingual-e5-small for Hinglish support.
pub fn init_embedding_model(cache_dir: PathBuf) -> Result<()> {
    if EMBED_MODEL.get().is_some() {
        return Ok(());
    }
    let model = TextEmbedding::try_new(
        InitOptions::new(EmbeddingModel::MultilingualE5Small)
            .with_cache_dir(cache_dir)
            .with_show_download_progress(false),
    )?;
    // Ignore error if another thread already set it (race on first boot).
    let _ = EMBED_MODEL.set(Mutex::new(model));
    Ok(())
}

/// Check whether the embedding model has been initialised.
pub fn is_ready() -> bool {
    EMBED_MODEL.get().is_some()
}

/// Embed a batch of texts. Returns L2-normalised vectors.
pub fn embed_batch(texts: &[&str]) -> Result<Vec<Vec<f32>>> {
    let cell = EMBED_MODEL
        .get()
        .ok_or_else(|| anyhow::anyhow!("embedding model not initialised; call init_embedding_model first"))?;
    let mut model = cell
        .lock()
        .map_err(|_| anyhow::anyhow!("embedding model mutex poisoned"))?;
    let raw = model.embed(texts.to_vec(), None)?;
    Ok(raw.into_iter().map(l2_normalise).collect())
}

/// Embed a single text. Returns an L2-normalised vector.
pub fn embed_one(text: &str) -> Result<Vec<f32>> {
    let mut batch = embed_batch(&[text])?;
    batch.pop().ok_or_else(|| anyhow::anyhow!("empty embedding result"))
}

/// Cosine similarity of two pre-normalised vectors (= dot product).
#[inline]
pub fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// In-place L2 normalise.
fn l2_normalise(mut v: Vec<f32>) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-9 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
    v
}
