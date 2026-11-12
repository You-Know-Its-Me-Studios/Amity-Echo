//! Local Cadenza deployment management. Cadenza/1 remains the only wire protocol.
//! A future Amity relay can replace this transport at the inference boundary.
use crate::{config::{CadenzaSettings, Config}, credentials};
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use std::{fs::{self, OpenOptions}, net::{SocketAddr, TcpStream}, path::{Path, PathBuf}, process::{Command, Stdio}, time::{Duration, Instant}};

#[derive(Deserialize)]
struct ServiceConfig {
    development: bool,
    listen: SocketAddr,
    api_token_env: Option<String>,
    package: PathBuf,
    trust_file: PathBuf,
}

fn installation(settings: &CadenzaSettings) -> Result<Option<(PathBuf, PathBuf)>> {
    match (&settings.development_executable, &settings.development_config) {
        (Some(executable), Some(config)) => Ok(Some((executable.clone(), config.clone()))),
        (Some(_), None) | (None, Some(_)) => bail!("Choose both the Cadenza executable and its development configuration in /settings"),
        (None, None) => {
            let Some(root) = std::env::var_os("AMITY_CADENZA_HOME") else { return Ok(None); };
            let root = PathBuf::from(root);
            let name = if cfg!(windows) { "cadenza.exe" } else { "cadenza" };
            Ok(Some((root.join("target").join("release").join(name), root.join("config").join("local-lyra.json"))))
        }
    }
}

fn address(settings: &CadenzaSettings) -> Result<SocketAddr> {
    let uri = crate::cadenza::endpoint(settings)?;
    ensure!(uri.scheme_str() == Some("ws"), "Automatic Cadenza startup is limited to the loopback development endpoint");
    let host = uri.host().context("Cadenza endpoint has no host")?.trim_matches(['[', ']']);
    let ip = host.parse::<std::net::IpAddr>().context("Development Cadenza endpoint must use a loopback IP address")?;
    ensure!(ip.is_loopback(), "Development Cadenza endpoint must use a loopback IP address");
    Ok(SocketAddr::new(ip, uri.port_u16().unwrap_or(80)))
}

fn listening(address: SocketAddr) -> bool {
    TcpStream::connect_timeout(&address, Duration::from_millis(250)).is_ok()
}

pub fn prepare(settings: &CadenzaSettings) -> Result<()> {
    if settings.credential_ref != credentials::DEVELOPMENT_REFERENCE { return Ok(()); }
    let address = address(settings)?;
    if listening(address) {
        ensure!(credentials::development_credential_exists()?,
            "A service already owns the Cadenza endpoint, but Echo has no credential for it. Stop that service and let Echo start its configured development instance; Echo will not replace its credential");
        return Ok(());
    }
    let Some((executable, config_path)) = installation(settings)? else {
        bail!("Cadenza is not running. Select its executable and development configuration in /settings, or set AMITY_CADENZA_HOME to its installation directory");
    };
    let home = Config::home_dir();
    fs::create_dir_all(&home).context("creating Echo's local configuration directory")?;
    let lock = OpenOptions::new().read(true).write(true).create(true)
        .open(home.join("cadenza-start.lock"))?;
    lock.lock().context("waiting for another Echo instance to start Cadenza")?;
    // The OS releases the lock when this handle is dropped, including after a crash.
    if listening(address) { return Ok(()); }
    let executable = executable.canonicalize().with_context(|| format!("Cadenza executable is unavailable: {}", executable.display()))?;
    let config_path = config_path.canonicalize().with_context(|| format!("Cadenza development configuration is unavailable: {}", config_path.display()))?;
    ensure!(executable.is_file(), "Configured Cadenza executable is not a file");
    let config: ServiceConfig = serde_json::from_slice(&fs::read(&config_path)?)
        .context("reading Cadenza development configuration")?;
    ensure!(config.development, "Automatic startup requires an explicit Cadenza development configuration");
    ensure!(config.listen == address, "Cadenza configuration listens on {}, but Echo connects to {}; update the endpoint in /settings", config.listen, address);
    ensure!(config.listen.ip().is_loopback(), "Development Cadenza must listen on loopback");
    ensure!(config.package.is_file(), "Cadenza model package is unavailable: {}", config.package.display());
    ensure!(config.trust_file.is_file(), "Cadenza trust file is unavailable: {}", config.trust_file.display());
    let token_env = config.api_token_env.context("Cadenza development configuration has no api_token_env")?;
    credentials::validate_env(&token_env)?;
    let secret = credentials::provision_development()?;
    let log_path = home.join("cadenza-development.log");
    let log = OpenOptions::new().create(true).append(true).open(&log_path)?;
    let mut process = Command::new(&executable);
    process.arg("serve").arg("--config").arg(&config_path)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::from(log))
        .env_clear().env(&token_env, secret);
    for name in ["SystemRoot", "WINDIR", "TEMP", "TMP", "PATH", "CUDA_PATH",
        "HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA"] {
        if let Some(value) = std::env::var_os(name) { process.env(name, value); }
    }
    if let (Some(cuda), Some(path)) = (std::env::var_os("CUDA_PATH"), std::env::var_os("PATH")) {
        let mut entries = vec![Path::new(&cuda).join("bin")];
        entries.extend(std::env::split_paths(&path));
        process.env("PATH", std::env::join_paths(entries)?);
    }
    let mut child = process.spawn().with_context(|| format!("starting Cadenza from {}", executable.display()))?;
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        if listening(address) { return Ok(()); }
        if let Some(status) = child.try_wait()? {
            bail!("Cadenza exited during startup ({status}); see {}", log_path.display());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    bail!("Cadenza did not become available within two minutes; see {}", log_path.display())
}
