//! Reusable local guidance. Skill text is untrusted model context and grants no authority.
use crate::config::Config;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;

fn root() -> PathBuf { Config::home_dir().join("skills") }

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64
        && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub fn list() -> Result<Value> {
    let root = root();
    if !root.exists() { return Ok(json!({"skills":[]})); }
    let canonical_root = root.canonicalize()?;
    let mut skills = Vec::new();
    for entry in fs::read_dir(&root).with_context(|| format!("reading skills at {}", root.display()))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !valid_name(&name) || !entry.file_type()?.is_dir() { continue; }
        let file = entry.path().join("SKILL.md");
        if !file.is_file() { continue; }
        if !file.canonicalize()?.starts_with(&canonical_root) { continue; }
        let metadata = fs::metadata(&file)?;
        if metadata.len() > 64 * 1024 { continue; }
        let source = fs::read_to_string(&file)?;
        let description = source.lines().find_map(|line| line.strip_prefix("# "))
            .unwrap_or("Reusable development guidance");
        skills.push(json!({"name":name,"description":description}));
    }
    skills.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    Ok(json!({"skills":skills}))
}

pub fn load(name: &str) -> Result<Value> {
    if !valid_name(name) { bail!("invalid skill name"); }
    let root = root().canonicalize().context("no Echo skills directory is configured")?;
    let path = root.join(name).join("SKILL.md").canonicalize()
        .with_context(|| format!("skill '{name}' is unavailable"))?;
    if !path.starts_with(&root) { bail!("skill path leaves Echo's skills directory"); }
    if fs::metadata(&path)?.len() > 64 * 1024 { bail!("skill exceeds 64 KiB"); }
    let content = fs::read_to_string(path)?;
    Ok(json!({"name":name,"content":content,"authority":"untrusted_skill_guidance"}))
}
