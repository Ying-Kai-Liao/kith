//! Signed release manifests pin platform, version, URL, size and SHA-256.
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fmt::Write as _, io::Read, path::Path, time::Duration};

const OFF: &str = "updates are off in this build; rebuild with `kith serve start`";
pub const MAX_ARCHIVE: usize = 128 * 1024 * 1024;
const MAX_UNPACKED: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Release {
    pub version: String,
    pub platform: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

pub fn platform() -> Result<String> {
    let os = match std::env::consts::OS {
        "macos" => "macos",
        "linux" => "linux",
        // The release asset is kith-<version>-windows-x64.zip (scripts/package-windows.sh); the manifest platform
        // keeps the macOS/Linux spelling, windows-x86_64.
        "windows" => "windows",
        other => bail!("host updates do not support {other}"),
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x86_64",
        other => bail!("host updates do not support {other}"),
    };
    Ok(format!("{os}-{arch}"))
}

pub fn sha256(bytes: &[u8]) -> String {
    let mut hash = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        let _ = write!(hash, "{byte:02x}");
    }
    hash
}

/// `release`'s `minProtocol` from its signed compat file. This build has no release channel, so
/// there is never a compat file to read.
#[allow(clippy::unused_async)] // same signature as when it fetched
pub async fn min_protocol(_release: &Release) -> Result<u64> {
    bail!("{OFF}")
}

pub async fn download(url: &str, limit: usize) -> Result<Vec<u8>> {
    let mut response = crate::http()
        .get(url)
        .header(reqwest::header::USER_AGENT, concat!("kith-serve/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(180))
        .send()
        .await?
        .error_for_status()?;
    ensure!(
        response.content_length().is_none_or(|size| size <= limit as u64),
        "update download exceeds the size limit"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(bytes.len().saturating_add(chunk.len()) <= limit, "update download exceeds the size limit");
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// The newest release. This build has no update channel (the fork's upstream one was removed),
/// so there is none: nothing here ever contacts a release server.
#[allow(clippy::unused_async)]
pub async fn latest() -> Result<Release> {
    platform()?;
    bail!("{OFF}")
}

pub async fn archive(release: &Release) -> Result<Vec<u8>> {
    let bytes = download(&release.url, MAX_ARCHIVE).await?;
    ensure!(bytes.len() as u64 == release.size, "update archive size differs from signed manifest");
    ensure!(
        sha256(&bytes).eq_ignore_ascii_case(&release.sha256),
        "update archive checksum differs from signed manifest"
    );
    Ok(bytes)
}

/// Extract just the expected regular executable. Never unpack archive paths,
/// symlinks, permissions or other files into the user's installation.
pub fn extract(bytes: &[u8], platform: &str, destination: &Path) -> Result<()> {
    ensure!(
        extract_file(bytes, platform, "kith-serve", destination)?,
        "release archive does not contain its host executable"
    );
    Ok(())
}

/// Extracts one executable shipped beside the host (`kith-screen` on Linux), with the same
/// rules as [`extract`]. `false` when the archive doesn't carry it.
pub fn extract_file(bytes: &[u8], platform: &str, name: &str, destination: &Path) -> Result<bool> {
    let decoder = flate2::read::GzDecoder::new(bytes).take(MAX_UNPACKED);
    let mut archive = tar::Archive::new(decoder);
    let expected = format!("kith-serve-{platform}/{name}");
    let mut found = false;
    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.path()?.as_ref() != Path::new(&expected) {
            continue;
        }
        ensure!(!found && entry.header().entry_type().is_file(), "update executable must be a single regular file");
        ensure!(entry.size() > 0 && entry.size() < MAX_UNPACKED, "invalid update executable size");
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(destination)?;
        std::io::copy(&mut entry, &mut file)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(std::fs::Permissions::from_mode(0o755))?;
        }
        file.sync_all()?;
        found = true;
    }
    Ok(found)
}
