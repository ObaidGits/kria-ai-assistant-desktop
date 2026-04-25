use crate::infra::ToolResult;
use crate::safety::RiskLevel;
use crate::tools::registry::{ParamDef, ToolDef, ToolHandler, ToolRegistry};
use async_trait::async_trait;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

const WEB_RETRY_ATTEMPTS: usize = 3;

fn backoff_delay(attempt: usize) -> Duration {
    let shift = attempt.min(4) as u32;
    let ms = 250u64.saturating_mul(1u64 << shift);
    Duration::from_millis(ms)
}

fn is_private_or_sensitive_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.octets()[0] == 0
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_multicast()
                || v6.is_unspecified()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
        }
    }
}

fn validate_safe_url(raw_url: &str) -> Result<reqwest::Url, String> {
    let parsed = reqwest::Url::parse(raw_url).map_err(|e| format!("invalid url: {e}"))?;

    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("unsupported URL scheme (only http/https allowed)".to_string());
    }

    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("URLs with embedded credentials are not allowed".to_string());
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| "URL host is missing".to_string())?
        .to_ascii_lowercase();

    if host == "localhost"
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host.ends_with(".localhost")
    {
        return Err("local/internal hosts are blocked".to_string());
    }

    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_private_or_sensitive_ip(ip) {
            return Err("private/internal IP ranges are blocked".to_string());
        }
    }

    Ok(parsed)
}

async fn search_duckduckgo_lite(query: &str, max_results: usize) -> Result<Vec<String>, String> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(12))
        .build()
        .map_err(|e| format!("web_search client init failed: {e}"))?;

    let mut last_err = String::new();
    for attempt in 0..WEB_RETRY_ATTEMPTS {
        let resp = client
            .get("https://lite.duckduckgo.com/lite/")
            .query(&[("q", query)])
            .header("User-Agent", "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36")
            .send()
            .await;

        match resp {
            Ok(r) if r.status().is_success() => {
                let text = r.text().await.unwrap_or_default();
                let doc = scraper::Html::parse_document(&text);
                let selector = scraper::Selector::parse("a.result-link, .result-snippet").ok();
                let mut results = Vec::new();
                if let Some(sel) = selector {
                    for el in doc.select(&sel).take(max_results) {
                        results.push(el.text().collect::<String>().trim().to_string());
                    }
                }
                return Ok(results);
            }
            Ok(r) => {
                last_err = format!("duckduckgo returned status {}", r.status());
            }
            Err(e) => {
                last_err = e.to_string();
            }
        }

        if attempt + 1 < WEB_RETRY_ATTEMPTS {
            tokio::time::sleep(backoff_delay(attempt)).await;
        }
    }

    Err(format!("web_search failed after retries: {last_err}"))
}

fn param(name: &str, ty: &str, desc: &str, required: bool) -> ParamDef {
    ParamDef {
        name: name.into(),
        param_type: ty.into(),
        description: desc.into(),
        required,
        default: None,
    }
}

struct WebSearch;
#[async_trait]
impl ToolHandler for WebSearch {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let query = params["query"].as_str().unwrap_or("").trim();
        let max_results = params["max_results"].as_u64().unwrap_or(5) as usize;
        if query.is_empty() {
            return ToolResult::err("query is required".to_string());
        }

        match search_duckduckgo_lite(query, max_results).await {
            Ok(results) => ToolResult::ok(serde_json::json!({
                "query": query,
                "results": results,
            })),
            Err(e) => ToolResult::err(e),
        }
    }
}

struct FetchWebpage;
#[async_trait]
impl ToolHandler for FetchWebpage {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let url = params["url"].as_str().unwrap_or("").trim();
        let max_chars = params["max_chars"].as_u64().unwrap_or(20000) as usize;
        if url.is_empty() {
            return ToolResult::err("url is required".to_string());
        }
        let safe_url = match validate_safe_url(url) {
            Ok(u) => u,
            Err(e) => return ToolResult::err(format!("unsafe url: {e}")),
        };

        let content_limit =
            ((max_chars as u64).saturating_mul(8)).clamp(128 * 1024, 3 * 1024 * 1024);

        let client = match reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
        {
            Ok(c) => c,
            Err(e) => return ToolResult::err(format!("fetch_webpage client init failed: {e}")),
        };

