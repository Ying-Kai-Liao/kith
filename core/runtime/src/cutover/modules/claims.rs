//! The `claims` module (S2, the claims-and-turns lane): the live writer claim files `$KITH_STATE/live/terminal/<sid>.claim`
//! (and a person's session-less `proc-<pid>.claim`) move into the `claims` and `warm_procs` tables, and the claims become the
//! core's: `kith-sessions`, `kith` and `kith-wake` stop writing files and ask the core (`claim.*`), and the host runs its
//! turns in Writer mode. Stepped by `kith migrate step claims` on a Mac with the host stopped.
//!
//! - `preflight`: plain refusals, none of which touches anything (a refused step leaves no snapshot, record, marker or file):
//!   the stub is installed (a run without it takes no writer lock), the host is stopped (it reads the mode at its start), not
//!   Windows, every live claim file is one the import makes a row of, and no host turn sits queued in the core with nothing
//!   to run it. A connected holder cannot be checked here: the step holds the core's lifetime lock, so no daemon is up. The
//!   host in Writer mode waits ("core not answering") until its holder has connected and the generation's claims were
//!   reconciled, and runs nothing unclaimed.
//! - `import`: `import::claims::run_in` under the framework's lock. Read-only on the files, idempotent (a conversation that
//!   already has a live claim row on the file's pid is left alone; a live row on another pid is listed, never replaced).
//! - `same_answer`: every live claim file of a conversation the core knows has a claim row on its pid, every live proc claim a
//!   process row.
//! - The tombstone is `live/terminal/claims.tombstone`, the marker the kernel takers read by existence
//!   (`kernel/lib/kith_claims.py`). It is put in `before_tombstone`, i.e. BEFORE the marker and the switched record: from then
//!   on a taker asks the core, and the core refuses `claim.*` until the module reads switched, so the only thing that can
//!   happen in between is a taker that fails closed, never a claim file nobody reads. The files are re-read once the marker
//!   is down, and a live one that is not a row stops the step (it stays `stepping`; run it again to import).
//! - Rollback (reverse order): refuses while a core claim is live (not overridable: a forced rollback would leave two
//!   writers) or the host runs; then the record says rolling-back (the core stops serving), the marker is removed, and the
//!   record is cleared. No files are written back (the claim files were only read; a claim lives in a process, not a file).
//!   A step cancelled while `stepping` removes the marker too (`cancel_step`).

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use super::schedules::{HostBlock, host_blocker};
use crate::cutover::authority_preflight::{Item, Preflight};
use crate::cutover::{Env, Module, Opts, tombstone};
use crate::db::Core;
use crate::error::{Error, Result};
use crate::holder_link::HolderConfig;
use crate::import::claims::{blockers, differences, run, run_in};

pub(crate) struct Claims;

#[cfg(test)]
pub(super) enum TestStub {
    Nowhere,
    At(PathBuf),
}

#[cfg(test)]
thread_local! {
    /// A test's own answer to "where is the stub", so no test sets a variable the others read.
    pub(super) static STUB: std::cell::RefCell<Option<TestStub>> = const { std::cell::RefCell::new(None) };
}

fn state(env: &Env) -> Result<&Path> {
    env.state.as_deref().ok_or_else(|| {
        Error::Refused("cutover: claims: needs $KITH_STATE (the CLI passes it)".into())
    })
}

