use crate::config::{Config, invalid};
use crate::attachments::AttachmentRef;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub project_id: String,
    #[serde(default)]
    pub working_directory: Option<PathBuf>,
    pub model: Option<String>,
    #[serde(default)]
    pub cadenza_model: Option<crate::inference::ModelSelection>,
    #[serde(default)]
    pub model_history: Vec<ModelChange>,
    pub created_at_ms: u128,
    pub updated_at_ms: u128,
    pub status: SessionStatus,
    pub messages: Vec<Message>,
    pub task_ids: Vec<String>,
    pub active_task: Option<String>,
    pub active_step: Option<String>,
    pub pending_approval: Option<String>,
    #[serde(default)]
    pub pending_policy_change: Option<String>,
    #[serde(default)]
    pub pending_command: Option<Value>,
    #[serde(default)]
    pub pending_capability: Option<Value>,
    #[serde(default)]
    pub pending_denial: Option<String>,
    #[serde(default)]
    pub pending_instruction: Option<String>,
    #[serde(default)]
    pub agent_task: bool,
    #[serde(default)]
    pub usage_input_tokens: u64,
    #[serde(default)]
    pub usage_output_tokens: u64,
    #[serde(default)]
    pub usage_cost_usd: f64,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub last_input_tokens: Option<u64>,
    #[serde(default)]
    pub usage_cost_reported: bool,
    #[serde(default)]
    pub context_summary: Option<String>,
    #[serde(default)]
    pub compacted_messages: usize,
    #[serde(default)]
    pub pending_attachments: Vec<AttachmentRef>,
    #[serde(skip)]
    pub pending_editor_context: Option<Value>,
    #[serde(default)]
    pub capability_state: Value,
}

impl Session {
    pub fn select_cadenza_model(&mut self, model: crate::inference::ModelSelection) -> bool {
        if self.cadenza_model.as_ref() == Some(&model) && self.model.as_deref() == Some(model.label().as_str())
            && self.context_window == model.context_window() { return false; }
        let deployment_changed = self.cadenza_model.as_ref()
            .is_none_or(|previous| previous.root != model.root || previous.identity != model.identity);
        if deployment_changed {
            if self.model_history.is_empty() {
                if let Some(previous)=&self.model { self.model_history.push(ModelChange {message_index:0,label:previous.clone(),root:self.cadenza_model.as_ref().map(|m|m.root.clone())}); }
            }
            self.model_history.push(ModelChange {message_index:self.messages.len(),label:model.label(),root:Some(model.root.clone())});
            self.last_input_tokens=None;
        }
        self.model=Some(model.label()); self.context_window=model.context_window();
        self.cadenza_model=Some(model); self.updated_at_ms=now_ms();
        true
    }
    pub fn append_message(&mut self, role: MessageRole, content: impl AsRef<str>) {
        self.messages.push(Message {
            role: role.clone(),
            content: redact_credentials(content.as_ref()),
            created_at_ms: now_ms(),
            attachments: Vec::new(),
            model_root: if role == MessageRole::Assistant { self.cadenza_model.as_ref().map(|m|m.root.clone()) } else {None},
        });
        self.updated_at_ms = now_ms();
    }

    pub fn link_task(&mut self, task_id: impl Into<String>) {
        let task_id = task_id.into();
        if !self.task_ids.contains(&task_id) {
            self.task_ids.push(task_id.clone());
        }
        self.active_task = Some(task_id);
        self.status = SessionStatus::Running;
        self.updated_at_ms = now_ms();
    }

