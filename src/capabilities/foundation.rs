//! Lightweight, model-independent engineering records and Amity-backed observations.
//! These records describe Echo's work; Amity remains the authority for project facts,
//! permissions, execution, verification, and reversible changes.

use crate::state::{Session, contains_credential, redact_credentials, write_json_atomic};
use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{CapabilityContext, CapabilityResponse, CapabilityStatus};

static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

pub fn invoke(ctx: &mut CapabilityContext<'_>, id: &str, action: &str,
    input: &Value) -> Result<CapabilityResponse> {
    match id {
        "outcome_engine" => outcome(ctx, action, input),
        "architecture_twin" => architecture(ctx, action, input),
        "evidence_ledger" => evidence(ctx, action, input),
        "reversible_engineering" => recovery(ctx, action, input),
        "engineering_memory" => memory(ctx, action, input),
        "black_box" => black_box(ctx, action, input),
        _ => Ok(CapabilityResponse::new(CapabilityStatus::Unsupported,
            "This foundation capability is unknown", Value::Null)),
    }
}

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()
}

fn safe_text(text: &str, limit: usize) -> Result<String> {
    if text.trim().is_empty() || text.len() > limit || text.contains('\0')
        || text.contains("-----BEGIN") || text.contains('\n') || text.contains('\r')
        || contains_credential(&Value::String(text.into())) {
        bail!("record text must be a short, single-line summary without credentials");
    }
    Ok(redact_credentials(text.trim()))
}

fn safe_list(input: &Value, key: &str, limit: usize) -> Result<Vec<String>> {
    let Some(value) = input.get(key) else { return Ok(Vec::new()); };
    let Some(values) = value.as_array() else { bail!("{key} must be an array"); };
    if values.len() > limit { bail!("{key} exceeds its {limit} item limit"); }
    values.iter().map(|value| safe_text(value.as_str().context("array item must be text")?, 500))
        .collect()
}

fn state(session: &Session, key: &str) -> Value {
    session.capability_state.get(key).cloned().unwrap_or(Value::Null)
}

fn set_state(session: &mut Session, key: &str, value: Value) {
    if !session.capability_state.is_object() {
        session.capability_state = Value::Object(Map::new());
    }
    if let Some(map) = session.capability_state.as_object_mut() {
        map.insert(key.into(), value);
    }
}

fn archive_outcome(session: &mut Session, prior: Value) {
    if prior.is_null() { return; }
    let mut history = state(session, "outcome_history").as_array().cloned().unwrap_or_default();
    history.push(prior);
    if history.len() > 32 { history.drain(..history.len() - 32); }
    set_state(session, "outcome_history", Value::Array(history));
}

pub fn init_outcome(session: &mut Session, objective: &str) {
    if objective.trim().is_empty() { return; }
    let objective = if contains_credential(&Value::String(objective.into())) {
        "[objective contains a credential; content omitted]".to_owned()
    } else { redact_credentials(objective.trim()) };
    let objective = objective.chars().take(4096).collect::<String>();
    let prior = state(session, "outcome_engine");
    if !prior.is_null() && !matches!(prior.get("status").and_then(Value::as_str),
        Some("verified" | "completed" | "cancelled")) {
        let mut continuing = prior;
        if !continuing["follow_up_messages"].is_array() {
            continuing["follow_up_messages"] = json!([]);
        }
        if let Some(messages) = continuing["follow_up_messages"].as_array_mut() {
            messages.push(json!({"at_ms":now_ms(),"message":objective}));
            if messages.len() > 12 { messages.drain(..messages.len() - 12); }
        }
        continuing["updated_at_ms"] = json!(now_ms());
        set_state(session, "outcome_engine", continuing);
        return;
    }
    let id = format!("{}:{}", session.id, session.messages.len());
    archive_outcome(session, prior);
    set_state(session, "outcome_engine", json!({
        "id":id,"objective":objective,"deliverables":[],"criteria":[],
        "assumptions":[],"status":"interpreting","blockers":[],
        "created_at_ms":now_ms(),"updated_at_ms":now_ms(),
    }));
}

