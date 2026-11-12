use crate::events::{AgentEvent, AgentEventSink};
use crate::state::{Session, SessionStatus};
use anyhow::Result;
use serde_json::json;
use std::io::{self, Write};

pub struct JsonSink {
    output_error: Option<io::Error>,
}

impl JsonSink {
    pub fn new() -> Self { Self { output_error: None } }

    pub fn record(&mut self, value: serde_json::Value) {
        if self.output_error.is_some() { return; }
        let mut out = io::stdout().lock();
        if let Err(error) = writeln!(out, "{value}").and_then(|_| out.flush()) {
            self.output_error = Some(error);
        }
    }

    pub fn finish(&mut self, session: &Session, error: Option<&anyhow::Error>) -> Result<i32> {
        let code = if error.is_some() { 1 } else { match session.status {
            SessionStatus::WaitingApproval => 2,
            SessionStatus::AwaitingUserDecision => 3,
            SessionStatus::Interrupted | SessionStatus::Cancelled => 4,
            SessionStatus::Failed => 1,
            _ => 0,
        }};
        self.record(json!({"type":"result","session_id":session.id,
            "project_id":session.project_id,"task_id":session.active_task,
            "status":session.status,"exit_code":code,
            "error":error.map(|error| crate::state::redact_credentials(&format!("{error:#}"))),
            "usage":{"input_tokens":session.usage_input_tokens,
                "output_tokens":session.usage_output_tokens,
                "cost_usd":session.usage_cost_reported.then_some(session.usage_cost_usd),
                "context_window":session.context_window}}));
        if let Some(error) = self.output_error.take() { return Err(error.into()); }
        Ok(code)
    }
}

impl AgentEventSink for JsonSink {
    fn emit(&mut self, event: AgentEvent) {
        if let AgentEvent::ModelDelta { text } = &event {
            self.record(json!({"type":"model_progress","characters":text.chars().count()}));
            return;
        }
        let mut event = event;
        redact_event(&mut event);
        self.record(serde_json::to_value(event).unwrap_or_else(|error|
            json!({"type":"error","message":format!("could not serialize event: {error}")})));
    }

    fn ask_approval(&mut self, _description: &str) -> Result<bool> { Ok(false) }

    fn ask_decision(&mut self, _description: &str, _choices: &[String]) -> Result<Option<String>> {
        Ok(None)
    }
}

fn redact_event(event: &mut AgentEvent) {
    use crate::state::redact_credentials;
    match event {
        AgentEvent::Assistant { message } | AgentEvent::Info { message }
        | AgentEvent::Warning { message } | AgentEvent::Error { message } => {
            *message = redact_credentials(message);
        }
        AgentEvent::Operation { detail } | AgentEvent::ToolRequest { detail, .. } => {
            *detail = redact_credentials(detail);
        }
        AgentEvent::ToolResult { result, .. } | AgentEvent::Raw { data: result } => {
            *result = redact_credentials(result);
        }
        AgentEvent::Proposal { details, .. } => *details = redact_credentials(details),
        AgentEvent::PolicyDecision { reason, .. } => *reason = redact_credentials(reason),
        AgentEvent::ApprovalRequested { description } => *description = redact_credentials(description),
        AgentEvent::Completed { summary } => *summary = redact_credentials(summary),
        _ => {}
    }
}
