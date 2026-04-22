//! RouterTrace — structured observability event emitted per routing turn.
//! Fed into the existing AuditLogger.

use serde::{Deserialize, Serialize};
use super::domain::Domain;
use super::decide::RouteDecision;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterTrace {
    pub input_text: String,
    pub primary_modality: String,
    pub all_modalities: Vec<String>,
    pub destructive: bool,
    pub segments: Vec<String>,
    pub top_domains: Vec<(String, f32)>,
    pub decision: String,
    pub selected_tools: Vec<String>,
    pub cache_state: String,
    pub latency_ms: u64,
}

impl RouterTrace {
    pub fn from_parts(
        input_text: &str,
        modality: &super::verbs::ModalityResult,
        segments: &[String],
        sims: &[(Domain, f32)],
        decision: &RouteDecision,
        selected_tools: Vec<String>,
        cache_state: &str,
        latency_ms: u64,
    ) -> Self {
        Self {
            input_text: input_text.to_string(),
            primary_modality: modality.primary.as_str().to_string(),
            all_modalities: modality.all.iter().map(|m| m.as_str().to_string()).collect(),
            destructive: modality.destructive,
            segments: segments.to_vec(),
            top_domains: sims.iter().take(3).map(|(d, s)| (d.as_str().to_string(), *s)).collect(),
            decision: decision_str(decision),
            selected_tools,
            cache_state: cache_state.to_string(),
            latency_ms,
        }
    }
}

fn decision_str(d: &RouteDecision) -> String {
    match d {
        RouteDecision::Conversation => "conversation".into(),
        RouteDecision::SingleDomain(dom) => format!("single:{}", dom.as_str()),
        RouteDecision::MultiDomain(doms) => {
            let names: Vec<&str> = doms.iter().map(|d| d.as_str()).collect();
            format!("multi:{}", names.join("+"))
        }
        RouteDecision::Ambiguous { top } => {
            let names: Vec<&str> = top.iter().map(|d| d.as_str()).collect();
            format!("ambiguous:{}", names.join("+"))
        }
    }
}
