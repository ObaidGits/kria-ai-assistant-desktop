//! GPU Watchdog — telemetry polling loop with hysteresis-based state machine.
//!
//! States: Idle → Yielding → Swapping → Cooldown → Idle
//! Emergency path: any state → Emergency → Swapping (bypass hysteresis)

use crate::config::OrchestratorConfig;
use crate::infra::event_bus::{EventBus, KriaEvent};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::server_manager::LlamaServerManager;
use super::strategy;
use super::telemetry::GpuTelemetry;
use super::GpuBackend;

/// Watchdog state machine states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WatchdogState {
    /// Normal operation — polling telemetry.
    Idle,
    /// VRAM pressure detected, waiting for sustained threshold breach.
    Yielding { since: Instant },
    /// Post-swap cooldown to prevent thrashing.
    Cooldown { until: Instant },
}

pub struct GpuWatchdog {
    config: OrchestratorConfig,
    backend: GpuBackend,
    telemetry: Arc<dyn GpuTelemetry>,
    server: Arc<LlamaServerManager>,
    event_bus: Arc<EventBus>,
}

impl GpuWatchdog {
    pub fn new(
        config: OrchestratorConfig,
        backend: GpuBackend,
        telemetry: Arc<dyn GpuTelemetry>,
        server: Arc<LlamaServerManager>,
        event_bus: Arc<EventBus>,
    ) -> Self {
        Self {
            config,
            backend,
            telemetry,
            server,
            event_bus,
        }
    }

    /// Main watchdog loop. Runs until the task is aborted.
    pub async fn run(&self) {
        let poll_interval = Duration::from_secs(self.config.poll_interval_secs);
        let mut state = WatchdogState::Idle;
        let mut transitions_this_hour: Vec<Instant> = Vec::new();

        // On macOS Metal, the watchdog is less aggressive since there's no
        // discrete VRAM to compete for — only RAM pressure matters.
        let (yield_threshold, emergency_threshold, recover_threshold) =
            if self.backend == GpuBackend::Metal {
                (
                    self.config.macos_yield_ram_mb,
                    self.config.macos_emergency_ram_mb,
                    self.config.macos_recover_ram_mb,
                )
            } else {
                (
                    self.config.yield_threshold_mb,
                    self.config.emergency_threshold_mb,
                    self.config.recover_threshold_mb,
                )
            };

        tracing::info!(
            backend = ?self.backend,
            telemetry = self.telemetry.source_name(),
            yield_mb = yield_threshold,
            emergency_mb = emergency_threshold,
            recover_mb = recover_threshold,
            "watchdog: starting telemetry loop"
        );

        loop {
            tokio::time::sleep(poll_interval).await;

            let snap = self.telemetry.snapshot().await;
            let free = snap.free_vram_mb;

            // Prune old transitions (sliding 1-hour window)
            let one_hour_ago = Instant::now() - Duration::from_secs(3600);
            transitions_this_hour.retain(|t| *t > one_hour_ago);

            // Emergency path: bypass all hysteresis (skip if in cooldown)
            if free < emergency_threshold && !matches!(state, WatchdogState::Cooldown { .. }) {
                tracing::warn!(
                    free_mb = free,
                    threshold = emergency_threshold,
                    "watchdog: EMERGENCY — VRAM critically low"
                );

                self.event_bus.publish(KriaEvent::VramPressure {
                    free_vram_mb: free,
                });

                self.handle_swap(free, true, &mut transitions_this_hour)
                    .await;
                state = WatchdogState::Cooldown {
                    until: Instant::now() + Duration::from_secs(self.config.cooldown_secs),
                };
                continue;
            }

            state = match state {
                WatchdogState::Idle => {
                    if free < yield_threshold {
                        tracing::info!(
                            free_mb = free,
                            threshold = yield_threshold,
                            "watchdog: VRAM pressure detected, entering yield state"
                        );
                        WatchdogState::Yielding {
                            since: Instant::now(),
                        }
                    } else if free > recover_threshold {
                        // Check if we can recover (scale up)
                        self.try_recover(free).await;
                        WatchdogState::Idle
                    } else {
                        WatchdogState::Idle
                    }
                }

                WatchdogState::Yielding { since } => {
                    if free >= yield_threshold {
                        // Pressure relieved
                        tracing::info!("watchdog: VRAM pressure relieved, returning to idle");
                        WatchdogState::Idle
                    } else if since.elapsed() > Duration::from_secs(5) {
                        // Sustained pressure — trigger swap
                        tracing::warn!(
                            free_mb = free,
                            sustained_secs = since.elapsed().as_secs(),
                            "watchdog: sustained VRAM pressure, initiating swap"
                        );

                        self.event_bus.publish(KriaEvent::VramPressure {
                            free_vram_mb: free,
                        });

                        // Rate limit check
                        if transitions_this_hour.len() as u32
                            >= self.config.max_transitions_per_hour
                        {
                            tracing::warn!(
                                count = transitions_this_hour.len(),
                                max = self.config.max_transitions_per_hour,
                                "watchdog: transition rate limit reached, staying idle"
                            );
                            WatchdogState::Cooldown {
                                until: Instant::now()
                                    + Duration::from_secs(self.config.cooldown_secs),
                            }
                        } else {
                            self.handle_swap(free, false, &mut transitions_this_hour)
                                .await;
                            WatchdogState::Cooldown {
                                until: Instant::now()
                                    + Duration::from_secs(self.config.cooldown_secs),
                            }
                        }
                    } else {
                        // Still in hysteresis window
                        WatchdogState::Yielding { since }
                    }
                }

                WatchdogState::Cooldown { until } => {
                    if Instant::now() >= until {
                        tracing::info!("watchdog: cooldown expired, returning to idle");
                        WatchdogState::Idle
                    } else {
                        WatchdogState::Cooldown { until }
                    }
                }
            };
        }
    }

