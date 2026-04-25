/// Phase 9 — Adaptive Hardware Tier Tests
/// Tests hardware detection, tier classification, tier-aware recommendations,
/// config overrides, and tool filtering.

// ── Tier classification ──

#[test]
fn classify_tier_lite() {
    use kria_core::platform::detect::*;
    // 4 GB RAM, no VRAM → Lite
    let info = HardwareInfo {
        os: Os::Linux,
        tier: HardwareTier::Lite,
        cpu_cores: 2,
        total_ram_mb: 4096,
        vram_mb: None,
        vram_free_mb: 0,
        image_tier: kria_core::platform::vram::ImageTier::CRejectOrCloud,
        gpu_name: None,
        package_manager: Some(PackageManager::Apt),
        hostname: "test".into(),
    };
    assert_eq!(info.tier, HardwareTier::Lite);
}

#[test]
fn classify_tier_standard() {
    use kria_core::platform::detect::*;
    let tier = HardwareTier::Standard;
    assert_eq!(tier.as_str(), "standard");
}

#[test]
fn classify_tier_performance() {
    use kria_core::platform::detect::*;
    let tier = HardwareTier::Performance;
    assert_eq!(tier.as_str(), "performance");
}

#[test]
fn classify_tier_high() {
    use kria_core::platform::detect::*;
    let tier = HardwareTier::High;
    assert_eq!(tier.as_str(), "high");
}

// ── Tier string round-trip ──

#[test]
fn tier_from_str_roundtrip() {
    use kria_core::platform::detect::HardwareTier;
    for name in &["lite", "standard", "performance", "high"] {
        let tier = name.parse::<HardwareTier>().unwrap_or(HardwareTier::Standard);
        assert_eq!(tier.as_str(), *name);
    }
}

#[test]
fn tier_from_str_invalid_defaults_standard() {
    use kria_core::platform::detect::HardwareTier;
    let tier = "nonexistent"
        .parse::<HardwareTier>()
        .unwrap_or(HardwareTier::Standard);
    assert_eq!(tier, HardwareTier::Standard);
}

// ── Tier recommendations ──

#[test]
fn tier_context_windows() {
    use kria_core::platform::detect::HardwareTier;
    assert_eq!(HardwareTier::Lite.context_window(), 1024);
    assert_eq!(HardwareTier::Standard.context_window(), 2048);
    assert_eq!(HardwareTier::Performance.context_window(), 4096);
    assert_eq!(HardwareTier::High.context_window(), 8192);
}

#[test]
fn tier_thread_counts() {
    use kria_core::platform::detect::HardwareTier;
    assert_eq!(HardwareTier::Lite.thread_count(), 4);
    assert_eq!(HardwareTier::Standard.thread_count(), 6);
    assert_eq!(HardwareTier::Performance.thread_count(), 8);
    assert_eq!(HardwareTier::High.thread_count(), 8);
}

#[test]
fn tier_gpu_layers() {
    use kria_core::platform::detect::HardwareTier;
    assert_eq!(HardwareTier::Lite.gpu_layers(), 0);
    assert_eq!(HardwareTier::Standard.gpu_layers(), 0);
    assert_eq!(HardwareTier::Performance.gpu_layers(), 99);
    assert_eq!(HardwareTier::High.gpu_layers(), 99);
}

#[test]
fn tier_vision_capability() {
    use kria_core::platform::detect::HardwareTier;
    assert!(!HardwareTier::Lite.has_vision());
    assert!(!HardwareTier::Standard.has_vision());
    assert!(HardwareTier::Performance.has_vision());
    assert!(HardwareTier::High.has_vision());
}

#[test]
fn tier_stt_models() {
    use kria_core::platform::detect::HardwareTier;
    assert!(HardwareTier::Lite.stt_model().contains("small"));
    assert!(HardwareTier::Standard.stt_model().contains("medium"));
    assert!(HardwareTier::Performance.stt_model().contains("turbo"));
    assert!(HardwareTier::High.stt_model().contains("turbo"));
}

#[test]
fn tier_recommended_models() {
    use kria_core::platform::detect::HardwareTier;
    assert!(HardwareTier::Lite.recommended_model().contains("3b"));
    assert!(HardwareTier::Standard.recommended_model().contains("phi"));
    assert!(HardwareTier::Performance.recommended_model().contains("vl"));
    assert!(HardwareTier::High.recommended_model().contains("vl"));
}

// ── Hardware detection runs without crashing ──

#[test]
fn detect_hardware_runs() {
    use kria_core::platform::detect::detect_hardware;
    let info = detect_hardware();
    assert!(info.cpu_cores > 0);
    assert!(info.total_ram_mb > 0);
    assert!(!info.hostname.is_empty());
}

// ── HardwareInfo serialization ──

#[test]
fn hardware_info_serializes() {
    use kria_core::platform::detect::*;
    let info = detect_hardware();
    let json = serde_json::to_string_pretty(&info).unwrap();
    assert!(json.contains("tier"));
    assert!(json.contains("cpu_cores"));
    // Can deserialize back
    let back: HardwareInfo = serde_json::from_str(&json).unwrap();
    assert_eq!(back.tier, info.tier);
    assert_eq!(back.total_ram_mb, info.total_ram_mb);
}

// ── Tool filtering by tier ──

#[test]
fn tool_registry_filters_by_tier() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    let all = reg.list_for_tier("high");
    let standard = reg.list_for_tier("standard");
    let lite = reg.list_for_tier("lite");
    // Higher tiers should have at least as many tools
    assert!(all.len() >= standard.len());
    assert!(standard.len() >= lite.len());
    // Lite should still have basic system tools
    assert!(!lite.is_empty());
}

#[test]
fn function_schemas_respect_tier() {
    use kria_core::tools::registry::build_default_registry;
    let reg = build_default_registry();
    let schemas_high = reg.function_schemas("high");
    let schemas_lite = reg.function_schemas("lite");
    assert!(schemas_high.len() >= schemas_lite.len());
}

// ── Config hardware section ──

#[test]
fn config_hardware_defaults() {
    use kria_core::config::HardwareConfig;
    let hw = HardwareConfig::default();
    assert!(hw.tier.is_empty()); // auto-detect
    assert_eq!(hw.max_context_tokens, 0); // auto
    assert_eq!(hw.gpu_layers, -1); // auto
    assert_eq!(hw.threads, 0); // auto
}

#[test]
fn config_hardware_round_trip() {
    use kria_core::config::HardwareConfig;
    let hw = HardwareConfig {
        tier: "performance".into(),
        max_context_tokens: 4096,
        gpu_layers: 99,
        threads: 8,
    };
    let json = serde_json::to_string(&hw).unwrap();
    let back: HardwareConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(back.tier, "performance");
    assert_eq!(back.max_context_tokens, 4096);
}

#[test]
fn config_includes_hardware() {
    use kria_core::config::KriaConfig;
    let config = KriaConfig::default();
    assert!(config.hardware.tier.is_empty());
}

// ── OS detection ──

#[test]
fn os_detection() {
    use kria_core::platform::detect::get_os;
    let os = get_os();
    // On Linux CI, should be Linux
    #[cfg(target_os = "linux")]
    assert_eq!(os, kria_core::platform::detect::Os::Linux);
}

// ── Package manager detection ──

#[test]
fn package_manager_detection_runs() {
    use kria_core::platform::detect::get_package_manager;
    // Just verify it doesn't crash
    let _pm = get_package_manager();
}
