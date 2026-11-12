//! Ephemeral, isolated Solution Arena candidates. Baselines come only from
//! Amity-authorized reads; candidates never write into the connected project.
use crate::config::Config;
use crate::host::HostClient;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_FILES: usize = 20;
const MAX_FILE_BYTES: usize = 256 * 1024;
const MAX_TOTAL_BYTES: usize = 2 * 1024 * 1024;
static NEXT_WORKSPACE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
struct Baseline {
    text: String,
    digest: String,
}

/// One set of comparable candidate sandboxes. Dropping it removes the local
/// copies unless the user explicitly requested retention through the caller.
pub struct CandidateWorkspaces {
    root: PathBuf,
    base: PathBuf,
    project_root: PathBuf,
    source_revision: Option<String>,
    policy_fingerprint: String,
    baseline: BTreeMap<String, Baseline>,
    candidates: usize,
    retained: bool,
}

impl CandidateWorkspaces {
    /// `files` are exact existing project-relative files selected for the
    /// comparison. The Amity gateway decides whether each read is authorized.
    pub fn create(host: &HostClient, project_root: &Path, files: &[String],
        count: usize, cancel: &AtomicBool) -> Result<Self> {
        if !(2..=3).contains(&count) { bail!("candidate count must be two or three"); }
        if files.is_empty() || files.len() > MAX_FILES {
            bail!("select between one and {MAX_FILES} existing files for candidate workspaces");
        }
        let project_root = project_root.canonicalize()
            .context("resolving authorized project root")?;
        if !project_root.is_dir() { bail!("authorized project root is not a directory"); }
        let home = Config::home_dir().canonicalize()
            .context("resolving Echo's local storage directory")?;
        let base = home.join("capabilities").join("candidate-workspaces");
        if home.starts_with(&project_root) || base.starts_with(&project_root)
            || project_root.starts_with(&base) {
            bail!("candidate storage overlaps the authorized project");
        }
        fs::create_dir_all(&base).context("creating local candidate storage")?;
        let base = base.canonicalize().context("resolving local candidate storage")?;
        if base.starts_with(&project_root) || project_root.starts_with(&base) {
            bail!("resolved candidate storage overlaps the authorized project");
        }
        let mut unique = BTreeSet::new();
        let mut baseline = BTreeMap::new();
        let mut total_bytes = 0usize;
        let mut source_revision: Option<String> = None;
        let policy_fingerprint = active_policy_fingerprint(host, cancel)?;
        for path in files {
            if cancel.load(Ordering::Relaxed) { bail!("candidate workspace creation cancelled"); }
            let relative = safe_relative(path)?;
            if !unique.insert(relative.clone()) { bail!("candidate files must be unique"); }
            let reply = host.agent_tool_cancelable(None,
                json!({"type":"read","path":relative.as_str()}), cancel)?;
            if reply.get("ok").and_then(Value::as_bool) != Some(true) {
                let reason = reply.pointer("/error/message").and_then(Value::as_str)
                    .unwrap_or("Amity refused the source read");
                bail!("Amity could not authorize candidate source {relative}: {reason}");
            }
            let result = reply.get("result").context("Amity read returned no result")?;
            let text = result.get("text").and_then(Value::as_str)
                .context("Amity read returned no text")?;
            if text.len() > MAX_FILE_BYTES { bail!("candidate source {relative} exceeds {MAX_FILE_BYTES} bytes"); }
            total_bytes = total_bytes.saturating_add(text.len());
            if total_bytes > MAX_TOTAL_BYTES { bail!("candidate source set exceeds {MAX_TOTAL_BYTES} bytes"); }
            let digest = result.get("digest").and_then(Value::as_str)
                .context("Amity read returned no source digest")?;
            let actual = sha256(text);
            if digest != actual { bail!("Amity source digest mismatch for {relative}"); }
            let revision = result.pointer("/head/fingerprint").and_then(Value::as_str);
            if let (Some(previous), Some(current)) = (source_revision.as_deref(), revision) {
                if previous != current { bail!("project revision changed while preparing candidate workspaces"); }
            }
            if source_revision.is_none() { source_revision = revision.map(str::to_owned); }
            baseline.insert(relative, Baseline { text: text.to_owned(), digest: digest.to_owned() });
        }
        if active_policy_fingerprint(host, cancel)? != policy_fingerprint {
            bail!("active Amity policy changed while preparing candidate workspaces");
        }
        let root = create_unique_root(&base)?;
        let created = (|| -> Result<()> {
            for index in 0..count {
                let candidate_root = root.join(format!("candidate-{}", index + 1));
                fs::create_dir(&candidate_root)?;
                for (relative, source) in &baseline {
                    let target = candidate_root.join(relative);
                    write_new_bounded(&candidate_root, &target, source.text.as_bytes())?;
                }
            }
            Ok(())
        })();
        if let Err(error) = created {
            let _ = fs::remove_dir_all(&root);
            return Err(error).context("creating isolated candidate copies");
        }
        Ok(Self { root, base, project_root, source_revision, policy_fingerprint,
            baseline, candidates: count,
            retained: false })
    }

