//! Echo's provider-independent native capability runtime.
pub mod budgets;
pub mod conditional;
pub mod evaluation;
pub mod foundation;
pub mod persistent;
pub mod protocol;
pub mod registry;
pub mod training;
pub mod workspaces;

use crate::config::Config;
use crate::events::{AgentEvent, AgentEventSink};
use crate::host::HostClient;
use crate::model::ChatModel;
use crate::state::{Session, SessionStatus, SessionStore, redact_credentials};
use anyhow::{Result, Context};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};

pub use budgets::Budget;
pub use foundation::{init_outcome, record_session_event};
pub use protocol::{CapabilityResponse, CapabilityStatus};
pub use registry::{CapabilityDescriptor, descriptors, prompt_catalog};

pub struct CapabilityContext<'a> {
    pub config: &'a Config,
    pub session: &'a mut Session,
    pub store: &'a SessionStore,
    pub host: &'a HostClient,
    pub model: &'a ChatModel,
    pub events: &'a mut dyn AgentEventSink,
    pub cancel: &'a AtomicBool,
    pub objective: &'a str,
    pub project_context: &'a Value,
    pub budget: Budget,
    pub stack: Vec<String>,
}

impl CapabilityContext<'_> {
    /// Capability-to-capability calls share the same budget, task, cancellation,
    /// and cycle guard. A capability cannot grant its dependency more authority.
    pub fn invoke(&mut self, id: &str, action: &str, input: &Value) -> Result<CapabilityResponse> {
        invoke_internal(self, id, action, input)
    }

    pub fn active_task_id(&self) -> Option<&str> { self.session.active_task.as_deref() }
}

pub fn catalog(ctx: &CapabilityContext<'_>) -> Result<Value> {
    let current = Config::load().context("loading current capability settings")?;
    let project = current.projects.iter().find(|item| item.id == ctx.session.project_id);
    let host = ctx.project_context.get("development_tools");
    Ok(json!({"protocol":protocol::PROTOCOL,"capabilities":descriptors(&current, project, host),
        "project_id":ctx.session.project_id,"session_id":ctx.session.id,
        "task_id":ctx.session.active_task}))
}

pub fn dispatch(request: Value, ctx: &mut CapabilityContext<'_>) -> Result<CapabilityResponse> {
    let parsed: protocol::CapabilityRequest = serde_json::from_value(request)
        .context("invalid echo_capability request")?;
    if parsed.protocol != protocol::PROTOCOL {
        return finalize(ctx, parsed.capability.as_deref().unwrap_or(""), &parsed.action,
            CapabilityResponse::new(CapabilityStatus::Unsupported,
                format!("Unsupported capability protocol {}", parsed.protocol), Value::Null), false);
    }
    if parsed.mode.as_deref().is_some_and(|mode| !matches!(mode, "automatic" | "manual")) {
        return finalize(ctx, parsed.capability.as_deref().unwrap_or(""), &parsed.action,
            CapabilityResponse::new(CapabilityStatus::Failed,
                "Activation mode must be automatic or manual", Value::Null), false);
    }
    if parsed.action == "discover" && parsed.capability.is_none() {
        let include_training = parsed.input.get("include_training_contract")
            .and_then(Value::as_bool).unwrap_or(false);
        let data = if include_training {
            json!({"catalog":catalog(ctx)?,"training_contract":training::specification(ctx.config)})
        } else { catalog(ctx)? };
        return finalize(ctx, "", "discover", CapabilityResponse::new(
            CapabilityStatus::Completed, "Capability catalog", data), false);
    }
    let id = parsed.capability.as_deref().context("capability ID is required")?;
    let input = if parsed.input.is_null() { json!({}) } else { parsed.input };
    if !input.is_object() { return finalize(ctx, id, &parsed.action,
        CapabilityResponse::new(CapabilityStatus::Failed,
            "Capability input must be a JSON object", Value::Null), true); }
    if input.to_string().len() > 64 * 1024 {
        return finalize(ctx, id, &parsed.action,
            CapabilityResponse::new(CapabilityStatus::Failed,
                "Capability input exceeds 64 KiB", Value::Null), true);
    }
    invoke_internal(ctx, id, &parsed.action, &input)
}

