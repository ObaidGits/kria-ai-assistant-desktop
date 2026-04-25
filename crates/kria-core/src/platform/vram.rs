//! VRAM profiling — queries free/total GPU memory for image-gen tier admission.
//!
//! Three impls behind a single trait:
//! - `NvmlProfiler`        — NVIDIA, via the `nvml-wrapper` crate (feature "nvidia")
//! - `RocmProfiler`        — AMD, shells out to `rocm-smi --showmeminfo vram --json`
//! - `WgpuFallbackProfiler`— Intel / Apple / unknown: no free-memory data; assumes 60 %
//!
//! The `ImageTier` classification is derived live from a `VramSnapshot` at request time,
//! not just at boot, so admission control stays accurate throughout a session.

use async_trait::async_trait;
use std::sync::Arc;

/// Snapshot of GPU memory at a point in time.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct VramSnapshot {
    /// Free (available) GPU memory in MiB.
    pub free_mb: u64,
    /// Total GPU memory in MiB.
    pub total_mb: u64,
    /// Driver-reserved / fragmented memory in MiB (NVML only; 0 otherwise).
    pub reserved_mb: u64,
    pub vendor: GpuVendor,
}

impl VramSnapshot {
    /// True when this snapshot comes from a real GPU query.
    pub fn is_real(&self) -> bool {
        self.vendor != GpuVendor::Unknown
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Apple,
    Unknown,
}

/// Image-generation hardware tier, separate from the LLM `HardwareTier`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageTier {
    /// ≥ 14 GB free VRAM — keep LLM + Flux-FP8 resident simultaneously.
    SHighRes,
    /// 10–14 GB free VRAM — keep LLM + Flux-Q4 resident simultaneously.
    AStandard,
    /// 4–10 GB free VRAM — drop LLM to CPU, run Flux-Q4, restore LLM.
    BDropSwap,
    /// < 4 GB free VRAM or no discrete GPU — reject or delegate to cloud.
    #[default]
    CRejectOrCloud,
}

impl ImageTier {
    /// Classify a snapshot into a tier.
    pub fn from_snapshot(snap: &VramSnapshot) -> Self {
        if !snap.is_real() {
            return Self::CRejectOrCloud;
        }
        match snap.free_mb {
            mb if mb >= 14_000 => Self::SHighRes,
            mb if mb >= 10_000 => Self::AStandard,
            mb if mb >= 4_000 => Self::BDropSwap,
            _ => Self::CRejectOrCloud,
        }
    }

    /// Whether the LLM and image model can co-reside in VRAM.
    pub fn is_parallel(&self) -> bool {
        matches!(self, Self::SHighRes | Self::AStandard)
    }

    /// Whether the drop-and-swap protocol is needed.
    pub fn needs_swap(&self) -> bool {
        matches!(self, Self::BDropSwap)
    }

    /// Tier name as a stable lowercase slug.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SHighRes => "s_high_res",
            Self::AStandard => "a_standard",
            Self::BDropSwap => "b_drop_swap",
            Self::CRejectOrCloud => "c_reject_or_cloud",
        }
    }

    /// Minimum free VRAM required in MiB for this tier's image workflow.
    pub fn required_free_mb(&self) -> u64 {
        match self {
            Self::SHighRes => 6_500,   // Flux-FP8 peak + 1024 MB safety
            Self::AStandard => 6_000,  // Flux-Q4 peak + 768 MB safety
            Self::BDropSwap => 4_500,  // Flux-Q4 after LLM offload + 512 MB safety
            Self::CRejectOrCloud => 0,
        }
    }
}

/// Parse `ImageTier` from a user-supplied config override string.
impl std::str::FromStr for ImageTier {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "s" | "s_high_res" | "shighres" => Self::SHighRes,
            "a" | "a_standard" | "astandard" => Self::AStandard,
            "b" | "b_drop_swap" | "bdropswap" | "drop_swap" => Self::BDropSwap,
            _ => Self::CRejectOrCloud,
        })
    }
}