    pub fn set_status(&mut self, status: SessionStatus) {
        self.status = status;
        self.updated_at_ms = now_ms();
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Idle,
    Running,
    WaitingApproval,
    AwaitingUserDecision,
    Interrupted,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub content: String,
    pub created_at_ms: u128,
    #[serde(default)]
    pub attachments: Vec<AttachmentRef>,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub model_root: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelChange {pub message_index:usize,pub label:String,pub root:Option<String>}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole { User, Assistant, Tool, System }

pub struct SessionStore {
    dir: PathBuf,
}

impl SessionStore {
    pub fn new(config: &Config) -> io::Result<Self> {
        fs::create_dir_all(&config.session_dir)?;
        Ok(Self { dir: config.session_dir.clone() })
    }

    pub fn create(&self, project_id: &str, model: Option<crate::inference::ModelSelection>) -> io::Result<Session> {
        if project_id.trim().is_empty() {
            return Err(invalid("session project ID cannot be empty"));
        }
        let now = now_ms();
        let cwd = std::env::current_dir()?;
        let working_directory = Some(cwd.canonicalize().unwrap_or(cwd));
        let session = Session {
            id: new_id(), project_id: project_id.to_owned(), working_directory, model:model.as_ref().map(|m|m.label()),
            context_window: model.as_ref().and_then(|model| model.context_window()),
            cadenza_model:model,model_history:Vec::new(),
            created_at_ms: now, updated_at_ms: now,
            status: SessionStatus::Idle, messages: Vec::new(), task_ids: Vec::new(),
            active_task: None, active_step: None, pending_approval: None,
            pending_policy_change: None, pending_command: None, pending_capability: None,
            pending_denial: None,
            pending_instruction: None, agent_task: false,
            usage_input_tokens: 0, usage_output_tokens: 0,
            usage_cost_usd: 0.0,
            last_input_tokens: None, usage_cost_reported: false,
            context_summary: None, compacted_messages: 0,
            pending_attachments: Vec::new(),
            pending_editor_context: None,
            capability_state: serde_json::json!({}),
        };
        self.save(&session)?;
        Ok(session)
    }

    pub fn load(&self, id: &str) -> io::Result<Session> {
        validate_id(id)?;
        let path = self.path_for(id);
        let bytes = fs::read(&path)?;
        let session: Session = serde_json::from_slice(&bytes)
            .map_err(|e| invalid(format!("invalid session {}: {e}", path.display())))?;
        if session.id != id {
            return Err(invalid(format!("session ID mismatch in {}", path.display())));
        }
        Ok(session)
    }

    pub fn save(&self, session: &Session) -> io::Result<()> {
        validate_id(&session.id)?;
        let mut safe = session.clone();
        if let Some(instruction) = &mut safe.pending_instruction {
            *instruction = redact_credentials(instruction);
        }
        for message in &mut safe.messages {
            message.content = redact_credentials(&message.content);
        }
        if let Some(summary) = &mut safe.context_summary {
            *summary = redact_credentials(summary);
        }
        if let Some(command) = &mut safe.pending_command { redact_value(command); }
        if let Some(request) = &mut safe.pending_capability { redact_value(request); }
        redact_value(&mut safe.capability_state);
        if let Some(change) = &mut safe.pending_policy_change { *change = redact_credentials(change); }
        write_json_atomic(&self.path_for(&session.id), &safe)
    }

    pub fn list(&self, project_id: Option<&str>) -> io::Result<Vec<Session>> {
        let mut sessions = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let path = entry?.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") { continue; }
            let bytes = fs::read(&path)?;
            let session: Session = serde_json::from_slice(&bytes)
                .map_err(|e| invalid(format!("invalid session {}: {e}", path.display())))?;
            if project_id.is_none_or(|id| session.project_id == id) {
                sessions.push(session);
            }
        }
        sessions.sort_by(|a, b| b.updated_at_ms.cmp(&a.updated_at_ms).then_with(|| b.id.cmp(&a.id)));
        Ok(sessions)
    }

    pub fn latest(&self, project_id: &str) -> io::Result<Option<Session>> {
        Ok(self.list(Some(project_id))?.into_iter().next())
    }

    fn path_for(&self, id: &str) -> PathBuf { self.dir.join(format!("{id}.json")) }
}

fn redact_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => *text = redact_credentials(text),
        serde_json::Value::Array(items) => items.iter_mut().for_each(redact_value),
        serde_json::Value::Object(fields) => {
            for (key, value) in fields {
                if ["password", "secret", "api_key", "access_token", "refresh_token",
                    "authorization", "private_key"].iter().any(|part| key.to_ascii_lowercase().contains(part)) {
                    *value = serde_json::Value::String("[redacted credential]".into());
                } else { redact_value(value); }
            }
        }
        _ => {}
    }
}

