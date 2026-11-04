//! Guest doors in the core (M2-14, lane G): the state of a door and its sweep over core.db, so a revoked person's door
//! start and the sweep that ends their doors cannot both win.
//!
//! A door is a fixed command the kernel runs for a guest in a box of its own (`kernel/lib/kith_door.py`). Today its
//! state is a claim file per door and the start and the sweep take kith-auth's flock; the host's sweep
//! (`serve/host/src/doors.rs`) runs `kith door sweep` on a cut-off, at start and every minute. This module is the port
//! of that onto one row, and it is NOT switched: `kith_door.py` and the host keep their own state until a later step
//! (the host's callers move in lane H, the Python in M2-18). A door started before the switch has no row here, so a
//! core sweep cannot see it; the old sweep still ends it. Nothing here kills a process: the sweep marks rows, says which,
//! and the supervisor (or the one who sweeps) ends the box, then confirms with [`end`].
//!
//! ```text
//! start -> starting -> live          (mark_live: the box is up)
//!    starting | live -> ended        (end: the box ended by itself)
//!    starting | live -> swept        (sweep: access lost, supervisor gone, or the person was cut off)
//!    swept -> swept, reaped          (end after a sweep: the box is confirmed gone)
//! ```
//!
//! Every change is ONE `BEGIN IMMEDIATE` transaction through [`Core::claim_tx`], and every change to a door row goes by
//! its `version` (`UPDATE ... WHERE id = ? AND version = ?`). The two that race:
//! - [`start`] reads who the person is and what they may do, and writes the row, in the same transaction. A person who
//!   is cut off (`cut_off`), who may not run the agent (`may`: an unknown or removed person, a bot whose owner may not)
//!   or, for anyone but the owner, whose Mac is not in sync with the cloud ([`crate::sync_lease`]; the model's
//!   `DoorGate`) writes nothing.
//! - [`sweep`] (and the revoke hook, [`sweep_ids`]) moves `starting` and `live` rows to `swept` and bumps the version.
//!   So a start that committed first leaves a row the sweep sees, and a sweep that committed first leaves a start
//!   either refused (the person is cut off by then) or, for a start that was already past its check, a row the
//!   supervisor's [`mark_live`] finds swept, and the supervisor stops the box. Never both "live" and "swept".
//!
//! `swept` and `ended` are final. An undo of a revoke clears one fact on the person and touches no door row, so a swept
//! door stays swept; the person starts a new door. Starting again with the id of a swept or ended door is refused, never
//! resurrected, and a start retried with a live door's id answers that door unchanged.
//!
//! The revoke hook: `people.suspend`, `people.removePerson` and `people.removeBot` sweep the doors of the ids they name
//! (the person and their bots) in the SAME transaction as the write, and `people.renamePerson` follows the rename. In
//! the same transaction because the model's door sweep ends every door of a revoked person (`DoorSweep`, `DoorD1`):
//! with one commit there is no moment when a person is revoked and one of their doors is still `starting` or `live` in
//! the table, and a crash between a revoke and its sweep cannot leave one. The owner is never named by a revoke, so the
//! owner's doors stay. The answer of those rows carries the swept doors under `doors`, for the caller to stop the boxes.
//! Undo does nothing here.
//!
//! The check is `kith-auth door_state` (`kernel/bin/kith-auth`), ported as far as core.db and the agent's
//! `capabilities.yaml` allow, the same one for the start and for the recheck of a running door: the owner is refused
//! ("doors are for the people an agent is shared with"), then `cut_off` and `may` (their words), then (start only) the
//! sync lease, then the door itself: `door` is the piece `name@digest` and must be among the doors the person's ticked
//! jobs grant, as `resolve` answers them: cut to the package as it is now, and for a bot cut to what its owner holds
//! (`owner_cut`). "Held" never ends a running door, as there. What the core cannot tell apart yet, and so refuses with
//! one sentence (fail closed): `no-job` / `stale` (no job of the agent has the door, or the package changed),
//! `not-ticked`, `declined` / `widened` (`job_view`), and `owner-lacks` (a bot whose own tick has it and whose owner's
//! does not). Each is a refusal in `kith-auth` too, only with its own words.
//!
//! Probes of a process (`ps`, `kill -0`) run before the transaction, never inside it: the version decides what the
//! probe cannot (a row that changed in between loses its compare-and-swap).

use kith_kernel::secrets::{self, Ctx};
use rusqlite::{Connection, Row, params};
use serde_json::{Value, json};

use crate::authority;
use crate::db::Core;
use crate::error::{Error, Result};
use crate::gate::Liveness;

