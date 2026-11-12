use serde::{Deserialize, Serialize};
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
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectRegistration {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub host_config: PathBuf,
    #[serde(default)]
    pub models: Vec<ModelProfile>,
    #[serde(default)]
    pub selected_model: Option<String>,
}

impl ProjectRegistration {
    pub fn effective_host_config(&self) -> io::Result<&Path> {
        let path = match self.selected_model.as_deref() {
            Some(name) => self.models.iter().find(|model| model.name == name)
                .map(|model| model.host_config.as_path())
                .ok_or_else(|| invalid(format!("model profile '{name}' is not configured for {}", self.name))),
            None => Ok(self.host_config.as_path()),
        }?;
        canonical_host_config(path, &self.id, &self.path)?;
        Ok(path)
    }

    pub fn select_model(&mut self, name: Option<&str>) -> io::Result<()> {
        if let Some(name) = name {
            if !self.models.iter().any(|model| model.name == name) {
                return Err(invalid(format!("model profile '{name}' is not configured for {}", self.name)));
            }
        }
        self.selected_model = name.map(str::to_owned);
        Ok(())
    }

    pub fn add_model(&mut self, name: &str, host_config: impl AsRef<Path>) -> io::Result<()> {
        if name.trim().is_empty() {
            return Err(invalid("model profile name cannot be empty"));
        }
        let host_config = canonical_host_config(host_config.as_ref(), &self.id, &self.path)?;
        if let Some(existing) = self.models.iter_mut().find(|model| model.name == name) {
            existing.host_config = host_config;
        } else {
            self.models.push(ModelProfile { name: name.to_owned(), host_config,
                supports_images: false });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelProfile {
    pub name: String,
    pub host_config: PathBuf,
    #[serde(default)]
    pub supports_images: bool,
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

    pub fn load() -> io::Result<Self> {
        let path = Self::path();
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| invalid(format!("{}: {e}", path.display()))),
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
            .filter(|project| cwd.starts_with(&project.path))
            .max_by_key(|project| project.path.components().count())
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
            existing.host_config = config_path;
            existing.name = name;
        } else {
            self.projects.push(ProjectRegistration {
                id: id.clone(), name, path, host_config: config_path,
                models: Vec::new(), selected_model: None,
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
