//! Hardware Orchestrator — manages llama-server lifecycle and dynamic GPU
//! layer offloading based on real-time VRAM/RAM telemetry.
//!
//! Cross-platform: NVML on Linux/Windows, RAM-based on macOS, disabled when
//! no GPU is present.

pub mod gpu_watchdog;
pub mod server_manager;
pub mod strategy;
pub mod telemetry;

use crate::config::OrchestratorConfig;
use crate::infra::event_bus::EventBus;
use crate::infra::health::HealthRegistry;
use std::sync::Arc;

/// Which GPU backend is available on this platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuBackend {
    /// NVIDIA GPU (Linux/Windows) — full VRAM-based orchestration.
    Cuda,
    /// Apple Silicon (macOS) — unified memory, RAM-based telemetry, static ngl.
    Metal,
    /// No discrete GPU — CPU-only inference, orchestrator mostly static.
    CpuOnly,
}

impl GpuBackend {
    /// Detect the GPU backend for the current platform.
    pub fn detect() -> Self {
        #[cfg(target_os = "macos")]
        {
            return GpuBackend::Metal;
        }

        #[cfg(not(target_os = "macos"))]
        {
            // Check NVML availability first, then nvidia-smi CLI fallback
            if Self::has_nvidia_gpu() {
                GpuBackend::Cuda
            } else {
                GpuBackend::CpuOnly
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn has_nvidia_gpu() -> bool {
        // Try NVML feature first
        #[cfg(feature = "nvidia")]
        {
            if nvml_wrapper::Nvml::init().is_ok() {
                return true;
            }
        }
        // CLI fallback: check if nvidia-smi exists and works
        std::process::Command::new("nvidia-smi")
            .arg("--query-gpu=name")
            .arg("--format=csv,noheader")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Whether VRAM-based dynamic orchestration is supported.
    pub fn supports_vram_orchestration(&self) -> bool {
        matches!(self, GpuBackend::Cuda)
    }
}

/// Snapshot of the current orchestrator state exposed to other components.
#[derive(Debug, Clone)]
pub struct OrchestratorSnapshot {
    pub backend: GpuBackend,
    pub current_ngl: u32,
    pub current_context: u32,
    pub degradation: strategy::DegradationLevel,
    pub server_healthy: bool,
}

/// Top-level orchestrator that wires telemetry → watchdog → server_manager.
pub struct Orchestrator {
    pub config: OrchestratorConfig,
    pub backend: GpuBackend,
    pub server_manager: Arc<server_manager::LlamaServerManager>,
    watchdog_handle: Option<tokio::task::JoinHandle<()>>,
}

impl Orchestrator {
    /// Create and start the orchestrator.
    ///
    /// - Detects GPU backend
    /// - Spawns llama-server with optimal initial parameters
    /// - Starts the GPU watchdog telemetry loop
    pub async fn start(
        config: OrchestratorConfig,
        model_path: String,
        mmproj_path: Option<String>,
        event_bus: Arc<EventBus>,
        health: Arc<HealthRegistry>,
    ) -> anyhow::Result<Arc<Self>> {
        let backend = GpuBackend::detect();
        tracing::info!(?backend, "orchestrator: detected GPU backend");

        // Build telemetry source
        let telemetry: Arc<dyn telemetry::GpuTelemetry> = match backend {
            GpuBackend::Cuda => telemetry::create_cuda_telemetry(),
            GpuBackend::Metal | GpuBackend::CpuOnly => {
                Arc::new(telemetry::RamTelemetry::new())
            }
        };

        // Calculate initial parameters from pre-spawn telemetry
        let initial_snapshot = telemetry.snapshot().await;
        let initial_params = strategy::calculate_target_params(
            &config.model_profile,
            initial_snapshot.free_vram_mb,
            config.safety_margin_mb,
            backend,
        );

        tracing::info!(
            ngl = initial_params.ngl,
            ctx = initial_params.context,
            degradation = ?initial_params.degradation,
            "orchestrator: initial parameters"
        );

        // Create and spawn llama-server
        let server_manager = Arc::new(server_manager::LlamaServerManager::new(
            config.clone(),
            model_path,
            mmproj_path,
        ));

        server_manager
            .spawn(
                initial_params.ngl,
                initial_params.context,
                initial_params.enable_vision,
                event_bus.clone(),
            )
            .await?;

        health.register("llama-server");
        health.update("llama-server", crate::infra::health::ServiceStatus::Healthy, None);
        health.register("orchestrator");
        health.update("orchestrator", crate::infra::health::ServiceStatus::Healthy, None);

        // Start the watchdog loop
        let watchdog = gpu_watchdog::GpuWatchdog::new(
            config.clone(),
            backend,
            telemetry,
            server_manager.clone(),
            event_bus.clone(),
        );

        let watchdog_handle = tokio::spawn(async move {
            watchdog.run().await;
        });

        let orchestrator = Arc::new(Self {
            config,
            backend,
            server_manager,
            watchdog_handle: Some(watchdog_handle),
        });

        Ok(orchestrator)
    }

    /// Get a snapshot of the current orchestrator state.
    pub fn snapshot(&self) -> OrchestratorSnapshot {
        let (ngl, ctx) = self.server_manager.current_params();
        let degradation = strategy::degradation_level(ngl, ctx, &self.config.model_profile);
        OrchestratorSnapshot {
            backend: self.backend,
            current_ngl: ngl,
            current_context: ctx,
            degradation,
            server_healthy: self.server_manager.is_healthy(),
        }
    }

    /// Get the current API URL of the running llama-server.
    pub fn api_url(&self) -> String {
        self.server_manager.api_url()
    }

    /// Graceful shutdown: stop watchdog, then kill server.
    pub async fn shutdown(&self) {
        tracing::info!("orchestrator: shutting down");
        // The watchdog will stop when it detects server_manager is shutting down
        if let Some(ref handle) = self.watchdog_handle {
            handle.abort();
        }
        self.server_manager.kill().await;
    }
}

impl Drop for Orchestrator {
    fn drop(&mut self) {
        if let Some(handle) = self.watchdog_handle.take() {
            handle.abort();
        }
    }
}
