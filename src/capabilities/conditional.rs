//! Demand-driven engineering capabilities. Project mutations remain in Amity's gateway.
use super::{CapabilityContext, CapabilityResponse, CapabilityStatus};
use super::workspaces::CandidateWorkspaces;
use crate::events::{AgentEvent, AgentEventSink, BusyInput};
use crate::host::HostClient;
use crate::model::ChatModel;
use crate::state::{MessageRole, contains_credential, redact_credentials};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

pub fn invoke(ctx: &mut CapabilityContext<'_>, id: &str, action: &str, input: &Value) -> Result<CapabilityResponse> {
    if ctx.cancel.load(Ordering::Relaxed) {
        return Ok(reply(CapabilityStatus::Cancelled, "Operation cancelled.", Value::Null));
    }
    match id {
        "reality_loop" => reality(ctx, action, input),
        "solution_arena" => arena(ctx, action, input),
        "futurecast" => futurecast(ctx, action, input),
        "debug_lab" => debugging(ctx, action, input),
        "intelligent_workforce" => workforce(ctx, action, input),
        "visual_to_code_navigation" => visual(ctx, action, input),
        _ => Ok(reply(CapabilityStatus::Unsupported, "Unknown conditional capability.", Value::Null)),
    }
}

fn reply(status: CapabilityStatus, message: &str, data: Value) -> CapabilityResponse {
    CapabilityResponse::new(status, message, data)
}

fn digest(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}

fn delegate_busy(events: &mut dyn AgentEventSink, cancel: &std::sync::atomic::AtomicBool,
    model: &ChatModel, host: &HostClient, tasks: Value) -> Result<(Value, Vec<String>)> {
    events.begin_busy()?;
    let result = thread::scope(|scope| {
        let worker = scope.spawn(move || crate::subagents::delegate(model, host, tasks, cancel));
        let mut queued = Vec::new();
        while !worker.is_finished() {
            match events.poll_busy() {
                Some(BusyInput::Cancel) => cancel.store(true, Ordering::Relaxed),
                Some(BusyInput::Message(message)) => queued.push(message),
                None => {},
            }
            thread::sleep(Duration::from_millis(25));
        }
        let outcome = worker.join().map_err(|_| anyhow::anyhow!("worker delegation panicked"))?;
        Ok::<_, anyhow::Error>((outcome?, queued))
    });
    events.end_busy()?;
    result
}

fn account_workers(ctx: &mut CapabilityContext<'_>, result: &Value) -> Result<()> {
    for worker in result.get("workers").and_then(Value::as_array).into_iter().flat_map(|items| items.iter()) {
        let usage = worker.get("usage").unwrap_or(&Value::Null);
        let input = usage.get("input_tokens").and_then(Value::as_u64);
        let output = usage.get("output_tokens").and_then(Value::as_u64);
        let window = usage.get("context_window").and_then(Value::as_u64);
        let cost = usage.get("cost_usd").and_then(Value::as_f64)
            .filter(|cost| cost.is_finite() && *cost >= 0.0);
        ctx.session.usage_input_tokens = ctx.session.usage_input_tokens.saturating_add(input.unwrap_or(0));
        ctx.session.usage_output_tokens = ctx.session.usage_output_tokens.saturating_add(output.unwrap_or(0));
        if let Some(input) = input { ctx.session.last_input_tokens = Some(input); }
        if let Some(window) = window { ctx.session.context_window = Some(window); }
        if let Some(cost) = cost {
            ctx.session.usage_cost_usd += cost;
            ctx.session.usage_cost_reported = true;
        }
        ctx.events.emit(AgentEvent::ModelUsage { input_tokens:input, output_tokens:output,
            context_window:window, cost_usd:cost });
    }
    ctx.store.save(ctx.session)?;
    Ok(())
}

fn persist_queued(ctx: &mut CapabilityContext<'_>, messages: &[String]) -> Result<Vec<String>> {
    let mut safe = Vec::new();
    for message in messages {
        if message.trim().is_empty() { continue; }
        ctx.session.append_message(MessageRole::User, message);
        if let Some(last) = ctx.session.messages.last() { safe.push(last.content.clone()); }
    }
    ctx.store.save(ctx.session)?;
    Ok(safe)
}

fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value.get(name).and_then(Value::as_str).filter(|s| !s.trim().is_empty())
        .with_context(|| format!("{name} is required"))
}

fn checked_text<'a>(value: &'a Value, name: &str, max: usize) -> Result<&'a str> {
    let text = field(value, name)?;
    if text.len() > max { bail!("{name} exceeds {max} bytes"); }
    Ok(text)
}

