//! Cache state machine for tool embeddings.
//!
//! # State transitions
//! ```
//! Empty → Loading → Validating → Ready
//!                              ↓ (tool delta detected)
//!                        Reconciling → Ready
//!
//! Any state → Invalidated (model swap / file corruption)
//!           → Reconciling → Ready
//! ```
//!
//! # On-disk layout  (~/.kria/cache/router/)
//! ```
//! manifest.v1.json          — HashMap<tool_id, ToolCacheEntry>
//! embeddings.v1.bin         — contiguous f32 vectors (mmap-able)
//! embeddings.v1.bin.next    — double-buffer for atomic swap
//! domain_centroids.v1.bin   — one centroid per Domain (ordered by Domain discriminant)
//! ood_calibration.v1.bin    — OOD reference distribution (pre-embedded)
//! model.fingerprint         — embedding model id string
//! ```

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};
use tracing::{info, warn};

use super::domain::{category_to_domain, Domain};
use super::embed;

// ─── Cache entry ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCacheEntry {
    /// blake3 hex of the canonical tool JSON.
    pub fingerprint: String,
    /// Byte offset into embeddings.v1.bin.
    pub offset: usize,
    /// Embedding dimension (sanity check).
    pub dim: usize,
    pub domain: String,
    pub embedded_at: u64, // unix epoch secs
}

// ─── Cache event ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum RouterCacheEvent {
    /// One or more tool descriptions changed (from ToolRegistry / MCP deltas).
    ToolsChanged,
    /// Embedding model was swapped in config.
    ModelChanged,
}

// ─── Cache state ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheState {
    Empty,
    Loading,
    Validating,
    Reconciling,
    Ready,
    Invalidated,
}

// ─── Manifest ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CacheManifest {
    pub entries: HashMap<String, ToolCacheEntry>,
    /// blake3 hex of the entire embeddings.v1.bin file.
    pub file_checksum: String,
    pub dim: usize,
}

// ─── Router cache ───────────────────────────────────────────────────────────

/// Thread-safe in-process cache of tool embeddings + domain centroids.
pub struct RouterCache {
    cache_dir: PathBuf,
    model_id: String,
    state: RwLock<CacheState>,
    /// All tool embeddings — tool_id → L2-normalised f32 vector.
    embeddings: RwLock<HashMap<String, Vec<f32>>>,
    /// Domain → centroid vector.
    centroids: RwLock<HashMap<Domain, Vec<f32>>>,
    /// OOD calibration: top-1 similarity values from ~500 OOD prompts.
    ood_dist: RwLock<Vec<f32>>,
    /// Broadcast channel to trigger reconciliation from outside.
    event_tx: broadcast::Sender<RouterCacheEvent>,
}

impl RouterCache {
    pub fn new(cache_dir: PathBuf, model_id: String) -> (Arc<Self>, broadcast::Sender<RouterCacheEvent>) {
        let (tx, _) = broadcast::channel(16);
        let cache = Arc::new(Self {
            cache_dir,
            model_id,
            state: RwLock::new(CacheState::Empty),
            embeddings: RwLock::new(HashMap::new()),
            centroids: RwLock::new(HashMap::new()),
            ood_dist: RwLock::new(Vec::new()),
            event_tx: tx.clone(),
        });
        (cache, tx)
    }

    pub async fn state(&self) -> CacheState {
        self.state.read().await.clone()
    }

    /// Get embedding for a tool by id. Returns None when cache not ready yet.
    pub async fn get_embedding(&self, tool_id: &str) -> Option<Vec<f32>> {
        let embs = self.embeddings.read().await;
        embs.get(tool_id).cloned()
    }

    /// Get all domain centroids. Returns empty map when not ready.
    pub async fn centroids(&self) -> HashMap<Domain, Vec<f32>> {
        self.centroids.read().await.clone()
    }

    /// Get OOD calibration distribution. Empty when not ready.
    pub async fn ood_distribution(&self) -> Vec<f32> {
        self.ood_dist.read().await.clone()
    }

    /// Return tool IDs that belong to any of the provided domains.
    pub(crate) async fn tool_ids_for_domains(&self, domains: &[Domain]) -> Vec<String> {
        let embs = self.embeddings.read().await;
        embs.keys()
            .filter(|id| {
                let cat = id.split('_').next().unwrap_or("");
                let d = super::domain::category_to_domain(cat);
                domains.contains(&d)
            })
            .cloned()
            .collect()
    }

    // ─── Boot sequence ────────────────────────────────────────────────────

