//! The ask as a job on the ledger (P2-3, design 2026-11-24-ask-ledger): the `bg` turn an open makes (R1), the settle by turn
//! (R3), the mailbox verbs `post` and `read` (R5, R9) and the card park (R7).
//!
//! **One settle, three callers.** [`settle_ask_in`] is the one place an ask leaves `open` through a run's end: the host's
//! report (`ask.finish`), the tick (`sweep.rs`) and a `fg` caller's own report all go through it. It moves only an `open`
//! ask that Stop did not end, so a second caller finds nothing to do (the host's report and the tick settle once) and an
//! answer on its way after `ask.end` is dropped (`StopWins`). The answer becomes a mailbox row and the asker is owed the wake
//! `ask:<id>/<n>` in the same transaction, whether or not a writer holds the asker's session: the wake queue parks the turn
//! while a terminal holds it (S4 R2.5).

use std::path::Path;

use rusqlite::Connection;
use serde_json::{Value, json};

use super::rows::{self, Kind};
use super::{Ask, LedgerOpen, get, settle_in};
use crate::conversations::claims::{self, NewTurn};
use crate::error::{Error, Result};
use crate::wake::{self, State};

/// The engine of an ask's turn when neither the caller nor the conversation names one.
const DEFAULT_ENGINE: &str = "claude";

/// The runs a wake resumes itself (`RUN_VIAS` of kith-wake): a caller of these needs no person's door.
const RUN_VIAS: [&str; 4] = ["terminal", "ask", "schedule", "wake"];

/// The callee's answer, as its JSON result gave it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Callee {
    pub text: String,
    pub cost: f64,
    pub is_error: bool,
    pub session: Option<String>,
}

