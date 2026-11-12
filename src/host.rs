//! Transport for the existing `amity` host CLI. The host remains the sole
//! owner of project context, authority, gateway execution, and task evidence.

use serde_json::{json, Value};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DEFAULT_OUTPUT_LIMIT: usize = 16 * 1024 * 1024;
const STDERR_LIMIT: usize = 16 * 1024;

#[derive(Debug)]
pub enum HostError {
    Configuration(String),
    Start(String),
    Io(String),
    Timeout(String),
    Cancelled(String),
    Failed { command: String, status: String, message: String },
    Protocol(String),
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(message) | Self::Start(message) | Self::Io(message)
            | Self::Timeout(message) | Self::Cancelled(message) | Self::Protocol(message) => f.write_str(message),
            Self::Failed { command, status, message } => {
                write!(f, "Amity {command} failed ({status}): {message}")
            }
        }
    }
}

impl std::error::Error for HostError {}

/// A host command can emit more than one JSON record. In particular,
/// `execute` emits the gateway report and then the flight task report.
#[derive(Debug, Clone)]
pub struct HostResponse {
    pub records: Vec<Value>,
    pub raw: String,
}

impl HostResponse {
    pub fn last(&self) -> Result<&Value, HostError> {
        self.records.last().ok_or_else(|| HostError::Protocol("Amity returned no JSON record".into()))
    }

    pub fn first(&self) -> Result<&Value, HostError> {
        self.records.first().ok_or_else(|| HostError::Protocol("Amity returned no JSON record".into()))
    }
}

#[derive(Debug, Clone)]
pub struct HostClient {
    executable: PathBuf,
    config_path: PathBuf,
    timeout: Duration,
    max_output_bytes: usize,
}

/// A short-lived host config that binds AIP reads to the model actor already
/// selected by Amity's host config. Amity still enforces that actor's grants.
pub struct ModelScopedHost {
    client: HostClient,
    config_path: PathBuf,
}

impl ModelScopedHost {
    pub fn client(&self) -> &HostClient { &self.client }
}

impl Drop for ModelScopedHost {
    fn drop(&mut self) { let _ = fs::remove_file(&self.config_path); }
}

impl HostClient {
    pub fn new(executable: PathBuf, config_path: PathBuf, timeout: Duration) -> Result<Self, HostError> {
        if executable.as_os_str().is_empty() || config_path.as_os_str().is_empty() {
            return Err(HostError::Configuration("Amity executable and host configuration are required".into()));
        }
        if timeout.is_zero() {
            return Err(HostError::Configuration("Amity command timeout must be positive".into()));
        }
        Ok(Self { executable, config_path, timeout, max_output_bytes: DEFAULT_OUTPUT_LIMIT })
    }

    pub fn with_max_output_bytes(mut self, bytes: usize) -> Result<Self, HostError> {
        if bytes == 0 {
            return Err(HostError::Configuration("Amity output limit must be positive".into()));
        }
        self.max_output_bytes = bytes;
        Ok(self)
    }

    pub fn config_path(&self) -> &PathBuf { &self.config_path }

