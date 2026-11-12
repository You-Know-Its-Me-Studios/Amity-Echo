//! Conversational use of Amity's configured JSON-over-stdio model backend.
//! The process receives the same ModelInput fields as Amity task_runtime.
//! Echo validates each model turn and sends requested tools to the Amity host.

use crate::config::ProjectRegistration;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Deserialize)]
struct ModelBinding {
    project: String,
    project_root: PathBuf,
    model_command: Option<PathBuf>,
    #[serde(default)]
    model_args: Vec<String>,
    model_version: Option<String>,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelTurn {
    AipRequest { request: String },
    ToolRequest { tool: String, arguments: Value },
    FinalProposal { proposal: String },
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub context_window: Option<u64>,
    pub cost_usd: Option<f64>,
}

pub enum ModelProgress {
    Delta(String),
    Usage(ModelUsage),
    Heartbeat,
}

enum ProcessRecord {
    Line(String),
    End(std::io::Result<()>),
}

#[derive(Clone)]
pub struct ChatModel {
    command: PathBuf,
    args: Vec<String>,
    pub version: String,
    timeout: Duration,
    supports_images: bool,
}

impl ChatModel {
    pub fn open(project: &ProjectRegistration, config_path: &Path, timeout: Duration) -> Result<Self> {
        let binding: ModelBinding = serde_json::from_slice(&fs::read(config_path)
            .with_context(|| format!("reading Amity host config {}", config_path.display()))?)
            .context("parsing Amity model binding")?;
        let root = binding.project_root.canonicalize().context("resolving Amity project root")?;
        if binding.project != project.id || root != project.path {
            bail!("selected model host config belongs to another project");
        }
        let command = binding.model_command.context("the selected Amity host config has no model_command")?;
        if command.as_os_str().is_empty() || binding.model_args.len() > 32
            || binding.model_args.iter().any(|arg| arg.len() > 4096 || arg.contains('\0')) {
            bail!("invalid Amity model process configuration");
        }
        if timeout.is_zero() || timeout > Duration::from_secs(300) {
            bail!("model timeout must be between 1 and 300 seconds");
        }
        let version = binding.model_version.filter(|value| !value.trim().is_empty())
            .context("the selected Amity host config has no model_version")?;
        let supports_images = project.selected_model.as_ref()
            .and_then(|name| project.models.iter().find(|profile| &profile.name == name))
            .is_some_and(|profile| profile.supports_images);
        Ok(Self { command, args: binding.model_args, version, timeout, supports_images })
    }

    pub fn supports_images(&self) -> bool { self.supports_images }

