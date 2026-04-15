use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Tracks sidecar health via periodic pings.
pub struct SidecarHealth {
    alive: Arc<AtomicBool>,
    consecutive_failures: Arc<AtomicU64>,
    max_failures: u64,
    shutdown: Arc<AtomicBool>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl SidecarHealth {
    pub fn new(max_failures: u64) -> Self {
        Self {
            alive: Arc::new(AtomicBool::new(false)),
            consecutive_failures: Arc::new(AtomicU64::new(0)),
            max_failures,
            shutdown: Arc::new(AtomicBool::new(false)),
            task: Mutex::new(None),
        }
    }

    /// Start the health-check loop. Calls `ping_fn` every `interval`.
    /// If `max_failures` consecutive pings fail, calls `restart_fn`.
    pub fn start<P, R>(
        &self,
        interval: std::time::Duration,
        ping_fn: P,
        restart_fn: R,
    ) where
        P: Fn() -> tokio::task::JoinHandle<bool> + Send + Sync + 'static,
        R: Fn() + Send + Sync + 'static,
    {
        let alive = self.alive.clone();
        let failures = self.consecutive_failures.clone();
        let max = self.max_failures;
        let shutdown = self.shutdown.clone();

        let handle = tokio::spawn(async move {
            loop {
                if shutdown.load(Ordering::Relaxed) {
                    break;
                }
                tokio::time::sleep(interval).await;
                if shutdown.load(Ordering::Relaxed) {
                    break;
                }

                let ping_handle = ping_fn();
                let ok = ping_handle.await.unwrap_or(false);

                if ok {
                    alive.store(true, Ordering::Relaxed);
                    failures.store(0, Ordering::Relaxed);
                } else {
                    alive.store(false, Ordering::Relaxed);
                    let count = failures.fetch_add(1, Ordering::Relaxed) + 1;
                    tracing::warn!(consecutive = count, "sidecar health check failed");
                    if count >= max {
                        tracing::error!("sidecar exceeded max failures, restarting");
                        failures.store(0, Ordering::Relaxed);
                        restart_fn();
                    }
                }
            }
        });

        // Store handle (fire-and-forget for now)
        let task = self.task.try_lock();
        if let Ok(mut guard) = task {
            *guard = Some(handle);
        }
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    pub fn mark_alive(&self) {
        self.alive.store(true, Ordering::Relaxed);
        self.consecutive_failures.store(0, Ordering::Relaxed);
    }

    pub fn stop(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}
