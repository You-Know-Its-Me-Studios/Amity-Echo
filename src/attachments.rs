//! Explicitly user-selected local files. Contents are never persisted in sessions.
use anyhow::{Context, Result, bail};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AttachmentRef {
    pub path: PathBuf,
    pub name: String,
    pub mime: String,
    pub sha256: String,
}

pub fn attach(path: impl AsRef<Path>) -> Result<AttachmentRef> {
    let path = path.as_ref().canonicalize().context("finding attachment")?;
    let name = path.file_name().context("attachment has no file name")?
        .to_string_lossy().to_string();
    let lower = name.to_ascii_lowercase();
    if lower == ".env" || lower.starts_with(".env.") || lower.contains("credential")
        || lower.ends_with(".key") || lower.ends_with(".pem") || lower.ends_with(".p12") {
        bail!("credential files cannot be attached");
    }
    let mime = match path.extension().and_then(|extension| extension.to_str())
        .unwrap_or("").to_ascii_lowercase().as_str() {
        "png" => "image/png", "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp", "gif" => "image/gif",
        "txt" | "md" | "rs" | "toml" | "json" | "yaml" | "yml"
        | "xml" | "html" | "css" | "js" | "ts" | "tsx" | "jsx"
        | "py" | "cpp" | "h" | "hpp" | "cs" | "gd" | "shader"
        | "log" | "csv" => "text/plain",
        _ => bail!("this file type is not yet accepted as an attachment"),
    }.to_owned();
    let bytes = fs::read(&path).context("reading attachment")?;
    if bytes.is_empty() || bytes.len() > 4 * 1024 * 1024 {
        bail!("attachments must be between 1 byte and 4 MiB");
    }
    if mime == "text/plain" { std::str::from_utf8(&bytes).context("text attachment is not UTF-8")?; }
    Ok(AttachmentRef { path, name, mime, sha256: format!("{:x}", Sha256::digest(&bytes)) })
}

pub fn content(reference: &AttachmentRef) -> Result<Value> {
    let current = attach(&reference.path)?;
    if current.sha256 != reference.sha256 || current.mime != reference.mime {
        bail!("attachment '{}' changed since it was added", reference.name);
    }
    let bytes = fs::read(&reference.path)?;
    if reference.mime.starts_with("image/") {
        Ok(json!({"name":reference.name,"mime":reference.mime,
            "sha256":reference.sha256,
            "data_base64":base64::engine::general_purpose::STANDARD.encode(bytes)}))
    } else {
        Ok(json!({"name":reference.name,"mime":reference.mime,
            "sha256":reference.sha256,
            "text":String::from_utf8(bytes)?}))
    }
}
