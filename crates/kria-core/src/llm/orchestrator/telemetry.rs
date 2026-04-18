//! GPU/RAM telemetry providers.
//!
//! - `NvmlTelemetry`: NVML bindings (Linux/Windows, requires `nvidia` feature)
//! - `CliTelemetry`: nvidia-smi CLI fallback
//! - `RamTelemetry`: sysinfo-based RAM monitoring (macOS, no-GPU)

use async_trait::async_trait;
use std::sync::Arc;

/// A point-in-time telemetry reading.
#[derive(Debug, Clone)]
pub struct TelemetrySnapshot {
    /// Free VRAM in MB (for CUDA), or free RAM (for Metal/CpuOnly).
    pub free_vram_mb: u64,
    /// Total VRAM in MB (for CUDA), or total RAM (for Metal/CpuOnly).
    pub total_vram_mb: u64,
    /// GPU utilization percentage (0-100), if available.
    pub gpu_util_pct: Option<u32>,
}

/// Trait for telemetry sources. Implementations must be Send + Sync for
/// use across async tasks.
#[async_trait]
pub trait GpuTelemetry: Send + Sync {
    /// Take a telemetry snapshot. Returns zero/dummy values on failure
    /// rather than propagating errors (the watchdog handles degraded telemetry).
    async fn snapshot(&self) -> TelemetrySnapshot;

    /// Human-readable name of this telemetry source.
    fn source_name(&self) -> &'static str;
}

// ── NVML Telemetry (feature-gated) ─────────────────────────────────

#[cfg(feature = "nvidia")]
pub struct NvmlTelemetry {
    nvml: nvml_wrapper::Nvml,
    device_index: u32,
}

#[cfg(feature = "nvidia")]
impl NvmlTelemetry {
    pub fn try_new(device_index: u32) -> Option<Self> {
        match nvml_wrapper::Nvml::init() {
            Ok(nvml) => {
                // Validate the device index
                match nvml.device_by_index(device_index) {
                    Ok(dev) => {
                        if let Ok(info) = dev.memory_info() {
                            tracing::info!(
                                device = device_index,
                                total_mb = info.total / (1024 * 1024),
                                "NVML telemetry initialized"
                            );
                        }
                        Some(Self { nvml, device_index })
                    }
                    Err(e) => {
                        tracing::warn!(?e, "NVML: failed to get device {}", device_index);
                        None
                    }
                }
            }
            Err(e) => {
                tracing::warn!(?e, "NVML: failed to initialize");
                None
            }
        }
    }
}

#[cfg(feature = "nvidia")]
#[async_trait]
impl GpuTelemetry for NvmlTelemetry {
    async fn snapshot(&self) -> TelemetrySnapshot {
        let device = match self.nvml.device_by_index(self.device_index) {
            Ok(d) => d,
            Err(e) => {
                tracing::debug!(?e, "NVML snapshot failed");
                return TelemetrySnapshot {
                    free_vram_mb: 0,
                    total_vram_mb: 0,
                    gpu_util_pct: None,
                };
            }
        };

        let mem = device.memory_info().ok();
        let util = device.utilization_rates().ok();

        TelemetrySnapshot {
            free_vram_mb: mem.as_ref().map(|m| m.free / (1024 * 1024)).unwrap_or(0),
            total_vram_mb: mem.as_ref().map(|m| m.total / (1024 * 1024)).unwrap_or(0),
            gpu_util_pct: util.map(|u| u.gpu),
        }
    }

    fn source_name(&self) -> &'static str {
        "nvml"
    }
}

// ── CLI Telemetry (nvidia-smi fallback) ─────────────────────────────

pub struct CliTelemetry;

impl CliTelemetry {
    /// Returns Some(Self) if nvidia-smi is available and functional.
    pub fn try_new() -> Option<Self> {
        let ok = std::process::Command::new("nvidia-smi")
            .arg("--query-gpu=memory.free")
            .arg("--format=csv,noheader,nounits")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        if ok {
            tracing::info!("CLI telemetry (nvidia-smi) initialized");
            Some(Self)
        } else {
            None
        }
    }
}

#[async_trait]
impl GpuTelemetry for CliTelemetry {
    async fn snapshot(&self) -> TelemetrySnapshot {
        // Run nvidia-smi in a blocking thread to avoid blocking the tokio runtime
        let result = tokio::task::spawn_blocking(|| {
            let output = std::process::Command::new("nvidia-smi")
                .args([
                    "--query-gpu=memory.free,memory.total,utilization.gpu",
                    "--format=csv,noheader,nounits",
                ])
                .output();

            match output {
                Ok(o) if o.status.success() => {
                    let text = String::from_utf8_lossy(&o.stdout);
                    let parts: Vec<&str> = text.trim().split(',').map(|s| s.trim()).collect();
                    let free = parts.first().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
                    let total = parts.get(1).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
                    let util = parts.get(2).and_then(|s| s.parse::<u32>().ok());
                    TelemetrySnapshot {
                        free_vram_mb: free,
                        total_vram_mb: total,
                        gpu_util_pct: util,
                    }
                }
                _ => TelemetrySnapshot {
                    free_vram_mb: 0,
                    total_vram_mb: 0,
                    gpu_util_pct: None,
                },
            }
        })
        .await;

        result.unwrap_or(TelemetrySnapshot {
            free_vram_mb: 0,
            total_vram_mb: 0,
            gpu_util_pct: None,
        })
    }

    fn source_name(&self) -> &'static str {
        "nvidia-smi"
    }
}

// ── RAM Telemetry (macOS / no-GPU fallback) ─────────────────────────

pub struct RamTelemetry {
    system: std::sync::Mutex<sysinfo::System>,
}

impl RamTelemetry {
    pub fn new() -> Self {
        Self {
            system: std::sync::Mutex::new(sysinfo::System::new()),
        }
    }
}

#[async_trait]
impl GpuTelemetry for RamTelemetry {
    async fn snapshot(&self) -> TelemetrySnapshot {
        // sysinfo is not Send, so use a Mutex and refresh in place
        let (free, total) = {
            let mut sys = self.system.lock().unwrap_or_else(|e| e.into_inner());
            sys.refresh_memory();
            let free_mb = sys.available_memory() / (1024 * 1024);
            let total_mb = sys.total_memory() / (1024 * 1024);
            (free_mb, total_mb)
        };

        TelemetrySnapshot {
            free_vram_mb: free,
            total_vram_mb: total,
            gpu_util_pct: None,
        }
    }

    fn source_name(&self) -> &'static str {
        "ram"
    }
}

// ── Factory ─────────────────────────────────────────────────────────

/// Create the best available CUDA telemetry source.
/// Tries NVML first, then nvidia-smi CLI, then falls back to RAM telemetry.
pub fn create_cuda_telemetry() -> Arc<dyn GpuTelemetry> {
    #[cfg(feature = "nvidia")]
    {
        if let Some(nvml) = NvmlTelemetry::try_new(0) {
            tracing::info!("telemetry: using NVML");
            return Arc::new(nvml);
        }
    }

    if let Some(cli) = CliTelemetry::try_new() {
        tracing::info!("telemetry: using nvidia-smi CLI fallback");
        return Arc::new(cli);
    }

    tracing::warn!("telemetry: no GPU telemetry available, falling back to RAM");
    Arc::new(RamTelemetry::new())
}
