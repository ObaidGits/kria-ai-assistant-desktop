//! RAII process guard with guaranteed zombie cleanup and ordered termination.
//!
//! # Guarantees
//! - **No orphan on host panic/kill**: `configure_child_command()` sets
//!   `PR_SET_PDEATHSIG = SIGKILL` in the child's `pre_exec`. If the Kria
//!   process dies for any reason the kernel delivers SIGKILL to llama-server.
//! - **No orphan on process-group**: `setsid()` puts the child in its own
//!   session/group so `kill(-pgid, SIGKILL)` reaps any threads it may fork.
//! - **No zombie on explicit shutdown**: `ChildGuard::terminate()` issues
//!   SIGTERM → wait(timeout) → SIGKILL → wait and always reaps.
//! - **No zombie on Drop / panic unwind**: `Drop` sends SIGKILL synchronously
//!   (via libc, no async runtime needed) and calls `start_kill()` so Tokio's
//!   SIGCHLD watcher reaps the child on the next event loop iteration.

use std::time::Duration;
use tokio::process::Child;

/// Configure a `tokio::process::Command` for safe subprocess management.
///
/// Call this **before** `cmd.spawn()`. On Linux it installs a `pre_exec`
/// hook that runs in the child after `fork()` but before `exec()`:
///
/// - `setsid()`: child becomes its own session leader (new process group).
///   `kill(-pgid, SIG)` will now reach all threads the child spawns.
/// - `prctl(PR_SET_PDEATHSIG, SIGKILL)`: kernel delivers SIGKILL to the
///   child if the parent process (Kria) dies — handles SIGKILL/panic cases.
pub fn configure_child_command(cmd: &mut tokio::process::Command) {
    #[cfg(target_os = "linux")]
    unsafe {
        cmd.pre_exec(|| {
            // New session → child becomes process group leader.
            libc::setsid();
            // Parent-death signal: kernel kills child when parent dies.
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);
            Ok(())
        });
    }
}

/// RAII guard around a `tokio::process::Child`.
///
/// The guard tracks the child PID and ensures it is killed and reaped
/// through `terminate()`, `force_kill()`, or — as a last resort — `Drop`.
///
/// # Usage pattern
/// ```ignore
/// let mut guard = ChildGuard::new(child);
/// // ...
/// guard.terminate(Duration::from_secs(5)).await;  // graceful shutdown
/// ```
pub struct ChildGuard {
    /// The guarded child. `Option` so `Drop` can take it.
    child: Option<Child>,
    /// PID captured at construction. Preserved even after `child` is consumed.
    pid: Option<u32>,
}

impl ChildGuard {
    /// Wrap a freshly spawned child.
    pub fn new(child: Child) -> Self {
        let pid = child.id();
        Self {
            child: Some(child),
            pid,
        }
    }

    /// Take the `stderr` pipe handle from the child (available once, before
    /// any other I/O). Returns `None` if the child has already exited or
    /// stderr was already taken.
    pub fn take_stderr(&mut self) -> Option<tokio::process::ChildStderr> {
        self.child.as_mut()?.stderr.take()
    }

    /// Take the `stdout` pipe handle from the child.
    pub fn take_stdout(&mut self) -> Option<tokio::process::ChildStdout> {
        self.child.as_mut()?.stdout.take()
    }

    /// Non-blocking check whether the child has exited. Returns `Ok(Some(…))`
    /// if it has exited, `Ok(None)` if still running, or `Err` if the child
    /// was already fully reaped (i.e. `terminate`/`force_kill` completed).
    pub fn try_wait(
        &mut self,
    ) -> std::io::Result<Option<std::process::ExitStatus>> {
        match self.child.as_mut() {
            Some(c) => c.try_wait(),
            None => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "child process was already reaped",
            )),
        }
    }

    /// Graceful termination ladder: **SIGTERM → wait(timeout) → SIGKILL → wait**.
    ///
    /// Always reaps the child before returning. After this call
    /// `is_alive()` returns `false`.
    pub async fn terminate(&mut self, graceful_timeout: Duration) {
        let Some(child) = self.child.as_mut() else {
            return;
        };

        if let Some(id) = self.pid {
            tracing::info!(pid = id, "child_guard: SIGTERM → process group");
            kill_process_group(id, false);
        }

        match tokio::time::timeout(graceful_timeout, child.wait()).await {
            Ok(Ok(status)) => {
                tracing::info!(code = ?status.code(), "child_guard: exited gracefully");
                self.child = None;
                return;
            }
            Ok(Err(e)) => {
                tracing::warn!(?e, "child_guard: wait error after SIGTERM");
            }
            Err(_) => {
                tracing::warn!(
                    timeout_secs = graceful_timeout.as_secs(),
                    "child_guard: graceful timeout — escalating to SIGKILL"
                );
            }
        }

        self.force_kill().await;
    }

    /// Immediate SIGKILL and reap. Used by the watchdog emergency path.
    pub async fn force_kill(&mut self) {
        let Some(child) = self.child.as_mut() else {
            return;
        };

        if let Some(id) = self.pid {
            tracing::warn!(pid = id, "child_guard: SIGKILL → process group");
            kill_process_group(id, true);
        }

        // tokio-level kill covers any platform gaps.
        let _ = child.kill().await;

        match tokio::time::timeout(Duration::from_secs(3), child.wait()).await {
            Ok(Ok(status)) => {
                tracing::debug!(code = ?status.code(), "child_guard: reaped after SIGKILL");
            }
            Ok(Err(e)) => {
                tracing::warn!(?e, "child_guard: reap error after SIGKILL");
            }
            Err(_) => {
                tracing::warn!("child_guard: reap timed out — may leave brief zombie");
            }
        }

        self.child = None;
    }

    /// PID of the guarded process (available even after the child exits).
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Whether the child handle is still live (i.e. `terminate`/`force_kill`
    /// has not been called yet).
    pub fn is_alive(&self) -> bool {
        self.child.is_some()
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };

        tracing::warn!(
            pid = self.pid,
            "child_guard: dropped without explicit terminate — force-killing synchronously"
        );

        // Synchronous kill via libc (no async runtime required — safe on panic).
        if let Some(id) = self.pid {
            kill_process_group(id, true);
        }

        // `start_kill()` is synchronous — sends the OS kill signal without
        // waiting for the process to exit. Tokio's SIGCHLD watcher will reap
        // the zombie on the next event-loop tick.
        //
        // We cannot call the async `child.wait()` from a sync Drop, so we
        // accept a brief zombie existence. PR_SET_PDEATHSIG=SIGKILL ensures
        // the child is still killed even if *we* die before reaping.
        let _ = child.start_kill();
    }
}

/// Send signal to the process *group* whose PGID equals `pid`.
///
/// After `setsid()` in the child, its PGID == its PID, so `kill(-pid, sig)`
/// reaches all threads the child may have spawned.
fn kill_process_group(pid: u32, sigkill: bool) {
    #[cfg(unix)]
    {
        // Guard against overflow (pid 0 would send to own group).
        let pgid = match i32::try_from(pid) {
            Ok(p) if p > 0 => p,
            _ => return,
        };
        let sig = if sigkill { libc::SIGKILL } else { libc::SIGTERM };
        unsafe {
            libc::kill(-pgid, sig);
        }
    }

    #[cfg(not(unix))]
    let _ = (pid, sigkill);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kill_process_group_zero_pid_is_noop() {
        // Must not panic or send signals to pgid 0.
        kill_process_group(0, true);
        kill_process_group(0, false);
    }
}
