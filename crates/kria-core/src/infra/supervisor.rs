use std::time::Duration;
use tokio::task::JoinHandle;

/// A supervised async task that restarts on failure with exponential backoff.
pub struct SupervisedTask {
    name: String,
    max_retries: usize,
    base_delay: Duration,
    max_delay: Duration,
    handle: Option<JoinHandle<()>>,
}

impl SupervisedTask {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            max_retries: 10,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
            handle: None,
        }
    }

    /// Start a supervised coroutine. Will restart on panic/error up to max_retries.
    pub fn spawn<F, Fut>(&mut self, factory: F)
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let name = self.name.clone();
        let max_retries = self.max_retries;
        let base_delay = self.base_delay;
        let max_delay = self.max_delay;

        let handle = tokio::spawn(async move {
            let mut retries = 0usize;
            loop {
                tracing::info!(task = %name, attempt = retries + 1, "starting supervised task");
                match factory().await {
                    Ok(()) => {
                        tracing::info!(task = %name, "supervised task completed normally");
                        break;
                    }
                    Err(e) => {
                        retries += 1;
                        if retries > max_retries {
                            tracing::error!(
                                task = %name,
                                error = %e,
                                "supervised task exceeded max retries, giving up"
                            );
                            break;
                        }
                        let delay = std::cmp::min(
                            base_delay * 2u32.saturating_pow(retries as u32 - 1),
                            max_delay,
                        );
                        tracing::warn!(
                            task = %name,
                            error = %e,
                            retry = retries,
                            delay_ms = delay.as_millis(),
                            "supervised task failed, restarting"
                        );
                        tokio::time::sleep(delay).await;
                    }
                }
            }
        });

        self.handle = Some(handle);
    }

    /// Cancel the supervised task.
    pub fn cancel(&mut self) {
        if let Some(h) = self.handle.take() {
            h.abort();
            tracing::info!(task = %self.name, "supervised task cancelled");
        }
    }

    pub fn is_running(&self) -> bool {
        self.handle.as_ref().is_some_and(|h| !h.is_finished())
    }
}

impl Drop for SupervisedTask {
    fn drop(&mut self) {
        self.cancel();
    }
}
