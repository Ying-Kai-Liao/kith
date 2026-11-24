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
    let engine = l
        .engine
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
    ask.conversation = Some(conversation);
    ask.turn = Some(turn);
    ask.out_path = Some(
        machine
            .join("core")
            .join("asks")
            .join(format!("{}.out", ask.id))
            .to_string_lossy()
            .into_owned(),
    );
    Ok(ask)
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
    notify: bool,
    now: i64,
) -> Result<Settled> {
    let Some(a) = get(tx, id)? else {
        return Ok(Settled::Nothing);
    };
    if a.state != "open" || a.ended_ms.is_some() {
        return Ok(Settled::Nothing);
    }
    if a.parked_card.is_some() && matches!(ended, Ended::Lost) {
        return Ok(Settled::Parked);
    }
    let (state, cost) = match ended {
        Ended::Answered(c) if c.is_error => ("stopped", c.cost),
        Ended::Answered(c) => ("answered", c.cost),
        Ended::Lost => ("lost", 0.0),
    };
    settle_in(tx, machine, id, cost, Some(state), now)?;
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
        // A `fg` call reports its own end and prints the answer itself; a run the host started has no caller to print it.
        let notify = a.mode.as_deref() != Some("fg");
        let said = settle_ask_in(tx, st, machine, f.id, &ended, f.session, notify, now)?;
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
    let n = match card {
        Some(card) => tx.execute(
            "UPDATE asks SET parked_card = ?2, parked_ms = ?3 WHERE id = ?1",
            rusqlite::params![id, card, now],
        )?,
        None => tx.execute(
            "UPDATE asks SET parked_card = NULL, parked_ms = NULL WHERE id = ?1",
            [id],
        )?,
    };
    tx.commit()?;
    if n == 0 {
        return Err(Error::Refused(format!("kith-core: asks: no ask {id}")));
    }
    Ok(json!({"id": id, "parked": card.is_some()}))
}
