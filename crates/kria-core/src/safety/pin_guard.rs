/// PIN brute-force lockout guard.
///
/// Enforces an exponential backoff on failed PIN attempts:
/// - 5 failures → 60-second lockout
/// - Each subsequent failure while locked OR after lockout expires → lockout
///   duration doubles (120s, 240s, 480s, …, capped at 3600s)
///
/// The guard is per-session so different sessions can have independent lockouts.
/// All state is in-memory; on restart the counter resets (acceptable for desktop use).
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const INITIAL_LOCKOUT_SECS: u64 = 60;
const MAX_LOCKOUT_SECS: u64 = 3600;
const FAIL_THRESHOLD: u32 = 5;

#[derive(Debug, Clone, PartialEq)]
pub enum PinCheckResult {
    /// PIN accepted; consecutive failure counter has been reset.
    Accepted,
    /// PIN rejected; `attempts_left` tells how many are left before lockout.
    Rejected { attempts_left: u32 },
    /// Too many failures — `retry_after` is the remaining lockout in seconds.
    Locked { retry_after_secs: u64 },
}

struct SessionState {
    consecutive_failures: u32,
    locked_until: Option<Instant>,
    /// Current lockout duration (doubles on each new lockout trigger).
    lockout_duration: Duration,
}

impl SessionState {
    fn new() -> Self {
        Self {
            consecutive_failures: 0,
            locked_until: None,
            lockout_duration: Duration::from_secs(INITIAL_LOCKOUT_SECS),
        }
    }

    fn is_locked(&self) -> Option<u64> {
        if let Some(until) = self.locked_until {
            let now = Instant::now();
            if now < until {
                let diff = until - now;
                // Round up so we never report "0 seconds" and tests see exact values.
                let secs = diff.as_secs() + if diff.subsec_nanos() > 0 { 1 } else { 0 };
                return Some(secs.max(1));
            }
        }
        None
    }
}

/// Global PIN lockout guard.  Wrap in `Arc` and share across request handlers.
pub struct PinGuard {
    sessions: Arc<Mutex<HashMap<String, SessionState>>>,
}

impl PinGuard {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Verify a PIN attempt for `session_id`.
    ///
    /// `verify_fn` should return `true` if the PIN is correct.
    /// The function is not called when the session is locked.
    pub async fn check<F>(&self, session_id: &str, verify_fn: F) -> PinCheckResult
    where
        F: FnOnce() -> bool,
    {
        let mut guard = self.sessions.lock().await;
        let state = guard
            .entry(session_id.to_string())
            .or_insert_with(SessionState::new);

        // Check existing lockout.
        if let Some(remaining) = state.is_locked() {
            return PinCheckResult::Locked {
                retry_after_secs: remaining,
            };
        }

        if verify_fn() {
            // Correct PIN — reset counter and clear any expired lockout.
            state.consecutive_failures = 0;
            state.locked_until = None;
            state.lockout_duration = Duration::from_secs(INITIAL_LOCKOUT_SECS);
            PinCheckResult::Accepted
        } else {
            state.consecutive_failures += 1;

            if state.consecutive_failures >= FAIL_THRESHOLD {
                // Trigger lockout.
                let duration = state.lockout_duration;
                state.locked_until = Some(Instant::now() + duration);
                // Double the duration for the NEXT lockout, capped at max.
                state.lockout_duration = Duration::from_secs(
                    (duration.as_secs() * 2).min(MAX_LOCKOUT_SECS),
                );
                // Reset failure counter so the threshold applies again after lockout.
                state.consecutive_failures = 0;

                PinCheckResult::Locked {
                    retry_after_secs: duration.as_secs(),
                }
            } else {
                let attempts_left = FAIL_THRESHOLD - state.consecutive_failures;
                PinCheckResult::Rejected { attempts_left }
            }
        }
    }

    /// Clear the lockout state for a session (e.g. after user changes PIN).
    pub async fn reset(&self, session_id: &str) {
        let mut guard = self.sessions.lock().await;
        guard.remove(session_id);
    }

    /// Returns `Some(remaining_secs)` if the session is currently locked.
    pub async fn lockout_remaining(&self, session_id: &str) -> Option<u64> {
        let guard = self.sessions.lock().await;
        guard.get(session_id).and_then(|s| s.is_locked())
    }
}

impl Default for PinGuard {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "test-session";
    const CORRECT: &str = "1234";
    const WRONG: &str = "9999";

    async fn guard_with_correct(correct_pin: &'static str) -> PinGuard {
        let _ = correct_pin;
        PinGuard::new()
    }

    #[tokio::test]
    async fn correct_pin_accepted_immediately() {
        let g = PinGuard::new();
        let result = g.check(SESSION, || true).await;
        assert_eq!(result, PinCheckResult::Accepted);
    }

    #[tokio::test]
    async fn wrong_pin_counts_down() {
        let g = PinGuard::new();
        for expected_left in (1..=4).rev() {
            let result = g.check(SESSION, || false).await;
            assert_eq!(result, PinCheckResult::Rejected { attempts_left: expected_left });
        }
    }

    #[tokio::test]
    async fn fifth_failure_triggers_lockout() {
        let g = PinGuard::new();
        for _ in 0..4 {
            let _ = g.check(SESSION, || false).await;
        }
        let result = g.check(SESSION, || false).await;
        assert!(matches!(result, PinCheckResult::Locked { retry_after_secs: 60 }));
    }

    #[tokio::test]
    async fn locked_session_rejected_without_calling_verify() {
        let g = PinGuard::new();
        for _ in 0..5 {
            let _ = g.check(SESSION, || false).await;
        }
        // Even if we pass the correct verifier, the gate should be locked.
        let result = g.check(SESSION, || true).await;
        assert!(matches!(result, PinCheckResult::Locked { .. }));
    }

    #[tokio::test]
    async fn lockout_doubles_on_second_trigger() {
        let g = PinGuard::new();
        // Trigger first lockout (60s).
        for _ in 0..5 {
            let _ = g.check(SESSION, || false).await;
        }
        // Manually expire the lockout by manipulating the state.
        {
            let mut guard = g.sessions.lock().await;
            if let Some(state) = guard.get_mut(SESSION) {
                // Set locked_until to the past.
                state.locked_until = Some(Instant::now() - Duration::from_secs(1));
            }
        }
        // Trigger second lockout (should be 120s).
        for _ in 0..5 {
            let _ = g.check(SESSION, || false).await;
        }
        let result = g.check(SESSION, || false).await;
        // Already locked from the 5 failures above, retry should show ~120s.
        assert!(matches!(result, PinCheckResult::Locked { retry_after_secs: 120 }));
    }

    #[tokio::test]
    async fn correct_pin_resets_counter() {
        let g = PinGuard::new();
        // 4 failures — one short of lockout.
        for _ in 0..4 {
            let _ = g.check(SESSION, || false).await;
        }
        // Correct PIN resets.
        let _ = g.check(SESSION, || true).await;
        // Now 4 more failures should be allowed again without triggering lockout.
        for _ in 0..4 {
            let result = g.check(SESSION, || false).await;
            assert!(matches!(result, PinCheckResult::Rejected { .. }));
        }
    }

    #[tokio::test]
    async fn reset_clears_lockout() {
        let g = PinGuard::new();
        for _ in 0..5 {
            let _ = g.check(SESSION, || false).await;
        }
        g.reset(SESSION).await;
        // After reset, a correct PIN should work.
        let result = g.check(SESSION, || true).await;
        assert_eq!(result, PinCheckResult::Accepted);
    }
}
