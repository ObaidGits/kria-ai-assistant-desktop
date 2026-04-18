/// Package tools regression tests.
///
/// Guards against argument-shape mismatches during install flows.

#[test]
fn search_package_schema_keeps_query_and_alias_name() {
    use kria_core::tools::registry::build_default_registry;

    let reg = build_default_registry();
    let def = reg
        .get_def("search_package")
        .expect("search_package should be registered");

    let query_param = def
        .parameters
        .iter()
        .find(|p| p.name == "query")
        .expect("query param should exist");
    assert!(
        query_param.required,
        "query should remain required for canonical calls"
    );

    let alias_param = def
        .parameters
        .iter()
        .find(|p| p.name == "name")
        .expect("name alias should exist");
    assert!(!alias_param.required, "name alias should be optional");
}

#[tokio::test]
async fn search_package_accepts_name_alias_when_query_missing() {
    use kria_core::tools::registry::build_default_registry;

    let reg = build_default_registry();
    let handler = reg
        .get_handler("search_package")
        .expect("search_package handler should exist");

    // Use an invalid source to keep the test deterministic and avoid shelling out.
    // The important assertion is that we no longer fail with "query parameter is required"
    // when only the `name` alias is provided.
    let result = handler
        .execute(serde_json::json!({
            "name": "chromium",
            "source": "not-a-real-source"
        }))
        .await;

    assert!(!result.success, "invalid source should still fail");
    let err = result.error.unwrap_or_default();
    assert!(
        err.contains("unknown package source"),
        "expected source validation error, got: {err}"
    );
    assert!(
        !err.contains("query parameter is required"),
        "name alias should satisfy query input, got: {err}"
    );
}

#[tokio::test]
async fn search_package_requires_query_or_name() {
    use kria_core::tools::registry::build_default_registry;

    let reg = build_default_registry();
    let handler = reg
        .get_handler("search_package")
        .expect("search_package handler should exist");

    let result = handler
        .execute(serde_json::json!({
            "source": "apt"
        }))
        .await;

    assert!(!result.success);
    let err = result.error.unwrap_or_default();
    assert!(
        err.contains("query parameter is required"),
        "expected missing query/name error, got: {err}"
    );
}
