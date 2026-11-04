//! The wake queue's files into core.db (M3-7): `$KITH_STATE/sessions/queue/*.json` (one per session owed something),
//! `queue/answered.json` (the ids answered or withdrawn, kept seven days) and `sessions/waits/*.json` (one per waiter), the
//! files `kernel/bin/kith-wake` writes, into the `wake_sessions`, `wake_items`, `wake_answered` and `waiters` tables.
//!
//! | file                                | core.db                                                                  |
//! |-------------------------------------|--------------------------------------------------------------------------|
//! | `queue/<hash>.json` (`v` 1)         | `wake_sessions` row, its `requests` as `wake_items` rows (in order)      |
//! |   `turn` (`run`, `pid`, `procStart`, `at`, `sid`) | `turn_*` columns: the turn in flight stays in flight          |
//! |   `tries`, `next`                   | the retry state, kept (a retry in the future stays in the future)        |
//! | `queue/answered.json`               | `wake_answered` (`how`, `t`, `run`), every entry, expired ones too       |
//! | `waits/<id>.json` (`v` 1)           | `waiters` row (`kind` ci / pid / at; `seen`, `checked`, `until` kept)    |
//!
//! What it promises:
//! - Read-only on the sources. The files are only ever read (a shared lock on `queue/.lock` and `waits/.lock`, when they
//!   exist, so a read does not see half a change of the Python; `kith-wake` takes them exclusively).
//! - A file that is not an item (damaged JSON, no session, a waiter the Python could not read) is skipped and listed, as
//!   `kith-wake ls` skips it. A file whose `v` is not 1, an item with a door that is neither `run` nor `host`, a session
//!   in two files or a request id in two items REFUSES the whole import with the file named: nothing is guessed.
//! - `--dry-run` opens nothing for writing (core.db read-only, no lock) and says what would change.
//! - A real run holds the core's lifetime lock and writes in one transaction: the rows and a row in `import_log`.
//! - Idempotent: when the tables already hold exactly what the files say, nothing is written, not even a log row. A table
//!   that differs is made to match the files (Python is the writer until the switch, so the files are the truth).
//! - It does not make core.db authoritative: `kith-wake` and the host keep reading and writing the files.

use std::collections::BTreeSet;
use std::path::Path;

use rusqlite::{Connection, OpenFlags, params};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::db::{Core, MIGRATIONS};
use crate::error::{Error, Result};
use crate::layout::Layout;
use crate::wake::{Answered, Item, State, Waiter, counts, load, save};

/// What an import did, a line each.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Report {
    pub dry_run: bool,
    /// Whether core.db changed, or in a dry run would.
    pub changed: bool,
    pub lines: Vec<String>,
}

fn refuse(file: &str, why: &str) -> Error {
    Error::Refused(format!("wake import: {file}: {why}; nothing was imported"))
}

/// The `*.json` files of a folder, sorted, with their bytes. Nothing there is not an error.
fn json_files(dir: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::Io(format!("wake import: {}: {e}", dir.display()))),
    };
    let mut names: Vec<String> = rd
        .filter_map(std::result::Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            !n.starts_with('.')
                && Path::new(n)
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        })
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|n| {
            std::fs::read(dir.join(&n))
                .map(|b| (n.clone(), b))
                .map_err(|e| Error::Io(format!("wake import: {}: {e}", dir.join(&n).display())))
        })
        .collect()
}

/// A shared lock on `dir/.lock`, if there is one: the Python holds it exclusively while it changes the files.
fn share_lock(dir: &Path) -> Option<std::fs::File> {
    let f = std::fs::File::open(dir.join(".lock")).ok()?;
    fs2::FileExt::lock_shared(&f).ok()?;
    Some(f)
}

