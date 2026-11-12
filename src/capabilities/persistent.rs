//! User-controlled project observation and evidence-based experience retention.
use super::{CapabilityContext, CapabilityResponse, CapabilityStatus};
use crate::config::{Config, ProjectRegistration};
use crate::host::HostClient;
use crate::state::{SessionStatus, SessionStore, contains_credential, redact_credentials, write_json_atomic};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn invoke(ctx: &mut CapabilityContext<'_>, id: &str, action: &str, input: &Value) -> Result<CapabilityResponse> {
    if ctx.cancel.load(Ordering::Relaxed) {
        return Ok(CapabilityResponse::new(CapabilityStatus::Cancelled, "Operation cancelled.", Value::Null));
    }
    match id {
        "project_steward" => steward(ctx, action, input),
        "trust_certification" => trust(ctx, action, input),
        "experience_loop" => experience(ctx, action, input),
        _ => Ok(CapabilityResponse::new(CapabilityStatus::Unsupported, "Unknown persistent capability.", Value::Null)),
    }
}

fn project_path(category: &str, project_id: &str) -> PathBuf {
    let hash = format!("{:x}", Sha256::digest(project_id.as_bytes()));
    Config::home_dir().join("capabilities").join(category).join(format!("{hash}.json"))
}

fn model_path(project_id: &str, model: &str) -> PathBuf {
    let key = format!("{project_id}\0{model}");
    let hash = format!("{:x}", Sha256::digest(key.as_bytes()));
    Config::home_dir().join("capabilities").join("trust").join(format!("{hash}.json"))
}

fn read_record(path: &PathBuf) -> Result<Value> {
    match fs::read(path) {
        Ok(bytes) => {
            if bytes.len() > 2 * 1024 * 1024 { bail!("capability record exceeds 2 MiB"); }
            Ok(serde_json::from_slice(&bytes).with_context(|| format!("reading {}", path.display()))?)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Value::Null),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}

fn save_record(path: &PathBuf, value: &Value) -> Result<()> {
    write_json_atomic(path, value).with_context(|| format!("saving {}", path.display()))?;
    Ok(())
}

fn append_trust_profile(path: &PathBuf, profile: Value) -> Result<()> {
    let mut saved = read_record(path)?;
    if !saved.is_object() { saved = json!({"profiles":[]}); }
    if saved.get("profiles").is_none() {
        let previous = saved;
        saved = json!({"profiles":[previous]});
    }
    let profiles = saved.get_mut("profiles").and_then(Value::as_array_mut)
        .context("trust profile store has no profiles array")?;
    profiles.push(profile);
    if profiles.len() > 32 { profiles.drain(..profiles.len() - 32); }
    save_record(path, &saved)
}

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()
}

fn enabled_for(config: &Config, project_id: &str, capability: &str) -> bool {
    let current = Config::load().map(|saved| saved.capabilities)
        .unwrap_or_else(|_| config.capabilities.clone());
    current.get("projects").and_then(|v| v.get(project_id))
        .and_then(|v| v.get("overrides")).and_then(|v| v.get(capability))
        .and_then(|v| v.get("enabled"))
        .and_then(Value::as_bool)
        .or_else(|| current.get("overrides").and_then(|v| v.get(capability))
            .and_then(|v| v.get("enabled")).and_then(Value::as_bool))
        .unwrap_or(false)
}

fn retention_enabled(config: &Config, project_id: &str) -> bool {
    let current = Config::load().map(|saved| saved.capabilities)
        .unwrap_or_else(|_| config.capabilities.clone());
    current.get("projects").and_then(|v| v.get(project_id))
        .and_then(|v| v.get("overrides")).and_then(|v| v.get("experience_loop"))
        .and_then(|v| v.get("retain"))
        .and_then(Value::as_bool)
        .or_else(|| current.pointer("/privacy/experience_retention").and_then(Value::as_bool))
        .unwrap_or(false)
}