pub(crate) mod call;
#[cfg(test)]
mod tests;

pub(crate) use call::{call, serves};

/// How long a closed door (ended, or swept and confirmed gone) stays in the table, in milliseconds.
const KEEP_CLOSED_MS: i64 = 86_400_000;

const COLS: &str = "id, agent, door, person, folder, pid, proc_start, state, version, reaped, why, started_at, changed_at";
const OPEN: &str = "state IN ('starting', 'live')";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    Starting,
    Live,
    Ended,
    Swept,
}

impl State {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Live => "live",
            Self::Ended => "ended",
            Self::Swept => "swept",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "starting" => Self::Starting,
            "live" => Self::Live,
            "ended" => Self::Ended,
            "swept" => Self::Swept,
            other => return Err(Error::Corrupt(format!("core.db: doors: state {other:?}"))),
        })
    }
}

/// One door row.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_field_names,
    reason = "`door` is the declared door the row runs"
)]
pub(crate) struct Door {
    pub id: String,
    pub agent: String,
    pub door: String,
    pub person: String,
    pub folder: String,
    pub pid: u32,
    pub proc_start: String,
    pub state: State,
    pub version: i64,
    pub reaped: bool,
    pub why: String,
    pub started_at: i64,
    pub changed_at: i64,
}

impl Door {
    fn read(r: &Row<'_>) -> rusqlite::Result<(Self, String)> {
        let state: String = r.get(7)?;
        Ok((
            Self {
                id: r.get(0)?,
                agent: r.get(1)?,
                door: r.get(2)?,
                person: r.get(3)?,
                folder: r.get(4)?,
                pid: u32::try_from(r.get::<_, i64>(5)?).unwrap_or(0),
                proc_start: r.get(6)?,
                state: State::Starting,
                version: r.get(8)?,
                reaped: r.get::<_, i64>(9)? != 0,
                why: r.get(10)?,
                started_at: r.get(11)?,
                changed_at: r.get(12)?,
            },
            state,
        ))
    }

    pub(crate) fn json(&self) -> Value {
        json!({
            "id": self.id,
            "agent": self.agent,
            "door": self.door,
            "person": self.person,
            "folder": self.folder,
            "pid": self.pid,
            "procStart": self.proc_start,
            "state": self.state.name(),
            "version": self.version,
            "reaped": self.reaped,
            "why": self.why,
            "startedAt": self.started_at,
            "changedAt": self.changed_at,
        })
    }

    fn is_open(&self) -> bool {
        matches!(self.state, State::Starting | State::Live)
    }
}

fn refuse(what: impl std::fmt::Display) -> Error {
    Error::Refused(format!("kith-core: doors: {what}"))
}

fn rows(conn: &Connection, sql: &str, args: &[&dyn rusqlite::ToSql]) -> Result<Vec<Door>> {
    let mut st = conn.prepare(sql)?;
    let found = st.query_map(args, Door::read)?;
    let mut out = Vec::new();
    for f in found {
        let (mut d, state) = f?;
        d.state = State::parse(&state)?;
        out.push(d);
    }
    Ok(out)
}

fn by_id(conn: &Connection, id: &str) -> Result<Option<Door>> {
    Ok(rows(
        conn,
        &format!("SELECT {COLS} FROM doors WHERE id = ?1"),
        &[&id],
    )?
    .pop())
}

/// A random claim id, 16 hex digits (what `kith_door.py` calls a claim).
fn new_id() -> Result<String> {
    use std::fmt::Write as _;
    let mut raw = [0u8; 8];
    getrandom::fill(&mut raw)
        .map_err(|e| Error::Io(format!("kith-core: no random id for a door: {e}")))?;
    Ok(raw.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    }))
}

