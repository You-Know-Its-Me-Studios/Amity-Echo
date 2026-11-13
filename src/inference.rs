//! Echo's inference boundary. The current deployment uses native Cadenza/1.
//! A future Amity relay belongs behind this interface, not in the terminal,
//! session store, or agent workflow.
use crate::{cadenza, config::CadenzaSettings};
use anyhow::Result;
use serde_json::Value;
use std::sync::atomic::AtomicBool;

pub use cadenza::{Completion, ModelSelection};

/// Presentation choices contain complete inference selections, so choosing an
/// effort can never become a display-only setting or an invented wire option.
pub struct ModelChoice {
    pub model: ModelSelection,
    pub reasoning: Vec<ReasoningChoice>,
}

pub struct ReasoningChoice {
    pub label: String,
    pub description: String,
    pub is_default: bool,
    pub selection: ModelSelection,
}

pub struct Connection(cadenza::Client);

impl Connection {
    pub fn connect(settings: &CadenzaSettings) -> Result<Self> {
        Ok(Self(cadenza::Client::connect(settings)?))
    }

    pub fn discover(&mut self) -> Result<Vec<ModelSelection>> { self.0.discover() }
    pub fn model_choices(&mut self) -> Result<Vec<ModelChoice>> {
        // Cadenza/1 has no configurable reasoning control in discovery or
        // generation. Only an adapter that can apply an effort may offer it.
        Ok(self.discover()?.into_iter().map(|model| ModelChoice {
            model, reasoning: Vec::new(),
        }).collect())
    }
    pub fn health(&mut self) -> Result<Value> { self.0.health() }
    pub fn validate_selection(&mut self, model: &ModelSelection) -> Result<ModelSelection> {
        self.0.validate_selection(model)
    }
    pub fn generate(&mut self, model: &ModelSelection, prompt: &str,
        max_tokens: u16, cancel: &AtomicBool,
        progress: impl FnMut(Value)) -> Result<Completion> {
        self.0.generate(model, prompt, max_tokens, cancel, progress)
    }
}

pub fn validate_endpoint(settings: &CadenzaSettings) -> Result<()> {
    cadenza::endpoint(settings).map(|_| ())
}
