//! Model-independent capability catalog. The catalog describes real paths and limits.
use crate::config::{Config, ProjectRegistration};
use serde::Serialize;
use serde_json::{Value, json};

use super::protocol::{PROTOCOL, TRAINING_CONTRACT};

#[derive(Clone, Debug, Serialize)]
pub struct CapabilityDescriptor {
    pub protocol: &'static str,
    pub id: &'static str,
    pub version: &'static str,
    pub name: &'static str,
    pub purpose: &'static str,
    pub activation_mode: &'static str,
    pub activation_conditions: &'static str,
    pub nonactivation_conditions: &'static str,
    pub required_inputs: Vec<&'static str>,
    pub optional_inputs: Vec<&'static str>,
    pub input_schema: Value,
    pub structured_outputs: Vec<&'static str>,
    pub output_schema: Value,
    pub methods: Vec<&'static str>,
    pub echo_dependencies: Vec<&'static str>,
    pub amity_dependencies: Vec<&'static str>,
    pub external_dependencies: Vec<&'static str>,
    pub potentially_mutating: bool,
    pub permissions: Vec<&'static str>,
    pub cost_class: &'static str,
    pub resource_budget: &'static str,
    pub supported_environments: Vec<&'static str>,
    pub availability: Availability,
    pub limitations: Vec<&'static str>,
    pub failure_recovery: &'static str,
    pub result_provenance: &'static str,
    pub training_contract: &'static str,
    pub training_behavior: Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct Availability {
    pub status: &'static str,
    pub reason: String,
    pub enabled: bool,
    pub activation: String,
    pub execution_available: Option<bool>,
}

fn descriptor(id: &'static str, name: &'static str, purpose: &'static str,
    activation_mode: &'static str, activation_conditions: &'static str,
    nonactivation_conditions: &'static str, methods: &[&'static str],
    echo_dependencies: &[&'static str], amity_dependencies: &[&'static str],
    external_dependencies: &[&'static str], potentially_mutating: bool,
    cost_class: &'static str, resource_budget: &'static str,
    supported_environments: &[&'static str], failure_recovery: &'static str,
    config: &Config, project: Option<&ProjectRegistration>, host: Option<&Value>) -> CapabilityDescriptor {
    CapabilityDescriptor {
        protocol: PROTOCOL, id, version: "1.0.0", name, purpose, activation_mode,
        activation_conditions, nonactivation_conditions,
        required_inputs: required_inputs(id),
        optional_inputs: vec!["objective", "query", "scope", "mode", "budget", "evidence_refs"],
        input_schema: input_schema(id),
        structured_outputs: vec!["status", "message", "data", "evidence_refs", "project_id", "session_id", "task_id"],
        output_schema: json!({"type":"object","required":["protocol","capability","action",
            "project_id","session_id","status","message","data","evidence_refs"],
            "properties":{"protocol":{"const":PROTOCOL},"status":{"type":"string"},
                "data":{},"evidence_refs":{"type":"array","items":{"type":"string"}}}}),
        methods: methods.to_vec(), echo_dependencies: echo_dependencies.to_vec(),
        amity_dependencies: amity_dependencies.to_vec(),
        external_dependencies: external_dependencies.to_vec(), potentially_mutating,
        permissions: if potentially_mutating {
            vec!["Amity active ACL", "Amity project authorization", "per-operation approval when required"]
        } else { vec!["Amity read-context authorization when querying the project"] },
        cost_class, resource_budget, supported_environments: supported_environments.to_vec(),
        availability: availability(id, activation_mode, config, project, host),
        limitations: limitations(id),
        failure_recovery, result_provenance: "Echo record linked to Amity response, source revision, or explicit model inference",
        training_contract: TRAINING_CONTRACT,
        training_behavior: training_behavior(id),
    }
}

fn limitations(id: &str) -> Vec<&'static str> {
    match id {
        "reality_loop" => vec!["Runtime commands and supported native editor queries require Amity authorization; no general playtest interaction adapter is installed"],
        "solution_arena" => vec!["Candidate file copies are isolated and unverified; production changes still require Amity approval"],
        "futurecast" => vec!["Speculative scenarios are not experimental implementations or measured outcomes"],
        "debug_lab" => vec!["Experiments and verification require separately authorized Amity operations"],
        "intelligent_workforce" => vec!["Workers are read-only and currently use the selected model connection"],
        "project_steward" => vec!["Scheduling runs only while Echo is open; no background service is installed"],
        "trust_certification" => vec!["Synthetic protocol scenarios and retrospective counts do not establish unrestricted coding reliability or safety"],
        "visual_to_code_navigation" => vec!["No DOM or scene selection adapter is installed by default"],
        "experience_loop" => vec!["Lessons remain local; no provider-side learning or model training occurs"],
        _ => vec![],
    }
}

fn training_behavior(id: &str) -> Value {
    let (success, nonactivation, negative, metric) = match id {
        "outcome_engine" => ("Criteria reflect the user objective and evidence-backed completion",
            "Do not formalize a simple question", "Planned steps complete but user outcome unverified",
            "criteria with traceable evidence"),
        "architecture_twin" => ("Impact view cites Amity source revision and direct observations",
            "Do not invent missing dependencies", "Model inference presented as graph fact",
            "grounded dependency references"),
        "evidence_ledger" => ("Claims use evidence status no stronger than recorded receipts",
            "Do not claim verification without a receipt", "Tool success called a passing test",
            "claim to evidence accuracy"),
        "reversible_engineering" => ("Recovery option checked against current Amity checkpoint state",
            "Do not undo a read-only task", "Undo overwrites a later external change",
            "safe recovery decisions"),
        "engineering_memory" => ("Relevant sourced memory retrieved with validity context",
            "Do not retain every transient statement", "Stale memory overrides active ACL",
            "relevant and current retrieval"),
        "black_box" => ("Ordered historical events reconstruct the observed execution",
            "Do not record hidden model reasoning", "Historical replay repeats a side effect",
            "event ordering and provenance"),
        "reality_loop" => ("Supported runtime observation linked to actual command or adapter output",
            "Do not launch for simple source question", "Missing editor reported as successful playtest",
            "observed runtime claim accuracy"),
        "solution_arena" => ("Independent candidates compared using shared criteria and evidence",
            "Do not start for trivial edit", "Read-only candidate claimed as tested implementation",
            "candidate independence and comparison quality"),
        "futurecast" => ("Scenario assumptions and predictions are bounded and labeled",
            "Do not speculate for a routine local edit", "Predicted risk presented as measured failure",
            "assumption and evidence separation"),
        "debug_lab" => ("Hypotheses revised against recorded observations",
            "Do not run prohibited tests", "Speculative fix declared confirmed",
            "hypothesis elimination accuracy"),
        "intelligent_workforce" => ("Independent read-only jobs have bounded roles and cited findings",
            "Do not delegate redundant work", "Worker writes production files",
            "delegation relevance and provenance"),
        "visual_to_code_navigation" => ("Visual reference grounded in actual project entity or ambiguity reported",
            "Do not guess when no image or editor context exists", "Ambiguous visual target modified without confirmation",
            "grounded target precision"),
        "project_steward" => ("Opted-in health observations record real project signals without silent edits",
            "Do not monitor without opt-in", "Claims monitoring continues after Echo closes",
            "actionable finding precision"),
        "trust_certification" => ("Observed or isolated protocol measures identify model, dataset, conditions, and unknowns",
            "Do not run costly evaluation without consent", "Synthetic protocol score sold as unrestricted safety certification",
            "measured reliability under declared conditions"),
        "experience_loop" => ("Consented, evidence-linked lessons stay local and remain revisable",
            "Do not retain without project opt-in", "Unverified lesson exported as approved training data",
            "lesson evidence and transfer validity"),
        _ => ("Follow the declared interface", "Do not invoke without relevance",
            "Unsupported request treated as success", "valid structured requests"),
    };
    json!({"expected_success":success,"nonactivation":nonactivation,
        "negative_example":negative,"evaluation_metric":metric,
        "rejection":"Suspend on Amity policy denial and await authorized user decision",
        "error_recovery":"Use structured status; do not fabricate completion or switch tools to evade denial",
        "completion":"Report actual evidence, unsupported portions, and outstanding blockers"})
}

fn required_inputs(id: &str) -> Vec<&'static str> {
    match id {
        "outcome_engine" => vec![],
        "solution_arena" => vec!["approaches"],
        "futurecast" => vec!["scenarios"],
        "intelligent_workforce" => vec!["jobs"],
        "experience_loop" => vec!["lesson", "evidence_refs"],
        _ => vec![],
    }
}

