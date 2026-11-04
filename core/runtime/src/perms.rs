//! Owner-only files. The core folder is 0700 and core.db (+ -wal, -shm), core.lock, core.holder and the marker 0600, refused at
//! start if looser, with the command that fixes it. On Windows the same rule is an access list with no entry for
//! anyone but the owner, SYSTEM and Administrators (read with `icacls`).
//!
//! TODO(windows): read the access list natively (`GetNamedSecurityInfoW` and the DACL's entries, through windows-sys)
//! instead of running `icacls`, as the holder's start time on macOS no longer runs `ps`. Left: it can only be written
//! and tried on Windows, and `acl_is_owner_only` below is the rule the native read would feed.

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

/// What reading one access list came to.
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(unix, allow(dead_code))]
enum Verdict {
    Private,
    Open,
    /// `icacls` failed and the file is gone (a real `NotFound`), and it is one of `SQLite`'s sidecars: they are deleted
    /// when the last connection closes, which can happen between the existence check and the read. Nothing to protect.
    Vanished,
    /// `icacls` failed and the file is not provably gone: still there, a handle opening, access denied, a dangling
    /// link, or any file other than a sidecar that went missing. Worth another try, then refused. (A delete-pending
    /// file under legacy delete semantics fails the metadata call too, so it reads as gone, i.e. Vanished for a
    /// sidecar.)
    Unreadable,
}

/// `SQLite`'s -wal and -shm: the only files allowed to vanish mid-check.
#[cfg_attr(unix, allow(dead_code))]
fn is_sidecar(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with("-wal") || n.ends_with("-shm"))
}

#[cfg_attr(unix, allow(dead_code))]
fn judge(
    read_ok: bool,
    text: &str,
    path: &str,
    user: &str,
    still_exists: bool,
    allow_vanish: bool,
) -> Verdict {
    if read_ok {
        if acl_is_owner_only(text, path, user) {
            Verdict::Private
        } else {
            Verdict::Open
        }
    } else if still_exists || !allow_vanish {
        Verdict::Unreadable
    } else {
        Verdict::Vanished
    }
}

#[cfg(not(unix))]
fn check_acl(path: &Path) -> Result<()> {
    let allow_vanish = is_sidecar(path);
    // A -shm being created or torn down by another connection answers `icacls` with an error for a moment; a real
    // open list never goes away, so a few short retries cost nothing on the normal path.
    let mut unreadable = false;
    for attempt in 0..5 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        let out = std::process::Command::new("icacls").arg(path).output();
        let verdict = match out {
            Ok(o) => judge(
                o.status.success(),
                &String::from_utf8_lossy(&o.stdout),
                &path.to_string_lossy(),
                &user(),
                // Only a real NotFound is gone; access denied or a dangling link is not.
                !matches!(fs::symlink_metadata(path), Err(e) if e.kind() == std::io::ErrorKind::NotFound),
                allow_vanish,
            ),
            Err(e) => {
                return Err(Error::Permissions(format!(
                    "kith-core: can't read the access list of {}: {e}",
                    path.display()
                )));
            }
        };
        match verdict {
            Verdict::Private | Verdict::Vanished => return Ok(()),
            Verdict::Open => {
                unreadable = false;
                break;
            }
            Verdict::Unreadable => unreadable = true,
        }
    }
    if unreadable {
        return Err(Error::Permissions(format!(
            "kith-core: can't read the access list of {}; kith won't start until it can: check that the file is there and readable by you (icacls \"{}\")",
            path.display(),
            path.display()
        )));
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const OWNER_ONLY: &str = "C:\\m\\core.db-shm HOST\\me:(F)\n NT AUTHORITY\\SYSTEM:(F)\n\nSuccessfully processed 1 files; Failed processing 0 files\n";

    #[test]
    fn a_sidecar_deleted_while_being_read_is_not_an_open_file() {
        assert_eq!(judge(false, "", "x", "me", false, true), Verdict::Vanished);
        assert_eq!(judge(false, "", "x", "me", true, true), Verdict::Unreadable);
    }

    #[test]
    fn only_sidecars_may_vanish() {
        assert_eq!(
            judge(false, "", "x", "me", false, false),
            Verdict::Unreadable
        );
        assert!(is_sidecar(Path::new("C:\\m\\core.db-wal")));
        assert!(is_sidecar(Path::new("C:\\m\\core.db-shm")));
        for n in [
            "core.db",
            "core.lock",
            "core.holder",
            "core.token",
            "snap.db",
        ] {
            assert!(!is_sidecar(Path::new(n)), "{n}");
        }
    }

    #[test]
    fn a_readable_list_is_still_judged_by_its_entries() {
        assert_eq!(
            judge(true, OWNER_ONLY, "C:\\m\\core.db-shm", "me", true, true),
            Verdict::Private
        );
        let open = format!("{OWNER_ONLY} BUILTIN\\Users:(R)\n");
        assert_eq!(
            judge(true, &open, "C:\\m\\core.db-shm", "me", true, true),
            Verdict::Open
        );
    }

    /// The order from the CI flake: one connection creating and dropping the sidecars while the folder is checked.
    #[cfg(windows)]
    #[test]
    fn checking_while_another_connection_makes_and_removes_sidecars_passes() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("core");
        ensure_dir(&dir).unwrap();
        let db = dir.join("core.db");
        let churn_db = db.clone();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = stop.clone();
        let t = std::thread::spawn(move || {
            while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                let c = rusqlite::Connection::open(&churn_db).unwrap();
                c.pragma_update(None, "journal_mode", "WAL").unwrap();
                c.execute_batch("CREATE TABLE IF NOT EXISTS t(x)").unwrap();
            }
        });
        for _ in 0..40 {
            for f in ["core.db", "core.db-wal", "core.db-shm"] {
                let p = dir.join(f);
                if p.exists() {
                    check_file(&p).unwrap();
                }
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        t.join().unwrap();
    }
}
