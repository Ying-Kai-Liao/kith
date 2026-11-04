//! core.db's `wake_sessions`, `wake_items`, `wake_answered` and `waiters` tables (migration 90) as a [`State`] and back.
//! A write reads the whole state (a few rows: the queue is small and answered ids last a week), changes it with the rules
//! of `queue.rs` / `waiters.rs`, and stores only what differs, in the caller's transaction.

use std::collections::BTreeSet;

use rusqlite::{Connection, params};

use super::model::{Answered, Item, Request, State, Turn, Waiter, Watch};
use crate::error::{Error, Result};

fn bad(what: &str, why: &str) -> Error {
    Error::Refused(format!("wake: {what}: {why}"))
}

/// Everything the wake tables hold.
pub(crate) fn load(c: &Connection) -> Result<State> {
    let mut st = State::default();
    let mut q = c.prepare(
        "SELECT session, agent, person, room, door, tries, next, turn_run, turn_pid, turn_proc_start, turn_at, turn_sid, format
         FROM wake_sessions",
    )?;
    let rows = q.query_map([], |r| {
        let turn = r
            .get::<_, Option<String>>(7)?
            .map(|run| -> rusqlite::Result<Turn> {
                Ok(Turn {
                    run,
                    pid: r.get(8)?,
                    proc_start: r.get(9)?,
                    at: r.get::<_, Option<String>>(10)?.unwrap_or_default(),
                    sid: r.get(11)?,
                })
            });
        Ok(Item {
            session: r.get(0)?,
            agent: r.get(1)?,
            person: r.get(2)?,
            room: r.get(3)?,
            door: r.get(4)?,
            requests: Vec::new(),
            turn: turn.transpose()?,
            tries: r.get(5)?,
            next: r.get(6)?,
            format: r.get(12)?,
        })
    })?;
    for it in rows {
        let it = it?;
        st.items.insert(it.session.clone(), it);
    }
    let mut q = c.prepare("SELECT session, id, why, text, at, run FROM wake_items ORDER BY seq")?;
    let rows = q.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            Request {
                id: r.get(1)?,
                why: r.get(2)?,
                text: r.get(3)?,
                at: r.get(4)?,
                run: r.get(5)?,
            },
        ))
    })?;
    for row in rows {
        let (session, req) = row?;
        st.items
            .get_mut(&session)
            .ok_or_else(|| bad(&req.id, "a request of a session that is not queued"))?
            .requests
            .push(req);
    }
    let mut q = c.prepare("SELECT id, how, t, run FROM wake_answered")?;
    let rows = q.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            Answered {
                how: r.get(1)?,
                t: r.get(2)?,
                run: r.get(3)?,
            },
        ))
    })?;
    for row in rows {
        let (id, a) = row?;
        st.answered.insert(id, a);
    }
    let mut q = c.prepare(
        "SELECT id, waits_on, session, agent, why, note, kind, repo, pr, pid, proc_start, at, created, checked, seen, until, format
         FROM waiters",
    )?;
    let rows = q.query_map([], |r| {
        let kind: String = r.get(6)?;
        Ok((
            kind,
            Waiter {
                id: r.get(0)?,
                on: r.get(1)?,
                session: r.get(2)?,
                agent: r.get(3)?,
                why: r.get(4)?,
                note: r.get(5)?,
                watch: Watch::At { at: 0.0 },
                created: r.get(12)?,
                checked: r.get(13)?,
                seen: r.get(14)?,
                until: r.get(15)?,
                format: r.get(16)?,
            },
            (
                r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<i64>>(8)?,
                r.get::<_, Option<i64>>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, Option<f64>>(11)?,
            ),
        ))
    })?;
    for row in rows {
        let (kind, mut w, (repo, pr, pid, proc_start, at)) = row?;
        w.watch = match (kind.as_str(), repo, pr, at) {
            ("ci", Some(repo), Some(pr), _) => Watch::Ci { repo, pr },
            ("pid", ..) => Watch::Pid { pid, proc_start },
            ("at", _, _, Some(at)) => Watch::At { at },
            _ => {
                return Err(bad(
                    &w.id,
                    &format!("a {kind} waiter without what it waits for"),
                ));
            }
        };
        st.waiters.insert(w.id.clone(), w);
    }
    Ok(st)
}

