//! Integration tests for the Phase 13A image-generation mode resolver.
//!
//! These tests exercise `resolve_image_mode()` across the full §4.5 behavior
//! matrix.  Because `KRIA_IMAGE_MODE` and `KRIA_IMAGE_CLOUD_FALLBACK` are
//! process-global env vars, every test that touches them must run serially.
//! Tests that do NOT set env vars can run freely.

use kria_core::image::mode::{resolve_image_mode, ModeError, ResolvedMode};
use kria_core::platform::vram::ImageTier;
use serial_test::serial;

// ─── Helper ──────────────────────────────────────────────────────────────────

/// Run `f` with `KRIA_IMAGE_MODE` set to `val`, then restore the previous state.
fn with_mode_env<F: FnOnce()>(val: &str, f: F) {
    let prev = std::env::var("KRIA_IMAGE_MODE").ok();
    std::env::set_var("KRIA_IMAGE_MODE", val);
    f();
    match prev {
        Some(v) => std::env::set_var("KRIA_IMAGE_MODE", v),
        None => std::env::remove_var("KRIA_IMAGE_MODE"),
    }
}

/// Run `f` with `KRIA_IMAGE_CLOUD_FALLBACK` set to `val`, then restore.
fn with_cloud_env<F: FnOnce()>(val: &str, f: F) {
    let prev = std::env::var("KRIA_IMAGE_CLOUD_FALLBACK").ok();
    std::env::set_var("KRIA_IMAGE_CLOUD_FALLBACK", val);
    f();
    match prev {
        Some(v) => std::env::set_var("KRIA_IMAGE_CLOUD_FALLBACK", v),
        None => std::env::remove_var("KRIA_IMAGE_CLOUD_FALLBACK"),
    }
}

// ─── M-T01 through M-T04: Auto mode, no env vars ─────────────────────────────

/// M-T01: auto + cloud enabled + Tier C → CloudOnly
#[test]
fn m_t01_auto_tier_c_cloud_always() {
    let r = resolve_image_mode("auto", "always", ImageTier::CRejectOrCloud).unwrap();
    assert_eq!(r, ResolvedMode::CloudOnly, "M-T01 failed");
}

/// M-T02: auto + cloud enabled + Tier B → LocalThenCloud
#[test]
fn m_t02_auto_tier_b_cloud_always() {
    let r = resolve_image_mode("auto", "always", ImageTier::BDropSwap).unwrap();
    assert_eq!(r, ResolvedMode::LocalThenCloud, "M-T02 failed");
}

/// M-T03: auto + cloud OFF + Tier B → LocalOnly
#[test]
fn m_t03_auto_tier_b_cloud_off() {
    let r = resolve_image_mode("auto", "off", ImageTier::BDropSwap).unwrap();
    assert_eq!(r, ResolvedMode::LocalOnly, "M-T03 failed");
}

/// M-T04: auto + cloud always + Tier S → LocalOnly
#[test]
fn m_t04_auto_tier_s_cloud_always() {
    let r = resolve_image_mode("auto", "always", ImageTier::SHighRes).unwrap();
    assert_eq!(r, ResolvedMode::LocalOnly, "M-T04 failed");
}

// ─── M-T05 / M-T06: LocalOnly ────────────────────────────────────────────────

/// M-T05: local_only + no GPU → Err(LocalRequestedNoGpu)
#[test]
fn m_t05_local_only_tier_c_errors() {
    let e = resolve_image_mode("local_only", "always", ImageTier::CRejectOrCloud)
        .unwrap_err();
    assert!(
        matches!(e, ModeError::LocalRequestedNoGpu),
        "M-T05: expected LocalRequestedNoGpu, got {e}"
    );
}

/// M-T06: local_only + Tier B → LocalOnly
#[test]
fn m_t06_local_only_tier_b() {
    let r = resolve_image_mode("local_only", "always", ImageTier::BDropSwap).unwrap();
    assert_eq!(r, ResolvedMode::LocalOnly, "M-T06 failed");
}

// ─── M-T07 / M-T08: CloudOnly ────────────────────────────────────────────────

/// M-T07: cloud_only + cloud enabled + Tier S → CloudOnly
#[test]
fn m_t07_cloud_only_tier_s() {
    let r = resolve_image_mode("cloud_only", "always", ImageTier::SHighRes).unwrap();
    assert_eq!(r, ResolvedMode::CloudOnly, "M-T07 failed");
}

/// M-T08: cloud_only + cloud OFF → Err(CloudRequestedButDisabled)
#[test]
fn m_t08_cloud_only_cloud_off_errors() {
    let e = resolve_image_mode("cloud_only", "off", ImageTier::BDropSwap).unwrap_err();
    assert!(
        matches!(e, ModeError::CloudRequestedButDisabled),
        "M-T08: expected CloudRequestedButDisabled, got {e}"
    );
}

