//! The queue rules, ported from `kernel/bin/kith-wake` (the model is `designs/2026-10-25-all-together/model/Lifecycle.tla`).
//! Each is a function over [`State`]; the caller wraps it in one transaction (`mod.rs`).
//!
//! - Rule 13: an id that is queued, answered or withdrawn is dropped, whoever sends it again.
//! - S5: requests for one session merge into one item and one turn.
//! - W2: a request leaves the queue only when its turn ends (`commit`), never when the turn starts (`start_turn`).
//! - W3: a turn starts only on a session nobody holds; what comes in meanwhile merges into the next turn.
//! - A turn whose holder died either reached the session (its marker is in the transcript: answered) or did not (owed
//!   again): `recover`.
//!
//! The run door's delivery (`deliver`, the drain's process starts) is not here: in the core a turn starts through the
//! conversations lane (M3-2), and a message for a session a terminal holds parks as `terminal_live` there (q217).

use std::fmt::Write as _;

use serde_json::{Value, json};

use super::model::{Item, Request, State, Turn, stamp};
use super::outside::{Outside, Transcripts};
use super::waiters::unwait;
use crate::error::{Error, Result};

/// A run-door turn that fails this often without reaching the session ends as failed.
pub(crate) const MAX_TRIES: i64 = 5;
/// Seconds before a failed try is offered again; doubles to [`MAX_RETRY`].
pub(crate) const FIRST_RETRY: f64 = 5.0;
pub(crate) const MAX_RETRY: f64 = 600.0;
/// Seconds a held session waits before it is offered again (not a failed try).
pub(crate) const HELD_PAUSE: f64 = 10.0;

pub(crate) fn refused(msg: impl Into<String>) -> Error {
    Error::Refused(msg.into())
}

/// What an enqueue did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Enqueued {
    Queued,
    Merged,
    Duplicate,
}

impl Enqueued {
    pub(crate) fn word(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Merged => "merged",
            Self::Duplicate => "duplicate",
        }
    }
}

/// One request to enqueue.
pub(crate) struct New<'a> {
    pub session: &'a str,
    pub id: &'a str,
    pub agent: &'a str,
    pub text: &'a str,
    pub person: Option<&'a str>,
    pub room: Option<&'a str>,
    pub door: &'a str,
    pub why: &'a str,
}

/// `WaiterResult` / `OtherEvent` in the model: one more wake owed to a session.
pub(crate) fn enqueue(st: &mut State, now: f64, n: &New<'_>) -> Result<Enqueued> {
    let person = n.person.filter(|p| !p.is_empty());
    let room = n.room.filter(|r| !r.is_empty());
    if n.door != "run" && n.door != "host" {
        return Err(Error::BadParams(format!(
            "wake: door is run or host, not {:?}",
            n.door
        )));
    }
    if n.door == "run" && person.is_some() {
        return Err(refused(
            "a person's session wakes through the host (--door host): only it can issue their run token",
        ));
    }
    if st.answered.contains_key(n.id) || st.find_request(n.id).is_some() {
        return Ok(Enqueued::Duplicate);
    }
    if let Some(it) = st.items.get(n.session) {
        if it.person.as_deref() != person {
            return Err(refused(format!(
                "{} is {}'s session, not {}'s",
                n.session,
                it.person.as_deref().unwrap_or("the owner"),
                person.unwrap_or("the owner")
            )));
        }
        if it.agent != n.agent {
            return Err(refused(format!(
                "{} belongs to {}, not {}",
                n.session, it.agent, n.agent
            )));
        }
    }
    let merged = st.items.contains_key(n.session);
    let it = st
        .items
        .entry(n.session.to_owned())
        .or_insert_with(|| Item {
            session: n.session.to_owned(),
            agent: n.agent.to_owned(),
            person: person.map(str::to_owned),
            room: room.map(str::to_owned),
            door: n.door.to_owned(),
            requests: Vec::new(),
            turn: None,
            tries: 0,
            next: 0.0,
            format: 1,
        });
    if it.door == "run" && n.door == "host" {
        it.door = "host".into();
    }
    it.requests.push(Request {
        id: n.id.to_owned(),
        why: n.why.to_owned(),
        text: n.text.to_owned(),
        at: stamp(now),
        run: None,
    });
    it.next = 0.0;
    Ok(if merged {
        Enqueued::Merged
    } else {
        Enqueued::Queued
    })
}

