use kria_core::llm::model_router::ModelRouter;
/// Phase 4 — Vision & Multimodal tests
///
/// Validates: ImageAttachment, ChatMessage multimodal, vision routing,
/// vision tools registration, image preprocessing, OCR tool structure.
use kria_core::llm::{ChatMessage, ImageAttachment};
use kria_core::preprocessing::image::ImageProcessor;
use kria_core::safety::RiskLevel;
use kria_core::tools::registry;
use std::path::Path;

// ── 4.1: ImageAttachment and ChatMessage extension ──────────

#[test]
fn phase4_image_attachment_creation() {
    let img = ImageAttachment {
        data: "iVBORw0KGgo=".into(),
        mime_type: "image/png".into(),
    };
    assert_eq!(img.mime_type, "image/png");
    assert!(!img.data.is_empty());
}

#[test]
fn phase4_chat_message_without_images() {
    let msg = ChatMessage {
        role: "user".into(),
        content: "Hello".into(),
        name: None,
        images: None,
    };
    assert!(!msg.has_images());
    // Multimodal content should return simple string
    let content = msg.to_multimodal_content();
    assert_eq!(content.as_str().unwrap(), "Hello");
}

#[test]
fn phase4_chat_message_with_images() {
    let msg = ChatMessage {
        role: "user".into(),
        content: "What's in this image?".into(),
        name: None,
        images: Some(vec![ImageAttachment {
            data: "abc123base64data".into(),
            mime_type: "image/jpeg".into(),
        }]),
    };
    assert!(msg.has_images());
}

#[test]
fn phase4_multimodal_content_format() {
    let msg = ChatMessage {
        role: "user".into(),
        content: "describe this".into(),
        name: None,
        images: Some(vec![ImageAttachment {
            data: "TESTDATA".into(),
            mime_type: "image/png".into(),
        }]),
    };
    let content = msg.to_multimodal_content();
    let parts = content.as_array().expect("should be array");
    assert_eq!(parts.len(), 2); // text + image
    assert_eq!(parts[0]["type"], "text");
    assert_eq!(parts[0]["text"], "describe this");
    assert_eq!(parts[1]["type"], "image_url");
    let url = parts[1]["image_url"]["url"].as_str().unwrap();
    assert!(url.starts_with("data:image/png;base64,"));
    assert!(url.contains("TESTDATA"));
}

#[test]
fn phase4_multimodal_multiple_images() {
    let msg = ChatMessage {
        role: "user".into(),
        content: "compare these".into(),
        name: None,
        images: Some(vec![
            ImageAttachment {
                data: "IMG1".into(),
                mime_type: "image/png".into(),
            },
            ImageAttachment {
                data: "IMG2".into(),
                mime_type: "image/jpeg".into(),
            },
        ]),
    };
    let content = msg.to_multimodal_content();
    let parts = content.as_array().unwrap();
    assert_eq!(parts.len(), 3); // text + 2 images
}

#[test]
fn phase4_empty_content_with_image() {
    let msg = ChatMessage {
        role: "user".into(),
        content: "".into(),
        name: None,
        images: Some(vec![ImageAttachment {
            data: "DATA".into(),
            mime_type: "image/png".into(),
        }]),
    };
    let content = msg.to_multimodal_content();
    let parts = content.as_array().unwrap();
    // Only image, no text part (content is empty)
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0]["type"], "image_url");
}

#[test]
fn phase4_empty_images_vec() {
    let msg = ChatMessage {
        role: "user".into(),
        content: "hello".into(),
        name: None,
        images: Some(vec![]),
    };
    assert!(!msg.has_images());
    // Empty images vec should return plain string
    let content = msg.to_multimodal_content();
    assert_eq!(content.as_str().unwrap(), "hello");
}

#[test]
fn phase4_image_attachment_serialization() {
    let msg = ChatMessage {
        role: "user".into(),
        content: "test".into(),
        name: None,
        images: Some(vec![ImageAttachment {
            data: "b64data".into(),
            mime_type: "image/webp".into(),
        }]),
    };
    let json = serde_json::to_value(&msg).unwrap();
    assert!(json["images"].is_array());
    assert_eq!(json["images"][0]["mime_type"], "image/webp");
}

