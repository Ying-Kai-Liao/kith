//! Owner-only files. The core folder is 0700 and core.db (+ -wal, -shm), core.lock, core.holder and the marker 0600, refused at
//! start if looser, with the command that fixes it. On Windows the same rule is an access list with no entry for
//! anyone but the owner, SYSTEM and Administrators, read and written natively by SID (`kith-winsys`: no `icacls` text, no
//! account names, no `USERNAME`, so it means the same on every display language).

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
    kith_winsys::acl::restrict(dir, true).map_err(|e| {
        Error::Permissions(format!(
            "kith-core: can't make {} private: {e}",
            dir.display()
        ))
    })
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
    use std::os::unix::fs::PermissionsExt as _;
    // A sidecar another connection deleted since the listing is nothing to protect; anything else gone is an error.
    let m = match fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && is_sidecar(path) => return Ok(()),
        r => r?.permissions().mode() & 0o777,
    };
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

/// `SQLite`'s `<name>.db-wal` and `<name>.db-shm`: the only files allowed to vanish mid-check.
fn is_sidecar(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(".db-wal") || n.ends_with(".db-shm"))
}

/// Whether a list that could not be read means the file is gone. Only a sidecar may vanish. `meta` is the kind of
/// error a fresh metadata call gave (None when it succeeded). A real `NotFound` is gone. A file mid-delete on Windows
/// (another connection closed its handle, delete pending) answers both the security read and the metadata call with
/// access denied: that pair is gone too, but access denied on the read alone (a file we can't see into, which metadata
/// still describes) is not.
#[cfg_attr(unix, allow(dead_code))]
fn vanished(sidecar: bool, read: std::io::ErrorKind, meta: Option<std::io::ErrorKind>) -> bool {
    use std::io::ErrorKind::{NotFound, PermissionDenied};
    sidecar
        && match meta {
            Some(NotFound) => true,
            Some(PermissionDenied) => read == PermissionDenied,
            _ => false,
        }
}

#[cfg(not(unix))]
fn check_acl(path: &Path) -> Result<()> {
    use kith_winsys::acl::{self, Check};
    let sidecar = is_sidecar(path);
    // A -shm being created or torn down by another connection fails the read for a moment; a real open list never
    // goes away, so a few short retries cost nothing on the normal path.
    let mut last = None;
    for attempt in 0..10 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        match acl::check(path) {
            Ok(Check::OwnerOnly) => return Ok(()),
            Ok(Check::Open(why)) => {
                // The fix names the owner by SID (`*S-1-5-...`), which icacls takes in any language.
                let fix = acl::current_user_text().map_or_else(
                    |_| String::new(),
                    |sid| {
                        let inherit = if path.is_dir() { "(OI)(CI)" } else { "" };
                        format!(": icacls \"{}\" /inheritance:r /grant:r \"*{sid}:{inherit}F\"", path.display())
                    },
                );
                return Err(Error::Permissions(format!(
                    "kith-core: {} is open to other accounts ({why}); kith won't start until it is private{fix}",
                    path.display()
                )));
            }
            Err(e) => {
                let meta = fs::symlink_metadata(path).err().map(|m| m.kind());
                if vanished(sidecar, e.kind(), meta) {
                    return Ok(());
                }
                last = Some(e);
            }
        }
    }
    let why = last.map_or_else(String::new, |e| format!(": {e}"));
    Err(Error::Permissions(format!(
        "kith-core: can't read the access list of {}; kith won't start until it can: check that the file is there and readable by you{why}",
        path.display()
    )))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_sidecar_deleted_while_being_read_is_not_an_open_file() {
        use std::io::ErrorKind::{NotFound, Other, PermissionDenied};
        assert!(vanished(true, Other, Some(NotFound)));
        // Delete pending: both calls say access denied.
        assert!(vanished(true, PermissionDenied, Some(PermissionDenied)));
        // Still described by a metadata call: a list we can't read, not a vanished file.
        assert!(!vanished(true, PermissionDenied, None));
        assert!(!vanished(true, NotFound, None));
        // Metadata fails but the read did not say access denied: not provably gone.
        assert!(!vanished(true, Other, Some(PermissionDenied)));
    }

    #[test]
    fn only_sidecars_may_vanish() {
        use std::io::ErrorKind::NotFound;
        assert!(!vanished(false, NotFound, Some(NotFound)));
        assert!(is_sidecar(Path::new("C:\\m\\core.db-wal")));
        assert!(is_sidecar(Path::new("C:\\m\\core.db-shm")));
        for n in [
            "core.db",
            "core.lock",
            "core.holder",
            "core.token",
            "snap.db",
            "core.db-journal",
            "core.db-wal.bak",
            "x-wal",
            "x-shm",
            "-wal",
            "core.db-shm.tmp",
        ] {
            assert!(!is_sidecar(Path::new(n)), "{n}");
        }
    }

    /// The Unix side of the same race: listed, then deleted before the stat.
    #[cfg(unix)]
    #[test]
    fn a_sidecar_deleted_before_the_mode_read_passes_but_other_files_do_not() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(check_file(&tmp.path().join("core.db-wal")).is_ok());
        assert!(check_file(&tmp.path().join("core.db-shm")).is_ok());
        assert!(check_file(&tmp.path().join("core.db")).is_err());
    }

    /// A folder made by `ensure_dir` and a file made inside it pass; the same folder with Users let in is refused.
    #[cfg(windows)]
    #[test]
    fn a_new_core_folder_is_owner_only_and_an_added_entry_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("core");
        ensure_dir(&dir).unwrap();
        let db = dir.join("core.db");
        fs::write(&db, b"x").unwrap();
        check_files_in(&dir, &db);
        let out = std::process::Command::new("icacls")
            .arg(&dir)
            .args(["/grant", "*S-1-5-32-545:(OI)(CI)(R)"])
            .output()
            .unwrap();
        assert!(out.status.success());
        let err = check_dir(&dir).unwrap_err().to_string();
        assert!(err.contains("open to other accounts"), "{err}");
        assert!(err.contains("S-1-5-32-545"), "{err}");
    }

    #[cfg(windows)]
    fn check_files_in(dir: &Path, file: &Path) {
        check_dir(dir).unwrap();
        check_file(file).unwrap();
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
