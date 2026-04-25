//! Hardware profiling for first-boot and tier detection.
//!
//! Extends the basic `detect_hardware()` in `platform::detect` with:
//! - NVML-based VRAM detection (fast, no subprocess) when the `nvidia` feature is enabled
//! - Multi-vendor GPU detection (NVIDIA, AMD reported, Apple Silicon Metal)
//! - Persistence to `~/.kria/hardware_profile.json`
//! - Architecture and platform metadata for manifest matching

use crate::platform::detect::{self, HardwareInfo, HardwareTier};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// GPU vendor detected on the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    AppleSilicon,
    None,
}

/// Extended hardware profile with provisioning-relevant metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareProfile {
    /// Base hardware info (os, tier, cpu, ram, vram, gpu_name, etc.)
    #[serde(flatten)]
    pub info: HardwareInfo,
    /// Detected GPU vendor
    pub gpu_vendor: GpuVendor,
    /// CPU architecture (x86_64, aarch64, etc.)
    pub arch: String,
    /// Whether CUDA is available (NVML or nvidia-smi responded)
    pub cuda_available: bool,
    /// Whether this GPU vendor is supported for GPU-accelerated inference
    pub gpu_supported: bool,
}

impl HardwareProfile {
    /// Convenience access to the tier.
    pub fn tier(&self) -> HardwareTier {
        self.info.tier
    }

    /// Platform-arch string for manifest matching (e.g. "linux-x86_64").
    pub fn platform_key(&self) -> String {
        let os = match self.info.os {
            detect::Os::Linux => "linux",
            detect::Os::Windows => "windows",
            detect::Os::MacOS => "macos",
        };
        format!("{}-{}", os, self.arch)
    }
}

/// Run full hardware profiling.
///
/// Uses NVML (if `nvidia` feature enabled) for fast VRAM detection,
/// falls back to `nvidia-smi` CLI, then checks for AMD/Apple Silicon.
pub fn profile_hardware() -> HardwareProfile {
    // Start with the existing detection (nvidia-smi CLI + sysinfo)
    let mut info = detect::detect_hardware();

    let (gpu_vendor, cuda_available) = detect_gpu_vendor(&info);

    // If NVML is available and we didn't get VRAM from CLI, try NVML
    #[cfg(feature = "nvidia")]
    if info.vram_mb.is_none() {
        if let Some((vram, name)) = detect_vram_nvml() {
            info.vram_mb = Some(vram);
            if info.gpu_name.is_none() {
                info.gpu_name = Some(name);
            }
            // Reclassify tier with new VRAM data
            info.tier = classify_tier(info.total_ram_mb, info.vram_mb);
        }
    }

    // On macOS with Apple Silicon, treat system RAM as shared GPU memory for tiering
    if gpu_vendor == GpuVendor::AppleSilicon && info.vram_mb.is_none() {
        // Apple Silicon uses unified memory — treat 75% of RAM as available for GPU
        let unified_vram = (info.total_ram_mb as f64 * 0.75) as u64;
        info.vram_mb = Some(unified_vram);
        info.tier = classify_tier(info.total_ram_mb, info.vram_mb);
    }

    let gpu_supported = matches!(gpu_vendor, GpuVendor::Nvidia | GpuVendor::AppleSilicon);

    HardwareProfile {
        info,
        gpu_vendor,
        arch: std::env::consts::ARCH.to_string(),
        cuda_available,
        gpu_supported,
    }
}

/// Detect GPU vendor from hardware info and system probes.
fn detect_gpu_vendor(info: &HardwareInfo) -> (GpuVendor, bool) {
    // NVIDIA: already detected via nvidia-smi in detect_hardware()
    if info.vram_mb.is_some() || info.gpu_name.is_some() {
        return (GpuVendor::Nvidia, true);
    }

    // NVIDIA: check NVML directly (feature-gated)
    #[cfg(feature = "nvidia")]
    if nvml_wrapper::Nvml::init().is_ok() {
        return (GpuVendor::Nvidia, true);
    }

    // macOS: check for Apple Silicon
    if cfg!(target_os = "macos") && std::env::consts::ARCH == "aarch64" {
        return (GpuVendor::AppleSilicon, false);
    }

    // Linux: check /sys/class/drm for AMD/Intel
    #[cfg(target_os = "linux")]
    {
        if let Some(vendor) = detect_gpu_vendor_sysfs() {
            let cuda = false; // AMD/Intel don't use CUDA
            return (vendor, cuda);
        }
    }

    (GpuVendor::None, false)
}

