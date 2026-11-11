//! Download and install an authenticated GitHub Release of Echo.

use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::Read;
use std::process::{Command, Output};

use anyhow::{Context, Result, bail, ensure};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const REPOSITORY: &str = "You-Know-Its-Me-Studios/Amity-Echo";
const EXECUTABLE_ASSET: &str = "echocli-x86_64-pc-windows-msvc.exe";
const CHECKSUM_ASSET: &str = "echocli-x86_64-pc-windows-msvc.exe.sha256";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Release {
    tag_name: String,
    is_draft: bool,
    is_prerelease: bool,
    assets: Vec<ReleaseAsset>,
}

#[derive(Deserialize)]
struct ReleaseAsset {
    name: String,
}

pub fn run() -> Result<()> {
    ensure!(
        cfg!(all(target_os = "windows", target_arch = "x86_64", target_env = "msvc")),
        "Echo updates currently support Windows x64 MSVC only"
    );

    println!("Checking GitHub Releases for Amity Echo...");
    let output = github(&[
        "release", "view", "--repo", REPOSITORY,
        "--json", "tagName,isDraft,isPrerelease,assets",
    ])?;
    let release: Release = serde_json::from_slice(&output.stdout)
        .context("GitHub CLI returned invalid release metadata")?;
    ensure!(!release.is_draft && !release.is_prerelease, "latest Echo release is not a stable published release");
    let remote_version = release.tag_name.strip_prefix('v')
        .context("Echo release tag must begin with v")
        .and_then(|version| Version::parse(version).context("Echo release tag has an invalid version"))?;
    let current_version = Version::parse(env!("CARGO_PKG_VERSION"))
        .context("installed Echo version is invalid")?;
    if remote_version <= current_version {
        println!("Echo is up to date (installed {current_version}, latest {remote_version}).");
        return Ok(());
    }
    ensure!(release.assets.iter().any(|asset| asset.name == EXECUTABLE_ASSET),
        "release {} has no Windows executable asset", release.tag_name);
    ensure!(release.assets.iter().any(|asset| asset.name == CHECKSUM_ASSET),
        "release {} has no checksum asset", release.tag_name);

    println!("Downloading Echo {}...", release.tag_name);
    let download = tempfile::tempdir().context("creating temporary update directory")?;
    let destination = download.path().to_str().context("temporary update path is not Unicode")?;
    github(&[
        "release", "download", &release.tag_name, "--repo", REPOSITORY,
        "--pattern", EXECUTABLE_ASSET, "--pattern", CHECKSUM_ASSET,
        "--dir", destination,
    ])?;

    let executable = download.path().join(EXECUTABLE_ASSET);
    let checksum = download.path().join(CHECKSUM_ASSET);
    verify_checksum(&executable, &checksum)?;
    self_replace::self_replace(&executable)
        .context("replacing the Echo executable; ensure its install directory is writable")?;
    println!("Updated Echo from {current_version} to {remote_version}. Start echocli again to use it.");
    Ok(())
}

fn github(arguments: &[&str]) -> Result<Output> {
    let output = Command::new("gh").args(arguments).output()
        .context("GitHub CLI (gh) is required for updates from the private Echo repository; install gh and run `gh auth login`")?;
    if !output.status.success() {
        let detail = crate::state::redact_credentials(&String::from_utf8_lossy(&output.stderr));
        bail!("GitHub release request failed: {}. Check `gh auth status` and your access to {REPOSITORY}.", detail.trim());
    }
    Ok(output)
}

fn verify_checksum(executable: &std::path::Path, checksum: &std::path::Path) -> Result<()> {
    let expected = fs::read_to_string(checksum)
        .with_context(|| format!("reading release checksum {}", checksum.display()))?;
    let mut fields = expected.split_whitespace();
    let expected_hash = fields.next().context("release checksum is empty")?;
    let expected_name = fields.next().context("release checksum has no asset name")?;
    ensure!(fields.next().is_none() && expected_name == EXECUTABLE_ASSET
        && expected_hash.len() == 64 && expected_hash.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "release checksum has an invalid format");

    let mut input = File::open(executable)
        .with_context(|| format!("opening release executable {}", executable.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = input.read(&mut buffer).context("reading downloaded Echo executable")?;
        if count == 0 { break; }
        hasher.update(&buffer[..count]);
    }
    let mut actual_hash = String::with_capacity(64);
    for byte in hasher.finalize() {
        write!(&mut actual_hash, "{byte:02x}").expect("writing a hash to a String cannot fail");
    }
    ensure!(actual_hash.eq_ignore_ascii_case(expected_hash),
        "downloaded Echo executable does not match the release checksum");
    Ok(())
}