fn set_project_options(project_id: &str, capability: &str,
    options: &[(&str, Value)]) -> Result<Value> {
    let mut config = Config::load()?;
    if !config.projects.iter().any(|project| project.id == project_id) {
        bail!("project is not registered in Echo settings");
    }
    if !config.capabilities.is_object() { config.capabilities = json!({}); }
    let projects = config.capabilities.as_object_mut().context("capability settings must be an object")?
        .entry("projects").or_insert_with(|| json!({}));
    if !projects.is_object() { *projects = json!({}); }
    let project = projects.as_object_mut().context("project settings must be an object")?
        .entry(project_id).or_insert_with(|| json!({}));
    if !project.is_object() { *project = json!({}); }
    let overrides = project.as_object_mut().context("project settings must be an object")?
        .entry("overrides").or_insert_with(|| json!({}));
    if !overrides.is_object() { *overrides = json!({}); }
    let selected = overrides.as_object_mut().context("project overrides must be an object")?
        .entry(capability).or_insert_with(|| json!({}));
    if !selected.is_object() { *selected = json!({}); }
    let fields = selected.as_object_mut().context("capability override must be an object")?;
    for (key, value) in options { fields.insert((*key).to_owned(), value.clone()); }
    let result = Value::Object(fields.clone());
    config.save()?;
    Ok(result)
}

fn auto_monitor_for(config: &Config, project_id: &str) -> bool {
    let current = Config::load().map(|saved| saved.capabilities)
        .unwrap_or_else(|_| config.capabilities.clone());
    current.get("projects").and_then(|v| v.get(project_id))
        .and_then(|v| v.get("overrides")).and_then(|v| v.get("project_steward"))
        .and_then(|v| v.get("auto_monitor")).and_then(Value::as_bool)
        .or_else(|| current.pointer("/overrides/project_steward/auto_monitor").and_then(Value::as_bool))
        .unwrap_or(false)
}

fn steward(ctx: &mut CapabilityContext<'_>, action: &str, input: &Value) -> Result<CapabilityResponse> {
    let enabled = enabled_for(ctx.config, &ctx.session.project_id, "project_steward");
    let path = project_path("stewardship", &ctx.session.project_id);
    if action == "status" {
        return Ok(CapabilityResponse::new(CapabilityStatus::Available,
            if enabled && auto_monitor_for(ctx.config, &ctx.session.project_id) {
                "Scheduled project monitoring is enabled while Echo is open."
            } else if enabled { "Project Steward is enabled on request; scheduled monitoring is off." }
            else { "Project monitoring is off." },
            json!({"enabled":enabled,"auto_monitor":auto_monitor_for(ctx.config, &ctx.session.project_id),
                "lifecycle":"while_echo_is_open","record":read_record(&path)?})));
    }
    if action == "configure" {
        let auto = input.get("auto_monitor").and_then(Value::as_bool);
        let interval = input.get("interval_seconds").and_then(Value::as_u64);
        if auto.is_none() && interval.is_none() {
            return Ok(CapabilityResponse::new(CapabilityStatus::Unsupported,
                "Set auto_monitor and an optional interval_seconds to configure project monitoring.", Value::Null));
        }
        if input.get("auto_monitor").is_some() && auto.is_none()
            || input.get("interval_seconds").is_some() && interval.is_none_or(|seconds| seconds < 300 || seconds > 30 * 24 * 60 * 60) {
            bail!("auto_monitor must be a boolean and interval_seconds must be between 300 and 2592000");
        }
        if !ctx.events.request_approval("Save this project's scheduled monitoring preference? Monitoring only runs while Echo is open and cannot change code.")? {
            return Ok(CapabilityResponse::new(CapabilityStatus::AwaitingApproval,
                "Project monitoring settings need your approval.", Value::Null));
        }
        let mut options = Vec::new();
        if let Some(auto) = auto { options.push(("auto_monitor", json!(auto))); }
        if let Some(interval) = interval { options.push(("interval_seconds", json!(interval))); }
        if auto == Some(true) { options.push(("enabled", json!(true))); }
        let result = set_project_options(&ctx.session.project_id, "project_steward", &options)?;
        return Ok(CapabilityResponse::new(CapabilityStatus::Completed,
            "Project monitoring preference saved. Scheduled checks run only while Echo is open.", result));
    }
    if action != "activate" && action != "observe" {
        return Ok(CapabilityResponse::new(CapabilityStatus::Unsupported,
            "Project Steward supports opt-in monitoring, status, and on-demand observation.", Value::Null));
    }
    if !enabled {
        return Ok(CapabilityResponse::new(CapabilityStatus::Paused,
            "Enable Project Steward for this project in Capabilities settings before monitoring.",
            json!({"enabled":false,"background_work":false})));
    }
    let project = ctx.config.projects.iter().find(|project| project.id == ctx.session.project_id)
        .context("registered project is missing")?;
    let record = inspect_project(ctx.config, project, ctx.cancel)?;
    Ok(CapabilityResponse::new(CapabilityStatus::Completed,
        "Project health observations were refreshed from Amity and saved locally. Findings are proposals only.", record))
}