fn valid_id(id: &str) -> bool {
    id.len() == 16 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Whether the door's supervisor is gone. The gate's and the cards' rule: a start time that reads and differs is a
/// reused pid; one that cannot be read is gone only when the system says there is no such process (a failed `ps` never
/// ends a live door).
fn supervisor_gone(d: &Door, live: &dyn Liveness) -> bool {
    match live.start(d.pid) {
        Some(current) if !d.proc_start.is_empty() => current != d.proc_start,
        Some(_) => false,
        None => live.gone(d.pid),
    }
}

/// The compare-and-swap of a sweep: this row at this version, still open, becomes `swept`. A row that changed since it
/// was read (it cannot inside one transaction, but the statement is the rule) is left as it is.
fn swept(conn: &Connection, d: &Door, why: &str, now: i64) -> Result<Option<Door>> {
    let n = conn.execute(
        &format!(
            "UPDATE doors SET state = 'swept', version = version + 1, why = ?3, changed_at = ?4 \
             WHERE id = ?1 AND version = ?2 AND {OPEN}"
        ),
        params![d.id, d.version, why, now],
    )?;
    if n == 0 {
        return Ok(None);
    }
    Ok(Some(Door {
        state: State::Swept,
        version: d.version + 1,
        why: why.to_owned(),
        changed_at: now,
        ..d.clone()
    }))
}

/// The revoke hook: marks every open door of these people swept, in the caller's transaction. `ids` are the person
/// and their bots as the write that cut them off names them. Returns the doors it swept.
pub(crate) fn sweep_ids(
    conn: &Connection,
    ids: &[String],
    why: &str,
    now: i64,
) -> Result<Vec<Door>> {
    let mut out = Vec::new();
    for id in ids {
        let open = rows(
            conn,
            &format!(
                "SELECT {COLS} FROM doors WHERE person = ?1 AND {OPEN} ORDER BY started_at, id"
            ),
            &[id],
        )?;
        for d in open {
            out.extend(swept(conn, &d, why, now)?);
        }
    }
    Ok(out)
}

/// A rename carried through the doors: the old id's rows now name the new one, so a later revoke finds them.
pub(crate) fn renamed(conn: &Connection, old: &str, new: &str) -> Result<()> {
    conn.execute(
        "UPDATE doors SET person = ?2 WHERE person = ?1",
        params![old, new],
    )?;
    Ok(())
}

/// What [`start`] is asked.
pub(crate) struct Start<'a> {
    /// The claim id to use (16 hex), or `None` for a new one. A start retried with the same id is the same door.
    pub id: Option<&'a str>,
    pub agent: &'a str,
    pub door: &'a str,
    pub person: &'a str,
    pub folder: &'a str,
    /// The supervisor (the process that runs the box and cleans up after it).
    pub pid: u32,
}

/// The answer of [`start`].
#[derive(Debug)]
pub(crate) struct Started {
    pub door: Door,
    /// True when this id was already a starting or live door and nothing was written.
    pub repeat: bool,
}

/// Starts a door: the access check and the row, in one claim transaction. Refused (and nothing written) by
/// [`door_refusal`]: the owner, a person who is cut off or may not run the agent, a Mac out of sync with the cloud, or a
/// door the person's jobs do not give them.
pub(crate) fn start(
    core: &mut Core,
    ctx: &Ctx,
    live: &dyn Liveness,
    s: &Start<'_>,
    now: i64,
) -> Result<Started> {
    start_between(core, ctx, live, s, now, &mut || {})
}

/// [`start`], calling `between` after the access is decided and before the row is written, inside the transaction: a
/// test commits a revoke from a second connection there, which must find the database locked.
fn start_between(
    core: &mut Core,
    ctx: &Ctx,
    live: &dyn Liveness,
    s: &Start<'_>,
    now: i64,
    between: &mut dyn FnMut(),
) -> Result<Started> {
    if s.pid < 2 {
        return Err(Error::BadParams(
            "kith-core: doors: pid is the supervisor's, 2 or more".into(),
        ));
    }
    for (what, v) in [("agent", s.agent), ("door", s.door), ("person", s.person)] {
        if v.is_empty() || v == "-" {
            return Err(Error::BadParams(format!(
                "kith-core: doors: a door runs for a person: {what} is required"
            )));
        }
    }
    let id = match s.id {
        Some(id) if valid_id(id) => id.to_owned(),
        Some(_) => {
            return Err(Error::BadParams(
                "kith-core: doors: a door id is 16 lowercase hex digits".into(),
            ));
        }
        None => new_id()?,
    };
    // The supervisor, probed before the lock is taken (a probe is a process spawn).
    let proc_start = live.start(s.pid);
    if proc_start.is_none() && live.gone(s.pid) {
        return Err(refuse(format_args!("supervisor {} is not running", s.pid)));
    }
    let tx = core.claim_tx()?;
    let auth = authority::tables_within(&tx)?;
    let person = secrets::canonical(&auth, s.person);
    // The access, read in this transaction: a revoke commits before it or after the row, never between the two.
    let in_sync = secrets::is_owner(&auth, &person) || crate::sync_lease::held_in(&tx, now)?;
    if let Some(why) = door_refusal(ctx, &auth, s.agent, &person, s.door, in_sync) {
        return Err(refuse(why));
    }
    between();
    if let Some(d) = by_id(&tx, &id)? {
        if d.agent == s.agent && d.door == s.door && d.person == person && d.is_open() {
            return Ok(Started {
                door: d,
                repeat: true,
            });
        }
        return Err(refuse(format_args!(
            "door {id} is {} already; start a new one",
            d.state.name()
        )));
    }
    let proc_start = proc_start.unwrap_or_default();
    tx.execute(
        "INSERT INTO doors (id, agent, door, person, folder, pid, proc_start, state, version, reaped, why, started_at, changed_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'starting', 0, 0, '', ?8, ?8)",
        params![id, s.agent, s.door, person, s.folder, s.pid, proc_start, now],
    )?;
    let door = by_id(&tx, &id)?.ok_or_else(|| refuse("the new row is gone"))?;
    tx.commit()?;
    Ok(Started {
        door,
        repeat: false,
    })
}