    /// Start the cache boot sequence. Blocks until at least one domain is ready.
    /// Remaining reconciliation continues in background.
    pub async fn boot(
        self: &Arc<Self>,
        tool_descriptions: Vec<(String, String, String)>, // (id, description, category)
    ) {
        *self.state.write().await = CacheState::Loading;
        info!("[RouterCache] booting — {} tools", tool_descriptions.len());

        let model_ok = self.check_model_fingerprint().await;
        if !model_ok {
            *self.state.write().await = CacheState::Invalidated;
            info!("[RouterCache] model fingerprint changed — full rebuild");
            let this = Arc::clone(self);
            tokio::spawn(async move { this.rebuild_all(tool_descriptions).await });
            return;
        }

        *self.state.write().await = CacheState::Validating;
        match self.load_from_disk().await {
            Ok(manifest) => {
                let delta = self.compute_delta(&manifest, &tool_descriptions);
                if delta.is_empty() {
                    info!("[RouterCache] all embeddings valid — transitioning to Ready");
                    *self.state.write().await = CacheState::Ready;
                } else {
                    info!("[RouterCache] {} tool(s) need re-embedding — reconciling", delta.len());
                    *self.state.write().await = CacheState::Reconciling;
                    let this = Arc::clone(self);
                    tokio::spawn(async move { this.reconcile(delta, tool_descriptions).await });
                }
            }
            Err(e) => {
                warn!("[RouterCache] failed to load cache: {e} — rebuilding");
                *self.state.write().await = CacheState::Invalidated;
                let this = Arc::clone(self);
                tokio::spawn(async move { this.rebuild_all(tool_descriptions).await });
            }
        }

        // Spawn event listener for runtime invalidation.
        let this = Arc::clone(self);
        let mut rx = self.event_tx.subscribe();
        tokio::spawn(async move {
            while let Ok(evt) = rx.recv().await {
                match evt {
                    RouterCacheEvent::ToolsChanged | RouterCacheEvent::ModelChanged => {
                        *this.state.write().await = CacheState::Invalidated;
                        // Caller is responsible for calling boot() again with updated descriptions.
                        info!("[RouterCache] invalidated by event {:?}", evt);
                    }
                }
            }
        });
    }

    // ─── Fingerprint ──────────────────────────────────────────────────────

    async fn check_model_fingerprint(&self) -> bool {
        let fp_path = self.cache_dir.join("model.fingerprint");
        match std::fs::read_to_string(&fp_path) {
            Ok(stored) => stored.trim() == self.model_id.trim(),
            Err(_) => false,
        }
    }

    fn write_model_fingerprint(&self) -> Result<()> {
        std::fs::create_dir_all(&self.cache_dir)?;
        let fp_path = self.cache_dir.join("model.fingerprint");
        let mut f = std::fs::File::create(&fp_path)?;
        f.write_all(self.model_id.as_bytes())?;
        Ok(())
    }

    // ─── Disk load ────────────────────────────────────────────────────────

    async fn load_from_disk(&self) -> Result<CacheManifest> {
        let manifest_path = self.cache_dir.join("manifest.v1.json");
        let bytes = std::fs::read(&manifest_path)?;
        let manifest: CacheManifest = serde_json::from_slice(&bytes)?;

        let emb_path = self.cache_dir.join("embeddings.v1.bin");
        let emb_bytes = std::fs::read(&emb_path)?;

        // Integrity check
        let actual_checksum = hex::encode(blake3::hash(&emb_bytes).as_bytes());
        if actual_checksum != manifest.file_checksum {
            anyhow::bail!("embeddings.v1.bin checksum mismatch");
        }

        // Deserialise vectors
        let dim = manifest.dim;
        if dim == 0 {
            anyhow::bail!("manifest dim is 0");
        }
        let expected_bytes = manifest.entries.len() * dim * 4;
        if emb_bytes.len() < expected_bytes {
            anyhow::bail!("embeddings.v1.bin too short");
        }

        let mut embs = self.embeddings.write().await;
        for (id, entry) in &manifest.entries {
            let start = entry.offset * 4;
            let end = start + entry.dim * 4;
            if end > emb_bytes.len() {
                warn!("[RouterCache] skipping '{}': offset out of bounds", id);
                continue;
            }
            let v: Vec<f32> = emb_bytes[start..end]
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
            embs.insert(id.clone(), v);
        }

        // Reload centroids
        self.recompute_centroids().await;
        // Load OOD distribution
        self.load_ood_calibration().await;

        Ok(manifest)
    }

    // ─── Delta computation ────────────────────────────────────────────────