        let mut last_err = String::new();
        for attempt in 0..WEB_RETRY_ATTEMPTS {
            let resp = client
                .get(safe_url.clone())
                .header("User-Agent", "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36")
                .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
                .header("Accept-Language", "en-US,en;q=0.5")
                .send()
                .await;

            match resp {
                Ok(resp) if resp.status().is_success() => {
                    let content_type = resp
                        .headers()
                        .get(reqwest::header::CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    // Only block binary formats; allow missing/empty CT (usually HTML)
                    let is_binary = !content_type.is_empty()
                        && (content_type.starts_with("image/")
                            || content_type.starts_with("audio/")
                            || content_type.starts_with("video/")
                            || content_type.starts_with("font/"));
                    if is_binary {
                        return ToolResult::err(format!(
                            "unsupported binary content type: {content_type}"
                        ));
                    }

                    if let Some(len) = resp.content_length() {
                        if len > content_limit {
                            return ToolResult::err(format!(
                                "response too large: {} bytes (limit {} bytes)",
                                len, content_limit
                            ));
                        }
                    }

                    let text = resp.text().await.unwrap_or_default();
                    // Strip HTML tags for text content
                    let doc = scraper::Html::parse_document(&text);
                    let body_sel = scraper::Selector::parse("body").ok();
                    let body_text = body_sel
                        .and_then(|sel| {
                            doc.select(&sel)
                                .next()
                                .map(|el| el.text().collect::<String>())
                        })
                        .unwrap_or(text);

                    let content = if body_text.len() > max_chars {
                        &body_text[..max_chars]
                    } else {
                        &body_text
                    };
                    return ToolResult::ok(serde_json::json!({
                        "url": url,
                        "content": content.trim(),
                        "truncated": body_text.len() > max_chars,
                    }));
                }
                Ok(resp) => {
                    last_err = format!("status {}", resp.status());
                }
                Err(e) => {
                    last_err = e.to_string();
                }
            }

            if attempt + 1 < WEB_RETRY_ATTEMPTS {
                tokio::time::sleep(backoff_delay(attempt)).await;
            }
        }

        ToolResult::err(format!("fetch_webpage failed after retries: {last_err}"))
    }
}

struct CheckUrlStatus;
#[async_trait]
impl ToolHandler for CheckUrlStatus {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let url = params["url"].as_str().unwrap_or("").trim();
        let safe_url = match validate_safe_url(url) {
            Ok(u) => u,
            Err(e) => {
                return ToolResult::ok(serde_json::json!({
                    "url": url,
                    "reachable": false,
                    "error": format!("unsafe url: {e}"),
                }));
            }
        };

        let client = match reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                return ToolResult::ok(serde_json::json!({
                    "url": url,
                    "reachable": false,
                    "error": format!("client init failed: {e}"),
                }));
            }
        };

        let mut last_err = String::new();
        for attempt in 0..WEB_RETRY_ATTEMPTS {
            match client.head(safe_url.clone()).send().await {
                Ok(resp) => {
                    return ToolResult::ok(serde_json::json!({
                        "url": url,
                        "status": resp.status().as_u16(),
                        "reachable": resp.status().is_success() || resp.status().is_redirection(),
                    }))
                }
                Err(e) => {
                    last_err = e.to_string();
                }
            }

            if attempt + 1 < WEB_RETRY_ATTEMPTS {
                tokio::time::sleep(backoff_delay(attempt)).await;
            }
        }

        ToolResult::ok(serde_json::json!({
            "url": url,
            "reachable": false,
            "error": format!("status check failed after retries: {last_err}"),
        }))
    }
}

struct GetPublicIp;
#[async_trait]
impl ToolHandler for GetPublicIp {
    async fn execute(&self, _params: serde_json::Value) -> ToolResult {
        let client = reqwest::Client::new();
        match client.get("https://api.ipify.org?format=json").send().await {
            Ok(resp) => {
                let body: serde_json::Value = resp.json().await.unwrap_or_default();
                ToolResult::ok(body)
            }
            Err(e) => ToolResult::err(format!("get_public_ip failed: {e}")),
        }
    }
}

struct PingHost;
#[async_trait]
impl ToolHandler for PingHost {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let host = params["host"].as_str().unwrap_or("");
        let count = params["count"].as_u64().unwrap_or(4).to_string();
        let output = tokio::process::Command::new("ping")
            .args(["-c", &count, host])
            .output()
            .await;
        match output {
            Ok(o) => ToolResult::ok(serde_json::json!({
                "host": host,
                "success": o.status.success(),
                "output": String::from_utf8_lossy(&o.stdout).to_string(),
            })),
            Err(e) => ToolResult::err(format!("ping failed: {e}")),
        }
    }
}