/// Stop: the requests a session is still owed are dropped and their ids kept, so a late copy does not come back; without
/// `id` its waiters go too. A request a running turn already carries stays with that turn (it was delivered). Returns how
/// many requests and waiters went.
pub(crate) fn withdraw(st: &mut State, now: f64, session: &str, id: Option<&str>) -> usize {
    let waits = if id.is_some() { 0 } else { unwait(st, session) };
    let Some(it) = st.items.get_mut(session) else {
        return waits;
    };
    let gone: Vec<String> = it
        .requests
        .iter()
        .filter(|r| r.run.is_none() && id.is_none_or(|i| i == r.id))
        .map(|r| r.id.clone())
        .collect();
    if gone.is_empty() {
        return waits;
    }
    it.requests.retain(|r| !gone.contains(&r.id));
    st.mark_answered(&gone, "withdrawn", None, now);
    st.settle(session);
    gone.len() + waits
}

/// Revoke or removal: everything the sessions of these people are owed is withdrawn as Stop does it, and the waiters of
/// those sessions go (a waiter carries no person: it is found by its session).
pub(crate) fn withdraw_person(st: &mut State, now: f64, persons: &[String]) -> usize {
    let sessions: Vec<String> = st
        .order()
        .into_iter()
        .filter(|s| {
            st.items[s]
                .person
                .as_ref()
                .is_some_and(|p| persons.contains(p))
        })
        .collect();
    sessions.iter().map(|s| withdraw(st, now, s, None)).sum()
}

/// `[kith wake RUN] why: text`, or a numbered list for several.
fn note_for(run: &str, requests: &[&Request]) -> String {
    let head = format!("[kith wake {run}]");
    let line = |r: &Request| {
        if r.why.is_empty() {
            r.text.clone()
        } else {
            format!("{}: {}", r.why, r.text)
        }
    };
    if let [r] = requests {
        return format!("{head} {}", line(r));
    }
    let mut out = format!(
        "{head} {} things happened while this session was idle:",
        requests.len()
    );
    for (n, r) in requests.iter().enumerate() {
        let _ = write!(out, "\n{}. {}", n + 1, line(r));
    }
    out
}

/// Whether the item's session is a Claude session id: every run-door item, and a host-door one the host resumes through
/// its run door (not a bot chat's `acp:<chat>:<thread>`, not a channel thread's `owner/repo#n` key).
fn claude_session(it: &Item) -> bool {
    it.door == "run" || !(it.session.starts_with("acp:") || it.session.contains('#'))
}

/// `TurnStart`: the owed requests of `session` go into one turn held by `holder`. The caller has checked the session is
/// free (W3). Nothing leaves the queue here (W2).
fn start_turn(
    st: &mut State,
    out: &dyn Outside,
    session: &str,
    holder: i64,
    sid: Option<&str>,
    now: f64,
) -> Value {
    let run = out.new_id();
    let it = st
        .items
        .get_mut(session)
        .expect("the caller found the item");
    let mut owed = Vec::new();
    for r in &mut it.requests {
        if r.run.is_none() {
            r.run = Some(run.clone());
            owed.push(r.clone());
        }
    }
    let sid = sid
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| claude_session(it).then(|| it.session.clone()));
    it.turn = Some(Turn {
        run: run.clone(),
        pid: Some(holder),
        proc_start: out.proc_start(holder),
        at: stamp(now),
        sid,
    });
    let refs: Vec<&Request> = owed.iter().collect();
    json!({
        "run": run,
        "session": it.session,
        "agent": it.agent,
        "person": it.person,
        "room": it.room,
        "door": it.door,
        "ids": owed.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
        "note": note_for(&run, &refs),
        "requests": owed.iter().map(|r| json!({"id": r.id, "why": r.why, "text": r.text})).collect::<Vec<_>>(),
    })
}

/// The session whose turn is `run`.
fn find_run(st: &State, run: &str) -> Option<String> {
    st.order()
        .into_iter()
        .find(|s| st.items[s].turn.as_ref().is_some_and(|t| t.run == run))
}

/// `TurnCommit`: what the turn carried is answered and leaves the queue; what came in meanwhile stays owed.
pub(crate) fn commit(st: &mut State, now: f64, run: &str, how: &str) -> bool {
    let Some(session) = find_run(st, run) else {
        return false;
    };
    answer_run(st, now, &session, run, how);
    true
}

fn answer_run(st: &mut State, now: f64, session: &str, run: &str, how: &str) {
    let it = st.items.get_mut(session).expect("the caller found it");
    let ids: Vec<String> = it
        .requests
        .iter()
        .filter(|r| r.run.as_deref() == Some(run))
        .map(|r| r.id.clone())
        .collect();
    it.requests.retain(|r| r.run.as_deref() != Some(run));
    it.turn = None;
    it.tries = 0;
    it.next = 0.0;
    st.mark_answered(&ids, how, Some(run), now);
    st.settle(session);
}

/// The turn did not reach the session: its requests are owed again. A held writer is not a failed try; a failed try waits
/// a doubling pause, and after [`MAX_TRIES`] the requests end as failed instead of looping.
pub(crate) fn give_back(st: &mut State, now: f64, run: &str, held: bool) -> bool {
    let Some(session) = find_run(st, run) else {
        return false;
    };
    release(st, now, &session, held);
    true
}

