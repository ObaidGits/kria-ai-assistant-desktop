//! News tools — delegate to the Python sidecar news processor.
//!
//! GREEN tools (auto-execute, no approval needed):
//!   search_news       → keyword search across deduplicated, trust-scored articles
//!   fetch_article     → extract full text from a news URL
//!   list_news_sources → which sources are being polled and when
//!   news_status       → poller health + DB stats

use crate::infra::ToolResult;
use crate::safety::RiskLevel;
use crate::sidecar::SidecarBridge;
use crate::tools::registry::{ParamDef, ToolDef, ToolHandler, ToolRegistry};
use async_trait::async_trait;
use std::sync::Arc;

fn param(name: &str, ty: &str, desc: &str, required: bool) -> ParamDef {
    ParamDef {
        name: name.into(),
        param_type: ty.into(),
        description: desc.into(),
        required,
        default: None,
    }
}

/// Shared sidecar handle, cloned cheaply into each handler.
#[derive(Clone)]
struct Sidecar(Arc<SidecarBridge>);

impl Sidecar {
    async fn call(&self, method: &str, params: serde_json::Value) -> ToolResult {
        match self.0.request(method, params).await {
            Ok(v) => ToolResult {
                success: true,
                data: v,
                error: None,
            },
            Err(e) => ToolResult {
                success: false,
                data: serde_json::Value::Null,
                error: Some(e.to_string()),
            },
        }
    }
}

// ── search_news ────────────────────────────────────────────────────────────────

struct SearchNews(Sidecar);

#[async_trait]
impl ToolHandler for SearchNews {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        self.0.call("news.search", params).await
    }
}

// ── fetch_article ──────────────────────────────────────────────────────────────

struct FetchArticle(Sidecar);

#[async_trait]
impl ToolHandler for FetchArticle {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        self.0.call("news.fetch_article", params).await
    }
}

// ── list_news_sources ──────────────────────────────────────────────────────────

struct ListNewsSources(Sidecar);

#[async_trait]
impl ToolHandler for ListNewsSources {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        self.0.call("news.list_sources", params).await
    }
}

// ── news_status ────────────────────────────────────────────────────────────────

struct NewsStatus(Sidecar);

#[async_trait]
impl ToolHandler for NewsStatus {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        self.0.call("news.get_status", params).await
    }
}

// ── Register ───────────────────────────────────────────────────────────────────

pub fn register(reg: &ToolRegistry, bridge: Arc<SidecarBridge>) {
    let sc = Sidecar(bridge);

    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        (
            ToolDef {
                name: "search_news".into(),
                description: "Search recent news articles for any topic. Returns deduplicated, \
                    trust-scored results from curated RSS sources plus optional GDELT coverage. \
                    Supports freshness-aware ranking and regional source preference (for example \
                    India-focused authentic coverage). Results are clustered by story so you see \
                    one entry per event with a cross-reference count. Always use this before \
                    summarising news.".into(),
                category: "news".into(),
                default_tier: RiskLevel::Green,
                min_tier: "lite",
                parameters: vec![
                    param("query",     "string",  "Topic, keywords, or question to search for", true),
                    param("hours",     "integer", "How many hours back to search (default depends on freshness_mode: live=6, recent=24, archive=168; max: 336)", false),
                    param("freshness_mode", "string", "Freshness policy: live | recent | archive (default: recent)", false),
                    param("min_trust", "integer", "Minimum source tier: 1=wire services only, 2=major outlets, 3=all sources including GDELT (default: 3)", false),
                    param("limit",     "integer", "Max number of stories to return (default: 10)", false),
                    param("use_gdelt", "boolean", "Also query GDELT live for broader coverage (default: true)", false),
                    param("country",   "string",  "Optional preferred country ISO code (e.g. IN, US)", false),
                    param("region",    "string",  "Optional preferred region tag (e.g. south-asia, europe)", false),
                    param("language",  "string",  "Optional preferred language code (e.g. en)", false),
                    param("source_profile", "string", "Source profile: balanced | authentic | global_authentic | india | india_authentic", false),
                ],
            },
            Arc::new(SearchNews(sc.clone())),
        ),
        (
            ToolDef {
                name: "fetch_article".into(),
                description: "Fetch and extract the full text of a news article from a URL. \
                    Use this after search_news to read the complete story from a result's URL. \
                    Returns clean article text, author, date, and metadata.".into(),
                category: "news".into(),
                default_tier: RiskLevel::Green,
                min_tier: "lite",
                parameters: vec![
                    param("url", "string", "Full URL of the article to fetch", true),
                ],
            },
            Arc::new(FetchArticle(sc.clone())),
        ),
        (
            ToolDef {
                name: "list_news_sources".into(),
                description: "List all news sources being monitored, their trust tier, \
                    and when they were last polled. Useful for transparency about where \
                    news data comes from.".into(),
                category: "news".into(),
                default_tier: RiskLevel::Green,
                min_tier: "lite",
                parameters: vec![],
            },
            Arc::new(ListNewsSources(sc.clone())),
        ),
        (
            ToolDef {
                name: "news_status".into(),
                description: "Get news poller status: total articles indexed, how many from \
                    the last 24h, and DB health. Use to check if the news system is working.".into(),
                category: "news".into(),
                default_tier: RiskLevel::Green,
                min_tier: "lite",
                parameters: vec![],
            },
            Arc::new(NewsStatus(sc.clone())),
        ),
    ];

    for (def, handler) in tools {
        reg.register(def, handler);
    }
}
