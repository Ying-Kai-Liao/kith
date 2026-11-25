//! Asks across computers, the core's rows (design 2026-11-24-ask-ledger §9, A5a). The host carries the words between two
//! computers; the rows on each end are the ledger's own, one writer each:
//!
//! - **The caller's end** (A): an ask of `mode` `peer`, with no local turn. `ask.peerOwed` says what its host owes the link
//!   (send, follow, stop); `ask.peerApply` copies the other computer's rows in one transaction, once each, one wake per row,
//!   and nothing after the ask was stopped or ended in any final state; the tick cuts off an ask whose computer is away past
//!   the wait or whose link ended ([`cut_off_in`]).
//! - **The callee's end** (B): an ordinary `bg` ask carrying `origin` (computer, key, epoch). A send that is made again finds
//!   its row. Every refusal is a row ([`refused_origin_in`]).
//! - **The fence** (`PeerFence.tla`): `ask_peers (computer, epoch, live)`. Opening ([`live_in`] in `ask.open`), claiming
//!   ([`cut_origin_in`] in the claim) and attaching (the same test in `claim.attach`) each read it in their own transaction,
//!   so an epoch that was ended starts nothing, with or without [`end_epoch_in`]'s sweep (each is enough alone: model
//!   variants v6 and v7). `ask.endPeer` ends the epoch and sweeps its asks in one transaction, and the host calls it before it
//!   removes a pairing; `ask.peerLink` writes the epoch first when a pairing starts.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use kith_kernel::delegation as d;
use rusqlite::{Connection, OptionalExtension as _, params};
use serde_json::{Value, json};

use super::ledger::owe_in;
use super::rows::{self, Kind};
use super::{Ask, EndReq, LedgerOpen, end_tree_in, get, load_all, regs, settle_in};
use crate::conversations::claims::Kill;
use crate::db::Core;
use crate::error::{Error, Result};
use crate::wake::{self, State};

/// How long a computer may be away from an ask before the ask is cut off here (Kai, 2026-11-25: 7 days, the one number the
/// away wait of every kind shares).
pub(crate) const AWAY_MS: i64 = 7 * 24 * 3600 * 1000;

/// The key a computer's reported cost is booked under in `ask_costs`: once per ask, whatever turn numbers there are.
const PEER_COST: i64 = -1;

/// The states a computer may report an ask in.
const STATES: [&str; 6] = ["open", "answered", "capped", "stopped", "lost", "refused"];

#[allow(clippy::cast_precision_loss)]
fn secs(now: i64) -> f64 {
    now as f64 / 1000.0
}

/// Whether `epoch` is the live pairing with `computer`, read in the caller's transaction.
pub(crate) fn live_in(tx: &Connection, computer: &str, epoch: i64) -> Result<bool> {
    Ok(tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM ask_peers WHERE computer = ?1 AND epoch = ?2 AND live = 1)",
        params![computer, epoch],
        |r| r.get(0),
    )?)
}

/// The live pairing epoch with `computer`, if any.
fn epoch_in(tx: &Connection, computer: &str) -> Result<Option<i64>> {
    Ok(tx
        .query_row(
            "SELECT epoch FROM ask_peers WHERE computer = ?1 AND live = 1",
            [computer],
            |r| r.get(0),
        )
        .optional()?)
}

/// The fence of a claim and an attach: the computer whose pairing `turn`'s ask came under, when that epoch is no longer the
/// live one. None for every turn that is not an origin ask's.
pub(crate) fn cut_origin_in(tx: &Connection, turn: i64) -> Result<Option<String>> {
    Ok(tx
        .query_row(
            "SELECT a.origin_computer FROM turns t JOIN asks a ON a.turn = t.id
             WHERE t.id = ?1 AND t.source = 'ask' AND a.origin_computer IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM ask_peers p
                               WHERE p.computer = a.origin_computer AND p.epoch = a.origin_epoch AND p.live = 1)",
            [turn],
            |r| r.get(0),
        )
        .optional()?)
}

