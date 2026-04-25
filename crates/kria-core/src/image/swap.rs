//! Tier B drop-and-swap coordinator.
//!
//! ## Flow
//! 1. `AudioFreezeGuard::new()` — pauses VAD/STT path (wake-word tap stays live)
//! 2. `EvictionToken::acquire()` — hard-restarts llama-server with `--n-gpu-layers 0`
//! 3. `VramBarrier` waits for 3 consecutive stable samples above threshold
//! 4. Caller runs the ComfyUI job
//! 5. `EvictionToken::drop()` — RAII hard-restarts llama-server back on GPU
//! 6. Eager warmup: 1-token completion to prime KV cache (inside audio blackout)
//! 7. `AudioFreezeGuard::drop()` — resumes VAD/STT path
//!
//! ## Why a hard restart instead of `POST /props`?
//! Modern llama.cpp builds do not implement dynamic `n_gpu_layers` mutation
//! (the request returns HTTP 501 Not Implemented). GPU layers are baked in
//! at process start via the `--n-gpu-layers` CLI flag, so the only reliable
//! way to free VRAM without leaking the model is a SIGTERM + respawn cycle.
//! Conversational context is preserved best-effort by saving / restoring
//! the slot KV cache via `POST /slots/{id}?action=save|restore`.

use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tracing::{info, warn};

use crate::platform::vram::{VramBarrier, VramProfiler};
use crate::voice::capture::AudioCaptureHandle;

// ─── LlmEvictionController ───────────────────────────────────────────────────

/// Abstraction over the LLM hardware orchestrator used by the image swap
/// path. The image module deliberately depends on this narrow surface (and
/// not on the full `llm::orchestrator::Orchestrator`) so it stays decoupled
/// from llama-server lifecycle internals — and so unit tests can stub it.
#[async_trait]
pub trait LlmEvictionController: Send + Sync {
    /// Whether an LLM is currently running with GPU layers loaded. When
    /// `false`, the swap path can skip eviction entirely.
    fn is_gpu_resident(&self) -> bool;

    /// Hard-restart the llama-server process with `--n-gpu-layers 0` so all
    /// model weights live in CPU RAM. Best-effort persists the slot-0 KV
    /// cache before stop and reloads it after restart.
    ///
    /// On success the implementation guarantees:
    /// * the previous llama-server process has been reaped, and
    /// * a new llama-server is healthy and answering on the same logical
    ///   API (the manager hides the ephemeral port change).
    async fn evict_to_cpu(&self) -> Result<(), String>;

    /// Inverse of `evict_to_cpu`: hard-restart back onto GPU using the
    /// previously recorded `(ngl, ctx)` clamped to currently free VRAM.
    /// Best-effort restores the saved slot-0 KV cache after restart.
    async fn restore_from_cpu(&self) -> Result<(), String>;
}

#[derive(Debug, thiserror::Error)]
pub enum SwapError {
    #[error("LLM eviction failed after {attempts} attempt(s): {reason}")]
    EvictionFailed { attempts: u32, reason: String },
    #[error("VRAM barrier timeout: free={free_mb}MB required={required_mb}MB")]
    VramTimeout { free_mb: u64, required_mb: u64 },
    #[error("Cancelled by user")]
    Cancelled,
}

// ─── AudioFreezeGuard ─────────────────────────────────────────────────────────

/// RAII guard that pauses the VAD/STT audio pipeline while held.
///
/// Wake-word detection uses a split tap and is NOT paused — the user can
/// still say "Hey Ria" during image generation.
pub struct AudioFreezeGuard {
    handle: Arc<AudioCaptureHandle>,
    #[allow(dead_code)]
    vad_reset_fn: Box<dyn FnOnce() + Send + Sync>,
}

impl AudioFreezeGuard {
    pub fn new(
        handle: Arc<AudioCaptureHandle>,
        vad_reset: impl FnOnce() + Send + Sync + 'static,
    ) -> Self {
        handle.pause();
        info!("AudioFreezeGuard: VAD/STT path paused");
        Self {
            handle,
            vad_reset_fn: Box::new(vad_reset),
        }
    }
}

impl Drop for AudioFreezeGuard {
    fn drop(&mut self) {
        self.handle.resume();
        info!("AudioFreezeGuard: VAD/STT path resumed");
        // vad_reset_fn is consumed here — we need a workaround for FnOnce in Drop.
        // Use Option wrapper to take ownership.
    }
}