struct DnsLookup;
#[async_trait]
impl ToolHandler for DnsLookup {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let domain = params["domain"].as_str().unwrap_or("");
        let output = tokio::process::Command::new("dig")
            .args(["+short", domain])
            .output()
            .await;
        match output {
            Ok(o) => {
                let records: Vec<String> = String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .map(String::from)
                    .collect();
                ToolResult::ok(serde_json::json!({ "domain": domain, "records": records }))
            }
            Err(e) => ToolResult::err(format!("dns_lookup failed: {e}")),
        }
    }
}

struct DownloadFile;
#[async_trait]
impl ToolHandler for DownloadFile {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let url = params["url"].as_str().unwrap_or("").trim();
        let dest = params["destination"].as_str().unwrap_or("");
        let max_mb = params["max_size_mb"].as_u64().unwrap_or(500);
        if url.is_empty() {
            return ToolResult::err("url is required".to_string());
        }
        if dest.trim().is_empty() {
            return ToolResult::err("destination is required".to_string());
        }
        let safe_url = match validate_safe_url(url) {
            Ok(u) => u,
            Err(e) => return ToolResult::err(format!("unsafe url: {e}")),
        };

        let client = match reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
        {
            Ok(c) => c,
            Err(e) => return ToolResult::err(format!("download client init failed: {e}")),
        };

        let mut last_err = String::new();
        let mut resp_opt = None;
        for attempt in 0..WEB_RETRY_ATTEMPTS {
            match client.get(safe_url.clone()).send().await {
                Ok(r) if r.status().is_success() => {
                    resp_opt = Some(r);
                    break;
                }
                Ok(r) => {
                    last_err = format!("status {}", r.status());
                }
                Err(e) => {
                    last_err = e.to_string();
                }
            }
            if attempt + 1 < WEB_RETRY_ATTEMPTS {
                tokio::time::sleep(backoff_delay(attempt)).await;
            }
        }
        let resp = match resp_opt {
            Some(r) => r,
            None => return ToolResult::err(format!("download failed after retries: {last_err}")),
        };

        if let Some(len) = resp.content_length() {
            if len > max_mb * 1024 * 1024 {
                return ToolResult::err(format!(
                    "file too large: {} MB (max {max_mb} MB)",
                    len / (1024 * 1024)
                ));
            }
        }

        let bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => return ToolResult::err(format!("download failed: {e}")),
        };

        if bytes.len() as u64 > max_mb * 1024 * 1024 {
            return ToolResult::err(format!(
                "file too large after download: {} MB (max {max_mb} MB)",
                bytes.len() / (1024 * 1024)
            ));
        }

        if let Some(parent) = std::path::Path::new(dest).parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        match tokio::fs::write(dest, &bytes).await {
            Ok(_) => ToolResult::ok(serde_json::json!({
                "url": url,
                "destination": dest,
                "size_bytes": bytes.len(),
            })),
            Err(e) => ToolResult::err(format!("write failed: {e}")),
        }
    }
}

struct SpeedTest;
#[async_trait]
impl ToolHandler for SpeedTest {
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
            Err(e) => ToolResult::err(format!("speed test failed: {e}")),
        }
    }
}

// ── Phase 2 tools ───────────────────────────────────────────────────

struct SearxngSearch;
#[async_trait]
impl ToolHandler for SearxngSearch {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let query = params["query"].as_str().unwrap_or("").trim();
        let max_results = params["max_results"].as_u64().unwrap_or(5) as usize;
        let instance = params["instance_url"]
            .as_str()
            .unwrap_or("http://localhost:8888");

        if query.is_empty() {
            return ToolResult::err("query is required".to_string());
        }

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();

        let url = format!("{}/search", instance.trim_end_matches('/'));
        let resp = client
            .get(&url)
            .query(&[("q", query), ("format", "json"), ("language", "en")])
            .header("User-Agent", "KRIA/0.1")
            .send()
            .await;

