//! Owner-only files. The core folder is 0700 and core.db (+ -wal, -shm), core.lock, core.holder and the marker 0600, refused at
//! start if looser, with the command that fixes it. On Windows the same rule is an access list with no entry for
//! anyone but the owner, SYSTEM and Administrators (read with `icacls`).

use std::fs;
use std::path::Path;

use crate::error::{Error, Result};
use crate::layout::Layout;

/// Creates the core folder (owner-only) when it is missing, and refuses one that is open to others.
pub(crate) fn ensure_dir(dir: &Path) -> Result<()> {
    if !dir.exists() {
        create_private_dir(dir)?;
    }
    check_dir(dir)
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    // The machine folder above may not exist yet on a first setup; those parents keep the default mode.
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    restrict(dir)
}

#[cfg(unix)]
fn mode(path: &Path) -> Result<u32> {
    use std::os::unix::fs::PermissionsExt as _;
    Ok(fs::metadata(path)?.permissions().mode() & 0o777)
}

#[cfg(unix)]
fn check_dir(dir: &Path) -> Result<()> {
    let m = mode(dir)?;
    if m & 0o077 != 0 {
        return Err(Error::Permissions(format!(
            "kith-core: {} is mode {m:03o}, open to other users; kith won't start until it is private: chmod 700 '{}'",
            dir.display(),
            dir.display()
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn check_file(path: &Path) -> Result<()> {
    let m = mode(path)?;
    if m & 0o077 != 0 {
        return Err(Error::Permissions(format!(
            "kith-core: {} is mode {m:03o}, open to other users; kith won't start until it is private: chmod 600 '{}'",
            path.display(),
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_dir(dir: &Path) -> Result<()> {
    check_acl(dir)
}

#[cfg(not(unix))]
fn check_file(path: &Path) -> Result<()> {
    check_acl(path)
}

/// The folder and every file of the layout that exists must be owner-only.
pub(crate) fn check_files(layout: &Layout) -> Result<()> {
    check_dir(&layout.dir)?;
    for f in layout.files() {
        if f.exists() {
            check_file(&f)?;
        }
    }
    Ok(())
}

/// Makes an owner-only empty file when none exists (so SQLite, which copies the main file's mode to its -wal and -shm,
/// never creates core.db with the process umask). On Windows files take their access list from the core folder (set
/// with `(OI)(CI)` in `ensure_dir`), so nothing is pre-made: SQLite creates core.db itself and `check_files` reads the
/// result. (Pre-making it and rewriting its list with `icacls` first is what SQLite then could not open there.)
#[cfg_attr(not(unix), allow(clippy::unnecessary_wraps))]
pub(crate) fn create_private_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    if !path.exists() {
        open_private(path)?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Opens (creating owner-only) a file for read and write, without truncating it.
pub(crate) fn open_private(path: &Path) -> Result<fs::File> {
    let mut o = fs::OpenOptions::new();
    o.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        o.mode(0o600);
    }
    Ok(o.open(path)?)
}

/// Sets owner-only on a file the core did not create itself (a `VACUUM INTO` snapshot). On Windows it already has the
/// folder's list; this checks it.
pub(crate) fn make_private(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    check_file(path)?;
    Ok(())
}

/// Principals an owner-only access list may name besides the owner: the system and the administrators already have the
/// machine.
const ALLOWED_PRINCIPALS: [&str; 3] = [
    "NT AUTHORITY\\SYSTEM",
    "BUILTIN\\ADMINISTRATORS",
    "CREATOR OWNER",
];

/// Whether `icacls` output lists only `user` (and the allowed system principals). The output is the path, then one
/// `DOMAIN\name:(rights)` entry per line (the first shares the path's line). Compiled everywhere so it is tested
/// everywhere; only Windows runs `icacls`.
#[cfg_attr(unix, allow(dead_code))]
pub(crate) fn acl_is_owner_only(icacls_output: &str, path: &str, user: &str) -> bool {
    let user = user.to_ascii_uppercase();
    let mut seen = 0;
    for line in icacls_output.lines() {
        let line = line.trim();
        let line = line.strip_prefix(path).unwrap_or(line).trim();
        if line.is_empty() || line.starts_with("Successfully processed") {
            continue;
        }
        // `DOMAIN\name:(OI)(CI)(F)`: the principal ends at the colon that is followed by a parenthesis.
        let Some(at) = line.find(":(") else { continue };
        let principal = line[..at].trim().to_ascii_uppercase();
        seen += 1;
        let mine = principal == user || principal.rsplit('\\').next() == Some(user.as_str());
        if !mine && !ALLOWED_PRINCIPALS.contains(&principal.as_str()) {
            return false;
        }
    }
    seen > 0
}

#[cfg(not(unix))]
fn user() -> String {
    std::env::var("USERNAME").unwrap_or_default()
}

#[cfg(not(unix))]
fn check_acl(path: &Path) -> Result<()> {
    let out = std::process::Command::new("icacls")
        .arg(path)
        .output()
        .map_err(|e| {
            Error::Permissions(format!(
                "kith-core: can't read the access list of {}: {e}",
                path.display()
            ))
        })?;
    let text = String::from_utf8_lossy(&out.stdout);
    if out.status.success() && acl_is_owner_only(&text, &path.to_string_lossy(), &user()) {
        return Ok(());
    }
    Err(Error::Permissions(format!(
        "kith-core: {} is open to other accounts; kith won't start until it is private: icacls \"{}\" /inheritance:r /grant:r \"{}:(OI)(CI)F\"",
        path.display(),
        path.display(),
        user()
    )))
}

/// Removes inherited entries and grants the owner alone, passed on to every file made inside.
#[cfg(not(unix))]
fn restrict(path: &Path) -> Result<()> {
    let st = std::process::Command::new("icacls")
        .arg(path)
        .args(["/inheritance:r", "/grant:r"])
        .arg(format!("{}:(OI)(CI)F", user()))
        .output()?;
    if st.status.success() {
        Ok(())
    } else {
        Err(Error::Permissions(format!(
            "kith-core: can't make {} private (icacls failed)",
            path.display()
        )))
    }
}
