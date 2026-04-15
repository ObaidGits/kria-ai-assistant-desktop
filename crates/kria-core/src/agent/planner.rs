/// Multi-step task planner. Breaks complex requests into tool-use steps.
pub struct Planner;

/// A planned step in a multi-step task.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PlanStep {
    pub step_number: usize,
    pub tool_name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub depends_on: Vec<usize>,
    pub error_handling: String,
}

impl Planner {
    /// Create a plan from user input + LLM output.
    ///
    /// The planner prompts the LLM to produce a numbered plan, then
    /// parses it into structured steps.
    pub fn parse_plan(llm_output: &str) -> Vec<PlanStep> {
        let mut steps = Vec::new();
        let mut current_step = 0usize;

        for line in llm_output.lines() {
            let trimmed = line.trim();
            // Match numbered lines: "1. ..." or "Step 1: ..."
            if let Some(rest) = trimmed.strip_prefix(&format!("{}.", current_step + 1))
                .or_else(|| trimmed.strip_prefix(&format!("Step {}: ", current_step + 1)))
                .or_else(|| trimmed.strip_prefix(&format!("Step {}:", current_step + 1)))
            {
                current_step += 1;
                let rest = rest.trim();

                // Try to extract tool name from "Use tool_name" or "Call tool_name"
                let tool_name = extract_tool_name(rest);

                steps.push(PlanStep {
                    step_number: current_step,
                    tool_name: tool_name.unwrap_or_default(),
                    description: rest.to_string(),
                    parameters: serde_json::json!({}),
                    depends_on: if current_step > 1 { vec![current_step - 1] } else { vec![] },
                    error_handling: String::new(),
                });
            }
        }

        steps
    }
}

fn extract_tool_name(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let prefixes = ["use ", "call ", "run ", "execute "];
    for prefix in &prefixes {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let tool = rest.split_whitespace().next()?;
            // Strip trailing punctuation
            let tool = tool.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_');
            if !tool.is_empty() {
                return Some(tool.to_string());
            }
        }
    }
    None
}
