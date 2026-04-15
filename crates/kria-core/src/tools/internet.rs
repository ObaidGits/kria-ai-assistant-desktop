use std::sync::Arc;
use async_trait::async_trait;
use crate::infra::ToolResult;
use crate::safety::RiskLevel;
use crate::tools::registry::{ToolRegistry, ToolDef, ToolHandler, ParamDef};

fn param(name: &str, ty: &str, desc: &str, required: bool) -> ParamDef {
    ParamDef { name: name.into(), param_type: ty.into(), description: desc.into(), required, default: None }
}

struct WebSearch;
#[async_trait] impl ToolHandler for WebSearch {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let query = params["query"].as_str().unwrap_or("");
        let max_results = params["max_results"].as_u64().unwrap_or(5);
        // Uses DuckDuckGo lite HTML (no API key required)
        let client = reqwest::Client::new();
        let resp = client.get("https://lite.duckduckgo.com/lite/")
            .query(&[("q", query)])
            .header("User-Agent", "KRIA/0.1")
            .send().await;

        match resp {
            Ok(r) => {
                let text = r.text().await.unwrap_or_default();
                // Parse results from HTML (simplified)
                let doc = scraper::Html::parse_document(&text);
                let selector = scraper::Selector::parse("a.result-link, .result-snippet").ok();
                let mut results = Vec::new();
                if let Some(sel) = selector {
                    for el in doc.select(&sel).take(max_results as usize) {
                        results.push(el.text().collect::<String>().trim().to_string());
                    }
                }
                ToolResult::ok(serde_json::json!({
                    "query": query,
                    "results": results,
                }))
            }
            Err(e) => ToolResult::err(format!("web_search failed: {e}"))
        }
    }
}

struct FetchWebpage;
#[async_trait] impl ToolHandler for FetchWebpage {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let url = params["url"].as_str().unwrap_or("");
        let max_chars = params["max_chars"].as_u64().unwrap_or(20000) as usize;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build().unwrap_or_default();

        match client.get(url).header("User-Agent", "KRIA/0.1").send().await {
            Ok(resp) => {
                let text = resp.text().await.unwrap_or_default();
                // Strip HTML tags for text content
                let doc = scraper::Html::parse_document(&text);
                let body_sel = scraper::Selector::parse("body").ok();
                let body_text = body_sel.and_then(|sel| {
                    doc.select(&sel).next().map(|el| el.text().collect::<String>())
                }).unwrap_or(text);

                let content = if body_text.len() > max_chars {
                    &body_text[..max_chars]
                } else {
                    &body_text
                };
                ToolResult::ok(serde_json::json!({
                    "url": url,
                    "content": content.trim(),
                    "truncated": body_text.len() > max_chars,
                }))
            }
            Err(e) => ToolResult::err(format!("fetch_webpage failed: {e}"))
        }
    }
}

struct CheckUrlStatus;
#[async_trait] impl ToolHandler for CheckUrlStatus {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let url = params["url"].as_str().unwrap_or("");
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build().unwrap_or_default();
        match client.head(url).send().await {
            Ok(resp) => ToolResult::ok(serde_json::json!({
                "url": url,
                "status": resp.status().as_u16(),
                "reachable": resp.status().is_success() || resp.status().is_redirection(),
            })),
            Err(e) => ToolResult::ok(serde_json::json!({
                "url": url,
                "reachable": false,
                "error": e.to_string(),
            }))
        }
    }
}

struct GetPublicIp;
#[async_trait] impl ToolHandler for GetPublicIp {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        let client = reqwest::Client::new();
        match client.get("https://api.ipify.org?format=json").send().await {
            Ok(resp) => {
                let body: serde_json::Value = resp.json().await.unwrap_or_default();
                ToolResult::ok(body)
            }
            Err(e) => ToolResult::err(format!("get_public_ip failed: {e}"))
        }
    }
}

struct PingHost;
#[async_trait] impl ToolHandler for PingHost {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let host = params["host"].as_str().unwrap_or("");
        let count = params["count"].as_u64().unwrap_or(4).to_string();
        let output = tokio::process::Command::new("ping")
            .args(["-c", &count, host])
            .output().await;
        match output {
            Ok(o) => ToolResult::ok(serde_json::json!({
                "host": host,
                "success": o.status.success(),
                "output": String::from_utf8_lossy(&o.stdout).to_string(),
            })),
            Err(e) => ToolResult::err(format!("ping failed: {e}"))
        }
    }
}

