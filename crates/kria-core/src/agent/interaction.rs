use chrono::Utc;

/// Represents a complete user interaction (session).
#[derive(Debug, Clone, serde::Serialize)]
pub struct Interaction {
    pub session_id: String,
    pub started_at: String,
    pub turns: Vec<Turn>,
    pub metadata: InteractionMeta,
}

/// A single turn in the conversation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Turn {
    pub role: String,
    pub content: String,
    pub timestamp: String,
    pub tool_calls: Vec<ToolCallRecord>,
}

/// Record of a tool call within a turn.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolCallRecord {
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub result: serde_json::Value,
    pub success: bool,
    pub risk_level: String,
    pub duration_ms: u64,
}

/// Session metadata.
#[derive(Debug, Clone, serde::Serialize)]
pub struct InteractionMeta {
    pub total_turns: usize,
    pub total_tool_calls: usize,
    pub model_used: String,
}

impl Interaction {
    pub fn new(session_id: String) -> Self {
        Self {
            session_id,
            started_at: Utc::now().to_rfc3339(),
            turns: Vec::new(),
            metadata: InteractionMeta {
                total_turns: 0,
                total_tool_calls: 0,
                model_used: String::new(),
            },
        }
    }

    pub fn add_user_turn(&mut self, content: &str) {
        self.turns.push(Turn {
            role: "user".into(),
            content: content.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            tool_calls: Vec::new(),
        });
        self.metadata.total_turns += 1;
    }

    pub fn add_assistant_turn(&mut self, content: &str, tool_calls: Vec<ToolCallRecord>) {
        self.metadata.total_tool_calls += tool_calls.len();
        self.turns.push(Turn {
            role: "assistant".into(),
            content: content.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            tool_calls,
        });
        self.metadata.total_turns += 1;
    }

    pub fn set_model(&mut self, model: &str) {
        self.metadata.model_used = model.to_string();
    }
}