/// The row a send made, by who asked and the caller's ask id.
pub(super) fn by_origin_in(tx: &Connection, computer: &str, key: &str) -> Result<Option<Ask>> {
    let id: Option<String> = tx
        .query_row(
            "SELECT id FROM asks WHERE origin_computer = ?1 AND origin_key = ?2",
            params![computer, key],
            |r| r.get(0),
        )
        .optional()?;
    id.map_or(Ok(None), |id| get(tx, &id))
}

/// What an open that is made again answers: the row it made, with its turn when it has one.
pub(super) fn replayed(old: &Ask) -> Value {
    json!({"decision": null, "record": Value::Object(old.to_reg()), "turn": old.turn, "out": old.out_path,
           "state": old.state, "peer": old.peer, "replayed": true})
}

/// Whether this is a person other than the owner: a guest's ask never crosses (D5).
fn is_guest(person: Option<&str>) -> bool {
    person.is_some_and(|p| !p.is_empty() && p != "owner")
}

/// The checks of a `peer` open that need the rows: the caller is the owner's (and so is everyone up its chain), and the
/// computer is paired. Answers the live epoch the ask is opened under.
pub(super) fn open_peer_check_in(
    tx: &Connection,
    asks: &[Ask],
    place: &d::Place,
    env: &dyn Fn(&str) -> Option<String>,
    l: &LedgerOpen<'_>,
) -> Result<i64> {
    let Some(computer) = l.peer.filter(|c| !c.is_empty()) else {
        return Err(Error::BadParams(
            "kith-core: asks: a peer ask needs the computer asked (peer)".into(),
        ));
    };
    if l.task.is_none_or(str::is_empty) {
        return Err(Error::BadParams(
            "kith-core: asks: a peer ask needs its task".into(),
        ));
    }
    let mut guest = is_guest(env("KITH_PERSON").as_deref());
    let mut up = place.ask.as_deref();
    for _ in 0..=asks.len() {
        let Some(p) = up.and_then(|p| asks.iter().find(|a| a.id == p)) else {
            break;
        };
        guest |= is_guest(p.caller_person.as_deref());
        up = p.parent.as_deref();
    }
    if guest {
        return Err(Error::Refused(
            "kith-core: asks: only the owner's runs may ask an agent on another computer".into(),
        ));
    }
    epoch_in(tx, computer)?.ok_or_else(|| {
        Error::Refused(format!(
            "kith-core: asks: this computer is not paired with {computer}"
        ))
    })
}

/// A refusal of an ask from another computer, as a row it can read back: a `bg` ask that is `refused` at once, with one `lost`
/// row giving the words. The caller's ask id decides the row, so the same send again finds it. An id that has a record
/// already refuses as it did (nothing is overwritten).
pub(super) fn refused_origin_in(
    tx: &rusqlite::Transaction<'_>,
    o: &super::Open<'_>,
    why: &str,
    now: i64,
) -> Result<Value> {
    let Some(og) = o.ledger.as_ref().and_then(|l| l.origin) else {
        return Err(Error::Refused(why.to_owned()));
    };
    if get(tx, o.id)?.is_some() {
        return Err(Error::Refused(why.to_owned()));
    }
    let ask = Ask {
        id: o.id.to_owned(),
        chain: o.id.to_owned(),
        parent: None,
        reply_session: None,
        hop: 0,
        cross_hops: 0,
        crossed: true,
        asked: Some(o.asked.to_owned()),
        caller_agent: None,
        caller_person: None,
        caller_via: None,
        conversation: None,
        cost: Some(0.0),
        cost_base: None,
        cap_usd: None,
        state: "refused".to_owned(),
        at_ms: Some(now),
        answered_ms: Some(now),
        capped_ms: None,
        cap_card_ms: None,
        ended_ms: Some(now),
        pid: None,
        pid_at_ms: None,
        brief: None,
        done: true,
        delivered: false,
        woke_answer: false,
        extra: None,
        claim_id: None,
        mode: Some("bg".to_owned()),
        task: o.ledger.as_ref().and_then(|l| l.task.map(str::to_owned)),
        out_path: None,
        parked_card: None,
        parked_ms: None,
        turn: None,
        cli_pid: None,
        cli_start: None,
        session: None,
        peer: None,
        peer_epoch: None,
        sent: false,
        fetched: 0,
        stop_owed: false,
        peer_seen_ms: None,
        origin_computer: Some(og.computer.to_owned()),
        origin_key: Some(og.key.to_owned()),
        origin_epoch: Some(og.epoch),
    };
    ask.put(tx)?;
    rows::add_in(
        tx,
        o.id,
        Some(o.asked),
        Kind::Lost,
        &format!("refused: {why}"),
        now,
    )?;
    let mut said = replayed(&ask);
    said["replayed"] = json!(false);
    said["refused"] = json!(true);
    said["reason"] = json!(why);
    Ok(said)
}

