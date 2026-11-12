//! One-turn editor context supplied explicitly by an editor integration.
use crate::config::ProjectRegistration;
use crate::state::redact_credentials;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;

pub fn load(path: &Path, project: &ProjectRegistration) -> Result<Value> {
    let bytes = fs::read(path).context("reading editor context")?;
    if bytes.len() > 64 * 1024 { bail!("editor context exceeds 64 KiB"); }
    let value: Value = serde_json::from_slice(&bytes).context("invalid editor context JSON")?;
    let file = value.get("file").and_then(Value::as_str)
        .context("editor context requires an active file")?;
    let file = Path::new(file).canonicalize().context("active editor file does not exist")?;
    if !file.starts_with(&project.path) || file.components().any(|part| {
        let name = part.as_os_str().to_string_lossy().to_ascii_lowercase();
        name == ".git" || name == ".env" || name.starts_with(".env.")
            || name == "credentials.json" || name.ends_with(".key")
            || name.ends_with(".pem") || name.ends_with(".p12")
    }) {
        bail!("active editor file is outside the connected project or contains credentials");
    }
    let relative = file.strip_prefix(&project.path)?.to_string_lossy().replace('\\', "/");
    let selection = value.get("selection").and_then(Value::as_str).unwrap_or("");
    if selection.len() > 32 * 1024 { bail!("selected editor text exceeds 32 KiB"); }
    let diagnostics = value.get("diagnostics").and_then(Value::as_array)
        .map(|items| items.iter().take(50).filter_map(|item| {
            let message = item.get("message")?.as_str()?;
            let line = item.get("line").and_then(Value::as_u64).unwrap_or(0);
            let severity = item.get("severity").and_then(Value::as_str).unwrap_or("info");
            Some(json!({"line":line,"severity":severity,
                "message":redact_credentials(&message.chars().take(2000).collect::<String>())}))
        }).collect::<Vec<_>>()).unwrap_or_default();
    Ok(json!({"file":relative,"selection":redact_credentials(selection),
        "diagnostics":diagnostics,"authority":"editor_supplied_observation"}))
}