fn outcome(ctx: &mut CapabilityContext<'_>, action: &str, input: &Value)
    -> Result<CapabilityResponse> {
    if state(ctx.session, "outcome_engine").is_null() {
        init_outcome(ctx.session, ctx.objective);
    }
    let current = state(ctx.session, "outcome_engine");
    if let Some(proposed) = input.get("objective").and_then(Value::as_str)
        .filter(|proposed| current.get("objective").and_then(Value::as_str) != Some(*proposed)) {
        if proposed.is_empty() || proposed.len() > 4096 || proposed.contains('\0')
            || contains_credential(&Value::String(proposed.into())) {
            bail!("new objective must be bounded text");
        }
        let summary = proposed.chars().take(180).collect::<String>();
        let question = format!("Change this task's objective to: {summary}?");
        if !ctx.events.request_approval(&question)? {
            return Ok(CapabilityResponse::new(CapabilityStatus::AwaitingApproval,
                "Changing the user's objective requires approval", json!({"proposed":summary})));
        }
        archive_outcome(ctx.session, current.clone());
        let id = format!("{}:{}:revision",ctx.session.id,now_ms());
        let updated = json!({"id":id,"objective":redact_credentials(proposed),
            "deliverables":[],"criteria":[],"assumptions":[],"status":"interpreting",
            "blockers":[],"created_at_ms":now_ms(),"updated_at_ms":now_ms()});
        set_state(ctx.session, "outcome_engine", updated.clone());
        ctx.store.save(ctx.session)?;
        return Ok(CapabilityResponse::new(CapabilityStatus::Activated,
            "Approved task objective change recorded; define its new criteria", updated));
    }
    match action {
        "status" => Ok(CapabilityResponse::new(CapabilityStatus::Completed,
            "Current engineering outcome", current)),
        "observe" => {
            let Some(criterion_id) = input.get("criterion_id").and_then(Value::as_str) else {
                return Ok(CapabilityResponse::new(CapabilityStatus::Completed,
                    "Current outcome and outstanding criteria", current));
            };
            let mut record = current;
            let blocker = input.get("blocker").and_then(Value::as_str)
                .map(|text| safe_text(text, 500)).transpose()?;
            let mut observation = Value::Null;
            {
                let criteria = record["criteria"].as_array_mut().context("outcome criteria are invalid")?;
                let criterion = criteria.iter_mut().find(|item|
                    item.get("id").and_then(Value::as_str) == Some(criterion_id))
                    .context("outcome criterion was not found")?;
                if let Some(reference) = input.get("evidence_ref").and_then(Value::as_str) {
                    if reference.is_empty() || reference.len() > 160 { bail!("invalid evidence reference"); }
                    let event = state(ctx.session, "black_box").pointer("/events").and_then(Value::as_array)
                        .and_then(|events| events.iter().find(|event|
                            event.get("id").and_then(Value::as_str) == Some(reference))).cloned();
                    if let Some(event) = event {
                        let status = if event.get("checkpoint_id").is_some()
                            && event.get("status").and_then(Value::as_str) == Some("completed") {
                            "implemented_in_source"
                        } else { "observation_recorded" };
                        criterion["status"] = json!(status);
                        observation = event;
                    } else {
                        let completion = ctx.host.aip(aip_request(json!({"type":"get_completion",
                            "receipt_id":reference})))?;
                        let completed = completion.pointer("/result/completion/status")
                            .and_then(Value::as_str) == Some("COMPLETE")
                            && completion.pointer("/result/head_matches_current")
                                .and_then(Value::as_bool) == Some(true);
                        let check_id = input.get("check_id").and_then(Value::as_str);
                        let check_passed = check_id.is_some_and(|check_id|
                            completion.pointer("/result/completion/passed")
                                .and_then(Value::as_array).is_some_and(|checks| checks.iter().any(|check|
                                    check.as_str() == Some(check_id))));
                        criterion["status"] = json!(if completed && check_passed {
                            "amity_check_passed"
                        } else { "receipt_review_required" });
                        observation = completion;
                    }
                    let refs = criterion["evidence_refs"].as_array_mut()
                        .context("criterion evidence references are invalid")?;
                    if refs.len() >= 16 { bail!("criterion evidence reference limit reached"); }
                    if !refs.iter().any(|item| item.as_str() == Some(reference)) {
                        refs.push(json!(reference));
                    }
                }
                if blocker.is_some() { criterion["status"] = json!("blocked"); }
            }
            if let Some(blocker) = blocker {
                let blockers = record["blockers"].as_array_mut()
                    .context("outcome blockers are invalid")?;
                if blockers.len() < 24 && !blockers.iter().any(|item| item.as_str() == Some(blocker.as_str())) {
                    blockers.push(json!(blocker));
                }
            }
            record["updated_at_ms"] = json!(now_ms());
            if record["criteria"].as_array().is_some_and(|criteria| !criteria.is_empty()
                && criteria.iter().all(|criterion| matches!(criterion.get("status").and_then(Value::as_str),
                    Some("verified" | "amity_check_passed" | "runtime_observed")))) {
                record["status"] = json!("verified");
            }
            set_state(ctx.session, "outcome_engine", record.clone());
            ctx.store.save(ctx.session)?;
            let evidence_refs = input.get("evidence_ref").and_then(Value::as_str)
                .map(|value| vec![value.to_owned()]).unwrap_or_default();
            Ok(CapabilityResponse::new(CapabilityStatus::Completed,
                "Outcome evidence updated; Echo must still assess the actual requested result",
                json!({"outcome":record,"observation":observation})).with_evidence(evidence_refs))
        }
        "activate" => {
            let deliverables = safe_list(input, "deliverables", 24)?;
            let criteria = safe_list(input, "criteria", 32)?;
            let assumptions = safe_list(input, "assumptions", 12)?;
            if deliverables.is_empty() && criteria.is_empty() {
                return Ok(CapabilityResponse::new(CapabilityStatus::Available,
                    "Provide project-specific deliverables or observable criteria to define the outcome",
                    state(ctx.session, "outcome_engine")));
            }
            let mut record = state(ctx.session, "outcome_engine");
            if record.get("status").and_then(Value::as_str) != Some("interpreting") {
                let existing_deliverables = record.get("deliverables").cloned().unwrap_or(json!([]));
                let existing_criteria = record.get("criteria").and_then(Value::as_array)
                    .map(|items| items.iter().filter_map(|item|
                        item.get("description").and_then(Value::as_str)).collect::<Vec<_>>())
                    .unwrap_or_default();
                if existing_deliverables != json!(deliverables)
                    || existing_criteria != criteria.iter().map(String::as_str).collect::<Vec<_>>() {
                    let question = format!("Revise the deliverables and acceptance criteria for '{}'? Proposed deliverables: {}. Criteria: {}.",
                        record.get("objective").and_then(Value::as_str).unwrap_or("this task")
                            .chars().take(120).collect::<String>(),
                        deliverables.join("; ").chars().take(500).collect::<String>(),
                        criteria.join("; ").chars().take(500).collect::<String>());
                    if !ctx.events.request_approval(&question)? {
                        return Ok(CapabilityResponse::new(CapabilityStatus::AwaitingApproval,
                            "Changing established outcome scope requires approval",
                            json!({"current":record,"proposed_deliverables":deliverables,
                                "proposed_criteria":criteria})));
                    }
                    archive_outcome(ctx.session, record.clone());
                    record["id"] = json!(format!("{}:{}:revision",ctx.session.id,now_ms()));
                } else {
                    return Ok(CapabilityResponse::new(CapabilityStatus::Available,
                        "Existing outcome criteria remain active", record));
                }
            }
            record["deliverables"] = json!(deliverables);
            record["criteria"] = Value::Array(criteria.into_iter().enumerate().map(|(index, text)|
                json!({"id":format!("criterion-{}", index + 1),"description":text,
                    "status":"unverified","evidence_refs":[]})).collect());
            record["assumptions"] = json!(assumptions);
            record["status"] = json!("in_progress");
            record["policy_revision"] = json!(policy_revision(ctx.project_context));
            record["updated_at_ms"] = json!(now_ms());
            if input.get("runtime_observation_required").and_then(Value::as_bool) == Some(true) {
                let objective = ctx.objective.to_owned();
                let observation = ctx.invoke("reality_loop", "activate",
                    &json!({"objective":objective}))?;
                record["runtime_observation"] = json!({"status":observation.status,
                    "message":observation.message,"capability":"reality_loop"});
            }
            set_state(ctx.session, "outcome_engine", record.clone());
            ctx.store.save(ctx.session)?;
            Ok(CapabilityResponse::new(CapabilityStatus::Activated,
                "Outcome criteria recorded; completion still requires evidence", record))
        }
        _ => Ok(unsupported_action(action)),
    }
}

fn aip_request(body: Value) -> Value {
    json!({"schema":"amity.aip.v1",
        "request_id":format!("echo-capability:{}:{}:{}",std::process::id(),now_ms(),
            NEXT_REQUEST.fetch_add(1,Ordering::Relaxed)),
        "request":body})
}

fn architecture(ctx: &mut CapabilityContext<'_>, action: &str, input: &Value)
    -> Result<CapabilityResponse> {
    match action {
        "status" => {
            let manifest = ctx.project_context.get("context").cloned().unwrap_or(Value::Null);
            Ok(CapabilityResponse::new(CapabilityStatus::Available,
                "Amity project context is available", json!({"manifest":manifest,
                    "source":"amity_project_intelligence"})))
        }
        "activate" | "observe" => {
            if let Some(nodes) = input.get("changed") {
                let nodes = nodes.as_array().context("changed must be an array of Amity node references")?;
                if nodes.is_empty() || nodes.len() > 16 { bail!("changed requires 1 to 16 nodes"); }
                let reply = ctx.host.aip(aip_request(json!({"type":"query_impact",
                    "changed":nodes,"max_depth":input.get("max_depth").and_then(Value::as_u64)
                        .unwrap_or(3).min(8)})))?;
                return Ok(CapabilityResponse::new(CapabilityStatus::Completed,
                    "Amity change-impact observation", json!({"source":"amity_aip",
                        "observation":reply})));
            }
            if let Some(query) = input.get("query").and_then(Value::as_str) {
                if query.is_empty() || query.len() > 2048 { bail!("architecture query must be 1 to 2048 bytes"); }
                let reply = ctx.host.query_source(query, 12)?;
                return Ok(CapabilityResponse::new(CapabilityStatus::Completed,
                    "Amity source intelligence observation", json!({"source":"amity_aip",
                        "observation":reply})));
            }
            let manifest = ctx.host.context_manifest(&format!("echo:{}:architecture",ctx.session.id))?;
            Ok(CapabilityResponse::new(CapabilityStatus::Completed,
                "Amity architecture context refreshed", json!({"source":"amity_aip",
                    "observation":manifest})))
        }
        _ => Ok(unsupported_action(action)),
    }
}

fn evidence(ctx: &mut CapabilityContext<'_>, action: &str, input: &Value)
    -> Result<CapabilityResponse> {
    match action {
        "status" | "observe" => {
            let local = state(ctx.session, "evidence_ledger");
            let activity = if let Some(task_id) = ctx.session.active_task.as_deref() {
                Some(ctx.host.agent_tool(Some(task_id), json!({"type":"activity"}))?)
            } else { None };
            let flight = if input.get("include_flight").and_then(Value::as_bool) == Some(true) {
                Some(ctx.host.flight_events()?)
            } else { None };
            let (receipt, completion) = if let Some(receipt_id) = input.get("receipt_id").and_then(Value::as_str) {
                if receipt_id.is_empty() || receipt_id.len() > 160 { bail!("invalid receipt ID"); }
                (Some(ctx.host.aip(aip_request(json!({"type":"get_receipt",
                    "receipt_id":receipt_id})))?),
                Some(ctx.host.aip(aip_request(json!({"type":"get_completion",
                    "receipt_id":receipt_id})))?))
            } else { (None, None) };
            let evidence_refs = input.get("receipt_id").and_then(Value::as_str)
                .map(|value| vec![value.to_owned()]).unwrap_or_default();
            Ok(CapabilityResponse::new(CapabilityStatus::Completed,
                "Evidence and verification state from Echo and Amity",
                json!({"claims":local.get("claims").cloned().unwrap_or(json!([])),
                    "observations":local.get("observations").cloned().unwrap_or(json!([])),
                    "amity_activity":activity,"amity_flight":flight,"amity_receipt":receipt,
                    "amity_completion":completion,
                    "source":"amity_task_runtime"})).with_evidence(evidence_refs))
        }
        "activate" => {
            let statement = safe_text(input.get("statement").and_then(Value::as_str)
                .context("evidence claim requires statement")?, 500)?;
            let criterion_id = input.get("criterion_id").and_then(Value::as_str)
                .map(|id| safe_text(id, 64)).transpose()?;
            let mut ledger = state(ctx.session, "evidence_ledger");
            if !ledger.is_object() { ledger = json!({"claims":[],"observations":[]}); }
            let claims = ledger["claims"].as_array_mut().context("invalid evidence ledger")?;
            if claims.len() >= 128 { bail!("evidence ledger is full; archive or retire old claims first"); }
            let id = format!("{}:claim-{}",ctx.session.id,claims.len() + 1);
            let claim = json!({"id":id,"statement":statement,"criterion_id":criterion_id,
                "status":"not_verified","evidence_refs":[],
                "task_id":ctx.session.active_task,"at_ms":now_ms(),
                "policy_revision":policy_revision(ctx.project_context)});
            claims.push(claim.clone());
            set_state(ctx.session, "evidence_ledger", ledger);
            ctx.store.save(ctx.session)?;
            Ok(CapabilityResponse::new(CapabilityStatus::Activated,
                "Claim recorded as unverified until Amity evidence supports it", claim))
        }
        _ => Ok(unsupported_action(action)),
    }
}

fn recovery(ctx: &mut CapabilityContext<'_>, action: &str, input: &Value)
    -> Result<CapabilityResponse> {
    match action {
        "status" | "observe" | "activate" => {
            let reply = ctx.host.agent_tool(None, json!({"type":"checkpoints"}))?;
            if reply.get("ok").and_then(Value::as_bool) != Some(true) {
                return Ok(CapabilityResponse::new(CapabilityStatus::Unavailable,
                    "Amity checkpoints are unavailable for this project", reply));
            }
            let checkpoints = reply.pointer("/result/checkpoints").cloned().unwrap_or(json!([]));
            let mut data = json!({"checkpoints":checkpoints,
                "session_change_relationships":state(ctx.session,"recovery_changes"),
                "recovery_boundary":"Amity will revalidate and refuse unsafe restoration"});
            if let Some(checkpoint_id) = input.get("checkpoint_id").and_then(Value::as_str) {
                let found = checkpoints.as_array().is_some_and(|items| items.iter().any(|item|
                    item.get("checkpoint_id").and_then(Value::as_str) == Some(checkpoint_id)));
                if !found { return Ok(CapabilityResponse::new(CapabilityStatus::Unavailable,
                    "The requested checkpoint is not available", data)); }
                data["next_governed_tool"] = json!({"tool":"undo_checkpoint",
                    "arguments":{"checkpoint_id":checkpoint_id}});
                return Ok(CapabilityResponse::new(CapabilityStatus::AwaitingApproval,
                    "Restoration requires a separate Amity-governed proposal and approval", data)
                    .with_evidence(vec![checkpoint_id.to_owned()]));
            }
            Ok(CapabilityResponse::new(CapabilityStatus::Completed,
                "Available Amity checkpoints", data))
        }
        _ => Ok(unsupported_action(action)),
    }
}

fn policy_revision(context: &Value) -> Option<Value> {
    context.pointer("/active_policy/result/revision").cloned()
}

/// The parent agent calls this for actual tool and policy transitions. Only a
/// bounded metadata allowlist is stored: no raw prompts, source, or tool output.
pub fn record_session_event(session: &mut Session, event: Value) {
    let kind = event.get("kind").and_then(Value::as_str).unwrap_or("event");
    let mut record = Map::new();
    let mut box_state = state(session, "black_box");
    if !box_state.is_object() { box_state = json!({"next_seq":1,"events":[]}); }
    let sequence = box_state.get("next_seq").and_then(Value::as_u64).unwrap_or(1);
    record.insert("id".into(), json!(format!("{}:{sequence}", session.id)));
    record.insert("at_ms".into(), json!(now_ms()));
    record.insert("kind".into(), json!(safe_field(kind)));
    record.insert("protocol".into(), json!("echo.capabilities.v1"));
    let current_outcome = state(session, "outcome_engine");
    if let Some(objective) = current_outcome.get("objective").and_then(Value::as_str) {
        record.insert("objective_digest".into(), json!(format!("{:x}",
            Sha256::digest(objective.as_bytes()))));
    }
    if let Some(model) = session.model.as_deref() {
        record.insert("model".into(), json!(safe_field(model)));
    }
    for key in ["tool", "status", "task_id", "policy_revision", "evidence_ref",
        "receipt_id", "proposal_id", "checkpoint_id", "error_code", "capability",
        "observation_digest", "model", "resource_path", "base_revision",
        "denial_id", "operation", "decision"] {
        if let Some(value) = event.get(key).and_then(Value::as_str) {
            record.insert(key.into(), json!(safe_field(value)));
        }
    }
    let record = Value::Object(record);
    if !box_state["events"].is_array() { box_state["events"] = json!([]); }
    let events = box_state["events"].as_array_mut();
    if let Some(events) = events {
        events.push(record.clone());
        if events.len() > 512 { events.drain(..events.len() - 512); }
    }
    box_state["next_seq"] = json!(sequence.saturating_add(1));
    set_state(session, "black_box", box_state);
    if record.get("tool").is_some() {
        let mut ledger = state(session, "evidence_ledger");
        if !ledger.is_object() { ledger = json!({"claims":[],"observations":[]}); }
        if !ledger["observations"].is_array() { ledger["observations"] = json!([]); }
        if let Some(observations) = ledger["observations"].as_array_mut() {
            let classification = if record.get("receipt_id").is_some()
                || record.get("checkpoint_id").is_some() {
                "execution_receipt_recorded"
            } else { "tool_result_observed" };
            observations.push(json!({"event_id":record.get("id"),"tool":record.get("tool"),
                "status":record.get("status"),"classification":classification,
                "receipt_id":record.get("receipt_id"),"proposal_id":record.get("proposal_id"),
                "checkpoint_id":record.get("checkpoint_id"),
                "task_id":record.get("task_id"),"at_ms":record.get("at_ms")}));
            if observations.len() > 256 { observations.drain(..observations.len() - 256); }
        }
        set_state(session, "evidence_ledger", ledger);
        if record.get("checkpoint_id").is_some() || record.get("proposal_id").is_some() {
            let mut changes = state(session, "recovery_changes").as_array().cloned().unwrap_or_default();
            changes.push(json!({"event_id":record.get("id"),"task_id":record.get("task_id"),
                "proposal_id":record.get("proposal_id"),"checkpoint_id":record.get("checkpoint_id"),
                "objective_digest":record.get("objective_digest"),
                "resource_path":record.get("resource_path"),
                "base_revision":record.get("base_revision"),
                "result_revision":record.get("observation_digest"),
                "at_ms":record.get("at_ms")}));
            if changes.len() > 128 { changes.drain(..changes.len() - 128); }
            set_state(session, "recovery_changes", Value::Array(changes));
        }
    }
}

fn safe_field(text: &str) -> String {
    if contains_credential(&Value::String(text.into())) { return "[redacted]".into(); }
    redact_credentials(text).chars().filter(|ch| !ch.is_control())
        .take(160).collect()
}

fn black_box(ctx: &mut CapabilityContext<'_>, action: &str, input: &Value)
    -> Result<CapabilityResponse> {
    match action {
        "status" | "observe" | "activate" => {
            if input.get("mode").and_then(Value::as_str) == Some("experimental") {
                return Ok(CapabilityResponse::new(CapabilityStatus::Unsupported,
                    "Experimental replay requires a separately authorized isolated execution task",
                    json!({"historical_replay_available":true})));
            }
            let events = state(ctx.session, "black_box").get("events").cloned().unwrap_or(json!([]));
            let limit = input.get("limit").and_then(Value::as_u64).unwrap_or(50).min(200) as usize;
            let total = events.as_array().map_or(0, Vec::len);
            let selected = events.as_array().map(|items| items.iter().skip(total.saturating_sub(limit))
                .cloned().collect::<Vec<_>>()).unwrap_or_default();
            Ok(CapabilityResponse::new(CapabilityStatus::Completed,
                "Historical activity reconstructed from recorded metadata; no operations were replayed",
                json!({"mode":"historical","events":selected,"total_retained":total,
                    "experimental_replay":"requires a separately authorized task"})))
        }
        _ => Ok(unsupported_action(action)),
    }
}

fn memory_path(project_id: &str) -> PathBuf {
    let digest = Sha256::digest(project_id.as_bytes());
    crate::config::Config::home_dir().join("capabilities").join("memory")
        .join(format!("{digest:x}.json"))
}

fn load_memory(project_id: &str) -> Result<Value> {
    let path = memory_path(project_id);
    match fs::read(&path) {
        Ok(bytes) => {
            let value: Value = serde_json::from_slice(&bytes)
                .with_context(|| format!("reading engineering memory {}",path.display()))?;
            if value.get("project_id").and_then(Value::as_str) != Some(project_id) {
                bail!("engineering memory belongs to a different project");
            }
            Ok(value)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(json!({
            "schema":"echo.capabilities.v1.memory","project_id":project_id,"records":[]})),
        Err(error) => Err(error).with_context(|| format!("reading engineering memory {}",path.display())),
    }
}

fn save_memory(project_id: &str, value: &Value) -> Result<()> {
    write_json_atomic(&memory_path(project_id), value).context("saving engineering memory")
}

fn memory(ctx: &mut CapabilityContext<'_>, action: &str, input: &Value)
    -> Result<CapabilityResponse> {
    let current_config = crate::config::Config::load()
        .context("loading current engineering memory settings")?;
    let enabled = current_config.capabilities.get("projects")
        .and_then(|projects| projects.get(&ctx.session.project_id))
        .and_then(|project| project.get("engineering_memory"))
        .and_then(|settings| settings.get("enabled"))
        .and_then(Value::as_bool)
        .or_else(|| current_config.capabilities.pointer("/engineering_memory/enabled")
            .and_then(Value::as_bool)).unwrap_or(true);
    if !enabled {
        return Ok(CapabilityResponse::new(CapabilityStatus::Unavailable,
            "Project engineering memory is disabled in Echo settings", Value::Null));
    }
    let project_id = &ctx.session.project_id;
    let mut file = load_memory(project_id)?;
    let record_count = file.get("records").and_then(Value::as_array)
        .context("invalid engineering memory records")?.len();
    match action {
        "status" => Ok(CapabilityResponse::new(CapabilityStatus::Available,
            "Local project engineering memory is enabled",
            json!({"count":record_count,"storage":"local_project_only"}))),
        "observe" => {
            let query = input.get("query").and_then(Value::as_str).unwrap_or(ctx.objective);
            let words = query.to_ascii_lowercase().split(|ch:char| !ch.is_alphanumeric())
                .filter(|part| part.len() >= 4).map(str::to_owned).collect::<Vec<_>>();
            let records = file.get("records").and_then(Value::as_array)
                .context("invalid engineering memory records")?;
            let mut ranked = records.iter().filter(|record|
                record.get("superseded").and_then(Value::as_bool) != Some(true))
                .map(|record| {
                    let summary = record.get("summary").and_then(Value::as_str).unwrap_or("").to_ascii_lowercase();
                    let score = words.iter().filter(|word| summary.contains(word.as_str())).count();
                    (score, record)
                }).filter(|(score, _)| *score > 0).collect::<Vec<_>>();
            ranked.sort_by(|a,b| b.0.cmp(&a.0));
            let selected = ranked.into_iter().take(8).map(|(_,record)| record.clone()).collect::<Vec<_>>();
            Ok(CapabilityResponse::new(CapabilityStatus::Completed,
                "Relevant prior project knowledge; validate it against current Amity context",
                json!({"records":selected,"authority":"memory_is_not_policy"})))
        }
        "activate" => {
            let operation = input.get("operation").and_then(Value::as_str).unwrap_or("remember");
            match operation {
                "remember" => {
                    let summary = safe_text(input.get("summary").and_then(Value::as_str)
                        .context("memory requires a summary")?, 500)?;
                    let kind = input.get("kind").and_then(Value::as_str).unwrap_or("observation");
                    if !matches!(kind,"fact"|"preference"|"observation"|"inference"|"decision"|"lesson") {
                        bail!("unsupported memory kind");
                    }
                    let source = safe_text(input.get("source").and_then(Value::as_str)
                        .context("memory requires a source reference")?, 160)?;
                    let records = file["records"].as_array_mut().context("invalid memory records")?;
                    if records.len() >= 500 { bail!("project memory is full; retire old records first"); }
                    let id = format!("{}:memory-{}",project_id,now_ms());
                    let record = json!({"id":id,"scope":"project","kind":kind,"summary":summary,
                        "source":source,"source_validated":false,"confidence":"unverified",
                        "created_at_ms":now_ms(),
                        "last_validated_ms":Value::Null,"validity_revision":policy_revision(ctx.project_context),
                        "superseded":false,"evidence_refs":safe_list(input,"evidence_refs",8)?});
                    records.push(record.clone());
                    save_memory(project_id, &file)?;
                    Ok(CapabilityResponse::new(CapabilityStatus::Completed,
                        "Project knowledge saved locally", record))
                }
                "correct" | "retire" | "delete" => {
                    let id = input.get("id").and_then(Value::as_str).context("memory record ID is required")?;
                    let records = file["records"].as_array_mut().context("invalid memory records")?;
                    let position = records.iter().position(|record| record.get("id").and_then(Value::as_str)==Some(id))
                        .context("memory record was not found")?;
                    if operation == "delete" {
                        records.remove(position);
                    } else if operation == "retire" {
                        records[position]["superseded"] = json!(true);
                    } else {
                        let summary = safe_text(input.get("summary").and_then(Value::as_str)
                            .context("corrected summary is required")?, 500)?;
                        records[position]["summary"] = json!(summary);
                        records[position]["last_validated_ms"] = json!(now_ms());
                        records[position]["confidence"] = json!("unverified");
                    }
                    save_memory(project_id, &file)?;
                    let message = match operation {
                        "correct" => "Project memory record corrected",
                        "retire" => "Project memory record retired",
                        _ => "Project memory record deleted",
                    };
                    Ok(CapabilityResponse::new(CapabilityStatus::Completed,
                        message, json!({"id":id})))
                }
                _ => Ok(unsupported_action(operation)),
            }
        }
        "configure" => Ok(CapabilityResponse::new(CapabilityStatus::Available,
            "Manage engineering memory retention through Echo's Capabilities settings",
            json!({"enabled":enabled,"storage":"local_project_only"}))),
        _ => Ok(unsupported_action(action)),
    }
}

fn unsupported_action(action: &str) -> CapabilityResponse {
    CapabilityResponse::new(CapabilityStatus::Unsupported,
        format!("Capability action '{action}' is unsupported"), Value::Null)
}
