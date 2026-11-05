//! `kith migrate`'s authority step (M2-8): copies auth.json's access tables into core.db, and the Keychain items its profiles
//! name into the secrets envelope, once, safely.
//!
//! What it promises:
//! - Read-only on the sources: auth.json, serve.db and the Keychain are only ever read.
//! - `--dry-run` opens nothing for writing (core.db is read-only, the lock is not taken) and prints what would change.
//! - A real run holds the core's lifetime lock, takes a `VACUUM INTO` backup of core.db before its first write, and
//!   writes in one transaction: the rows, the same-answer check over them, the sealed secrets, a row in `import_log`.
//!   Anything failing leaves core.db as it was (the backup is then a copy of the same file).
//! - The same-answer check (`check.rs`) runs first on a scratch copy and again on the real rows before they commit: for
//!   every person x agent x via (and bots), `Authority` over core.db equals `Rules` over the original file; one
//!   difference fails the step. It proves storage fidelity (Rust rules over the file vs Rust rules over core.db), not
//!   parity with Python `kith-auth`: `kernel/tests/difftest` is what holds the Rust rules to the Python (guest domains
//!   included).
//! - Idempotent: when core.db already holds exactly what the file says (people in the same order too) and every Keychain
//!   item has its record, no row, record, backup or log line is written. (Opening the core still bumps its generation, as
//!   every open does.)
//! - core.db rows that differ from the file (the owner's M2-2 writes, an older import) are NOT merged: the step refuses
//!   unless `--force`, which replaces the authority rows with the file's (after the backup).
//! - It does not make core.db authoritative and does not tombstone auth.json: `kith-auth` and the host keep deciding
//!   from the file until the cutover (M2-23).
//!
//! Where the mapping and what is left behind are written: `authority.rs` (tables), `items.rs` (secrets).
//!
//! Later (not here): cards (PR #711), access requests (M2-3), the host vault rows of serve.db (the host moves them itself
//! at M2-7), `profiles`/`grants`/`env` (kith-auth, M2-18), the cutover
//! and the auth.json tombstone (M2-23). Also later: `--force` could un-revoke a person once core.db writes come first
//! (before the cutover it should refuse to clear `revoked` unless the file agrees); a rotated Keychain value is never
//! resealed; the first envelope put can mint the master key outside the transaction; every put error reads as "store
//! unavailable"; the check's cost on very large files; `\r` handling differs from Python's text mode; the dry run opens
//! a WAL database read-only and needs its `-shm` to exist or be creatable.

// The ask tree's import (M3-8a): wired into `kith migrate` at the switch (M3-10).
#[allow(dead_code, reason = "first caller is the switch of kith-ask (M3-10)")]
pub(crate) mod asks;
// M3-9a. Not wired into `kith migrate` yet (M3-10 does that), so only the tests call it.
#[cfg(unix)]
#[allow(dead_code, reason = "M3-10 wires it into kith migrate")]
pub(crate) mod accounts;
mod authority;
mod check;
// M3-10b3: the writer claim files. Not wired into `kith migrate` yet (M3-10 does that), so only the tests call it.
#[cfg(unix)]
#[allow(dead_code, reason = "M3-10 wires it into kith migrate")]
pub(crate) mod claims;
pub(crate) mod items;
pub(crate) mod schedules;
// The wake queue's files (M3-7): nothing runs it until the switch (M3-10); the tests do.
#[cfg(test)]
mod tests;
#[allow(dead_code, reason = "wired into kith migrate by M3-10")]
pub(crate) mod transcripts;
#[allow(dead_code, reason = "first caller is the switch of kith-wake, M3-10")]
pub(crate) mod wake;

use std::path::Path;

use kith_kernel::secrets::Ctx;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::db::{self, Core, MIGRATIONS};
use crate::error::{Error, Result};
use crate::layout::Layout;
use crate::{AuthorityOptions, AuthorityReport};
use items::{ItemSource, Reference, SecretSink};