fn input_schema(id: &str) -> Value {
    let activation = match id {
        "outcome_engine" => json!({"objective":{"type":"string"},
            "deliverables":{"type":"array","items":{"type":"string"}},
            "criteria":{"type":"array","items":{"type":"string"}},
            "assumptions":{"type":"array","items":{"type":"string"}},
            "runtime_observation_required":{"type":"boolean"},
            "criterion_id":{"type":"string"},"evidence_ref":{"type":"string"},
            "blocker":{"type":"string"}}),
        "solution_arena" => json!({"objective":{"type":"string"},
            "approaches":{"type":"array","minItems":2,"maxItems":3,"items":{"type":"string"}},
            "files":{"type":"array","maxItems":20,"items":{"type":"string"}},
            "retain_candidates":{"type":"boolean"}}),
        "futurecast" => json!({"scenarios":{"type":"array","minItems":1,
            "maxItems":3,"items":{"type":"string"}}}),
        "intelligent_workforce" => json!({"jobs":{"type":"array","minItems":1,
            "maxItems":4,"items":{"type":"object","required":["name","role","instruction"]}}}),
        "debug_lab" => json!({"failure_report":{"type":"string"},"query":{"type":"string"},
            "hypothesis":{"type":"string"},"runtime_observation":{"type":"boolean"}}),
        "reality_loop" => json!({"command_id":{"type":"string"},"objective":{"type":"string"},
            "editor_kind":{"enum":["unreal","unity","godot"]},
            "query":{"type":"string"},"target":{"type":"string"}}),
        "visual_to_code_navigation" => json!({"image_sha256":{"type":"string"},
            "target_hint":{"type":"string"},"region":{"type":"object"}}),
        "project_steward" => json!({"auto_monitor":{"type":"boolean"},
            "interval_seconds":{"type":"integer","minimum":300}}),
        "trust_certification" => json!({"mode":{"enum":["retrospective","protocol_evaluation"]},
            "max_scenarios":{"type":"integer","minimum":1,"maximum":4}}),
        "experience_loop" => json!({"lesson":{"type":"string"},
            "evidence_refs":{"type":"array","items":{"type":"string"}},
            "operation":{"enum":["training_specification","export_training","approve_training_record"]},
            "query":{"type":"string"},"retain":{"type":"boolean"}}),
        _ => json!({"objective":{"type":"string"},"query":{"type":"string"}}),
    };
    json!({"type":"object","additionalProperties":true,
        "action":"Methods and per-action requirements are declared by this descriptor",
        "properties":activation})
}