        match resp {
            Ok(r) if r.status().is_success() => {
                let body: serde_json::Value = r.json().await.unwrap_or_default();
                let results: Vec<serde_json::Value> = body["results"]
                    .as_array()
                    .unwrap_or(&Vec::new())
                    .iter()
                    .take(max_results)
                    .map(|r| {
                        serde_json::json!({
                            "title": r["title"],
                            "url": r["url"],
                            "snippet": r["content"],
                            "engine": r["engine"],
                        })
                    })
                    .collect();
                ToolResult::ok(
                    serde_json::json!({ "query": query, "results": results, "count": results.len() }),
                )
            }
            Ok(r) => {
                let searx_err = format!("searxng returned status {}", r.status());
                tracing::warn!(instance, %searx_err, "searxng_search failed, falling back to DuckDuckGo");
                match search_duckduckgo_lite(query, max_results).await {
                    Ok(fallback_rows) => {
                        let results: Vec<serde_json::Value> = fallback_rows
                            .into_iter()
                            .map(|row| {
                                serde_json::json!({
                                    "title": row,
                                    "url": serde_json::Value::Null,
                                    "snippet": serde_json::Value::Null,
                                    "engine": "duckduckgo-lite",
                                })
                            })
                            .collect();
                        ToolResult::ok(serde_json::json!({
                            "query": query,
                            "results": results,
                            "count": results.len(),
                            "backend": "duckduckgo-lite",
                            "fallback_from": "searxng",
                            "fallback_reason": searx_err,
                        }))
                    }
                    Err(fallback_err) => ToolResult::err(format!(
                        "searxng_search failed ({searx_err}) and fallback web_search failed: {fallback_err}"
                    )),
                }
            }
            Err(e) => {
                let searx_err =
                    format!("searxng_search failed: {e}. Is SearXNG running at {instance}?");
                tracing::warn!(instance, %searx_err, "searxng_search failed, falling back to DuckDuckGo");
                match search_duckduckgo_lite(query, max_results).await {
                    Ok(fallback_rows) => {
                        let results: Vec<serde_json::Value> = fallback_rows
                            .into_iter()
                            .map(|row| {
                                serde_json::json!({
                                    "title": row,
                                    "url": serde_json::Value::Null,
                                    "snippet": serde_json::Value::Null,
                                    "engine": "duckduckgo-lite",
                                })
                            })
                            .collect();
                        ToolResult::ok(serde_json::json!({
                            "query": query,
                            "results": results,
                            "count": results.len(),
                            "backend": "duckduckgo-lite",
                            "fallback_from": "searxng",
                            "fallback_reason": searx_err,
                        }))
                    }
                    Err(fallback_err) => ToolResult::err(format!(
                        "{searx_err}; fallback web_search failed: {fallback_err}"
                    )),
                }
            }
        }
    }
}

struct GetCurrentTime;
#[async_trait]
impl ToolHandler for GetCurrentTime {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let tz_name = params["timezone"].as_str().unwrap_or("UTC");
        let now = chrono::Utc::now();

        // Try common timezone offsets
        let (display_time, tz_label) = match tz_name.to_uppercase().as_str() {
            "UTC" | "GMT" => (now.format("%Y-%m-%d %H:%M:%S").to_string(), "UTC"),
            "EST" | "US/EASTERN" => {
                let offset = chrono::FixedOffset::west_opt(5 * 3600).unwrap();
                (
                    now.with_timezone(&offset)
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string(),
                    "EST (UTC-5)",
                )
            }
            "CST" | "US/CENTRAL" => {
                let offset = chrono::FixedOffset::west_opt(6 * 3600).unwrap();
                (
                    now.with_timezone(&offset)
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string(),
                    "CST (UTC-6)",
                )
            }
            "MST" | "US/MOUNTAIN" => {
                let offset = chrono::FixedOffset::west_opt(7 * 3600).unwrap();
                (
                    now.with_timezone(&offset)
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string(),
                    "MST (UTC-7)",
                )
            }
            "PST" | "US/PACIFIC" => {
                let offset = chrono::FixedOffset::west_opt(8 * 3600).unwrap();
                (
                    now.with_timezone(&offset)
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string(),
                    "PST (UTC-8)",
                )
            }
            "CET" | "EUROPE/BERLIN" | "EUROPE/PARIS" => {
                let offset = chrono::FixedOffset::east_opt(3600).unwrap();
                (
                    now.with_timezone(&offset)
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string(),
                    "CET (UTC+1)",
                )
            }
            "JST" | "ASIA/TOKYO" => {
                let offset = chrono::FixedOffset::east_opt(9 * 3600).unwrap();
                (
                    now.with_timezone(&offset)
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string(),
                    "JST (UTC+9)",
                )
            }
            "IST" | "ASIA/KOLKATA" => {
                let offset = chrono::FixedOffset::east_opt(5 * 3600 + 1800).unwrap();
                (
                    now.with_timezone(&offset)
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string(),
                    "IST (UTC+5:30)",
                )
            }
            "PKT" | "ASIA/KARACHI" => {
                let offset = chrono::FixedOffset::east_opt(5 * 3600).unwrap();
                (
                    now.with_timezone(&offset)
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string(),
                    "PKT (UTC+5)",
                )
            }
            "AEST" | "AUSTRALIA/SYDNEY" => {
                let offset = chrono::FixedOffset::east_opt(10 * 3600).unwrap();
                (
                    now.with_timezone(&offset)
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string(),
                    "AEST (UTC+10)",
                )
            }
            _ => {
                // Try parsing as UTC offset like "+5" or "-8"
                if let Ok(hours) = tz_name.parse::<i32>() {
                    let offset = chrono::FixedOffset::east_opt(hours * 3600).unwrap();
                    (
                        now.with_timezone(&offset)
                            .format("%Y-%m-%d %H:%M:%S")
                            .to_string(),
                        tz_name,
                    )
                } else {
                    (
                        now.format("%Y-%m-%d %H:%M:%S").to_string(),
                        "UTC (unknown timezone, defaulting)",
                    )
                }
            }
        };

