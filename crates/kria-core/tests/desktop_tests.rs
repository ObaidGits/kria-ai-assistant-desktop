/// Phase 10 — Desktop Automation & Contextual Awareness Tests
/// Tests desktop tools registration, URL validation, and window management tool structure.

// ── Desktop tools registration ──

#[test]
fn desktop_tools_registered() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    let desktop_tools = reg.list_by_category("desktop");
    assert!(
        desktop_tools.len() >= 8,
        "expected at least 8 desktop tools, got {}",
        desktop_tools.len()
    );
}

#[test]
fn get_active_window_registered() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    assert!(reg.get_def("get_active_window").is_some());
    assert!(reg.get_handler("get_active_window").is_some());
}

#[test]
fn list_windows_registered() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    assert!(reg.get_def("list_windows").is_some());
}

#[test]
fn move_window_registered() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    let def = reg.get_def("move_window").unwrap();
    assert_eq!(def.category, "desktop");
    assert_eq!(def.parameters.len(), 3); // title, x, y
}

#[test]
fn resize_window_registered() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    let def = reg.get_def("resize_window").unwrap();
    assert_eq!(def.parameters.len(), 3); // title, width, height
}

#[test]
fn maximize_minimize_registered() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    assert!(reg.get_def("maximize_window").is_some());
    assert!(reg.get_def("minimize_window").is_some());
}

#[test]
fn tile_windows_registered() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    let def = reg.get_def("tile_windows").unwrap();
    assert_eq!(def.parameters.len(), 2); // windows, layout
}

#[test]
fn open_url_registered() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    let def = reg.get_def("open_url").unwrap();
    assert_eq!(def.min_tier, "lite"); // URL opening available on all tiers
}

// ── Open URL validation ──

#[tokio::test]
async fn open_url_rejects_empty() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    let handler = reg.get_handler("open_url").unwrap();
    let result = handler.execute(serde_json::json!({"url": ""})).await;
    assert!(!result.success);
}

#[tokio::test]
async fn open_url_rejects_non_http() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    let handler = reg.get_handler("open_url").unwrap();
    let result = handler
        .execute(serde_json::json!({"url": "file:///etc/passwd"}))
        .await;
    assert!(!result.success);
}

#[tokio::test]
async fn open_url_rejects_javascript() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    let handler = reg.get_handler("open_url").unwrap();
    let result = handler
        .execute(serde_json::json!({"url": "javascript:alert(1)"}))
        .await;
    assert!(!result.success);
}

// ── Tile windows needs at least 2 ──

#[tokio::test]
async fn tile_windows_requires_two() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    let handler = reg.get_handler("tile_windows").unwrap();
    let result = handler
        .execute(serde_json::json!({"windows": ["one"], "layout": "side-by-side"}))
        .await;
    assert!(!result.success);
    assert!(result.error.unwrap_or_default().contains("at least 2"));
}

// ── Desktop tools have correct tiers ──

#[test]
fn desktop_tools_standard_tier() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    // Most window tools require at least standard tier
    assert_eq!(
        reg.get_def("get_active_window").unwrap().min_tier,
        "standard"
    );
    assert_eq!(reg.get_def("list_windows").unwrap().min_tier, "standard");
    assert_eq!(reg.get_def("move_window").unwrap().min_tier, "standard");
    // open_url is available on all tiers
    assert_eq!(reg.get_def("open_url").unwrap().min_tier, "lite");
}

#[test]
fn desktop_tools_not_visible_on_lite() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    let lite_tools = reg.list_for_tier("lite");
    let lite_names: Vec<&str> = lite_tools.iter().map(|d| d.name.as_str()).collect();
    // get_active_window should NOT be in lite tier
    assert!(!lite_names.contains(&"get_active_window"));
    // open_url SHOULD be in lite tier
    assert!(lite_names.contains(&"open_url"));
}

// ── Total tool count increased ──

#[test]
fn total_tools_include_desktop() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    // We had 60+ tools before, now we should have 8 more
    assert!(
        reg.len() >= 68,
        "expected at least 68 tools, got {}",
        reg.len()
    );
}
