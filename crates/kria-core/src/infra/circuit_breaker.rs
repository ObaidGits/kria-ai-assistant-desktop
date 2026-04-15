use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Circuit breaker states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

/// Generic circuit breaker wrapping async calls.
///
/// CLOSED → OPEN after `failure_threshold` failures.
/// OPEN → HALF_OPEN after `recovery_timeout`.
/// HALF_OPEN → CLOSED on success, OPEN on failure.
#[derive(Debug)]
pub struct CircuitBreaker {
    name: String,
    failure_threshold: u32,
    recovery_timeout: Duration,
    failure_count: AtomicU32,
    state: Mutex<CircuitState>,
    last_failure: Mutex<Option<Instant>>,
}

impl CircuitBreaker {
    pub fn new(name: impl Into<String>, failure_threshold: u32, recovery_timeout: Duration) -> Self {
        Self {
            name: name.into(),
            failure_threshold,
            recovery_timeout,
            failure_count: AtomicU32::new(0),
            state: Mutex::new(CircuitState::Closed),
            last_failure: Mutex::new(None),
        }
    }

    pub fn with_defaults(name: impl Into<String>) -> Self {
        Self::new(name, 3, Duration::from_secs(30))
    }

    /// Current state.
    pub async fn state(&self) -> CircuitState {
        let mut state = self.state.lock().await;
        // Check for OPEN → HALF_OPEN transition
        if *state == CircuitState::Open {
            if let Some(last) = *self.last_failure.lock().await {
                if last.elapsed() >= self.recovery_timeout {
                    *state = CircuitState::HalfOpen;
                }
            }
        }
        *state
    }

    /// Execute a function through the circuit breaker.
    ///
    /// `is_ignored` — predicate on the error: if true, the error is re-raised
    /// without counting as a failure (e.g., context-too-large errors).
    pub async fn call<F, T, E>(
        &self,
        f: F,
        is_ignored: impl Fn(&E) -> bool,
    ) -> Result<T, CircuitBreakerError<E>>
    where
        F: std::future::Future<Output = Result<T, E>>,
    {
        let current = self.state().await;
        if current == CircuitState::Open {
            return Err(CircuitBreakerError::Open(self.name.clone()));
        }

        match f.await {
            Ok(val) => {
                self.on_success().await;
                Ok(val)
            }
            Err(e) => {
                if is_ignored(&e) {
                    // Re-raise without counting as failure
                    return Err(CircuitBreakerError::Inner(e));
                }
                self.on_failure().await;
                Err(CircuitBreakerError::Inner(e))
            }
        }
    }

    /// Record a success — reset counters, close circuit.
    pub async fn on_success(&self) {
        self.failure_count.store(0, Ordering::Relaxed);
        let mut state = self.state.lock().await;
        *state = CircuitState::Closed;
    }

    /// Record a failure — increment counter, possibly open circuit.
    pub async fn on_failure(&self) {
        let count = self.failure_count.fetch_add(1, Ordering::Relaxed) + 1;
        *self.last_failure.lock().await = Some(Instant::now());
        if count >= self.failure_threshold {
            let mut state = self.state.lock().await;
            *state = CircuitState::Open;
            tracing::warn!(
                circuit = %self.name,
                failures = count,
                "circuit breaker OPEN"
            );
        }
    }

    /// Manually reset the circuit to CLOSED.
    pub async fn reset(&self) {
        self.failure_count.store(0, Ordering::Relaxed);
        *self.state.lock().await = CircuitState::Closed;
        *self.last_failure.lock().await = None;
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CircuitBreakerError<E> {
    #[error("circuit breaker '{0}' is OPEN")]
    Open(String),
    #[error(transparent)]
    Inner(E),
}
