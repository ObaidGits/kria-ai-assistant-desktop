/// Phase 2 — Internet, Search & Real-Time Access tests
///
/// Validates: SearXNG tool registration, weather/news/exchange/calculator tools,
/// time zone computation, math expression evaluator safety, and tool registry completeness.

use kria_core::tools::registry;

// ── Tool registration ──────────────────────────────────────────

#[test]
fn phase2_internet_tools_registered() {
    let reg = registry::build_default_registry();
    // Original tools
    assert!(reg.get_def("web_search").is_some());
    assert!(reg.get_def("fetch_webpage").is_some());
    assert!(reg.get_def("check_url_status").is_some());
    assert!(reg.get_def("get_public_ip").is_some());
    assert!(reg.get_def("ping_host").is_some());
    assert!(reg.get_def("dns_lookup").is_some());
    assert!(reg.get_def("speed_test").is_some());
    assert!(reg.get_def("download_file").is_some());
    // Phase 2 new tools
    assert!(reg.get_def("searxng_search").is_some());
    assert!(reg.get_def("get_current_time").is_some());
    assert!(reg.get_def("get_weather").is_some());
    assert!(reg.get_def("get_news").is_some());
    assert!(reg.get_def("get_exchange_rate").is_some());
    assert!(reg.get_def("calculate").is_some());
}

#[test]
fn phase2_tools_are_green_tier() {
    let reg = registry::build_default_registry();
    let green_tools = ["searxng_search", "get_current_time", "get_weather", "get_news", "get_exchange_rate", "calculate"];
    for name in &green_tools {
        let def = reg.get_def(name).unwrap_or_else(|| panic!("missing tool: {name}"));
        assert_eq!(def.default_tier, kria_core::safety::RiskLevel::Green, "{name} should be GREEN tier");
    }
}

#[test]
fn phase2_tools_have_correct_categories() {
    let reg = registry::build_default_registry();
    let names = ["searxng_search", "get_current_time", "get_weather", "get_news", "get_exchange_rate", "calculate"];
    for name in &names {
        let def = reg.get_def(name).unwrap();
        assert_eq!(def.category, "internet", "{name} should be in 'internet' category");
    }
}

#[test]
fn phase2_total_tool_count_increased() {
    let reg = registry::build_default_registry();
    // Phase 0 had ~60 tools, Phase 2 adds 6 new
    assert!(reg.len() >= 66, "expected >= 66 tools, got {}", reg.len());
}

#[test]
fn phase2_tools_generate_function_schemas() {
    let reg = registry::build_default_registry();
    let schemas = reg.function_schemas("lite");
    // All new tools should appear in schemas for lite tier
    let names: Vec<String> = schemas.iter()
        .filter_map(|s| s["function"]["name"].as_str().map(String::from))
        .collect();
    assert!(names.contains(&"get_weather".to_string()), "weather tool missing from schemas");
    assert!(names.contains(&"calculate".to_string()), "calculate tool missing from schemas");
    assert!(names.contains(&"get_current_time".to_string()), "time tool missing from schemas");
}

// ── GetCurrentTime ─────────────────────────────────────────────

#[tokio::test]
async fn current_time_utc() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("get_current_time").unwrap().clone();
    let result = handler.execute(serde_json::json!({})).await;
    assert!(result.success);
    assert!(result.data["timezone"].as_str().unwrap().contains("UTC"));
    assert!(result.data["unix_timestamp"].as_i64().is_some());
    assert!(result.data["day_of_week"].as_str().is_some());
}

#[tokio::test]
async fn current_time_est() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("get_current_time").unwrap().clone();
    let result = handler.execute(serde_json::json!({"timezone": "EST"})).await;
    assert!(result.success);
    assert!(result.data["timezone"].as_str().unwrap().contains("EST"));
}

#[tokio::test]
async fn current_time_jst() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("get_current_time").unwrap().clone();
    let result = handler.execute(serde_json::json!({"timezone": "JST"})).await;
    assert!(result.success);
    assert!(result.data["timezone"].as_str().unwrap().contains("JST"));
}