/// What the file says, parsed as `load_auth` reads it (every table present), and the SHA-256 of its bytes. `None`: no file.
fn read_source(path: &Path) -> Result<Option<(Value, String)>> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(Error::Refused(format!(
                "auth.json: {}: {e}; nothing was imported",
                path.display()
            )));
        }
    };
    let text = std::str::from_utf8(&bytes).map_err(|_| {
        Error::Refused(format!(
            "auth.json: {} is not UTF-8 text; nothing was imported",
            path.display()
        ))
    })?;
    let mut auth: Value = serde_json::from_str(text).map_err(|e| {
        Error::Refused(format!(
            "auth.json: {} is not valid JSON ({e}); nothing was imported",
            path.display()
        ))
    })?;
    let Some(obj) = auth.as_object_mut() else {
        return Err(Error::Refused(format!(
            "auth.json: {} is not a JSON object; nothing was imported",
            path.display()
        )));
    };
    for t in [
        "profiles", "grants", "env", "people", "presets", "members", "invites", "retired", "once",
    ] {
        obj.entry(t)
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
    }
    Ok(Some((auth, sha_hex(&bytes))))
}

fn sha_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut s, b| {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// The tables as the file would leave them, in a throwaway database.
fn scratch_db(auth: &Value) -> Result<Connection> {
    let mut conn = Connection::open_in_memory()?;
    db::apply(&mut conn, MIGRATIONS, Some("import-scratch"))?;
    authority::insert_all(&conn, auth)?;
    Ok(conn)
}

/// A scratch database holding exactly the authority rows `src` holds, every column as stored.
fn copy_out(src: &Connection) -> Result<Connection> {
    let mut dst = Connection::open_in_memory()?;
    db::apply(&mut dst, MIGRATIONS, Some("import-readback"))?;
    for table in [
        "people",
        "memberships",
        "presets",
        "invites",
        "retired",
        "agent_access",
        "authority_settings",
    ] {
        let cols: Vec<String> = src
            .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))?
            .query_map([], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        let list = cols.join(", ");
        let marks = vec!["?"; cols.len()].join(", ");
        // `people` keeps file order in `seq`; the other tables are WITHOUT ROWID and read in key order.
        let order = if table == "people" {
            "ORDER BY seq"
        } else {
            ""
        };
        let mut read = src.prepare(&format!("SELECT {list} FROM {table} {order}"))?;
        let mut rows = read.query([])?;
        let mut write = dst.prepare(&format!("INSERT INTO {table} ({list}) VALUES ({marks})"))?;
        while let Some(r) = rows.next()? {
            let values = (0..cols.len())
                .map(|i| r.get::<_, rusqlite::types::Value>(i))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            write.execute(rusqlite::params_from_iter(values))?;
        }
    }
    Ok(dst)
}

fn is_empty(snapshot: &Value) -> bool {
    [
        "people",
        "members",
        "presets",
        "invites",
        "retired",
        "agent_access",
    ]
    .iter()
    .all(|t| {
        snapshot
            .get(t)
            .and_then(Value::as_object)
            .is_none_or(serde_json::Map::is_empty)
    }) && snapshot.get("owner_bots_budget").is_none()
}

#[derive(PartialEq, Eq, Debug)]
enum Rows {
    /// core.db already holds exactly what the file says.
    Same,
    /// core.db holds no authority rows.
    Empty,
    /// core.db holds other rows.
    Differs,
}

/// The order of people decides which row `owner()` and `canonical()` take first, and a JSON object compares equal whatever
/// its key order, so the order is compared by itself.
fn people_order(snapshot: &Value) -> Vec<&str> {
    snapshot
        .get("people")
        .and_then(Value::as_object)
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default()
}

fn rows_state(existing: &Value, desired: &Value) -> Rows {
    if existing == desired && people_order(existing) == people_order(desired) {
        Rows::Same
    } else if is_empty(existing) {
        Rows::Empty
    } else {
        Rows::Differs
    }
}

