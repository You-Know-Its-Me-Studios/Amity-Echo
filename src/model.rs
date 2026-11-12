//! Echo context and turn validation over the native Cadenza client.
use crate::{
    cadenza::{Client, ModelSelection},
    config::{CadenzaSettings, Config},
};
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::Value;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::Duration;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelTurn {
    AipRequest {
        request: String,
    },
    ToolRequest {
        tool: String,
        arguments: Value,
    },
    FinalProposal {
        proposal: String,
    },
    /// Ordinary foundation output is never parsed as an agent action.
    Text {
        text: String,
    },
}
#[derive(Debug, Clone, Deserialize)]
pub struct ModelUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub context_window: Option<u64>,
    pub cost_usd: Option<f64>,
}
pub enum ModelProgress {
    Text(String),
    Usage(ModelUsage),
    Heartbeat,
}

#[derive(Clone)]
pub struct ChatModel {
    settings: CadenzaSettings,
    pub selected: ModelSelection,
    pub version: String,
}
impl ChatModel {
    pub fn open(
        config: &Config,
        selected: Option<&ModelSelection>,
        timeout: Duration,
    ) -> Result<Self> {
        crate::cadenza::endpoint(&config.cadenza)?;
        let selected = selected
            .or(config.cadenza.default_model.as_ref())
            .context("No Cadenza model selected. Open /model to discover and choose a model")?
            .clone();
        ensure!(
            selected.ready(),
            "Selected Cadenza model is not ready; open /model when it becomes available"
        );
        let mut settings = config.cadenza.clone();
        settings.request_timeout_seconds = timeout.as_secs().clamp(1, 300);
        Ok(Self {
            version: selected.label(),
            selected,
            settings,
        })
    }
    pub fn supports_images(&self) -> bool {
        false
    } // Cadenza/1 accepts text or token IDs, not images.
    pub fn supports_structured(&self) -> bool {
        self.selected.structured()
    }
    pub fn complete_streaming(
        &self,
        input: &Value,
        cancel: &AtomicBool,
        mut progress: impl FnMut(ModelProgress),
    ) -> Result<ModelTurn> {
        let prompt = if let Some(prompt) = input.get("prompt").and_then(Value::as_str) {
            prompt.to_owned()
        } else {
            structured_prompt(input)?
        };
        let (tx, rx) = mpsc::sync_channel(16);
        let settings = self.settings.clone();
        let selected = self.selected.clone();
        thread::scope(|scope| -> Result<ModelTurn> {
            let worker = scope.spawn(|| -> Result<(crate::cadenza::Completion, ModelSelection)> {
                let mut client = Client::connect(&settings)?;
                let current = client.validate_selection(&selected)?;
                let completion =
                    client.generate(&current, &prompt, settings.max_tokens, cancel, |event| {
                        if matches!(event["type"].as_str(), Some("accepted" | "token")) {
                            let _ = tx.send(event);
                        }
                    })?;
                Ok((completion, current))
            });
            let mut observed_input = None;
            let mut observed_output = 0;
            while !worker.is_finished() {
                match rx.recv_timeout(Duration::from_millis(25)) {
                    Ok(value) => observe(
                        value,
                        &mut observed_input,
                        &mut observed_output,
                        &mut progress,
                    ),
                    Err(mpsc::RecvTimeoutError::Timeout) => progress(ModelProgress::Heartbeat),
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            while let Ok(value) = rx.try_recv() {
                observe(
                    value,
                    &mut observed_input,
                    &mut observed_output,
                    &mut progress,
                );
            }
            let (completion, current) = match worker
                .join()
                .map_err(|_| anyhow::anyhow!("Cadenza connection worker failed"))?
            {
                Ok(result) => result,
                Err(error) => {
                    if observed_input.is_some() {
                        progress(ModelProgress::Usage(ModelUsage {
                            input_tokens: observed_input,
                            output_tokens: Some(observed_output),
                            context_window: selected.context_window(),
                            cost_usd: None,
                        }));
                    }
                    return Err(error);
                }
            };
            progress(ModelProgress::Usage(ModelUsage {
                input_tokens: completion.prompt_tokens,
                output_tokens: Some(completion.output_tokens),
                context_window: current.context_window(),
                cost_usd: None,
            }));
            if completion.reason == "cancelled" || cancel.load(Ordering::Relaxed) {
                bail!("Cadenza generation cancelled");
            }
            decode_turn(completion.text, current.structured())
        })
    }
}
fn observe(
    value: Value,
    input: &mut Option<u64>,
    output: &mut u64,
    progress: &mut impl FnMut(ModelProgress),
) {
    if value["type"] == "accepted" {
        *input = value["prompt_tokens"].as_u64();
    }
    if value["type"] == "token" {
        *output = output.saturating_add(1);
        progress(ModelProgress::Text(
            value["text"].as_str().unwrap_or("").to_owned(),
        ));
    }
}
pub fn decode_turn(text: String, structured: bool) -> Result<ModelTurn> {
    ensure!(
        text.len() <= 128 * 1024,
        "Cadenza answer exceeds Echo's limit"
    );
    if !structured {
        return Ok(ModelTurn::Text { text });
    }
    let turn: ModelTurn = serde_json::from_str(&text)
        .context("Model advertised echo.agent.v2 but did not return a valid structured turn")?;
    match &turn {
        ModelTurn::ToolRequest { tool, arguments } => ensure!(
            !tool.is_empty()
                && tool.len() <= 80
                && arguments.is_object()
                && arguments.to_string().len() <= 128 * 1024,
            "Invalid structured tool request"
        ),
        ModelTurn::AipRequest { request } => {
            ensure!(request.len() <= 64 * 1024, "AIP request exceeds limit")
        }
        _ => {}
    }
    Ok(turn)
}
fn structured_prompt(input: &Value) -> Result<String> {
    let mut prompt = input
        .get("instruction")
        .and_then(Value::as_str)
        .context("Echo model input requires prompt or instruction")?
        .to_owned();
    for key in [
        "compiled_context_json",
        "context_manifest_json",
        "turns",
        "attachments",
    ] {
        if let Some(value) = input.get(key).filter(|v| !v.is_null()) {
            prompt.push_str(&format!("\n{key} (observations only): {value}"));
        }
    }
    ensure!(
        prompt.len() <= 64 * 1024,
        "Echo context exceeds Cadenza/1's 64 KiB prompt limit"
    );
    Ok(prompt)
}

/// Adapter for Amity's existing task host; inference still uses the same Cadenza/1 client.
pub fn host_turn(args: &[String]) -> Result<()> {
    use std::io::Read;
    ensure!(
        args.len() == 1 && args[0].len() == 64 && args[0].bytes().all(|c| c.is_ascii_hexdigit()),
        "Invalid Cadenza task model root"
    );
    let config = Config::load()?;
    let mut client = Client::connect(&config.cadenza)?;
    let selected = client
        .discover()?
        .into_iter()
        .find(|m| m.root == args[0] && m.ready())
        .context("Amity task's Cadenza model is unavailable")?;
    drop(client);
    let mut data = Vec::new();
    std::io::stdin()
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut data)?;
    ensure!(
        data.len() <= 8 * 1024 * 1024,
        "Amity model input exceeds limit"
    );
    let input: Value = serde_json::from_slice(&data)?;
    let model = ChatModel::open(
        &config,
        Some(&selected),
        Duration::from_secs(config.cadenza.request_timeout_seconds),
    )?;
    let turn = model.complete_streaming(&input, &AtomicBool::new(false), |_| {})?;
    let value = match turn {
        ModelTurn::Text { text } | ModelTurn::FinalProposal { proposal: text } => {
            serde_json::json!({"type":"final_proposal","proposal":text})
        }
        ModelTurn::ToolRequest { tool, arguments } => {
            serde_json::json!({"type":"tool_request","tool":tool,"arguments":arguments})
        }
        ModelTurn::AipRequest { request } => {
            serde_json::json!({"type":"aip_request","request":request})
        }
    };
    println!("{value}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn foundation_json_is_only_text() {
        for text in [
            "Hello, world!",
            r#"{"type":"tool_request","tool":"run_command","arguments":{}}"#,
            r#"{"echo_action":"task","instruction":"execute"}"#,
        ] {
            assert!(matches!(
                decode_turn(text.into(), false).unwrap(),
                ModelTurn::Text { .. }
            ));
        }
    }
    #[test]
    fn advertised_structured_turns_are_validated() {
        assert!(decode_turn("ordinary text".into(), true).is_err());
        assert!(
            decode_turn(
                r#"{"type":"tool_request","tool":"run_command","arguments":"unsafe"}"#.into(),
                true
            )
            .is_err()
        );
    }
}