pub(crate) fn contains_credential(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(fields) => fields.iter().any(|(key, value)| {
            ["password", "secret", "api_key", "access_token", "refresh_token",
                "authorization", "private_key"].iter().any(|part| key.to_ascii_lowercase().contains(part))
                || contains_credential(value)
        }),
        serde_json::Value::Array(items) => items.iter().any(contains_credential),
        serde_json::Value::String(text) => ["sk-", "ghp_", "github_pat_", "glpat-", "xoxb-", "Bearer "]
            .iter().any(|prefix| text.contains(prefix)),
        _ => false,
    }
}

fn validate_id(id: &str) -> io::Result<()> {
    if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-') {
        return Err(invalid("invalid session ID"));
    }
    Ok(())
}

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()
}

fn new_id() -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:x}-{:x}-{sequence:x}", std::process::id())
}

pub(crate) fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| invalid("storage path has no parent"))?;
    fs::create_dir_all(parent)?;
    let data = serde_json::to_vec_pretty(value)
        .map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?;
    let suffix = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let name = path.file_name().ok_or_else(|| invalid("storage path has no file name"))?;
    let temp = parent.join(format!("{}.tmp-{}-{}-{suffix}", name.to_string_lossy(), std::process::id(), now_ms()));
    let write_result = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&temp)?;
        file.write_all(&data)?;
        file.sync_all()?;
        drop(file);
        replace_file_with_retry(&temp, path)
    })();
    if write_result.is_err() { let _ = fs::remove_file(&temp); }
    write_result
}

fn replace_file_with_retry(temp: &Path, target: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        for attempt in 0..5 {
            match replace_file(temp, target) {
                Err(error) if attempt < 4
                    && matches!(error.raw_os_error(), Some(32 | 33 | 1175)) => {
                    std::thread::sleep(std::time::Duration::from_millis(25 << attempt));
                }
                result => return result,
            }
        }
        unreachable!("the final replacement attempt always returns");
    }
    #[cfg(not(windows))]
    {
        replace_file(temp, target)
    }
}

#[cfg(not(windows))]
fn replace_file(temp: &Path, target: &Path) -> io::Result<()> {
    fs::rename(temp, target)
}

#[cfg(windows)]
fn replace_file(temp: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn ReplaceFileW(replaced: *const u16, replacement: *const u16, backup: *const u16,
            flags: u32, exclude: *const core::ffi::c_void, reserved: *const core::ffi::c_void) -> i32;
    }
    if !target.exists() { return fs::rename(temp, target); }
    let replaced: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    let replacement: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
    let result = unsafe {
        ReplaceFileW(replaced.as_ptr(), replacement.as_ptr(), std::ptr::null(), 0,
            std::ptr::null(), std::ptr::null())
    };
    if result == 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

pub(crate) fn redact_credentials(text: &str) -> String {
    let mut output = String::new();
    for line in text.split_inclusive('\n') {
        let lower = line.to_ascii_lowercase();
        let sensitive_assignment = ["amity_host_token=", "amity_host_token:", "cadenza_api_token=", "cadenza_api_token:", "authorization: bearer ",
            "api_key=", "api_key:", "api-key=", "api-key:", "access_token=", "access_token:",
            "refresh_token=", "refresh_token:", "client_secret=", "client_secret:",
            "password=", "password:"];
        if sensitive_assignment.iter().any(|needle| lower.contains(needle)) {
            output.push_str("[redacted credential]");
            if line.ends_with('\n') { output.push('\n'); }
            continue;
        }
        output.push_str(&redact_inline_tokens(line));
    }
    output
}