    /// Materialize a candidate's full-file proposals in its own sandbox only.
    /// `changes` is an array of `{path,content}` objects; paths must be among
    /// the explicitly authorized baseline files.
    pub fn apply_changes(&self, index: usize, changes: &Value) -> Result<Value> {
        if index >= self.candidates { bail!("candidate index is out of range"); }
        let changes = changes.as_array().context("candidate files must be an array")?;
        if changes.len() > MAX_FILES { bail!("candidate proposed too many files"); }
        let candidate_root = self.root.join(format!("candidate-{}", index + 1));
        let candidate_root = candidate_root.canonicalize()
            .context("candidate workspace is unavailable")?;
        if !candidate_root.starts_with(&self.root) || candidate_root.starts_with(&self.project_root) {
            bail!("candidate workspace escaped its local storage boundary");
        }
        let mut seen = BTreeSet::new();
        let mut total_bytes = 0usize;
        let mut diffs = Vec::new();
        for change in changes {
            let path = change.get("path").and_then(Value::as_str)
                .context("candidate change requires a path")?;
            let path = safe_relative(path)?;
            if !seen.insert(path.clone()) { bail!("candidate proposed the same file twice"); }
            let source = self.baseline.get(&path)
                .with_context(|| format!("candidate file {path} was not authorized for comparison"))?;
            let content = change.get("content").and_then(Value::as_str)
                .context("candidate change requires complete UTF-8 file content")?;
            if content.len() > MAX_FILE_BYTES { bail!("candidate file {path} exceeds {MAX_FILE_BYTES} bytes"); }
            total_bytes = total_bytes.saturating_add(content.len());
            if total_bytes > MAX_TOTAL_BYTES { bail!("candidate changes exceed {MAX_TOTAL_BYTES} bytes"); }
            let target = candidate_root.join(&path);
            let metadata = fs::symlink_metadata(&target)
                .with_context(|| format!("candidate baseline {path} is missing"))?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                bail!("candidate baseline {path} is not a regular file");
            }
            let resolved = target.canonicalize()?;
            if !resolved.starts_with(&candidate_root) { bail!("candidate file escaped its workspace"); }
            let mut file = OpenOptions::new().write(true).truncate(true).open(&resolved)?;
            file.write_all(content.as_bytes())?;
            file.sync_all()?;
            diffs.push(json!({"path":path,"base_digest":source.digest.as_str(),
                "candidate_digest":sha256(content),"changed_lines":changed_lines(&source.text, content),
                "base_bytes":source.text.len(),"candidate_bytes":content.len(),
                "changed":source.text != content}));
        }
        Ok(json!({"candidate":index + 1,"workspace_id":self.workspace_id(),
            "source_revision":self.source_revision.as_deref(),
            "policy_fingerprint":self.policy_fingerprint.as_str(),"files":diffs,
            "production_mutated":false,"runtime_verified":false}))
    }

    pub fn manifest(&self) -> Value {
        let source_bytes: usize = self.baseline.values().map(|source| source.text.len()).sum();
        json!({"workspace_id":self.workspace_id(),"workspace_mode":"isolated_local_copies",
            "candidate_count":self.candidates,"source_file_count":self.baseline.len(),
            "temporary_storage_bytes_estimate":source_bytes.saturating_mul(self.candidates),
            "source_revision":self.source_revision.as_deref(),
            "policy_fingerprint":self.policy_fingerprint.as_str(),
            "cleanup":"automatic_on_drop",
            "production_mutated":false,"runtime_verified":false})
    }

    /// Retention is an explicit caller decision after user approval. The
    /// returned path remains local and may contain authorized project source.
    pub fn retain(&mut self) -> PathBuf {
        self.retained = true;
        self.root.clone()
    }

    /// Remove an explicitly retained workspace after review. It refuses any
    /// path that no longer resolves beneath the original Echo storage root.
    pub fn cleanup(&mut self) -> Result<()> {
        if !self.root.exists() { self.retained = false; return Ok(()); }
        let root = self.root.canonicalize()?;
        let base = self.base.canonicalize()?;
        if root == base || !root.starts_with(&base) || root.starts_with(&self.project_root) {
            bail!("candidate cleanup target is outside Echo's workspace boundary");
        }
        fs::remove_dir_all(&root).context("removing candidate workspaces")?;
        self.retained = false;
        Ok(())
    }

    fn workspace_id(&self) -> String {
        self.root.file_name().map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown".into())
    }
}