// ─── M-T09 / M-T10: LocalWithCloudFallback ───────────────────────────────────

/// M-T09: local_with_cloud_fallback + Tier B → LocalThenCloud
#[test]
fn m_t09_local_with_cloud_fallback_tier_b() {
    let r = resolve_image_mode("local_with_cloud_fallback", "always", ImageTier::BDropSwap)
        .unwrap();
    assert_eq!(r, ResolvedMode::LocalThenCloud, "M-T09 failed");
}

/// M-T10: local_with_cloud_fallback + Tier C + cloud enabled → graceful CloudOnly
#[test]
fn m_t10_local_with_cloud_fallback_tier_c_degrades() {
    let r = resolve_image_mode("local_with_cloud_fallback", "always", ImageTier::CRejectOrCloud)
        .unwrap();
    assert_eq!(r, ResolvedMode::CloudOnly, "M-T10: expected graceful CloudOnly degrade");
}

// ─── M-T11: CloudWithLocalFallback ───────────────────────────────────────────

/// M-T11: cloud_with_local_fallback + Tier B → CloudThenLocal
#[test]
fn m_t11_cloud_with_local_fallback_tier_b() {
    let r = resolve_image_mode("cloud_with_local_fallback", "always", ImageTier::BDropSwap)
        .unwrap();
    assert_eq!(r, ResolvedMode::CloudThenLocal, "M-T11 failed");
}

// ─── M-T12 / M-T13: env var overrides (serial — mutate process env) ──────────

/// M-T12: KRIA_IMAGE_MODE=cloud_only overrides config "auto" on Tier B
#[test]
#[serial]
fn m_t12_env_mode_overrides_config() {
    with_mode_env("cloud_only", || {
        let r = resolve_image_mode("auto", "always", ImageTier::BDropSwap).unwrap();
        assert_eq!(r, ResolvedMode::CloudOnly, "M-T12: env var should win");
    });
}

/// M-T13: KRIA_IMAGE_CLOUD_FALLBACK=false collapses LocalThenCloud → LocalOnly
#[test]
#[serial]
fn m_t13_env_cloud_fallback_false_overrides_config() {
    with_cloud_env("false", || {
        let r = resolve_image_mode("auto", "always", ImageTier::BDropSwap).unwrap();
        assert_eq!(r, ResolvedMode::LocalOnly, "M-T13: cloud env=false should win");
    });
}

// ─── M-T14: invalid mode value ───────────────────────────────────────────────

/// M-T14: unrecognised mode string → Err(Invalid)
#[test]
fn m_t14_invalid_mode_string() {
    let e = resolve_image_mode("garbage_mode", "always", ImageTier::BDropSwap).unwrap_err();
    assert!(
        matches!(e, ModeError::Invalid(_)),
        "M-T14: expected Invalid error, got {e}"
    );
}

// ─── Additional edge cases ────────────────────────────────────────────────────

/// Empty string mode treats as "auto".
#[test]
fn empty_mode_treated_as_auto() {
    let r = resolve_image_mode("", "always", ImageTier::BDropSwap).unwrap();
    assert_eq!(r, ResolvedMode::LocalThenCloud);
}

/// Hyphenated variant parses correctly.
#[test]
fn hyphen_variant_parses() {
    let r = resolve_image_mode("local-only", "always", ImageTier::BDropSwap).unwrap();
    assert_eq!(r, ResolvedMode::LocalOnly);
}

/// cloud_with_local_fallback + Tier C + cloud enabled → CloudOnly (no local).
#[test]
fn cloud_with_local_fallback_no_gpu_stays_cloud() {
    let r = resolve_image_mode("cloud_with_local_fallback", "always", ImageTier::CRejectOrCloud)
        .unwrap();
    assert_eq!(r, ResolvedMode::CloudOnly);
}

/// local_with_cloud_fallback + cloud OFF + Tier C → error (no GPU, no cloud).
#[test]
fn local_with_cloud_fallback_no_gpu_no_cloud_errors() {
    let e =
        resolve_image_mode("local_with_cloud_fallback", "off", ImageTier::CRejectOrCloud)
            .unwrap_err();
    assert!(matches!(e, ModeError::LocalRequestedNoGpu));
}

/// Tier A behaves like Tier B in Auto mode.
#[test]
fn auto_tier_a_with_cloud() {
    let r = resolve_image_mode("auto", "always", ImageTier::AStandard).unwrap();
    assert_eq!(r, ResolvedMode::LocalThenCloud);
}

/// KRIA_IMAGE_CLOUD_FALLBACK=true truthy override.
#[test]
#[serial]
fn env_cloud_fallback_true_enables_cloud() {
    with_cloud_env("true", || {
        let r = resolve_image_mode("auto", "off", ImageTier::BDropSwap).unwrap();
        assert_eq!(r, ResolvedMode::LocalThenCloud, "env true should override config off");
    });
}
