use crate::memory::store::MemoryStore;
use chrono::Utc;

/// Recompute decay scores for all facts based on age and access patterns.
///
/// decay_score = base * recency_bonus * frequency_bonus
/// where:
///   base = initial 1.0, decays over time
///   recency = 1 / (1 + hours_since_access / 168)  (1-week half-life)
///   frequency = min(1.0, log(access_count + 1) / 5)
pub fn recompute_decay(store: &MemoryStore) -> anyhow::Result<usize> {
    let facts = store.all_facts_with_decay(0.0)?;
    let now = Utc::now();
    let mut updated = 0;

    for fact in &facts {
        let hours = (now - fact.last_accessed).num_hours().max(0) as f64;
        let recency = 1.0 / (1.0 + hours / 168.0);
        let frequency = ((fact.access_count as f64 + 1.0).ln() / 5.0).min(1.0);
        let new_score = recency * 0.7 + frequency * 0.3;

        if let Some(id) = fact.id {
            store.update_fact_decay(id, new_score)?;
            updated += 1;
        }
    }

    tracing::debug!(updated, "decay scores recomputed");
    Ok(updated)
}

/// Remove facts whose decay score has dropped below the threshold.
pub fn prune_expired(store: &MemoryStore, threshold: f64) -> anyhow::Result<usize> {
    let facts = store.all_facts_with_decay(0.0)?;
    let mut pruned = 0;

    for fact in &facts {
        if fact.decay_score < threshold {
            if let Some(id) = fact.id {
                store.delete_fact(id)?;
                pruned += 1;
            }
        }
    }

    if pruned > 0 {
        tracing::info!(pruned, threshold, "pruned expired facts");
    }
    Ok(pruned)
}