    pub fn model_scope(&self) -> Result<ModelScopedHost, HostError> {
        let bytes = fs::read(&self.config_path).map_err(|error| HostError::Configuration(format!(
            "Could not read Amity host configuration: {error}"
        )))?;
        let mut config: Value = serde_json::from_slice(&bytes).map_err(|error| HostError::Configuration(format!(
            "Invalid Amity host configuration: {error}"
        )))?;
        let model_actor = config.get("model_actor_id").and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| HostError::Configuration("model_actor_id is not configured in Amity".into()))?.to_owned();
        let fields = config.as_object_mut().ok_or_else(|| HostError::Configuration(
            "Amity host configuration must be a JSON object".into()
        ))?;
        fields.insert("actor_id".into(), Value::String(model_actor));
        fields.insert("actor_kind".into(), Value::String("model".into()));
        let directory = std::env::temp_dir().join("amity-echo-model-aip");
        fs::create_dir_all(&directory).map_err(|error| HostError::Configuration(format!(
            "Could not create model AIP runtime directory: {error}"
        )))?;
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos()).unwrap_or(0);
        let path = directory.join(format!("{}-{stamp}.json", std::process::id()));
        let mut file = OpenOptions::new().write(true).create_new(true).open(&path)
            .map_err(|error| HostError::Configuration(format!("Could not create model AIP binding: {error}")))?;
        let serialized = serde_json::to_vec(&config).map_err(|error| HostError::Configuration(format!(
            "Could not encode model AIP binding: {error}"
        )))?;
        if let Err(error) = file.write_all(&serialized) {
            let _ = fs::remove_file(&path);
            return Err(HostError::Configuration(format!("Could not write model AIP binding: {error}")));
        }
        drop(file);
        let mut client = Self::new(self.executable.clone(), path.clone(), self.timeout)?;
        client.max_output_bytes = self.max_output_bytes;
        Ok(ModelScopedHost { client, config_path: path })
    }

    pub fn call(&self, command: &str, args: &[&str], input: Option<&str>) -> Result<HostResponse, HostError> {
        self.call_cancelable(command, args, input, &AtomicBool::new(false))
    }

    pub fn call_cancelable(
        &self, command: &str, args: &[&str], input: Option<&str>, cancel: &AtomicBool,
    ) -> Result<HostResponse, HostError> {
        if command.is_empty() || command.contains('\0') || args.iter().any(|arg| arg.contains('\0')) {
            return Err(HostError::Configuration("Invalid Amity command argument".into()));
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(HostError::Cancelled(format!("Amity {command} was cancelled")));
        }
        let token = std::env::var("AMITY_HOST_TOKEN")
            .map_err(|_| HostError::Configuration("AMITY_HOST_TOKEN is required for the configured Amity host".into()))?;
        if token.is_empty() {
            return Err(HostError::Configuration("AMITY_HOST_TOKEN is empty".into()));
        }
        let mut process = Command::new(&self.executable);
        process.arg(command).arg(&self.config_path).args(args)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .env_clear().env("AMITY_HOST_TOKEN", &token);
        // Windows process creation and temporary files can require these
        // operating-system variables. Other inherited credentials are omitted.
        for name in ["SystemRoot", "WINDIR", "TEMP", "TMP", "PATH"] {
            if let Some(value) = std::env::var_os(name) { process.env(name, value); }
        }
        let mut child = process.spawn().map_err(|error| HostError::Start(format!(
            "Could not start Amity host '{}': {error}", self.executable.display()
        )))?;

        let stdin = child.stdin.take().ok_or_else(|| HostError::Io("Amity host stdin is unavailable".into()))?;
        let payload = input.unwrap_or("").as_bytes().to_vec();
        let (write_tx, write_rx) = mpsc::channel();
        thread::spawn(move || {
            let mut stdin = stdin;
            let result = stdin.write_all(&payload);
            drop(stdin);
            let _ = write_tx.send(result);
        });

        let stdout = child.stdout.take().ok_or_else(|| HostError::Io("Amity host stdout is unavailable".into()))?;
        let stderr = child.stderr.take().ok_or_else(|| HostError::Io("Amity host stderr is unavailable".into()))?;
        let (out_tx, out_rx) = mpsc::channel();
        let (err_tx, err_rx) = mpsc::channel();
        let output_limit = self.max_output_bytes;
        thread::spawn(move || { let _ = out_tx.send(read_capped(stdout, output_limit)); });
        thread::spawn(move || { let _ = err_tx.send(read_capped(stderr, STDERR_LIMIT)); });

        let status = wait_for_child(&mut child, self.timeout, cancel, command)?;
        let pipe_deadline = Instant::now() + Duration::from_secs(2);
        let (stdout, oversized) = receive_pipe(out_rx, pipe_deadline, "stdout")?
            .map_err(|error| HostError::Io(format!("Could not read Amity response: {error}")))?;
        let (stderr, _) = receive_pipe(err_rx, pipe_deadline, "stderr")?
            .map_err(|error| HostError::Io(format!("Could not read Amity error output: {error}")))?;
        if oversized {
            return Err(HostError::Protocol(format!("Amity {command} response exceeded the configured output limit")));
        }
        let stdout = String::from_utf8(stdout)
            .map_err(|_| HostError::Protocol(format!("Amity {command} returned non-UTF-8 output")))?;
        let stderr = String::from_utf8_lossy(&stderr);
        let safe_stderr = redact(&stderr, &token);
        if !status.success() {
            let message = safe_stderr.trim().strip_prefix("amity: ")
                .unwrap_or(safe_stderr.trim());
            return Err(HostError::Failed {
                command: command.into(), status: status.to_string(),
                message: if message.is_empty() { "host returned no error details".into() } else { message.into() },
            });
        }
        let written = receive_pipe(write_rx, pipe_deadline, "stdin")?;
        written.map_err(|error| HostError::Io(format!("Could not send request to Amity: {error}")))?;
        let raw = redact(&stdout, &token);
        let records: Vec<Value> = raw.lines().filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).map_err(|error| HostError::Protocol(format!(
                "Amity {command} returned invalid JSON: {error}"
            )))).collect::<Result<_, _>>()?;
        if records.is_empty() {
            return Err(HostError::Protocol(format!("Amity {command} returned no JSON response")));
        }
        Ok(HostResponse { records, raw })
    }

    fn one(&self, command: &str, args: &[&str], input: Option<&str>) -> Result<Value, HostError> {
        Ok(self.call(command, args, input)?.last()?.clone())
    }

    pub fn create_task(&self, instruction: &str) -> Result<Value, HostError> {
        self.one("task-create", &[], Some(instruction))
    }

    pub fn create_task_steps(&self, request: &str, steps: Value) -> Result<Value, HostError> {
        self.one("task-create-steps", &[], Some(&json!({"request": request, "steps": steps}).to_string()))
    }

    pub fn run_task(&self, task_id: &str, cancel: &AtomicBool) -> Result<Value, HostError> {
        Ok(self.call_cancelable("task-run", &[task_id], None, cancel)?.last()?.clone())
    }

    pub fn task_status(&self, task_id: &str) -> Result<Value, HostError> {
        self.one("task-status", &[task_id], None)
    }

    pub fn task_transcript(&self, task_id: &str) -> Result<Value, HostError> {
        self.one("task-transcript", &[task_id], None)
    }

    pub fn task_history(&self, task_id: &str) -> Result<Value, HostError> {
        self.one("task-history", &[task_id], None)
    }

    pub fn task_report(&self, task_id: &str) -> Result<Value, HostError> {
        self.one("task-report", &[task_id], None)
    }

    pub fn flight_events(&self) -> Result<Value, HostError> {
        self.one("flight-events", &[], None)
    }

    pub fn native_discover(&self) -> Result<Value, HostError> {
        self.one("native-discover", &[], None)
    }

    pub fn tool_docs(&self) -> Result<Value, HostError> {
        self.one("tool-docs", &[], None)
    }

    pub fn cancel_task(&self, task_id: &str) -> Result<Value, HostError> {
        self.one("task-cancel", &[task_id], None)
    }

    pub fn interrupt_step(&self, task_id: &str, step_id: &str) -> Result<Value, HostError> {
        self.one("task-interrupt", &[task_id, step_id], None)
    }

    pub fn retry_step(&self, task_id: &str, step_id: &str) -> Result<Value, HostError> {
        self.one("task-retry", &[task_id, step_id], None)
    }

    pub fn reconcile_step(&self, task_id: &str, step_id: &str) -> Result<Value, HostError> {
        self.one("task-reconcile", &[task_id, step_id], None)
    }

    pub fn refresh_context(&self, task_id: &str) -> Result<Value, HostError> {
        self.one("task-refresh-context", &[task_id], None)
    }

    pub fn approve_resource(&self, proposal_id: &str) -> Result<Value, HostError> {
        self.one("approve-resource", &[proposal_id], None)
    }

    pub fn policy_approve(&self, change_id: &str) -> Result<Value, HostError> {
        self.one("policy-approve", &[change_id], None)
    }

    pub fn policy_activate(&self, change_id: &str) -> Result<Value, HostError> {
        self.one("policy-activate", &[change_id], None)
    }

    pub fn execute(&self, task_id: &str, step_id: &str, approval_id: Option<&str>) -> Result<HostResponse, HostError> {
        let mut args = vec![task_id, step_id];
        if let Some(id) = approval_id { args.push(id); }
        self.call("execute", &args, None)
    }

    pub fn compiled_intent(&self, task_id: &str, instruction: &str) -> Result<Value, HostError> {
        self.one("intent", &[task_id], Some(instruction))
    }

    pub fn tool_read(&self, path: &str) -> Result<Value, HostError> {
        self.one("tool-read", &[path], None)
    }

    pub fn tool_search(&self, query: &str) -> Result<Value, HostError> {
        self.one("tool-search", &[query], None)
    }

    /// Forward one coding-agent action to Amity's governed tool gateway.
    /// Policy denials remain structured host replies for the agent to handle.
    pub fn agent_tool(&self, task_id: Option<&str>, action: Value) -> Result<Value, HostError> {
        let input = json!({"task_id": task_id, "action": action}).to_string();
        let response = self.call("agent-tool", &[], Some(&input))?;
        if response.records.len() != 1 {
            return Err(HostError::Protocol("Amity agent-tool must return exactly one JSON record".into()));
        }
        let record = response.records.into_iter().next()
            .expect("agent-tool record count was checked");
        if !record.get("ok").is_some_and(Value::is_boolean) {
            return Err(HostError::Protocol("Amity agent-tool reply has no boolean ok field".into()));
        }
        Ok(record)
    }

    pub fn aip(&self, request: Value) -> Result<Value, HostError> {
        let response = self.one("aip", &[], Some(&request.to_string()))?;
        if response.pointer("/result/type").and_then(Value::as_str) == Some("error") {
            let message = response.pointer("/result/message").and_then(Value::as_str).unwrap_or("AIP request was refused");
            let code = response.pointer("/result/code").and_then(Value::as_str).unwrap_or("unknown");
            return Err(HostError::Failed { command: "aip".into(), status: code.into(), message: message.into() });
        }
        Ok(response)
    }

    pub fn capabilities(&self) -> Result<Value, HostError> {
        self.aip(aip_request(json!({"type":"capabilities"})))
    }

    pub fn project_status(&self) -> Result<Value, HostError> {
        self.aip(aip_request(json!({"type":"status"})))
    }

    pub fn query_source(&self, query: &str, limit: u32) -> Result<Value, HostError> {
        self.aip(aip_request(json!({"type":"query_source","query":query,"limit":limit})))
    }

    pub fn context_manifest(&self, task_id: &str) -> Result<Value, HostError> {
        self.aip(aip_request(json!({"type":"get_context","task_id":task_id})))
    }

    pub fn proposal(&self, proposal_id: &str) -> Result<Value, HostError> {
        self.aip(aip_request(json!({"type":"get_proposal","proposal_id":proposal_id})))
    }
}

