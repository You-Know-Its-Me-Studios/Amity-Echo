//! Human-invoked initialization of the current directory's Amity workspace.
//! Model tool requests cannot reach this entry point.

use crate::config::{Config, ProjectRegistration};
use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn initialize(config: &mut Config, project: &ProjectRegistration) -> Result<bool> {
    if project.host_config.is_some() { return Ok(false); }
    let host = find_host(config)?;
    let token_path = Config::local_token_path(&project.id);
    let (token, created) = local_token(&token_path)?;
    let result = if project.path.join(".amity").exists() {
        validate_existing(&project.path, &project.id, &token)
    } else {
        initialize_host(&host, &project.path, &project.id, &token)
    };
    if let Err(error) = result {
        if created { let _ = fs::remove_file(&token_path); }
        return Err(error);
    }
    let binding = project.path.join(".amity").join("host.json");
    let mut next = config.clone();
    next.register_project(&project.path, &binding)?;
    next.active_project_mut().context("initialized workspace is missing")?
        .token_file = Some(token_path);
    next.host_executable = host;
    next.save().context("saving initialized Amity connection")?;
    *config = next;
    Ok(true)
}

fn find_host(config: &Config) -> Result<PathBuf> {
    if config.host_executable != Path::new("amity") {
        return Ok(config.host_executable.clone());
    }
    let installed = Config::home_dir().join("bin")
        .join(if cfg!(windows) { "amity.exe" } else { "amity" });
    if installed.is_file() { return Ok(installed); }
    if let Some(parent) = std::env::current_exe()?.parent() {
        let beside_echo = parent.join(if cfg!(windows) { "amity.exe" } else { "amity" });
        if beside_echo.is_file() { return Ok(beside_echo); }
    }
    if cfg!(windows) { return crate::update::ensure_host_helper(); }
    if let Some(paths) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&paths) {
            let candidate = directory.join(if cfg!(windows) { "amity.exe" } else { "amity" });
            if candidate.is_file() { return Ok(candidate); }
        }
    }
    Ok(PathBuf::from("amity"))
}

fn local_token(path: &Path) -> Result<(String, bool)> {
    match fs::read_to_string(path) {
        Ok(token) => {
            let token = token.trim().to_owned();
            ensure!(token.len() >= 32, "stored Amity credential is invalid");
            return Ok((token, false));
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {},
        Err(error) => return Err(error).context("reading local Amity credential"),
    }
    fs::create_dir_all(path.parent().context("credential directory is missing")?)
        .context("creating local Amity credential directory")?;
    let mut random = [0u8; 32];
    getrandom::fill(&mut random).context("generating Amity workspace credential")?;
    let token = random.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            return local_token(path);
        }
        Err(error) => return Err(error).context("creating local Amity credential"),
    };
    if let Err(error) = file.write_all(token.as_bytes()).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(error).context("saving local Amity credential");
    }
    Ok((token, true))
}

fn initialize_host(executable: &Path, root: &Path, project_id: &str, token: &str) -> Result<()> {
    let mut command = Command::new(executable);
    command.arg("workspace-init").arg(root).arg(project_id)
        .env_clear().env("AMITY_HOST_TOKEN", token);
    for name in ["SystemRoot", "WINDIR", "TEMP", "TMP", "PATH",
        "HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA"] {
        if let Some(value) = std::env::var_os(name) { command.env(name, value); }
    }
    let output = command.output().with_context(|| format!(
        "starting Amity for /init from {}; install Amity or set its location in /settings",
        executable.display()))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = detail.trim().strip_prefix("amity: ").unwrap_or(detail.trim());
        bail!("Amity could not initialize this workspace: {}", detail.chars().take(4096).collect::<String>());
    }
    Ok(())
}

fn validate_existing(root: &Path, project_id: &str, token: &str) -> Result<()> {
    let state = root.join(".amity");
    let metadata = fs::symlink_metadata(&state)?;
    ensure!(metadata.is_dir() && !metadata.file_type().is_symlink(),
        "existing .amity is not a regular directory");
    let state_canonical = state.canonicalize()?;
    let binding = state.join("host.json");
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&binding)
        .with_context(|| format!(".amity already exists but {} is missing", binding.display()))?)
        .context("reading existing Amity workspace")?;
    ensure!(value.get("project").and_then(serde_json::Value::as_str) == Some(project_id),
        "existing .amity belongs to a different project");
    let bound_root = value.get("project_root").and_then(serde_json::Value::as_str)
        .context("existing Amity workspace has no project root")?;
    ensure!(Path::new(bound_root).canonicalize()? == root.canonicalize()?,
        "existing .amity belongs to a different directory");
    let digest = format!("sha256:{:x}", Sha256::digest(token.as_bytes()));
    ensure!(value.get("token_sha256").and_then(serde_json::Value::as_str) == Some(digest.as_str()),
        "existing .amity does not match this user's local credential");
    for key in ["registry", "contract_store", "authority", "context", "gateway_journal",
        "verification", "intelligence_index", "task_db", "change_db", "flight_db",
        "workspace_base"] {
        let location = value.get(key).and_then(serde_json::Value::as_str)
            .with_context(|| format!("existing Amity workspace has no {key}"))?;
        ensure!(Path::new(location).starts_with(&state)
            && !Path::new(location).components().any(|component|
                matches!(component, std::path::Component::ParentDir | std::path::Component::CurDir)),
            "existing Amity workspace {key} points outside .amity");
        ensure!(Path::new(location).canonicalize()?.starts_with(&state_canonical),
            "existing Amity workspace {key} resolves outside .amity");
    }
    if let Some(location) = value.get("policy_change_db").and_then(serde_json::Value::as_str) {
        ensure!(Path::new(location).starts_with(&state)
            && !Path::new(location).components().any(|component|
                matches!(component, std::path::Component::ParentDir | std::path::Component::CurDir)),
            "existing Amity policy store points outside .amity");
    }
    Ok(())
}