fn save_state(ctx: &mut CapabilityContext<'_>, id: &str, value: Value) -> Result<()> {
    if !ctx.session.capability_state.is_object() { ctx.session.capability_state = json!({}); }
    ctx.session.capability_state.as_object_mut().context("capability state must be an object")?
        .insert(id.to_owned(), value);
    ctx.store.save(ctx.session)?;
    Ok(())
}

fn get_state<'a>(ctx: &'a CapabilityContext<'_>, id: &str) -> Option<&'a Value> {
    ctx.session.capability_state.get(id)
}

fn model_read(ctx: &CapabilityContext<'_>, action: Value) -> Result<Value> {
    let scoped = ctx.host.model_scope()?;
    let response = scoped.client().agent_tool_cancelable(None, action, ctx.cancel)?;
    if response.get("ok").and_then(Value::as_bool) != Some(true) {
        bail!("Amity refused the read-only observation: {}", response.pointer("/error/message")
            .and_then(Value::as_str).unwrap_or("unknown error"));
    }
    Ok(response.get("result").cloned().unwrap_or(Value::Null))
}

fn configured_editors(ctx: &CapabilityContext<'_>) -> Vec<String> {
    ctx.project_context.pointer("/development_tools/result/editor_queries")
        .and_then(Value::as_array).map(|editors| editors.iter()
            .filter(|entry| entry.get("available").and_then(Value::as_bool) == Some(true))
            .filter_map(|entry| entry.get("kind").and_then(Value::as_str).map(str::to_owned))
            .collect()).unwrap_or_default()
}

fn editor_query_available(ctx: &CapabilityContext<'_>, kind: &str, query: &str) -> bool {
    ctx.project_context.pointer("/development_tools/result/editor_queries")
        .and_then(Value::as_array).is_some_and(|editors| editors.iter().any(|entry|
            entry.get("kind").and_then(Value::as_str) == Some(kind)
                && entry.get("available_queries").and_then(Value::as_array)
                    .is_some_and(|queries| queries.iter().any(|item| item.as_str() == Some(query)))))
}

fn host_activity(ctx: &CapabilityContext<'_>) -> Result<Value> {
    let task_id = ctx.session.active_task.as_deref().context("No active Amity task has execution activity")?;
    let response = ctx.host.agent_tool_cancelable(Some(task_id), json!({"type":"activity"}), ctx.cancel)?;
    if response.get("ok").and_then(Value::as_bool) != Some(true) {
        bail!("Amity could not supply task activity: {}", response.pointer("/error/message")
            .and_then(Value::as_str).unwrap_or("unknown error"));
    }
    Ok(response.get("result").cloned().unwrap_or(Value::Null))
}

