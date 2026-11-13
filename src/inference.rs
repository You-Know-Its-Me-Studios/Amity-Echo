//! Echo's inference boundary. The current deployment uses native Cadenza/1.
//! A future Amity relay belongs behind this interface, not in the terminal,
//! session store, or agent workflow.
use crate::{cadenza, config::CadenzaSettings};
use anyhow::Result;
use serde_json::Value;
use std::sync::atomic::AtomicBool;

pub use cadenza::{Completion, ModelSelection};

pub fn available_models_message(count: usize) -> String {
    format!("{count} model{} available. Use /model to select.", if count == 1 { "" } else { "s" })
}

/// Translate infrastructure diagnostics only at the human presentation boundary.
/// Original errors remain intact in structured output and stored task diagnostics.
pub fn user_message(diagnostic: &str) -> &str {
    let lower = diagnostic.to_ascii_lowercase();
    if !lower.contains("cadenza") { return diagnostic; }
    if lower.contains("rejected the supplied credential") || lower.contains("authentication_or_version_failed") {
        "Model authentication failed. Check your access settings in /settings."
    } else if lower.contains("credential") {
        "Model authentication is unavailable. Check your access settings in /settings."
    } else if lower.contains("cancelled") {
        "Model request cancelled."
    } else if lower.contains("no cadenza model selected") {
        "No model selected. Use /model to select."
    } else if lower.contains("model is unavailable") || lower.contains("model is not ready") || lower.contains("worker is not ready") {
        "Selected model is unavailable. Use /model to choose another."
    } else if lower.contains("cadenza unavailable") || lower.contains("resolving cadenza host") {
        "Models unavailable. Try again later."
    } else if lower.contains("handshake") || lower.contains("tls") || lower.contains("certificate") {
        "Could not establish a secure model connection. Check /settings."
    } else if lower.contains("endpoint") || lower.contains("generation preference") {
        "Model connection settings are invalid. Check /settings."
    } else if lower.contains("timed out") {
        "The model took too long to respond. Try again."
    } else if lower.contains("disconnected") || lower.contains("connection interrupted") {
        "Model connection interrupted. Send your request again when the connection is available."
    } else if lower.contains("prompt") || lower.contains("context exceeds") {
        "This request exceeds the model's input limit. Shorten it or start a new conversation."
    } else if lower.contains("output exceeds") || lower.contains("answer exceeds") {
        "The model response exceeded Echo's size limit."
    } else if lower.contains("image input") {
        "This model connection does not support images. Your attachment remains in the conversation."
    } else if lower.contains("overloaded") {
        "The model is busy. Try again shortly."
    } else {
        "The model request failed. Try again or use /model to choose another model."
    }
}

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
