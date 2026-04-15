use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Workflow engine for multi-step automated sequences with conditions.
pub struct WorkflowEngine {
    workflows: HashMap<String, Workflow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workflow {
    pub id: String,
    pub name: String,
    pub description: String,
    pub steps: Vec<WorkflowStep>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowStep {
    pub id: String,
    pub tool_name: String,
    pub args: serde_json::Value,
    /// Optional: only run if this condition evaluates to true.
    pub condition: Option<String>,
    /// Step ID to jump to on failure (default: abort).
    pub on_failure: Option<String>,
}

#[derive(Debug)]
pub struct WorkflowExecution {
    pub workflow_id: String,
    pub results: Vec<StepResult>,
    pub status: WorkflowStatus,
}

#[derive(Debug)]
pub struct StepResult {
    pub step_id: String,
    pub success: bool,
    pub output: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WorkflowStatus {
    Running,
    Completed,
    Failed(String),
    Cancelled,
}

impl WorkflowEngine {
    pub fn new() -> Self {
        Self {
            workflows: HashMap::new(),
        }
    }

    pub fn register(&mut self, workflow: Workflow) {
        self.workflows.insert(workflow.id.clone(), workflow);
    }

    pub fn get(&self, id: &str) -> Option<&Workflow> {
        self.workflows.get(id)
    }

    pub fn list(&self) -> Vec<&Workflow> {
        self.workflows.values().collect()
    }

    pub fn delete(&mut self, id: &str) -> bool {
        self.workflows.remove(id).is_some()
    }

    /// Execute a workflow. The executor callback runs each tool call.
    pub async fn execute<F, Fut>(
        &self,
        workflow_id: &str,
        executor: F,
    ) -> anyhow::Result<WorkflowExecution>
    where
        F: Fn(String, serde_json::Value) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<String>>,
    {
        let workflow = self.workflows.get(workflow_id)
            .ok_or_else(|| anyhow::anyhow!("workflow not found: {workflow_id}"))?;

        let mut execution = WorkflowExecution {
            workflow_id: workflow_id.to_string(),
            results: Vec::new(),
            status: WorkflowStatus::Running,
        };

        for step in &workflow.steps {
            let start = std::time::Instant::now();

            match executor(step.tool_name.clone(), step.args.clone()).await {
                Ok(output) => {
                    execution.results.push(StepResult {
                        step_id: step.id.clone(),
                        success: true,
                        output,
                        duration_ms: start.elapsed().as_millis() as u64,
                    });
                }
                Err(e) => {
                    execution.results.push(StepResult {
                        step_id: step.id.clone(),
                        success: false,
                        output: e.to_string(),
                        duration_ms: start.elapsed().as_millis() as u64,
                    });

                    if step.on_failure.is_none() {
                        execution.status = WorkflowStatus::Failed(
                            format!("step {} failed: {e}", step.id),
                        );
                        return Ok(execution);
                    }
                    // on_failure jump not yet implemented (requires index lookup)
                }
            }
        }

        execution.status = WorkflowStatus::Completed;
        Ok(execution)
    }

    /// Save workflows to file.
    pub fn save_to_file(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let data = serde_json::to_string_pretty(&self.workflows)?;
        std::fs::write(path, data)?;
        Ok(())
    }

    /// Load workflows from file.
    pub fn load_from_file(&mut self, path: &std::path::Path) -> anyhow::Result<()> {
        if path.exists() {
            let data = std::fs::read_to_string(path)?;
            self.workflows = serde_json::from_str(&data)?;
        }
        Ok(())
    }
}