/// Closes a peer ask here as `cut_off`: its asker gets one `lost` row with `text` and the wake for it, and `stop_owed` says
/// whether the computer must still be told to stop (it may be running the ask yet).
fn cut_off_ask_in(
    tx: &Connection,
    st: &mut State,
    mut a: Ask,
    text: &str,
    owe_stop: bool,
    now: i64,
) -> Result<()> {
    "cut_off".clone_into(&mut a.state);
    a.done = true;
    a.answered_ms = Some(now);
    a.stop_owed = owe_stop;
    if let Some(n) = rows::add_in(tx, &a.id, a.asked.as_deref(), Kind::Lost, text, now)? {
        owe_in(st, now, &a, n, Kind::Lost, text);
    }
    a.put(tx)
}

fn is_live(a: &Ask) -> bool {
    a.state == "open" && a.ended_ms.is_none()
}

/// The tick's cut-off (D4): an open peer ask whose pairing is no longer the live one is cut off at once, and one whose
/// computer has not answered for [`AWAY_MS`] is cut off with its stop still owed. Each writes one notice row and owes one
/// wake; after it the ask is final, so nothing of the computer's reaches the asker (never both a final row and the notice).
/// Answers the ids.
pub(crate) fn cut_off_in(tx: &Connection, st: &mut State, now: i64) -> Result<Vec<String>> {
    let mut cut = Vec::new();
    let open: Vec<Ask> = load_all(tx)?
        .into_iter()
        .filter(|a| a.mode.as_deref() == Some("peer") && is_live(a))
        .collect();
    for a in open {
        let (Some(computer), Some(epoch)) = (a.peer.clone(), a.peer_epoch) else {
            continue;
        };
        let who = format!("{}@{computer}", a.asked.as_deref().unwrap_or("the agent"));
        let seen = a.peer_seen_ms.or(a.at_ms).unwrap_or(0);
        if !live_in(tx, &computer, epoch)? {
            let text = format!("{who} was cut off (the link ended); it may have finished there");
            let id = a.id.clone();
            cut_off_ask_in(tx, st, a, &text, false, now)?;
            cut.push(id);
        } else if now - seen > AWAY_MS {
            let text = format!("{computer} hasn't answered for 7 days; this ask is closed here");
            let id = a.id.clone();
            cut_off_ask_in(tx, st, a, &text, true, now)?;
            cut.push(id);
        }
    }
    Ok(cut)
}

/// What ending an epoch did.
#[derive(Debug, Default)]
pub(crate) struct EndedPeer {
    /// The asks it ended: stopped (running), refused (not started), cut off (the caller's end).
    pub ids: Vec<String>,
    pub sessions: Vec<String>,
    pub kill: Vec<Kill>,
}

