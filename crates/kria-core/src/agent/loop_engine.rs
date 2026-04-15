use std::sync::Arc;
use tokio::sync::mpsc;

use crate::llm::{ChatMessage, ModelRouter, ToolSchema, TOOL_RESULT_MAX_CHARS};
use crate::tools::registry::ToolRegistry;
use crate::safety::{PolicyEngine, HitlGateway, AuditLogger, RollbackManager, RiskLevel};
use crate::safety::audit::{Decision, DecidedBy};
use crate::safety::hitl::ApprovalResponse;
use crate::agent::response_parser::{parse_tool_calls, extract_text_response};
use crate::infra::isolation::run_isolated;

/// Events emitted during agent loop execution.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// Text token from the LLM.
    Token(String),
    /// Tool is being called.
    ToolStart { name: String, params: serde_json::Value },
    /// Tool completed.
    ToolEnd { name: String, result: serde_json::Value, success: bool },
    /// Waiting for HITL approval.
    ApprovalRequired { request_id: String, action: String, risk_level: String },
    /// Approval result.
    ApprovalResult { action: String, approved: bool },
    /// Planning step.
    Plan(String),
    /// Error.
    Error(String),
    /// Final response text.
    Done(String),
}

/// The core ReAct agent loop.
pub struct AgentLoop {
    model_router: Arc<ModelRouter>,
    tool_registry: Arc<ToolRegistry>,
    policy_engine: Arc<PolicyEngine>,
    hitl_gateway: Arc<HitlGateway>,
    audit_logger: Arc<AuditLogger>,
    #[allow(dead_code)]
    rollback_mgr: Arc<RollbackManager>,
    max_tool_rounds: usize,
}

impl AgentLoop {
    pub fn new(
        model_router: Arc<ModelRouter>,
        tool_registry: Arc<ToolRegistry>,
        policy_engine: Arc<PolicyEngine>,
        hitl_gateway: Arc<HitlGateway>,
        audit_logger: Arc<AuditLogger>,
        rollback_mgr: Arc<RollbackManager>,
    ) -> Self {
        Self {
            model_router,
            tool_registry,
            policy_engine,
            hitl_gateway,
            audit_logger,
            rollback_mgr,
            max_tool_rounds: 10,
        }
    }

