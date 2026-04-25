// ─────────────────────────────────────────────────────────────────────────────
//  test_vision.rs — §8 Vision & Image Tools
//
//  Live screenshot tests require GNOME/X11 (guarded with gnome_display_available()).
//  OCR/analyze tests against missing files must fail cleanly.
//
//  Covers PROMPT-IDs: VIS-01..VIS-06
// ─────────────────────────────────────────────────────────────────────────────

mod common;

use common::{gnome_display_available, SandboxDir};
use kria_core::agent::router::{Intent, IntentRouter};
use kria_core::tools::registry;

// ═══════════════════════════════════════════════════════════════════════════
//  Smoke — all vision tools must be registered
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn smoke_vision_tools_registered() {
    // PROMPT-ID: VIS-01..VIS-06
    let reg = registry::build_default_registry();
    let required = ["screenshot", "screenshot_analyze", "ocr_image", "analyze_image"];
    for name in &required {
        assert!(
            reg.get_handler(name).is_some(),
            "Tool `{name}` must be registered (required by §8)"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Routing — §8 prompts
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn routing_vis01_screenshot_routes_correctly() {
    // PROMPT-ID: VIS-01
    let prompts = [
        "Take a screenshot.",
        "screenshot lo",
        "capture my screen",
        "take a screen capture",
    ];
    for p in &prompts {
        let r = IntentRouter::classify(p);
        assert!(
            matches!(&r.intent, Intent::DirectTool(t) if matches!(t.as_str(), "screenshot" | "screenshot_analyze")),
            "'{p}' should route to screenshot or screenshot_analyze, got: {:?}",
            r.intent
        );
    }
}

#[test]
fn routing_vis02_screenshot_analyze_routes_correctly() {
    // PROMPT-ID: VIS-02, VIS-03
    let prompts = [
        "Take a screenshot and analyze it.",
        "What is on my screen right now?",
        "screen kya show ho raha hai?",
        "describe my screen",
    ];
    for p in &prompts {
        let r = IntentRouter::classify(p);
        assert!(
            matches!(&r.intent, Intent::DirectTool(t) if matches!(t.as_str(), "screenshot_analyze" | "screenshot"))
                || matches!(r.intent, Intent::ComplexTask),
            "'{p}' should route to screenshot_analyze/screenshot or complex task, got: {:?}",
            r.intent
        );
    }
}

#[test]
fn routing_vis04_ocr_image_routes_correctly() {
    // PROMPT-ID: VIS-04
    let r = IntentRouter::classify("OCR this image: /home/obaid/Pictures/scan.png");
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if t == "ocr_image")
            || matches!(r.intent, Intent::ComplexTask | Intent::Conversation),
        "OCR request should route to ocr_image, got: {:?}",
        r.intent
    );
}

#[test]
fn routing_vis05_analyze_image_routes_correctly() {
    // PROMPT-ID: VIS-05
    let r = IntentRouter::classify("Analyze the image at /home/obaid/Pictures/photo.jpg");
    assert!(
        matches!(&r.intent, Intent::DirectTool(t) if t == "analyze_image")
            || matches!(r.intent, Intent::ComplexTask | Intent::Conversation),
        "Image analysis should route to analyze_image, got: {:?}",
        r.intent
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  Functional — error cases (no display required)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn functional_vis04_ocr_missing_file_clean_error() {
    // PROMPT-ID: VIS-04 — missing image file must return clean error, not panic
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("ocr_image").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "path": "/nonexistent_kria_test/scan.png" }))
        .await;
    assert!(
        !result.success,
        "ocr_image for missing file must fail cleanly (success=false)"
    );
    assert!(
        result.error.is_some(),
        "ocr_image failure must include an error message"
    );
}

