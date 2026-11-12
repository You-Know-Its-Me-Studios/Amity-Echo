use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub host_executable: PathBuf,
    pub session_dir: PathBuf,
    pub projects: Vec<ProjectRegistration>,
    pub active_project: Option<String>,
    pub terminal: TerminalSettings,
    pub execution: ExecutionSettings,
    pub resources: ResourceSettings,
    #[serde(default)]
    pub capabilities: serde_json::Value,
    pub cadenza: CadenzaSettings,
}

impl Default for Config {
    fn default() -> Self {
        let home = Self::home_dir();
        Self {
            host_executable: PathBuf::from("amity"),
            session_dir: home.join("sessions"),
            projects: Vec::new(),
            active_project: None,
            terminal: TerminalSettings::default(),
            execution: ExecutionSettings::default(),
            resources: ResourceSettings::default(),
            capabilities: serde_json::json!({}),
            cadenza: CadenzaSettings::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectRegistration {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    #[serde(default)]
    pub host_config: Option<PathBuf>,
    #[serde(default)]
    pub token_file: Option<PathBuf>,
}

impl ProjectRegistration {
    pub fn effective_host_config(&self) -> io::Result<&Path> {
        let base = self.host_config.as_deref().ok_or_else(|| invalid(format!(
            "Amity is not initialized for this workspace ({}). Run /init here to enable governed coding operations",
            self.path.display()
        )))?;
        canonical_host_config(base, &self.id, &self.path)?;
        Ok(base)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct CadenzaSettings {
    pub endpoint: String,
    pub credential_ref: String,
    pub default_model: Option<crate::cadenza::ModelSelection>,
    pub connect_timeout_seconds: u64,
    pub request_timeout_seconds: u64,
    pub max_tokens: u16,
}
impl Default for CadenzaSettings {
    fn default() -> Self {
        Self {endpoint:"ws://127.0.0.1:7941/cadenza".into(),credential_ref:"env:CADENZA_API_TOKEN".into(),default_model:None,
            connect_timeout_seconds:5,request_timeout_seconds:120,max_tokens:32}
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalSettings {
    pub color: bool,
    pub show_tool_details: bool,
}

impl Default for TerminalSettings {
    fn default() -> Self { Self { color: true, show_tool_details: false } }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ExecutionSettings {
    pub ask_before_execution: bool,
    pub auto_continue: bool,
}

impl Default for ExecutionSettings {
    fn default() -> Self { Self { ask_before_execution: true, auto_continue: true } }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ResourceSettings {
    pub host_timeout_seconds: u64,
    pub max_agent_turns: usize,
    pub max_history_messages: usize,
}

impl Default for ResourceSettings {
    fn default() -> Self {
        Self { host_timeout_seconds: 1200, max_agent_turns: 32, max_history_messages: 40 }
    }
}

#[derive(Deserialize)]
struct HostBinding {
    project: String,
    project_root: PathBuf,
}

impl Config {
    pub fn home_dir() -> PathBuf {
        if let Some(path) = std::env::var_os("AMITY_ECHO_HOME") {
            return PathBuf::from(path);
        }
        if let Some(path) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(path).join("Amity").join("Echo");
        }
        if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
            return PathBuf::from(path).join("amity-echo");
        }
        if let Some(path) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
            return PathBuf::from(path).join(".amity-echo");
        }
        PathBuf::from(".amity-echo")
    }

    pub fn path() -> PathBuf { Self::home_dir().join("config.json") }

    pub fn local_token_path(project_id: &str) -> PathBuf {
        Self::home_dir().join("secrets").join(format!("{project_id}.token"))
    }

    pub fn load() -> io::Result<Self> {
        let path = Self::path();
        match fs::read(&path) {
            Ok(bytes) => {
                let mut value: serde_json::Value=serde_json::from_slice(&bytes).map_err(|e|invalid(format!("{}: {e}",path.display())))?;
                let legacy=value.get("projects").and_then(serde_json::Value::as_array).is_some_and(|items|items.iter().any(|p|p.get("models").is_some() || p.get("selected_model").is_some()));
                if let Some(projects)=value.get_mut("projects").and_then(serde_json::Value::as_array_mut) {
                    for project in projects {
                        let selected=project.get("selected_model").and_then(serde_json::Value::as_str);
                        let host=project.get("models").and_then(serde_json::Value::as_array).and_then(|models|models.iter().find(|m|m.get("name").and_then(serde_json::Value::as_str)==selected && selected.is_some())).and_then(|m|m.get("host_config")).cloned();
                        if let Some(host)=host {project["host_config"]=host;}
                    }
                }
                let config:Self=serde_json::from_value(value).map_err(|e|invalid(format!("{}: {e}",path.display())))?;
                if legacy {
                    let backup=Self::home_dir().join("config.pre-cadenza.json");
                    match fs::OpenOptions::new().write(true).create_new(true).open(backup) {
                        Ok(mut file)=>{use std::io::Write;file.write_all(&bytes)?;file.sync_all()?;},
                        Err(e) if e.kind()==ErrorKind::AlreadyExists=>{},
                        Err(e)=>return Err(e),
                    }
                    config.save()?;
                }
                Ok(config)
            },
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    pub fn save(&self) -> io::Result<()> {
        super::state::write_json_atomic(&Self::path(), self)
    }

    pub fn active_project(&self) -> Option<&ProjectRegistration> {
        let id = self.active_project.as_deref()?;
        self.projects.iter().find(|project| project.id == id)
    }

    pub fn active_project_mut(&mut self) -> Option<&mut ProjectRegistration> {
        let id = self.active_project.as_deref()?;
        self.projects.iter_mut().find(|project| project.id == id)
    }

    pub fn project(&self, id_or_path: &str) -> Option<&ProjectRegistration> {
        self.projects.iter().find(|project| project.id == id_or_path
            || project.name == id_or_path
            || project.path.to_string_lossy().eq_ignore_ascii_case(id_or_path))
    }

    pub fn select_project(&mut self, id_or_path: &str) -> io::Result<&ProjectRegistration> {
        let id = self.project(id_or_path)
            .ok_or_else(|| invalid(format!("project '{id_or_path}' is not registered")))?.id.clone();
        self.active_project = Some(id.clone());
        Ok(self.projects.iter().find(|project| project.id == id).expect("selected project exists"))
    }

    pub fn discover_project(&self, cwd: &Path) -> Option<&ProjectRegistration> {
        let cwd = cwd.canonicalize().ok()?;
        self.projects.iter()
            .filter(|project| project.path == cwd)
            .max_by_key(|project| project.host_config.is_some())
    }

    /// The terminal's working directory is an Echo project even before an
    /// Amity host has been configured for governed operations.
    pub fn select_working_directory(&mut self, cwd: &Path) -> io::Result<&ProjectRegistration> {
        let path = canonical_dir(cwd)?;
        if let Some(id) = self.discover_project(&path).map(|project| project.id.clone()) {
            self.active_project = Some(id.clone());
            return Ok(self.projects.iter().find(|project| project.id == id)
                .expect("discovered project exists"));
        }
        let key = if cfg!(windows) {
            path.to_string_lossy().to_lowercase()
        } else {
            path.to_string_lossy().into_owned()
        };
        let digest = format!("{:x}", Sha256::digest(key.as_bytes()));
        let id = format!("workspace-{}", &digest[..32]);
        if self.projects.iter().any(|project| project.id == id && project.path != path) {
            return Err(invalid("workspace identity collision"));
        }
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        self.projects.push(ProjectRegistration {
            id: id.clone(), name, path, host_config: None,
            token_file: None,
        });
        self.active_project = Some(id.clone());
        Ok(self.projects.last().expect("workspace was registered"))
    }

    pub fn register_project(&mut self, path: impl AsRef<Path>, host_config: impl AsRef<Path>) -> io::Result<&ProjectRegistration> {
        let path = canonical_dir(path.as_ref())?;
        let config_path = host_config.as_ref().canonicalize()?;
        let binding = read_host_binding(&config_path)?;
        let bound_path = canonical_dir(&binding.project_root)?;
        if path != bound_path {
            return Err(invalid(format!("host config project_root {} differs from {}", bound_path.display(), path.display())));
        }
        if binding.project.trim().is_empty() {
            return Err(invalid("host config project ID is empty"));
        }
        if self.projects.iter().any(|project| project.id == binding.project && project.path != path) {
            return Err(invalid(format!("project ID '{}' is already registered at another path", binding.project)));
        }
        let id = binding.project;
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| id.clone());
        if let Some(existing) = self.projects.iter_mut().find(|project| project.id == id) {
            existing.host_config = Some(config_path);
            existing.name = name;
        } else {
            self.projects.push(ProjectRegistration {
                id: id.clone(), name, path, host_config: Some(config_path),
                token_file: None,
            });
        }
        self.active_project = Some(id.clone());
        Ok(self.projects.iter().find(|project| project.id == id).expect("registered project exists"))
    }

    /// Register the project already bound by an existing Amity host config.
    /// The host, rather than Echo's current working directory, supplies the root.
    pub fn register_host_config(&mut self, host_config: impl AsRef<Path>) -> io::Result<&ProjectRegistration> {
        let host_config = host_config.as_ref().canonicalize()?;
        let binding = read_host_binding(&host_config)?;
        self.register_project(binding.project_root, host_config)
    }
}

fn canonical_dir(path: &Path) -> io::Result<PathBuf> {
    let path = path.canonicalize()?;
    if !path.is_dir() {
        return Err(invalid(format!("{} is not a directory", path.display())));
    }
    Ok(path)
}

fn canonical_host_config(path: &Path, expected_id: &str, expected_root: &Path) -> io::Result<PathBuf> {
    let path = path.canonicalize()?;
    let binding = read_host_binding(&path)?;
    if binding.project != expected_id || canonical_dir(&binding.project_root)? != expected_root {
        return Err(invalid(format!("model host config {} belongs to a different project", path.display())));
    }
    Ok(path)
}

fn read_host_binding(path: &Path) -> io::Result<HostBinding> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|e| invalid(format!("invalid Amity host config {}: {e}", path.display())))
}

pub(crate) fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn old_project_settings_retire_without_losing_registration(){
        let config:Config=serde_json::from_value(serde_json::json!({"projects":[{"id":"p","name":"project","path":"C:/work","host_config":"C:/binding.json","models":[{"name":"previous","host_config":"C:/old.json"}],"selected_model":"previous"}]})).unwrap();
        let saved=serde_json::to_value(&config).unwrap();assert_eq!(saved["projects"][0]["id"],"p");assert!(saved["projects"][0].get("models").is_none());assert!(saved["projects"][0].get("selected_model").is_none());assert_eq!(config.cadenza.endpoint,"ws://127.0.0.1:7941/cadenza");
    }
    #[test] fn global_model_configuration_is_independent_of_projects(){
        let mut config=Config::default();config.cadenza.endpoint="wss://inference.example.com/cadenza".into();config.cadenza.credential_ref="windows:Amity/Echo/Cadenza/default".into();
        let saved=serde_json::to_vec(&config).unwrap();let restored:Config=serde_json::from_slice(&saved).unwrap();assert_eq!(restored.cadenza.endpoint,config.cadenza.endpoint);assert_eq!(restored.cadenza.credential_ref,config.cadenza.credential_ref);assert!(restored.projects.is_empty());
    }
}