fn put_item(c: &Connection, it: &Item) -> Result<()> {
    let t = it.turn.as_ref();
    c.execute(
        "INSERT INTO wake_sessions (session, agent, person, room, door, tries, next, turn_run, turn_pid, turn_proc_start, turn_at, turn_sid, format)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            it.session,
            it.agent,
            it.person,
            it.room,
            it.door,
            it.tries,
            it.next,
            t.map(|t| &t.run),
            t.and_then(|t| t.pid),
            t.and_then(|t| t.proc_start.as_ref()),
            t.map(|t| &t.at),
            t.and_then(|t| t.sid.as_ref()),
            it.format,
        ],
    )?;
    for r in &it.requests {
        c.execute(
            "INSERT INTO wake_items (id, session, why, text, at, run, state) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                r.id,
                it.session,
                r.why,
                r.text,
                r.at,
                r.run,
                if r.run.is_some() { "taken" } else { "queued" }
            ],
        )?;
    }
    Ok(())
}

fn put_waiter(c: &Connection, w: &Waiter) -> Result<()> {
    let (repo, pr, pid, proc_start, at) = match &w.watch {
        Watch::Ci { repo, pr } => (Some(repo.as_str()), Some(*pr), None, None, None),
        Watch::Pid { pid, proc_start } => (None, None, *pid, proc_start.as_deref(), None),
        Watch::At { at } => (None, None, None, None, Some(*at)),
    };
    c.execute(
        "INSERT INTO waiters (id, session, agent, waits_on, kind, repo, pr, pid, proc_start, at, why, note, created, checked, seen, until, format)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        params![
            w.id,
            w.session,
            w.agent,
            w.on,
            w.watch.kind(),
            repo,
            pr,
            pid,
            proc_start,
            at,
            w.why,
            w.note,
            w.created,
            w.checked,
            w.seen,
            w.until,
            w.format,
        ],
    )?;
    Ok(())
}

/// Makes the tables hold `after` where `before` (what was read) differs, and nothing else.
pub(crate) fn save(c: &Connection, before: &State, after: &State) -> Result<()> {
    let sessions: BTreeSet<&String> = before.items.keys().chain(after.items.keys()).collect();
    let changed: Vec<&String> = sessions
        .into_iter()
        .filter(|s| before.items.get(*s) != after.items.get(*s))
        .collect();
    // Every delete before any insert: a request id may move to another session within one change.
    for s in &changed {
        c.execute("DELETE FROM wake_items WHERE session = ?1", [s])?;
        c.execute("DELETE FROM wake_sessions WHERE session = ?1", [s])?;
    }
    for s in &changed {
        if let Some(it) = after.items.get(*s) {
            put_item(c, it)?;
        }
    }
    let ids: BTreeSet<&String> = before
        .answered
        .keys()
        .chain(after.answered.keys())
        .collect();
    for id in ids {
        if before.answered.get(id) == after.answered.get(id) {
            continue;
        }
        c.execute("DELETE FROM wake_answered WHERE id = ?1", [id])?;
        if let Some(a) = after.answered.get(id) {
            c.execute(
                "INSERT INTO wake_answered (id, how, t, run) VALUES (?1, ?2, ?3, ?4)",
                params![id, a.how, a.t, a.run],
            )?;
        }
    }
    let ids: BTreeSet<&String> = before.waiters.keys().chain(after.waiters.keys()).collect();
    for id in ids {
        if before.waiters.get(id) == after.waiters.get(id) {
            continue;
        }
        c.execute("DELETE FROM waiters WHERE id = ?1", [id])?;
        if let Some(w) = after.waiters.get(id) {
            put_waiter(c, w)?;
        }
    }
    Ok(())
}

/// One line of what a state holds.
pub(crate) fn counts(st: &State) -> String {
    format!(
        "{} session(s) with {} request(s), {} answered id(s), {} waiter(s)",
        st.items.len(),
        st.items.values().map(|i| i.requests.len()).sum::<usize>(),
        st.answered.len(),
        st.waiters.len()
    )
}