fn reality(ctx: &mut CapabilityContext<'_>, action: &str, input: &Value) -> Result<CapabilityResponse> {
    if action == "deactivate" || action == "pause" {
        save_state(ctx, "reality_loop", json!({"state":"paused"}))?;
        return Ok(reply(CapabilityStatus::Paused, "Runtime observation paused.", Value::Null));
    }
    if action == "status" {
        return Ok(reply(CapabilityStatus::Available, "Runtime observation state.",
            get_state(ctx, "reality_loop").cloned().unwrap_or(Value::Null)));
    }
    if action == "observe" {
        if let Some(kind) = input.get("editor_kind").and_then(Value::as_str) {
            let query = field(input, "query")?;
            let supported = match kind {
                "unreal" => matches!(query, "assets" | "blueprint" | "uobject" | "level"),
                "unity" => matches!(query, "scene" | "prefab" | "components" | "console"),
                "godot" => matches!(query, "scene" | "nodes" | "signals" | "resource" | "diagnostics"),
                _ => false,
            };
            if !supported { bail!("unsupported native editor observation kind or query"); }
            if !editor_query_available(ctx, kind, query) {
                return Ok(reply(CapabilityStatus::Unavailable,
                    "Amity has no available adapter for this editor query.",
                    json!({"editor_kind":kind,"query":query,"runtime_behavior_verified":false})));
            }
            let target = input.get("target").and_then(Value::as_str);
            if target.is_some_and(|value| value.is_empty() || value.len() > 500) {
                bail!("editor target must be a bounded project resource");
            }
            return Ok(reply(CapabilityStatus::AwaitingApproval,
                "Request this editor observation through Echo's Amity-governed editor_query tool. It needs a task, policy evaluation, and your approval.",
                json!({"next_governed_tool":{"tool":"editor_query",
                    "arguments":{"editor_kind":kind,"query":query,"target":target}},
                    "runtime_behavior_verified":false})))
        }
        let activity = host_activity(ctx)?;
        let events = activity.get("events").and_then(Value::as_array).cloned().unwrap_or_default();
        let observed = events.iter().rev().find(|entry| {
            let kind = entry.get("kind").or_else(|| entry.get("event")).and_then(Value::as_str).unwrap_or("");
            kind.contains("command") || kind.contains("verification") || kind.contains("mcp")
        }).cloned();
        return match observed {
            Some(event) => Ok(reply(CapabilityStatus::Completed,
                "Amity recorded execution activity. Interpret its actual output and receipts before claiming runtime behavior.",
                json!({"task_id":ctx.session.active_task,"observation":event,"authority":"amity_task_activity",
                    "runtime_behavior_verified":false}))),
            None => Ok(reply(CapabilityStatus::Unavailable,
                "No Amity runtime execution observation is recorded for this task.",
                json!({"task_id":ctx.session.active_task,"runtime_behavior_verified":false}))),
        };
    }
    if action != "activate" && action != "resume" {
        return Ok(reply(CapabilityStatus::Unsupported, "Reality Loop supports activate, observe, status, pause, resume, and deactivate.", Value::Null));
    }
    let tools = model_read(ctx, json!({"type":"capabilities"}))?;
    let commands = tools.get("commands").and_then(Value::as_array).cloned().unwrap_or_default();
    let observation_commands: Vec<Value> = commands.iter().filter(|command| {
        let category = command.get("category").and_then(Value::as_str).unwrap_or("");
        matches!(category, "editor" | "runtime" | "observe" | "preview")
    }).cloned().collect();
    let native = ctx.host.model_scope().ok().and_then(|scoped| {
        scoped.client().call_cancelable("native-discover", &[], None, ctx.cancel).ok()
            .and_then(|response| response.last().ok().cloned())
    });
    let editors = configured_editors(ctx);
    if observation_commands.is_empty() && editors.is_empty() {
        return Ok(reply(CapabilityStatus::Unavailable,
            "No authorized runtime observation command or native project adapter is available.",
            json!({"supported_local_observation":false,"native_editor_execution":false,
                "missing":"Configure an Amity governed runtime command or native adapter."})));
    }
    let requested = input.get("command_id").and_then(Value::as_str);
    if let Some(command_id) = requested {
        if !observation_commands.iter().any(|command| command.get("id").and_then(Value::as_str) == Some(command_id)) {
            return Ok(reply(CapabilityStatus::Unavailable,
                "That runtime observation command is not configured in Amity.",
                json!({"command_id":command_id,"available_commands":observation_commands})));
        }
    }
    let state = json!({"state":"activated","objective_digest":digest(&json!(ctx.objective)),
        "commands":observation_commands,"configured_read_only_editors":editors,
        "native_facts_digest":native.as_ref().map(digest),
        "runtime_behavior_verified":false});
    save_state(ctx, "reality_loop", state.clone())?;
    let status = if requested.is_some() { CapabilityStatus::AwaitingApproval } else { CapabilityStatus::Activated };
    Ok(reply(status, if requested.is_some() {
        "Request the configured command through Echo's governed run_command tool; Amity and the user must approve execution."
    } else { "Runtime observation options discovered. Select a governed command or inspect native project facts." },
        json!({"state":state,"native_facts":native,"next_tool":requested.map(|command_id| json!({
            "tool":"run_command","arguments":{"command_id":command_id,"paths":[]}}))})))
}