        ToolResult::ok(serde_json::json!({
            "datetime": display_time,
            "timezone": tz_label,
            "utc": now.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
            "unix_timestamp": now.timestamp(),
            "day_of_week": now.format("%A").to_string(),
        }))
    }
}

struct GetWeather;
#[async_trait]
impl ToolHandler for GetWeather {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let location = params["location"].as_str().unwrap_or("Berlin");

        // Step 1: Geocode location name → lat/lon via Open-Meteo geocoding API
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();

        let geo_resp = client
            .get("https://geocoding-api.open-meteo.com/v1/search")
            .query(&[("name", location), ("count", "1"), ("language", "en")])
            .send()
            .await;

        let (lat, lon, resolved_name) = match geo_resp {
            Ok(r) => {
                let body: serde_json::Value = r.json().await.unwrap_or_default();
                if let Some(result) = body["results"].as_array().and_then(|a| a.first()) {
                    (
                        result["latitude"].as_f64().unwrap_or(52.52),
                        result["longitude"].as_f64().unwrap_or(13.41),
                        result["name"].as_str().unwrap_or(location).to_string(),
                    )
                } else {
                    return ToolResult::err(format!("location not found: {location}"));
                }
            }
            Err(e) => return ToolResult::err(format!("geocoding failed: {e}")),
        };

        // Step 2: Get weather from Open-Meteo (free, no API key)
        let weather_resp = client
            .get("https://api.open-meteo.com/v1/forecast")
            .query(&[
                ("latitude", &lat.to_string()),
                ("longitude", &lon.to_string()),
                (
                    "current",
                    &"temperature_2m,relative_humidity_2m,wind_speed_10m,weather_code,is_day"
                        .to_string(),
                ),
                (
                    "daily",
                    &"temperature_2m_max,temperature_2m_min,precipitation_sum,weather_code"
                        .to_string(),
                ),
                ("timezone", &"auto".to_string()),
                ("forecast_days", &"3".to_string()),
            ])
            .send()
            .await;

        match weather_resp {
            Ok(r) => {
                let body: serde_json::Value = r.json().await.unwrap_or_default();
                let current = &body["current"];

                // Decode WMO weather codes to descriptions
                let weather_desc = match current["weather_code"].as_u64().unwrap_or(0) {
                    0 => "Clear sky",
                    1 => "Mainly clear",
                    2 => "Partly cloudy",
                    3 => "Overcast",
                    45 | 48 => "Foggy",
                    51..=55 => "Drizzle",
                    61..=65 => "Rain",
                    71..=75 => "Snow",
                    80..=82 => "Rain showers",
                    85 | 86 => "Snow showers",
                    95 => "Thunderstorm",
                    96 | 99 => "Thunderstorm with hail",
                    _ => "Unknown",
                };

                ToolResult::ok(serde_json::json!({
                    "location": resolved_name,
                    "coordinates": { "lat": lat, "lon": lon },
                    "current": {
                        "temperature_c": current["temperature_2m"],
                        "humidity_percent": current["relative_humidity_2m"],
                        "wind_speed_kmh": current["wind_speed_10m"],
                        "condition": weather_desc,
                        "is_day": current["is_day"],
                    },
                    "forecast": body["daily"],
                }))
            }
            Err(e) => ToolResult::err(format!("weather API failed: {e}")),
        }
    }
}