/// The answer of [`mark_live`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Live {
    /// The row is live (this call made it so, or it was already).
    Live(Door),
    /// The row was swept or ended first (or is gone): the box must not run. `state` says which.
    Lost {
        state: &'static str,
        door: Option<Door>,
    },
}

/// The supervisor says the box is up: `starting -> live`, only at the version [`start`] gave. A row a sweep got to first
/// answers [`Live::Lost`] and the caller stops the box.
pub(crate) fn mark_live(core: &mut Core, id: &str, version: i64, now: i64) -> Result<Live> {
    let tx = core.claim_tx()?;
    let n = tx.execute(
        "UPDATE doors SET state = 'live', version = version + 1, changed_at = ?3 \
         WHERE id = ?1 AND version = ?2 AND state = 'starting'",
        params![id, version, now],
    )?;
    let door = by_id(&tx, id)?;
    tx.commit()?;
    Ok(match door {
        Some(d) if n == 1 || d.state == State::Live => Live::Live(d),
        Some(d) => Live::Lost {
            state: d.state.name(),
            door: Some(d),
        },
        None => Live::Lost {
            state: "gone",
            door: None,
        },
    })
}

/// The box is gone. An open row becomes `ended`; a swept one is confirmed gone (`reaped`), staying swept; an ended one
/// stays. Ending twice changes nothing. Refuses an id that is no door.
pub(crate) fn end(core: &mut Core, id: &str, now: i64) -> Result<Door> {
    let tx = core.claim_tx()?;
    let d = by_id(&tx, id)?.ok_or_else(|| refuse(format_args!("no door {id}")))?;
    let out = match d.state {
        State::Starting | State::Live => {
            tx.execute(
                &format!(
                    "UPDATE doors SET state = 'ended', version = version + 1, changed_at = ?3 \
                     WHERE id = ?1 AND version = ?2 AND {OPEN}"
                ),
                params![id, d.version, now],
            )?;
            by_id(&tx, id)?
        }
        State::Swept if !d.reaped => {
            tx.execute(
                "UPDATE doors SET reaped = 1, changed_at = ?2 WHERE id = ?1 AND state = 'swept'",
                params![id, now],
            )?;
            by_id(&tx, id)?
        }
        State::Swept | State::Ended => Some(d),
    };
    tx.commit()?;
    out.ok_or_else(|| refuse(format_args!("no door {id}")))
}

/// What a sweep did.
#[derive(Debug, Default)]
pub(crate) struct Swept {
    /// Doors this sweep moved to `swept`.
    pub swept: Vec<Door>,
    /// Doors swept earlier whose box is not confirmed gone: the caller tries to stop them again.
    pub pending: Vec<Door>,
}

/// The sweep, as `kith door sweep [--person P]...`: every open door of `persons` and of their bots, every open door
/// whose supervisor is gone, and (`recheck`) every open door whose person may no longer run its agent. The owner is
/// only ever swept for a dead supervisor: naming them does nothing. One claim transaction.
pub(crate) fn sweep(
    core: &mut Core,
    ctx: &Ctx,
    live: &dyn Liveness,
    persons: &[String],
    recheck: bool,
    now: i64,
) -> Result<Swept> {
    sweep_between(core, ctx, live, persons, recheck, now, &mut || {})
}