fn arena(ctx: &mut CapabilityContext<'_>, action: &str, input: &Value) -> Result<CapabilityResponse> {
    if action == "status" { return Ok(reply(CapabilityStatus::Available, "Solution comparison state.",
        get_state(ctx, "solution_arena").cloned().unwrap_or(Value::Null))); }
    if action == "deactivate" || action == "pause" {
        save_state(ctx, "solution_arena", json!({"state":"paused"}))?;
        return Ok(reply(CapabilityStatus::Paused, "Solution comparison paused.", Value::Null));
    }
    if action != "activate" && action != "resume" {
        return Ok(reply(CapabilityStatus::Unsupported, "Solution Arena supports activate, status, pause, resume, and deactivate.", Value::Null));
    }
    let approaches = input.get("approaches").and_then(Value::as_array)
        .context("Solution Arena needs two or three distinct approaches")?;
    if approaches.len() < 2 || approaches.len() > 3 || approaches.iter().any(|item| {
        item.as_str().is_none_or(|s| s.is_empty() || s.len() > 500)
    }) { bail!("Solution Arena requires two or three bounded approach descriptions"); }
    if approaches.len() > ctx.budget.max_workers {
        bail!("Solution Arena exceeds the shared worker budget of {}", ctx.budget.max_workers);
    }
    let objective = input.get("objective").and_then(Value::as_str).unwrap_or(ctx.objective);
    if objective.is_empty() || objective.len() > 3000 { bail!("Solution Arena objective must be 1 to 3000 bytes"); }
    let files = if let Some(files) = input.get("files") {
        let files = files.as_array().context("candidate files must be an array")?;
        if files.is_empty() || files.len() > 20 || files.iter().any(|file| file.as_str().is_none()) {
            bail!("candidate workspaces require 1 to 20 existing project file paths");
        }
        files.iter().filter_map(Value::as_str).map(str::to_owned).collect::<Vec<_>>()
    } else { Vec::new() };
    let mut workspaces = if files.is_empty() { None } else {
        let project = ctx.config.projects.iter().find(|project| project.id == ctx.session.project_id)
            .context("Solution Arena project is no longer registered")?;
        Some(CandidateWorkspaces::create(ctx.host, &project.path, &files,
            approaches.len(), ctx.cancel)?)
    };
    let tasks = approaches.iter().enumerate().map(|(index, approach)| json!({
        "name":format!("candidate-{}",index+1),"role":"alternative solution designer",
        "instruction":format!("Independently analyze this approach to the same objective: {objective}\nApproach: {}\nAuthorized comparison files: {}\nUse only read-only investigation. Return final_proposal as a JSON object string with proposal, evidence_refs, and optionally files:[{{path,content}}] containing COMPLETE UTF-8 file contents for only the listed files. If no complete implementation is justified, omit files and explain why. Do not claim execution or verification.",
            approach.as_str().unwrap_or_default(), files.join(", "))
    })).collect::<Vec<_>>();
    ctx.budget.reserve_model_calls(ctx.session, approaches.len() as u64 * 12)
        .map_err(anyhow::Error::msg)?;
    ctx.store.save(ctx.session)?;
    ctx.events.operation("Comparing independent read-only solution candidates…");
    let (result, queued) = delegate_busy(&mut *ctx.events, ctx.cancel, ctx.model, ctx.host,
        Value::Array(tasks))?;
    account_workers(ctx, &result)?;
    let queued = persist_queued(ctx, &queued)?;
    let candidate_count = result.pointer("/workers").and_then(Value::as_array).map_or(0, Vec::len);
    let mut materializations = Vec::new();
    if queued.is_empty() {
    if let Some(workspaces) = workspaces.as_ref() {
        for (index, worker) in result.get("workers").and_then(Value::as_array)
            .into_iter().flat_map(|workers| workers.iter()).enumerate() {
            let proposal = worker.get("summary").and_then(Value::as_str)
                .and_then(|text| serde_json::from_str::<Value>(text).ok());
            let materialized = match proposal.as_ref().and_then(|proposal| proposal.get("files")) {
                Some(changes) => match workspaces.apply_changes(index, changes) {
                    Ok(diff) => json!({"candidate":index+1,"status":"materialized_in_isolated_workspace",
                        "diff":diff}),
                    Err(error) => json!({"candidate":index+1,"status":"candidate_change_rejected",
                        "reason":redact_credentials(&format!("{error:#}"))}),
                },
                None => json!({"candidate":index+1,"status":"analysis_only",
                    "reason":"worker returned no complete file proposal"}),
            };
            materializations.push(materialized);
        }
    }
    }
    let mut public_result = result.clone();
    if let Some(workers) = public_result.get_mut("workers").and_then(Value::as_array_mut) {
        for worker in workers {
            let parsed = worker.get("summary").and_then(Value::as_str)
                .and_then(|text| serde_json::from_str::<Value>(text).ok());
            if let Some(parsed) = parsed {
                let rationale = parsed.get("proposal").and_then(Value::as_str).unwrap_or("");
                let rationale = if contains_credential(&json!(rationale)) {
                    "[candidate rationale omitted because it contains a credential]".to_owned()
                } else { redact_credentials(&rationale.chars().take(2000).collect::<String>()) };
                let proposed_files = parsed.get("files").and_then(Value::as_array)
                    .map(|files| files.iter().filter_map(|file| {
                        let path = file.get("path").and_then(Value::as_str)?;
                        let content = file.get("content").and_then(Value::as_str)?;
                        Some(json!({"path":path,"content_digest":digest(&json!(content))}))
                    }).collect::<Vec<_>>()).unwrap_or_default();
                worker["summary"] = json!(rationale);
                worker["proposed_files"] = json!(proposed_files);
            } else if worker.get("summary").is_some() {
                worker["summary"] = json!("Worker returned an unstructured response; no candidate file content is retained.");
            }
        }
    }
    let workspace_manifest = workspaces.as_ref().map(CandidateWorkspaces::manifest);
    let retained_path = if queued.is_empty()
        && input.get("retain_candidates").and_then(Value::as_bool) == Some(true) {
        if let Some(workspaces) = workspaces.as_mut() {
            if ctx.events.request_approval("Keep these isolated candidate copies locally for later review? They contain project source.")? {
                Some(workspaces.retain())
            } else { None }
        } else { None }
    } else { None };
    let record = json!({"objective":redact_credentials(objective),"approaches":approaches,
        "result_digest":digest(&result),"candidate_count":candidate_count,
        "workspace_mode":if workspace_manifest.is_some() { "isolated_local_copies" }
            else { "independent_read_only_contexts" },
        "materializations":materializations.clone(),"production_mutated":false,
        "runtime_verified":false,"candidate_copies_retained":retained_path.is_some()});
    save_state(ctx, "solution_arena", record.clone())?;
    let failures = result.get("workers").and_then(Value::as_array).map(|workers| workers.iter()
        .filter(|worker| worker.get("status").and_then(Value::as_str) != Some("completed")).count()).unwrap_or(0);
    Ok(reply(if queued.is_empty() { CapabilityStatus::Completed } else { CapabilityStatus::Paused },
        if !queued.is_empty() { "A new instruction arrived. Reconsider the candidate comparison before continuing." }
        else if failures > 0 { "Some read-only candidates failed. Compare the available results and limitations before choosing." }
        else { "Independent read-only candidate analyses are ready. Any production change still requires Amity approval." },
        json!({"candidate_count":candidate_count,"failed_candidates":failures,
            "comparison":public_result,"record":record,"queued_messages":queued,
            "workspaces":workspace_manifest.clone(),"retained_path":retained_path})))
}