#[tokio::test]
async fn current_time_numeric_offset() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("get_current_time").unwrap().clone();
    let result = handler.execute(serde_json::json!({"timezone": "5"})).await;
    assert!(result.success);
}

// ── Calculate ──────────────────────────────────────────────────

#[tokio::test]
async fn calculate_basic_arithmetic() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("calculate").unwrap().clone();

    let result = handler.execute(serde_json::json!({"expression": "2 + 3 * 4"})).await;
    assert!(result.success);
    assert_eq!(result.data["result"].as_f64().unwrap(), 14.0);
}

#[tokio::test]
async fn calculate_parentheses() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("calculate").unwrap().clone();

    let result = handler.execute(serde_json::json!({"expression": "(2 + 3) * 4"})).await;
    assert!(result.success);
    assert_eq!(result.data["result"].as_f64().unwrap(), 20.0);
}

#[tokio::test]
async fn calculate_power() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("calculate").unwrap().clone();

    let result = handler.execute(serde_json::json!({"expression": "2^10"})).await;
    assert!(result.success);
    assert_eq!(result.data["result"].as_f64().unwrap(), 1024.0);
}

#[tokio::test]
async fn calculate_sqrt_function() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("calculate").unwrap().clone();

    let result = handler.execute(serde_json::json!({"expression": "sqrt(144)"})).await;
    assert!(result.success);
    assert_eq!(result.data["result"].as_f64().unwrap(), 12.0);
}

#[tokio::test]
async fn calculate_pi_constant() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("calculate").unwrap().clone();

    let result = handler.execute(serde_json::json!({"expression": "pi * 2"})).await;
    assert!(result.success);
    let val = result.data["result"].as_f64().unwrap();
    assert!((val - std::f64::consts::TAU).abs() < 0.001);
}

#[tokio::test]
async fn calculate_complex_expression() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("calculate").unwrap().clone();

    let result = handler.execute(serde_json::json!({"expression": "2^10 + sqrt(144)"})).await;
    assert!(result.success);
    assert_eq!(result.data["result"].as_f64().unwrap(), 1036.0);
}

#[tokio::test]
async fn calculate_division_by_zero() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("calculate").unwrap().clone();

    let result = handler.execute(serde_json::json!({"expression": "1 / 0"})).await;
    assert!(!result.success);
}

#[tokio::test]
async fn calculate_rejects_invalid_chars() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("calculate").unwrap().clone();

    let result = handler.execute(serde_json::json!({"expression": "system('ls')"})).await;
    assert!(!result.success, "should reject shell injection");
}

#[tokio::test]
async fn calculate_negative_numbers() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("calculate").unwrap().clone();

    let result = handler.execute(serde_json::json!({"expression": "-5 + 3"})).await;
    assert!(result.success);
    assert_eq!(result.data["result"].as_f64().unwrap(), -2.0);
}

// ── SearchConfig ───────────────────────────────────────────────

#[test]
fn config_has_search_defaults() {
    let config = kria_core::config::KriaConfig::default();
    assert_eq!(config.search.engine, "duckduckgo");
    assert!(!config.search.searxng_url.is_empty());
    assert!(!config.search.news_feeds.is_empty());
}

// ── System prompt includes datetime ────────────────────────────

#[test]
fn system_prompt_includes_datetime() {
    let prompt = kria_core::agent::prompts::build_system_prompt(
        "tools here", "TestUser", "linux", "standard", "apt", ""
    );
    assert!(prompt.contains("Current Date/Time:"), "prompt should include datetime");
    assert!(prompt.contains("TestUser"), "prompt should include user name");
}

#[test]
fn system_prompt_news_rules_include_freshness_and_region_controls() {
    let prompt = kria_core::agent::prompts::build_system_prompt(
        "tools here", "TestUser", "linux", "standard", "apt", ""
    );
    assert!(
        prompt.contains("freshness_mode=live"),
        "prompt should guide live freshness mode for breaking updates"
    );
    assert!(
        prompt.contains("source_profile=authentic") && prompt.contains("india_authentic"),
        "prompt should include authenticity and India-specific profile guidance"
    );
    assert!(
        prompt.contains("country") && prompt.contains("region"),
        "prompt should guide regional targeting for news queries"
    );
}
