//! The ask part of `wake.tick` (R4): the level-triggered half of the settle, on the host's clock. The host's waiter is the
//! fast path; this settles what no one reported, so no ask stays `open` once its run is gone (NoStuck):
//! - an `open` `bg` ask whose turn has ended (a host restart lost the waiter), with what its out file says;
//! - an `open` `fg` ask whose recorded process is gone (a caller killed with no handler: `kill -9`, a closed terminal, a
//!   Windows `TerminateProcess`), `lost`;
//! - the cost of a finished run whose report was lost is booked (cost only: no settle, no row, no wake, so a parked ask
//!   stays parked), once per turn through `ask_costs`, as `ask.finish` books it;
//! - settled asks and their rows older than 31 days are forgotten.
//!
//! Three steps, in the tick's own shape (`wake/tick.rs`): [`seen_in`] under the lock in phase 1, [`look`] with no lock held
//! (it reads files and asks the process table), [`settle_in`] under the lock in phase 2. A `bg` ask is settled only if its
//! turn was already final in phase 1: the out file was read after that, so a turn that goes final between the phases is the
//! next tick's (it could otherwise be settled `lost` with an answer that was not written yet). A `fg` ask is settled only if
//! it still has the process phase 1 saw.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use super::ledger::{self, Callee, Ended, Settled};
use super::rows;
use crate::error::Result;
use crate::wake::{Outside, State};

/// A turn in one of these states has ended (it will not run again).
const FINAL: &str = "('done', 'failed', 'cancelled', 'expired')";

/// A `bg` ask whose turn had ended when phase 1 looked.
pub(crate) struct Ended1 {
    id: String,
    turn: i64,
    out_path: Option<String>,
}

/// A `fg` ask and the process that was its caller.
pub(crate) struct Fg {
    id: String,
    pid: i64,
    start: Option<String>,
}

/// A run whose cost may not be booked yet: key `key` of ask `ask` (0: the run of an ask with no turn, a `fg` CLI's own, which
/// is over once its process is gone), and the out files its answer may be in.
pub(crate) struct CostSeen {
    ask: String,
    key: i64,
    paths: Vec<PathBuf>,
    /// The process that must be gone first (key 0).
    cli: Option<(i64, Option<String>)>,
    /// When the turn ended (ms; key 0: none). A run that gave no answer in [`GRACE_MS`] after it is booked at no cost.
    ended_ms: Option<i64>,
}

/// How long after a turn ended its out file may still show up (a host restart, a slow disk) before the tick gives up on it.
const GRACE_MS: i64 = 10 * 60 * 1000;

/// What phase 1 read.
#[derive(Default)]
pub(crate) struct Seen {
    bg: Vec<Ended1>,
    fg: Vec<Fg>,
    costs: Vec<CostSeen>,
}

/// What the world said, with no lock held.
#[derive(Default)]
pub(crate) struct Looked {
    /// Per `bg` ask id: its answer, when the out file held one.
    answers: BTreeMap<String, Option<Callee>>,
    /// The `fg` asks whose process is gone.
    gone: BTreeSet<String>,
    /// The cost each looked-at run's out file gave, by (ask, key).
    costs: BTreeMap<(String, i64), f64>,
}

/// Phase 1: the asks whose run may have ended.
pub(crate) fn seen_in(tx: &Connection, machine: &Path) -> Result<Seen> {
    let dir = machine.join("core").join("asks");
    let mut st = tx.prepare(&format!(
        "SELECT a.id, a.turn, a.out_path FROM asks a JOIN turns t ON t.id = a.turn
         WHERE a.state = 'open' AND a.ended_ms IS NULL AND a.parked_card IS NULL AND t.state IN {FINAL} ORDER BY a.id"
    ))?;
    let bg = st
        .query_map([], |r| {
            Ok(Ended1 {
                id: r.get(0)?,
                turn: r.get(1)?,
                out_path: r.get(2)?,
            })
        })?
        .collect::<std::result::Result<_, _>>()?;
    let mut st = tx.prepare(
        "SELECT id, cli_pid, cli_start FROM asks
         WHERE state = 'open' AND ended_ms IS NULL AND mode = 'fg' AND turn IS NULL AND cli_pid IS NOT NULL ORDER BY id",
    )?;
    let fg = st
        .query_map([], |r| {
            Ok(Fg {
                id: r.get(0)?,
                pid: r.get(1)?,
                start: r.get(2)?,
            })
        })?
        .collect::<std::result::Result<_, _>>()?;
    // Runs whose cost no report booked: a `fg` CLI's own run (key 0), and each finished turn made for an ask (`ask_turns`).
    let mut costs = Vec::new();
    let mut st = tx.prepare(
        "SELECT id, cli_pid, cli_start FROM asks
         WHERE mode = 'fg' AND cli_pid IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM ask_costs c WHERE c.ask = asks.id AND c.turn = 0) ORDER BY id",
    )?;
    for r in st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, Option<String>>(2)?,
        ))
    })? {
        let (id, pid, start) = r?;
        costs.push(CostSeen {
            paths: vec![dir.join(format!("{id}.out"))],
            ask: id,
            key: 0,
            cli: Some((pid, start)),
            ended_ms: None,
        });
    }
    let mut st = tx.prepare(&format!(
        "SELECT k.ask, k.turn, k.out_path, t.finished_at
         FROM ask_turns k JOIN turns t ON t.id = k.turn
         WHERE t.state IN {FINAL}
           AND NOT EXISTS (SELECT 1 FROM ask_costs c WHERE c.ask = k.ask AND c.turn = k.turn) ORDER BY k.ask, k.turn"
    ))?;
    for r in st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<i64>>(3)?,
        ))
    })? {
        let (id, turn, out_path, ended) = r?;
        costs.push(CostSeen {
            ask: id,
            key: turn,
            paths: vec![PathBuf::from(out_path)],
            cli: None,
            ended_ms: Some(ended.unwrap_or(0)),
        });
    }
    Ok(Seen { bg, fg, costs })
}