fn futurecast(ctx: &mut CapabilityContext<'_>, action: &str, input: &Value) -> Result<CapabilityResponse> {
    if action == "status" { return Ok(reply(CapabilityStatus::Available, "Futurecast state.",
        get_state(ctx, "futurecast").cloned().unwrap_or(Value::Null))); }
    if action != "activate" { return Ok(reply(CapabilityStatus::Unsupported,
        "Futurecast currently supports bounded read-only activation and status.", Value::Null)); }
    let scenarios = input.get("scenarios").and_then(Value::as_array)
        .context("Futurecast needs one to three future scenarios")?;
    if scenarios.is_empty() || scenarios.len() > 3 || scenarios.iter().any(|item| {
        item.as_str().is_none_or(|s| s.is_empty() || s.len() > 500)
    }) { bail!("Futurecast requires one to three bounded scenario descriptions"); }
    let scoped = ctx.host.model_scope()?;
    let mut observations = Vec::new();
    for scenario in scenarios {
        if ctx.cancel.load(Ordering::Relaxed) { return Ok(reply(CapabilityStatus::Cancelled, "Futurecast cancelled.", Value::Null)); }
        let description = scenario.as_str().unwrap_or_default();
        let source = scoped.client().query_source(description, 10)?;
        observations.push(json!({"scenario":description,"source_observations":source,
            "prediction_status":"unassessed","experimental_validation":false}));
    }
    let stored = observations.iter().map(|item| json!({"scenario":item.get("scenario"),
        "observation_digest":digest(item),"prediction_status":"unassessed"})).collect::<Vec<_>>();
    let record = json!({"objective":redact_credentials(ctx.objective),"scenarios":stored,
        "source_revision":ctx.project_context.pointer("/project_status/result/head/fingerprint"),
        "production_mutated":false});
    save_state(ctx, "futurecast", record.clone())?;
    Ok(reply(CapabilityStatus::Completed,
        "Relevant source evidence collected for the future scenarios. Architectural risks remain predictions until assessed or experimentally observed.",
        json!({"record":record,"observations":observations})))
}