/// The callee's JSON result in `text`: the whole text, else the last line that is one (a launcher may print a line first).
/// None when there is none, which settles the ask `lost`.
pub(crate) fn parse_callee(text: &str) -> Option<Callee> {
    let one = |t: &str| -> Option<Callee> {
        let v: Value = serde_json::from_str(t.trim()).ok()?;
        let o = v.as_object()?;
        if !["result", "is_error", "total_cost_usd"]
            .iter()
            .any(|k| o.contains_key(*k))
        {
            return None;
        }
        Some(Callee {
            text: o
                .get("result")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            cost: o
                .get("total_cost_usd")
                .and_then(Value::as_f64)
                .unwrap_or(0.0),
            is_error: o.get("is_error").and_then(Value::as_bool).unwrap_or(false),
            session: o
                .get("session_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
    };
    one(text).or_else(|| text.lines().rev().find_map(one))
}

/// Gives a `bg` ask its turn (R1): the callee's conversation (`ask:<id>`, or the one named for `-c`), a turn of source `ask`
/// whose prompt is the task and whose person is the asker's, and the file the host's run writes the answer to. The caller
/// writes the row.
pub(super) fn with_turn_in(
    tx: &rusqlite::Transaction<'_>,
    machine: &Path,
    mut ask: Ask,
    l: &LedgerOpen<'_>,
    now: i64,
) -> Result<Ask> {
    let Some(task) = l.task.filter(|t| !t.is_empty()) else {
        return Err(Error::BadParams(
            "kith-core: asks: a --bg ask needs its task".into(),
        ));
    };
    let (turn, out, conversation) = new_turn_in(tx, machine, &ask, task, l.engine, false, now)?;
    ask.conversation = Some(conversation);
    ask.turn = Some(turn);
    ask.out_path = Some(out);
    Ok(ask)
}

/// A turn of source `ask` for the ask's callee, with the prompt `prompt`: the first one of a `bg` open, or the resume a card's
/// answer makes (`again`, with an out file of its own, so the earlier run's answer is not read as this one's). Answers
/// (turn, out file, conversation).
fn new_turn_in(
    tx: &rusqlite::Transaction<'_>,
    machine: &Path,
    ask: &Ask,
    task: &str,
    engine: Option<&str>,
    again: bool,
    now: i64,
) -> Result<(i64, String, String)> {
    let Some(agent) = ask.asked.clone().filter(|a| !a.is_empty()) else {
        return Err(Error::BadParams(
            "kith-core: asks: a --bg ask needs the agent asked".into(),
        ));
    };
    let conversation = ask
        .conversation
        .clone()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| format!("ask:{}", ask.id));
    let person = ask
        .caller_person
        .clone()
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| "owner".to_owned());
    let kept: Option<String> = rusqlite::OptionalExtension::optional(tx.query_row(
        "SELECT engine FROM conversations WHERE id = ?1",
        [&conversation],
        |r| r.get(0),
    ))?;
    let engine = engine
        .map(str::to_owned)
        .or(kept)
        .unwrap_or_else(|| DEFAULT_ENGINE.to_owned());
    let turn = claims::enqueue_turn_in(
        tx,
        &NewTurn {
            conversation: &conversation,
            agent: &agent,
            engine: &engine,
            source: "ask",
            person: &person,
            prompt: task,
            deadline: None,
            revoke_seen: claims::revoke_epoch_in(tx, &person)?,
        },
        now,
    )?;
    let name = if again {
        format!("{}-{turn}.out", ask.id)
    } else {
        format!("{}.out", ask.id)
    };
    let out = machine
        .join("core")
        .join("asks")
        .join(name)
        .to_string_lossy()
        .into_owned();
    Ok((turn, out, conversation))
}

/// How a run ended, for [`settle_ask_in`].
pub(crate) enum Ended {
    /// The callee gave its JSON result.
    Answered(Callee),
    /// Its turn ended, or its caller died, with no result.
    Lost,
}

/// What a settle did.
#[derive(Debug, PartialEq)]
pub(crate) enum Settled {
    /// The ask left `open`: its new state and the mailbox row it got (None when its caller was waiting in its own process).
    Now(String, Option<i64>),
    /// Nothing to do: already settled, or Stop ended it.
    Nothing,
    /// A lost run of an ask that waits on a card stays open: the card's answer resumes it.
    Parked,
}

/// The wake owed for row `n`: the asker's session is woken with the text, `door` as the old `hand_over` chose it. A session
/// the wake queue refuses (it belongs to another agent or person) leaves the row for `--wait`, as before.
fn owe_in(st: &mut State, now: i64, a: &Ask, n: i64, kind: Kind, text: &str) {
    let (Some(session), Some(agent)) = (&a.reply_session, &a.caller_agent) else {
        return;
    };
    if session.is_empty() || agent.is_empty() || kind == Kind::Progress {
        return;
    }
    let person = a.caller_person.as_deref().filter(|p| !p.is_empty());
    let run = person.is_none()
        && a.caller_via
            .as_deref()
            .is_some_and(|v| RUN_VIAS.contains(&v));
    let who = a.asked.as_deref().unwrap_or("the agent");
    let why = if kind == Kind::Lost {
        format!("{who} stopped before answering your kith ask {}", a.id)
    } else {
        format!("{who} answered your kith ask {}", a.id)
    };
    #[allow(clippy::cast_precision_loss)]
    let secs = now as f64 / 1000.0;
    // Refused or duplicate: the row stays unread for `--wait`, or it was owed already.
    let _ = wake::enqueue(
        st,
        secs,
        &wake::New {
            session,
            id: &rows::wake_id(&a.id, n),
            agent,
            text,
            person,
            room: None,
            door: if run { "run" } else { "host" },
            why: &why,
        },
    );
}

/// R3: the ask's run ended. Moves an `open` ask that Stop did not end to `answered` (`stopped` when the callee said it
/// failed, `capped` when its cost took the chain to its cap) or `lost`, adds the cost to its chain, and, when `notify`, writes
/// the mailbox row and owes the asker its wake: all in the caller's transaction (`st` is the wake queue's state, saved by
/// the caller). A `fg` ask whose caller reports its own end (`notify` false) was answered on the caller's stdout, so it gets
/// no row and no wake and counts as delivered.
#[allow(clippy::too_many_arguments)]
pub(crate) fn settle_ask_in(
    tx: &Connection,
    st: &mut State,
    machine: &Path,
    id: &str,
    ended: &Ended,
    session: Option<&str>,
    turn: Option<i64>,
    swept: bool,
    now: i64,
) -> Result<Settled> {
    let Some(a) = get(tx, id)? else {
        return Ok(Settled::Nothing);
    };
    // The run's cost joins the chain once per turn, whatever became of the ask (parked, stopped, settled already).
    let spent = match ended {
        Ended::Answered(c) => c.cost,
        Ended::Lost => 0.0,
    };
    // An answer books its turn even at no cost, so the tick does not look for it again.
    // A report that names no turn, of a `bg` ask that has one, is not any turn's: the tick books that turn's cost from its out file.
    let nameless_bg = turn.is_none() && a.turn.is_some() && a.mode.as_deref() != Some("fg");
    let fresh = matches!(ended, Ended::Answered(_))
        && !nameless_bg
        && rows::count_cost_in(tx, id, turn.unwrap_or(0))?
        && spent > 0.0;
    // Every settle goes by the ask's current turn: a run of an earlier turn (before a card's resume) settles nothing.
    // No turn named: the run of an ask with no turn (a `fg` CLI's own first run), never the current turn of a resumed one.
    let current = match turn {
        Some(t) => a.turn == Some(t),
        None => a.turn.is_none(),
    };
    let live = a.state == "open" && a.ended_ms.is_none() && current;
    // A parked ask is waiting for its card: its run's end, with words or without, is not its answer (kith-ask lets park win).
    if !live || a.parked_card.is_some() {
        if fresh {
            settle_in(tx, machine, id, spent, None, now)?;
        }
        return Ok(if live {
            Settled::Parked
        } else {
            Settled::Nothing
        });
    }
    // Only the `fg` CLI's own report, on its first run, was answered on its stdout; a resumed run has no caller to print it.
    let notify = swept || a.turn.is_some() || a.mode.as_deref() != Some("fg");
    let state = match ended {
        Ended::Answered(c) if c.is_error => "stopped",
        Ended::Answered(_) => "answered",
        Ended::Lost => "lost",
    };
    settle_in(
        tx,
        machine,
        id,
        if fresh { spent } else { 0.0 },
        Some(state),
        now,
    )?;
    let sid = session
        .map(str::to_owned)
        .or_else(|| match ended {
            Ended::Answered(c) => c.session.clone(),
            Ended::Lost => None,
        })
        .or_else(|| a.session.clone());
    let mut a =
        get(tx, id)?.ok_or_else(|| Error::Refused(format!("kith-core: asks: no ask {id}")))?;
    a.done = true;
    a.parked_card = None;
    a.parked_ms = None;
    a.session = sid.clone().or(a.session.take());
    let n = if notify {
        let (kind, text) = match ended {
            Ended::Answered(c) => (Kind::Answer, c.text.clone()),
            Ended::Lost => (
                Kind::Lost,
                format!(
                    "{} stopped before answering; its session is {}",
                    a.asked.as_deref().unwrap_or("the agent"),
                    a.session
                        .as_deref()
                        .or(a.conversation.as_deref())
                        .unwrap_or("unknown")
                ),
            ),
        };
        let n = rows::add_in(tx, id, a.asked.as_deref(), kind, &text, now)?;
        if let Some(n) = n {
            owe_in(st, now, &a, n, kind, &text);
        }
        n
    } else {
        a.delivered = true;
        None
    };
    a.put(tx)?;
    Ok(Settled::Now(a.state, n))
}

#[allow(clippy::cast_precision_loss)]
fn secs(now: i64) -> f64 {
    now as f64 / 1000.0
}

/// What `ask.finish` was told.
pub(crate) struct Finish<'a> {
    pub id: &'a str,
    /// The out file's text, when the host read one: parsed here (as the tick parses what it reads).
    pub out: Option<&'a str>,
    /// The callee's answer as the caller has it already (a `fg` CLI).
    pub callee: Option<Callee>,
    pub session: Option<&'a str>,
    /// The turn whose run ended; the ask is settled only if it is the ask's current turn. Omitted: the run of an ask with no turn (a fg CLI's own first run). The host's report always names its turn.
    pub turn: Option<i64>,
}