/// [`sweep`], calling `between` after the doors to end are decided and before they are written (see [`start_between`]).
fn sweep_between(
    core: &mut Core,
    ctx: &Ctx,
    live: &dyn Liveness,
    persons: &[String],
    recheck: bool,
    now: i64,
    between: &mut dyn FnMut(),
) -> Result<Swept> {
    // Which supervisors are gone, probed before the lock is taken. A door started after this read is not in the set and
    // is not swept for it; one whose row changes in between loses the compare-and-swap below.
    let gone: Vec<String> = rows(
        core.conn(),
        &format!("SELECT {COLS} FROM doors WHERE {OPEN}"),
        &[],
    )?
    .into_iter()
    .filter(|d| supervisor_gone(d, live))
    .map(|d| d.id)
    .collect();
    let tx = core.claim_tx()?;
    let auth = authority::tables_within(&tx)?;
    let named: Vec<String> = persons
        .iter()
        .map(|p| secrets::canonical(&auth, p))
        .filter(|p| !secrets::is_owner(&auth, p))
        .collect();
    let open = rows(
        &tx,
        &format!("SELECT {COLS} FROM doors WHERE {OPEN} ORDER BY started_at, id"),
        &[],
    )?;
    let mut out = Swept::default();
    let mut doomed = Vec::new();
    for d in open {
        let person = secrets::canonical(&auth, &d.person);
        let why = if !secrets::is_owner(&auth, &person) && descends(&auth, &person, &named) {
            Some("its person was cut off")
        } else if gone.contains(&d.id) {
            Some("its supervisor is gone")
        } else if recheck && door_refusal(ctx, &auth, &d.agent, &person, &d.door, true).is_some() {
            Some("its person may no longer run it")
        } else {
            None
        };
        if let Some(why) = why {
            doomed.push((d, why));
        }
    }
    between();
    for (d, why) in doomed {
        out.swept.extend(swept(&tx, &d, why, now)?);
    }
    out.pending = rows(
        &tx,
        &format!(
            "SELECT {COLS} FROM doors WHERE state = 'swept' AND reaped = 0 ORDER BY started_at, id"
        ),
        &[],
    )?
    .into_iter()
    .filter(|d| !out.swept.iter().any(|s| s.id == d.id))
    .collect();
    tx.execute(
        "DELETE FROM doors WHERE changed_at < ?1 AND (state = 'ended' OR (state = 'swept' AND reaped = 1))",
        [now - KEEP_CLOSED_MS],
    )?;
    tx.commit()?;
    Ok(out)
}

/// Whether `person` is one of `named` or a bot (of a bot ...) of one.
fn descends(auth: &Value, person: &str, named: &[String]) -> bool {
    let mut seen: Vec<String> = Vec::new();
    let mut cur = Some(person.to_owned());
    while let Some(p) = cur.filter(|p| !seen.contains(p)) {
        if named.contains(&p) {
            return true;
        }
        cur = secrets::parent_of(auth, &p);
        seen.push(p);
    }
    false
}

/// `door_state`: why the person's run of `agent` may not have the door `piece` (`name@digest`) now, or `None`. The order
/// and words are kith-auth's: the owner, then `cut_off` and `may`, then the sync lease (`in_sync`; a recheck passes true,
/// as held never ends a door), then the door among the person's granted doors. A rule with no answer (a membership naming
/// a preset nobody defined) reads as a refusal: fail closed.
fn door_refusal(
    ctx: &Ctx,
    auth: &Value,
    agent: &str,
    person: &str,
    piece: &str,
    in_sync: bool,
) -> Option<String> {
    if secrets::is_owner(auth, person) {
        return Some("doors are for the people an agent is shared with".to_owned());
    }
    if secrets::cut_off(auth, person) {
        return Some(format!("{person}'s access was removed"));
    }
    match authority::may_in(ctx, auth, person, agent) {
        Ok((true, _)) => {}
        Ok((false, why)) => return Some(why),
        Err(e) => return Some(e.to_string()),
    }
    if !in_sync {
        return Some(
            "this computer is out of sync with the Kith cloud; try again in a moment".to_owned(),
        );
    }
    match authority::resolve_in(ctx, auth, agent, person) {
        Ok(got)
            if got
                .get("doors")
                .and_then(Value::as_array)
                .is_some_and(|d| d.iter().any(|x| x.as_str() == Some(piece))) =>
        {
            None
        }
        Ok(_) => Some(format!(
            "the door {piece} is not part of a job given to {person} on {agent} now (no such job, the package changed, not ticked, changed since it was given, or more than their owner holds)"
        )),
        Err(e) => Some(e.to_string()),
    }
}

/// The doors, open ones first (and swept ones not yet confirmed gone); `all` adds the closed.
pub(crate) fn list(core: &mut Core, all: bool) -> Result<Vec<Door>> {
    let tx = core.claim_tx()?;
    let sql = if all {
        format!("SELECT {COLS} FROM doors ORDER BY started_at, id")
    } else {
        format!(
            "SELECT {COLS} FROM doors WHERE {OPEN} OR (state = 'swept' AND reaped = 0) ORDER BY started_at, id"
        )
    };
    rows(&tx, &sql, &[])
}
