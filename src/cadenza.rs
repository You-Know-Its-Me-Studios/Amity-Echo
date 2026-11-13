//! First-party Cadenza/1 controls/events. WebSocket framing and TLS are tungstenite/Rustls.
use crate::config::CadenzaSettings;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::net::{IpAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tungstenite::{Message, WebSocket, protocol::WebSocketConfig, stream::MaybeTlsStream};

const MAX_PAYLOAD: usize = 1024 * 1024;
pub const PROTOCOL_VERSION: u32 = 1;
static REQUEST: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ModelSelection {
    pub identity: Value,
    pub root: String,
    /// Actual discovered fields, retained without inventing absent metadata.
    pub metadata: Value,
}
impl ModelSelection {
    pub fn from_discovery(value: Value) -> Result<Self> {
        let root = value
            .get("root")
            .and_then(Value::as_str)
            .context("Cadenza model discovery omitted its root")?
            .to_owned();
        ensure!(
            root.len() == 64 && root.bytes().all(|c| c.is_ascii_hexdigit()),
            "Cadenza returned an invalid model root"
        );
        let identity = value
            .get("identity")
            .filter(|v| v.is_object())
            .context("Cadenza model discovery omitted identity")?
            .clone();
        ensure!(
            identity
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|v| !v.is_empty()),
            "Cadenza returned an invalid model identity"
        );
        Ok(Self {
            identity,
            root,
            metadata: value,
        })
    }
    pub fn label(&self) -> String {
        let name = self.metadata.get("name").and_then(Value::as_str)
            .or_else(|| self.identity.get("name").and_then(Value::as_str))
            .filter(|name| !name.trim().is_empty()).map(str::to_owned)
            .unwrap_or_else(|| {
                // Package IDs are routing identities. Show their final name
                // component when the package has no display name.
                let id = self.identity.get("id").and_then(Value::as_str).unwrap_or("Model");
                id.rsplit('.').next().unwrap_or(id).split(['_', '-'])
                    .filter(|word| !word.is_empty()).map(|word| {
                        let mut chars = word.chars();
                        chars.next().map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                            .unwrap_or_default()
                    }).collect::<Vec<_>>().join(" ")
            });
        let version = self.identity.get("version").and_then(Value::as_str).unwrap_or("").trim();
        if version.is_empty() { name } else { format!("{name} {version}") }
    }
    pub fn ready(&self) -> bool {
        self.metadata.get("ready").and_then(Value::as_bool) == Some(true)
    }
    pub fn context_window(&self) -> Option<u64> {
        self.metadata.get("max_context").and_then(Value::as_u64)
    }
    pub fn structured(&self) -> bool {
        self.metadata
            .get("capabilities")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|c| {
                    c.get("id").and_then(Value::as_str) == Some("echo.agent.v2")
                        && c.get("state").and_then(Value::as_str) == Some("implemented")
                })
            })
    }
    pub fn description(&self) -> String {
        let mut parts = Vec::new();
        if !self.ready() { parts.push("Not ready".to_owned()); }
        if let Some(description) = self.metadata.get("description").and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty()) {
            parts.push(description.to_owned());
        } else {
            parts.push(if self.structured() { "Supports coding tools" } else { "Text generation" }.to_owned());
        }
        if let Some(context) = self.context_window() {
            parts.push(format!("{context} token context"));
        }
        parts.join(" · ")
    }
}

pub fn endpoint(settings: &CadenzaSettings) -> Result<tungstenite::http::Uri> {
    ensure!(
        !settings.endpoint.contains('#'),
        "Cadenza endpoint must not contain a fragment"
    );
    let uri: tungstenite::http::Uri = settings
        .endpoint
        .parse()
        .context("Invalid Cadenza endpoint")?;
    let scheme = uri
        .scheme_str()
        .context("Cadenza endpoint requires ws:// or wss://")?;
    let host = uri.host().context("Cadenza endpoint requires a host")?;
    ensure!(
        uri.path() == "/cadenza"
            && uri.query().is_none()
            && !uri.authority().is_some_and(|a| a.as_str().contains('@')),
        "Use a Cadenza /cadenza endpoint without credentials, query or fragment"
    );
    ensure!(
        scheme == "wss"
            || (scheme == "ws"
                && host
                    .trim_matches(['[', ']'])
                    .parse::<IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())),
        "Remote Cadenza endpoints require certificate-verified wss://; plaintext ws:// is restricted to loopback IPs"
    );
    ensure!(
        (1..=30).contains(&settings.connect_timeout_seconds)
            && (1..=300).contains(&settings.request_timeout_seconds)
            && (1..=256).contains(&settings.max_tokens),
        "Invalid Cadenza timeout or generation preference"
    );
    Ok(uri)
}