    /// Returns list of tool ids + descriptions that need re-embedding.
    fn compute_delta(
        &self,
        manifest: &CacheManifest,
        tools: &[(String, String, String)],
    ) -> Vec<(String, String, String)> {
        tools
            .iter()
            .filter(|(id, desc, cat)| {
                let fp = canonical_fingerprint(id, desc, cat);
                match manifest.entries.get(id.as_str()) {
                    Some(e) => e.fingerprint != fp,
                    None => true,
                }
            })
            .cloned()
            .collect()
    }

    // ─── Rebuild / reconcile ──────────────────────────────────────────────

    async fn rebuild_all(&self, tools: Vec<(String, String, String)>) {
        self.reconcile(tools.clone(), tools).await;
        let _ = self.write_model_fingerprint();
    }

    async fn reconcile(
        &self,
        to_embed: Vec<(String, String, String)>,
        all_tools: Vec<(String, String, String)>,
    ) {
        if !embed::is_ready() {
            warn!("[RouterCache] embedding model not ready; skipping reconcile");
            *self.state.write().await = CacheState::Ready; // degrade gracefully
            return;
        }

        // Embed only the delta tools
        let texts: Vec<&str> = to_embed.iter().map(|(_, d, _)| d.as_str()).collect();
        let new_embeddings = match embed::embed_batch(&texts) {
            Ok(e) => e,
            Err(e) => {
                warn!("[RouterCache] embed_batch failed: {e}");
                *self.state.write().await = CacheState::Ready;
                return;
            }
        };

        {
            let mut embs = self.embeddings.write().await;
            for ((id, desc, cat), vec) in to_embed.iter().zip(new_embeddings.iter()) {
                let _ = (desc, cat); // used for fingerprinting only
                embs.insert(id.clone(), vec.clone());
            }
        }

        // Persist
        if let Err(e) = self.persist_to_disk(&all_tools).await {
            warn!("[RouterCache] persist failed: {e}");
        }

        self.recompute_centroids().await;
        self.load_ood_calibration().await;

        info!("[RouterCache] reconcile complete → Ready");
        *self.state.write().await = CacheState::Ready;
    }

    async fn persist_to_disk(&self, all_tools: &[(String, String, String)]) -> Result<()> {
        std::fs::create_dir_all(&self.cache_dir)?;
        let embs = self.embeddings.read().await;

        let mut manifest = CacheManifest::default();
        let mut raw_bytes: Vec<u8> = Vec::new();
        let mut offset = 0usize;

        for (id, desc, cat) in all_tools {
            if let Some(v) = embs.get(id) {
                let start = offset;
                for f in v {
                    raw_bytes.extend_from_slice(&f.to_le_bytes());
                }
                let dim = v.len();
                if manifest.dim == 0 { manifest.dim = dim; }
                manifest.entries.insert(id.clone(), ToolCacheEntry {
                    fingerprint: canonical_fingerprint(id, desc, cat),
                    offset: start,
                    dim,
                    domain: category_to_domain(cat).as_str().to_string(),
                    embedded_at: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                });
                offset += dim;
            }
        }

        let checksum = hex::encode(blake3::hash(&raw_bytes).as_bytes());
        manifest.file_checksum = checksum;

        // Atomic write: write to .next then rename
        let next_path = self.cache_dir.join("embeddings.v1.bin.next");
        let final_path = self.cache_dir.join("embeddings.v1.bin");
        std::fs::write(&next_path, &raw_bytes)?;
        std::fs::rename(&next_path, &final_path)?;

        let manifest_path = self.cache_dir.join("manifest.v1.json");
        std::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;

        Ok(())
    }

    // ─── Centroids ────────────────────────────────────────────────────────

    async fn recompute_centroids(&self) {
        if !embed::is_ready() {
            return;
        }
        let mut per_domain: HashMap<Domain, Vec<Vec<f32>>> = HashMap::new();

        // Tool description embeddings grouped by domain
        {
            let embs = self.embeddings.read().await;
            for (id, vec) in embs.iter() {
                // Derive domain from id prefix (mcp_<server>_<tool> or plain tool_name)
                let domain = id_to_domain_hint(id);
                per_domain.entry(domain).or_default().push(vec.clone());
            }
        }

        // Anchor sentences weighted into centroids
        for domain in Domain::tool_domains() {
            let anchors = domain.anchor_sentences();
            if let Ok(anchor_embs) = embed::embed_batch(anchors) {
                let entry = per_domain.entry(*domain).or_default();
                for ae in anchor_embs {
                    entry.push(ae);
                }
            }
        }

        let mut centroids = self.centroids.write().await;
        centroids.clear();
        for (domain, vecs) in &per_domain {
            if vecs.is_empty() {
                continue;
            }
            let dim = vecs[0].len();
            let mut sum = vec![0f32; dim];
            for v in vecs {
                for (s, x) in sum.iter_mut().zip(v.iter()) {
                    *s += x;
                }
            }
            let n = vecs.len() as f32;
            let mean: Vec<f32> = sum.iter().map(|x| x / n).collect();
            // Normalise centroid
            let norm: f32 = mean.iter().map(|x| x * x).sum::<f32>().sqrt();
            let centroid: Vec<f32> = if norm > 1e-9 {
                mean.iter().map(|x| x / norm).collect()
            } else {
                mean
            };
            centroids.insert(*domain, centroid);
        }

        // Persist centroids
        let _ = self.save_centroids(&centroids);
    }