/// Detect GPU vendor from Linux sysfs (AMD, Intel).
#[cfg(target_os = "linux")]
fn detect_gpu_vendor_sysfs() -> Option<GpuVendor> {
    use std::fs;
    let drm = std::path::Path::new("/sys/class/drm");
    if !drm.exists() {
        return None;
    }
    for entry in fs::read_dir(drm).ok()?.flatten() {
        let vendor_path = entry.path().join("device").join("vendor");
        if let Ok(vendor_id) = fs::read_to_string(&vendor_path) {
            let id = vendor_id.trim();
            if id == "0x1002" {
                tracing::info!("detected AMD GPU via sysfs (not yet supported for acceleration)");
                return Some(GpuVendor::Amd);
            }
            if id == "0x8086" {
                // Intel integrated or Arc
                tracing::info!(
                    "detected Intel GPU via sysfs (not yet supported for acceleration)"
                );
                return Some(GpuVendor::Intel);
            }
        }
    }
    None
}

/// Detect VRAM using NVML (fast, no subprocess).
#[cfg(feature = "nvidia")]
fn detect_vram_nvml() -> Option<(u64, String)> {
    let nvml = nvml_wrapper::Nvml::init().ok()?;
    let device = nvml.device_by_index(0).ok()?;
    let mem = device.memory_info().ok()?;
    let name = device.name().unwrap_or_else(|_| "NVIDIA GPU".to_string());
    Some((mem.total / (1024 * 1024), name))
}

/// Classify tier from RAM + VRAM (matches detect_hardware.sh logic).
fn classify_tier(ram_mb: u64, vram_mb: Option<u64>) -> HardwareTier {
    let vram = vram_mb.unwrap_or(0);
    // Match detect_hardware.sh: GPU >= 8GB AND RAM >= 16GB → High
    if vram >= 8192 && ram_mb >= 16384 {
        HardwareTier::High
    } else if vram >= 4096 && ram_mb >= 12288 {
        HardwareTier::Performance
    } else if ram_mb >= 8192 {
        HardwareTier::Standard
    } else {
        HardwareTier::Lite
    }
}

/// Path to the persisted hardware profile.
pub fn profile_path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".kria").join("hardware_profile.json")
}

/// Load a previously saved profile, if it exists.
pub fn load_profile() -> Option<HardwareProfile> {
    let path = profile_path();
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Save the profile to disk.
pub fn save_profile(profile: &HardwareProfile) -> anyhow::Result<()> {
    let path = profile_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(profile)?;
    std::fs::write(&path, json)?;
    tracing::info!(tier = ?profile.info.tier, gpu = ?profile.gpu_vendor, "hardware profile saved");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tier_classification_matches_script() {
        // High: GPU >= 8GB AND RAM >= 16GB
        assert_eq!(classify_tier(16384, Some(8192)), HardwareTier::High);
        assert_eq!(classify_tier(32768, Some(12288)), HardwareTier::High);

        // Performance: GPU >= 4GB AND RAM >= 12GB
        assert_eq!(
            classify_tier(12288, Some(4096)),
            HardwareTier::Performance
        );
        assert_eq!(
            classify_tier(16384, Some(6144)),
            HardwareTier::Performance
        );

        // Standard: RAM >= 8GB (no GPU or GPU < 4GB)
        assert_eq!(classify_tier(8192, None), HardwareTier::Standard);
        assert_eq!(classify_tier(8192, Some(2048)), HardwareTier::Standard);

        // Lite: RAM < 8GB
        assert_eq!(classify_tier(4096, None), HardwareTier::Lite);
        assert_eq!(classify_tier(6144, Some(1024)), HardwareTier::Lite);

        // Edge: GPU >= 8GB but RAM < 16GB → not High
        assert_eq!(
            classify_tier(12288, Some(8192)),
            HardwareTier::Performance
        );
    }

    #[test]
    fn test_platform_key() {
        let profile = HardwareProfile {
            info: HardwareInfo {
                os: crate::platform::detect::Os::Linux,
                tier: HardwareTier::Standard,
                cpu_cores: 8,
                total_ram_mb: 16384,
                vram_mb: None,
                vram_free_mb: 0,
                image_tier: crate::platform::vram::ImageTier::CRejectOrCloud,
                gpu_name: None,
                package_manager: None,
                hostname: "test".into(),
            },
            gpu_vendor: GpuVendor::None,
            arch: "x86_64".into(),
            cuda_available: false,
            gpu_supported: false,
        };
        assert_eq!(profile.platform_key(), "linux-x86_64");
    }
}
