//! Built-in model-process bridge for local Ollama models.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::time::Duration;

pub fn discover_ollama(base_url: &str) -> Result<Vec<String>> {
    let url = endpoint(base_url, "api/tags")?;
    let agent = client(Duration::from_secs(10));
    let mut response = agent.get(&url).call().context("connecting to Ollama")?;
    let bytes = response.body_mut().with_config().limit(1024 * 1024).read_to_vec()
        .context("reading Ollama model list")?;
    let data: Value = serde_json::from_slice(&bytes).context("Ollama returned invalid model list")?;
    let mut names = data.get("models").and_then(Value::as_array)
        .context("Ollama response has no models")?.iter()
        .filter_map(|model| model.get("name").and_then(Value::as_str))
        .map(str::to_owned).collect::<Vec<_>>();
    names.sort(); names.dedup();
    Ok(names)
}

pub fn ollama_supports_vision(base_url: &str, model: &str) -> Result<bool> {
    let url = endpoint(base_url, "api/show")?;
    let agent = client(Duration::from_secs(10));
    let mut response = agent.post(&url).header("Content-Type", "application/json")
        .send(json!({"model":model}).to_string())?;
    let bytes = response.body_mut().with_config().limit(1024 * 1024).read_to_vec()?;
    let data: Value = serde_json::from_slice(&bytes)?;
    Ok(data.get("capabilities").and_then(Value::as_array)
        .is_some_and(|items| items.iter().any(|value| value.as_str() == Some("vision"))))
}

pub fn run(args: &[String]) -> Result<()> {
    if let [provider, base_url, model, key_env] = args {
        if provider == "api_chat" { return run_api_chat(base_url, model, key_env); }
    }
    let [provider, base_url, model] = args else { bail!("invalid internal model bridge arguments"); };
    if provider != "ollama" { bail!("unsupported built-in model provider"); }
    if model.trim().is_empty() || model.len() > 256 { bail!("invalid model name"); }
    let url = endpoint(base_url, "api/chat")?;
    let mut input = Vec::new();
    io::stdin().take(8 * 1024 * 1024 + 1).read_to_end(&mut input)?;
    if input.len() > 8 * 1024 * 1024 { bail!("model input exceeds 8 MiB"); }
    let mut value: Value = serde_json::from_slice(&input).context("invalid Echo model input")?;
    let mut images = Vec::new();
    let mut text_files = String::new();
    if let Some(items) = value.as_object_mut().and_then(|fields| fields.remove("attachments"))
        .and_then(|value| value.as_array().cloned()) {
        for item in items {
            let name = item.get("name").and_then(Value::as_str).unwrap_or("attachment");
            if let Some(encoded) = item.get("data_base64").and_then(Value::as_str) {
                images.push(encoded.to_owned());
            } else if let Some(text) = item.get("text").and_then(Value::as_str) {
                text_files.push_str(&format!("\nUser attached text file {name}:\n{text}\n"));
            }
        }
    }
    let turns = if value.get("echo_protocol").and_then(Value::as_str) == Some("agent.v2") {
        "tool_request, aip_request, or final_proposal"
    } else { "aip_request or final_proposal" };
    let body = json!({"model":model,"stream":true,"format":"json",
        "messages":[
            {"role":"system","content":format!("You are the selected model for Amity Echo. Return exactly one JSON ModelTurn object of these types: {turns}. Do not wrap it in Markdown. Follow the supplied task instruction and prior turns. Never claim a tool succeeded without an observation.")},
            {"role":"user","content":format!("{}{text_files}",value),"images":images}
        ]});
    let agent = client(Duration::from_secs(300));
    let mut response = agent.post(&url).header("Content-Type", "application/json")
        .send(body.to_string())
        .context("requesting a response from Ollama")?;
    let mut reader = BufReader::new(response.body_mut().as_reader());
    let mut output = String::new();
    let mut preview = String::new();
    let mut done = false;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 { break; }
        if line.trim().is_empty() { continue; }
        let record: Value = serde_json::from_str(&line).context("invalid Ollama stream record")?;
        if let Some(fragment) = record.pointer("/message/content").and_then(Value::as_str) {
            output.push_str(fragment);
            if output.len() > 128 * 1024 { bail!("model response exceeds 128 KiB"); }
            if let Some(partial) = proposal_prefix(&output) {
                if partial.starts_with(&preview) && partial.len() > preview.len() {
                    emit(&json!({"type":"delta","text":&partial[preview.len()..]}))?;
                    preview = partial;
                }
            }
        }
        if record.get("done").and_then(Value::as_bool) == Some(true) {
            emit(&json!({"type":"usage",
                "input_tokens":record.get("prompt_eval_count"),
                "output_tokens":record.get("eval_count")}))?;
            done = true;
            break;
        }
    }
    if !done { bail!("Ollama closed the response before completion"); }
    let turn: Value = serde_json::from_str(&output).context("model did not return a valid ModelTurn")?;
    if !matches!(turn.get("type").and_then(Value::as_str),
        Some("tool_request" | "aip_request" | "final_proposal")) {
        bail!("model returned an unknown turn type");
    }
    emit(&turn)
}