fn aip_request(body: Value) -> Value {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    json!({"schema":"amity.aip.v1","request_id":format!("echo:{}:{stamp}",std::process::id()),"request":body})
}

fn wait_for_child(child: &mut Child, timeout: Duration, cancel: &AtomicBool, command: &str)
    -> Result<std::process::ExitStatus, HostError> {
    let deadline = Instant::now() + timeout;
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(HostError::Cancelled(format!("Amity {command} was cancelled")));
        }
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(HostError::Timeout(format!("Amity {command} timed out after {} seconds", timeout.as_secs())));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(HostError::Io(format!("Could not wait for Amity {command}: {error}")));
            }
        }
    }
}

fn read_capped(mut reader: impl Read, limit: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut output = Vec::new();
    let mut exceeded = false;
    let mut chunk = [0u8; 8192];
    loop {
        let count = reader.read(&mut chunk)?;
        if count == 0 { break; }
        let remaining = limit.saturating_sub(output.len());
        output.extend_from_slice(&chunk[..remaining.min(count)]);
        if count > remaining { exceeded = true; }
    }
    Ok((output, exceeded))
}

fn receive_pipe<T>(receiver: mpsc::Receiver<T>, deadline: Instant, name: &str) -> Result<T, HostError> {
    receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| HostError::Io(format!("Amity {name} stream did not close")))
}

fn redact(text: &str, token: &str) -> String {
    let escaped = serde_json::to_string(token).unwrap_or_default();
    let escaped = escaped.strip_prefix('"').and_then(|value| value.strip_suffix('"')).unwrap_or("");
    let safe = if escaped.is_empty() { text.to_owned() } else { text.replace(escaped, "[redacted]") };
    safe.replace(token, "[redacted]")
}