// ─── Trait ────────────────────────────────────────────────────────────────────

/// Async interface for querying GPU memory.
#[async_trait]
pub trait VramProfiler: Send + Sync {
    /// Take a live snapshot of GPU memory.
    async fn snapshot(&self) -> VramSnapshot;
}

// ─── NVML (NVIDIA) impl ───────────────────────────────────────────────────────

/// NVIDIA profiler using the `nvml-wrapper` crate.
///
/// Only compiled when the `nvidia` feature is enabled.
#[cfg(feature = "nvidia")]
pub struct NvmlProfiler {
    nvml: nvml_wrapper::Nvml,
    device_idx: u32,
}

#[cfg(feature = "nvidia")]
impl NvmlProfiler {
    /// Try to initialise NVML and select device `device_idx` (usually 0).
    pub fn try_new(device_idx: u32) -> Option<Arc<dyn VramProfiler>> {
        match nvml_wrapper::Nvml::init() {
            Ok(nvml) => {
                tracing::info!(device_idx, "NVML initialised — using NvmlProfiler");
                Some(Arc::new(Self { nvml, device_idx }))
            }
            Err(e) => {
                tracing::info!(error = %e, "NVML unavailable, will try ROCm / fallback");
                None
            }
        }
    }
}

#[cfg(feature = "nvidia")]
#[async_trait]
impl VramProfiler for NvmlProfiler {
    async fn snapshot(&self) -> VramSnapshot {
        match self.nvml.device_by_index(self.device_idx) {
            Ok(device) => {
                let mem = device.memory_info().unwrap_or(nvml_wrapper::struct_wrappers::device::MemoryInfo {
                    free: 0,
                    total: 0,
                    used: 0,
                });
                // Bar1 reserved (fragmentation pressure).
                let reserved = device
                    .bar1_memory_info()
                    .map(|b| b.used / 1_048_576)
                    .unwrap_or(0);
                VramSnapshot {
                    free_mb: mem.free / 1_048_576,
                    total_mb: mem.total / 1_048_576,
                    reserved_mb: reserved,
                    vendor: GpuVendor::Nvidia,
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "NvmlProfiler: device query failed");
                VramSnapshot { free_mb: 0, total_mb: 0, reserved_mb: 0, vendor: GpuVendor::Nvidia }
            }
        }
    }
}

// ─── ROCm (AMD) impl ─────────────────────────────────────────────────────────

/// AMD profiler — shells out to `rocm-smi --showmeminfo vram --json`.
pub struct RocmProfiler;

impl RocmProfiler {
    pub fn try_new() -> Option<Arc<dyn VramProfiler>> {
        if crate::platform::detect::has_command("rocm-smi") {
            tracing::info!("rocm-smi found — using RocmProfiler");
            Some(Arc::new(Self))
        } else {
            None
        }
    }
}

#[async_trait]
impl VramProfiler for RocmProfiler {
    async fn snapshot(&self) -> VramSnapshot {
        let result = tokio::process::Command::new("rocm-smi")
            .args(["--showmeminfo", "vram", "--json"])
            .output()
            .await;

        let (free_mb, total_mb) = match result {
            Ok(out) if out.status.success() => parse_rocm_json(&out.stdout),
            _ => (0, 0),
        };

        VramSnapshot { free_mb, total_mb, reserved_mb: 0, vendor: GpuVendor::Amd }
    }
}

fn parse_rocm_json(bytes: &[u8]) -> (u64, u64) {
    // rocm-smi JSON: {"card0": {"VRAM Total Memory (B)": "8589934592", "VRAM Total Used Memory (B)": "..."}}
    let text = match std::str::from_utf8(bytes) { Ok(s) => s, Err(_) => return (0, 0) };
    let val = match serde_json::from_str::<serde_json::Value>(text) { Ok(v) => v, Err(_) => return (0, 0) };

    let card = match val.as_object().and_then(|m| m.values().next()) {
        Some(c) => c.clone(),
        None => return (0, 0),
    };

    let total_b: u64 = card.get("VRAM Total Memory (B)")
        .and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0);
    let used_b: u64 = card.get("VRAM Total Used Memory (B)")
        .and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0);

    (
        total_b.saturating_sub(used_b) / 1_048_576,
        total_b / 1_048_576,
    )
}