    /// Run the agent loop for a single user turn.
    /// Returns a channel of StreamEvents.
    pub async fn run(
        &self,
        session_id: &str,
        messages: &mut Vec<ChatMessage>,
        event_tx: mpsc::UnboundedSender<StreamEvent>,
    ) {
        let backend = match self.model_router.route("chat").await {
            Some(b) => b,
            None => {
                let _ = event_tx.send(StreamEvent::Error("no LLM backend available".into()));
                return;
            }
        };

        // Build tool schemas for the LLM
        let tool_schemas: Vec<ToolSchema> = self.tool_registry.list_defs().iter().map(|d| {
            ToolSchema {
                name: d.name.clone(),
                description: d.description.clone(),
                parameters: d.to_function_schema()["function"]["parameters"].clone(),
            }
        }).collect();

        for _round in 0..self.max_tool_rounds {
            // Call LLM
            let response = match backend.chat(
                messages,
                Some(&tool_schemas),
                0.7,
                4096,
            ).await {
                Ok(r) => r,
                Err(e) => {
                    let _ = event_tx.send(StreamEvent::Error(format!("LLM error: {e}")));
                    return;
                }
            };

            // Parse tool calls from response
            let tool_calls = parse_tool_calls(&response.content);
            let text_response = extract_text_response(&response.content);

            // Send text tokens
            if !text_response.is_empty() {
                let _ = event_tx.send(StreamEvent::Token(text_response.clone()));
            }

            // If no tool calls, we're done
            if tool_calls.is_empty() {
                let _ = event_tx.send(StreamEvent::Done(text_response));
                return;
            }

            // Add assistant message to history
            messages.push(ChatMessage {
                role: "assistant".into(),
                content: response.content.clone(),
                name: None,
            });

            // Execute each tool call
            for call in &tool_calls {
                let _ = event_tx.send(StreamEvent::ToolStart {
                    name: call.name.clone(),
                    params: call.arguments.clone(),
                });

                // Policy check
                let decision = self.policy_engine.evaluate(&call.name, &call.arguments);

                if decision.blocked {
                    // BLACK tier — always denied
                    self.audit_logger.log(
                        session_id, &call.name, &call.arguments,
                        RiskLevel::Black, Decision::Blocked, DecidedBy::Hardcoded,
                    );
                    let _ = event_tx.send(StreamEvent::ToolEnd {
                        name: call.name.clone(),
                        result: serde_json::json!({ "error": "blocked by safety policy" }),
                        success: false,
                    });
                    messages.push(ChatMessage {
                        role: "tool".into(),
                        content: format!("Tool '{}' blocked by safety policy: {}", call.name, decision.reason),
                        name: Some(call.name.clone()),
                    });
                    continue;
                }

                if decision.requires_approval {
                    // RED tier — needs HITL approval
                    let _ = event_tx.send(StreamEvent::ApprovalRequired {
                        request_id: "pending".into(),
                        action: call.name.clone(),
                        risk_level: decision.risk_level.as_str().into(),
                    });

                    let approval = self.hitl_gateway.request_approval(
                        &call.name,
                        call.arguments.clone(),
                        decision.risk_level,
                        &format!("Execute {} with params: {}", call.name, call.arguments),
                        true,
                    ).await;

                    let (audit_decision, decided_by, approved) = match approval {
                        ApprovalResponse::Approved => (Decision::Approved, DecidedBy::UserGui, true),
                        ApprovalResponse::Denied => (Decision::Denied, DecidedBy::UserGui, false),
                        ApprovalResponse::Timeout => (Decision::Timeout, DecidedBy::Timeout, false),
                    };

                    self.audit_logger.log(
                        session_id, &call.name, &call.arguments,
                        decision.risk_level, audit_decision, decided_by,
                    );

                    let _ = event_tx.send(StreamEvent::ApprovalResult {
                        action: call.name.clone(),
                        approved,
                    });

                    if !approved {
                        messages.push(ChatMessage {
                            role: "tool".into(),
                            content: format!("Tool '{}' denied by user", call.name),
                            name: Some(call.name.clone()),
                        });
                        continue;
                    }

                    // Create rollback snapshot for RED actions
                    // (actual file backup happens inside specific tool handlers)
                }

                // Execute the tool
                let tool_result = if let Some(handler) = self.tool_registry.get_handler(&call.name) {
                    let handler = handler.clone();
                    let args = call.arguments.clone();
                    run_isolated(
                        &format!("tool:{}", call.name),
                        std::time::Duration::from_secs(30),
                        move || async move { handler.execute(args).await },
                    ).await
                } else {
                    crate::infra::isolation::ToolResult::err(format!("unknown tool: {}", call.name))
                };

                // Log GREEN/YELLOW auto-executed
                if !decision.requires_approval {
                    let audit_decision = if tool_result.success {
                        Decision::AutoExecuted
                    } else {
                        Decision::AutoExecuted
                    };
                    self.audit_logger.log(
                        session_id, &call.name, &call.arguments,
                        decision.risk_level, audit_decision, DecidedBy::Policy,
                    );
                }

                // Truncate large results
                let result_str = tool_result.data.to_string();
                let truncated = if result_str.len() > TOOL_RESULT_MAX_CHARS {
                    format!("{}...<truncated>", &result_str[..TOOL_RESULT_MAX_CHARS])
                } else {
                    result_str
                };

                let _ = event_tx.send(StreamEvent::ToolEnd {
                    name: call.name.clone(),
                    result: tool_result.data.clone(),
                    success: tool_result.success,
                });

                messages.push(ChatMessage {
                    role: "tool".into(),
                    content: truncated,
                    name: Some(call.name.clone()),
                });
            }
        }

        let _ = event_tx.send(StreamEvent::Error(
            format!("max tool rounds ({}) reached", self.max_tool_rounds)
        ));
    }
}