// A cleaner version using Option<Box<dyn FnOnce>> so Drop can call it once.
pub struct AudioFreezeGuardV2 {
    handle: Arc<AudioCaptureHandle>,
    vad_reset_fn: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl AudioFreezeGuardV2 {
    pub fn new(
        handle: Arc<AudioCaptureHandle>,
        vad_reset: impl FnOnce() + Send + Sync + 'static,
    ) -> Self {
        handle.pause();
        info!("AudioFreezeGuard: VAD/STT path paused");
        Self {
            handle,
            vad_reset_fn: Some(Box::new(vad_reset)),
        }
    }
}

impl Drop for AudioFreezeGuardV2 {
    fn drop(&mut self) {
        self.handle.resume();
        if let Some(f) = self.vad_reset_fn.take() {
            f();
        }
        info!("AudioFreezeGuard: VAD/STT path resumed, VAD state reset");
    }
}

// ─── EvictionToken ────────────────────────────────────────────────────────────

/// RAII token that holds a llama-server VRAM eviction.
///
/// `acquire()` orchestrates the full hard-swap sequence:
///   1. SIGTERM (graceful) the running llama-server
///   2. wait for VRAM telemetry to actually drop (NVML)
///   3. respawn llama-server with `--n-gpu-layers 0`
///   4. wait on `VramBarrier` for `required_mb` to be reported free with
///      stable consecutive samples (defends against driver-side caching)
///
/// On `Drop`, the inverse hard-restart is fired-and-forgotten so callers
/// never block on lifecycle in async cleanup paths. Use `restore().await`
/// when you need to await GPU-resident readiness before the next turn.
pub struct EvictionToken {
    controller: Arc<dyn LlmEvictionController>,
    restored: AtomicBool,
}

impl EvictionToken {
    /// Evict the LLM out of VRAM by hard-restarting llama-server with
    /// `--n-gpu-layers 0`, then wait for the VRAM telemetry barrier.
    ///
    /// Returns an `EvictionToken`; dropping it (or calling `restore`) hard
    /// restarts the server back onto GPU.
    pub async fn acquire(
        controller: Arc<dyn LlmEvictionController>,
        profiler: Arc<dyn VramProfiler>,
        required_mb: u64,
    ) -> Result<Self, SwapError> {
        let already_cpu_resident = !controller.is_gpu_resident();

        // Fast path: nothing on GPU. We still need to confirm the VRAM
        // barrier so the caller can rely on `required_mb` being free.
        if already_cpu_resident {
            info!("EvictionToken: LLM already CPU-resident; skipping restart");
        } else {
            // Single attempt — the controller itself owns retry/timeout
            // semantics for the underlying SIGTERM ladder. Surfacing a
            // separate retry loop here would just stack timeouts.
            if let Err(reason) = controller.evict_to_cpu().await {
                warn!(%reason, "EvictionToken: hard-restart eviction failed");
                return Err(SwapError::EvictionFailed {
                    attempts: 1,
                    reason,
                });
            }
        }

        // Wait for VRAM telemetry only when an actual eviction happened.
        // If the LLM is already CPU-resident, no additional free VRAM is
        // expected from this acquire call, so enforcing the barrier here can
        // deadlock on a static free value and create false failures.
        if !already_cpu_resident {
            let barrier = VramBarrier::new(profiler, required_mb);
            match barrier.await_free().await {
                Ok(_snapshot) => {
                    info!(required_mb, "EvictionToken: VRAM barrier passed");
                }
                Err(e) => {
                    return Err(SwapError::VramTimeout {
                        free_mb: e.free_mb(),
                        required_mb,
                    });
                }
            }
        } else {
            let snap = profiler.snapshot().await;
            info!(
                free_mb = snap.free_mb,
                required_mb,
                "EvictionToken: CPU-resident fast path continuing without VRAM barrier"
            );
        }

        Ok(Self {
            controller,
            restored: AtomicBool::new(false),
        })
    }

    /// Restore the LLM back onto GPU. Idempotent — also called by `Drop`,
    /// but `Drop` cannot await; calling this explicitly lets the caller
    /// observe restart errors and gate the next turn on a healthy server.
    pub async fn restore(&self) {
        if self.restored.swap(true, Ordering::AcqRel) {
            return;
        }
        info!("EvictionToken: restoring LLM onto GPU");
        if let Err(reason) = self.controller.restore_from_cpu().await {
            warn!(%reason, "EvictionToken: GPU restore failed");
        }
    }
}

impl Drop for EvictionToken {
    fn drop(&mut self) {
        if !self.restored.load(Ordering::Acquire) {
            self.restored.store(true, Ordering::Release);
            let controller = self.controller.clone();
            tokio::spawn(async move {
                if let Err(reason) = controller.restore_from_cpu().await {
                    warn!(%reason, "EvictionToken(Drop): GPU restore failed");
                } else {
                    info!("EvictionToken(Drop): LLM restored onto GPU");
                }
            });
        }
    }
}

// ─── SwapCoordinator ─────────────────────────────────────────────────────────

/// Tracks the swap cycle count for defrag scheduling.
pub struct SwapCoordinator {
    swap_count: AtomicUsize,
    defrag_threshold: usize,
}

impl SwapCoordinator {
    pub fn new(defrag_every_n_swaps: usize) -> Arc<Self> {
        Arc::new(Self {
            swap_count: AtomicUsize::new(0),
            defrag_threshold: defrag_every_n_swaps,
        })
    }

    /// Increment the swap counter; return `true` if a defrag pass is due.
    pub fn tick(&self) -> bool {
        let n = self.swap_count.fetch_add(1, Ordering::AcqRel) + 1;
        if self.defrag_threshold > 0 && n % self.defrag_threshold == 0 {
            info!(swap_count = n, defrag_threshold = self.defrag_threshold,
                "SwapCoordinator: defrag threshold reached");
            true
        } else {
            false
        }
    }

    pub fn count(&self) -> usize {
        self.swap_count.load(Ordering::Acquire)
    }
}