struct GetNews;
#[async_trait]
impl ToolHandler for GetNews {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let feed_url = params["feed_url"]
            .as_str()
            .unwrap_or("https://hnrss.org/frontpage");
        let max_items = params["max_items"].as_u64().unwrap_or(10) as usize;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();

        match client
            .get(feed_url)
            .header("User-Agent", "KRIA/0.1")
            .send()
            .await
        {
            Ok(resp) => {
                let xml = resp.text().await.unwrap_or_default();
                // Simple RSS/Atom parsing: extract <item>/<entry> titles and links
                let mut items = Vec::new();
                let doc = scraper::Html::parse_document(&xml);

                // Try RSS <item> format
                if let Ok(item_sel) = scraper::Selector::parse("item") {
                    let title_sel = scraper::Selector::parse("title").ok();
                    let link_sel = scraper::Selector::parse("link").ok();
                    let desc_sel = scraper::Selector::parse("description").ok();

                    for item in doc.select(&item_sel).take(max_items) {
                        let title = title_sel
                            .as_ref()
                            .and_then(|s| item.select(s).next())
                            .map(|e| e.text().collect::<String>())
                            .unwrap_or_default();
                        let link = link_sel
                            .as_ref()
                            .and_then(|s| item.select(s).next())
                            .map(|e| e.text().collect::<String>())
                            .unwrap_or_default();
                        let desc = desc_sel
                            .as_ref()
                            .and_then(|s| item.select(s).next())
                            .map(|e| e.text().collect::<String>())
                            .unwrap_or_default();

                        items.push(serde_json::json!({
                            "title": title.trim(),
                            "link": link.trim(),
                            "description": if desc.len() > 200 { &desc[..200] } else { &desc },
                        }));
                    }
                }

                // Try Atom <entry> format if no RSS items found
                if items.is_empty() {
                    if let Ok(entry_sel) = scraper::Selector::parse("entry") {
                        let title_sel = scraper::Selector::parse("title").ok();
                        let link_sel = scraper::Selector::parse("link").ok();

                        for entry in doc.select(&entry_sel).take(max_items) {
                            let title = title_sel
                                .as_ref()
                                .and_then(|s| entry.select(s).next())
                                .map(|e| e.text().collect::<String>())
                                .unwrap_or_default();
                            let link = link_sel
                                .as_ref()
                                .and_then(|s| entry.select(s).next())
                                .and_then(|e| e.value().attr("href").map(String::from))
                                .unwrap_or_default();

                            items.push(serde_json::json!({
                                "title": title.trim(),
                                "link": link.trim(),
                            }));
                        }
                    }
                }

                ToolResult::ok(serde_json::json!({
                    "feed_url": feed_url,
                    "items": items,
                    "count": items.len(),
                }))
            }
            Err(e) => ToolResult::err(format!("news feed failed: {e}")),
        }
    }
}

struct GetExchangeRate;
#[async_trait]
impl ToolHandler for GetExchangeRate {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let base = params["base_currency"]
            .as_str()
            .unwrap_or("USD")
            .to_uppercase();
        let target = params["target_currency"]
            .as_str()
            .unwrap_or("EUR")
            .to_uppercase();
        let amount = params["amount"].as_f64().unwrap_or(1.0);

        // Use ECB exchange rates via open API (free, no key)
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();

        let url = format!("https://open.er-api.com/v6/latest/{}", base);
        match client.get(&url).send().await {
            Ok(resp) => {
                let body: serde_json::Value = resp.json().await.unwrap_or_default();
                if let Some(rate) = body["rates"][&target].as_f64() {
                    let converted = amount * rate;
                    ToolResult::ok(serde_json::json!({
                        "base": base,
                        "target": target,
                        "rate": rate,
                        "amount": amount,
                        "converted": format!("{:.2}", converted),
                        "last_update": body["time_last_update_utc"],
                    }))
                } else {
                    ToolResult::err(format!("currency not found: {target}"))
                }
            }
            Err(e) => ToolResult::err(format!("exchange rate failed: {e}")),
        }
    }
}

