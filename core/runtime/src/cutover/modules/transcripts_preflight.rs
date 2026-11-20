//! The transcripts step's preflight: plain refusals, none of which touches anything (a refused step leaves no snapshot,
//! record, marker or file). It also runs on every rerun of a step that stopped with the tombstone in place, so it must not
//! refuse a state the step's own first writes create (entries the tombstone made hooks spool: architect MUST 1).

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use super::schedules::{HostBlock, host_blocker};
use crate::cutover::Env;
use crate::cutover::authority_preflight::{Item, Preflight};
use crate::error::Result;

pub(super) const HOST_RUNNING: &str = "the host is running: a hook that starts during the step could be answered by a core that is not serving yet. Stop the host, step transcripts, start it";

#[cfg(test)]
thread_local! {
    /// A test's own answer to "what does the installed kith-sessions say for `caps`".
    pub(super) static CAPS: std::cell::RefCell<Option<std::result::Result<String, String>>> = const { std::cell::RefCell::new(None) };
}

pub(super) fn host_line(env: &Env, running: &str) -> Option<String> {
    match host_blocker(env)? {
        HostBlock::Running => Some(running.to_owned()),
        HostBlock::Unknown(e) => Some(format!("host.lock cannot be checked: {e}")),
    }
}

/// The kernel whose `bin/kith-sessions` the `SessionStart` hook runs: `$KITH` (set inside `bin/kith`, or by the caller),
/// else the host's own order (`serve/host/src/kernel.rs` `find_kernel`, minus the binary-relative prefix): `$KITH_KERNEL`,
/// the installed CLI's `cli/current` the installer links, the old flat install. Neither `kith kernel cutover` nor
/// `kith migrate` sets `$KITH`, so a shell without it must still find the same file.
fn kernel_for_hook(
    kith: Option<std::ffi::OsString>,
    kernel: Option<std::ffi::OsString>,
    machine: &Path,
) -> std::result::Result<PathBuf, String> {
    let set = |v: Option<std::ffi::OsString>| v.filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(k) = set(kith) {
        return Ok(k);
    }
    let shipped = "share/kith/kernel";
    // Like `find_kernel`, a set `$KITH_KERNEL` is the only candidate: it never falls through to the installed CLI.
    let tried: Vec<PathBuf> = match set(kernel) {
        Some(k) if !k.join("bin/kith-sessions").is_file() => {
            return Err(format!(
                "$KITH is not set and $KITH_KERNEL={} has no bin/kith-sessions, so the kith-sessions the SessionStart hook runs cannot be found (set $KITH=<kernel dir>, or point $KITH_KERNEL at a kernel)",
                k.display()
            ));
        }
        Some(k) => vec![k],
        None => vec![
            machine.join("cli/current").join(shipped),
            machine.join(shipped),
        ],
    };
    tried
        .iter()
        .find(|d| d.join("bin/kith-sessions").is_file())
        .cloned()
        .ok_or_else(|| {
            let looked: Vec<String> = tried.iter().map(|d| d.display().to_string()).collect();
            format!(
                "$KITH is not set and no kernel with bin/kith-sessions is installed (looked in {}), so the kith-sessions the SessionStart hook runs cannot be found (set $KITH=<kernel dir> to name it)",
                looked.join(", ")
            )
        })
}

/// What `<kernel>/bin/kith-sessions caps` prints: the very path `bin/kith` writes into the `SessionStart` hook, with no search
/// of `PATH` (a copy found there could say `registry-core` for a hook that never runs it). `Err(why)` when it cannot be asked.
fn caps(machine: &Path) -> std::result::Result<String, String> {
    #[cfg(test)]
    if let Some(c) = CAPS.with(|c| c.borrow().clone()) {
        return c;
    }
    caps_of(&kernel_for_hook(
        std::env::var_os("KITH"),
        std::env::var_os("KITH_KERNEL"),
        machine,
    )?)
}