    /// A connector may emit JSONL delta/usage records before its final ModelTurn.
    /// Existing connectors that emit one JSON document remain compatible.
    pub fn complete_streaming(&self, input: &Value, cancel: &AtomicBool,
        mut progress: impl FnMut(ModelProgress)) -> Result<ModelTurn> {
        let payload = serde_json::to_vec(input).context("serializing model input")?;
        if payload.len() > 8 * 1024 * 1024 { bail!("model input exceeds Amity's 8 MiB limit"); }
        if cancel.load(Ordering::Relaxed) { bail!("model turn cancelled"); }
        let mut command = Command::new(&self.command);
        command.args(&self.args).env_clear()
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        if self.args.first().is_some_and(|arg| arg == "__model_bridge")
            && self.args.get(1).is_some_and(|arg| arg == "api_chat") {
            let name = self.args.get(4).context("API model credential variable is missing")?;
            crate::bridge::validate_key_env(name)?;
            let value = std::env::var(name).with_context(|| format!("set {name} before starting Echo"))?;
            command.env(name, value);
        }
        let mut child = command.spawn().with_context(|| format!("starting configured model process {}", self.command.display()))?;
        let outcome = (|| -> Result<ModelTurn> {
        let mut stdin = child.stdin.take().context("model stdin unavailable")?;
        let (write_tx, write_rx) = mpsc::channel();
        thread::spawn(move || {
            let result = stdin.write_all(&payload);
            drop(stdin);
            let _ = write_tx.send(result);
        });
        let stdout = child.stdout.take().context("model stdout unavailable")?;
        let (read_tx, read_rx) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut total = 0usize;
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => { let _ = read_tx.send(ProcessRecord::End(Ok(()))); break; }
                    Ok(count) => {
                        total += count;
                        if total > 2 * 1024 * 1024 || line.len() > 128 * 1024 {
                            let _ = read_tx.send(ProcessRecord::End(Err(std::io::Error::other("model stream exceeds size limit"))));
                            break;
                        }
                        if read_tx.send(ProcessRecord::Line(line)).is_err() { break; }
                    }
                    Err(error) => { let _ = read_tx.send(ProcessRecord::End(Err(error))); break; }
                }
            }
        });
        let deadline = Instant::now() + self.timeout;
        let mut final_turn = None;
        let mut stream_done = false;
        let mut legacy_document = String::new();
        while !stream_done {
            if cancel.load(Ordering::Relaxed) {
                let _ = child.kill(); let _ = child.wait();
                bail!("model turn cancelled");
            }
            if Instant::now() >= deadline {
                let _ = child.kill(); let _ = child.wait();
                bail!("model process timed out");
            }
            match read_rx.recv_timeout(Duration::from_millis(25)) {
                Ok(ProcessRecord::Line(line)) if line.trim().is_empty() => {},
                Ok(ProcessRecord::Line(line)) => {
                    if !legacy_document.is_empty() {
                        legacy_document.push_str(&line);
                        continue;
                    }
                    let value: Value = match serde_json::from_str(&line) {
                        Ok(value) => value,
                        Err(_) if final_turn.is_none() => {
                            legacy_document.push_str(&line);
                            continue;
                        }
                        Err(error) => return Err(error).context("model returned invalid streaming JSON"),
                    };
                    match value.get("type").and_then(Value::as_str) {
                        Some("delta") => {
                            let delta = value.get("text").and_then(Value::as_str)
                                .context("model delta has no text")?;
                            if delta.len() > 16 * 1024 { bail!("model delta exceeds size limit"); }
                            progress(ModelProgress::Delta(delta.to_owned()));
                        }
                        Some("usage") => {
                            let usage: ModelUsage = serde_json::from_value(value)
                                .context("model usage record is invalid")?;
                            progress(ModelProgress::Usage(usage));
                        }
                        _ => {
                            if final_turn.is_some() { bail!("model returned multiple final turns"); }
                            final_turn = Some(serde_json::from_value(value)
                                .context("model returned invalid ModelTurn JSON")?);
                        }
                    }
                }
                Ok(ProcessRecord::End(result)) => { result?; stream_done = true; },
                Err(mpsc::RecvTimeoutError::Timeout) => progress(ModelProgress::Heartbeat),
                Err(mpsc::RecvTimeoutError::Disconnected) => bail!("model output stream disconnected"),
            }
        }
        let status = wait(&mut child, deadline.saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1)), cancel)?;
        if !legacy_document.is_empty() {
            if final_turn.is_some() { bail!("model mixed JSONL and document responses"); }
            final_turn = Some(serde_json::from_str(&legacy_document)
                .context("model returned invalid ModelTurn JSON")?);
        }
        write_rx.recv_timeout(Duration::from_secs(2))
            .context("model stdin did not close")??;
        if !status.success() { bail!("model process exited with {status}"); }
        let turn: ModelTurn = final_turn.context("model returned no final ModelTurn")?;
        match &turn {
            ModelTurn::AipRequest { request } if request.len() > 64 * 1024 => bail!("model AIP request exceeds limit"),
            ModelTurn::ToolRequest { tool, arguments }
                if tool.trim().is_empty() || tool.len() > 80 || !arguments.is_object()
                    || arguments.to_string().len() > 128 * 1024 => bail!("model tool request is invalid or exceeds limit"),
            ModelTurn::FinalProposal { proposal } if proposal.trim().is_empty() || proposal.len() > 32 * 1024 => bail!("model answer exceeds limit or is empty"),
            _ => Ok(turn),
        }
        })();
        if outcome.is_err() { let _ = child.kill(); let _ = child.wait(); }
        outcome
    }
}

fn wait(child: &mut Child, timeout: Duration, cancel: &AtomicBool) -> Result<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill(); let _ = child.wait();
            bail!("model turn cancelled");
        }
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
            Ok(None) => { let _ = child.kill(); let _ = child.wait(); bail!("model process timed out"); }
            Err(error) => { let _ = child.kill(); let _ = child.wait(); return Err(error).context("waiting for model process"); }
        }
    }
}
