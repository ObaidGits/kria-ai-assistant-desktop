use std::collections::HashMap;
use tokio::sync::mpsc;

/// Automation scheduler: run agent tasks on cron-like schedules.
pub struct AutomationScheduler {
    tasks: HashMap<String, ScheduledTask>,
    cancel_tx: HashMap<String, mpsc::Sender<()>>,
}

#[derive(Debug, Clone)]
pub struct ScheduledTask {
    pub id: String,
    pub name: String,
    /// Interval in seconds.
    pub interval_secs: u64,
    /// Agent prompt to execute.
    pub prompt: String,
    pub enabled: bool,
}

impl AutomationScheduler {
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
            cancel_tx: HashMap::new(),
        }
    }

    /// Register a scheduled task.
    pub fn add_task(&mut self, task: ScheduledTask) {
        self.tasks.insert(task.id.clone(), task);
    }

    /// Start a scheduled task. Returns a handle for the background loop.
    pub fn start_task(
        &mut self,
        task_id: &str,
        callback: impl Fn(String) + Send + Sync + 'static,
    ) -> anyhow::Result<()> {
        let task = self
            .tasks
            .get(task_id)
            .ok_or_else(|| anyhow::anyhow!("task not found: {task_id}"))?
            .clone();

        if !task.enabled {
            anyhow::bail!("task is disabled: {task_id}");
        }

        let (cancel_tx, mut cancel_rx) = mpsc::channel::<()>(1);
        self.cancel_tx.insert(task_id.to_string(), cancel_tx);

        let prompt = task.prompt.clone();
        let interval = std::time::Duration::from_secs(task.interval_secs);

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {
                        callback(prompt.clone());
                    }
                    _ = cancel_rx.recv() => {
                        tracing::info!("scheduled task {} cancelled", task.id);
                        break;
                    }
                }
            }
        });

        Ok(())
    }

    /// Stop a running scheduled task.
    pub fn stop_task(&mut self, task_id: &str) {
        self.cancel_tx.remove(task_id);
    }

    /// List all registered tasks.
    pub fn list_tasks(&self) -> Vec<&ScheduledTask> {
        self.tasks.values().collect()
    }

    /// Remove a task.
    pub fn remove_task(&mut self, task_id: &str) {
        self.stop_task(task_id);
        self.tasks.remove(task_id);
    }
}

impl Default for AutomationScheduler {
    fn default() -> Self {
        Self::new()
    }
}
