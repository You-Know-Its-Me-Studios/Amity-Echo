//! Conversational use of Amity's configured JSON-over-stdio model backend.
//! The process receives the same ModelInput fields as Amity task_runtime.
//! Echo validates each model turn and sends requested tools to the Amity host.

use crate::config::ProjectRegistration;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;
use std::fs;
use std::io::{Read, Write};
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

pub struct ChatModel {
    command: PathBuf,
    args: Vec<String>,
    pub version: String,
    timeout: Duration,
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
        Ok(Self { command, args: binding.model_args, version, timeout })
    }

    pub fn complete(&self, input: &Value, cancel: &AtomicBool) -> Result<ModelTurn> {
        let payload = serde_json::to_vec(input).context("serializing model input")?;
        if payload.len() > 8 * 1024 * 1024 { bail!("model input exceeds Amity's 8 MiB limit"); }
        if cancel.load(Ordering::Relaxed) { bail!("model turn cancelled"); }
        let mut child = Command::new(&self.command)
            .args(&self.args).env_clear()
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
            .spawn().with_context(|| format!("starting configured model process {}", self.command.display()))?;
        let mut stdin = child.stdin.take().context("model stdin unavailable")?;
        let (write_tx, write_rx) = mpsc::channel();
        thread::spawn(move || {
            let result = stdin.write_all(&payload);
            drop(stdin);
            let _ = write_tx.send(result);
        });
        let stdout = child.stdout.take().context("model stdout unavailable")?;
        let (read_tx, read_rx) = mpsc::channel();
        thread::spawn(move || { let _ = read_tx.send(read_capped(stdout, 128 * 1024)); });
        let status = wait(&mut child, self.timeout, cancel)?;
        let deadline = Instant::now() + Duration::from_secs(2);
        write_rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .context("model stdin did not close")??;
        let (output, oversized) = read_rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .context("model stdout did not close")??;
        if !status.success() { bail!("model process exited with {status}"); }
        if oversized { bail!("model response exceeds Amity's 128 KiB limit"); }
        let turn: ModelTurn = serde_json::from_slice(&output).context("model returned invalid ModelTurn JSON")?;
        match &turn {
            ModelTurn::AipRequest { request } if request.len() > 64 * 1024 => bail!("model AIP request exceeds limit"),
            ModelTurn::ToolRequest { tool, arguments }
                if tool.trim().is_empty() || tool.len() > 80 || !arguments.is_object()
                    || arguments.to_string().len() > 128 * 1024 => bail!("model tool request is invalid or exceeds limit"),
            ModelTurn::FinalProposal { proposal } if proposal.trim().is_empty() || proposal.len() > 32 * 1024 => bail!("model answer exceeds limit or is empty"),
            _ => Ok(turn),
        }
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

fn read_capped(mut reader: impl Read, limit: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut output = Vec::new();
    let mut oversized = false;
    let mut chunk = [0u8; 8192];
    loop {
        let count = reader.read(&mut chunk)?;
        if count == 0 { break; }
        let remaining = limit.saturating_sub(output.len());
        output.extend_from_slice(&chunk[..count.min(remaining)]);
        if count > remaining { oversized = true; }
    }
    Ok((output, oversized))
}
