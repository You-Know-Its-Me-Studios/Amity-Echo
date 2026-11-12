//! Structured agent output. Presentation adapters consume these events; the
//! coding runtime does not depend on a particular terminal or remote client.

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    Assistant { message: String },
    ModelDelta { text: String },
    ModelReset,
    ModelUsage { input_tokens: Option<u64>, output_tokens: Option<u64>,
        context_window: Option<u64>, cost_usd: Option<f64> },
    Info { message: String },
    Warning { message: String },
    Error { message: String },
    Operation { detail: String },
    ToolRequest { tool: String, detail: String },
    ToolResult { name: String, result: String, success: bool },
    Proposal { label: String, details: String },
    Raw { data: String },
    Context { project: String, session: String, model: String },
    PolicyDecision {
        task_id: String,
        operation: String,
        decision: String,
        reason: String,
        policy: Option<String>,
        choices: Vec<String>,
    },
    ApprovalRequested { description: String },
    TaskState { task_id: String, state: String },
    Completed { summary: String },
    Capability { id: String, action: String, status: String, message: String },
}

pub enum BusyInput { Cancel, Message(String) }

pub trait AgentEventSink {
    fn emit(&mut self, event: AgentEvent);
    fn ask_approval(&mut self, description: &str) -> Result<bool>;
    fn ask_decision(&mut self, description: &str, choices: &[String]) -> Result<Option<String>>;
    fn begin_busy(&mut self) -> Result<()> { Ok(()) }
    fn poll_busy(&mut self) -> Option<BusyInput> { None }
    fn end_busy(&mut self) -> Result<()> { Ok(()) }
    fn begin_model_thinking(&mut self) -> Result<()> { Ok(()) }
    fn end_model_thinking(&mut self) -> Result<()> { Ok(()) }

    fn request_approval(&mut self, description: &str) -> Result<bool> {
        self.emit(AgentEvent::ApprovalRequested { description: description.into() });
        self.ask_approval(description)
    }

    fn assistant(&mut self, message: &str) {
        self.emit(AgentEvent::Assistant { message: message.into() });
    }
    fn info(&mut self, message: &str) {
        self.emit(AgentEvent::Info { message: message.into() });
    }
    fn warning(&mut self, message: &str) {
        self.emit(AgentEvent::Warning { message: message.into() });
    }
    fn error(&mut self, message: &str) {
        self.emit(AgentEvent::Error { message: message.into() });
    }
    fn operation(&mut self, detail: &str) {
        self.emit(AgentEvent::Operation { detail: detail.into() });
    }
    fn tool_request(&mut self, tool: &str, detail: &str) {
        self.emit(AgentEvent::ToolRequest { tool: tool.into(), detail: detail.into() });
    }
    fn tool_result(&mut self, name: &str, result: &str, success: bool) {
        self.emit(AgentEvent::ToolResult { name: name.into(), result: result.into(), success });
    }
    fn proposal(&mut self, label: &str, details: &str) {
        self.emit(AgentEvent::Proposal { label: label.into(), details: details.into() });
    }
    fn raw(&mut self, data: &str) {
        self.emit(AgentEvent::Raw { data: data.into() });
    }
    fn context(&mut self, project: &str, session: &str, model: &str) {
        self.emit(AgentEvent::Context {
            project: project.into(), session: session.into(), model: model.into(),
        });
    }
    fn policy_decision(&mut self, task_id: &str, operation: &str, decision: &str,
        reason: &str, policy: Option<&str>, choices: Vec<String>) {
        self.emit(AgentEvent::PolicyDecision {
            task_id: task_id.into(), operation: operation.into(), decision: decision.into(),
            reason: reason.into(), policy: policy.map(str::to_owned), choices,
        });
    }
    fn task_state(&mut self, task_id: &str, state: &str) {
        self.emit(AgentEvent::TaskState { task_id: task_id.into(), state: state.into() });
    }
    fn completed(&mut self, summary: &str) {
        self.emit(AgentEvent::Completed { summary: summary.into() });
    }
}
