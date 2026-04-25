//! Integration test — Image generation via cloud (Pollinations.ai) fallback.
//!
//! Runs with: `cargo test -p kria-core --test image_generation_test -- --nocapture --test-threads=1`
//!
//! Requires internet access. Skipped automatically when
//! `KRIA_SKIP_NETWORK_TESTS=1` is set (CI environments without outbound HTTP).

use kria_core::config::ImageGenerationConfig;
use kria_core::image::{ImageOrchestrator, ImageRequest};
use std::path::PathBuf;

fn skip_if_no_network() -> bool {
    std::env::var("KRIA_SKIP_NETWORK_TESTS")
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// Build a minimal config with cloud fallback forced on and ComfyUI disabled.
fn cloud_only_config(out_dir: &PathBuf) -> ImageGenerationConfig {
    ImageGenerationConfig {
        enabled: true,
        tier_override: "c".to_string(), // Force Tier C (cloud)
        cloud_fallback: "always".to_string(),
        pollinations_base_url: "https://image.pollinations.ai".to_string(),
        output_dir: out_dir.to_string_lossy().to_string(),
        max_concurrent_jobs: 1,
        max_queued_swap_jobs: 1,
        ..ImageGenerationConfig::default()
    }
}

fn test_out_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent().unwrap()  // crates/
        .parent().unwrap()  // workspace root
        .join("target/test-sandbox/image_gen_test");
    std::fs::create_dir_all(&dir).expect("create test output dir");
    dir
}

#[tokio::test]
async fn test_cloud_image_generation_saves_file() {
    if skip_if_no_network() {
        println!("Skipping image generation test (KRIA_SKIP_NETWORK_TESTS=1)");
        return;
    }

    let out_dir = test_out_dir();
    let cfg = cloud_only_config(&out_dir);
    let orchestrator = ImageOrchestrator::new(cfg, &out_dir);

    let req = ImageRequest {
        prompt: "a serene Japanese garden at dawn, koi pond, cherry blossoms".to_string(),
        style: None,      // auto-classify → photorealistic
        aspect: Default::default(),
        count: 1,
        seed: Some(42),
        force_cloud: true,
        quality: None,
        negative: None,
        enhance: None,
    };

    println!("Generating photorealistic image via Pollinations.ai…");
    let result = orchestrator.generate(req, None, None).await;

    match result {
        Ok(img_result) => {
            assert!(
                !img_result.images.is_empty(),
                "Expected at least one generated image"
            );
            let img = &img_result.images[0];
            println!("Generated image: {:?}", img.path);
            println!("  sha256:     {}", img.sha256);
            println!("  size:       {}×{}", img.width, img.height);
            println!("  style:      {}", img.style);
            println!("  provenance: {}", img.provenance);            println!("  seed:       {}", img.seed);
            println!("  quality:    {}", img.quality);
            println!("  steps:      {}", img.steps);
            println!("  sampler:    {}", img.sampler);
            println!("  enhance:    {}", img.enhance_mode);            println!("  elapsed:    {}ms", img_result.elapsed_ms);
            println!("  tier:       {}", img_result.tier_used);

            assert!(img.path.exists(), "Output file should exist on disk: {:?}", img.path);
            let bytes = std::fs::read(&img.path).expect("read output file");
            assert!(!bytes.is_empty(), "Output file should not be empty");

            // Verify PNG or JPEG magic bytes (Pollinations may return JPEG).
            let is_png = bytes.len() >= 8 && bytes.starts_with(b"\x89PNG\r\n\x1a\n");
            let is_jpeg = bytes.len() >= 3 && bytes.starts_with(b"\xff\xd8\xff");
            assert!(
                is_png || is_jpeg,
                "Output should be a valid PNG or JPEG (got {} bytes, prefix: {:?})",
                bytes.len(),
                &bytes[..bytes.len().min(8)]
            );

            println!("✅ Image saved to: {} ({} bytes)", img.path.display(), bytes.len());
        }
        Err(e) => {
            panic!("Image generation failed: {e}");
        }
    }
}

#[tokio::test]
async fn test_cloud_image_generation_anime_style() {
    if skip_if_no_network() {
        return;
    }

    let out_dir = test_out_dir();
    let cfg = cloud_only_config(&out_dir);
    let orchestrator = ImageOrchestrator::new(cfg, &out_dir);

    let req = ImageRequest {
        prompt: "anime girl with purple hair standing in a cyberpunk city at night".to_string(),
        style: Some(kria_core::image::styles::ImageStyle::Anime),
        aspect: Default::default(),
        count: 1,
        seed: Some(1337),
        force_cloud: true,
        quality: None,
        negative: None,
        enhance: None,
    };

    println!("Generating anime-style image via Pollinations.ai…");
    let result = orchestrator.generate(req, None, None).await;

    assert!(result.is_ok(), "Anime image generation failed: {:?}", result.err());
    let img_result = result.unwrap();
    let img = &img_result.images[0];

    assert!(img.path.exists(), "Anime image file should exist: {:?}", img.path);
    let bytes = std::fs::read(&img.path).expect("read anime output file");
    let is_png = bytes.len() >= 8 && bytes.starts_with(b"\x89PNG\r\n\x1a\n");
    let is_jpeg = bytes.len() >= 3 && bytes.starts_with(b"\xff\xd8\xff");
    assert!(is_png || is_jpeg, "Anime output should be a valid PNG or JPEG");
    println!("✅ Anime image saved to: {} ({} bytes)", img.path.display(), bytes.len());
}