    /// Execute a swap: calculate new params, kill old server, spawn new one.
    async fn handle_swap(
        &self,
        free_vram_mb: u64,
        emergency: bool,
        transitions: &mut Vec<Instant>,
    ) {
        let (old_ngl, _old_ctx) = self.server.current_params();

        let new_params = strategy::calculate_target_params(
            &self.config.model_profile,
            free_vram_mb,
            self.config.safety_margin_mb,
            self.backend,
        );

        // Check minimum delta to avoid micro-adjustments (skip for emergency)
        if !emergency {
            let delta = (old_ngl as i64 - new_params.ngl as i64).unsigned_abs() as u32;
            if delta < self.config.min_ngl_delta {
                tracing::debug!(
                    old_ngl,
                    new_ngl = new_params.ngl,
                    delta,
                    min_delta = self.config.min_ngl_delta,
                    "watchdog: ngl delta too small, skipping swap"
                );
                return;
            }
        }

        tracing::info!(
            old_ngl,
            new_ngl = new_params.ngl,
            new_ctx = new_params.context,
            emergency,
            degradation = %new_params.degradation,
            "watchdog: executing swap"
        );

        self.event_bus.publish(KriaEvent::LlmSwapStarted {
            from_ngl: old_ngl,
            to_ngl: new_params.ngl,
            emergency,
        });

        let swap_start = Instant::now();

        // Cancel in-flight streams
        self.server.cancel_streams();

        self.event_bus.publish(KriaEvent::LlmStreamInterrupted);

        // Kill old server (emergency = immediate kill, yield = graceful)
        if emergency {
            self.server.kill().await;
        } else {
            self.server.graceful_stop().await;
        }

        // On CUDA, poll for VRAM to actually free (ghost prevention)
        if self.backend == GpuBackend::Cuda {
            self.wait_for_vram_release().await;
        }

        // Spawn new server with calculated params
        match self
            .server
            .spawn(
                new_params.ngl,
                new_params.context,
                new_params.enable_vision,
                self.event_bus.clone(),
            )
            .await
        {
            Ok(()) => {
                let duration = swap_start.elapsed();
                transitions.push(Instant::now());

                self.event_bus.publish(KriaEvent::LlmSwapCompleted {
                    new_ngl: new_params.ngl,
                    new_context: new_params.context,
                    duration_ms: duration.as_millis() as u64,
                });

                self.event_bus.publish(KriaEvent::LlmDegradationChanged {
                    level: new_params.degradation.as_str().to_string(),
                });

                tracing::info!(
                    ngl = new_params.ngl,
                    ctx = new_params.context,
                    duration_ms = duration.as_millis(),
                    "watchdog: swap completed"
                );
            }
            Err(e) => {
                tracing::error!(?e, "watchdog: failed to spawn new server after swap");
                // Try emergency CPU-only fallback
                let _ = self
                    .server
                    .spawn(0, self.config.model_profile.min_context, false, self.event_bus.clone())
                    .await;
            }
        }
    }