/// Ends the pairing with `computer` and sweeps its asks, in the caller's transaction (`st` is the wake queue's state the
/// caller saves):
/// - the fence goes first (`live = 0`), so nothing opened, claimed or attached after this reads a live epoch;
/// - the callee's end: its running asks (and what they asked) are stopped, the run's claim to `ending` for the waiter to
///   kill; its queued and claimed ones are refused with a row (`end_tree_in` ends their turn and unspawned claim, so no attach
///   can win);
/// - the caller's end: every open ask of that computer is cut off with one row (the link ended: it may have finished
///   there) and every stop owed to it is dropped, since there is no link left to carry it.
///
/// Repeating it changes nothing.
pub(crate) fn end_epoch_in(
    tx: &rusqlite::Transaction<'_>,
    st: &mut State,
    computer: &str,
    now: i64,
) -> Result<EndedPeer> {
    tx.execute(
        "UPDATE ask_peers SET live = 0 WHERE computer = ?1",
        [computer],
    )?;
    let mut out = EndedPeer::default();
    // The callee's end. A turn that has attached (`running`) is a run to stop; any other open ask of the epoch never started.
    let all: Vec<(String, bool)> = {
        let mut stmt = tx.prepare(
            "SELECT a.id, COALESCE((SELECT t.state FROM turns t WHERE t.id = a.turn), '') = 'running'
             FROM asks a WHERE a.origin_computer = ?1 AND a.state = 'open' AND a.ended_ms IS NULL ORDER BY a.id",
        )?;
        stmt.query_map([computer], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?))
        })?
        .collect::<std::result::Result<_, _>>()?
    };
    let ids: Vec<String> = all.iter().map(|(id, _)| id.clone()).collect();
    let started: BTreeSet<&str> = all
        .iter()
        .filter(|(_, run)| *run)
        .map(|(id, _)| id.as_str())
        .collect();
    if !ids.is_empty() {
        let ended = end_tree_in(
            tx,
            &EndReq {
                roots: &ids,
                sessions: &[],
                persons: &[],
            },
            now,
        )?;
        for s in &ended.sessions {
            wake::withdraw_in(tx, st, secs(now), s, None)?;
        }
        out.ids.extend(ended.ids);
        out.sessions = ended.sessions;
        out.kill = ended.kill;
        for id in ids.iter().filter(|id| !started.contains(id.as_str())) {
            let n = tx.execute(
                "UPDATE asks SET state = 'refused' WHERE id = ?1 AND state = 'stopped'",
                [id],
            )?;
            if n == 1 {
                rows::add_in(
                    tx,
                    id,
                    None,
                    Kind::Lost,
                    "refused: the link ended before it started",
                    now,
                )?;
            }
        }
    }
    // The caller's end.
    let peers: Vec<Ask> = load_all(tx)?
        .into_iter()
        .filter(|a| a.peer.as_deref() == Some(computer) && is_live(a))
        .collect();
    for a in peers {
        let who = format!("{}@{computer}", a.asked.as_deref().unwrap_or("the agent"));
        let text = format!("{who} was cut off (the link ended); it may have finished there");
        out.ids.push(a.id.clone());
        cut_off_ask_in(tx, st, a, &text, false, now)?;
    }
    tx.execute(
        "UPDATE asks SET stop_owed = 0 WHERE peer = ?1 AND stop_owed = 1",
        [computer],
    )?;
    out.ids.sort();
    out.ids.dedup();
    Ok(out)
}

/// `ask.endPeer`: [`end_epoch_in`] in one transaction.
pub(crate) fn end(core: &mut Core, computer: &str, now: i64) -> Result<EndedPeer> {
    wake::write_tx(core, secs(now), |tx, st| {
        end_epoch_in(tx, st, computer, now)
    })
}