struct Calculate;
#[async_trait]
impl ToolHandler for Calculate {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        let expression = params["expression"].as_str().unwrap_or("");

        // Simple safe expression evaluator — supports basic arithmetic
        // Using a minimal recursive descent parser to avoid any code injection
        match eval_math(expression) {
            Ok(result) => ToolResult::ok(serde_json::json!({
                "expression": expression,
                "result": result,
            })),
            Err(e) => ToolResult::err(format!("calculation error: {e}")),
        }
    }
}

/// Minimal safe math expression evaluator.
/// Supports: +, -, *, /, %, ^, parentheses, and basic functions (sqrt, abs, sin, cos, tan, log, ln, pi, e).
fn eval_math(expr: &str) -> Result<f64, String> {
    let expr = expr.trim();
    if expr.is_empty() {
        return Err("empty expression".into());
    }

    // Sanitize: only allow digits, operators, parens, dots, spaces, and function names
    let allowed = |c: char| c.is_alphanumeric() || "+-*/%.^() ,".contains(c);
    if !expr.chars().all(allowed) {
        return Err("invalid characters in expression".into());
    }

    eval_expr(&mut expr.chars().peekable())
}

fn eval_expr(chars: &mut std::iter::Peekable<std::str::Chars>) -> Result<f64, String> {
    let mut result = eval_term(chars)?;
    loop {
        skip_spaces(chars);
        match chars.peek() {
            Some(&'+') => {
                chars.next();
                result += eval_term(chars)?;
            }
            Some(&'-') => {
                chars.next();
                result -= eval_term(chars)?;
            }
            _ => break,
        }
    }
    Ok(result)
}

fn eval_term(chars: &mut std::iter::Peekable<std::str::Chars>) -> Result<f64, String> {
    let mut result = eval_power(chars)?;
    loop {
        skip_spaces(chars);
        match chars.peek() {
            Some(&'*') => {
                chars.next();
                result *= eval_power(chars)?;
            }
            Some(&'/') => {
                chars.next();
                let divisor = eval_power(chars)?;
                if divisor == 0.0 {
                    return Err("division by zero".into());
                }
                result /= divisor;
            }
            Some(&'%') => {
                chars.next();
                let divisor = eval_power(chars)?;
                if divisor == 0.0 {
                    return Err("modulo by zero".into());
                }
                result %= divisor;
            }
            _ => break,
        }
    }
    Ok(result)
}

fn eval_power(chars: &mut std::iter::Peekable<std::str::Chars>) -> Result<f64, String> {
    let base = eval_unary(chars)?;
    skip_spaces(chars);
    if chars.peek() == Some(&'^') {
        chars.next();
        let exp = eval_unary(chars)?;
        Ok(base.powf(exp))
    } else {
        Ok(base)
    }
}

fn eval_unary(chars: &mut std::iter::Peekable<std::str::Chars>) -> Result<f64, String> {
    skip_spaces(chars);
    if chars.peek() == Some(&'-') {
        chars.next();
        Ok(-eval_atom(chars)?)
    } else if chars.peek() == Some(&'+') {
        chars.next();
        eval_atom(chars)
    } else {
        eval_atom(chars)
    }
}

fn eval_atom(chars: &mut std::iter::Peekable<std::str::Chars>) -> Result<f64, String> {
    skip_spaces(chars);
    if chars.peek() == Some(&'(') {
        chars.next();
        let result = eval_expr(chars)?;
        skip_spaces(chars);
        if chars.peek() == Some(&')') {
            chars.next();
        }
        return Ok(result);
    }

    // Check for function names or constants
    let mut name = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_alphabetic() || c == '_' {
            name.push(c);
            chars.next();
        } else {
            break;
        }
    }

    if !name.is_empty() {
        let name_lower = name.to_lowercase();
        match name_lower.as_str() {
            "pi" => return Ok(std::f64::consts::PI),
            "e" => return Ok(std::f64::consts::E),
            "sqrt" | "abs" | "sin" | "cos" | "tan" | "log" | "ln" | "ceil" | "floor" | "round" => {
                skip_spaces(chars);
                if chars.peek() == Some(&'(') {
                    chars.next();
                    let arg = eval_expr(chars)?;
                    skip_spaces(chars);
                    if chars.peek() == Some(&')') {
                        chars.next();
                    }
                    return match name_lower.as_str() {
                        "sqrt" => Ok(arg.sqrt()),
                        "abs" => Ok(arg.abs()),
                        "sin" => Ok(arg.sin()),
                        "cos" => Ok(arg.cos()),
                        "tan" => Ok(arg.tan()),
                        "log" => Ok(arg.log10()),
                        "ln" => Ok(arg.ln()),
                        "ceil" => Ok(arg.ceil()),
                        "floor" => Ok(arg.floor()),
                        "round" => Ok(arg.round()),
                        _ => unreachable!(),
                    };
                }
                return Err(format!("expected '(' after function '{name}'"));
            }
            _ => return Err(format!("unknown function: {name}")),
        }
    }

    // Parse number
    let mut num_str = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() || c == '.' {
            num_str.push(c);
            chars.next();
        } else {
            break;
        }
    }
    skip_spaces(chars);

    if num_str.is_empty() {
        return Err("expected number".into());
    }
    num_str
        .parse::<f64>()
        .map_err(|_| format!("invalid number: {num_str}"))
}