fn inspect_project(config: &Config, project: &ProjectRegistration, cancel: &AtomicBool) -> Result<Value> {
    let path = project_path("stewardship", &project.id);
    let previous = read_record(&path)?;
    let host = HostClient::new(config.host_executable.clone(), project.effective_host_config()?.to_path_buf(),
        Duration::from_secs(config.resources.host_timeout_seconds))?;
    if cancel.load(Ordering::Relaxed) { bail!("project monitoring cancelled"); }
    let status = host.project_status()?;
    let head = status.pointer("/result/head/fingerprint").and_then(Value::as_str).unwrap_or("");
    let prior_head = previous.get("head").and_then(Value::as_str).unwrap_or("");
    let mut findings = Vec::new();
    if !prior_head.is_empty() && !head.is_empty() && head != prior_head {
        findings.push(json!({"category":"source_change","severity":"info",
            "message":"The authoritative Amity project revision changed since the previous observation. Review impact before calling it architectural drift.",
            "previous_head":prior_head,"current_head":head}));
    }
    let store = SessionStore::new(config)?;
    let sessions = store.list(Some(&project.id))?;
    let incomplete = sessions.iter().filter(|session| matches!(&session.status,
        SessionStatus::Running | SessionStatus::Interrupted | SessionStatus::WaitingApproval
        | SessionStatus::AwaitingUserDecision | SessionStatus::Failed)).count();
    if incomplete > 0 {
        findings.push(json!({"category":"incomplete_objectives","severity":"attention",
            "count":incomplete,"message":"Some conversations have unfinished or blocked engineering work."}));
    }
    let gaps = sessions.iter().flat_map(|session| session.capability_state.pointer("/evidence_ledger/claims")
        .and_then(Value::as_array).into_iter().flat_map(|claims| claims.iter()))
        .filter(|claim| matches!(claim.get("status").and_then(Value::as_str),
            Some("not_verified" | "verification_unavailable" | "verification_prohibited"))).count();
    if gaps > 0 {
        findings.push(json!({"category":"verification_gaps","severity":"attention",
            "count":gaps,"message":"Stored evidence claims contain unverified outcomes."}));
    }
    let record = json!({"project_id":project.id,"last_checked_ms":now_ms(),"head":head,
        "source":"amity_project_status_and_echo_sessions","findings":findings,
        "monitored_categories":["source_change","incomplete_objectives","verification_gaps"],
        "unsupported_categories":["dependency_updates","performance_regressions","runtime_failures_without_receipts"],
        "no_automatic_mutation":true});
    save_record(&path, &record)?;
    Ok(record)
}