/// What the files say, the lines for what was skipped, and a hash of what was read.
pub(crate) fn read_files(state: &Path) -> Result<(State, Vec<String>, String)> {
    let queue = state.join("sessions").join("queue");
    let waits = state.join("sessions").join("waits");
    // The Python's order: the waiters' lock, then the queue's.
    let _w = share_lock(&waits);
    let _q = share_lock(&queue);
    let mut st = State::default();
    let mut skipped = Vec::new();
    let mut hash = Sha256::new();
    let mut ids = BTreeSet::new();
    for (name, bytes) in json_files(&queue)? {
        hash.update(format!("queue/{name}\0").as_bytes());
        hash.update(&bytes);
        let parsed = serde_json::from_slice::<Value>(&bytes).ok();
        if name == "answered.json" {
            for (id, v) in parsed
                .as_ref()
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
            {
                if let Some(a) = Answered::from_json(v) {
                    st.answered.insert(id.clone(), a);
                }
            }
            continue;
        }
        let item = match parsed.as_ref().map(Item::from_json) {
            None => {
                skipped.push(format!("queue/{name}: not JSON, skipped"));
                continue;
            }
            Some(Err(why)) => return Err(refuse(&format!("queue/{name}"), &why)),
            Some(Ok(None)) => {
                skipped.push(format!("queue/{name}: not a queue item, skipped"));
                continue;
            }
            Some(Ok(Some(item))) => item,
        };
        if st.items.contains_key(&item.session) {
            return Err(refuse(
                &format!("queue/{name}"),
                &format!("session {} is in another file too", item.session),
            ));
        }
        for r in &item.requests {
            if !ids.insert(r.id.clone()) {
                return Err(refuse(
                    &format!("queue/{name}"),
                    &format!("request {} is queued twice", r.id),
                ));
            }
        }
        st.items.insert(item.session.clone(), item);
    }
    for (name, bytes) in json_files(&waits)? {
        hash.update(format!("waits/{name}\0").as_bytes());
        hash.update(&bytes);
        let waiter = match serde_json::from_slice::<Value>(&bytes)
            .ok()
            .as_ref()
            .map(Waiter::from_json)
        {
            None => {
                skipped.push(format!("waits/{name}: not JSON, skipped"));
                continue;
            }
            Some(Err(why)) => return Err(refuse(&format!("waits/{name}"), &why)),
            Some(Ok(None)) => {
                skipped.push(format!("waits/{name}: not a waiter, skipped"));
                continue;
            }
            Some(Ok(Some(w))) => w,
        };
        if st.waiters.contains_key(&waiter.id) {
            return Err(refuse(
                &format!("waits/{name}"),
                &format!("waiter {} is in another file too", waiter.id),
            ));
        }
        st.waiters.insert(waiter.id.clone(), waiter);
    }
    let hex = hash.finalize().iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
        s
    });
    Ok((st, skipped, hex))
}

/// The step: `state` is `$KITH_STATE`. A real run opens the core (the lifetime lock, migrating an older schema).
pub(crate) fn run(layout: &Layout, state: &Path, dry_run: bool) -> Result<Report> {
    let (desired, skipped, sha) = read_files(state)?;
    let mut rep = Report {
        dry_run,
        changed: false,
        lines: skipped,
    };
    if dry_run {
        let db = Connection::open_with_flags(&layout.db, OpenFlags::SQLITE_OPEN_READ_ONLY).ok();
        let ready = db.filter(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'wake_sessions'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .is_ok_and(|n| n > 0)
        });
        let current = if let Some(c) = &ready {
            load(c)?
        } else {
            rep.lines.push(
                "core.db is missing or not migrated yet: a real run would migrate it first".into(),
            );
            State::default()
        };
        summarize(&mut rep, &current, &desired);
        rep.lines.push("dry run: nothing was written".into());
        return Ok(rep);
    }
    let mut core = crate::start_with(layout, MIGRATIONS)?;
    into(&mut core, state, &desired, &sha, &mut rep)?;
    Ok(rep)
}

/// [`run`] over an open core (a test holds one).
#[cfg(test)]
pub(crate) fn run_in(core: &mut Core, state: &Path, dry_run: bool) -> Result<Report> {
    let (desired, skipped, sha) = read_files(state)?;
    let mut rep = Report {
        dry_run,
        changed: false,
        lines: skipped,
    };
    if dry_run {
        let current = load(core.conn())?;
        summarize(&mut rep, &current, &desired);
        return Ok(rep);
    }
    into(core, state, &desired, &sha, &mut rep)?;
    Ok(rep)
}

fn summarize(rep: &mut Report, current: &State, desired: &State) {
    if current == desired {
        rep.lines.push(format!(
            "wake: core.db already holds exactly the files' rows ({}); nothing to change",
            counts(desired)
        ));
    } else {
        rep.lines.push(format!(
            "wake: would {} core.db's rows with the files' ({})",
            if *current == State::default() {
                "import"
            } else {
                "REPLACE"
            },
            counts(desired)
        ));
        rep.changed = true;
    }
}

fn into(core: &mut Core, state: &Path, desired: &State, sha: &str, rep: &mut Report) -> Result<()> {
    let tx = core.claim_tx()?;
    let current = load(&tx)?;
    if current == *desired {
        rep.lines.push(format!(
            "wake: nothing to change: core.db already holds the files' rows ({})",
            counts(desired)
        ));
        return Ok(());
    }
    save(&tx, &current, desired)?;
    // Read back what was stored: the rows must say what the files said.
    if load(&tx)? != *desired {
        return Err(Error::Refused(
            "wake import: the stored rows do not read back as the files'; nothing was imported"
                .into(),
        ));
    }
    tx.execute(
        "INSERT INTO import_log (step, at, source, source_sha256, backup, summary) VALUES ('wake', ?1, ?2, ?3, NULL, ?4)",
        params![
            super::now_secs(),
            state.join("sessions").display().to_string(),
            sha,
            format!("wake: {}", counts(desired)),
        ],
    )?;
    tx.commit()?;
    rep.changed = true;
    rep.lines.push(format!(
        "wake: {} core.db's rows with the files' ({}). core.db is a copy: kith-wake and the host still read and write the files (the switch is a later step)",
        if current == State::default() { "imported into" } else { "replaced" },
        counts(desired)
    ));
    Ok(())
}