fn refuse_differs(conn: &Connection) -> Error {
    let counts = authority::counts(conn).unwrap_or_default();
    Error::Refused(format!(
        "core.db already holds authority rows that differ from auth.json ({counts}), perhaps the owner's own writes or an \
         earlier import. Nothing was changed. Run `kith kernel core-migrate --authority --force` to replace them with the file's rows \
         (a backup of core.db is taken first)."
    ))
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// The step. `source` reads the Keychain and `sink` is where sealed values go; the real ones are in `items`.
pub(crate) fn run(
    layout: &Layout,
    ctx: &Ctx,
    opts: AuthorityOptions,
    source: &dyn ItemSource,
    sink: &mut dyn SecretSink,
) -> Result<AuthorityReport> {
    run_in(layout, ctx, opts, source, sink, None)
}

/// [`run`] inside a core the caller already holds the lifetime lock of (the cutover framework's `step`). `None` takes
/// the lock for the step, as `kith migrate --authority` does.
pub(crate) fn run_in(
    layout: &Layout,
    ctx: &Ctx,
    opts: AuthorityOptions,
    source: &dyn ItemSource,
    sink: &mut dyn SecretSink,
    held: Option<&mut Core>,
) -> Result<AuthorityReport> {
    let mut rep = AuthorityReport {
        dry_run: opts.dry_run,
        ..AuthorityReport::default()
    };
    let Some((auth, sha)) = read_source(&ctx.auth)? else {
        rep.lines.push(format!(
            "no auth.json at {}: nothing to import",
            ctx.auth.display()
        ));
        return Ok(rep);
    };
    authority::refuse_what_it_cannot_keep(&auth)?;
    // Everything that can refuse happens on a scratch copy, before the real file is opened for writing.
    let scratch = scratch_db(&auth)?;
    let desired = crate::authority::snapshot(&scratch)?;
    let diffs = check::diffs(&auth, &scratch, ctx)?;
    if !diffs.is_empty() {
        return Err(Error::Refused(format!(
            "auth.json: core.db's answers would differ from the file's; nothing was imported. First differences: {}",
            diffs.join(" | ")
        )));
    }
    let (refs, unusable) = items::references(&auth);
    for u in unusable {
        rep.lines.push(format!(
            "secret skipped: {u} names a Keychain item that cannot be a record name"
        ));
    }
    rep.lines.extend(authority::left_behind(&auth));
    if opts.dry_run {
        dry(layout, opts, &desired, &scratch, &refs, sink, &mut rep)?;
    } else {
        real(
            layout, ctx, opts, &auth, &sha, &desired, &scratch, &refs, source, sink, held, &mut rep,
        )?;
    }
    Ok(rep)
}

fn dry(
    layout: &Layout,
    opts: AuthorityOptions,
    desired: &Value,
    scratch: &Connection,
    refs: &[Reference],
    sink: &mut dyn SecretSink,
    rep: &mut AuthorityReport,
) -> Result<()> {
    let counts = authority::counts(scratch)?;
    let db = Connection::open_with_flags(&layout.db, OpenFlags::SQLITE_OPEN_READ_ONLY).ok();
    let has = |c: &Connection, t: &str| -> bool {
        c.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [t],
            |r| r.get::<_, i64>(0),
        )
        .is_ok_and(|n| n > 0)
    };
    let ready = db
        .as_ref()
        .filter(|c| has(c, "people") && has(c, "retired"));
    let state = match ready {
        None => {
            rep.lines.push(
                "core.db is missing or not migrated yet: a real run would migrate it first".into(),
            );
            Rows::Empty
        }
        Some(c) => rows_state(&crate::authority::snapshot(c)?, desired),
    };
    match state {
        Rows::Same => rep.lines.push(format!(
            "authority: core.db already holds exactly auth.json's rows ({counts}); nothing to change"
        )),
        Rows::Empty => {
            rep.lines
                .push(format!("authority: would import {counts}"));
            rep.changed = true;
        }
        Rows::Differs if opts.force => {
            rep.lines.push(format!(
                "authority: would REPLACE the rows core.db holds with auth.json's ({counts}); --force given"
            ));
            rep.changed = true;
        }
        Rows::Differs => {
            return Err(refuse_differs(ready.ok_or_else(|| {
                Error::Io("core.db vanished during the dry run".into())
            })?));
        }
    }
    let have = match ready.filter(|c| has(c, "secret_records")) {
        Some(c) => sink.names(c)?,
        None => std::collections::BTreeSet::new(),
    };
    let todo: Vec<&str> = refs
        .iter()
        .filter(|r| !have.contains(&r.name))
        .map(|r| r.name.as_str())
        .collect();
    if !todo.is_empty() {
        rep.changed = true;
    }
    rep.lines.push(format!(
        "secrets: would try to seal {} Keychain item(s) into the envelope ({} already there): {todo:?}; values are not read in a dry run",
        todo.len(),
        refs.len() - todo.len()
    ));
    rep.lines.push("dry run: nothing was written".into());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn real(
    layout: &Layout,
    ctx: &Ctx,
    opts: AuthorityOptions,
    auth: &Value,
    sha: &str,
    desired: &Value,
    scratch: &Connection,
    refs: &[Reference],
    source: &dyn ItemSource,
    sink: &mut dyn SecretSink,
    held: Option<&mut Core>,
    rep: &mut AuthorityReport,
) -> Result<()> {
    let counts = authority::counts(scratch)?;
    // The lifetime lock for the whole step; this also brings the schema up to date, as `kith migrate` does.
    let mut own;
    let core = if let Some(c) = held {
        c
    } else {
        own = crate::start_with(layout, MIGRATIONS)?;
        &mut own
    };
    let (state, have) = {
        let tx = core.claim_tx()?;
        let state = rows_state(&crate::authority::snapshot(&copy_out(&tx)?)?, desired);
        if state == Rows::Differs && !opts.force {
            return Err(refuse_differs(&tx));
        }
        (state, sink.names(&tx)?)
    };
    let (fetched, mut lines) = items::fetch(refs, &have, source);
    if state == Rows::Same && fetched.is_empty() {
        rep.lines.append(&mut lines);
        rep.lines.push(format!(
            "nothing to change: core.db already holds auth.json's rows ({counts}) and every Keychain item has its record"
        ));
        return Ok(());
    }
    // Before the first write.
    let backup = core.backup(layout, "pre-import")?;
    rep.lines
        .push(format!("backup of core.db: {}", backup.display()));
    rep.backup = Some(backup.clone());

    let tx = core.claim_tx()?;
    if state == Rows::Same {
        rep.lines
            .push("authority: already imported; unchanged".into());
    } else {
        if state == Rows::Differs {
            authority::clear(&tx)?;
        }
        authority::insert_all(&tx, auth)?;
        // The authority reader opens its own read transaction, which cannot nest in this write one, so the rows just
        // written are read back raw into a scratch database and asked from there: same bytes, same answers.
        let back = copy_out(&tx)?;
        if crate::authority::snapshot(&back)? != *desired {
            return Err(Error::Refused(
                "authority import: the stored rows do not read back as the file's; nothing was imported".into(),
            ));
        }
        let diffs = check::diffs(auth, &back, ctx)?;
        if !diffs.is_empty() {
            return Err(Error::Refused(format!(
                "authority import: core.db's answers differ from the file's; nothing was imported, core.db is as the backup. \
                 First differences: {}",
                diffs.join(" | ")
            )));
        }
        rep.lines.push(match state {
            Rows::Differs => {
                format!("authority: replaced core.db's rows with auth.json's ({counts})")
            }
            _ => format!("authority: imported {counts}"),
        });
    }
    let (sealed, mut seal_lines) = items::seal(&tx, fetched, sink);
    rep.lines.append(&mut lines);
    rep.lines.append(&mut seal_lines);
    if state == Rows::Same && sealed == 0 {
        // Nothing was sealed (the store keeps refusing): no log row, no backup left behind, rows as they were.
        drop(tx);
        let _ = std::fs::remove_file(&backup);
        rep.backup = None;
        rep.lines
            .push("nothing was changed; the backup just taken was removed".into());
        return Ok(());
    }
    // kith-auth may have written the file while this ran: what was checked is no longer what is there.
    let now = std::fs::read(&ctx.auth).map(|b| sha_hex(&b)).ok();
    if now.as_deref() != Some(sha) {
        return Err(Error::Refused(
            "auth.json changed while the import ran; nothing was imported. Run it again.".into(),
        ));
    }
    tx.execute(
        "INSERT INTO import_log (step, at, source, source_sha256, backup, summary) VALUES ('authority', ?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            now_secs(),
            ctx.auth.display().to_string(),
            sha,
            backup.display().to_string(),
            format!("authority: {counts}; {sealed} secret(s) sealed"),
        ],
    )?;
    tx.commit()?;
    rep.changed = true;
    rep.lines.push(
        "done. core.db is a copy: kith-auth and the host still decide from auth.json (the cutover is a later step)".into(),
    );
    Ok(())
}