/// `ask.peerLink` (a pairing starts, the widening write): `epoch` is the live one for `computer`. A newer epoch than the one
/// kept replaces it, ending that one first in the same transaction; the same epoch again changes nothing (an ended epoch is
/// never made live again); an older one is refused. Answers whether it linked, and what the old epoch's end ended.
pub(crate) fn link(
    core: &mut Core,
    computer: &str,
    epoch: i64,
    now: i64,
) -> Result<(bool, EndedPeer)> {
    if epoch < 0 {
        return Err(Error::BadParams(
            "kith-core: asks: epoch is not negative".into(),
        ));
    }
    wake::write_tx(core, secs(now), |tx, st| {
        let old: Option<(i64, bool)> = tx
            .query_row(
                "SELECT epoch, live FROM ask_peers WHERE computer = ?1",
                [computer],
                |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0)),
            )
            .optional()?;
        match old {
            Some((e, live)) if e == epoch => Ok((live, EndedPeer::default())),
            Some((e, _)) if e > epoch => Err(Error::Refused(format!(
                "kith-core: asks: {computer} is paired at epoch {e} already; epoch {epoch} is older"
            ))),
            _ => {
                let ended = if matches!(old, Some((_, true))) {
                    end_epoch_in(tx, st, computer, now)?
                } else {
                    EndedPeer::default()
                };
                tx.execute(
                    "INSERT INTO ask_peers (computer, epoch, live) VALUES (?1, ?2, 1)
                     ON CONFLICT (computer) DO UPDATE SET epoch = excluded.epoch, live = 1",
                    params![computer, epoch],
                )?;
                Ok((true, ended))
            }
        }
    })
}

/// One row of a computer's, as `peerThread` carried it.
pub(crate) struct InRow {
    pub n: i64,
    pub who: Option<String>,
    pub kind: Kind,
    pub text: String,
    pub at_ms: Option<i64>,
}

/// What `ask.peerApply` was told.
pub(crate) struct Apply<'a> {
    pub id: &'a str,
    pub epoch: i64,
    pub rows: Vec<InRow>,
    /// The ask's state on its computer; a final one ends the ask here.
    pub state: Option<&'a str>,
    /// What the run and its descendants cost there, in total (read once, at the end).
    pub cost: Option<f64>,
}

/// `ask.peerApply` (D3 following): copies what the computer's row of this ask says, in one transaction.
/// - each row once (unique on (ask, n); `fetched` moves up), with one owed wake per `answer` or `lost` row (R3);
/// - a final state ends the ask here, and the cost is booked once (`ask_costs`, [`PEER_COST`]); a computer that stopped it
///   on its own gives the asker one `lost` row unless the rows said how it ended;
/// - nothing is copied or changed once the ask is stopped, ended or final in any way (`StopWins`; the cut-off notice and a
///   computer's final row never both), except that its cost is still booked and a final state clears the stop owed;
/// - the call is the computer answering: `sent` is set and the away wait starts over;
/// - an answer from another epoch than the ask's is dropped.
pub(crate) fn apply(core: &mut Core, machine: &Path, f: &Apply<'_>, now: i64) -> Result<Value> {
    if f.state.is_some_and(|s| !STATES.contains(&s)) {
        return Err(Error::BadParams(
            "kith-core: asks: state is open, answered, capped, stopped, lost or refused".into(),
        ));
    }
    let end = f.state.filter(|s| *s != "open");
    wake::write_tx(core, secs(now), |tx, st| {
        let Some(a) = get(tx, f.id)? else {
            return Err(Error::Refused(format!("kith-core: asks: no ask {}", f.id)));
        };
        if a.mode.as_deref() != Some("peer") {
            return Err(Error::Refused(format!(
                "kith-core: asks: ask {} is not an ask of another computer",
                f.id
            )));
        }
        if a.peer_epoch != Some(f.epoch) {
            return Ok(json!({"id": f.id, "applied": false, "stale": true}));
        }
        let live = is_live(&a);
        // The computer's cost joins the chain once, whatever became of the ask here.
        if let (Some(_), Some(cost)) = (end, f.cost)
            && cost > 0.0
            && rows::count_cost_in(tx, f.id, PEER_COST)?
        {
            settle_in(tx, machine, f.id, cost, None, now)?;
        }
        let Some(mut a) = get(tx, f.id)? else {
            return Err(Error::Refused(format!("kith-core: asks: no ask {}", f.id)));
        };
        a.sent = true;
        a.peer_seen_ms = Some(now);
        let mut copied = Vec::new();
        if live {
            let mut sorted: Vec<&InRow> = f.rows.iter().collect();
            sorted.sort_by_key(|r| r.n);
            for r in sorted {
                if r.n <= a.fetched || r.n < 1 {
                    continue;
                }
                let fresh = rows::add_numbered_in(
                    tx,
                    f.id,
                    r.n,
                    r.who.as_deref().or(a.asked.as_deref()),
                    r.kind,
                    &r.text,
                    r.at_ms.unwrap_or(now),
                )?;
                a.fetched = r.n;
                if fresh {
                    owe_in(st, now, &a, r.n, r.kind, &r.text);
                    copied.push(r.n);
                }
            }
            if let Some(s) = end {
                s.clone_into(&mut a.state);
                a.done = true;
                a.answered_ms = Some(now);
                a.parked_card = None;
                a.parked_ms = None;
                // The computer stopped it itself and said nothing: the asker is told, once.
                if s == "stopped" && !rows::has_final_in(tx, f.id)? {
                    let text = format!(
                        "{}@{} was stopped there",
                        a.asked.as_deref().unwrap_or("the agent"),
                        a.peer.as_deref().unwrap_or("the computer")
                    );
                    if let Some(n) =
                        rows::add_in(tx, f.id, a.asked.as_deref(), Kind::Lost, &text, now)?
                    {
                        owe_in(st, now, &a, n, Kind::Lost, &text);
                        copied.push(n);
                    }
                }
            }
        }
        if end.is_some() {
            a.stop_owed = false;
        }
        a.put(tx)?;
        Ok(
            json!({"id": f.id, "applied": live, "rows": copied, "state": a.state, "fetched": a.fetched}),
        )
    })
}