/// A connector for providers implementing the OpenAI Chat Completions stream.
/// The credential name is saved in the profile; the credential value remains in the environment.
fn run_api_chat(base_url: &str, model: &str, key_env: &str) -> Result<()> {
    if model.trim().is_empty() || model.len() > 256 { bail!("invalid model name"); }
    let base = api_base(base_url)?;
    validate_key_env(key_env)?;
    let key = std::env::var(key_env).with_context(|| format!("set {key_env} before starting Echo"))?;
    if key.trim().is_empty() || key.contains('\n') || key.contains('\r') {
        bail!("model credential is missing or invalid");
    }
    let mut input = Vec::new();
    io::stdin().take(8 * 1024 * 1024 + 1).read_to_end(&mut input)?;
    if input.len() > 8 * 1024 * 1024 { bail!("model input exceeds 8 MiB"); }
    let mut value: Value = serde_json::from_slice(&input).context("invalid Echo model input")?;
    let mut content = vec![json!({"type":"text","text":""})];
    let mut text_files = String::new();
    if let Some(items) = value.as_object_mut().and_then(|fields| fields.remove("attachments"))
        .and_then(|value| value.as_array().cloned()) {
        for item in items {
            let name = item.get("name").and_then(Value::as_str).unwrap_or("attachment");
            if let Some(encoded) = item.get("data_base64").and_then(Value::as_str) {
                let mime = item.get("mime").and_then(Value::as_str).unwrap_or("image/png");
                if !matches!(mime, "image/png" | "image/jpeg" | "image/webp" | "image/gif") {
                    bail!("unsupported image type");
                }
                content.push(json!({"type":"image_url","image_url":{
                    "url":format!("data:{mime};base64,{encoded}")}}));
            } else if let Some(text) = item.get("text").and_then(Value::as_str) {
                text_files.push_str(&format!("\nUser attached text file {name}:\n{text}\n"));
            }
        }
    }
    let turns = if value.get("echo_protocol").and_then(Value::as_str) == Some("agent.v2") {
        "tool_request, aip_request, or final_proposal"
    } else { "aip_request or final_proposal" };
    content[0] = json!({"type":"text","text":format!("{value}{text_files}")});
    let body = json!({"model":model,"stream":true,
        "stream_options":{"include_usage":true},
        "messages":[
            {"role":"system","content":format!("You are the selected model for Amity Echo. Return exactly one JSON ModelTurn object of these types: {turns}. Do not wrap it in Markdown. Never claim a tool succeeded without an observation.")},
            {"role":"user","content":content}
        ]});
    let agent = client(Duration::from_secs(300));
    let mut response = agent.post(&format!("{base}/chat/completions"))
        .header("Authorization", &format!("Bearer {key}"))
        .header("Content-Type", "application/json")
        .send(body.to_string()).context("requesting a model response")?;
    let mut reader = BufReader::new(response.body_mut().as_reader());
    let mut output = String::new();
    let mut preview = String::new();
    let mut done = false;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 { break; }
        let line = line.trim();
        let Some(data) = line.strip_prefix("data:").map(str::trim) else { continue; };
        if data == "[DONE]" { done = true; break; }
        let record: Value = serde_json::from_str(data).context("invalid model stream event")?;
        if let Some(fragment) = record.pointer("/choices/0/delta/content").and_then(Value::as_str) {
            output.push_str(fragment);
            if output.len() > 128 * 1024 { bail!("model response exceeds 128 KiB"); }
            if let Some(partial) = proposal_prefix(&output) {
                if partial.starts_with(&preview) && partial.len() > preview.len() {
                    emit(&json!({"type":"delta","text":&partial[preview.len()..]}))?;
                    preview = partial;
                }
            }
        }
        if let Some(usage) = record.get("usage").filter(|usage| !usage.is_null()) {
            emit(&json!({"type":"usage","input_tokens":usage.get("prompt_tokens"),
                "output_tokens":usage.get("completion_tokens")}))?;
        }
    }
    if !done { bail!("model stream closed before completion"); }
    let turn: Value = serde_json::from_str(&output).context("model did not return a valid ModelTurn")?;
    if !matches!(turn.get("type").and_then(Value::as_str),
        Some("tool_request" | "aip_request" | "final_proposal")) {
        bail!("model returned an unknown turn type");
    }
    emit(&turn)
}