fn skip_spaces(chars: &mut std::iter::Peekable<std::str::Chars>) {
    while chars.peek() == Some(&' ') {
        chars.next();
    }
}

pub fn register(reg: &ToolRegistry) {
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
        // Phase 2 tools
        (ToolDef {
            name: "searxng_search".into(),
            description: "Search the web via a SearXNG instance (structured results)".into(),
            category: "internet".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![
                param("query", "string", "Search query", true),
                param("max_results", "integer", "Max results (default 5)", false),
                param("instance_url", "string", "SearXNG URL (default http://localhost:8888)", false),
            ],
        }, Arc::new(SearxngSearch)),
        (ToolDef {
            name: "get_current_time".into(),
            description: "Get the current date, time, and day of week in any timezone".into(),
            category: "internet".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![
                param("timezone", "string", "Timezone name (UTC, EST, PST, JST, IST, PKT, CET, AEST) or offset like +5, -8. Default: UTC", false),
            ],
        }, Arc::new(GetCurrentTime)),
        (ToolDef {
            name: "get_weather".into(),
            description: "Get current weather and 3-day forecast for a location (Open-Meteo, free, no API key)".into(),
            category: "internet".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![
                param("location", "string", "City name or location", true),
            ],
        }, Arc::new(GetWeather)),
        (ToolDef {
            name: "get_news".into(),
            description: "Fetch latest news headlines from an RSS/Atom feed".into(),
            category: "internet".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![
                param("feed_url", "string", "RSS/Atom feed URL (default: Hacker News)", false),
                param("max_items", "integer", "Max items (default 10)", false),
            ],
        }, Arc::new(GetNews)),
        (ToolDef {
            name: "get_exchange_rate".into(),
            description: "Get currency exchange rates and convert amounts".into(),
            category: "internet".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![
                param("base_currency", "string", "Base currency code, e.g. USD", true),
                param("target_currency", "string", "Target currency code, e.g. EUR", true),
                param("amount", "number", "Amount to convert (default 1.0)", false),
            ],
        }, Arc::new(GetExchangeRate)),
        (ToolDef {
            name: "calculate".into(),
            description: "Evaluate a mathematical expression safely. Supports: +, -, *, /, %, ^, sqrt, abs, sin, cos, tan, log, ln, pi, e".into(),
            category: "internet".into(), default_tier: RiskLevel::Green, min_tier: "lite",
            parameters: vec![
                param("expression", "string", "Mathematical expression, e.g. '2^10 + sqrt(144)'", true),
            ],
        }, Arc::new(Calculate)),
    ];
    for (def, handler) in tools {
        reg.register(def, handler);
    }
}

#[cfg(test)]
mod tests {
    use super::validate_safe_url;

    #[test]
    fn allows_public_https_url() {
        assert!(validate_safe_url("https://example.com/path?q=1").is_ok());
    }

    #[test]
    fn blocks_localhost_and_private_ips() {
        assert!(validate_safe_url("http://localhost:8080").is_err());
        assert!(validate_safe_url("http://127.0.0.1:8080").is_err());
        assert!(validate_safe_url("http://10.0.0.5").is_err());
        assert!(validate_safe_url("http://192.168.1.3").is_err());
    }

    #[test]
    fn blocks_non_http_schemes_and_embedded_credentials() {
        assert!(validate_safe_url("file:///etc/passwd").is_err());
        assert!(validate_safe_url("ftp://example.com/file.txt").is_err());
        assert!(validate_safe_url("https://user:pass@example.com").is_err());
    }
}