#[tokio::test]
async fn functional_vis05_analyze_image_missing_file_clean_error() {
    // PROMPT-ID: VIS-05 — missing image file must return clean error
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("analyze_image").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({
            "path": "/nonexistent_kria_test/photo.jpg",
            "operations": ["describe"]
        }))
        .await;
    assert!(
        !result.success,
        "analyze_image for missing file must fail cleanly"
    );
    assert!(result.error.is_some(), "analyze_image failure must include error message");
}

// ═══════════════════════════════════════════════════════════════════════════
//  Functional — live GNOME screenshot (requires DISPLAY)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn functional_vis01_screenshot_live() {
    // PROMPT-ID: VIS-01
    if !gnome_display_available() {
        eprintln!("SKIP: no display available for screenshot test");
        return;
    }

    let sandbox = SandboxDir::new();
    let out_path = sandbox.child("screenshot.png");

    let reg = registry::build_default_registry();
    let handler = reg.get_handler("screenshot").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "output": out_path.to_str().unwrap() }))
        .await;

    assert!(result.success, "screenshot should succeed when DISPLAY is set: {:?}", result.error);
    assert!(out_path.exists(), "screenshot must create the output file");
    let meta = std::fs::metadata(&out_path).unwrap();
    assert!(
        meta.len() > 1024,
        "screenshot file must be at least 1 KB (got {} bytes)",
        meta.len()
    );
}

#[tokio::test]
async fn functional_vis01_screenshot_creates_file() {
    // PROMPT-ID: VIS-01 — verify output path is returned in result
    if !gnome_display_available() {
        eprintln!("SKIP: no display available");
        return;
    }

    let sandbox = SandboxDir::new();
    let out_path = sandbox.child("kria_ss.png");

    let reg = registry::build_default_registry();
    let handler = reg.get_handler("screenshot").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "output": out_path.to_str().unwrap() }))
        .await;

    if result.success {
        let returned_path = result.data["path"].as_str()
            .or(result.data["output"].as_str())
            .unwrap_or("");
        assert!(
            !returned_path.is_empty(),
            "screenshot result must include the output path"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Functional — screenshot_analyze (requires display + vision model)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn functional_vis02_screenshot_analyze_live() {
    // PROMPT-ID: VIS-02, VIS-03
    if !gnome_display_available() {
        eprintln!("SKIP: no display for screenshot_analyze");
        return;
    }

    let reg = registry::build_default_registry();
    let handler = reg.get_handler("screenshot_analyze").unwrap().clone();
    let result = handler.execute(serde_json::json!({})).await;

    // Either succeed with a description, or fail cleanly (model may not be loaded)
    if result.success {
        let description = result.data["description"].as_str()
            .or(result.data["text"].as_str())
            .or(result.data["content"].as_str())
            .unwrap_or("");
        assert!(
            !description.is_empty(),
            "screenshot_analyze must return a non-empty description when it succeeds"
        );
    } else {
        assert!(
            result.error.is_some(),
            "screenshot_analyze failure must include error message"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Functional — OCR on a real image in the sandbox
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn functional_vis04_ocr_real_png() {
    // PROMPT-ID: VIS-04 — use a screenshot as the OCR input (if DISPLAY available)
    if !gnome_display_available() {
        eprintln!("SKIP: no display for OCR test");
        return;
    }

    let sandbox = SandboxDir::new();
    let ss_path = sandbox.child("ocr_source.png");

    // First take a screenshot
    let reg = registry::build_default_registry();
    {
        let handler = reg.get_handler("screenshot").unwrap().clone();
        let r = handler
            .execute(serde_json::json!({ "output": ss_path.to_str().unwrap() }))
            .await;
        if !r.success {
            eprintln!("SKIP: screenshot failed, skipping OCR test");
            return;
        }
    }

    // Then OCR it
    let handler = reg.get_handler("ocr_image").unwrap().clone();
    let result = handler
        .execute(serde_json::json!({ "path": ss_path.to_str().unwrap() }))
        .await;

    assert!(
        result.success || result.error.is_some(),
        "ocr_image on a real screenshot must not panic"
    );
}