/// `ask.finish` (R3): the run behind ask `id` ended.
pub(crate) fn finish(
    core: &mut crate::db::Core,
    machine: &Path,
    f: &Finish<'_>,
    now: i64,
) -> Result<Value> {
    let ended = match (f.callee.clone(), f.out.and_then(parse_callee)) {
        (Some(c), _) | (None, Some(c)) => Ended::Answered(c),
        (None, None) => Ended::Lost,
    };
    let done = wake::write_tx(core, secs(now), |tx, st| {
        let Some(a) = get(tx, f.id)? else {
            return Err(Error::Refused(format!("kith-core: asks: no ask {}", f.id)));
        };
        let said = settle_ask_in(tx, st, machine, f.id, &ended, f.session, f.turn, false, now)?;
        Ok((said, a.state))
    })?;
    Ok(match done {
        (Settled::Now(state, row), _) => {
            json!({"id": f.id, "settled": true, "state": state, "row": row})
        }
        (Settled::Parked, state) => {
            json!({"id": f.id, "settled": false, "state": state, "parked": true})
        }
        (Settled::Nothing, state) => json!({"id": f.id, "settled": false, "state": state}),
    })
}

/// `ask.post` (R9): one row for the asker, appended (`kith-send --to ask:ID`, `kith ask --answer`). An `answer` also owes
/// the asker a wake; a `progress` waits for `--wait`. The same row again is a duplicate. An ask Stop ended keeps nothing
/// (`StopWins`). Answers `{id, row}` (`row` null for a duplicate), or `{id, stopped: true}`.
pub(crate) fn post(
    core: &mut crate::db::Core,
    id: &str,
    text: &str,
    who: Option<&str>,
    kind: Kind,
    now: i64,
) -> Result<Value> {
    if kind == Kind::Lost {
        return Err(Error::BadParams(
            "kith-core: asks: kind is answer or progress".into(),
        ));
    }
    wake::write_tx(core, secs(now), |tx, st| {
        let Some(a) = get(tx, id)? else {
            return Err(Error::Refused(format!("kith-core: asks: no ask {id}")));
        };
        if a.state == "stopped" || a.ended_ms.is_some() {
            return Ok(json!({"id": id, "stopped": true}));
        }
        let n = rows::add_in(tx, id, who.or(a.asked.as_deref()), kind, text, now)?;
        if let Some(n) = n {
            owe_in(st, now, &a, n, kind, text);
        }
        Ok(json!({"id": id, "row": n}))
    })
}