/// The people the file says, as a plain map from id to the facts a revoke, a bot or a rename hangs on.
fn facts(snapshot: &Value) -> std::collections::BTreeMap<String, Value> {
    snapshot
        .get("people")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(id, p)| {
            let get = |k: &str| p.get(k).cloned().unwrap_or(Value::Null);
            // The time of a revoke is when it was written (the file and the core write it apart), not a fact to compare.
            let revoked = p.get("revoked").is_some_and(|r| r != &Value::Bool(false));
            (
                id.clone(),
                serde_json::json!({
                    "owner": get("owner"),
                    "revoked": revoked,
                    "bot_of": get("bot_of"),
                    "was": get("was"),
                }),
            )
        })
        .collect()
}

/// The M2-18 same-answer check of the authority module: the differences between auth.json (what `kith-auth` still
/// decides from) and core.db, empty when the core answers what the file answers.
///
/// Two layers. First the facts a decision hangs on, compared row by row so the line names the person: who is revoked,
/// who is a bot of whom, every `was` and `retired` entry, who is the owner and the order of people (it decides which
/// row `owner()` and `canonical()` take). Then `check.rs`: for every person x agent x via, `Rules` over the file and
/// `Authority` over core.db give the same `may`, `resolve`, `resolve_bot` and `cut_off`. A file the import would refuse
/// (an owner who is also revoked, a domain the rules cannot keep) is a difference too: it cannot have been imported.
pub(crate) fn same_answer(ctx: &Ctx, conn: &Connection) -> Result<Vec<String>> {
    let auth = match read_source(&ctx.auth) {
        Ok(Some((auth, _))) => auth,
        // No file: the old store answers nothing, so only an empty core agrees.
        Ok(None) => serde_json::json!({}),
        Err(Error::Refused(why)) => return Ok(vec![why]),
        Err(e) => return Err(e),
    };
    if let Err(Error::Refused(why)) = authority::refuse_what_it_cannot_keep(&auth) {
        return Ok(vec![why]);
    }
    let want = crate::authority::snapshot(&scratch_db(&auth)?)?;
    let have = crate::authority::snapshot(conn)?;
    let (want_people, have_people) = (facts(&want), facts(&have));
    let mut out = Vec::new();
    for (id, w) in &want_people {
        match have_people.get(id) {
            None => out.push(format!("person {id}: in auth.json, not in core.db")),
            Some(h) if h != w => out.push(format!("person {id}: auth.json {w}, core.db {h}")),
            Some(_) => {}
        }
    }
    for id in have_people.keys().filter(|k| !want_people.contains_key(*k)) {
        out.push(format!("person {id}: in core.db, not in auth.json"));
    }
    if people_order(&want) != people_order(&have) {
        out.push(
            "people are in a different order (the first owner and the first row of an id win)"
                .into(),
        );
    }
    let retired = |v: &Value| -> std::collections::BTreeSet<String> {
        v.get("retired")
            .and_then(Value::as_object)
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default()
    };
    let (rw, rh) = (retired(&want), retired(&have));
    for id in rw.difference(&rh) {
        out.push(format!("retired {id}: in auth.json, not in core.db"));
    }
    for id in rh.difference(&rw) {
        out.push(format!("retired {id}: in core.db, not in auth.json"));
    }
    out.extend(check::diffs(&auth, conn, ctx)?);
    Ok(out)
}