fn api_base(url: &str) -> Result<String> {
    let base = url.trim_end_matches('/');
    if !(base.starts_with("https://") || base.starts_with("http://127.0.0.1:")
        || base.starts_with("http://localhost:")) || base.contains('@')
        || base.contains('?') || base.contains('#') || base.len() > 2048 {
        bail!("API endpoint must be HTTPS or local HTTP without credentials in the URL");
    }
    Ok(base.to_owned())
}

pub fn validate_key_env(name: &str) -> Result<()> {
    if !name.starts_with("AMITY_ECHO_") || name.len() > 80
        || !name.bytes().all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_') {
        bail!("credential variable must be an AMITY_ECHO_ uppercase environment name");
    }
    Ok(())
}

pub fn discover_api_models(base_url: &str, key_env: &str) -> Result<Vec<String>> {
    let base = api_base(base_url)?;
    validate_key_env(key_env)?;
    let key = std::env::var(key_env).with_context(|| format!("set {key_env} before starting Echo"))?;
    let agent = client(Duration::from_secs(10));
    let mut response = agent.get(&format!("{base}/models"))
        .header("Authorization", &format!("Bearer {key}"))
        .call().context("discovering provider models")?;
    let bytes = response.body_mut().with_config().limit(1024 * 1024).read_to_vec()?;
    let data: Value = serde_json::from_slice(&bytes)?;
    let mut names = data.get("data").and_then(Value::as_array)
        .context("provider model list has no data array")?.iter()
        .filter_map(|item| item.get("id").and_then(Value::as_str))
        .map(str::to_owned).collect::<Vec<_>>();
    names.sort(); names.dedup();
    Ok(names)
}

fn proposal_prefix(output: &str) -> Option<String> {
    let key = output.find("\"proposal\"")?;
    let tail = output.get(key + "\"proposal\"".len()..)?;
    let colon = tail.find(':')?;
    let tail = tail.get(colon + 1..)?.trim_start();
    let tail = tail.strip_prefix('"')?;
    let mut escaped = false;
    let mut end = tail.len();
    for (index, ch) in tail.char_indices() {
        if escaped { escaped = false; continue; }
        if ch == '\\' { escaped = true; continue; }
        if ch == '"' { end = index; break; }
    }
    let encoded = format!("\"{}\"", tail.get(..end)?);
    serde_json::from_str(&encoded).ok()
}

fn emit(value: &Value) -> Result<()> {
    let mut output = io::stdout().lock();
    writeln!(output, "{value}")?;
    output.flush()?;
    Ok(())
}

fn endpoint(base_url: &str, path: &str) -> Result<String> {
    let base = base_url.trim_end_matches('/');
    if !(base.starts_with("http://127.0.0.1:") || base.starts_with("http://localhost:")) {
        bail!("built-in Ollama connection must use a local endpoint");
    }
    if base.contains('?') || base.contains('#') || base.contains('@') {
        bail!("invalid Ollama endpoint");
    }
    Ok(format!("{base}/{path}"))
}

fn client(timeout: Duration) -> ureq::Agent {
    ureq::Agent::new_with_config(ureq::Agent::config_builder()
        .timeout_global(Some(timeout)).build())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_preview_only_uses_proposal_text() {
        assert_eq!(proposal_prefix("{\"type\":\"final_proposal\",\"proposal\":\"Hello\\nworld\"}"),
            Some("Hello\nworld".into()));
        assert_eq!(proposal_prefix("{\"type\":\"tool_request\",\"arguments\":{}}"), None);
    }

    #[test]
    fn api_connection_rejects_credential_urls_and_arbitrary_environment() {
        assert!(api_base("https://api.example.test/v1").is_ok());
        assert!(api_base("https://token@api.example.test/v1").is_err());
        assert!(api_base("http://api.example.test/v1").is_err());
        assert!(validate_key_env("AMITY_ECHO_API_KEY").is_ok());
        assert!(validate_key_env("HOME").is_err());
    }
}
