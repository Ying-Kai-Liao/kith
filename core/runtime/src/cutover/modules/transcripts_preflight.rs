//! The transcripts step's preflight: plain refusals, none of which touches anything (a refused step leaves no snapshot,
//! record, marker or file). It also runs on every rerun of a step that stopped with the tombstone in place, so it must not
//! refuse a state the step's own first writes create (entries the tombstone made hooks spool: architect MUST 1).

use std::path::PathBuf;

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

/// What `$KITH/bin/kith-sessions caps` prints: the very path `bin/kith` writes into the `SessionStart` hook, with no search of
/// `PATH` (a copy found there could say `registry-core` for a hook that never runs it). `Err(why)` when it cannot be asked.
fn caps() -> std::result::Result<String, String> {
    #[cfg(test)]
    if let Some(c) = CAPS.with(|c| c.borrow().clone()) {
        return c;
    }
    let Some(kith) = std::env::var_os("KITH").filter(|v| !v.is_empty()) else {
        return Err(
            "$KITH is not set, so the kith-sessions the SessionStart hook runs cannot be found"
                .into(),
        );
    };
    let bin = PathBuf::from(kith).join("bin").join("kith-sessions");
    let out = std::process::Command::new(&bin)
        .arg("caps")
        .output()
        .map_err(|e| format!("{} cannot be run: {e}", bin.display()))?;
    if !out.status.success() {
        return Err(format!("{} caps failed", bin.display()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn sessions_line() -> Option<String> {
    match caps() {
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
            sessions_line().into_iter().collect(),
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