#[test]
fn phase4_no_images_serialization_skip() {
    let msg = ChatMessage {
        role: "user".into(),
        content: "test".into(),
        name: None,
        images: None,
    };
    let json = serde_json::to_value(&msg).unwrap();
    // images field should be skipped when None
    assert!(json.get("images").is_none());
}

// ── 4.2: Vision tools registration ──────────────────────────

#[test]
fn phase4_vision_tools_registered() {
    let reg = registry::build_default_registry();
    assert!(
        reg.get_def("ocr_image").is_some(),
        "ocr_image tool not found"
    );
    assert!(
        reg.get_def("analyze_image").is_some(),
        "analyze_image tool not found"
    );
    assert!(
        reg.get_def("screenshot_analyze").is_some(),
        "screenshot_analyze tool not found"
    );
}

#[test]
fn phase4_vision_tools_category() {
    let reg = registry::build_default_registry();
    for name in &["ocr_image", "analyze_image", "screenshot_analyze"] {
        let def = reg.get_def(name).unwrap();
        assert_eq!(
            def.category, "vision",
            "{name} should be in vision category"
        );
        assert_eq!(
            def.default_tier,
            RiskLevel::Green,
            "{name} should be GREEN tier"
        );
    }
}

#[test]
fn phase4_ocr_tool_requires_path() {
    let reg = registry::build_default_registry();
    let def = reg.get_def("ocr_image").unwrap();
    assert!(def
        .parameters
        .iter()
        .any(|p| p.name == "path" && p.required));
}

#[test]
fn phase4_analyze_image_params() {
    let reg = registry::build_default_registry();
    let def = reg.get_def("analyze_image").unwrap();
    assert!(def
        .parameters
        .iter()
        .any(|p| p.name == "path" && p.required));
    assert!(def
        .parameters
        .iter()
        .any(|p| p.name == "operations" && !p.required));
}

// ── 4.3: ImageProcessor tests ───────────────────────────────

#[test]
fn phase4_image_processor_info() {
    // Create a valid 1x1 PNG using the image crate
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.png");
    let img = image::ImageBuffer::from_fn(1, 1, |_, _| image::Rgb([255u8, 255, 255]));
    img.save(&path).unwrap();
    let info = ImageProcessor::info(&path).unwrap();
    assert_eq!(info.width, 1);
    assert_eq!(info.height, 1);
    assert_eq!(info.format, "png");
    assert!(info.size_bytes > 0);
}

#[test]
fn phase4_image_processor_to_base64() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.txt");
    std::fs::write(&path, b"Hello World").unwrap();
    let b64 = ImageProcessor::to_base64(&path).unwrap();
    // "Hello World" in base64 is "SGVsbG8gV29ybGQ="
    assert_eq!(b64, "SGVsbG8gV29ybGQ=");
}

#[test]
fn phase4_image_processor_info_nonexistent() {
    let result = ImageProcessor::info(Path::new("/tmp/nonexistent_image_4321.png"));
    assert!(result.is_err());
}

#[test]
fn phase4_image_processor_thumbnail() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("thumb_test.png");
    // Create a small valid image with the image crate
    let img = image::ImageBuffer::from_fn(100, 100, |_, _| image::Rgb([255u8, 0, 0]));
    img.save(&path).unwrap();

    let thumb = ImageProcessor::thumbnail(&path, 50).unwrap();
    assert!(!thumb.is_empty());
    // Verify it's valid PNG data
    assert_eq!(&thumb[..4], &[0x89, 0x50, 0x4E, 0x47]);
}

// ── 4.4: Vision routing (ModelRouter) ───────────────────────

#[test]
fn phase4_model_router_no_vision() {
    // Default config has local_api_url set, so vision is available via
    // the local backend fallback (server may support vision).
    let config = kria_core::config::KriaConfig::default();
    let router = ModelRouter::from_config(&config);
    assert!(
        router.has_vision(),
        "local backend should serve as vision fallback"
    );

    // Only when local_api_url is empty should vision be unavailable
    let mut no_local = kria_core::config::KriaConfig::default();
    no_local.llm.local_api_url = String::new();
    let router2 = ModelRouter::from_config(&no_local);
    assert!(!router2.has_vision(), "no backend means no vision");
}

