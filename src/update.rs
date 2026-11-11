//! Download and install a public GitHub Release of Echo.

use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const REPOSITORY: &str = "You-Know-Its-Me-Studios/Amity-Echo";
const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/You-Know-Its-Me-Studios/Amity-Echo/releases/latest";
const EXECUTABLE_ASSET: &str = "echocli-x86_64-pc-windows-msvc.exe";
const CHECKSUM_ASSET: &str = "echocli-x86_64-pc-windows-msvc.exe.sha256";
const MAX_EXECUTABLE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<ReleaseAsset>,
}

#[derive(Deserialize)]
struct ReleaseAsset {
    name: String,
    size: u64,
    digest: Option<String>,
}

pub fn run(check_only: bool) -> Result<()> {
    ensure!(
        cfg!(all(target_os = "windows", target_arch = "x86_64", target_env = "msvc")),
        "Echo updates currently support Windows x64 MSVC only"
    );

    let client = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(120)))
            .https_only(true)
            .build(),
    );
    println!("Checking GitHub Releases for Amity Echo...");
    let mut response = client.get(LATEST_RELEASE_URL)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "Amity-Echo-Updater")
        .call()
        .context("requesting the latest public Echo release from GitHub")?;
    let metadata = response.body_mut().with_config().limit(1024 * 1024)
        .read_to_vec().context("reading Echo release metadata")?;
    let release: Release = serde_json::from_slice(&metadata)
        .context("GitHub returned invalid release metadata")?;
    ensure!(!release.draft && !release.prerelease, "latest Echo release is not a stable published release");
    let remote_version = release.tag_name.strip_prefix('v')
        .context("Echo release tag must begin with v")
        .and_then(|version| Version::parse(version).context("Echo release tag has an invalid version"))?;
    let current_version = Version::parse(env!("CARGO_PKG_VERSION"))
        .context("installed Echo version is invalid")?;
    let executable_asset = release.assets.iter().find(|asset| asset.name == EXECUTABLE_ASSET)
        .with_context(|| format!("release {} has no Windows executable asset", release.tag_name))?;
    let checksum_asset = release.assets.iter().find(|asset| asset.name == CHECKSUM_ASSET)
        .with_context(|| format!("release {} has no checksum asset", release.tag_name))?;
    println!("Installed: {current_version}  Latest: {remote_version}");
    if remote_version <= current_version {
        println!("Echo is up to date.");
        return Ok(());
    }
    println!("Release: https://github.com/{REPOSITORY}/releases/tag/{}", release.tag_name);
    if check_only {
        println!("Update available. Run `echocli update` to install it.");
        return Ok(());
    }
    ensure!(!cfg!(debug_assertions),
        "this development build cannot update itself; install a release build of Echo first");
    ensure!(!is_source_build()?,
        "this source-tree build cannot update itself; use `cargo install --path . --force` instead");
    ensure!(executable_asset.size <= MAX_EXECUTABLE_BYTES,
        "release executable exceeds the 64 MiB download limit");

    println!("Downloading Echo {} ({:.1} MiB)...", release.tag_name,
        executable_asset.size as f64 / (1024.0 * 1024.0));
    let download = tempfile::tempdir().context("creating temporary update directory")?;
    let executable = download.path().join(EXECUTABLE_ASSET);
    let checksum = download.path().join(CHECKSUM_ASSET);
    download_asset(&client, &release.tag_name, EXECUTABLE_ASSET, &executable,
        executable_asset.size, MAX_EXECUTABLE_BYTES)?;
    download_asset(&client, &release.tag_name, CHECKSUM_ASSET, &checksum,
        checksum_asset.size, 4096)?;
    println!("Verifying release SHA-256...");
    verify_checksum(&executable, &checksum, executable_asset.digest.as_deref())?;
    println!("Installing Echo {remote_version}...");
    self_replace::self_replace(&executable)
        .context("replacing the Echo executable; ensure its install directory is writable")?;
    println!("Updated Echo from {current_version} to {remote_version}. Start echocli again to use it.");
    Ok(())
}

fn download_asset(client: &ureq::Agent, tag: &str, name: &str, path: &Path,
    expected_size: u64, maximum: u64) -> Result<()> {
    ensure!(expected_size <= maximum, "release asset {name} exceeds its download limit");
    let url = format!("https://github.com/{REPOSITORY}/releases/download/{tag}/{name}");
    let mut response = client.get(&url)
        .header("User-Agent", "Amity-Echo-Updater")
        .call()
        .with_context(|| format!("downloading {name} from GitHub Releases"))?;
    let mut output = File::create(path)
        .with_context(|| format!("creating temporary {name}"))?;
    let mut reader = response.body_mut().as_reader().take(maximum + 1);
    let downloaded = io::copy(&mut reader, &mut output)
        .with_context(|| format!("reading release asset {name}"))?;
    ensure!(downloaded <= maximum, "release asset {name} is too large");
    ensure!(downloaded == expected_size, "release asset {name} has an unexpected size");
    Ok(())
}

fn verify_checksum(executable: &Path, checksum: &Path, github_digest: Option<&str>) -> Result<()> {
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
    if let Some(github_digest) = github_digest {
        ensure!(github_digest.eq_ignore_ascii_case(&format!("sha256:{actual_hash}")),
            "downloaded Echo executable does not match GitHub's release digest");
    }
    Ok(())
}

fn is_source_build() -> Result<bool> {
    let executable = std::env::current_exe().context("finding the running Echo executable")?;
    let segments: Vec<String> = executable.components()
        .map(|segment| segment.as_os_str().to_string_lossy().to_ascii_lowercase())
        .collect();
    Ok(segments.windows(2).any(|pair| pair[0] == "target"
        && matches!(pair[1].as_str(), "debug" | "release"))
        || segments.windows(3).any(|triple| triple[0] == "target"
            && matches!(triple[2].as_str(), "debug" | "release")))
}
