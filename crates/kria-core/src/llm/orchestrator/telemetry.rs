//! GPU/RAM telemetry providers.
//!
//! Production path: `TelemetryActor` owns all blocking I/O (NVML FFI,
//! sysinfo, nvidia-smi subprocess) on a dedicated OS thread and publishes
//! samples via `tokio::sync::watch`. Async consumers call `snapshot()` on
//! `WatchTelemetry`, which is a zero-cost borrow of the latest value.
//!
//! Test path: `GpuTelemetry` trait with `TestTelemetry` mock.
//!
//! Legacy path: `CliTelemetry` (already `spawn_blocking`-safe) and
//! `RamTelemetry` (fixed: now uses `spawn_blocking`) remain as fallbacks
//! for code that cannot migrate to the actor pattern immediately.

use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// A point-in-time telemetry reading.
#[derive(Debug, Clone)]
pub struct TelemetrySnapshot {
    /// Free VRAM in MB (for CUDA), or free RAM (for Metal/CpuOnly).
    pub free_vram_mb: u64,
    /// Total VRAM in MB (for CUDA), or total RAM (for Metal/CpuOnly).
    pub total_vram_mb: u64,
    /// GPU utilization percentage (0–100), if available.
    pub gpu_util_pct: Option<u32>,
}

/// Trait for telemetry sources. Kept as a seam for testing.
/// Production code should use `WatchTelemetry` backed by `TelemetryActor`.
#[async_trait]
pub trait GpuTelemetry: Send + Sync {
    /// Take a telemetry snapshot. Returns zero/dummy values on failure
    /// rather than propagating errors (the watchdog handles degraded telemetry).
    async fn snapshot(&self) -> TelemetrySnapshot;

    /// Human-readable name of this telemetry source.
    fn source_name(&self) -> &'static str;
}

// ── WatchTelemetry — production async facade ─────────────────────────

/// Implements `GpuTelemetry` by reading from a `watch` channel published by
/// `TelemetryActor`. `snapshot()` never blocks: it clones the latest value.
pub struct WatchTelemetry {
    rx: watch::Receiver<TelemetrySnapshot>,
}

impl WatchTelemetry {
    pub fn new(rx: watch::Receiver<TelemetrySnapshot>) -> Self {
        Self { rx }
    }
}

#[async_trait]
impl GpuTelemetry for WatchTelemetry {
    async fn snapshot(&self) -> TelemetrySnapshot {
        self.rx.borrow().clone()
    }

    fn source_name(&self) -> &'static str {
        "watch"
    }
}

// ── BlockingSampler — private sync trait for the actor thread ────────

/// Sync-only sampling backend, used exclusively inside the `TelemetryActor`
/// OS thread. Callers must never use this from async code directly.
trait BlockingSampler: Send {
    fn sample(&mut self) -> TelemetrySnapshot;
    fn name(&self) -> &'static str;
}

// ── NvmlSampler ──────────────────────────────────────────────────────

#[cfg(feature = "nvidia")]
struct NvmlSampler {
    nvml: nvml_wrapper::Nvml,
    device_index: u32,
}

#[cfg(feature = "nvidia")]
impl NvmlSampler {
    fn try_new(device_index: u32) -> Option<Self> {
        match nvml_wrapper::Nvml::init() {
            Ok(nvml) => {
                match nvml.device_by_index(device_index) {
                    Ok(dev) => {
                        if let Ok(info) = dev.memory_info() {
                            tracing::info!(
                                device = device_index,
                                total_mb = info.total / (1024 * 1024),
                                "telemetry: NVML sampler initialised"
                            );
                        }
                        Some(Self { nvml, device_index })
                    }
                    Err(e) => {
                        tracing::warn!(?e, "telemetry: NVML device {} unavailable", device_index);
                        None
                    }
                }
            }
            Err(e) => {
                tracing::warn!(?e, "telemetry: NVML init failed");
                None
            }
        }
    }
}