struct DnsLookup;
#[async_trait] impl ToolHandler for DnsLookup {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let domain = params["domain"].as_str().unwrap_or("");
        let output = tokio::process::Command::new("dig")
            .args(["+short", domain])
            .output().await;
        match output {
            Ok(o) => {
                let records: Vec<String> = String::from_utf8_lossy(&o.stdout)
                    .lines().map(String::from).collect();
                ToolResult::ok(serde_json::json!({ "domain": domain, "records": records }))
            }
            Err(e) => ToolResult::err(format!("dns_lookup failed: {e}"))
        }
    }
}

struct DownloadFile;
#[async_trait] impl ToolHandler for DownloadFile {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let url = params["url"].as_str().unwrap_or("");
        let dest = params["destination"].as_str().unwrap_or("");
        let max_mb = params["max_size_mb"].as_u64().unwrap_or(500);

        let client = reqwest::Client::new();
        let resp = match client.get(url).send().await {
            Ok(r) => r,
            Err(e) => return ToolResult::err(format!("download failed: {e}")),
        };

        if let Some(len) = resp.content_length() {
            if len > max_mb * 1024 * 1024 {
                return ToolResult::err(format!("file too large: {} MB (max {max_mb} MB)", len / (1024*1024)));
            }
        }

        let bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => return ToolResult::err(format!("download failed: {e}")),
        };

        if let Some(parent) = std::path::Path::new(dest).parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        match tokio::fs::write(dest, &bytes).await {
            Ok(_) => ToolResult::ok(serde_json::json!({
                "url": url,
                "destination": dest,
                "size_bytes": bytes.len(),
            })),
            Err(e) => ToolResult::err(format!("write failed: {e}"))
        }
    }
}

struct SpeedTest;
#[async_trait] impl ToolHandler for SpeedTest {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        // Simple download speed test: download a 1MB test file and measure time
        let url = "https://speed.cloudflare.com/__down?bytes=1048576";
        let start = std::time::Instant::now();
        let client = reqwest::Client::new();
        match client.get(url).send().await {
            Ok(resp) => {
                let bytes = resp.bytes().await.unwrap_or_default();
                let elapsed = start.elapsed().as_secs_f64();
                let mbps = (bytes.len() as f64 * 8.0) / (elapsed * 1_000_000.0);
                ToolResult::ok(serde_json::json!({
                    "download_mbps": format!("{:.1}", mbps),
                    "bytes_downloaded": bytes.len(),
                    "elapsed_seconds": format!("{:.2}", elapsed),
                }))
            }
            Err(e) => ToolResult::err(format!("speed test failed: {e}"))
        }
    }
}

pub fn register(reg: &mut ToolRegistry) {
    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        // GREEN
        (ToolDef {
            name: "web_search".into(), description: "Search the web using DuckDuckGo".into(),
            category: "internet".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![
                param("query", "string", "Search query", true),
                param("max_results", "integer", "Max results (default 5)", false),
            ],
        }, Arc::new(WebSearch)),
        (ToolDef {
            name: "fetch_webpage".into(), description: "Fetch and extract text from a webpage".into(),
            category: "internet".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![
                param("url", "string", "URL to fetch", true),
                param("max_chars", "integer", "Max chars (default 20000)", false),
            ],
        }, Arc::new(FetchWebpage)),
        (ToolDef {
            name: "check_url_status".into(), description: "Check if a URL is reachable".into(),
            category: "internet".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![param("url", "string", "URL to check", true)],
        }, Arc::new(CheckUrlStatus)),
        (ToolDef {
            name: "get_public_ip".into(), description: "Get public IP address".into(),
            category: "internet".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![],
        }, Arc::new(GetPublicIp)),
        (ToolDef {
            name: "ping_host".into(), description: "Ping a host and get response".into(),
            category: "internet".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![
                param("host", "string", "Hostname or IP", true),
                param("count", "integer", "Number of pings (default 4)", false),
            ],
        }, Arc::new(PingHost)),
        (ToolDef {
            name: "dns_lookup".into(), description: "DNS lookup for a domain".into(),
            category: "internet".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![param("domain", "string", "Domain name", true)],
        }, Arc::new(DnsLookup)),
        (ToolDef {
            name: "speed_test".into(), description: "Simple network speed test (download)".into(),
            category: "internet".into(), default_tier: RiskLevel::Green, min_tier: "standard",
            parameters: vec![],
        }, Arc::new(SpeedTest)),
        // YELLOW
        (ToolDef {
            name: "download_file".into(), description: "Download a file from URL to disk".into(),
            category: "internet".into(), default_tier: RiskLevel::Yellow, min_tier: "lite",
            parameters: vec![
                param("url", "string", "URL to download", true),
                param("destination", "string", "Local file path", true),
                param("max_size_mb", "integer", "Max file size in MB (default 500)", false),
            ],
        }, Arc::new(DownloadFile)),
    ];
    for (def, handler) in tools { reg.register(def, handler); }
}