fn redact_inline_tokens(line: &str) -> String {
    let prefixes = ["sk-", "ghp_", "gho_", "github_pat_", "glpat-", "xoxb-", "xoxp-"];
    let mut output = String::new();
    let mut rest = line;
    while !rest.is_empty() {
        let next = prefixes.iter().filter_map(|prefix| rest.find(prefix).map(|index| (index, *prefix)))
            .min_by_key(|(index, _)| *index);
        let Some((index, prefix)) = next else { output.push_str(rest); break; };
        output.push_str(&rest[..index]);
        let tail = &rest[index + prefix.len()..];
        let token_len = tail.find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '`' | ',' | ';' | ')' | ']'))
            .unwrap_or(tail.len());
        if token_len >= 8 {
            output.push_str("[redacted credential]");
        } else {
            output.push_str(prefix);
            output.push_str(&tail[..token_len]);
        }
        rest = &tail[token_len..];
    }
    output
}

#[cfg(test)]
mod cadenza_tests {
    use super::*;
    #[test] fn historical_sessions_and_model_changes_preserve_messages(){
        let dir=tempfile::tempdir().unwrap();let mut config=Config::default();config.session_dir=dir.path().join("sessions");
        let store=SessionStore::new(&config).unwrap();let mut session=store.create("project",None).unwrap();
        session.model=Some("previous model".into());session.append_message(MessageRole::User,"original question");session.append_message(MessageRole::Assistant,"original answer");
        let before=serde_json::to_value(&session.messages).unwrap();
        let mut legacy=serde_json::to_value(&session).unwrap();legacy.as_object_mut().unwrap().remove("cadenza_model");legacy.as_object_mut().unwrap().remove("model_history");
        let mut restored:Session=serde_json::from_value(legacy).unwrap();
        let selected=crate::cadenza::ModelSelection::from_discovery(serde_json::json!({"identity":{"id":"new.family","version":"2"},"root":"a".repeat(64),"ready":true,"max_context":2048})).unwrap();
        restored.select_cadenza_model(selected.clone());store.save(&restored).unwrap();
        let loaded=store.load(&session.id).unwrap();assert_eq!(serde_json::to_value(&loaded.messages).unwrap(),before);
        assert_eq!(loaded.cadenza_model.unwrap(),selected);assert_eq!(loaded.model_history[0].label,"previous model");assert_eq!(loaded.model_history[1].message_index,2);
        restored.append_message(MessageRole::Assistant,"new output");assert_eq!(restored.messages[2].model_root.as_deref(),Some("a".repeat(64).as_str()));
    }
    #[test] fn cadenza_credential_is_redacted(){
        assert_eq!(redact_credentials("CADENZA_API_TOKEN=private-value"),"[redacted credential]");
    }
    #[test]
    fn global_selection_updates_reopened_conversation_without_rewriting_history() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::default(); config.session_dir = dir.path().join("sessions");
        let store = SessionStore::new(&config).unwrap();
        let original = crate::cadenza::ModelSelection::from_discovery(serde_json::json!({
            "identity":{"id":"example.model","version":"1"},"root":"a".repeat(64),"ready":true,"max_context":128
        })).unwrap();
        let mut session = store.create("project", Some(original.clone())).unwrap();
        session.append_message(MessageRole::Assistant, "Original answer"); store.save(&session).unwrap();
        let historical = serde_json::to_value(&session.messages).unwrap();
        let mut replacement = original; replacement.root = "b".repeat(64);
        replacement.metadata["root"] = serde_json::json!(replacement.root);
        replacement.metadata["max_context"] = serde_json::json!(2048);
        config.cadenza.default_model = Some(replacement.clone());
        assert_eq!(crate::inference::preferred_model(&config, &session), Some(&replacement));
        crate::inference::sync_session(&config, &store, &mut session).unwrap();
        assert_eq!(serde_json::to_value(&session.messages).unwrap(), historical);
        assert_eq!(session.context_window, Some(2048));
        assert_eq!(session.model_history.len(), 2);
        crate::inference::sync_session(&config, &store, &mut session).unwrap();
        assert_eq!(session.model_history.len(), 2);
        session.append_message(MessageRole::Assistant, "New answer"); store.save(&session).unwrap();
        let loaded = store.load(&session.id).unwrap();
        assert_eq!(loaded.messages[0].model_root.as_deref(), Some("a".repeat(64).as_str()));
        assert_eq!(loaded.messages[1].model_root.as_deref(), Some("b".repeat(64).as_str()));
        assert_eq!(loaded.cadenza_model, config.cadenza.default_model);
    }
}