/// Test that the new GeneratedImage metadata fields are populated on cloud path.
#[tokio::test]
async fn test_cloud_image_metadata_fields() {
    if skip_if_no_network() {
        return;
    }

    let out_dir = test_out_dir();
    let cfg = cloud_only_config(&out_dir);
    let orchestrator = ImageOrchestrator::new(cfg, &out_dir);

    let req = ImageRequest {
        prompt: "a futuristic city skyline at sunset".to_string(),
        style: None,
        aspect: Default::default(),
        count: 1,
        seed: None, // should be randomized by orchestrator
        force_cloud: true,
        quality: None,
        negative: None,
        enhance: Some(true),
    };

    let result = orchestrator.generate(req, None, None).await
        .expect("cloud generation should succeed");

    let img = &result.images[0];

    // Seed must always be non-zero (randomized).
    assert_ne!(img.seed, 0, "seed should be randomized, not 0");

    // Quality field must be a known profile string.
    assert!(
        matches!(img.quality.as_str(), "fast" | "balanced" | "high"),
        "quality field should be fast/balanced/high, got: {}",
        img.quality
    );

    // Steps must be a reasonable number (Schnell: 4, Balanced: 4–8).
    assert!(img.steps >= 1 && img.steps <= 50, "steps out of range: {}", img.steps);

    // cfg_scale for Schnell must be exactly 1.0.
    assert!(
        (img.cfg_scale - 1.0).abs() < 0.01,
        "Schnell cfg_scale must be 1.0 on cloud path, got: {}",
        img.cfg_scale
    );

    // final_prompt must contain the original prompt.
    assert!(
        img.final_prompt.contains("futuristic city"),
        "final_prompt should contain original text, got: {}",
        img.final_prompt
    );

    println!(
        "✅ metadata fields: seed={} quality={} steps={} sampler={} cfg={} enhance={}",
        img.seed, img.quality, img.steps, img.sampler, img.cfg_scale, img.enhance_mode
    );
}

/// Test that count=2 produces two images with different seeds (same-seed bug fix).
#[tokio::test]
async fn test_cloud_count_two_unique_seeds() {
    if skip_if_no_network() {
        return;
    }

    let out_dir = test_out_dir();
    let cfg = cloud_only_config(&out_dir);
    let orchestrator = ImageOrchestrator::new(cfg, &out_dir);

    let req = ImageRequest {
        prompt: "two cats sitting on a rooftop".to_string(),
        style: None,
        aspect: Default::default(),
        count: 2,
        seed: None,
        force_cloud: true,
        quality: None,
        negative: None,
        enhance: None,
    };

    println!("Generating 2 images (testing seed uniqueness)…");
    let result = orchestrator.generate(req, None, None).await
        .expect("count=2 generation should succeed");

    assert_eq!(result.images.len(), 2, "Should get exactly 2 images");

    let seed0 = result.images[0].seed;
    let seed1 = result.images[1].seed;

    assert_ne!(seed0, seed1, "Image seeds must differ (same-seed bug fix): seed0={seed0} seed1={seed1}");
    assert_eq!(seed1, seed0.wrapping_add(1), "seed1 should be seed0 + 1, got seed0={seed0} seed1={seed1}");

    // Both files must exist.
    for (i, img) in result.images.iter().enumerate() {
        assert!(img.path.exists(), "image[{i}] path should exist: {:?}", img.path);
        let bytes = std::fs::read(&img.path).expect("read file");
        assert!(!bytes.is_empty(), "image[{i}] should not be empty");
        println!("✅ image[{i}]: seed={} path={}", img.seed, img.path.display());
    }
}

/// Test that prompt enhancement is applied and final_prompt differs from raw input.
#[tokio::test]
async fn test_prompt_enhancement_applied() {
    if skip_if_no_network() {
        return;
    }

    let out_dir = test_out_dir();
    let cfg = cloud_only_config(&out_dir);
    let orchestrator = ImageOrchestrator::new(cfg, &out_dir);

    // Short, bare prompt — should get template enrichment.
    let req = ImageRequest {
        prompt: "cat".to_string(),
        style: Some(kria_core::image::styles::ImageStyle::Photorealistic),
        aspect: Default::default(),
        count: 1,
        seed: Some(99),
        force_cloud: true,
        quality: None,
        negative: None,
        enhance: Some(true),
    };

    let result = orchestrator.generate(req, None, None).await
        .expect("enhanced generation should succeed");

    let img = &result.images[0];

    // Cloud path prefixes the style name, enhancement adds keywords.
    // The final_prompt should be longer than "cat".
    println!("raw='cat'  final_prompt='{}'", img.final_prompt);
    assert!(
        img.final_prompt.len() > 5,
        "final_prompt should be enriched beyond bare 'cat', got: '{}'",
        img.final_prompt
    );

    println!("✅ enhance_mode={} final_prompt_len={}", img.enhance_mode, img.final_prompt.len());
}