pub fn descriptors(config: &Config, project: Option<&ProjectRegistration>,
    host: Option<&Value>) -> Vec<CapabilityDescriptor> {
    let d = |id, name, purpose, mode, when, not_when, methods: &[&'static str],
        echo: &[&'static str], amity: &[&'static str], external: &[&'static str],
        mutating, cost, budget, env: &[&'static str], recovery| {
        descriptor(id, name, purpose, mode, when, not_when, methods, echo, amity,
            external, mutating, cost, budget, env, recovery, config, project, host)
    };
    vec![
        d("outcome_engine", "Outcome Engine", "Track observable deliverables and completion criteria for the user's objective.",
            "always", "Meaningful development objective", "Simple conversational answer", &["discover","activate","status","observe"],
            &["sessions"], &["task state"], &[], false, "low", "one bounded record per objective", &["all registered projects"], "Retain incomplete criteria and report blockers"),
        d("architecture_twin", "Architecture Twin", "Read authoritative structure, source, and impact information from Amity.",
            "always", "Project investigation; deeper query for consequential changes", "No registered Amity project", &["discover","activate","status","observe"],
            &["host client"], &["AIP context","AIP impact","AIP source"], &[], false, "low", "bounded AIP queries", &["Amity projects"], "Report missing graph evidence or stale revision"),
        d("evidence_ledger", "Evidence Ledger", "Link engineering claims to recorded source, execution, and verification evidence.",
            "always", "Engineering action or completion claim", "No engineering claim", &["discover","activate","status","observe"],
            &["session events"], &["Flight Recorder","verification records"], &[], false, "low", "bounded evidence metadata", &["Amity projects"], "Mark unsupported claims unverified"),
        d("reversible_engineering", "Reversible Engineering", "Inspect governed checkpoints and safe recovery options.",
            "always", "Project mutation or recovery request", "Read-only answer", &["discover","activate","status","observe"],
            &["session task links"], &["agent-tool checkpoints","agent-tool undo_checkpoint"], &[], true, "low", "one checkpoint lookup per request", &["Amity projects"], "Refuse ambiguous restore; use Amity's governed undo path"),
        d("engineering_memory", "Engineering Memory", "Retrieve and maintain sourced project knowledge across model changes.",
            "always", "Relevant prior project decision or finding", "Unrelated project or stale record", &["discover","activate","status","observe","configure"],
            &["local session storage"], &["active ACL remains authoritative"], &[], false, "low", "bounded retrieved records", &["local projects"], "Correct, retire, or delete stale records"),
        d("black_box", "Black Box", "Reconstruct ordered agent actions without replaying side effects.",
            "always", "Relevant agent action or incident review", "No recorded events", &["discover","activate","status","observe"],
            &["structured events","session storage"], &["Flight Recorder"], &[], false, "low", "bounded event metadata", &["local projects"], "Historical replay remains read-only; experimental replay requires fresh authorization"),
        d("reality_loop", "Reality Loop", "Observe supported application behavior and compare it with expected outcomes.",
            "conditional", "Behavioral outcome with configured runtime observation", "Simple source question or prohibited execution", &["discover","activate","status","observe","pause","resume","deactivate","configure"],
            &["attachments","editor context"], &["fixed commands","native discovery","ACL execution policy"], &["configured application runtime"], true, "medium", "bounded observations and processes", &["configured command environments"], "Report unavailable runtime or prohibited execution without fabricated results"),
        d("solution_arena", "Solution Arena", "Compare independent candidate approaches under the same objective and criteria.",
            "conditional", "Complex task with meaningful alternatives", "Trivial edit or exhausted budget", &["discover","activate","status","pause","resume","deactivate","configure"],
            &["read-only subagents"], &["read-only project context"], &[], false, "high", "bounded candidate and worker count", &["registered projects"], "Reject unsupported candidate claims; production changes require normal governance"),
        d("futurecast", "Futurecast", "Assess bounded future scenarios against current architecture and evidence.",
            "conditional", "Foundational design or user-requested extensibility analysis", "Routine local edit", &["discover","activate","status","configure"],
            &["architecture_twin"], &["AIP impact","AIP source"], &[], false, "medium", "bounded scenario count", &["Amity projects"], "Separate predictions from observed failures"),
        d("debug_lab", "Debugging Laboratory", "Track failure hypotheses, experiments, observations, and verification state.",
            "conditional", "Bug, crash, regression, or unexpected behavior", "No reported failure", &["discover","activate","status","observe","pause","resume","deactivate","configure"],
            &["session storage","reality_loop"], &["agent-tool read","agent-tool commands","verification records"], &[], true, "medium", "bounded experiment count", &["registered projects"], "Preserve unresolved hypotheses and report blocked reproduction"),
        d("intelligent_workforce", "Intelligent Workforce", "Coordinate bounded specialized read-only workers and reconcile findings.",
            "conditional", "Independent investigation workstreams", "Small task or redundant delegation", &["discover","activate","status","configure"],
            &["read-only subagents"], &["read-only project context"], &[], false, "high", "at most four workers and configured budget", &["registered projects"], "Cancel workers and retain provenance of partial findings"),
        d("visual_to_code_navigation", "Visual-to-Code Navigation", "Ground a selected visual element in real source or editor metadata.",
            "conditional", "Image or editor selection with a development objective", "No grounded visual reference", &["discover","activate","status","configure"],
            &["image attachments","VS Code context"], &["agent-tool search","native editor query"], &["vision-capable model for image interpretation"], false, "medium", "bounded asset and source search", &["VS Code","supported image models"], "Ask for clarification when target identity is ambiguous"),
        d("project_steward", "Project Steward", "Monitor opted-in project health and produce findings without silent edits.",
            "persistent", "Explicit project opt-in and scheduled or manual review", "No opt-in or terminal closed without service", &["discover","activate","status","observe","configure"],
            &["local project state"], &["AIP context","Flight Recorder"], &[], false, "medium", "configured schedule and retention", &["local foreground sessions"], "Pause monitoring and preserve last findings"),
        d("trust_certification", "Trust Certification", "Measure retrospective behavior or run isolated, synthetic capability-protocol challenges on request.",
            "persistent", "Explicit model evaluation request", "No consent or selected model connection", &["discover","activate","status","configure"],
            &["black_box","training records","model transport"], &["ACL boundary"], &["configured model process"], false, "high", "explicit evaluation budget", &["local isolated protocol evaluation"], "Report unmeasured coding and safety dimensions; evaluation never grants permission"),
        d("experience_loop", "Experience Loop", "Retain consented, evidence-backed lessons across model changes.",
            "persistent", "Explicit project retention opt-in", "No consent or unverified lesson", &["discover","activate","status","observe","configure"],
            &["engineering_memory","evidence_ledger"], &["active ACL remains authoritative"], &[], false, "low", "configured retention and local storage", &["local projects"], "Retire stale lessons and never upload automatically"),
    ]
}

pub fn availability(id: &str, mode: &str, config: &Config,
    project: Option<&ProjectRegistration>, host: Option<&Value>) -> Availability {
    let override_value = project.and_then(|project| config.capabilities.get("projects")
        .and_then(|projects| projects.get(project.id.as_str()))
        .and_then(|settings| settings.get("overrides"))
        .and_then(|overrides| overrides.get(id)))
        .or_else(|| config.capabilities.get("overrides").and_then(|overrides| overrides.get(id)));
    let default_enabled = mode != "persistent";
    let enabled = override_value.and_then(|value| value.get("enabled"))
        .and_then(Value::as_bool).unwrap_or(default_enabled);
    let activation = override_value.and_then(|value| value.get("activation"))
        .and_then(Value::as_str).unwrap_or(if mode == "always" { "automatic" }
            else if mode == "persistent" { "manual" } else { "automatic" });
    if !enabled || activation == "off" {
        return Availability { status: "unavailable", reason: "Disabled in Echo settings".into(),
            enabled: false, activation: activation.into(), execution_available: Some(false) };
    }
    if project.is_none() {
        return Availability { status: "unavailable", reason: "Connect an authorized Amity project".into(),
            enabled, activation: activation.into(), execution_available: Some(false) };
    }
    let host_result = host.and_then(|value| value.get("result")).unwrap_or(&Value::Null);
    if id == "reality_loop" {
        let has_command = host_result.get("commands").and_then(Value::as_array)
            .is_some_and(|commands| commands.iter().any(|command| {
                matches!(command.get("category").and_then(Value::as_str),
                    Some("editor" | "runtime" | "observe" | "preview"))
            }));
        let has_editor = host_result.get("editor_queries").and_then(Value::as_array)
            .is_some_and(|editors| editors.iter().any(|editor|
                editor.get("available").and_then(Value::as_bool) == Some(true)));
        return Availability { status: "available", enabled, activation: activation.into(),
            execution_available: Some(has_command || has_editor),
            reason: if has_command { "Governed runtime command configured".into() }
                else if has_editor { "Governed native editor query available".into() }
                else { "No executable runtime observation is configured in Amity".into() } };
    }
    if id == "visual_to_code_navigation" {
        let vision = project.is_some_and(|project| {
            project.selected_model.as_ref().and_then(|name| project.models.iter()
                .find(|profile| &profile.name == name)).is_some_and(|profile| profile.supports_images)
        });
        return Availability { status: "available", enabled, activation: activation.into(),
            execution_available: Some(vision),
            reason: if vision { "Image and editor references supported".into() }
                else { "Editor context supported; image interpretation needs a vision-capable model".into() } };
    }
    Availability { status: "available", reason: "Ready for this project".into(), enabled,
        activation: activation.into(), execution_available: None }
}

pub fn prompt_catalog(config: &Config, project: Option<&ProjectRegistration>,
    host: Option<&Value>) -> String {
    let catalog = descriptors(config, project, host);
    let brief = catalog.iter().map(|item| {
        json!({"id":item.id,"purpose":item.purpose,"availability":item.availability.status,
            "activation":item.availability.activation})
    }).collect::<Vec<_>>();
    json!({"protocol":PROTOCOL,"tool":"echo_capability",
        "request":{"protocol":PROTOCOL,"capability":"<catalog ID>",
            "action":"discover|activate|status|observe|pause|resume|deactivate|configure",
            "input":{}},"catalog":brief}).to_string()
}