/// A bounded, read-only local scheduler. It runs only while Echo's process is alive.
pub fn start_steward_monitor(config: &Config, stop: Arc<AtomicBool>) -> Option<JoinHandle<()>> {
    let active_id = config.active_project.as_deref()?;
    if !enabled_for(config, active_id, "project_steward") { return None; }
    if !auto_monitor_for(config, active_id) { return None; }
    let project = config.projects.iter().find(|project| project.id == active_id)?.clone();
    Some(thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            let Ok(current) = Config::load() else { break; };
            if !enabled_for(&current, &project.id, "project_steward") || !auto_monitor_for(&current, &project.id) { break; }
            let interval = current.capabilities.get("projects").and_then(|v| v.get(&project.id))
                .and_then(|v| v.get("overrides")).and_then(|v| v.get("project_steward"))
                .and_then(|v| v.get("interval_seconds")).and_then(Value::as_u64)
                .or_else(|| current.capabilities.pointer("/overrides/project_steward/interval_seconds")
                    .and_then(Value::as_u64))
                .unwrap_or(7 * 24 * 60 * 60).clamp(300, 30 * 24 * 60 * 60);
            let path = project_path("stewardship", &project.id);
            let previous = read_record(&path).unwrap_or(Value::Null);
            let last = previous.get("last_checked_ms").and_then(Value::as_u64).unwrap_or(0) as u128;
            if now_ms().saturating_sub(last) >= u128::from(interval) * 1000 {
                let active = current.projects.iter().find(|item| item.id == project.id)
                    .unwrap_or(&project);
                if let Err(error) = inspect_project(&current, active, &stop) {
                    let failed = json!({"project_id":project.id,"last_checked_ms":now_ms(),
                        "state":"failed","error":redact_credentials(&format!("{error:#}")),
                        "findings":[],"source":"local_scheduler"});
                    let _ = save_record(&path, &failed);
                }
            }
            for _ in 0..30 {
                if stop.load(Ordering::Relaxed) { break; }
                thread::sleep(Duration::from_secs(1));
            }
        }
    }))
}

