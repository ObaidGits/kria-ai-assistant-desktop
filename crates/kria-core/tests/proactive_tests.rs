/// Phase 13 — Proactive Intelligence & Smart Notifications Tests
/// Tests system health monitoring, alert management, file watcher config, and proactive tools.

use std::sync::Arc;
use kria_core::automation::proactive::{ProactiveEngine, HealthThresholds, AlertCategory};

fn make_engine() -> Arc<ProactiveEngine> {
    Arc::new(ProactiveEngine::new(HealthThresholds::default()))
}

// ── Alert Management ──

#[tokio::test]
async fn alerts_empty_initially() {
    let engine = make_engine();
    let alerts = engine.get_alerts().await;
    assert!(alerts.is_empty());
}

#[tokio::test]
async fn push_and_get_alert() {
    let engine = make_engine();
    engine.push_alert(AlertCategory::Info, "Test Alert", "This is a test", None).await;
    let alerts = engine.get_alerts().await;
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].title, "Test Alert");
    assert_eq!(alerts[0].category, AlertCategory::Info);
}

#[tokio::test]
async fn dismiss_alert() {
    let engine = make_engine();
    engine.push_alert(AlertCategory::Alert, "Dismiss Me", "msg", None).await;
    let alerts = engine.get_alerts().await;
    let id = alerts[0].id.clone();

    assert!(engine.dismiss_alert(&id).await);
    let undismissed = engine.get_alerts().await;
    assert!(undismissed.is_empty());

    // All alerts include dismissed
    let all = engine.get_all_alerts().await;
    assert_eq!(all.len(), 1);
    assert!(all[0].dismissed);
}

#[tokio::test]
async fn dismiss_nonexistent_alert() {
    let engine = make_engine();
    assert!(!engine.dismiss_alert("nonexistent").await);
}

#[tokio::test]
async fn push_multiple_alert_categories() {
    let engine = make_engine();
    engine.push_alert(AlertCategory::Alert, "Alert 1", "msg", Some("fix it")).await;
    engine.push_alert(AlertCategory::Suggestion, "Suggestion 1", "msg", None).await;
    engine.push_alert(AlertCategory::Info, "Info 1", "msg", None).await;
    let alerts = engine.get_alerts().await;
    assert_eq!(alerts.len(), 3);
}

#[tokio::test]
async fn alert_suggestion_field() {
    let engine = make_engine();
    engine.push_alert(AlertCategory::Alert, "Low disk", "10% free", Some("Clean up files")).await;
    let alerts = engine.get_alerts().await;
    assert_eq!(alerts[0].suggestion.as_deref(), Some("Clean up files"));
}

#[tokio::test]
async fn alert_cap_at_100() {
    let engine = make_engine();
    for i in 0..120 {
        engine.push_alert(AlertCategory::Info, &format!("Alert {i}"), "msg", None).await;
    }
    let all = engine.get_all_alerts().await;
    assert!(all.len() <= 100, "expected max 100 alerts, got {}", all.len());
}

// ── Health Thresholds ──

#[test]
fn default_thresholds() {
    let t = HealthThresholds::default();
    assert_eq!(t.min_disk_pct, 10.0);
    assert_eq!(t.min_ram_mb, 500);
    assert_eq!(t.min_battery_pct, 15);
}

#[tokio::test]
async fn check_system_health_runs() {
    let engine = make_engine();
    // Should not panic regardless of system state
    engine.check_system_health().await;
}

// ── File Watching ──

#[tokio::test]
async fn watch_dir_adds_to_list() {
    let engine = make_engine();
    let tmp = tempfile::tempdir().unwrap();
    engine.watch_dir(tmp.path().to_path_buf(), "TestDir").await;
    let dirs = engine.get_watched_dirs().await;
    assert_eq!(dirs.len(), 1);
    assert_eq!(dirs[0].label, "TestDir");
    assert!(dirs[0].enabled);
}

