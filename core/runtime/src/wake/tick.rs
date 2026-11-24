//! `wake.tick`: the core's clock work for the wake queue, which the host drives every few seconds (S4 design R1, R2). It
//! replaces what `kith-wake drain` did once a minute: waiters whose thing happened become requests, runs whose turn is over
//! are settled, parked turns of a conversation nothing holds are woken, and what is owed becomes one run and one turn.
//!
//! **Two phases, nothing slow under the core lock (R2).** Phase 1 reads a snapshot under the lock and lets go. With no lock
//! held, the tick then asks the world: `gh` for the `ci:` waiters, the process table for `pid:` ones, the clock for `at:`,
//! the transcript for the marker of each run whose turn was final in the snapshot, and the holder of each imported run.
//! Phase 2 takes the lock again and, in one claim transaction, does four things in order: applies the waiter results by
//! compare-and-swap (R7: a waiter a Stop removed meanwhile does not fire), settles each run by R4 (`settle.rs`), wakes the
//! parked turns of conversations with no live claim, and takes what is due.
//!
//! The marker is read only for a turn already seen final. A turn that goes final between the phases is settled on the next
//! tick, with a transcript read after it was seen final (model `broken-FreshMarker`). A tick that overlaps another (the
//! host's kick and its clock) is harmless: phase 2 reads the rows again, a waiter's result fails its compare-and-swap, a run
//! is settled once (it leaves the queue), and `wake_turns` keeps one turn per run.
//!
//! The daemon serves `wake.tick` on its own path, never inside one `with_core` closure: a `gh` that takes 20 s would make
//! every other call answer `Busy` after `LOCK_WAIT`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kith_kernel::paths::Paths;
use serde_json::{Value, json};

use super::model::{State, Turn};
use super::outside::{Files, Outside, System, Transcripts};
use super::settle::{self, Linked, Verdict};
use super::turns;
use super::waiters::{self, Looked};
use super::{load, queue, save};
use crate::asks;
use crate::conversations::claims;
use crate::db::Core;
use crate::error::Result;

/// How long the waiters' looks may take in all, so the call answers inside the client's timeout; waiters not looked at
/// wait for the next tick.
const LOOK_BUDGET: Duration = Duration::from_secs(20);

/// What a call needs of the daemon: the core under its lock, for one phase.
pub(crate) trait CoreLock {
    fn with_core<R>(&self, f: impl FnOnce(&mut Core) -> Result<R>) -> Result<R>;
}

/// The world the tick asks, made from the caller's view of the folders. A test gives its own.
pub(crate) type World =
    Arc<dyn Fn(&Paths) -> (Box<dyn Outside>, Box<dyn Transcripts>) + Send + Sync>;

/// The real world: the system's clock, processes and `gh`, and the Claude transcripts under the caller's home.
pub(crate) fn system() -> World {
    Arc::new(|folders: &Paths| {
        (
            Box::new(System {
                state: folders.state.clone(),
                gh: None,
            }),
            Box::new(Files::of(&folders.home, &folders.machine, None)),
        )
    })
}

/// What phase 1 read.
struct Seen {
    state: State,
    folders: Paths,
    linked: Vec<Linked>,
    legacy: Vec<(String, Turn)>,
    /// The asks whose run may have ended (the ask design's R4).
    asks: asks::Seen,
}

/// What the world said, with no lock held.
struct Asked {
    looks: Vec<Looked>,
    /// The runs whose turn was final in the snapshot, with what their transcript said (only for those that need it).
    final_runs: BTreeMap<String, bool>,
    probes: Vec<(String, String, queue::Probe)>,
    asks: asks::Looked,
}

fn phase_one(core: &mut Core, params: &Value) -> Result<Seen> {
    let (folders, _) = core.folders_for(params)?;
    let tx = core.conn().unchecked_transaction()?;
    let state = load(&tx)?;
    let (linked, legacy) = settle::runs_in(&tx, &state)?;
    let asks = asks::seen_in(&tx)?;
    Ok(Seen {
        state,
        folders,
        linked,
        legacy,
        asks,
    })
}

fn ask(seen: &Seen, out: &dyn Outside, tr: &dyn Transcripts, now: f64) -> Asked {
    let begun = Instant::now();
    let mut looks = Vec::new();
    for w in seen.state.waiters.values() {
        if begun.elapsed() >= LOOK_BUDGET {
            break;
        }
        looks.push(waiters::look(out, w, now));
    }
    let final_runs = seen
        .linked
        .iter()
        .filter(|l| l.is_final())
        .map(|l| {
            let marker = l.wants_marker()
                && l.sid
                    .as_deref()
                    .is_some_and(|sid| tr.has(sid, &format!("[kith wake {}]", l.run)));
            (l.run.clone(), marker)
        })
        .collect();
    let probes = seen
        .legacy
        .iter()
        .map(|(s, t)| (s.clone(), t.run.clone(), queue::probe(out, tr, t)))
        .collect();
    Asked {
        looks,
        final_runs,
        probes,
        asks: asks::look(&seen.asks, out),
    }
}

/// What a tick did.
#[derive(Default)]
struct Done {
    fired: usize,
    settled: Vec<(String, Verdict)>,
    unparked: Vec<i64>,
    taken: Vec<(String, String, i64)>,
    asks: Vec<String>,
}