// ─── Fallback (no dGPU info) ──────────────────────────────────────────────────

/// Fallback when no GPU telemetry is available.
/// Reports zero free VRAM → always routes to Tier C.
pub struct NullProfiler;

impl NullProfiler {
    pub fn new() -> Arc<dyn VramProfiler> {
        Arc::new(Self)
    }
}

#[async_trait]
impl VramProfiler for NullProfiler {
    async fn snapshot(&self) -> VramSnapshot {
        VramSnapshot { free_mb: 0, total_mb: 0, reserved_mb: 0, vendor: GpuVendor::Unknown }
    }
}

// ─── Factory ──────────────────────────────────────────────────────────────────

/// Build the best available `VramProfiler` for the current host.
/// Priority: NVML (NVIDIA) → ROCm (AMD) → Null.
pub fn build_profiler() -> Arc<dyn VramProfiler> {
    #[cfg(feature = "nvidia")]
    if let Some(p) = NvmlProfiler::try_new(0) {
        return p;
    }

    if let Some(p) = RocmProfiler::try_new() {
        return p;
    }

    tracing::info!("No GPU telemetry available — VramProfiler will report 0 free VRAM (Tier C)");
    NullProfiler::new()
}

// ─── VRAM Barrier (deterministic eviction guard) ──────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum BarrierError {
    #[error("VRAM eviction timed out: free={free_mb}MB required={required_mb}MB (delta={last_delta_mb}MB over last poll)")]
    Timeout {
        free_mb: u64,
        required_mb: u64,
        last_delta_mb: u64,
    },
}

impl BarrierError {
    /// Free VRAM at the time of timeout.
    pub fn free_mb(&self) -> u64 {
        match self {
            Self::Timeout { free_mb, .. } => *free_mb,
        }
    }
}

/// Polls free VRAM until it reaches a threshold, or returns a timeout error.
///
/// Requires N=`stable_samples` consecutive readings above `required_mb` so
/// transient driver flushes don't trigger a false positive.
pub struct VramBarrier {
    pub profiler: Arc<dyn VramProfiler>,
    /// MiB that must be free before the ComfyUI job is dispatched.
    pub required_mb: u64,
    pub poll_interval: std::time::Duration,
    pub timeout: std::time::Duration,
    /// Consecutive stable samples required.
    pub stable_samples: usize,
}

impl VramBarrier {
    pub fn new(profiler: Arc<dyn VramProfiler>, required_mb: u64) -> Self {
        Self {
            profiler,
            required_mb,
            poll_interval: std::time::Duration::from_millis(50),
            timeout: std::time::Duration::from_secs(3),
            stable_samples: 3,
        }
    }

    pub async fn await_free(&self) -> Result<VramSnapshot, BarrierError> {
        let deadline = tokio::time::Instant::now() + self.timeout;
        let mut stable_count = 0usize;
        let mut last_free = 0u64;

        loop {
            let snap = self.profiler.snapshot().await;

            if snap.free_mb >= self.required_mb {
                stable_count += 1;
                if stable_count >= self.stable_samples {
                    tracing::debug!(
                        free_mb = snap.free_mb,
                        required_mb = self.required_mb,
                        "VRAM barrier: memory confirmed free"
                    );
                    return Ok(snap);
                }
            } else {
                stable_count = 0;
            }

            if tokio::time::Instant::now() >= deadline {
                return Err(BarrierError::Timeout {
                    free_mb: snap.free_mb,
                    required_mb: self.required_mb,
                    last_delta_mb: snap.free_mb.saturating_sub(last_free),
                });
            }

            last_free = snap.free_mb;
            tokio::time::sleep(self.poll_interval).await;
        }
    }
}