fn debugging(ctx: &mut CapabilityContext<'_>, action: &str, input: &Value) -> Result<CapabilityResponse> {
    if action == "status" { return Ok(reply(CapabilityStatus::Available, "Debugging investigation state.",
        get_state(ctx, "debug_lab").cloned().unwrap_or(Value::Null))); }
    if action == "pause" || action == "deactivate" {
        let mut record = get_state(ctx, "debug_lab").cloned().unwrap_or_else(|| json!({}));
        record["state"] = json!("paused");
        save_state(ctx, "debug_lab", record)?;
        return Ok(reply(CapabilityStatus::Paused, "Debugging investigation paused.", Value::Null));
    }
    if action == "activate" || action == "resume" {
        let report = input.get("failure_report").and_then(Value::as_str).unwrap_or(ctx.objective);
        if report.is_empty() || report.len() > 4000 { bail!("failure report must be 1 to 4000 bytes"); }
        let current = get_state(ctx, "debug_lab").cloned();
        let record = current.unwrap_or_else(|| json!({"failure_report":redact_credentials(report),
            "hypotheses":[],"observations":[],"experiments":[],"eliminated":[],
            "verification_state":"not_verified"}));
        let mut record = record;
        record["state"] = json!("investigating");
        save_state(ctx, "debug_lab", record.clone())?;
        return Ok(reply(CapabilityStatus::Activated, "Debugging investigation opened.", record));
    }
    if action == "observe" {
        if input.get("runtime_observation").and_then(Value::as_bool) == Some(true) {
            let observation = ctx.invoke("reality_loop", "observe", input)?;
            let mut record = get_state(ctx, "debug_lab").cloned()
                .context("Activate the debugging investigation before recording observations")?;
            let observations = record.get_mut("observations").and_then(Value::as_array_mut)
                .context("debugging observation state is invalid")?;
            if observations.len() >= 40 { bail!("debugging investigation observation limit reached"); }
            observations.push(json!({"source":"reality_loop","result_status":observation.status.clone(),
                "result_digest":digest(&observation.data),"at_task":ctx.session.active_task}));
            save_state(ctx, "debug_lab", record)?;
            return Ok(reply(observation.status.clone(),
                "Runtime observation was coordinated through Reality Loop and recorded with its actual status.",
                json!({"observation":observation,"diagnosis_status":"pending_interpretation"})));
        }
        let query = checked_text(input, "query", 500)?;
        let found = model_read(ctx, json!({"type":"search","query":query}))?;
        let mut record = get_state(ctx, "debug_lab").cloned()
            .context("Activate the debugging investigation before recording observations")?;
        let observations = record.get_mut("observations").and_then(Value::as_array_mut)
            .context("debugging observation state is invalid")?;
        if observations.len() >= 40 { bail!("debugging investigation observation limit reached"); }
        observations.push(json!({"query_digest":digest(&json!(query)),"source":"amity_search",
            "result_digest":digest(&found),"hit_count":found.get("hits").and_then(Value::as_array)
                .map_or(0, Vec::len),"head":found.get("head")}));
        save_state(ctx, "debug_lab", record)?;
        return Ok(reply(CapabilityStatus::Completed, "Amity source observation added to the investigation.", found));
    }
    if action == "configure" {
        let hypothesis = checked_text(input, "hypothesis", 1000)?;
        let mut record = get_state(ctx, "debug_lab").cloned()
            .context("Activate the debugging investigation before adding a hypothesis")?;
        let hypotheses = record.get_mut("hypotheses").and_then(Value::as_array_mut)
            .context("debugging hypothesis state is invalid")?;
        if hypotheses.len() >= 20 { bail!("debugging hypothesis limit reached"); }
        hypotheses.push(json!({"statement":redact_credentials(hypothesis),"status":"unverified"}));
        save_state(ctx, "debug_lab", record)?;
        return Ok(reply(CapabilityStatus::Completed, "Hypothesis recorded as unverified.",
            json!({"hypothesis":redact_credentials(hypothesis),"status":"unverified"})));
    }
    Ok(reply(CapabilityStatus::Unsupported,
        "Debugging Laboratory supports activate, observe, configure, status, pause, resume, and deactivate.", Value::Null))
}