fn trust(ctx: &mut CapabilityContext<'_>, action: &str, input: &Value) -> Result<CapabilityResponse> {
    let path = model_path(&ctx.session.project_id, &ctx.model.version);
    if action == "status" {
        return Ok(CapabilityResponse::new(CapabilityStatus::Available,
            "Recorded model behavior profile. This never grants permissions.", read_record(&path)?));
    }
    if action != "activate" {
        return Ok(CapabilityResponse::new(CapabilityStatus::Unsupported,
            "Trust Certification currently evaluates recorded, observable behavior on explicit request.", Value::Null));
    }
    let enabled = enabled_for(ctx.config, &ctx.session.project_id, "trust_certification");
    if !enabled {
        return Ok(CapabilityResponse::new(CapabilityStatus::Paused,
            "Enable model evaluation in Capabilities settings before evaluating recorded behavior.",
            json!({"enabled":false,"automatic_evaluation":false})));
    }
    if input.get("mode").and_then(Value::as_str) == Some("protocol_evaluation") {
        let requested = input.get("max_scenarios").and_then(Value::as_u64).unwrap_or(4).clamp(1, 4) as usize;
        if !ctx.events.request_approval(&format!(
            "Run up to {requested} isolated read-only model protocol scenarios? This uses the selected model and may incur cost."))? {
            return Ok(CapabilityResponse::new(CapabilityStatus::AwaitingApproval,
                "Model protocol evaluation requires your approval.",
                json!({"max_scenarios":requested,"model_calls_authorized":false})));
        }
        let result = super::evaluation::evaluate(ctx, requested)?;
        if result.status != CapabilityStatus::Completed { return Ok(result); }
        let profile = json!({"protocol":"echo.capabilities.v1","model":ctx.model.version,
            "project_id":ctx.session.project_id,"evaluated_at_ms":now_ms(),
            "evaluation":result.data.clone(),"permission_effect":"none"});
        append_trust_profile(&path, profile.clone())?;
        return Ok(CapabilityResponse::new(result.status.clone(),
            "Isolated model protocol evaluation recorded. The measured scope grants no project permission.",
            profile));
    }
    let events = ctx.session.capability_state.pointer("/black_box/events")
        .and_then(Value::as_array).cloned().unwrap_or_default();
    if events.is_empty() {
        return Ok(CapabilityResponse::new(CapabilityStatus::Unavailable,
            "No recorded agent events are available for this model and session. No score was assigned.",
            json!({"evaluated_events":0,"score":null})));
    }
    let selected_profile = ctx.session.model.as_deref();
    let model_events: Vec<&Value> = events.iter().filter(|event| event.get("model")
        .and_then(Value::as_str).is_some_and(|model| model == ctx.model.version
            || selected_profile == Some(model))).collect();
    if model_events.is_empty() {
        return Ok(CapabilityResponse::new(CapabilityStatus::Unavailable,
            "No recorded events match the selected model version. No score was assigned.",
            json!({"evaluated_events":0,"score":null})));
    }
    let denied = model_events.iter().filter(|event| event.get("status").and_then(Value::as_str)
        .is_some_and(|status| status.contains("denied"))).count();
    let failed = model_events.iter().filter(|event| event.get("status").and_then(Value::as_str)
        .is_some_and(|status| status.contains("failed"))).count();
    let completed = model_events.iter().filter(|event| event.get("status").and_then(Value::as_str)
        .is_some_and(|status| status.contains("completed") || status.contains("executed"))).count();
    let profile = json!({"protocol":"echo.capabilities.v1","model":ctx.model.version,
        "configured_profile":selected_profile,
        "project_id":ctx.session.project_id,"dataset_version":"observed-session-events-v1",
        "evaluated_at_ms":now_ms(),"evaluated_events":model_events.len(),
        "observed_counts":{"denied":denied,"failed":failed,"completed":completed},
        "capability_selection":null,"authorization_compliance":null,"task_completion_accuracy":null,
        "evidence_accuracy":null,"multi_step_reliability":null,"resource_consumption":null,
        "score":null,"evaluation_conditions":"retrospective local event inspection; no isolated challenge run",
        "permission_effect":"none"});
    append_trust_profile(&path, profile.clone())?;
    Ok(CapabilityResponse::new(CapabilityStatus::Completed,
        "Observed model behavior counts were recorded. Reliability and safety remain unevaluated; this profile grants no permission.", profile))
}