#[tokio::test]
async fn phase4_model_router_route_vision_fallback() {
    let config = kria_core::config::KriaConfig::default();
    let router = ModelRouter::from_config(&config);
    // Default config has local_api_url set, so route_vision falls back to local
    let result = router.route_vision().await;
    // Vision backend should fall back to local text model
    assert!(result.is_some(), "should fall back to local backend");
}

// ── 4.5: OCR tool execution (file not found) ────────────────

#[tokio::test]
async fn phase4_ocr_tool_missing_path() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("ocr_image").unwrap();
    let result = handler.execute(serde_json::json!({})).await;
    assert!(!result.success);
    assert!(result.error.as_ref().unwrap().contains("missing"));
}

#[tokio::test]
async fn phase4_ocr_tool_file_not_found() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("ocr_image").unwrap();
    let result = handler
        .execute(serde_json::json!({"path": "/tmp/nonexistent_ocr_test.png"}))
        .await;
    assert!(!result.success);
    assert!(result.error.as_ref().unwrap().contains("not found"));
}

#[tokio::test]
async fn phase4_analyze_image_missing_path() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("analyze_image").unwrap();
    let result = handler.execute(serde_json::json!({})).await;
    assert!(!result.success);
    assert!(result.error.as_ref().unwrap().contains("missing"));
}

#[tokio::test]
async fn phase4_analyze_image_file_not_found() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("analyze_image").unwrap();
    let result = handler
        .execute(serde_json::json!({"path": "/tmp/no_such_img.png"}))
        .await;
    assert!(!result.success);
    assert!(result.error.as_ref().unwrap().contains("not found"));
}

#[tokio::test]
async fn phase4_analyze_image_accepts_file_uri_path() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("analyze_image").unwrap();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("uri_test.png");
    let img = image::ImageBuffer::from_fn(2, 2, |_, _| image::Rgb([0u8, 255, 0]));
    img.save(&path).unwrap();

    let uri = format!("file://{}", path.display());
    let result = handler.execute(serde_json::json!({"path": uri})).await;

    assert!(result.success, "expected file:// path to resolve");
    let data = result.data;
    assert!(data.get("metadata").is_some());
}

#[tokio::test]
async fn phase4_analyze_image_accepts_markdown_wrapped_path() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("analyze_image").unwrap();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wrapped_path_test.png");
    let img = image::ImageBuffer::from_fn(2, 2, |_, _| image::Rgb([0u8, 0, 255]));
    img.save(&path).unwrap();

    let wrapped = format!("[image: {}].", path.display());
    let result = handler.execute(serde_json::json!({"path": wrapped})).await;

    assert!(result.success, "expected [image: ...]. path to resolve");
    let data = result.data;
    assert!(data.get("metadata").is_some());
}

#[tokio::test]
async fn phase4_analyze_image_accepts_urlencoded_file_uri() {
    let reg = registry::build_default_registry();
    let handler = reg.get_handler("analyze_image").unwrap();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("uri test image.png");
    let img = image::ImageBuffer::from_fn(2, 2, |_, _| image::Rgb([255u8, 255, 0]));
    img.save(&path).unwrap();

    let encoded_path = path.to_string_lossy().replace(' ', "%20");
    let uri = format!("file://{}", encoded_path);
    let result = handler.execute(serde_json::json!({"path": uri})).await;

    assert!(
        result.success,
        "expected URL-encoded file:// path to resolve"
    );
    let data = result.data;
    assert!(data.get("metadata").is_some());
}

// ── 4.6: Integration — tool count includes vision tools ─────

#[test]
fn phase4_total_tool_count() {
    let reg = registry::build_default_registry();
    // Previous phases had ~60+ tools; Phase 4 adds 3 vision tools
    let count = reg.len();
    assert!(
        count >= 63,
        "expected at least 63 tools with vision, got {count}"
    );
}

#[test]
fn phase4_vision_tool_description_quality() {
    let reg = registry::build_default_registry();
    for name in &["ocr_image", "analyze_image", "screenshot_analyze"] {
        let def = reg.get_def(name).unwrap();
        assert!(def.description.len() > 20, "{name} description too short");
        assert!(!def.description.is_empty());
    }
}