fn workforce(ctx: &mut CapabilityContext<'_>, action: &str, input: &Value) -> Result<CapabilityResponse> {
    if action == "status" { return Ok(reply(CapabilityStatus::Available, "Workforce state.",
        get_state(ctx, "intelligent_workforce").cloned().unwrap_or(Value::Null))); }
    if action != "activate" { return Ok(reply(CapabilityStatus::Unsupported,
        "Intelligent Workforce supports read-only activation and status.", Value::Null)); }
    let jobs = input.get("jobs").and_then(Value::as_array)
        .context("Intelligent Workforce requires named read-only jobs")?;
    let maximum = (ctx.config.capabilities.pointer("/overrides/intelligent_workforce/max_workers")
        .and_then(Value::as_u64).unwrap_or(4).clamp(1, 4) as usize)
        .min(ctx.budget.max_workers);
    if jobs.is_empty() || jobs.len() > maximum { bail!("worker count exceeds configured limit of {maximum}"); }
    let allowed_roles = ["repository investigator","architecture analyst","debugging investigator",
        "alternative solution designer","documentation researcher","performance analyst","review specialist"];
    let mut names = std::collections::BTreeSet::new();
    for job in jobs {
        let name = checked_text(job, "name", 64)?;
        let role = checked_text(job, "role", 100)?;
        let _instruction = checked_text(job, "instruction", 3000)?;
        if !names.insert(name) { bail!("worker job names must be unique"); }
        if !allowed_roles.contains(&role) { bail!("unsupported worker role {role}"); }
    }
    for job in jobs {
        let name = field(job, "name")?;
        if job.get("depends_on").is_some_and(|value| !value.is_array()) {
            bail!("worker dependencies must be an array of job names");
        }
        if job.get("model").is_some_and(|value| !value.is_string()) {
            bail!("worker model must be a Cadenza model root");
        }
        if let Some(model_name) = job.get("model").and_then(Value::as_str) {
            let mut client=crate::cadenza::Client::connect(&ctx.config.cadenza)?;
            if !client.discover()?.iter().any(|m|m.root==model_name && m.ready()) {
                bail!("worker Cadenza model root is unavailable: {model_name}");
            }
        }
        let dependencies = job.get("depends_on").and_then(Value::as_array).cloned().unwrap_or_default();
        if dependencies.len() > 3 || dependencies.iter().any(|dependency| dependency.as_str()
            .is_none_or(|dependency| dependency == name || !names.contains(dependency))) {
            bail!("worker dependencies must name earlier or parallel jobs without self-reference");
        }
    }
    ctx.budget.reserve_model_calls(ctx.session, jobs.len() as u64 * 12)
        .map_err(anyhow::Error::msg)?;
    ctx.store.save(ctx.session)?;
    let mut remaining = jobs.clone();
    let mut reports = Vec::<Value>::new();
    let mut completed = std::collections::BTreeSet::<String>::new();
    let mut failed = std::collections::BTreeSet::<String>::new();
    while !remaining.is_empty() {
        if ctx.cancel.load(Ordering::Relaxed) {
            return Ok(reply(CapabilityStatus::Cancelled, "Worker delegation cancelled.",
                json!({"workers":reports})));
        }
        let mut ready = Vec::new();
        let mut blocked = Vec::new();
        for (index, job) in remaining.iter().enumerate() {
            let dependencies = job.get("depends_on").and_then(Value::as_array).cloned().unwrap_or_default();
            if dependencies.iter().any(|dependency| dependency.as_str()
                .is_some_and(|dependency| failed.contains(dependency))) {
                blocked.push(index);
            } else if dependencies.iter().all(|dependency| dependency.as_str()
                .is_some_and(|dependency| completed.contains(dependency))) {
                ready.push(index);
            }
        }
        for index in blocked.iter().rev() {
            let job = remaining.remove(*index);
            let name = field(&job, "name")?.to_owned();
            failed.insert(name.clone());
            reports.push(json!({"name":name,"status":"skipped",
                "reason":"a dependency failed","model":job.get("model")}));
        }
        if remaining.is_empty() { break; }
        ready.clear();
        for (index, job) in remaining.iter().enumerate() {
            let dependencies = job.get("depends_on").and_then(Value::as_array).cloned().unwrap_or_default();
            if dependencies.iter().all(|dependency| dependency.as_str()
                .is_some_and(|dependency| completed.contains(dependency))) { ready.push(index); }
        }
        if ready.is_empty() { bail!("worker dependency graph contains a cycle"); }
        let mut groups = std::collections::BTreeMap::<String, Vec<Value>>::new();
        for index in ready.iter().rev() {
            let job = remaining.remove(*index);
            let model_name = job.get("model").and_then(Value::as_str).unwrap_or("").to_owned();
            let mut instruction = field(&job, "instruction")?.to_owned();
            for dependency in job.get("depends_on").and_then(Value::as_array).into_iter()
                .flat_map(|items| items.iter()).filter_map(Value::as_str) {
                if let Some(summary) = reports.iter().find(|report| report.get("name").and_then(Value::as_str)
                    == Some(dependency)).and_then(|report| report.get("summary")).and_then(Value::as_str) {
                    let excerpt = redact_credentials(&summary.chars().take(350).collect::<String>());
                    instruction.push_str(&format!("\nPrior worker {dependency} found: {excerpt}"));
                }
            }
            if instruction.len() > 3000 { bail!("worker instruction exceeds the delegated 3000-byte limit"); }
            groups.entry(model_name).or_default().push(json!({"name":job.get("name"),
                "role":job.get("role"),"instruction":instruction}));
        }
        for (model_name, tasks) in groups {
            let selected = if model_name.is_empty() { None } else {
                let project = ctx.config.projects.iter().find(|p|p.id==ctx.session.project_id).context("workforce project is no longer registered")?;
                let path=project.effective_host_config()?.to_path_buf();
                let host=HostClient::new(ctx.config.host_executable.clone(),path,Duration::from_secs(ctx.config.resources.host_timeout_seconds))?.with_token_file(project.token_file.clone());
                let mut client=crate::cadenza::Client::connect(&ctx.config.cadenza)?;
                let selected=client.discover()?.into_iter().find(|m|m.root==model_name && m.ready()).context("worker Cadenza model is unavailable")?;
                let model=ChatModel::open(ctx.config,Some(&selected),Duration::from_secs(ctx.config.cadenza.request_timeout_seconds))?;
                Some((host, model))
            };
            let (host, model) = selected.as_ref().map(|(host, model)| (host, model))
                .unwrap_or((ctx.host, ctx.model));
            let model_version = model.version.clone();
            ctx.events.operation("Read-only engineering workers are investigating…");
            let (delegated, queued) = delegate_busy(&mut *ctx.events, ctx.cancel, model, host,
                Value::Array(tasks))?;
            account_workers(ctx, &delegated)?;
            let queued = persist_queued(ctx, &queued)?;
            for mut report in delegated.get("workers").and_then(Value::as_array)
                .cloned().unwrap_or_default() {
                let name = report.get("name").and_then(Value::as_str).unwrap_or("").to_owned();
                if report.get("status").and_then(Value::as_str) == Some("completed") {
                    completed.insert(name.clone());
                } else { failed.insert(name.clone()); }
                report["model"] = json!(model_version.clone());
                reports.push(report);
            }
            if !queued.is_empty() {
                return Ok(reply(CapabilityStatus::Paused,
                    "A new instruction arrived. Worker findings are preserved; reconsider remaining jobs before continuing.",
                    json!({"workers":reports,"queued_messages":queued,"remaining_jobs":remaining})));
            }
        }
    }
    let record = json!({"jobs":jobs.iter().map(|job| json!({"name":job.get("name"),
        "role":job.get("role"),"model":job.get("model"),"depends_on":job.get("depends_on")})).collect::<Vec<_>>(),
        "worker_count":reports.len(),"result_digest":digest(&json!(reports)),
        "mutation_boundary":"lead_agent_only"});
    save_state(ctx, "intelligent_workforce", record.clone())?;
    let failed_count = reports.iter().filter(|report| report.get("status").and_then(Value::as_str)
        != Some("completed")).count();
    Ok(reply(CapabilityStatus::Completed,
        if failed_count > 0 { "Some read-only worker jobs failed or were skipped. Review partial findings and dependencies before acting." }
        else { "Read-only workers returned findings. The lead model must reconcile conflicts and submit any writes through Amity." },
        json!({"record":record,"workers":reports,"failed_jobs":failed_count})))
}