pub struct Client {
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    timeout: Duration,
}
impl Client {
    pub fn connect(settings: &CadenzaSettings) -> Result<Self> {
        // Check service availability before reading a credential so an offline
        // server is never misreported as an authentication setup problem.
        let (stream, timeout) = Self::open_stream(settings)?;
        let secret = crate::credentials::resolve(&settings.credential_ref)?;
        Self::negotiate(settings, secret, stream, timeout)
    }
    #[cfg(test)]
    fn connect_authenticated(settings: &CadenzaSettings, secret: String) -> Result<Self> {
        let (stream, timeout) = Self::open_stream(settings)?;
        Self::negotiate(settings, secret, stream, timeout)
    }
    fn open_stream(settings: &CadenzaSettings) -> Result<(TcpStream, Duration)> {
        let uri = endpoint(settings)?;
        let timeout = Duration::from_secs(settings.connect_timeout_seconds);
        let host = uri.host().unwrap().trim_matches(['[', ']']);
        let addresses = (
            host,
            uri.port_u16()
                .unwrap_or(if uri.scheme_str() == Some("wss") {
                    443
                } else {
                    80
                }),
        )
            .to_socket_addrs()
            .context("Resolving Cadenza host")?;
        let deadline = Instant::now() + timeout;
        let mut connection = None;
        for address in addresses.take(8) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            if let Ok(stream) = TcpStream::connect_timeout(&address, remaining) {
                connection = Some(stream);
                break;
            }
        }
        let stream=connection.context("Cadenza unavailable; start the server or check its URL in /settings")?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        stream.set_nodelay(true)?;
        Ok((stream, timeout))
    }
    fn negotiate(settings: &CadenzaSettings, secret: String, stream: TcpStream, timeout: Duration) -> Result<Self> {
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_PAYLOAD))
            .max_frame_size(Some(MAX_PAYLOAD))
            .max_write_buffer_size(2 * MAX_PAYLOAD)
            .write_buffer_size(0);
        let (socket, response) = tungstenite::client_tls_with_config(
            settings.endpoint.as_str(),
            stream,
            Some(config),
            None,
        )
        .map_err(|_| {
            anyhow::anyhow!(
                "Cadenza WebSocket/TLS handshake failed; verify endpoint and server certificate"
            )
        })?;
        ensure!(
            response.headers().get("sec-websocket-extensions").is_none(),
            "Unnegotiated Cadenza WebSocket extension"
        );
        let mut client = Self {
            socket,
            timeout: Duration::from_secs(settings.request_timeout_seconds),
        };
        client.send(json!({"type":"hello","versions":[PROTOCOL_VERSION],"token":secret}))?;
        let hello = client.next_event(Instant::now() + timeout)?;
        check_error(&hello)?;
        ensure!(
            hello["type"] == "hello" && hello["version"] == PROTOCOL_VERSION,
            "Cadenza/1 negotiation failed"
        );
        client.poll_timeout()?;
        Ok(client)
    }
    fn poll_timeout(&mut self) -> Result<()> {
        let stream = match self.socket.get_mut() {
            MaybeTlsStream::Plain(s) => s,
            MaybeTlsStream::Rustls(s) => &mut s.sock,
            _ => bail!("Unsupported Cadenza TLS stream"),
        };
        stream.set_read_timeout(Some(Duration::from_millis(25)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        Ok(())
    }
    pub fn send(&mut self, value: Value) -> Result<()> {
        let text = serde_json::to_string(&value)?;
        ensure!(
            text.len() <= MAX_PAYLOAD,
            "Cadenza request exceeds protocol size limit"
        );
        self.socket
            .send(Message::Text(text.into()))
            .context("Sending Cadenza request")
    }
    pub fn poll(&mut self) -> Result<Option<Value>> {
        match self.socket.read() {
            Ok(Message::Text(text)) => Ok(Some(
                serde_json::from_str(text.as_str()).context("Invalid Cadenza JSON event")?,
            )),
            Ok(Message::Ping(_) | Message::Pong(_)) => {
                self.socket.flush()?;
                Ok(None)
            }
            Ok(Message::Close(_)) => {
                bail!("Cadenza disconnected; the stream was not retried. Reconnect for a new turn")
            }
            Ok(_) => bail!("Unexpected Cadenza/1 binary output"),
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Ok(None)
            }
            Err(_) => bail!(
                "Cadenza connection interrupted; no generation was replayed. Retry a new turn when the service is ready"
            ),
        }
    }
    fn next_event(&mut self, deadline: Instant) -> Result<Value> {
        loop {
            ensure!(Instant::now() < deadline, "Cadenza response timed out");
            if let Some(event) = self.poll()? {
                return Ok(event);
            }
        }
    }
    pub fn discover(&mut self) -> Result<Vec<ModelSelection>> {
        self.send(json!({"type":"discover"}))?;
        let event = self.next_event(Instant::now() + self.timeout.min(Duration::from_secs(10)))?;
        check_error(&event)?;
        ensure!(
            event["type"] == "models",
            "Expected Cadenza model discovery response"
        );
        let models = event["models"]
            .as_array()
            .context("Invalid Cadenza model list")?;
        ensure!(
            models.len() <= 1024,
            "Cadenza model discovery exceeds limit"
        );
        let mut seen = std::collections::HashSet::new();
        models
            .iter()
            .map(|v| {
                let model = ModelSelection::from_discovery(v.clone())?;
                ensure!(
                    seen.insert(model.root.clone()),
                    "Duplicate Cadenza model root"
                );
                Ok(model)
            })
            .collect()
    }
    pub fn health(&mut self) -> Result<Value> {
        self.send(json!({"type":"status"}))?;
        let event = self.next_event(Instant::now() + self.timeout.min(Duration::from_secs(10)))?;
        check_error(&event)?;
        ensure!(
            event["type"] == "status" && event["worker_alive"] == true,
            "Cadenza model worker is not ready"
        );
        Ok(event)
    }
    pub fn validate_selection(&mut self, selected: &ModelSelection) -> Result<ModelSelection> {
        let current=self.discover()?.into_iter().find(|m|m.root==selected.root).context("Selected Cadenza model is unavailable; restore it in Cadenza or select another model with /model")?;
        ensure!(
            current.identity == selected.identity,
            "Cadenza model identity changed for the selected root"
        );
        ensure!(
            current.ready(),
            "Selected Cadenza model is not ready; previous selection was preserved"
        );
        Ok(current)
    }
    pub fn generate(
        &mut self,
        model: &ModelSelection,
        prompt: &str,
        max_tokens: u16,
        cancel: &AtomicBool,
        mut event: impl FnMut(Value),
    ) -> Result<Completion> {
        ensure!(
            !prompt.is_empty() && prompt.len() <= 64 * 1024,
            "Cadenza prompt must contain 1..65536 UTF-8 bytes; shorten the conversation or start a new session"
        );
        ensure!(
            !cancel.load(Ordering::Relaxed),
            "Generation cancelled before submission"
        );
        let id = format!(
            "echo_{}_{}",
            std::process::id(),
            REQUEST.fetch_add(1, Ordering::Relaxed)
        );
        self.send(json!({"type":"generate","id":id,"model_root":model.root,"prompt":prompt,"max_tokens":max_tokens,"priority":0}))?;
        let deadline = Instant::now() + self.timeout;
        let mut cancellation = None;
        let mut text = String::new();
        let mut prompt_tokens = None;
        let mut output_tokens = 0;
        loop {
            if cancel.load(Ordering::Relaxed) && cancellation.is_none() {
                self.send(json!({"type":"cancel","id":id}))?;
                cancellation = Some(Instant::now());
            }
            ensure!(
                Instant::now() < deadline
                    && cancellation.is_none_or(|t| t.elapsed() < Duration::from_secs(5)),
                "Cadenza generation timed out; connection closed to cancel owned work"
            );
            event(json!({"type":"heartbeat"}));
            let Some(value) = self.poll()? else {
                continue;
            };
            if value.get("id").is_some()
                || matches!(
                    value["type"].as_str(),
                    Some("accepted" | "token" | "complete")
                )
            {
                ensure!(
                    value["id"] == id,
                    "Cadenza stream request identity mismatch"
                );
            }
            check_error(&value)?;
            match value["type"].as_str() {
                Some("accepted") => {
                    ensure!(prompt_tokens.is_none(), "Duplicate Cadenza admission event");
                    prompt_tokens = value["prompt_tokens"].as_u64();
                    ensure!(
                        prompt_tokens.is_some(),
                        "Cadenza admission omitted token usage"
                    );
                    event(value);
                }
                Some("token") => {
                    ensure!(
                        value["index"].as_u64() == Some(output_tokens),
                        "Cadenza token sequence mismatch"
                    );
                    ensure!(
                        prompt_tokens.is_some()
                            && value["token_id"].as_i64().is_some_and(|id| id >= 0),
                        "Invalid Cadenza token event"
                    );
                    text = value["text"]
                        .as_str()
                        .context("Cadenza token omitted cumulative text")?
                        .to_owned();
                    ensure!(text.len() <= 128 * 1024, "Cadenza output exceeds limit");
                    output_tokens += 1;
                    event(value);
                }
                Some("complete") => {
                    let reason = value["reason"]
                        .as_str()
                        .context("Cadenza completion omitted reason")?
                        .to_owned();
                    ensure!(
                        prompt_tokens.is_some(),
                        "Cadenza completion arrived without admission"
                    );
                    if let Some(usage) = value.get("usage") {
                        ensure!(
                            usage["prompt_tokens"].as_u64() == prompt_tokens
                                && usage["output_tokens"].as_u64() == Some(output_tokens),
                            "Cadenza usage does not match the observed stream"
                        );
                    }
                    return Ok(Completion {
                        text,
                        prompt_tokens,
                        output_tokens,
                        reason,
                    });
                }
                Some("pong") => {}
                _ => bail!("Unexpected Cadenza generation event"),
            }
        }
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.socket.close(None);
    }
}
pub struct Completion {
    pub text: String,
    pub prompt_tokens: Option<u64>,
    pub output_tokens: u64,
    pub reason: String,
}
pub fn check_error(value: &Value) -> Result<()> {
    if value["type"] == "error" {
        let code = value["code"].as_str().unwrap_or("unknown_error");
        if code == "authentication_or_version_failed" {
            bail!(
                "Cadenza rejected the supplied credential or Cadenza/1 negotiation. If another service owns the endpoint, Echo will not change its credential"
            );
        }
        bail!(
            "Cadenza {code} (retryable={}): {}",
            value["retryable"].as_bool().unwrap_or(false),
            crate::state::redact_credentials(
                value["message"]
                    .as_str()
                    .unwrap_or("request rejected; check model readiness and context limits")
            )
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, thread};
    fn fixture(root: char) -> Value {
        json!({"identity":{"id":format!("test.{root}"),"version":"1"},"root":root.to_string().repeat(64),"ready":true,"max_context":128})
    }
    fn server(
        work: impl FnOnce(WebSocket<TcpStream>) + Send + 'static,
    ) -> (CadenzaSettings, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            let hello: Value = serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(hello["versions"], json!([1]));
            ws.send(Message::Text(
                json!({"type":"hello","version":1}).to_string().into(),
            ))
            .unwrap();
            work(ws);
        });
        (
            CadenzaSettings {
                endpoint: format!("ws://127.0.0.1:{port}/cadenza"),
                ..Default::default()
            },
            handle,
        )
    }
    fn read(ws: &mut WebSocket<TcpStream>) -> Value {
        serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap()
    }
    fn send(ws: &mut WebSocket<TcpStream>, v: Value) {
        ws.send(Message::Text(v.to_string().into())).unwrap();
    }
    fn client(settings: &CadenzaSettings) -> Client {
        Client::connect_authenticated(settings, "synthetic-test-token".repeat(2)).unwrap()
    }
    #[test]
    fn secure_endpoints() {
        for url in [
            "ws://127.0.0.1:7941/cadenza",
            "ws://[::1]:7941/cadenza",
            "wss://example.com/cadenza",
        ] {
            assert!(
                endpoint(&CadenzaSettings {
                    endpoint: url.into(),
                    ..Default::default()
                })
                .is_ok()
            );
        }
        for url in [
            "ws://example.com/cadenza",
            "ws://localhost:7941/cadenza",
            "https://example.com/cadenza",
            "wss://secret@example.com/cadenza",
            "wss://example.com/cadenza?token=x",
            "wss://example.com/cadenza#token",
        ] {
            assert!(
                endpoint(&CadenzaSettings {
                    endpoint: url.into(),
                    ..Default::default()
                })
                .is_err(),
                "{url}"
            );
        }
    }
    #[test]
    #[cfg(windows)]
    fn offline_server_is_not_reported_as_missing_credentials() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let settings = CadenzaSettings {
            endpoint: format!("ws://127.0.0.1:{port}/cadenza"),
            credential_ref: format!("windows:Amity/Echo/Cadenza/missing-{}", std::process::id()),
            ..Default::default()
        };
        let missing = Client::connect(&settings).err().unwrap().to_string();
        assert!(missing.contains("credential is missing"), "{missing}");
        drop(listener);
        let offline = Client::connect(&settings).err().unwrap().to_string();
        assert!(offline.contains("Cadenza unavailable"), "{offline}");
    }
    #[test]
    fn dynamic_discovery_and_health() {
        let (settings, server) = server(|mut ws| {
            assert_eq!(read(&mut ws)["type"], "discover");
            send(
                &mut ws,
                json!({"type":"models","models":[fixture('a'),fixture('b')]}),
            );
            assert_eq!(read(&mut ws)["type"], "status");
            send(&mut ws, json!({"type":"status","worker_alive":true}));
        });
        let mut c = client(&settings);
        let models = c.discover().unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[1].root, "b".repeat(64));
        assert_eq!(models[0].description(), "Text generation · 128 token context");
        c.health().unwrap();
        server.join().unwrap();
    }
    #[test]
    fn cumulative_utf8_and_usage() {
        let (settings, server) = server(|mut ws| {
            let request = read(&mut ws);
            let id = &request["id"];
            send(
                &mut ws,
                json!({"type":"accepted","id":id,"prompt_tokens":3}),
            );
            for (i, text) in [" \u{fffd}", " é"].iter().enumerate() {
                send(
                    &mut ws,
                    json!({"type":"token","id":id,"index":i,"token_id":20,"text":text}),
                );
            }
            send(
                &mut ws,
                json!({"type":"complete","id":id,"reason":"length","usage":{"prompt_tokens":3,"output_tokens":2}}),
            );
        });
        let mut c = client(&settings);
        let model = ModelSelection::from_discovery(fixture('a')).unwrap();
        let mut snapshots = Vec::new();
        let result = c
            .generate(&model, "hello", 2, &AtomicBool::new(false), |v| {
                if v["type"] == "token" {
                    snapshots.push(v["text"].as_str().unwrap().to_owned())
                }
            })
            .unwrap();
        assert_eq!(result.text, " é");
        assert_eq!(result.output_tokens, 2);
        assert_eq!(snapshots.last().unwrap(), " é");
        server.join().unwrap();
    }
    #[test]
    fn cancellation_waits_for_ack() {
        let (settings, server) = server(|mut ws| {
            let request = read(&mut ws);
            let id = request["id"].clone();
            send(
                &mut ws,
                json!({"type":"accepted","id":id,"prompt_tokens":2}),
            );
            send(
                &mut ws,
                json!({"type":"token","id":id,"index":0,"token_id":1,"text":"first"}),
            );
            let cancel = read(&mut ws);
            assert_eq!(cancel["type"], "cancel");
            assert_eq!(cancel["id"], id);
            send(
                &mut ws,
                json!({"type":"complete","id":id,"reason":"cancelled"}),
            );
        });
        let mut c = client(&settings);
        let model = ModelSelection::from_discovery(fixture('a')).unwrap();
        let cancel = AtomicBool::new(false);
        let result = c
            .generate(&model, "hello", 32, &cancel, |v| {
                if v["type"] == "token" {
                    cancel.store(true, Ordering::Relaxed)
                }
            })
            .unwrap();
        assert_eq!(result.reason, "cancelled");
        assert_eq!(result.output_tokens, 1);
        server.join().unwrap();
    }
    #[test]
    fn disconnect_does_not_replay() {
        let (settings, server) = server(|mut ws| {
            assert_eq!(read(&mut ws)["type"], "generate");
            ws.close(None).unwrap();
        });
        let mut c = client(&settings);
        let result = c.generate(
            &ModelSelection::from_discovery(fixture('a')).unwrap(),
            "hello",
            2,
            &AtomicBool::new(false),
            |_| {},
        );
        assert!(result.is_err());
        server.join().unwrap();
    }
    #[test]
    fn invalid_stream_identity_rejected() {
        let (settings, server) = server(|mut ws| {
            read(&mut ws);
            send(
                &mut ws,
                json!({"type":"accepted","id":"other","prompt_tokens":1}),
            );
        });
        let mut c = client(&settings);
        assert!(
            c.generate(
                &ModelSelection::from_discovery(fixture('a')).unwrap(),
                "hi",
                1,
                &AtomicBool::new(false),
                |_| {}
            )
            .is_err()
        );
        server.join().unwrap();
    }
    #[test]
    fn missing_or_unready_model_preserves_selection() {
        let (settings, server) = server(|mut ws| {
            read(&mut ws);
            let mut model = fixture('b');
            model["ready"] = json!(false);
            send(&mut ws, json!({"type":"models","models":[model]}));
        });
        let mut c = client(&settings);
        let selected = ModelSelection::from_discovery(fixture('a')).unwrap();
        assert!(c.validate_selection(&selected).is_err());
        assert_eq!(selected.root, "a".repeat(64));
        server.join().unwrap();
    }
    #[test]
    fn structured_errors_are_actionable() {
        assert!(
            check_error(&json!({"type":"error","code":"authentication_or_version_failed"}))
                .unwrap_err()
                .to_string()
                .contains("supplied credential")
        );
        assert!(check_error(&json!({"type":"error","code":"invalid_request","message":"context capacity exceeded","retryable":false})).unwrap_err().to_string().contains("context capacity"));
    }
    #[test]
    #[ignore = "Requires a real Cadenza service and isolated Echo configuration"]
    fn real_cadenza_acceptance() {
        let config = crate::config::Config::load().unwrap();
        let settings = &config.cadenza;
        let mut c = Client::connect(settings).unwrap();
        let models = c.discover().unwrap();
        assert!(!models.is_empty());
        let model = models.into_iter().find(ModelSelection::ready).unwrap();
        c.health().unwrap();
        let mut events = Vec::new();
        let result = c
            .generate(
                &model,
                "The meaning of life is",
                8,
                &AtomicBool::new(false),
                |v| {
                    if v["type"] == "token" {
                        events.push(v)
                    }
                },
            )
            .unwrap();
        assert_eq!(events.len() as u64, result.output_tokens);
        assert!(!result.text.is_empty());
        assert!(result.prompt_tokens.is_some());
        let cancellation = AtomicBool::new(false);
        let partial = c
            .generate(&model, "The meaning of life is", 64, &cancellation, |v| {
                if v["type"] == "token" {
                    cancellation.store(true, Ordering::Relaxed)
                }
            })
            .unwrap();
        assert!(matches!(
            partial.reason.as_str(),
            "cancelled" | "eos" | "length"
        ));
        drop(c);
        let mut reconnected = Client::connect(settings).unwrap();
        reconnected.validate_selection(&model).unwrap();
        reconnected.health().unwrap();
        let mut missing = model.clone();
        missing.root = "0".repeat(64);
        assert!(reconnected.validate_selection(&missing).is_err());
        assert!(Client::connect_authenticated(settings, "invalid-test-token".repeat(3)).is_err());
        let report = json!({"model":model,"generated_text":result.text,"prompt_tokens":result.prompt_tokens,"output_tokens":result.output_tokens,"token_events":events,"cancellation_reason":partial.reason,"reconnection":true,"invalid_credentials_rejected":true,"missing_model_rejected":true});
        std::fs::write(
            std::env::var_os("ECHO_CADENZA_REPORT").unwrap(),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
    }
}
