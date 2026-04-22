//! GPU Watchdog — telemetry polling loop with hysteresis-based state machine.
//!
//! # States
//!
//! ```text
//! Idle ──────────────────────────────────► Pressured(since, target)
//!  ▲  EMA-V < yield_threshold (dwell ok)       │
//!  │                                            │ sustained ≥ pressure_dwell_secs
//!  │                                            │ AND rate-limit budget OK
//!  │                                            ▼
//!  │                                       Swapping ──► Cooldown(until)
//!  │                                                         │
//!  │                                                         │ until elapsed
//!  │                                                         ▼
//!  │                          EMA-V > recover_threshold ─► Recovering(since)
//!  │                                                         │
//!  └─────────────── recovery_dwell_secs elapsed ◄───────────┘
//!
//! Any state → Critical(since) when EMA-V < emergency_threshold
//!                               (for ≥ emergency_dwell_ms)
//! Critical → Swapping (emergency, separate rate budget)
//! ```
//!
//! # Anti-thrash guarantees
//! - **EMA debouncing**: single-sample spikes don't trigger transitions.
//! - **Hysteresis band**: exit from Pressured requires
//!   `EMA-V > yield_threshold + hysteresis_band_mb` (256MB deadband by default).
//! - **Separate emergency budget**: emergency path still self-throttles.
//! - **Hard dwell cap**: any state held > `state_max_dwell_secs` forces a
//!   resync + warning log.
//! - **Pre-computed target**: `TargetParams` calculated on entering Pressured
//!   so the Swapping phase only does I/O.
//! - **Asymmetric delta**: scale-up requires `min_ngl_delta_up`; scale-down
//!   requires `min_ngl_delta` (smaller → more responsive under pressure).

use crate::config::OrchestratorConfig;
use crate::infra::event_bus::{EventBus, KriaEvent};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::server_manager::LlamaServerManager;
use super::strategy::{self, TargetParams};
use super::telemetry::GpuTelemetry;
use super::GpuBackend;

// ── EMA helper ───────────────────────────────────────────────────────

/// Three-sample exponential moving average over free VRAM.
/// Smooths single-poll transient dips/spikes so they don't trigger swaps.
struct VramEma {
    value: Option<f64>,
    alpha: f64, // smoothing factor (0 < α ≤ 1; higher = less smoothing)
}

impl VramEma {
    fn new(alpha: f64) -> Self {
        Self { value: None, alpha }
    }

    fn update(&mut self, sample: u64) -> u64 {
        let s = sample as f64;
        let ema = match self.value {
            None => s,
            Some(prev) => self.alpha * s + (1.0 - self.alpha) * prev,
        };
        self.value = Some(ema);
        ema as u64
    }
}

// ── State machine ────────────────────────────────────────────────────

/// Watchdog state machine states.
#[derive(Debug, Clone)]
enum WatchdogState {
    /// Normal operation — polling telemetry, no pressure.
    Idle { since: Instant },

    /// VRAM pressure detected. Waits for sustained breach before swapping.
    /// `target` is pre-computed so Swapping only does I/O.
    Pressured {
        since: Instant,
        target: Box<TargetParams>,
    },

    /// Post-swap cooldown to prevent thrashing.
    Cooldown { until: Instant },

    /// VRAM is recovering (above recover_threshold). Waits for stability
    /// before triggering a scale-up swap.
    Recovering {
        since: Instant,
        target: Box<TargetParams>,
    },

    /// VRAM critically low — emergency swap path.
    Critical { since: Instant },
}

impl WatchdogState {
    fn name(&self) -> &'static str {
        match self {
            Self::Idle { .. } => "idle",
            Self::Pressured { .. } => "pressured",
            Self::Cooldown { .. } => "cooldown",
            Self::Recovering { .. } => "recovering",
            Self::Critical { .. } => "critical",
        }
    }

    fn entered_at(&self) -> Instant {
        match self {
            Self::Idle { since }
            | Self::Pressured { since, .. }
            | Self::Recovering { since, .. }
            | Self::Critical { since } => *since,
            Self::Cooldown { until } => *until - Duration::from_secs(0), // approximate
        }
    }
}

