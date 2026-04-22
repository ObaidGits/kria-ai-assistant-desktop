//! Routing module facade.
//!
//! # Architecture
//!
//! Three independent stages per user turn:
//!
//! 1. **Stage A — Coarse Domain Router (fastembed-rs)**
//!    Embeds the prompt, computes cosine similarity vs. domain centroids.
//!    Returns an ordered `Vec<(Domain, similarity)>`.
//!
//! 2. **Stage B — Verb / Modality Classifier (parallel, lexical)**
//!    Scans for action verbs, emits `IntentModality + destructive` flag.
//!    Fed into `safety/policy.rs` regardless of embedding result.
//!
//! 3. **Stage C — Decision** (`decide.rs`)
//!    Applies z-score OOD test, margin multi-intent check, and produces
//!    a `RouteDecision` consumed by `agent/loop_engine.rs`.
//!
//! Embedding failures degrade gracefully to the legacy `IntentRouter` (regex).

pub mod cache;
pub mod decide;
pub mod domain;
pub mod embed;
pub mod ood;
pub mod segment;
pub mod trace;
pub mod verbs;

pub use cache::{RouterCache, RouterCacheEvent};
pub use decide::RouteDecision;
pub use domain::Domain;
pub use trace::RouterTrace;
pub use verbs::{IntentModality, ModalityResult};

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::broadcast;
use tracing::{debug, warn};

use crate::agent::router::IntentRouter;
use crate::config::RoutingConfig;

/// The main routing entry point.
pub struct Router {
    cache: Arc<RouterCache>,
    config: RoutingConfig,
}

impl Router {
    /// Create and boot the router.
    /// `tool_descriptions`: `(id, description, category)` triples from ToolRegistry.
    pub async fn new(
        config: RoutingConfig,
        cache_dir: PathBuf,
        tool_descriptions: Vec<(String, String, String)>,
    ) -> (Arc<Self>, broadcast::Sender<RouterCacheEvent>) {
        // Initialise embedding model (no-op if already done)
        let model_cache = cache_dir.parent().unwrap_or(&cache_dir).join("embeddings");
        if let Err(e) = embed::init_embedding_model(model_cache) {
            warn!("[Router] failed to init embedding model: {e} — will degrade to regex router");
        }

        let model_id = "multilingual-e5-small".to_string();
        let (router_cache, event_tx) = RouterCache::new(cache_dir, model_id);
        router_cache.boot(tool_descriptions).await;

        let router = Arc::new(Self {
            cache: router_cache,
            config,
        });
        (router, event_tx)
    }

    /// Route a user prompt.
    /// Returns `(RouteDecision, ModalityResult, RouterTrace)`.
    pub async fn route(&self, text: &str) -> (RouteDecision, ModalityResult, RouterTrace) {
        let start = Instant::now();

        // ── Stage B: verb/modality (always runs, no embedding needed) ────
        let modality = verbs::classify_modality(text);

        // ── Stage A: embedding (degrade to regex if not ready) ───────────
        let cache_state = self.cache.state().await;
        let cache_state_str = format!("{:?}", cache_state);

        if !embed::is_ready() || cache_state == cache::CacheState::Empty {
            let decision = self.regex_fallback(text);
            let trace = RouterTrace::from_parts(
                text, &modality, &[text.to_string()], &[], &decision,
                vec![], &cache_state_str, start.elapsed().as_millis() as u64,
            );
            return (decision, modality, trace);
        }

        // Segment into sub-prompts
        let segments = segment::segment(text, modality.imperative_verb_count);

        // Embed prompt (and segments if multi)
        let all_texts: Vec<&str> = std::iter::once(text)
            .chain(segments.iter().map(|s| s.as_str()))
            .collect();

        let embeddings = match embed::embed_batch(&all_texts) {
            Ok(e) => e,
            Err(e) => {
                warn!("[Router] embed_batch failed: {e} — degrading to regex");
                let decision = self.regex_fallback(text);
                let trace = RouterTrace::from_parts(
                    text, &modality, &segments, &[], &decision,
                    vec![], &cache_state_str, start.elapsed().as_millis() as u64,
                );
                return (decision, modality, trace);
            }
        };

        let query_emb = &embeddings[0];
        let seg_embs = &embeddings[1..];

        // Cosine similarity vs. domain centroids
        let centroids = self.cache.centroids().await;
        let mut domain_sims: Vec<(Domain, f32)> = centroids
            .iter()
            .map(|(d, c)| (*d, embed::cosine_sim(query_emb, c)))
            .collect();
        domain_sims.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Per-segment similarities (only when we have multiple segments)
        let segment_sims: Vec<Vec<(Domain, f32)>> = if segments.len() > 1 {
            seg_embs.iter().map(|emb| {
                let mut s: Vec<(Domain, f32)> = centroids
                    .iter()
                    .map(|(d, c)| (*d, embed::cosine_sim(emb, c)))
                    .collect();
                s.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                s
            }).collect()
        } else {
            vec![]
        };

        let ood_dist = self.cache.ood_distribution().await;

        let input = decide::DecideInput {
            domain_sims: &domain_sims,
            ood_distribution: &ood_dist,
            modality: &modality,
            segments: &segments,
            segment_sims: &segment_sims,
            config: &self.config,
        };

        let decision = decide::decide(&input);

        debug!(
            "[Router] text={:?} decision={:?} margin={:.3} modality={:?}",
            &text[..text.len().min(60)],
            decision,
            if domain_sims.len() >= 2 { domain_sims[0].1 - domain_sims[1].1 } else { 1.0 },
            modality.primary,
        );

        let selected_tools = self.tools_for_decision(&decision).await;
        let trace = RouterTrace::from_parts(
            text, &modality, &segments, &domain_sims, &decision,
            selected_tools, &cache_state_str, start.elapsed().as_millis() as u64,
        );

        (decision, modality, trace)
    }