impl Done {
    fn json(&self) -> Value {
        let word = |v: &Verdict| match v {
            Verdict::Answered => "answered",
            Verdict::Recovered => "recovered",
            Verdict::Withdrawn => "withdrawn",
            Verdict::Back { try_: true } => "tried",
            Verdict::Back { try_: false } => "back",
        };
        json!({
            "fired": self.fired,
            "settled": self.settled.iter().map(|(r, v)| json!({"run": r, "how": word(v)})).collect::<Vec<_>>(),
            "unparked": self.unparked,
            "taken": self.taken.iter().map(|(r, s, t)| json!({"run": r, "session": s, "turn": t})).collect::<Vec<_>>(),
            "asks": self.asks,
        })
    }
}

fn phase_two(
    core: &mut Core,
    out: &dyn Outside,
    seen: &Seen,
    asked: Asked,
    now: f64,
) -> Result<Done> {
    let tx = core.claim_tx()?;
    let before = load(&tx)?;
    let mut after = before.clone();
    after.prune(now);
    let mut done = Done::default();
    let ms = now_ms(now);

    // 1. R7: a waiter's result is written only if its row is as it was looked at.
    for l in asked.looks {
        if l.apply(&mut after, now) {
            done.fired += 1;
        }
    }

    // 2. R4: the runs whose turn was final in the snapshot (and still carry the same turn). A turn that went final since
    //    is the next tick's: its marker was not read after it was seen final.
    let (linked, legacy) = settle::runs_in(&tx, &after)?;
    for run in linked.iter().filter(|r| r.is_final()) {
        let Some(marker) = asked.final_runs.get(&run.run) else {
            continue;
        };
        if !seen
            .linked
            .iter()
            .any(|s| s.run == run.run && s.turn == run.turn && s.is_final())
        {
            continue;
        }
        let verdict = settle::settle_in(&tx, &mut after, now, run, *marker)?;
        done.settled.push((run.run.clone(), verdict));
    }
    // The old rule for a run held by a process's pid (imported from the files): the probes were made outside.
    for (session, run, probe) in &asked.probes {
        if legacy.iter().any(|(s, t)| s == session && t.run == *run) {
            queue::recover_probed(&mut after, session, run, probe, now);
        }
    }

    // 3. The backstop of `wake.released`: a parked wake turn of a conversation nothing holds goes back to queued.
    for conversation in parked_wake_conversations(&tx)? {
        done.unparked.extend(claims::unpark_ready_in(
            &tx,
            &conversation,
            true,
            false,
            ms,
        )?);
    }

    // 4. Take what is due, either door: one run and one turn each, parked `terminal_live` when a writer holds the session.
    turns::fail_foreign_in(&tx, &mut after, now)?;
    for run in queue::take_due(&mut after, out, now) {
        let text = |k: &str| run.get(k).and_then(Value::as_str).unwrap_or_default();
        let (id, session) = (text("run").to_owned(), text("session").to_owned());
        match turns::start_claims_in(&tx, &run, None, now) {
            Ok(turn) => done.taken.push((id, session, turn)),
            // A turn the claims lane refuses (a person with no standing) is a failed try, so it ends instead of looping.
            Err(_) => {
                queue::give_back(&mut after, now, &id, false);
            }
        }
    }

    // The ask part of the tick (A1 of the ask design) plugs in here: it runs in this transaction, after the queue.
    done.asks = ask_part(&tx, &mut after, &seen.folders, &seen.asks, &asked.asks, ms)?;

    turns::close_gone_in(&tx, &before, &after, now)?;
    close_orphans(&tx, &after, ms)?;
    save(&tx, &before, &after)?;
    tx.commit()?;
    Ok(done)
}

/// The ask design's part of the tick (R4, `asks/sweep.rs`): in the tick's own transaction, after the queue's work, it settles
/// the asks whose run ended with no report, and owes their askers the wakes through the same queue state. What it needed from
/// the world (out files, the process table) was read between the phases.
fn ask_part(
    tx: &rusqlite::Connection,
    st: &mut State,
    folders: &Paths,
    seen: &asks::Seen,
    looked: &asks::Looked,
    now_ms: i64,
) -> Result<Vec<String>> {
    asks::sweep_in(tx, st, &folders.machine, seen, looked, now_ms)
}

#[allow(clippy::cast_possible_truncation)]
fn now_ms(now: f64) -> i64 {
    (now * 1000.0) as i64
}

/// The conversations that have a parked turn of a wake run.
fn parked_wake_conversations(tx: &rusqlite::Connection) -> Result<Vec<String>> {
    let mut st = tx.prepare(
        "SELECT DISTINCT t.conversation FROM wake_turns w JOIN turns t ON t.id = w.turn_id
         WHERE t.state = 'parked' ORDER BY t.conversation",
    )?;
    let rows = st.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// No orphan waiting turn: a link whose run is no longer in the queue (written by an older step) has its waiting turn closed.
fn close_orphans(tx: &rusqlite::Connection, after: &State, ms: i64) -> Result<()> {
    let mut st = tx.prepare("SELECT run, turn_id FROM wake_turns")?;
    let links: Vec<(String, i64)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    drop(st);
    for (run, turn) in links {
        let carried = after
            .items
            .values()
            .any(|it| it.turn.as_ref().is_some_and(|t| t.run == run));
        if !carried {
            claims::end_waiting_turn_in(tx, turn, false, ms)?;
            tx.execute("DELETE FROM wake_turns WHERE run = ?1", [&run])?;
        }
    }
    Ok(())
}

/// `wake.tick`.
pub(crate) fn tick(access: &impl CoreLock, params: &Value, world: &World) -> Result<Value> {
    let seen = access.with_core(|c| phase_one(c, params))?;
    let (out, tr) = world(&seen.folders);
    let now = out.now();
    let asked = ask(&seen, &*out, &*tr, now);
    let done = access.with_core(|c| phase_two(c, &*out, &seen, asked, now))?;
    Ok(done.json())
}
