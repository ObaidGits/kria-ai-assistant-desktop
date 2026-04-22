//! Hardware Orchestrator — manages llama-server lifecycle and dynamic GPU
//! layer offloading based on real-time VRAM/RAM telemetry.
//!
//! Cross-platform: NVML on Linux/Windows, RAM-based on macOS, disabled when
//! no GPU is present.

pub mod child_guard;
pub mod gpu_watchdog;
pub mod server_manager;
pub mod strategy;
pub mod telemetry;
pub mod tier_strategy;

use crate::config::OrchestratorConfig;
use crate::infra::event_bus::EventBus;
use crate::infra::health::HealthRegistry;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

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
    telemetry: Arc<dyn telemetry::GpuTelemetry>,
    event_bus: Arc<EventBus>,
    health: Arc<HealthRegistry>,
    watchdog_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    lifecycle_lock: Mutex<()>,
    last_restart_at: Mutex<Option<Instant>>,
    /// Keeps the TelemetryActor OS thread alive for the duration of the
    /// orchestrator's lifetime. Drop order: actor is dropped after watchdog.
    _telemetry_actor: Option<telemetry::TelemetryActor>,
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
        // GpuBackend::detect() calls nvidia-smi (a subprocess) when NVML is
        // unavailable. Wrap in spawn_blocking so we never block a Tokio worker.
        let backend = tokio::task::spawn_blocking(GpuBackend::detect)
            .await
            .unwrap_or(GpuBackend::CpuOnly);
        tracing::info!(?backend, "orchestrator: detected GPU backend");

        // Start the TelemetryActor: a dedicated OS thread that owns NVML/sysinfo
        // and publishes snapshots via a watch channel. All async consumers read
        // from WatchTelemetry (zero-cost borrow) — no executor blocking.
        let poll_interval = Duration::from_secs(config.poll_interval_secs.max(1));
        let (telemetry_actor, telemetry) =
            tokio::task::spawn_blocking(move || {
                telemetry::create_telemetry_actor(backend, poll_interval)
            })
            .await
            .expect("telemetry actor thread spawn failed");

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
            telemetry.clone(),
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
            telemetry,
            event_bus,
            health,
            watchdog_handle: Mutex::new(Some(watchdog_handle)),
            lifecycle_lock: Mutex::new(()),
            last_restart_at: Mutex::new(None),
            _telemetry_actor: Some(telemetry_actor),
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
        let _lifecycle_guard = self.lifecycle_lock.lock().await;
        tracing::info!("orchestrator: shutting down");
        self.stop_watchdog().await;

        self.server_manager
            .graceful_stop_with_timeout(Duration::from_secs(
                self.config.graceful_stop_timeout_secs.max(1),
            ))
            .await;

        if self.backend == GpuBackend::Cuda {
            self.wait_for_vram_release_bounded(
                Duration::from_secs(self.config.vram_release_timeout_secs.max(1)),
            )
            .await;
        }

        self.health.update(
            "llama-server",
            crate::infra::health::ServiceStatus::Stopped,
            Some("orchestrator shutdown completed".into()),
        );
        self.health.update(
            "orchestrator",
            crate::infra::health::ServiceStatus::Stopped,
            Some("orchestrator stopped".into()),
        );
    }

    /// Restart the managed llama-server and re-arm watchdog monitoring.
    pub async fn restart(&self, reason: &str) -> anyhow::Result<()> {
        let _lifecycle_guard = self.lifecycle_lock.lock().await;

        let cooldown = Duration::from_secs(self.config.restart_cooldown_secs.max(1));
        {
            let mut last = self.last_restart_at.lock().await;
            if let Some(previous) = *last {
                if previous.elapsed() < cooldown {
                    let remaining_ms = (cooldown - previous.elapsed()).as_millis() as u64;
                    anyhow::bail!(
                        "orchestrator restart cooldown active (remaining {} ms)",
                        remaining_ms
                    );
                }
            }
            *last = Some(Instant::now());
        }

        tracing::warn!(reason, "orchestrator: restart requested");

        self.stop_watchdog().await;

        self.health.update(
            "orchestrator",
            crate::infra::health::ServiceStatus::Starting,
            Some(format!("restarting ({reason})")),
        );
        self.health.update(
            "llama-server",
            crate::infra::health::ServiceStatus::Starting,
            Some("restarting local LLM runtime".into()),
        );

        let (previous_ngl, previous_ctx) = self.server_manager.current_params();

        self.server_manager
            .graceful_stop_with_timeout(Duration::from_secs(
                self.config.graceful_stop_timeout_secs.max(1),
            ))
            .await;

        if self.backend == GpuBackend::Cuda {
            self.wait_for_vram_release_bounded(
                Duration::from_secs(self.config.vram_release_timeout_secs.max(1)),
            )
            .await;
        }

        let snapshot = self.telemetry.snapshot().await;
        let target = strategy::calculate_target_params(
            &self.config.model_profile,
            snapshot.free_vram_mb,
            self.config.safety_margin_mb,
            self.backend,
        );

        let primary = self
            .server_manager
            .spawn(
                target.ngl,
                target.context,
                target.enable_vision,
                self.event_bus.clone(),
            )
            .await;

        let restart_result = match primary {
            Ok(()) => Ok(()),
            Err(primary_error) => {
                tracing::warn!(
                    ?primary_error,
                    "orchestrator: primary restart spawn failed; attempting fallback"
                );

                if self.config.restart_backoff_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(self.config.restart_backoff_ms)).await;
                }

                let fallback_ngl = previous_ngl;
                let fallback_ctx = if previous_ctx > 0 {
                    previous_ctx
                } else {
                    self.config.model_profile.min_context
                };
                let fallback_vision = self.config.model_profile.has_vision_projector && fallback_ngl >= self.config.model_profile.vision_min_ngl;

                self.server_manager
                    .spawn(
                        fallback_ngl,
                        fallback_ctx,
                        fallback_vision,
                        self.event_bus.clone(),
                    )
                    .await
                    .map_err(|fallback_error| {
                        anyhow::anyhow!(
                            "restart failed: primary error: {}; fallback error: {}",
                            primary_error,
                            fallback_error
                        )
                    })
            }
        };

        match restart_result {
            Ok(()) => {
                self.health.update(
                    "llama-server",
                    crate::infra::health::ServiceStatus::Healthy,
                    None,
                );
                self.health.update(
                    "orchestrator",
                    crate::infra::health::ServiceStatus::Healthy,
                    None,
                );
                self.ensure_watchdog_running().await;
                tracing::info!("orchestrator: restart completed");
                Ok(())
            }
            Err(e) => {
                self.health.update(
                    "llama-server",
                    crate::infra::health::ServiceStatus::Degraded,
                    Some(format!("restart failed: {e}")),
                );
                self.health.update(
                    "orchestrator",
                    crate::infra::health::ServiceStatus::Degraded,
                    Some(format!("restart failed: {e}")),
                );
                Err(e)
            }
        }
    }

    /// Ensure llama-server is running and healthy.
    ///
    /// This is used by desktop preflight checks before dispatching a turn,
    /// especially when idle-release has intentionally stopped the runtime.
    pub async fn ensure_ready(&self, reason: &str) -> anyhow::Result<()> {
        let _lifecycle_guard = self.lifecycle_lock.lock().await;

        let has_live_process = self.server_manager.has_live_process().await;
        if has_live_process && self.server_manager.is_healthy() {
            self.ensure_watchdog_running().await;
            return Ok(());
        }

        tracing::info!(
            reason,
            had_live_process = has_live_process,
            state = self.server_manager.state(),
            "orchestrator: ensure_ready starting local runtime"
        );

        self.health.update(
            "orchestrator",
            crate::infra::health::ServiceStatus::Starting,
            Some(format!("ensuring local runtime ({reason})")),
        );
        self.health.update(
            "llama-server",
            crate::infra::health::ServiceStatus::Starting,
            Some("starting local LLM runtime".into()),
        );

        let (previous_ngl, previous_ctx) = self.server_manager.current_params();
        if has_live_process || self.server_manager.state() != server_manager::STATE_STOPPED {
            self.server_manager
                .graceful_stop_with_timeout(Duration::from_secs(
                    self.config.graceful_stop_timeout_secs.max(1),
                ))
                .await;

            if self.backend == GpuBackend::Cuda {
                self.wait_for_vram_release_bounded(Duration::from_secs(
                    self.config.vram_release_timeout_secs.max(1),
                ))
                .await;
            }
        }

        let snapshot = self.telemetry.snapshot().await;
        let target = strategy::calculate_target_params(
            &self.config.model_profile,
            snapshot.free_vram_mb,
            self.config.safety_margin_mb,
            self.backend,
        );

        let primary = self
            .server_manager
            .spawn(
                target.ngl,
                target.context,
                target.enable_vision,
                self.event_bus.clone(),
            )
            .await;

        let ensure_result = match primary {
            Ok(()) => Ok(()),
            Err(primary_error) => {
                tracing::warn!(
                    ?primary_error,
                    "orchestrator: ensure_ready primary spawn failed; attempting fallback"
                );

                if self.config.restart_backoff_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(self.config.restart_backoff_ms)).await;
                }

                let fallback_ngl = previous_ngl;
                let fallback_ctx = if previous_ctx > 0 {
                    previous_ctx
                } else {
                    self.config.model_profile.min_context
                };
                let fallback_vision =
                    self.config.model_profile.has_vision_projector && fallback_ngl >= self.config.model_profile.vision_min_ngl;

                self.server_manager
                    .spawn(
                        fallback_ngl,
                        fallback_ctx,
                        fallback_vision,
                        self.event_bus.clone(),
                    )
                    .await
                    .map_err(|fallback_error| {
                        anyhow::anyhow!(
                            "ensure_ready failed: primary error: {}; fallback error: {}",
                            primary_error,
                            fallback_error
                        )
                    })
            }
        };

        match ensure_result {
            Ok(()) => {
                self.health.update(
                    "llama-server",
                    crate::infra::health::ServiceStatus::Healthy,
                    None,
                );
                self.health.update(
                    "orchestrator",
                    crate::infra::health::ServiceStatus::Healthy,
                    None,
                );
                self.ensure_watchdog_running().await;
                Ok(())
            }
            Err(e) => {
                self.health.update(
                    "llama-server",
                    crate::infra::health::ServiceStatus::Degraded,
                    Some(format!("ensure_ready failed: {e}")),
                );
                self.health.update(
                    "orchestrator",
                    crate::infra::health::ServiceStatus::Degraded,
                    Some(format!("ensure_ready failed: {e}")),
                );
                Err(e)
            }
        }
    }

    /// Release llama-server when the desktop runtime is idle.
    ///
    /// Returns true if a running process was released, false when there was
    /// nothing to release.
    pub async fn release_if_idle(&self, reason: &str) -> anyhow::Result<bool> {
        let _lifecycle_guard = self.lifecycle_lock.lock().await;

        if !self.server_manager.has_live_process().await {
            return Ok(false);
        }

        tracing::info!(reason, "orchestrator: idle release requested");
        self.stop_watchdog().await;

        self.server_manager
            .graceful_stop_with_timeout(Duration::from_secs(
                self.config.graceful_stop_timeout_secs.max(1),
            ))
            .await;

        if self.backend == GpuBackend::Cuda {
            self.wait_for_vram_release_bounded(Duration::from_secs(
                self.config.vram_release_timeout_secs.max(1),
            ))
            .await;
        }

        self.health.update(
            "llama-server",
            crate::infra::health::ServiceStatus::Stopped,
            Some("released while idle; will warm on next turn".into()),
        );
        self.health.update(
            "orchestrator",
            crate::infra::health::ServiceStatus::Healthy,
            Some("idle release active".into()),
        );

        Ok(true)
    }

    async fn stop_watchdog(&self) {
        if let Some(handle) = self.watchdog_handle.lock().await.take() {
            handle.abort();
        }
    }

    async fn ensure_watchdog_running(&self) {
        let mut lock = self.watchdog_handle.lock().await;
        let should_restart = lock.as_ref().map(|h| h.is_finished()).unwrap_or(true);
        if !should_restart {
            return;
        }

        let watchdog = gpu_watchdog::GpuWatchdog::new(
            self.config.clone(),
            self.backend,
            self.telemetry.clone(),
            self.server_manager.clone(),
            self.event_bus.clone(),
        );

        *lock = Some(tokio::spawn(async move {
            watchdog.run().await;
        }));
    }

    async fn wait_for_vram_release_bounded(&self, timeout: Duration) {
        let start = Instant::now();

        loop {
            if start.elapsed() > timeout {
                tracing::warn!(
                    timeout_secs = timeout.as_secs(),
                    "orchestrator: VRAM release wait timed out"
                );
                break;
            }

            let snap = self.telemetry.snapshot().await;
            if snap.free_vram_mb > self.config.yield_threshold_mb {
                tracing::debug!(
                    free_mb = snap.free_vram_mb,
                    "orchestrator: VRAM release observed"
                );
                break;
            }

            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

impl Drop for Orchestrator {
    fn drop(&mut self) {
        if let Ok(mut lock) = self.watchdog_handle.try_lock() {
            if let Some(handle) = lock.take() {
                handle.abort();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::health::ServiceStatus;
    use async_trait::async_trait;

    struct TestTelemetry;

    #[async_trait]
    impl telemetry::GpuTelemetry for TestTelemetry {
        async fn snapshot(&self) -> telemetry::TelemetrySnapshot {
            telemetry::TelemetrySnapshot {
                free_vram_mb: 4096,
                total_vram_mb: 8192,
                gpu_util_pct: Some(10),
            }
        }

        fn source_name(&self) -> &'static str {
            "test"
        }
    }

    fn build_test_orchestrator(config: OrchestratorConfig) -> Orchestrator {
        let health = Arc::new(HealthRegistry::new());
        health.register("llama-server");
        health.register("orchestrator");

        Orchestrator {
            config: config.clone(),
            backend: GpuBackend::CpuOnly,
            server_manager: Arc::new(server_manager::LlamaServerManager::new(
                config,
                "/tmp/kria_missing_model.gguf".into(),
                None,
            )),
            telemetry: Arc::new(TestTelemetry),
            event_bus: Arc::new(EventBus::new(16)),
            health,
            watchdog_handle: Mutex::new(None),
            lifecycle_lock: Mutex::new(()),
            last_restart_at: Mutex::new(None),
            _telemetry_actor: None,
        }
    }

    #[tokio::test]
    async fn release_if_idle_returns_false_without_live_process() {
        let orchestrator = build_test_orchestrator(OrchestratorConfig::default());

        let released = orchestrator
            .release_if_idle("unit_test_no_process")
            .await
            .expect("release_if_idle should not error when process is absent");

        assert!(!released);
    }

    #[tokio::test]
    async fn ensure_ready_marks_health_degraded_on_spawn_failure() {
        let mut config = OrchestratorConfig::default();
        config.health_check_timeout_secs = 1;
        config.port_discovery_timeout_secs = 1;

        let orchestrator = build_test_orchestrator(config);

        let result = orchestrator.ensure_ready("unit_test_failure").await;
        assert!(result.is_err(), "ensure_ready should fail without a valid model/runtime");

        let llama_health = orchestrator
            .health
            .get("llama-server")
            .expect("llama-server health should be registered");
        let orchestrator_health = orchestrator
            .health
            .get("orchestrator")
            .expect("orchestrator health should be registered");

        assert_eq!(llama_health.status, ServiceStatus::Degraded);
        assert_eq!(orchestrator_health.status, ServiceStatus::Degraded);
        assert!(
            llama_health
                .message
                .unwrap_or_default()
                .contains("ensure_ready failed"),
            "llama health message should include ensure_ready failure context"
        );
    }
}
