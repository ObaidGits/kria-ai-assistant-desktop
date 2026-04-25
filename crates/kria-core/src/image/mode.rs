//! Image generation mode resolver.
//!
//! Decides local vs cloud routing independently from hardware tier detection.
//!
//! # Resolution order
//! 1. `KRIA_IMAGE_MODE` environment variable (highest priority)
//! 2. `image_generation.image_mode` config key
//! 3. `Auto` → tier-based default
//!
//! # Tier-based defaults (when mode = Auto)
//! | Tier                | Default routing        |
//! |---------------------|------------------------|
//! | `CRejectOrCloud`    | `CloudOnly`            |
//! | `BDropSwap`         | `LocalThenCloud`       |
//! | `AStandard`         | `LocalThenCloud`       |
//! | `SHighRes`          | `LocalOnly`            |
//!
//! When `cloud_fallback = "off"` the cloud half of any `*ThenCloud` default
//! collapses to `LocalOnly`.

use std::str::FromStr;

use crate::platform::vram::ImageTier;

// ─── Public enums ─────────────────────────────────────────────────────────────

/// User-facing mode setting (stored in config / set via env var).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageMode {
    /// Let KRIA decide from the hardware tier (default).
    #[default]
    Auto,
    /// ComfyUI sidecar only; hard fail if no GPU available.
    LocalOnly,
    /// Cloud providers only; never start the sidecar.
    CloudOnly,
    /// Try local first; fall back to cloud on any local failure.
    LocalWithCloudFallback,
    /// Try cloud first; fall back to local on cloud failure.
    CloudWithLocalFallback,
}

impl FromStr for ImageMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "auto" | "" => Ok(Self::Auto),
            "local_only" => Ok(Self::LocalOnly),
            "cloud_only" => Ok(Self::CloudOnly),
            "local_with_cloud_fallback" | "local+cloud" => Ok(Self::LocalWithCloudFallback),
            "cloud_with_local_fallback" | "cloud+local" => Ok(Self::CloudWithLocalFallback),
            other => Err(format!("unknown image_mode value: \"{other}\"")),
        }
    }
}

/// Concrete routing decision used by `ImageOrchestrator::generate()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedMode {
    /// Use local sidecar only; hard fail if unavailable.
    LocalOnly,
    /// Use cloud providers only; do not start the sidecar.
    CloudOnly,
    /// Try local first; on any failure, fall back to cloud.
    LocalThenCloud,
    /// Try cloud first; on failure, fall back to local.
    CloudThenLocal,
}

// ─── Resolution error ─────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ModeError {
    #[error("local generation requires a GPU (current tier is C — no discrete GPU detected)")]
    LocalRequestedNoGpu,
    #[error("cloud generation requested but cloud_fallback is \"off\" in config")]
    CloudRequestedButDisabled,
    #[error("invalid image_mode value: {0}")]
    Invalid(String),
}

// ─── Resolver ─────────────────────────────────────────────────────────────────

/// Resolve the user-facing mode + tier into a concrete routing decision.
///
/// # Parameters
/// * `config_mode`     — `image_generation.image_mode` from config (e.g. `"auto"`).
/// * `cloud_fallback`  — `image_generation.cloud_fallback` from config
///                       (`"always"` | `"auto_offer"` | `"opt_in"` | `"off"`).
/// * `tier`            — live hardware tier already resolved by the caller.
///
/// Reads `KRIA_IMAGE_MODE` and `KRIA_IMAGE_CLOUD_FALLBACK` from the environment
/// at call time (each request), so changes take effect without a restart.
pub fn resolve_image_mode(
    config_mode: &str,
    cloud_fallback: &str,
    tier: ImageTier,
) -> Result<ResolvedMode, ModeError> {
    // ── 1. Determine effective mode string: env > config ─────────────────────
    let raw = std::env::var("KRIA_IMAGE_MODE")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| config_mode.trim().to_string());

    let mode = ImageMode::from_str(&raw).map_err(ModeError::Invalid)?;

    // ── 2. Determine effective cloud availability: env > config ──────────────
    let cloud_enabled = match std::env::var("KRIA_IMAGE_CLOUD_FALLBACK")
        .ok()
        .map(|v| v.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("true") | Some("1") | Some("yes") | Some("on") => true,
        Some("false") | Some("0") | Some("no") | Some("off") => false,
        // Env var not set or unrecognised — fall back to config value.
        _ => cloud_fallback.trim().to_ascii_lowercase() != "off",
    };

    let has_gpu = tier != ImageTier::CRejectOrCloud;

    // ── 3. Map mode + constraints to ResolvedMode ────────────────────────────
    match mode {
        ImageMode::LocalOnly => {
            if !has_gpu {
                return Err(ModeError::LocalRequestedNoGpu);
            }
            Ok(ResolvedMode::LocalOnly)
        }

        ImageMode::CloudOnly => {
            if !cloud_enabled {
                return Err(ModeError::CloudRequestedButDisabled);
            }
            Ok(ResolvedMode::CloudOnly)
        }

        ImageMode::LocalWithCloudFallback => {
            if !has_gpu {
                // Graceful degrade: no GPU, use cloud if available.
                if cloud_enabled {
                    Ok(ResolvedMode::CloudOnly)
                } else {
                    Err(ModeError::LocalRequestedNoGpu)
                }
            } else if cloud_enabled {
                Ok(ResolvedMode::LocalThenCloud)
            } else {
                Ok(ResolvedMode::LocalOnly)
            }
        }

        ImageMode::CloudWithLocalFallback => {
            if !cloud_enabled {
                return Err(ModeError::CloudRequestedButDisabled);
            }
            if has_gpu {
                Ok(ResolvedMode::CloudThenLocal)
            } else {
                Ok(ResolvedMode::CloudOnly)
            }
        }

        ImageMode::Auto => match tier {
            ImageTier::CRejectOrCloud => {
                if !cloud_enabled {
                    Err(ModeError::LocalRequestedNoGpu)
                } else {
                    Ok(ResolvedMode::CloudOnly)
                }
            }
            ImageTier::SHighRes => Ok(ResolvedMode::LocalOnly),
            ImageTier::AStandard | ImageTier::BDropSwap => {
                if cloud_enabled {
                    Ok(ResolvedMode::LocalThenCloud)
                } else {
                    Ok(ResolvedMode::LocalOnly)
                }
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_tier_c_with_cloud() {
        let r = resolve_image_mode("auto", "always", ImageTier::CRejectOrCloud).unwrap();
        assert_eq!(r, ResolvedMode::CloudOnly);
    }

    #[test]
    fn auto_tier_b_with_cloud() {
        let r = resolve_image_mode("auto", "always", ImageTier::BDropSwap).unwrap();
        assert_eq!(r, ResolvedMode::LocalThenCloud);
    }

    #[test]
    fn auto_tier_b_cloud_off() {
        let r = resolve_image_mode("auto", "off", ImageTier::BDropSwap).unwrap();
        assert_eq!(r, ResolvedMode::LocalOnly);
    }

    #[test]
    fn auto_tier_s() {
        let r = resolve_image_mode("auto", "always", ImageTier::SHighRes).unwrap();
        assert_eq!(r, ResolvedMode::LocalOnly);
    }
}