    fn save_centroids(&self, centroids: &HashMap<Domain, Vec<f32>>) -> Result<()> {
        std::fs::create_dir_all(&self.cache_dir)?;
        let path = self.cache_dir.join("domain_centroids.v1.bin");
        let data: Vec<u8> = centroids.iter().flat_map(|(domain, v)| {
            let domain_byte = [domain_to_u8(*domain)];
            let len_bytes = (v.len() as u32).to_le_bytes();
            let float_bytes: Vec<u8> = v.iter().flat_map(|f| f.to_le_bytes()).collect();
            domain_byte.iter().chain(len_bytes.iter()).chain(float_bytes.iter()).copied().collect::<Vec<u8>>()
        }).collect();
        std::fs::write(path, data)?;
        Ok(())
    }

    // ─── OOD calibration ─────────────────────────────────────────────────

    async fn load_ood_calibration(&self) {
        // Try to load pre-computed distribution from disk
        let path = self.cache_dir.join("ood_calibration.v1.bin");
        if let Ok(bytes) = std::fs::read(&path) {
            let floats: Vec<f32> = bytes
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
            *self.ood_dist.write().await = floats;
            return;
        }
        // If missing, build a tiny bootstrap set of generic conversational prompts
        // and record their top-1 cosine sim against domain centroids.
        self.build_ood_calibration_from_bootstrap().await;
    }

    async fn build_ood_calibration_from_bootstrap(&self) {
        let ood_prompts = [
            "explain quantum entanglement",
            "tell me a fun fact",
            "what is the meaning of life",
            "describe impressionism art movement",
            "how does a blockchain work conceptually",
            "what is photosynthesis",
            "compare jazz and classical music",
            "who invented the printing press",
            "explain relativity in simple terms",
            "is pineapple on pizza good",
            "define existentialism",
            "how are rainbows formed",
            "explain neural networks simply",
            "what causes thunder",
            "describe the Roman Empire",
            "zindagi ka matlab kya hai",
            "ek acchi kahani sunao",
            "duniya ka sabse bada desh kaunsa hai",
        ];

        if !embed::is_ready() {
            return;
        }

        let centroids = self.centroids.read().await;
        if centroids.is_empty() {
            return;
        }
        let centroid_vecs: Vec<&Vec<f32>> = centroids.values().collect();

        let prompts: Vec<&str> = ood_prompts.iter().map(|s| *s).collect();
        let Ok(embs) = embed::embed_batch(&prompts) else { return };

        let sims: Vec<f32> = embs
            .iter()
            .filter_map(|q| {
                centroid_vecs
                    .iter()
                    .map(|c| embed::cosine_sim(q, c))
                    .reduce(f32::max)
            })
            .collect();

        // Persist
        let bytes: Vec<u8> = sims.iter().flat_map(|f| f.to_le_bytes()).collect();
        let path = self.cache_dir.join("ood_calibration.v1.bin");
        let _ = std::fs::write(path, bytes);

        *self.ood_dist.write().await = sims;
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Canonical fingerprint of a tool entry (blake3 hex).
pub fn canonical_fingerprint(id: &str, description: &str, category: &str) -> String {
    let canonical = serde_json::json!({
        "id": id,
        "description": description,
        "category": category,
    });
    let json = serde_json::to_vec(&canonical).unwrap_or_default();
    hex::encode(blake3::hash(&json).as_bytes())
}

/// Derive a domain hint from a tool id string (best-effort, not authoritative).
fn id_to_domain_hint(id: &str) -> Domain {
    category_to_domain(id.split('_').next().unwrap_or(""))
}

fn domain_to_u8(d: Domain) -> u8 {
    match d {
        Domain::Conversation => 0,
        Domain::SystemInfo => 1,
        Domain::FileOps => 2,
        Domain::AppLifecycle => 3,
        Domain::Comms => 4,
        Domain::Workspace => 5,
        Domain::Knowledge => 6,
        Domain::Power => 7,
        Domain::Vision => 8,
        Domain::Packages => 9,
        Domain::Developer => 10,
        Domain::Planner => 11,
    }
}