#[tokio::test]
async fn watch_multiple_dirs() {
    let engine = make_engine();
    let tmp1 = tempfile::tempdir().unwrap();
    let tmp2 = tempfile::tempdir().unwrap();
    engine.watch_dir(tmp1.path().to_path_buf(), "Dir1").await;
    engine.watch_dir(tmp2.path().to_path_buf(), "Dir2").await;
    let dirs = engine.get_watched_dirs().await;
    assert_eq!(dirs.len(), 2);
}

// ── Tool Registration ──

#[test]
fn proactive_tools_registered() {
    let engine = make_engine();
    let reg = kria_core::tools::registry::build_registry_full(None, None, Some(engine));
    let proactive_tools = reg.list_by_category("proactive");
    assert!(proactive_tools.len() >= 6, "expected at least 6 proactive tools, got {}", proactive_tools.len());
}

#[test]
fn check_system_health_tool_exists() {
    let engine = make_engine();
    let reg = kria_core::tools::registry::build_registry_full(None, None, Some(engine));
    assert!(reg.get_def("check_system_health").is_some());
}

#[test]
fn get_alerts_tool_exists() {
    let engine = make_engine();
    let reg = kria_core::tools::registry::build_registry_full(None, None, Some(engine));
    assert!(reg.get_def("get_alerts").is_some());
}

#[test]
fn dismiss_alert_tool_exists() {
    let engine = make_engine();
    let reg = kria_core::tools::registry::build_registry_full(None, None, Some(engine));
    assert!(reg.get_def("dismiss_alert").is_some());
}

#[test]
fn watch_directory_tool_exists() {
    let engine = make_engine();
    let reg = kria_core::tools::registry::build_registry_full(None, None, Some(engine));
    assert!(reg.get_def("watch_directory").is_some());
}

#[test]
fn smart_suggest_tool_exists() {
    let engine = make_engine();
    let reg = kria_core::tools::registry::build_registry_full(None, None, Some(engine));
    assert!(reg.get_def("smart_suggest").is_some());
}

#[tokio::test]
async fn smart_suggest_returns_suggestions() {
    let engine = make_engine();
    let reg = kria_core::tools::registry::build_registry_full(None, None, Some(engine));
    let handler = reg.get_handler("smart_suggest").unwrap();
    let result = handler.execute(serde_json::json!({})).await;
    assert!(result.success);
    assert!(result.data["suggestions"].is_array());
}

#[tokio::test]
async fn check_system_health_tool_returns_data() {
    let engine = make_engine();
    let reg = kria_core::tools::registry::build_registry_full(None, None, Some(engine));
    let handler = reg.get_handler("check_system_health").unwrap();
    let result = handler.execute(serde_json::json!({})).await;
    assert!(result.success);
    assert!(result.data["thresholds"].is_object());
}

#[tokio::test]
async fn dismiss_alert_tool_requires_id() {
    let engine = make_engine();
    let reg = kria_core::tools::registry::build_registry_full(None, None, Some(engine));
    let handler = reg.get_handler("dismiss_alert").unwrap();
    let result = handler.execute(serde_json::json!({})).await;
    assert!(!result.success);
    assert!(result.error.unwrap().contains("required"));
}

#[tokio::test]
async fn watch_directory_tool_invalid_path() {
    let engine = make_engine();
    let reg = kria_core::tools::registry::build_registry_full(None, None, Some(engine));
    let handler = reg.get_handler("watch_directory").unwrap();
    let result = handler.execute(serde_json::json!({ "path": "/nonexistent/dir/xyz" })).await;
    assert!(!result.success);
}

#[test]
fn proactive_tools_lite_tier() {
    let engine = make_engine();
    let reg = kria_core::tools::registry::build_registry_full(None, None, Some(engine));
    let lite_tools = reg.list_for_tier("lite");
    let has_alerts = lite_tools.iter().any(|t| t.name == "get_alerts");
    assert!(has_alerts, "get_alerts should be available on lite tier");
}

#[test]
fn watch_directory_standard_tier() {
    let engine = make_engine();
    let reg = kria_core::tools::registry::build_registry_full(None, None, Some(engine));
    let lite_tools = reg.list_for_tier("lite");
    let has_watch = lite_tools.iter().any(|t| t.name == "watch_directory");
    assert!(!has_watch, "watch_directory should not be on lite tier");
}
