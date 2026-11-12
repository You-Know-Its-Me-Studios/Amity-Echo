//! Shared bounded resource accounting. Provider usage remains reported, never invented.
use crate::config::Config;
use crate::state::Session;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Budget {
    pub profile: String,
    pub max_invocations: u64,
    pub max_depth: usize,
    pub max_workers: usize,
    pub max_model_calls: u64,
    pub deadline_ms: u128,
    pub max_reported_cost_usd: Option<f64>,
    pub base_reported_cost_usd: f64,
}

impl Budget {
    pub fn for_session(config: &Config, session: &Session) -> Self {
        let settings = &config.capabilities;
        let project_settings = settings.get("projects")
            .and_then(|projects| projects.get(session.project_id.as_str()));
        let project_budget = project_settings.and_then(|value| value.get("budget"));
        let global_budget = settings.get("budget");
        let setting = |key: &str| project_budget.and_then(|value| value.get(key))
            .or_else(|| global_budget.and_then(|value| value.get(key)));
        let profile = project_settings.and_then(|value| value.get("budget_profile"))
            .or_else(|| settings.get("budget_profile")).and_then(Value::as_str)
            .unwrap_or("balanced");
        let (calls, workers, seconds, model_calls) = match profile {
            "low_cost" => (20, 1, 300, 16),
            "extensive" => (160, 4, 3600, 256),
            _ => (64, 3, 1200, 72),
        };
        let max_invocations = setting("max_invocations")
            .and_then(Value::as_u64).unwrap_or(calls).clamp(1, 1000);
        let max_workers = setting("max_workers")
            .and_then(Value::as_u64).unwrap_or(workers).clamp(1, 4) as usize;
        let max_seconds = setting("max_seconds")
            .and_then(Value::as_u64).unwrap_or(seconds).clamp(30, 86400);
        let deadline_ms = session.capability_state.pointer("/budget/deadline_ms")
            .and_then(Value::as_u64).map(u128::from)
            .unwrap_or_else(|| now_ms().saturating_add(u128::from(max_seconds) * 1000));
        Self {
            profile: profile.into(), max_invocations,
            max_depth: setting("max_depth").and_then(Value::as_u64)
                .unwrap_or(4).clamp(1, 8) as usize,
            max_workers,
            max_model_calls: setting("max_model_calls").and_then(Value::as_u64)
                .unwrap_or(model_calls).clamp(1, 10000),
            deadline_ms,
            max_reported_cost_usd: setting("max_reported_cost_usd")
                .and_then(Value::as_f64).filter(|value| value.is_finite() && *value >= 0.0),
            base_reported_cost_usd: session.capability_state.pointer("/budget/base_reported_cost_usd")
                .and_then(Value::as_f64).unwrap_or(session.usage_cost_usd),
        }
    }

    pub fn check(&self, session: &Session) -> Result<(), &'static str> {
        let count = session.capability_state.pointer("/budget/invocations")
            .and_then(Value::as_u64).unwrap_or(0);
        if count >= self.max_invocations { return Err("capability invocation budget exhausted"); }
        if now_ms() > self.deadline_ms { return Err("capability time budget exhausted"); }
        if self.max_reported_cost_usd.is_some_and(|limit| {
            session.usage_cost_reported
                && (session.usage_cost_usd - self.base_reported_cost_usd).max(0.0) >= limit
        }) { return Err("reported model cost budget exhausted"); }
        Ok(())
    }

    pub fn charge(&self, session: &mut Session) {
        if !session.capability_state.is_object() {
            session.capability_state = json!({});
        }
        let state = session.capability_state.as_object_mut().expect("object set above");
        let budget = state.entry("budget").or_insert_with(|| json!({}));
        if !budget.is_object() { *budget = json!({}); }
        let fields = budget.as_object_mut().expect("object set above");
        let count = fields.get("invocations").and_then(Value::as_u64).unwrap_or(0);
        fields.insert("invocations".into(), json!(count.saturating_add(1)));
        fields.entry("deadline_ms").or_insert_with(|| json!(self.deadline_ms));
        fields.entry("base_reported_cost_usd")
            .or_insert_with(|| json!(self.base_reported_cost_usd));
    }

    pub fn reserve_model_calls(&self, session: &mut Session, count: u64)
        -> Result<(), &'static str> {
        let prior = session.capability_state.pointer("/budget/reserved_model_calls")
            .and_then(Value::as_u64).unwrap_or(0);
        if prior.saturating_add(count) > self.max_model_calls {
            return Err("model call budget would be exceeded");
        }
        if !session.capability_state.is_object() { session.capability_state = json!({}); }
        let state = session.capability_state.as_object_mut().expect("object set above");
        let budget = state.entry("budget").or_insert_with(|| json!({}));
        if !budget.is_object() { *budget = json!({}); }
        budget.as_object_mut().expect("object set above")
            .insert("reserved_model_calls".into(), json!(prior.saturating_add(count)));
        Ok(())
    }
}

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()
}