impl Drop for CandidateWorkspaces {
    fn drop(&mut self) {
        if self.retained { return; }
        let Ok(root) = self.root.canonicalize() else { return; };
        let Ok(base) = self.base.canonicalize() else { return; };
        if root.starts_with(&base) && root != base && !root.starts_with(&self.project_root) {
            let _ = fs::remove_dir_all(root);
        }
    }
}

fn create_unique_root(base: &Path) -> Result<PathBuf> {
    for _ in 0..16 {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)
            .unwrap_or_default().as_nanos();
        let sequence = NEXT_WORKSPACE.fetch_add(1, Ordering::Relaxed);
        let name = format!("arena-{:x}-{:x}-{sequence:x}", std::process::id(), stamp);
        let root = base.join(name);
        match fs::create_dir(&root) {
            Ok(()) => return root.canonicalize().map_err(Into::into),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).context("creating local candidate workspace"),
        }
    }
    bail!("could not allocate a unique local candidate workspace")
}

fn safe_relative(raw: &str) -> Result<String> {
    if raw.is_empty() || raw.len() > 400 || raw.contains('\\') || raw.contains(':')
        || raw.chars().any(char::is_control) {
        bail!("candidate paths must be bounded project-relative paths using forward slashes");
    }
    let path = Path::new(raw);
    let mut parts = Vec::new();
    for component in path.components() {
        let Component::Normal(name) = component else { bail!("candidate path contains traversal or a root"); };
        let name = name.to_str().context("candidate path is not valid UTF-8")?;
        let lower = name.to_ascii_lowercase();
        if name.is_empty() || name.len() > 255 || lower == ".git" || lower == "target"
            || lower == ".env" || lower.starts_with(".env.") || lower == "credentials.json"
            || lower.ends_with(".key") || lower.ends_with(".pem") || lower.ends_with(".p12") {
            bail!("candidate path contains a protected or unsupported component");
        }
        parts.push(name);
    }
    if parts.is_empty() { bail!("candidate path is empty"); }
    Ok(parts.join("/"))
}

fn write_new_bounded(root: &Path, target: &Path, content: &[u8]) -> Result<()> {
    let parent = target.parent().context("candidate target has no parent")?;
    fs::create_dir_all(parent)?;
    let canonical_parent = parent.canonicalize()?;
    let canonical_root = root.canonicalize()?;
    if !canonical_parent.starts_with(&canonical_root) {
        bail!("candidate file escaped its workspace");
    }
    let mut file = OpenOptions::new().write(true).create_new(true).open(target)?;
    file.write_all(content)?;
    file.sync_all()?;
    Ok(())
}

fn changed_lines(before: &str, after: &str) -> usize {
    let left = before.lines().collect::<Vec<_>>();
    let right = after.lines().collect::<Vec<_>>();
    left.iter().zip(&right).filter(|(a,b)| *a != *b).count()
        + left.len().abs_diff(right.len())
}

fn sha256(text: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(text.as_bytes()))
}

fn active_policy_fingerprint(host: &HostClient, cancel: &AtomicBool) -> Result<String> {
    let reply = host.agent_tool_cancelable(None, json!({"type":"policy_status"}), cancel)?;
    if reply.get("ok").and_then(Value::as_bool) != Some(true) {
        bail!("Amity refused the active policy lookup for candidate comparison");
    }
    Ok(reply.pointer("/result/fingerprint").and_then(Value::as_str)
        .context("Amity did not identify the active policy revision")?.to_owned())
}