    /// Legacy regex-based fallback (uses existing IntentRouter).
    fn regex_fallback(&self, text: &str) -> RouteDecision {
        use crate::agent::router::Intent;
        let result = IntentRouter::classify(text);
        match result.intent {
            Intent::Conversation => RouteDecision::Conversation,
            Intent::DirectTool(name) => {
                // Map a direct tool to a best-effort domain
                let cat = tool_name_to_category(&name);
                RouteDecision::SingleDomain(domain::category_to_domain(&cat))
            }
            Intent::ComplexTask => RouteDecision::Ambiguous {
                top: vec![Domain::Planner],
            },
        }
    }

    /// Collect tool ids that belong to the selected domains (for trace / grammar building).
    async fn tools_for_decision(&self, decision: &RouteDecision) -> Vec<String> {
        // Note: the actual filtered ToolDef list is built by the loop_engine.
        // This is a lightweight id list for tracing only.
        let domains: Vec<Domain> = match decision {
            RouteDecision::Conversation => return vec![],
            RouteDecision::SingleDomain(d) => vec![*d],
            RouteDecision::MultiDomain(ds) => ds.clone(),
            RouteDecision::Ambiguous { top } => top.clone(),
        };
        self.cache.tool_ids_for_domains(&domains).await
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn tool_name_to_category(tool_name: &str) -> String {
    // Extract prefix (e.g. "get_cpu_usage" → "system_info" by prefix lookup)
    let lc = tool_name.to_lowercase();
    if lc.starts_with("get_cpu") || lc.starts_with("get_mem") || lc.starts_with("get_disk")
        || lc.starts_with("get_battery") || lc.starts_with("get_gpu")
        || lc.starts_with("get_network") || lc.starts_with("get_system")
    {
        "system_info".into()
    } else if lc.starts_with("read_") || lc.starts_with("write_") || lc.starts_with("delete_")
        || lc.starts_with("search_file") || lc.starts_with("list_dir") || lc.starts_with("parse_")
    {
        "file_ops".into()
    } else if lc.starts_with("open_app") || lc.starts_with("close_app") || lc.starts_with("list_running") {
        "app_lifecycle".into()
    } else if lc.starts_with("gw_") || lc.starts_with("mcp_gworkspace") {
        "mcp_gworkspace".into()
    } else if lc.starts_with("web_search") || lc.starts_with("fetch_") || lc.starts_with("get_news")
        || lc.starts_with("get_weather") || lc.starts_with("recall_") || lc.starts_with("search_knowledge")
    {
        "knowledge".into()
    } else if lc.starts_with("send_") || lc.starts_with("compose_") || lc.starts_with("reply_")
        || lc.starts_with("schedule_") || lc.starts_with("gw_mail") || lc.starts_with("gw_calendar")
    {
        "communication".into()
    } else if lc.starts_with("shutdown") || lc.starts_with("reboot") || lc.starts_with("set_volume")
        || lc.starts_with("mute_") || lc.starts_with("set_brightness") || lc.starts_with("lock_")
    {
        "power".into()
    } else if lc.starts_with("screenshot") || lc.starts_with("describe_screen") || lc.starts_with("vision") {
        "vision".into()
    } else if lc.starts_with("install_") || lc.starts_with("uninstall_") || lc.starts_with("update_") {
        "packages".into()
    } else if lc.starts_with("run_") || lc.starts_with("git_") || lc.starts_with("shell_")
        || lc.starts_with("kill_") || lc.starts_with("list_process")
    {
        "developer".into()
    } else {
        "knowledge".into()
    }
}