#[cfg(feature = "nvidia")]
impl BlockingSampler for NvmlSampler {
    fn sample(&mut self) -> TelemetrySnapshot {
        let device = match self.nvml.device_by_index(self.device_index) {
            Ok(d) => d,
            Err(e) => {
                tracing::debug!(?e, "telemetry: NVML sample failed");
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

    fn name(&self) -> &'static str {
        "nvml"
    }
}

// ── CliBlockingSampler ───────────────────────────────────────────────

/// nvidia-smi subprocess sampler. Safe to call on the actor thread
/// (dedicated OS thread — blocking is fine here).
struct CliBlockingSampler;

impl CliBlockingSampler {
    fn probe() -> bool {
        std::process::Command::new("nvidia-smi")
            .arg("--query-gpu=memory.free")
            .arg("--format=csv,noheader,nounits")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

impl BlockingSampler for CliBlockingSampler {
    fn sample(&mut self) -> TelemetrySnapshot {
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
                TelemetrySnapshot {
                    free_vram_mb: parts.first().and_then(|s| s.parse().ok()).unwrap_or(0),
                    total_vram_mb: parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0),
                    gpu_util_pct: parts.get(2).and_then(|s| s.parse().ok()),
                }
            }
            _ => TelemetrySnapshot {
                free_vram_mb: 0,
                total_vram_mb: 0,
                gpu_util_pct: None,
            },
        }
    }

    fn name(&self) -> &'static str {
        "nvidia-smi"
    }
}

// ── RamSampler ───────────────────────────────────────────────────────

struct RamSampler {
    sys: sysinfo::System,
}

impl RamSampler {
    fn new() -> Self {
        Self {
            sys: sysinfo::System::new(),
        }
    }
}

impl BlockingSampler for RamSampler {
    fn sample(&mut self) -> TelemetrySnapshot {
        self.sys.refresh_memory();
        TelemetrySnapshot {
            free_vram_mb: self.sys.available_memory() / (1024 * 1024),
            total_vram_mb: self.sys.total_memory() / (1024 * 1024),
            gpu_util_pct: None,
        }
    }

    fn name(&self) -> &'static str {
        "ram"
    }
}

// ── TelemetryActor ───────────────────────────────────────────────────

/// Dedicated OS thread that samples GPU/RAM telemetry and broadcasts via a
/// `tokio::sync::watch` channel.
///
/// All NVML FFI calls, sysinfo `/proc` reads, and nvidia-smi subprocesses
/// run on this thread — never on a Tokio worker thread.
///
/// The thread exits automatically when `TelemetryActor` is dropped (the
/// `watch::Sender` is dropped, causing the `receiver_count() == 0` check
/// to fire).
pub struct TelemetryActor {
    /// Held to keep the sender alive so the thread keeps running.
    _sender: watch::Sender<TelemetrySnapshot>,
    /// Held for diagnostics / future join-on-drop.
    _thread: std::thread::JoinHandle<()>,
}

impl TelemetryActor {
    /// Spawn the sampling thread.
    ///
    /// Returns the actor (must stay alive as long as telemetry is needed) and
    /// a `watch::Receiver` for constructing a `WatchTelemetry`.
    fn start(
        sampler: Box<dyn BlockingSampler>,
        poll_interval: Duration,
    ) -> (Self, watch::Receiver<TelemetrySnapshot>) {
        let initial = TelemetrySnapshot {
            free_vram_mb: 0,
            total_vram_mb: 0,
            gpu_util_pct: None,
        };
        let (tx, rx) = watch::channel(initial);
        // Clone sender for the thread; the actor holds the authoritative sender.
        let tx_thread = tx.clone();

        let thread = std::thread::Builder::new()
            .name("kria-telemetry".into())
            .spawn(move || {
                sampling_loop(tx_thread, sampler, poll_interval);
            })
            .expect("failed to spawn telemetry sampling thread");

        (
            Self {
                _sender: tx,
                _thread: thread,
            },
            rx,
        )
    }
}

fn sampling_loop(
    tx: watch::Sender<TelemetrySnapshot>,
    mut sampler: Box<dyn BlockingSampler>,
    poll: Duration,
) {
    tracing::info!(
        sampler = sampler.name(),
        poll_ms = poll.as_millis(),
        "telemetry thread: started"
    );

    loop {
        // Exit when the actor (and all `WatchTelemetry` clones) are dropped.
        if tx.receiver_count() == 0 {
            tracing::debug!("telemetry thread: no receivers, exiting");
            break;
        }

        let snap = sampler.sample();
        if tx.send(snap).is_err() {
            // All receivers dropped — orchestrator is shutting down.
            break;
        }

        std::thread::sleep(poll);
    }

    tracing::info!("telemetry thread: exited");
}

// ── Factory ─────────────────────────────────────────────────────────