// ── Sliding rate-limit window ────────────────────────────────────────

struct RateBucket {
    timestamps: Vec<Instant>,
    limit: u32,
}

impl RateBucket {
    fn new(limit: u32) -> Self {
        Self {
            timestamps: Vec::new(),
            limit,
        }
    }

    fn prune(&mut self) {
        let hour_ago = Instant::now() - Duration::from_secs(3600);
        self.timestamps.retain(|t| *t > hour_ago);
    }

    fn has_budget(&mut self) -> bool {
        self.prune();
        (self.timestamps.len() as u32) < self.limit
    }

    fn record(&mut self) {
        self.timestamps.push(Instant::now());
    }
}

// ── GpuWatchdog ──────────────────────────────────────────────────────

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
        let poll_interval = Duration::from_secs(self.config.poll_interval_secs.max(1));

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

        let hysteresis = self.config.hysteresis_band_mb;
        let pressure_dwell = Duration::from_secs(self.config.pressure_dwell_secs);
        let emergency_dwell = Duration::from_millis(self.config.emergency_dwell_ms);
        let recovery_dwell = Duration::from_secs(self.config.recovery_dwell_secs);
        let state_max_dwell = Duration::from_secs(self.config.state_max_dwell_secs);
        let cooldown_dur = Duration::from_secs(self.config.cooldown_secs);

        // EMA with α = 0.5 → roughly 3-sample smoothing.
        let mut ema = VramEma::new(0.5);

        let mut state = WatchdogState::Idle {
            since: Instant::now(),
        };

        // Separate rate buckets: normal and emergency.
        let mut normal_budget = RateBucket::new(self.config.max_transitions_per_hour);
        let mut emergency_budget =
            RateBucket::new(self.config.max_emergency_transitions_per_hour.max(1));

        tracing::info!(
            backend = ?self.backend,
            telemetry = self.telemetry.source_name(),
            yield_mb = yield_threshold,
            emergency_mb = emergency_threshold,
            recover_mb = recover_threshold,
            hysteresis_mb = hysteresis,
            pressure_dwell_secs = self.config.pressure_dwell_secs,
            emergency_dwell_ms = self.config.emergency_dwell_ms,
            "watchdog: starting"
        );

        loop {
            tokio::time::sleep(poll_interval).await;

            let raw = self.telemetry.snapshot().await.free_vram_mb;
            let free = ema.update(raw);

            let state_name = state.name();
            let state_age = state.entered_at().elapsed();

            // Hard dwell cap: if we've been in any state too long, log and reset.
            if state_age > state_max_dwell {
                tracing::warn!(
                    state = state_name,
                    age_secs = state_age.as_secs(),
                    max_secs = state_max_dwell.as_secs(),
                    "watchdog: state dwell cap exceeded — resetting to Idle"
                );
                state = WatchdogState::Idle {
                    since: Instant::now(),
                };
            }

            // Critical check overlay: any non-Cooldown state can transition
            // to Critical when EMA drops below emergency threshold for
            // ≥ emergency_dwell_ms. This prevents false triggers from driver
            // spikes.
            if free < emergency_threshold && !matches!(state, WatchdogState::Cooldown { .. }) {
                state = match state {
                    WatchdogState::Critical { since } => {
                        if since.elapsed() >= emergency_dwell {
                            if emergency_budget.has_budget() {
                                tracing::warn!(
                                    free_mb = free,
                                    elapsed_ms = since.elapsed().as_millis(),
                                    "watchdog: EMERGENCY — triggering swap"
                                );
                                self.event_bus.publish(KriaEvent::VramPressure {
                                    free_vram_mb: free,
                                });
                                self.execute_swap(free, true, &mut emergency_budget)
                                    .await;
                                WatchdogState::Cooldown {
                                    until: Instant::now() + cooldown_dur,
                                }
                            } else {
                                tracing::warn!(
                                    "watchdog: emergency rate limit reached — staying critical"
                                );
                                WatchdogState::Critical { since }
                            }
                        } else {
                            WatchdogState::Critical { since }
                        }
                    }
                    _ => {
                        tracing::warn!(
                            free_mb = free,
                            threshold = emergency_threshold,
                            prev_state = state_name,
                            "watchdog: entering Critical"
                        );
                        WatchdogState::Critical {
                            since: Instant::now(),
                        }
                    }
                };
                continue;
            }

            // Main state transitions.
            state = match state {
                WatchdogState::Idle { since } => {
                    if free < yield_threshold {
                        // Pre-compute target here so Swapping only does I/O.
                        let target = strategy::calculate_target_params(
                            &self.config.model_profile,
                            free,
                            self.config.safety_margin_mb,
                            self.backend,
                        );
                        tracing::info!(
                            free_mb = free,
                            threshold = yield_threshold,
                            new_ngl = target.ngl,
                            "watchdog: VRAM pressure — entering Pressured"
                        );
                        WatchdogState::Pressured {
                            since: Instant::now(),
                            target: Box::new(target),
                        }
                    } else if free > recover_threshold + hysteresis {
                        // Recovery path: check if a scale-up makes sense.
                        let (current_ngl, _) = self.server.current_params();
                        let target = strategy::calculate_target_params(
                            &self.config.model_profile,
                            free,
                            self.config.safety_margin_mb,
                            self.backend,
                        );
                        let delta =
                            target.ngl.saturating_sub(current_ngl);
                        if delta >= self.config.min_ngl_delta_up
                            && normal_budget.has_budget()
                        {
                            tracing::info!(
                                free_mb = free,
                                delta_ngl = delta,
                                "watchdog: recovery headroom — entering Recovering"
                            );
                            WatchdogState::Recovering {
                                since: Instant::now(),
                                target: Box::new(target),
                            }
                        } else {
                            WatchdogState::Idle { since }
                        }
                    } else {
                        WatchdogState::Idle { since }
                    }
                }

                WatchdogState::Pressured { since, target } => {
                    // Exit: pressure relieved (deadband).
                    if free > yield_threshold + hysteresis {
                        tracing::info!(
                            free_mb = free,
                            "watchdog: pressure relieved — returning to Idle"
                        );
                        WatchdogState::Idle {
                            since: Instant::now(),
                        }
                    } else if since.elapsed() >= pressure_dwell {
                        // Sustained pressure — check rate limit, then swap.
                        if normal_budget.has_budget() {
                            let (current_ngl, _) = self.server.current_params();
                            let delta = (current_ngl as i64 - target.ngl as i64)
                                .unsigned_abs() as u32;

                            if delta < self.config.min_ngl_delta {
                                tracing::debug!(
                                    delta,
                                    min = self.config.min_ngl_delta,
                                    "watchdog: delta too small, skipping swap"
                                );
                                WatchdogState::Cooldown {
                                    until: Instant::now() + cooldown_dur,
                                }
                            } else {
                                tracing::warn!(
                                    free_mb = free,
                                    new_ngl = target.ngl,
                                    "watchdog: sustained pressure — swapping"
                                );
                                self.event_bus.publish(KriaEvent::VramPressure {
                                    free_vram_mb: free,
                                });
                                self.execute_swap_with_target(&target, false, &mut normal_budget)
                                    .await;
                                WatchdogState::Cooldown {
                                    until: Instant::now() + cooldown_dur,
                                }
                            }
                        } else {
                            tracing::warn!(
                                "watchdog: normal rate limit reached — entering Cooldown"
                            );
                            WatchdogState::Cooldown {
                                until: Instant::now() + cooldown_dur,
                            }
                        }
                    } else {
                        WatchdogState::Pressured { since, target }
                    }
                }

                WatchdogState::Cooldown { until } => {
                    if Instant::now() >= until {
                        tracing::info!("watchdog: cooldown expired");
                        WatchdogState::Idle {
                            since: Instant::now(),
                        }
                    } else {
                        WatchdogState::Cooldown { until }
                    }
                }

                WatchdogState::Recovering { since, target } => {
                    if since.elapsed() >= recovery_dwell {
                        tracing::info!(
                            new_ngl = target.ngl,
                            "watchdog: recovery stable — scaling up"
                        );
                        self.execute_swap_with_target(&target, false, &mut normal_budget)
                            .await;
                        WatchdogState::Cooldown {
                            until: Instant::now() + cooldown_dur,
                        }
                    } else if free < recover_threshold {
                        // Recovery window closed before dwell expired.
                        tracing::info!("watchdog: recovery window closed — returning to Idle");
                        WatchdogState::Idle {
                            since: Instant::now(),
                        }
                    } else {
                        WatchdogState::Recovering { since, target }
                    }
                }

                WatchdogState::Critical { since } => {
                    // If we get here, free >= emergency_threshold (the Critical
                    // overlay above didn't fire). Transition back to Idle.
                    tracing::info!(
                        elapsed_ms = since.elapsed().as_millis(),
                        "watchdog: critical pressure resolved — returning to Idle"
                    );
                    WatchdogState::Idle {
                        since: Instant::now(),
                    }
                }
            };
        }
    }

    /// Execute a swap using a freshly calculated target (emergency path).
    async fn execute_swap(
        &self,
        free_vram_mb: u64,
        emergency: bool,
        budget: &mut RateBucket,
    ) {
        let target = strategy::calculate_target_params(
            &self.config.model_profile,
            free_vram_mb,
            self.config.safety_margin_mb,
            self.backend,
        );
        self.execute_swap_with_target(&target, emergency, budget)
            .await;
    }

    /// Execute a swap using a pre-computed target.
    async fn execute_swap_with_target(
        &self,
        target: &TargetParams,
        emergency: bool,
        budget: &mut RateBucket,
    ) {
        let (old_ngl, _) = self.server.current_params();

        tracing::info!(
            old_ngl,
            new_ngl = target.ngl,
            new_ctx = target.context,
            emergency,
            degradation = %target.degradation,
            "watchdog: executing swap"
        );

        self.event_bus.publish(KriaEvent::LlmSwapStarted {
            from_ngl: old_ngl,
            to_ngl: target.ngl,
            emergency,
        });

        let swap_start = Instant::now();

        // 1. Cancel in-flight streams.
        self.server.cancel_streams();
        self.event_bus.publish(KriaEvent::LlmStreamInterrupted);

        // 2. Kill old server (emergency = immediate, normal = graceful).
        if emergency {
            self.server.kill().await;
        } else {
            self.server.graceful_stop().await;
        }

        // 3. On CUDA: wait for VRAM to actually free before spawning. Use the
        //    watch-channel snapshot (not a fresh blocking NVML call) to poll.
        if self.backend == GpuBackend::Cuda {
            self.wait_for_vram_release().await;
        }

        // 4. Spawn new server.
        match self
            .server
            .spawn(
                target.ngl,
                target.context,
                target.enable_vision,
                self.event_bus.clone(),
            )
            .await
        {
            Ok(()) => {
                let duration = swap_start.elapsed();
                budget.record();

                self.event_bus.publish(KriaEvent::LlmSwapCompleted {
                    new_ngl: target.ngl,
                    new_context: target.context,
                    duration_ms: duration.as_millis() as u64,
                });

                self.event_bus.publish(KriaEvent::LlmDegradationChanged {
                    level: target.degradation.as_str().to_string(),
                });

                tracing::info!(
                    new_ngl = target.ngl,
                    duration_ms = duration.as_millis(),
                    "watchdog: swap completed"
                );
            }
            Err(e) => {
                tracing::error!(?e, "watchdog: swap spawn failed");
                self.event_bus.publish(KriaEvent::LlmSwapFailed {
                    reason: e.to_string(),
                });
            }
        }
    }

    /// Poll until free VRAM rises above `yield_threshold` or timeout elapses.
    /// Uses the watch-channel telemetry snapshot — never blocks the executor.
    async fn wait_for_vram_release(&self) {
        let timeout =
            Duration::from_secs(self.config.vram_release_timeout_secs.max(1));
        let deadline = Instant::now() + timeout;

        loop {
            if Instant::now() >= deadline {
                tracing::warn!("watchdog: VRAM release wait timed out");
                break;
            }
            let snap = self.telemetry.snapshot().await;
            if snap.free_vram_mb > self.config.yield_threshold_mb {
                tracing::debug!(
                    free_mb = snap.free_vram_mb,
                    "watchdog: VRAM release confirmed"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}