fn invoke_internal(ctx: &mut CapabilityContext<'_>, id: &str, action: &str,
    input: &Value) -> Result<CapabilityResponse> {
    if id.len() > 64 || !id.bytes().all(|byte| byte.is_ascii_lowercase() || byte == b'_') {
        return finalize(ctx, id, action, CapabilityResponse::new(
            CapabilityStatus::Unsupported, "Invalid capability ID", Value::Null), false);
    }
    let current = Config::load().context("loading current capability settings")?;
    let project = current.projects.iter().find(|item| item.id == ctx.session.project_id);
    let host = ctx.project_context.get("development_tools");
    let descriptor = descriptors(&current, project, host).into_iter()
        .find(|entry| entry.id == id);
    let Some(descriptor) = descriptor else {
        return finalize(ctx, id, action, CapabilityResponse::new(
            CapabilityStatus::Unsupported, "Unknown capability", Value::Null), false);
    };
    if !descriptor.methods.contains(&action) {
        return finalize(ctx, id, action, CapabilityResponse::new(
            CapabilityStatus::Unsupported, format!("{action} is not supported by {}", descriptor.name),
            json!({"supported_actions":descriptor.methods})), false);
    }
    if action == "discover" {
        return finalize(ctx, id, action, CapabilityResponse::new(
            CapabilityStatus::Completed, descriptor.name,
            serde_json::to_value(descriptor)?), false);
    }
    if action == "status" {
        let details = if matches!(id, "outcome_engine" | "architecture_twin" | "evidence_ledger"
            | "reversible_engineering" | "engineering_memory" | "black_box") {
            foundation::invoke(ctx, id, action, input)
        } else if matches!(id, "reality_loop" | "solution_arena" | "futurecast"
            | "debug_lab" | "intelligent_workforce" | "visual_to_code_navigation") {
            conditional::invoke(ctx, id, action, input)
        } else { persistent::invoke(ctx, id, action, input) };
        let mut details = details.unwrap_or_else(|error| CapabilityResponse::new(
            CapabilityStatus::Failed, format!("Capability status failed: {error:#}"), Value::Null));
        let unavailable = descriptor.availability.status != "available";
        let data = std::mem::take(&mut details.data);
        details.data = json!({"descriptor":descriptor,"details":data});
        if unavailable {
            details.status = CapabilityStatus::Unavailable;
        }
        return finalize(ctx, id, action, details, false);
    }
    if matches!(ctx.session.status, SessionStatus::AwaitingUserDecision) {
        return finalize(ctx, id, action, CapabilityResponse::new(
            CapabilityStatus::AwaitingUserDecision,
            "Amity suspended this task after a policy denial. A user decision is required.",
            json!({"denial_id":ctx.session.pending_denial})), true);
    }
    if ctx.cancel.load(Ordering::Relaxed) {
        return finalize(ctx, id, action, CapabilityResponse::new(
            CapabilityStatus::Cancelled, "Capability cancelled", Value::Null), true);
    }
    let training_operation = id == "experience_loop" && action == "observe"
        && matches!(input.get("operation").and_then(Value::as_str),
            Some("training_specification" | "export_training"));
    if descriptor.availability.status != "available" && action != "configure" && !training_operation {
        return finalize(ctx, id, action, CapabilityResponse::new(
            CapabilityStatus::Unavailable, descriptor.availability.reason, Value::Null), true);
    }
    if ctx.stack.len() >= ctx.budget.max_depth || ctx.stack.iter().any(|entry| entry == id) {
        return finalize(ctx, id, action, CapabilityResponse::new(
            CapabilityStatus::Failed, "Capability dependency depth or cycle limit reached", Value::Null), true);
    }
    if let Err(reason) = ctx.budget.check(ctx.session) {
        let budget = json!({"budget":ctx.budget});
        return finalize(ctx, id, action, CapabilityResponse::new(
            CapabilityStatus::Failed, reason, budget), true);
    }
    if action == "activate" && descriptor.availability.activation == "manual" {
        let question = format!("Activate {} for this project? This may use {} resources.",
            descriptor.name, descriptor.cost_class);
        if !ctx.events.request_approval(&question)? {
            return finalize(ctx, id, action, CapabilityResponse::new(
                CapabilityStatus::AwaitingApproval, question, Value::Null), true);
        }
    }
    ctx.budget.charge(ctx.session);
    ctx.store.save(ctx.session)?;
    ctx.stack.push(id.to_owned());
    let response = if action == "configure" && (id == "experience_loop"
        || id == "project_steward" && (input.get("auto_monitor").is_some()
            || input.get("interval_seconds").is_some())) {
        persistent::invoke(ctx, id, action, input)
    } else if action == "configure" && (input.get("enabled").is_some()
        || input.get("activation").is_some()) {
        configure(ctx, &descriptor, input)
    } else if matches!(id, "outcome_engine" | "architecture_twin" | "evidence_ledger"
        | "reversible_engineering" | "engineering_memory" | "black_box") {
        foundation::invoke(ctx, id, action, input)
    } else if matches!(id, "reality_loop" | "solution_arena" | "futurecast"
        | "debug_lab" | "intelligent_workforce" | "visual_to_code_navigation") {
        conditional::invoke(ctx, id, action, input)
    } else {
        persistent::invoke(ctx, id, action, input)
    };
    ctx.stack.pop();
    let response = match response {
        Ok(response) => response,
        Err(error) => CapabilityResponse::new(CapabilityStatus::Failed,
            redact_credentials(&format!("Capability failed: {error:#}")), Value::Null),
    };
    finalize(ctx, id, action, response, true)
}

fn finalize(ctx: &mut CapabilityContext<'_>, id: &str, action: &str,
    response: CapabilityResponse, persist: bool) -> Result<CapabilityResponse> {
    let response = contextual(ctx, id, action, response);
    if persist { record_state(ctx, &response)?; }
    ctx.events.emit(AgentEvent::Capability { id: id.into(), action: action.into(),
        status: response.status.as_str().into(), message: response.message.clone() });
    Ok(response)
}