fn release(st: &mut State, now: f64, session: &str, held: bool) {
    let it = st.items.get_mut(session).expect("the caller found it");
    let Some(run) = it.turn.take().map(|t| t.run) else {
        return;
    };
    if held {
        // The one who takes it asks again soon, not in a tight loop.
        it.next = now + HELD_PAUSE;
    } else {
        it.tries += 1;
        if it.tries >= MAX_TRIES {
            let ids: Vec<String> = it
                .requests
                .iter()
                .filter(|r| r.run.as_deref() == Some(&run))
                .map(|r| r.id.clone())
                .collect();
            it.requests.retain(|r| r.run.as_deref() != Some(&run));
            it.tries = 0;
            it.next = 0.0;
            st.mark_answered(&ids, "failed", Some(&run), now);
            st.settle(session);
            return;
        }
        #[allow(clippy::cast_precision_loss)]
        let pause =
            (FIRST_RETRY * 2f64.powi(i32::try_from(it.tries - 1).unwrap_or(0))).min(MAX_RETRY);
        it.next = now + pause;
    }
    for r in &mut it.requests {
        if r.run.as_deref() == Some(&run) {
            r.run = None;
        }
    }
}

/// Restart / `HostBack`: a turn whose holder is gone either reached the session (its marker is in the transcript: answered,
/// so it is not delivered twice) or did not (owed again). A session whose writer is still live is left alone: the turn may
/// still be running in a claude the dead holder started.
pub(crate) fn recover(
    st: &mut State,
    out: &dyn Outside,
    tr: &dyn Transcripts,
    session: &str,
    now: f64,
) {
    let Some(t) = st.items.get(session).and_then(|it| it.turn.clone()) else {
        return;
    };
    if t.pid.is_some_and(|p| out.alive(p, t.proc_start.as_deref())) {
        return;
    }
    if let Some(sid) = &t.sid {
        if out.writer_live(sid) {
            return;
        }
        if tr.has(sid, &format!("[kith wake {}]", t.run)) {
            answer_run(st, now, session, &t.run, "recovered");
            return;
        }
    }
    release(st, now, session, false);
}

/// Recovers every item's dead turn.
pub(crate) fn recover_all(st: &mut State, out: &dyn Outside, tr: &dyn Transcripts, now: f64) {
    for s in st.order() {
        recover(st, out, tr, &s, now);
    }
}

/// The host starts a host-door turn for `holder` (its own pid): the first owed session, or `session`. W3: a session a
/// live writer holds is not offered; what it is owed waits.
pub(crate) fn take(
    st: &mut State,
    out: &dyn Outside,
    tr: &dyn Transcripts,
    now: f64,
    holder: i64,
    session: Option<&str>,
    sid: Option<&str>,
) -> Option<Value> {
    recover_all(st, out, tr, now);
    for s in st.order() {
        let it = &st.items[&s];
        if it.door != "host"
            || it.turn.is_some()
            || session.is_some_and(|w| !w.is_empty() && it.session != w)
            || it.next > now
            || it.requests.iter().all(|r| r.run.is_some())
        {
            continue;
        }
        let held = sid
            .filter(|x| !x.is_empty())
            .map(str::to_owned)
            .or_else(|| claude_session(it).then(|| it.session.clone()));
        if held.is_some_and(|h| out.writer_live(&h)) {
            continue;
        }
        return Some(start_turn(st, out, &s, holder, sid, now));
    }
    None
}

/// Where request `id` is: `queued`, `running`, how it was answered, or `unknown`; and the turn that carried it.
pub(crate) fn status(st: &State, id: &str) -> (String, Option<String>) {
    if let Some((_, r)) = st.find_request(id) {
        let how = if r.run.is_some() { "running" } else { "queued" };
        return (how.into(), r.run.clone());
    }
    st.answered.get(id).map_or_else(
        || ("unknown".into(), None),
        |a| (a.how.clone(), a.run.clone()),
    )
}

/// What `kith-wake ls` prints, a line each: the sessions owed, then the waiters.
pub(crate) fn ls_lines(st: &State) -> Vec<String> {
    let mut lines: Vec<String> = st
        .order()
        .iter()
        .map(|s| {
            let it = &st.items[s];
            let state = it
                .turn
                .as_ref()
                .map_or_else(|| "owed".to_owned(), |t| format!("turn {}", t.run));
            format!(
                "{}  {}  {}  {}  {state}  {} request(s)",
                it.session,
                it.agent,
                it.person.as_deref().unwrap_or("owner"),
                it.door,
                it.requests.len()
            )
        })
        .collect();
    lines.extend(st.waiters.values().map(|w| {
        format!(
            "{}  {}  waiting {} on {} until {}",
            w.session,
            w.agent,
            w.id,
            w.on,
            stamp(w.until)
        )
    }));
    lines
}