/// `$KITH_STATE/live/terminal/claims.tombstone`: the file the kernel takers read (`kith_claims.MARKER`).
fn marker_of(state: &Path) -> PathBuf {
    kith_kernel::threads::registry_dir(state).join("claims.tombstone")
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// core.db for reading (the step holds the lifetime lock, and a read-only handle works beside it, as `check` does). `None`
/// when there is none yet or it is not migrated far enough to hold the tables.
fn read_db(env: &Env) -> Option<Connection> {
    let conn =
        Connection::open_with_flags(&env.layout.db, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('claims', 'warm_procs', 'conversations')",
        [],
        |r| r.get::<_, i64>(0),
    )
    .is_ok_and(|n| n == 3)
    .then_some(conn)
}

const HOST_RUNNING: &str = "the host is running: a Files-mode turn that runs during the step would be imported as an outside writer, and the host's Writer mode starts with it. Stop the host, step claims, start it";

fn host_line(env: &Env, running: &str) -> Option<String> {
    match host_blocker(env)? {
        HostBlock::Running => Some(running.to_owned()),
        HostBlock::Unknown(e) => Some(format!("host.lock cannot be checked: {e}")),
    }
}

/// Why no stub can be run, if so: the one the daemon's terminal finds (`$KITH_STUB_BIN`, then beside kith), and it must be
/// executable. The host also looks in the Mac app's `Resources/kith/bin`, which this does not, so on that layout the step
/// refuses a Mac whose host would find the stub: that fails closed, and `KITH_STUB_BIN` gets past it.
fn stub_line() -> Option<String> {
    #[cfg(test)]
    let found = STUB.with(|s| match &*s.borrow() {
        Some(TestStub::Nowhere) => None,
        Some(TestStub::At(p)) => Some(p.clone()),
        None => HolderConfig::find_stub(None),
    });
    #[cfg(not(test))]
    let found = HolderConfig::find_stub(None);
    let Some(stub) = found else {
        return Some(
            "kith-stub was not found (looked at $KITH_STUB_BIN, then beside kith); without it a run takes no writer lock. On a Mac app install the host also looks in the app's Resources/kith/bin: set KITH_STUB_BIN to the stub there for the step".into(),
        );
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if std::fs::metadata(&stub).is_ok_and(|m| m.permissions().mode() & 0o111 == 0) {
            return Some(format!("{} is not executable", stub.display()));
        }
    }
    None
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

impl Module for Claims {
    fn name(&self) -> &'static str {
        "claims"
    }

    fn tombstones(&self, env: &Env) -> Vec<PathBuf> {
        env.state.iter().map(|s| marker_of(s)).collect()
    }

    fn preflight(
        &self,
        env: &Env,
        _import: Option<&Result<Vec<String>>>,
    ) -> Result<Option<Preflight>> {
        let state = state(env)?;
        let db = read_db(env);
        let items = vec![
            item(
                1,
                "the kith-stub program is installed",
                stub_line().into_iter().collect(),
            ),
            item(
                2,
                "the host is stopped",
                host_line(env, HOST_RUNNING).into_iter().collect(),
            ),
            item(
                3,
                "this is not Windows",
                if cfg!(windows) {
                    vec!["the claims step is not available on Windows".into()]
                } else {
                    Vec::new()
                },
            ),
            item(
                4,
                "live claim files the import cannot make a core claim of",
                blockers(state, db.as_ref())?,
            ),
        ];
        Ok(Some(Preflight {
            items,
            notes: db.as_ref().and_then(strays_note).into_iter().collect(),
        }))
    }

    fn rollback_refusal(&self, env: &Env) -> Option<String> {
        // Not a `live` reason: `--force` must not get past it. A running host reads its mode at every begin and would meet a
        // half-rolled-back claims state; stop it, roll back, start it. (A live claim is refused under the lock, below.)
        host_line(
            env,
            "the host is running; stop it, roll back, start it; --force does not override this",
        )
    }

    fn before_rollback(&self, _env: &Env, core: &mut Core) -> Result<()> {
        // With no daemon nothing ends a claim whose process is gone, so a stopped host's dead turn would block the rollback
        // for ever. Only a process confirmed gone is ended.
        crate::conversations::claims::end_gone(
            core,
            &crate::gate::System,
            crate::holder_link::now_ms(),
        )?;
        Ok(())
    }

    fn rollback_lock(&self, env: &Env) -> Result<Option<crate::cutover::SourceLock>> {
        // Under the lifetime lock, not overridable by `--force` (which skips the generic check): a core claim is a running
        // writer that a Files-mode taker cannot see. Writes nothing when it refuses.
        let Some(db) = read_db(env) else {
            return Ok(None);
        };
        let mut st = db.prepare(
            "SELECT conversation, kind, pid FROM claims WHERE state != 'ended' ORDER BY claim_id LIMIT 6",
        )?;
        let live: Vec<String> = st
            .query_map([], |r| {
                Ok(format!(
                    "{} ({}, pid {})",
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<i64>>(2)?
                        .map_or_else(|| "none".to_owned(), |p| p.to_string())
                ))
            })?
            .collect::<std::result::Result<_, _>>()?;
        if live.is_empty() {
            return Ok(None);
        }
        Err(Error::Refused(format!(
            "cutover: claims: not rolling back while a core claim is live ({}); a rollback would leave a writer the files cannot see. End those runs, then roll back; --force does not override this",
            live.join(", ")
        )))
    }

    fn cancel_step(&self, env: &Env) -> Result<Vec<String>> {
        // A step that stopped after the marker was put and before the switch: the takers would fail closed for ever.
        remove_marker(&marker_of(state(env)?))
    }

    fn check_notes(&self, env: &Env) -> Vec<String> {
        let mut out = vec![
            "claims: Writer mode needs the host's holder connected to the core (start `kith serve`; `kith kernel core-call claim.list` answers once the core serves): until then the host waits (core not answering) and runs nothing unclaimed".to_owned(),
        ];
        out.extend(read_db(env).as_ref().and_then(strays_note));
        out
    }

    fn before_tombstone(&self, env: &Env) -> Result<()> {
        let state = state(env)?;
        // A host started since the preflight sees no switch file and no marker, and starts Files-mode turns.
        if let Some(why) = host_line(env, HOST_RUNNING) {
            return Err(Error::Refused(format!(
                "cutover: claims: {why}; NOT switched"
            )));
        }
        // First, so a taker that starts from here on asks the core (which refuses until the switch) and writes no file.
        tombstone::ensure(&marker_of(state), self.name(), now_secs())?;
        let db = read_db(env).ok_or_else(|| {
            Error::Refused(
                "cutover: claims: core.db cannot be read to check the claim files".into(),
            )
        })?;
        let mut left = blockers(state, Some(&db))?;
        left.extend(differences(state, &db)?);
        if left.is_empty() {
            return Ok(());
        }
        Err(Error::Refused(format!(
            "cutover: claims: a live claim appeared or cannot be imported while the step ran ({}); NOT switched. Run `kith migrate step claims` again, or `kith rollback claims`",
            left.join(" | ")
        )))
    }

    fn import(&self, env: &Env, core: Option<&mut Core>, opts: Opts) -> Result<Vec<String>> {
        let state = state(env)?;
        let report = match core {
            Some(core) => run_in(core, state, opts.dry_run)?,
            None => run(&env.layout, state, true)?,
        };
        Ok(report.lines)
    }

    fn same_answer(&self, env: &Env, db: &Connection) -> Result<Vec<String>> {
        differences(state(env)?, db)
    }

    fn export_back(&self, env: &Env, _core: &mut Core) -> Result<Vec<String>> {
        // Nothing to write back: the claim files were only read, and the rollback refused while a claim was live.
        let mut lines = remove_marker(&marker_of(state(env)?))?;
        lines.push(
            "claims: no files to write back (a claim is a running process; none was live)".into(),
        );
        Ok(lines)
    }
}

fn remove_marker(m: &Path) -> Result<Vec<String>> {
    match std::fs::remove_file(m) {
        Ok(()) => Ok(vec![format!("claims: removed {}", m.display())]),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(Error::Io(format!("cutover: claims: {}: {e}", m.display()))),
    }
}

/// Host turns the core still holds as queued or parked: the turns a host enqueued (they carry its key) that nothing runs. A
/// stopped host runs none, so every one is a stray, and the host's first sweep cancels them (`terminal::core_strays`).
/// Informational, never a refusal: the owner has no command that clears them.
fn strays_note(db: &Connection) -> Option<String> {
    let n: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM turns WHERE enq_key IS NOT NULL AND state IN ('queued', 'parked')",
            [],
            |r| r.get(0),
        )
        .ok()?;
    (n > 0).then(|| {
        format!("{n} host turn(s) are queued in the core; the host's first sweep after the step cancels them (nothing runs them)")
    })
}