/// `ask.read` (R5, behind `--wait`): the rows nobody has read, marked read, and the wakes owed for them withdrawn, in one
/// transaction: a row reaches the asker by `--wait` or by its wake, never both. An ended ask gives nothing (`StopWins`).
/// Answers `{id, state, rows}`.
pub(crate) fn read(core: &mut crate::db::Core, id: &str, now: i64) -> Result<Value> {
    wake::write_tx(core, secs(now), |tx, st| {
        let Some(a) = get(tx, id)? else {
            return Err(Error::Refused(format!("kith-core: asks: no ask {id}")));
        };
        if a.ended_ms.is_some() {
            return Ok(json!({"id": id, "state": a.state, "rows": [], "ended": true}));
        }
        let mut out = Vec::new();
        for row in rows::unread_in(tx, id)? {
            rows::mark_read_in(tx, id, row.n)?;
            if let Some(session) = a.reply_session.as_deref().filter(|s| !s.is_empty()) {
                wake::withdraw_in(tx, st, secs(now), session, Some(&rows::wake_id(id, row.n)))?;
            }
            out.push(row.json());
        }
        Ok(json!({"id": id, "state": a.state, "rows": out}))
    })
}

/// `ask.park` (R7): the ask waits on card `card`; `ask.unpark` (the card's answer) clears it. Answers whether a row changed.
pub(crate) fn park(
    core: &mut crate::db::Core,
    id: &str,
    card: Option<&str>,
    now: i64,
) -> Result<Value> {
    let tx = core.claim_tx()?;
    if get(&tx, id)?.is_none() {
        return Err(Error::Refused(format!("kith-core: asks: no ask {id}")));
    }
    // Only an open ask that Stop did not end waits on a card (as the permit's park does today).
    let n = tx.execute(
        "UPDATE asks SET parked_card = ?2, parked_ms = ?3 WHERE id = ?1 AND state = 'open' AND ended_ms IS NULL",
        rusqlite::params![id, card, now],
    )?;
    tx.commit()?;
    Ok(json!({"id": id, "parked": n == 1}))
}

/// `ask.unpark` (R7, amended): the card was answered. The ask no longer waits on it, and the callee's resume, with `note` as
/// its prompt, becomes the ask's new turn (source `ask`, an out file of its own) in the same transaction, so every later
/// settle goes by that turn. An ask Stop ended gets no turn. Answers `{id, parked: false, turn, out}` (`turn` null when none).
pub(crate) fn unpark(
    core: &mut crate::db::Core,
    machine: &Path,
    id: &str,
    note: Option<&str>,
    session: Option<&str>,
    now: i64,
) -> Result<Value> {
    let tx = core.claim_tx()?;
    let Some(mut a) = get(&tx, id)? else {
        return Err(Error::Refused(format!("kith-core: asks: no ask {id}")));
    };
    let live = a.state == "open" && a.ended_ms.is_none();
    // The card's answer is the resume's prompt: without it the parked turn would stay the ask's current one (and be settled
    // by the next tick from its old out file).
    if live && note.is_none_or(str::is_empty) {
        return Err(Error::Refused(
            "kith-core: asks: an unpark of an open ask needs the card's answer (note)".into(),
        ));
    }
    a.parked_card = None;
    a.parked_ms = None;
    if let Some(s) = session.filter(|s| !s.is_empty()) {
        a.session = Some(s.to_owned());
    }
    let mut made = None;
    if let Some(note) = note.filter(|n| !n.is_empty())
        && live
    {
        let (turn, out, conversation) = new_turn_in(&tx, machine, &a, note, None, true, now)?;
        a.conversation = Some(conversation);
        a.turn = Some(turn);
        a.out_path = Some(out.clone());
        made = Some((turn, out));
    }
    a.put(&tx)?;
    tx.commit()?;
    Ok(
        json!({"id": id, "parked": false, "turn": made.as_ref().map(|m| m.0), "out": made.map(|m| m.1)}),
    )
}