fn visual(ctx: &mut CapabilityContext<'_>, action: &str, input: &Value) -> Result<CapabilityResponse> {
    if action == "status" { return Ok(reply(CapabilityStatus::Available, "Visual navigation state.",
        get_state(ctx, "visual_to_code_navigation").cloned().unwrap_or(Value::Null))); }
    if action != "activate" { return Ok(reply(CapabilityStatus::Unsupported,
        "Visual-to-Code Navigation currently supports activation and status.", Value::Null)); }
    let requested_sha = input.get("image_sha256").and_then(Value::as_str);
    let image = ctx.session.messages.iter().rev().flat_map(|message| message.attachments.iter())
        .find(|attachment| attachment.mime.starts_with("image/")
            && requested_sha.is_none_or(|sha| sha == attachment.sha256))
        .map(|attachment| json!({"name":attachment.name,"sha256":attachment.sha256,"mime":attachment.mime}));
    let editor = ctx.project_context.get("editor_context").filter(|value| !value.is_null()).cloned();
    if image.is_none() && editor.is_none() {
        return Ok(reply(CapabilityStatus::Unavailable,
            "Attach an image or supply supported editor context to identify a visual target.",
            json!({"image_available":false,"editor_context_available":false})));
    }
    if image.is_some() && !ctx.model.supports_images() {
        return Ok(reply(CapabilityStatus::Unavailable,
            "The selected model does not advertise image support. Choose a vision-capable model or supply editor context.",
            json!({"image_available":true,"model_vision":false})));
    }
    let hint = input.get("target_hint").and_then(Value::as_str).unwrap_or("");
    if hint.len() > 500 { bail!("visual target hint exceeds 500 bytes"); }
    let source = if !hint.is_empty() {
        Some(ctx.host.model_scope()?.client().query_source(hint, 10)?)
    } else { None };
    let file = editor.as_ref().and_then(|value| value.get("file"))
        .and_then(Value::as_str).map(str::to_owned);
    let grounded_file = if let Some(file) = file.as_deref() {
        Some(model_read(ctx, json!({"type":"read","path":file}))?)
    } else { None };
    let reference = json!({"image":image,"region":input.get("region"),
        "editor_context":editor,"grounded_file":file.clone(),"source_observation":grounded_file,
        "source_candidates":source,"target_confirmed":file.is_some(),
        "native_scene_object_mapping":false});
    save_state(ctx, "visual_to_code_navigation", json!({"image":reference.get("image"),
        "region":input.get("region"),"grounded_file":file.clone(),"source_digest":digest(&reference),
        "target_confirmed":file.is_some(),"native_scene_object_mapping":false}))?;
    Ok(reply(if file.is_some() { CapabilityStatus::Completed } else { CapabilityStatus::Paused },
        if file.is_some() { "The visual reference is linked to an editor-selected project file. Verify the exact element before editing." }
        else { "The image is available, but the controlling source element is not confirmed. Ask for editor selection or more context before editing." },
        reference))
}