/// `ask.peerOwed`: what the host owes the link now, from the rows (nothing is written). For each peer ask whose pairing is
/// live:
/// - `send`: opened here and not yet answered by its computer: `{id, key, computer, epoch, agent, task, cont, lineage}`;
/// - `follow`: sent and still open: `{id, key, computer, epoch, after}`, `after` the last row copied;
/// - `stop`: a stop its computer has not confirmed, whatever became of the ask here: `{id, key, computer, epoch}`.
///
/// The `key` is the ask's id. The `lineage` is the chain as the computer needs it to check the caps again.
pub(crate) fn owed(core: &Core) -> Result<Value> {
    let asks = load_all(core.conn())?;
    let costs = d::chain_costs(&regs(&asks));
    let linked: BTreeMap<String, i64> = {
        let mut st = core
            .conn()
            .prepare("SELECT computer, epoch FROM ask_peers WHERE live = 1")?;
        st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<std::result::Result<_, _>>()?
    };
    let (mut send, mut follow, mut stop) = (Vec::new(), Vec::new(), Vec::new());
    for a in asks.iter().filter(|a| a.mode.as_deref() == Some("peer")) {
        let (Some(computer), Some(epoch)) = (&a.peer, a.peer_epoch) else {
            continue;
        };
        if linked.get(computer) != Some(&epoch) {
            continue;
        }
        if is_live(a) && !a.sent {
            send.push(json!({
                "id": a.id, "key": a.id, "computer": computer, "epoch": epoch,
                "agent": a.asked, "task": a.task,
                "cont": a.extra.as_ref().and_then(|e| e.get("cont")).cloned(),
                "lineage": {"chain": a.chain, "hop": a.hop, "cross": a.cross_hops, "from": a.id,
                            "cost": costs.get(&a.chain).copied().unwrap_or(0.0)},
            }));
        } else if is_live(a) {
            follow.push(json!({"id": a.id, "key": a.id, "computer": computer, "epoch": epoch, "after": a.fetched}));
        }
        if a.stop_owed {
            stop.push(json!({"id": a.id, "key": a.id, "computer": computer, "epoch": epoch}));
        }
    }
    Ok(json!({"send": send, "follow": follow, "stop": stop}))
}