    /// Try to recover (scale up) when VRAM is abundant.
    async fn try_recover(&self, free_vram_mb: u64) {
        let (current_ngl, current_ctx) = self.server.current_params();

        let optimal = strategy::calculate_target_params(
            &self.config.model_profile,
            free_vram_mb,
            self.config.safety_margin_mb,
            self.backend,
        );

        // Only recover if there's a meaningful improvement
        let ngl_gain = optimal.ngl.saturating_sub(current_ngl);
        if ngl_gain < self.config.min_ngl_delta
            && optimal.context <= current_ctx
        {
            return;
        }

        tracing::info!(
            current_ngl,
            optimal_ngl = optimal.ngl,
            current_ctx,
            optimal_ctx = optimal.context,
            "watchdog: recovery opportunity detected"
        );

        // Recovery uses the yield (graceful) path
        self.event_bus.publish(KriaEvent::LlmSwapStarted {
            from_ngl: current_ngl,
            to_ngl: optimal.ngl,
            emergency: false,
        });

        let swap_start = Instant::now();

        self.server.cancel_streams();
        self.event_bus.publish(KriaEvent::LlmStreamInterrupted);
        self.server.graceful_stop().await;

        match self
            .server
            .spawn(
                optimal.ngl,
                optimal.context,
                optimal.enable_vision,
                self.event_bus.clone(),
            )
            .await
        {
            Ok(()) => {
                let duration = swap_start.elapsed();
                self.event_bus.publish(KriaEvent::LlmSwapCompleted {
                    new_ngl: optimal.ngl,
                    new_context: optimal.context,
                    duration_ms: duration.as_millis() as u64,
                });
                self.event_bus.publish(KriaEvent::LlmDegradationChanged {
                    level: optimal.degradation.as_str().to_string(),
                });
                tracing::info!(
                    ngl = optimal.ngl,
                    ctx = optimal.context,
                    duration_ms = duration.as_millis(),
                    "watchdog: recovery completed"
                );
            }
            Err(e) => {
                tracing::error!(?e, "watchdog: recovery spawn failed, reverting");
                // Revert to previous params
                let _ = self
                    .server
                    .spawn(current_ngl, current_ctx, current_ngl >= 15, self.event_bus.clone())
                    .await;
            }
        }
    }

    /// After killing llama-server, VRAM may not free immediately (CUDA ghost).
    /// Poll NVML/CLI until VRAM actually drops or timeout.
    async fn wait_for_vram_release(&self) {
        let start = Instant::now();
        let timeout = Duration::from_secs(5);

        loop {
            if start.elapsed() > timeout {
                tracing::warn!("watchdog: VRAM release timeout after 5s");
                break;
            }

            let snap = self.telemetry.snapshot().await;
            // If we see a meaningful jump in free VRAM, the old process released
            if snap.free_vram_mb > self.config.yield_threshold_mb {
                tracing::debug!(
                    free_mb = snap.free_vram_mb,
                    "watchdog: VRAM released"
                );
                break;
            }

            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}