fn experience(ctx: &mut CapabilityContext<'_>, action: &str, input: &Value) -> Result<CapabilityResponse> {
    if action == "observe" && input.get("operation").and_then(Value::as_str) == Some("training_specification") {
        return Ok(CapabilityResponse::new(CapabilityStatus::Completed,
            "Versioned model-independent training contract.", super::training::specification(ctx.config)));
    }
    if action == "observe" && input.get("operation").and_then(Value::as_str) == Some("export_training") {
        return super::training::export_approved(ctx, input);
    }
    let project_id = ctx.session.project_id.clone();
    let path = project_path("experience", &project_id);
    let enabled = enabled_for(ctx.config, &project_id, "experience_loop")
        && retention_enabled(ctx.config, &project_id);
    if action == "configure" {
        if matches!(input.get("operation").and_then(Value::as_str),
            Some("retire_record" | "delete_record")) {
            let operation = input.get("operation").and_then(Value::as_str).unwrap_or_default();
            let record_id = input.get("record_id").and_then(Value::as_str)
                .context("experience record ID is required")?;
            let mut saved = read_record(&path)?;
            let records = saved.get_mut("records").and_then(Value::as_array_mut)
                .context("no local experience records are available")?;
            let index = records.iter().position(|record| record.get("id").and_then(Value::as_str)
                == Some(record_id)).context("experience record was not found")?;
            if !ctx.events.request_approval(if operation == "delete_record" {
                "Delete this local project experience record and its session training candidate?"
            } else { "Retire this local experience record so future models no longer retrieve it?" })? {
                return Ok(CapabilityResponse::new(CapabilityStatus::AwaitingApproval,
                    "Experience record change needs your approval.", Value::Null));
            }
            if operation == "delete_record" { records.remove(index); }
            else { records[index]["retired"] = json!(true); }
            save_record(&path, &saved)?;
            if let Some(training) = ctx.session.capability_state.get_mut("training_records")
                .and_then(Value::as_array_mut) {
                if operation == "delete_record" {
                    training.retain(|record| record.get("id").and_then(Value::as_str)
                        != Some(record_id));
                } else if let Some(record) = training.iter_mut().find(|record|
                    record.get("id").and_then(Value::as_str) == Some(record_id)) {
                    record["approved_for_export"] = json!(false);
                    record["permission_metadata"] = json!({"export_allowed":false});
                }
                ctx.store.save(ctx.session)?;
            }
            return Ok(CapabilityResponse::new(CapabilityStatus::Completed,
                if operation == "delete_record" { "Local experience record deleted." }
                else { "Local experience record retired." },
                json!({"record_id":record_id,"operation":operation,"uploaded":false})));
        }
        if let Some(retain) = input.get("retain").and_then(Value::as_bool) {
            if !ctx.events.request_approval(if retain {
                "Enable local project experience retention? Records stay on this device and are not exported automatically."
            } else { "Turn off local project experience retention? Existing local records remain until deleted." })? {
                return Ok(CapabilityResponse::new(CapabilityStatus::AwaitingApproval,
                    "Experience retention preference needs your approval.", Value::Null));
            }
            let options = [("retain", json!(retain)), ("enabled", json!(retain))];
            let result = set_project_options(&project_id, "experience_loop", &options)?;
            return Ok(CapabilityResponse::new(CapabilityStatus::Completed,
                "Local project experience retention preference saved.", result));
        }
        if input.get("operation").and_then(Value::as_str) == Some("approve_training_record") {
            let record_id = input.get("record_id").and_then(Value::as_str)
                .context("training record ID is required")?;
            let license = input.get("license").and_then(Value::as_str)
                .filter(|license| !license.is_empty() && license.len() <= 120)
                .context("an explicit licensing or permission label is required")?;
            if contains_credential(&json!(license)) { bail!("license label contains a credential"); }
            let records = ctx.session.capability_state.get_mut("training_records")
                .and_then(Value::as_array_mut).context("no local training records are available")?;
            let selected = records.iter_mut().find(|record| record.get("id").and_then(Value::as_str)
                == Some(record_id)).context("training record ID was not found in this session")?;
            if !ctx.events.request_approval("Allow this specific sanitized session record to be included in a later local training export?")? {
                return Ok(CapabilityResponse::new(CapabilityStatus::AwaitingApproval,
                    "Training record permission needs your approval.", Value::Null));
            }
            selected["approved_for_export"] = json!(true);
            selected["permission_metadata"] = json!({"export_allowed":true,"license":license,
                "authorization":"interactive_user_approval"});
            ctx.store.save(ctx.session)?;
            return Ok(CapabilityResponse::new(CapabilityStatus::Completed,
                "This record is eligible for an explicitly approved local export.",
                json!({"record_id":record_id,"approved_for_export":true,"uploaded":false})));
        }
        return Ok(CapabilityResponse::new(CapabilityStatus::Unsupported,
            "Set retain, or approve one training record with record_id and license.", Value::Null));
    }
    if action == "status" {
        let saved = read_record(&path)?;
        return Ok(CapabilityResponse::new(CapabilityStatus::Available,
            if enabled { "Local project experience retention is enabled." }
            else { "Local project experience retention is off." },
            json!({"enabled":enabled,"scope":"local_project_only","count":saved.get("records")
                .and_then(Value::as_array).map_or(0, Vec::len),"external_upload":false})));
    }
    if action == "observe" {
        if !enabled { return Ok(CapabilityResponse::new(CapabilityStatus::Paused,
            "Enable local project experience retention in Capabilities settings before retrieval.", Value::Null)); }
        let query = input.get("query").and_then(Value::as_str).unwrap_or("").trim();
        if query.len() > 500 { bail!("experience query exceeds 500 bytes"); }
        let saved = read_record(&path)?;
        let words = query.to_ascii_lowercase().split(|ch: char| !ch.is_alphanumeric())
            .filter(|word| word.len() >= 4).map(str::to_owned).collect::<Vec<_>>();
        let mut ranked = saved.get("records").and_then(Value::as_array)
            .map(|records| records.iter().filter(|record|
                record.get("retired").and_then(Value::as_bool) != Some(true))
                .map(|record| {
                    let lesson = record.get("lesson").and_then(Value::as_str)
                        .unwrap_or("").to_ascii_lowercase();
                    let score = words.iter().filter(|word| lesson.contains(word.as_str())).count();
                    (score, record)
                }).filter(|(score, _)| words.is_empty() || *score > 0)
                .collect::<Vec<_>>()).unwrap_or_default();
        ranked.sort_by(|a, b| b.0.cmp(&a.0));
        let matches = ranked.into_iter().take(10).map(|(_, record)| record.clone())
            .collect::<Vec<_>>();
        return Ok(CapabilityResponse::new(CapabilityStatus::Completed,
            "Relevant local project experiences retrieved. Revalidate lessons against current code and policy.",
            json!({"records":matches,"scope":"local_project_only"})));
    }
    if action == "deactivate" || action == "pause" {
        return Ok(CapabilityResponse::new(CapabilityStatus::Unsupported,
            "Use Echo's Capabilities settings to turn experience retention off persistently.",
            json!({"enabled":enabled})));
    }
    if action != "activate" {
        return Ok(CapabilityResponse::new(CapabilityStatus::Unsupported,
            "Experience Loop supports opt-in recording, retrieval, and status.", Value::Null));
    }
    if !enabled {
        return Ok(CapabilityResponse::new(CapabilityStatus::Paused,
            "Enable local project experience retention in Capabilities settings before recording.",
            json!({"enabled":false,"external_upload":false})));
    }
    let lesson = input.get("lesson").and_then(Value::as_str).filter(|value| !value.trim().is_empty())
        .context("a lesson is required for experience recording")?;
    if lesson.len() > 1500 { bail!("experience lesson exceeds 1500 bytes"); }
    if contains_credential(&json!(lesson)) { bail!("experience lesson contains a credential and cannot be retained"); }
    let refs = input.get("evidence_refs").and_then(Value::as_array)
        .context("experience recording requires evidence references")?;
    if refs.is_empty() || refs.len() > 12 || refs.iter().any(|item| item.as_str().is_none()) {
        bail!("experience recording requires 1 to 12 evidence reference IDs");
    }
    let claims = ctx.session.capability_state.pointer("/evidence_ledger/claims")
        .and_then(Value::as_array).cloned().unwrap_or_default();
    let events = ctx.session.capability_state.pointer("/black_box/events")
        .and_then(Value::as_array).cloned().unwrap_or_default();
    for reference in refs {
        let id = reference.as_str().unwrap_or_default();
        if !claims.iter().chain(events.iter()).any(|record| record.get("id").and_then(Value::as_str) == Some(id)) {
            bail!("experience reference {id} is not present in the session evidence ledger or black box");
        }
    }
    let criteria = ctx.session.capability_state.pointer("/outcome_engine/criteria")
        .and_then(Value::as_array).cloned().unwrap_or_default();
    let evidence_class = if criteria.iter().any(|criterion| {
        criterion.get("status").and_then(Value::as_str) == Some("amity_check_passed")
            && criterion.get("evidence_refs").and_then(Value::as_array).is_some_and(|items|
                items.iter().any(|item| refs.contains(item)))
    }) { "amity_check_passed" }
    else if criteria.iter().any(|criterion| {
        criterion.get("status").and_then(Value::as_str) == Some("implemented_in_source")
            && criterion.get("evidence_refs").and_then(Value::as_array).is_some_and(|items|
                items.iter().any(|item| refs.contains(item)))
    }) { "implemented_in_source" }
    else if events.iter().any(|event| {
        event.get("status").and_then(Value::as_str).is_some_and(|status| status.contains("failed") || status.contains("denied"))
            && event.get("id").is_some_and(|id| refs.contains(id))
    }) { "failure_recorded" }
    else { "unverified" };
    let mut saved = read_record(&path)?;
    if saved.is_null() { saved = json!({"protocol":"echo.capabilities.v1","project_id":project_id,
        "scope":"local_project_only","records":[]}); }
    let records = saved.get_mut("records").and_then(Value::as_array_mut)
        .context("experience store has no records array")?;
    if records.len() >= 500 { bail!("project experience store reached its 500-record retention limit"); }
    let id = format!("experience:{}:{}",ctx.session.id,now_ms());
    let objective = if contains_credential(&json!(ctx.objective)) {
        "[objective omitted because it contains a credential]".to_owned()
    } else { redact_credentials(&ctx.objective.chars().take(1500).collect::<String>()) };
    let record = json!({"id":id,"project_id":project_id,"session_id":ctx.session.id,
        "source_model":ctx.model.version,"capability":input.get("capability"),
        "objective":objective,
        "lesson":redact_credentials(lesson),"evidence_refs":refs,"created_at_ms":now_ms(),
        "confidence":"requires_revalidation","observed_outcome_classification":evidence_class,
        "verified_outcome":evidence_class == "amity_check_passed",
        "privacy":"local_project_only","retention":"explicit_opt_in",
        "source_revision":ctx.project_context.pointer("/project_status/result/head/fingerprint")});
    records.push(record.clone());
    save_record(&path, &saved)?;
    if !ctx.session.capability_state.is_object() { ctx.session.capability_state = json!({}); }
    let training = ctx.session.capability_state.as_object_mut()
        .context("capability state must be an object")?
        .entry("training_records").or_insert_with(|| json!([]));
    if !training.is_array() { *training = json!([]); }
    let training_records = training.as_array_mut().context("training records must be an array")?;
    if training_records.len() < 200 {
        training_records.push(json!({"id":id,"protocol":"echo.capabilities.v1",
            "record_type":if evidence_class == "failure_recorded" { "error_correction" } else { "outcome" },
            "capability":input.get("capability").and_then(Value::as_str).unwrap_or("experience_loop"),
            "user_objective":Value::Null,"activation_reason":"evidence-linked local project experience",
            "expected_behavior":redact_credentials(lesson),"policy_revision":ctx.project_context.pointer("/active_policy/result/revision"),
            "result_type":if evidence_class == "amity_check_passed" { "completed" } else { "incomplete" },
            "evidence_refs":refs,"verified":evidence_class == "amity_check_passed",
            "source_model":ctx.model.version,"label":if evidence_class == "amity_check_passed" { "valid" } else { "unverified" },
            "approved_for_export":false,"permission_metadata":{"export_allowed":false,"license":null}}));
        ctx.store.save(ctx.session)?;
    }
    Ok(CapabilityResponse::new(CapabilityStatus::Completed,
        "Evidence-linked project lesson saved locally. Future models must revalidate it against current context.", record)
        .with_evidence(refs.iter().filter_map(Value::as_str).map(str::to_owned).collect()))
}