fn contextual(ctx: &CapabilityContext<'_>, id: &str, action: &str,
    mut response: CapabilityResponse) -> CapabilityResponse {
    response.protocol = protocol::PROTOCOL.into();
    response.capability = id.into();
    response.action = action.into();
    response.project_id = ctx.session.project_id.clone();
    response.session_id = ctx.session.id.clone();
    response.task_id = ctx.session.active_task.clone();
    response
}

fn record_state(ctx: &mut CapabilityContext<'_>, response: &CapabilityResponse) -> Result<()> {
    if !ctx.session.capability_state.is_object() { ctx.session.capability_state = json!({}); }
    let state = ctx.session.capability_state.as_object_mut().expect("object set above");
    let capabilities = state.entry("capabilities").or_insert_with(|| json!({}));
    if !capabilities.is_object() { *capabilities = json!({}); }
    capabilities.as_object_mut().expect("object set above").insert(response.capability.clone(),
        json!({"status":response.status,"action":response.action,"message":response.message,
            "task_id":response.task_id,"evidence_refs":response.evidence_refs}));
    let sequence = state.get("training_sequence").and_then(Value::as_u64).unwrap_or(0);
    state.insert("training_sequence".into(), json!(sequence.saturating_add(1)));
    let records = state.entry("training_records").or_insert_with(|| json!([]));
    if !records.is_array() { *records = json!([]); }
    let records = records.as_array_mut().expect("array set above");
    if records.len() >= 200 { records.remove(0); }
    let revision = ctx.project_context.pointer("/active_policy/result/revision")
        .and_then(Value::as_str);
    records.push(json!({"id":format!("{}:{sequence}",ctx.session.id),
        "protocol":protocol::PROTOCOL,"record_type":"capability_decision",
        "capability":response.capability,"activation_reason":response.action,
        "expected_behavior":"Interpret the structured result and follow Amity policy",
        "policy_revision":revision,"result_type":response.status.as_str(),
        "evidence_refs":response.evidence_refs,"verified":false,
        "source_model":ctx.model.version,"permission_metadata":{"license":"local_record"},
        "label":if response.status == CapabilityStatus::Completed { "unverified" } else { "incomplete" }}));
    ctx.store.save(ctx.session)?;
    Ok(())
}

fn configure(ctx: &mut CapabilityContext<'_>, descriptor: &CapabilityDescriptor,
    input: &Value) -> Result<CapabilityResponse> {
    let enabled = input.get("enabled").and_then(Value::as_bool);
    let activation = input.get("activation").and_then(Value::as_str);
    if input.get("enabled").is_some() && enabled.is_none()
        || activation.is_some_and(|value| !matches!(value, "automatic" | "manual" | "off")) {
        return Ok(CapabilityResponse::new(CapabilityStatus::Failed,
            "Use a boolean enabled value and automatic, manual, or off activation",
            Value::Null));
    }
    if descriptor.activation_mode == "always" && (enabled == Some(false) || activation == Some("off")) {
        return Ok(CapabilityResponse::new(CapabilityStatus::Unsupported,
            "This foundation capability is required for governed engineering work",
            Value::Null));
    }
    let scope = input.get("scope").and_then(Value::as_str).unwrap_or("project");
    if !matches!(scope, "project" | "global") {
        return Ok(CapabilityResponse::new(CapabilityStatus::Failed,
            "Capability scope must be project or global", Value::Null));
    }
    let proposed = format!("Change {} {scope} settings: enabled={enabled:?}, activation={activation:?}?",
        descriptor.name);
    if !ctx.events.request_approval(&proposed)? {
        return Ok(CapabilityResponse::new(CapabilityStatus::AwaitingApproval,
            proposed, Value::Null));
    }
    let mut next = Config::load().context("reloading Echo capability settings")?;
    if !next.capabilities.is_object() { next.capabilities = json!({}); }
    let settings = next.capabilities.as_object_mut().expect("object set above");
    let settings = if scope == "project" {
        let projects = settings.entry("projects").or_insert_with(|| json!({}));
        if !projects.is_object() { *projects = json!({}); }
        let project = projects.as_object_mut().expect("object set above")
            .entry(ctx.session.project_id.clone()).or_insert_with(|| json!({}));
        if !project.is_object() { *project = json!({}); }
        project.as_object_mut().expect("object set above")
    } else { settings };
    let overrides = settings.entry("overrides").or_insert_with(|| json!({}));
    if !overrides.is_object() { *overrides = json!({}); }
    let entry = overrides.as_object_mut().expect("object set above")
        .entry(descriptor.id).or_insert_with(|| json!({}));
    if !entry.is_object() { *entry = json!({}); }
    let fields = entry.as_object_mut().expect("object set above");
    if let Some(enabled) = enabled { fields.insert("enabled".into(), json!(enabled)); }
    if let Some(activation) = activation { fields.insert("activation".into(), json!(activation)); }
    next.save()?;
    Ok(CapabilityResponse::new(CapabilityStatus::Completed,
        format!("{} settings saved. Amity policy still governs every project operation.", descriptor.name),
        json!({"enabled":enabled,"activation":activation,"scope":scope})))
}