/// Build the best available sampler for the given backend and start the
/// `TelemetryActor`. Returns the actor (keep alive in `Orchestrator`) and a
/// `WatchTelemetry` to hand to the watchdog and server_manager.
///
/// This function is **sync** and must be called before the Tokio runtime
/// drives it (or inside `spawn_blocking` if called from async).
pub fn create_telemetry_actor(
    backend: super::GpuBackend,
    poll_interval: Duration,
) -> (TelemetryActor, Arc<dyn GpuTelemetry>) {
    let sampler: Box<dyn BlockingSampler> = match backend {
        super::GpuBackend::Cuda => {
            // 1. Try NVML (fastest, no subprocess)
            #[cfg(feature = "nvidia")]
            {
                if let Some(s) = NvmlSampler::try_new(0) {
                    tracing::info!("telemetry: using NVML sampler");
                    let boxed: Box<dyn BlockingSampler> = Box::new(s);
                    let (actor, rx) = TelemetryActor::start(boxed, poll_interval);
                    return (actor, Arc::new(WatchTelemetry::new(rx)));
                }
            }

            // 2. Fall back to nvidia-smi CLI
            if CliBlockingSampler::probe() {
                tracing::info!("telemetry: using nvidia-smi CLI sampler");
                Box::new(CliBlockingSampler)
            } else {
                // 3. Last resort: RAM telemetry (won't report VRAM accurately)
                tracing::warn!("telemetry: no GPU telemetry available, using RAM sampler");
                Box::new(RamSampler::new())
            }
        }
        super::GpuBackend::Metal | super::GpuBackend::CpuOnly => {
            tracing::info!("telemetry: using RAM sampler (Metal/CpuOnly)");
            Box::new(RamSampler::new())
        }
    };

    let (actor, rx) = TelemetryActor::start(sampler, poll_interval);
    (actor, Arc::new(WatchTelemetry::new(rx)))
}

// ── Legacy wrappers (kept for test compat) ───────────────────────────

/// nvidia-smi based async telemetry. Uses `spawn_blocking` correctly.
/// Prefer `WatchTelemetry` backed by `TelemetryActor` for production paths.
pub struct CliTelemetry;

impl CliTelemetry {
    /// Returns `Some` if nvidia-smi is available and functional.
    /// Note: this probe call is sync — only call from a blocking context.
    pub fn try_new() -> Option<Self> {
        if CliBlockingSampler::probe() {
            tracing::info!("CLI telemetry (nvidia-smi) initialised");
            Some(Self)
        } else {
            None
        }
    }
}

#[async_trait]
impl GpuTelemetry for CliTelemetry {
    async fn snapshot(&self) -> TelemetrySnapshot {
        let result = tokio::task::spawn_blocking(|| {
            CliBlockingSampler.sample()
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

/// sysinfo-based RAM telemetry. Uses `spawn_blocking` to avoid blocking
/// the Tokio executor during `/proc` reads.
/// Prefer `WatchTelemetry` backed by `TelemetryActor` for production paths.
pub struct RamTelemetry;

impl RamTelemetry {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl GpuTelemetry for RamTelemetry {
    async fn snapshot(&self) -> TelemetrySnapshot {
        // Each call creates a fresh System — avoids mutex + blocking on /proc.
        let result = tokio::task::spawn_blocking(|| {
            RamSampler::new().sample()
        })
        .await;

        result.unwrap_or(TelemetrySnapshot {
            free_vram_mb: 0,
            total_vram_mb: 0,
            gpu_util_pct: None,
        })
    }

    fn source_name(&self) -> &'static str {
        "ram"
    }
}

/// Create the best available CUDA telemetry source using legacy trait path.
/// **Deprecated**: prefer `create_telemetry_actor` for production use.
pub fn create_cuda_telemetry() -> Arc<dyn GpuTelemetry> {
    #[cfg(feature = "nvidia")]
    {
        // NvmlTelemetry is no longer exposed; WatchTelemetry via actor is
        // the correct production path. This legacy factory uses CliTelemetry.
    }

    if CliTelemetry::try_new().is_some() {
        tracing::info!("telemetry: (legacy) using nvidia-smi CLI");
        return Arc::new(CliTelemetry);
    }

    tracing::warn!("telemetry: (legacy) no GPU telemetry available, falling back to RAM");
    Arc::new(RamTelemetry::new())
}