/// Between the phases, with no lock held: the out files and the process table.
pub(crate) fn look(seen: &Seen, out: &dyn Outside) -> Looked {
    let mut looked = Looked::default();
    #[allow(clippy::cast_possible_truncation)]
    let now_ms = (out.now() * 1000.0) as i64;
    for b in &seen.bg {
        let answer = b
            .out_path
            .as_deref()
            .and_then(|p| out.read_text(Path::new(p)))
            .and_then(|t| ledger::parse_callee(&t));
        looked.answers.insert(b.id.clone(), answer);
    }
    for f in &seen.fg {
        if !out.alive(f.pid, f.start.as_deref()) {
            looked.gone.insert(f.id.clone());
        }
    }
    for c in &seen.costs {
        if c.cli
            .as_ref()
            .is_some_and(|(pid, start)| out.alive(*pid, start.as_deref()))
        {
            continue;
        }
        let said = c
            .paths
            .iter()
            .find_map(|p| out.read_text(p).and_then(|t| ledger::parse_callee(&t)));
        if let Some(callee) = said {
            looked.costs.insert((c.ask.clone(), c.key), callee.cost);
        } else if c.ended_ms.is_some_and(|e| now_ms - e > GRACE_MS) {
            // No answer after the grace: the run gave none, so it is booked once at no cost and not looked for again.
            looked.costs.insert((c.ask.clone(), c.key), 0.0);
        }
    }
    looked
}

/// Phase 2, in the tick's transaction: settles what was seen and looked at, then forgets the old. `st` is the wake queue's
/// state the tick saves. Answers the ids settled.
pub(crate) fn settle_in(
    tx: &Connection,
    st: &mut State,
    machine: &Path,
    seen: &Seen,
    looked: &Looked,
    now: i64,
) -> Result<Vec<String>> {
    let mut settled = Vec::new();
    for b in &seen.bg {
        // Still the same turn, and still ended: a requeue of the ask (not made today) would not be settled from old news.
        let same: bool = tx.query_row(
            &format!(
                "SELECT EXISTS (SELECT 1 FROM asks a JOIN turns t ON t.id = a.turn
                                WHERE a.id = ?1 AND a.turn = ?2 AND t.state IN {FINAL})"
            ),
            rusqlite::params![b.id, b.turn],
            |r| r.get(0),
        )?;
        let Some(answer) = looked.answers.get(&b.id).filter(|_| same) else {
            continue;
        };
        let ended = answer.clone().map_or(Ended::Lost, Ended::Answered);
        if let Settled::Now(..) = ledger::settle_ask_in(
            tx,
            st,
            machine,
            &b.id,
            &ended,
            None,
            Some(b.turn),
            true,
            now,
        )? {
            settled.push(b.id.clone());
        }
    }
    for f in &seen.fg {
        if !looked.gone.contains(&f.id) {
            continue;
        }
        let same: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM asks WHERE id = ?1 AND turn IS NULL AND cli_pid = ?2 AND cli_start IS ?3)",
            rusqlite::params![f.id, f.pid, f.start],
            |r| r.get(0),
        )?;
        if !same {
            continue;
        }
        if let Settled::Now(..) =
            ledger::settle_ask_in(tx, st, machine, &f.id, &Ended::Lost, None, None, true, now)?
        {
            settled.push(f.id.clone());
        }
    }
    // A finished run whose report was lost still owes the chain its cost: booked once per turn, and nothing else (a parked ask
    // stays parked, no row, no wake).
    for ((ask, key), cost) in &looked.costs {
        if rows::count_cost_in(tx, ask, *key)? && *cost > 0.0 {
            super::settle_in(tx, machine, ask, *cost, None, now)?;
        }
    }
    // The asks of other computers: one whose link ended, or whose computer is away past the wait, is cut off (D4).
    settled.extend(super::peer::cut_off_in(tx, st, now)?);
    // The asks other computers sent this one that nobody started in a day expire (by this computer's clock).
    settled.extend(super::peer_b::expire_unstarted_in(tx, machine, now)?);
    rows::forget_old_in(tx, now)?;
    Ok(settled)
}