fn caps_of(kith: &Path) -> std::result::Result<String, String> {
    let bin = kith.join("bin").join("kith-sessions");
    let out = std::process::Command::new(&bin)
        .arg("caps")
        .output()
        .map_err(|e| format!("{} cannot be run: {e}", bin.display()))?;
    if !out.status.success() {
        return Err(format!("{} caps failed", bin.display()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn sessions_line(machine: &Path) -> Option<String> {
    match caps(machine) {
        Ok(out) if out.lines().any(|l| l.trim() == "registry-core") => None,
        Ok(_) => Some(
            "the installed kith-sessions does not know Writer mode (`kith-sessions caps` lacks registry-core); update this Mac's kith first"
                .into(),
        ),
        Err(why) => Some(why),
    }
}

/// Why core.db cannot take the step: it exists but is not migrated to `session_records`. No core.db is fine (the step makes
/// it). In a real step the lifetime lock has migrated core.db before this runs, so the item matters in a dry run.
fn table_line(env: &Env) -> Option<String> {
    if !env.layout.db.exists() {
        return None;
    }
    let conn =
        Connection::open_with_flags(&env.layout.db, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'session_records'",
            [],
            |r| r.get(0),
        )
        .ok()?;
    (n == 0).then(|| "core.db has no session_records table yet (the step's first open migrates it; this dry run cannot)".into())
}

fn item(n: u8, what: &'static str, ids: Vec<String>) -> Item {
    Item {
        n,
        what,
        ids,
        stops: true,
        skipped: None,
    }
}

pub(super) fn preflight(env: &Env) -> Result<Preflight> {
    super::transcripts::state(env)?;
    let items = vec![
        item(
            1,
            "the host is stopped",
            host_line(env, HOST_RUNNING).into_iter().collect(),
        ),
        item(
            2,
            "this is not Windows",
            if cfg!(windows) {
                vec!["the transcripts step is not available on Windows".into()]
            } else {
                Vec::new()
            },
        ),
        item(
            3,
            "the installed kith-sessions knows Writer mode",
            sessions_line(&env.machine).into_iter().collect(),
        ),
        item(
            4,
            "core.db is migrated to session_records",
            table_line(env).into_iter().collect(),
        ),
    ];
    Ok(Preflight {
        items,
        notes: vec![
            "spool entries in live/terminal/inbox are drained by the step's re-read, not refused"
                .into(),
        ],
    })
}

#[cfg(test)]
#[cfg(unix)]
#[allow(clippy::unwrap_used)]
mod kernel_tests {
    use super::*;

    fn fake_kernel(dir: &Path, says: &str) {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let f = bin.join("kith-sessions");
        std::fs::write(&f, format!("#!/bin/sh\necho {says}\n")).unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn unset_kith_resolves_the_installed_cli() {
        let t = tempfile::tempdir().unwrap();
        let k = t.path().join("cli/current/share/kith/kernel");
        fake_kernel(&k, "registry-core");
        let got = kernel_for_hook(None, None, t.path()).unwrap();
        assert_eq!(got, k);
        assert_eq!(caps_of(&got).unwrap().trim(), "registry-core");
        // An empty $KITH counts as unset.
        assert_eq!(kernel_for_hook(Some("".into()), None, t.path()).unwrap(), k);
    }

    #[test]
    fn kithkernel_alone_when_set_and_the_flat_install_is_found() {
        let t = tempfile::tempdir().unwrap();
        let flat = t.path().join("share/kith/kernel");
        fake_kernel(&flat, "x");
        assert_eq!(kernel_for_hook(None, None, t.path()).unwrap(), flat);
        fake_kernel(&t.path().join("cli/current/share/kith/kernel"), "x");
        // cli/current beats the flat install.
        assert_ne!(kernel_for_hook(None, None, t.path()).unwrap(), flat);
        // $KITH_KERNEL without a kith-sessions is refused, never replaced by the layout.
        let dev = t.path().join("dev");
        let e = kernel_for_hook(None, Some(dev.clone().into()), t.path()).unwrap_err();
        assert!(
            e.contains(&dev.display().to_string())
                && e.contains("$KITH_KERNEL=")
                && e.contains("has no bin/kith-sessions")
                && !e.contains("no kernel with bin/kith-sessions is installed")
                && !e.contains("cli/current"),
            "{e}"
        );
        fake_kernel(&dev, "x");
        assert_eq!(
            kernel_for_hook(None, Some(dev.clone().into()), t.path()).unwrap(),
            dev
        );
    }

    #[test]
    fn kith_wins_when_set() {
        let t = tempfile::tempdir().unwrap();
        fake_kernel(&t.path().join("cli/current/share/kith/kernel"), "x");
        let got = kernel_for_hook(Some("/elsewhere".into()), None, t.path()).unwrap();
        assert_eq!(got, PathBuf::from("/elsewhere"));
    }

    #[test]
    fn a_missing_kernel_refuses_naming_what_it_looked_for() {
        let t = tempfile::tempdir().unwrap();
        let e = kernel_for_hook(None, None, t.path()).unwrap_err();
        assert!(e.contains("$KITH is not set"), "{e}");
        assert!(e.contains("cli/current/share/kith/kernel"), "{e}");
    }
}
