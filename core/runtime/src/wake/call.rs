//! The `wake.*` rows the core answers (M3-7). The daemon routes them here (they need `&mut Core`: a write is a claim
//! transaction). The params were checked against the row already. Nothing calls them yet: `kernel/bin/kith-wake` keeps the
//! queue files until the switch (M3-10).
//!
//! Answers carry `lines`, what `kith-wake` prints for the same call.

use serde_json::{Value, json};

use super::model::Waiter;
use super::outside::{Files, Outside, System, Transcripts};
use super::queue::{self, New, refused};
use super::waiters::{self, Ask, WAIT_TIMEOUT};
use super::{read, write};
use crate::db::Core;
use crate::error::{Error, Result};

/// `wake.*` methods this module answers.
pub(crate) fn serves(method: &str) -> bool {
    matches!(
        method,
        "wake.enqueue"
            | "wake.withdraw"
            | "wake.withdrawPerson"
            | "wake.take"
            | "wake.done"
            | "wake.back"
            | "wake.queueList"
            | "wake.queueStatus"
            | "wake.wait"
    )
}

fn word<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str)
}

fn need<'a>(params: &'a Value, key: &str) -> Result<&'a str> {
    word(params, key)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::BadParams(format!("wake: {key} is required")))
}

fn strings(params: &Value, key: &str) -> Vec<String> {
    params
        .get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Answers a `wake.*` method over this core, as the caller's view sees the folders.
pub(crate) fn call(core: &mut Core, method: &str, params: &Value) -> Result<Value> {
    let (folders, _) = core.folders_for(params)?;
    let sys = System {
        state: folders.state.clone(),
        gh: None,
    };
    let files = Files::of(&folders.home, &folders.machine, None);
    call_with(core, method, params, &sys, &files)
}

/// [`call`] with the world given (a test answers for the clock, the processes, `gh` and the transcripts).
pub(crate) fn call_with(
    core: &mut Core,
    method: &str,
    params: &Value,
    out: &dyn Outside,
    tr: &dyn Transcripts,
) -> Result<Value> {
    let now = out.now();
    match method {
        "wake.enqueue" => {
            let text = strings(params, "text").join(" ");
            if text.is_empty() {
                return Err(Error::BadParams("wake: text is required".into()));
            }
            let n = New {
                session: need(params, "session")?,
                id: need(params, "id")?,
                agent: need(params, "agent")?,
                text: &text,
                person: word(params, "person"),
                room: word(params, "room"),
                door: word(params, "door").unwrap_or("run"),
                why: word(params, "why").unwrap_or_default(),
            };
            let said = write(core, now, |st| queue::enqueue(st, now, &n))?.word();
            Ok(json!({"result": said, "lines": [said]}))
        }
        "wake.withdraw" => {
            let session = need(params, "session")?;
            let id = word(params, "id").filter(|s| !s.is_empty());
            let n = write(core, now, |st| Ok(queue::withdraw(st, now, session, id)))?;
            Ok(json!({"count": n, "lines": [format!("withdrew {n}")]}))
        }
        "wake.withdrawPerson" => {
            let persons = strings(params, "persons");
            if persons.is_empty() {
                return Err(Error::BadParams("wake: persons is required".into()));
            }
            let n = write(core, now, |st| {
                Ok(queue::withdraw_person(st, now, &persons))
            })?;
            Ok(json!({"count": n, "lines": [format!("withdrew {n}")]}))
        }
        "wake.take" => {
            let holder = params
                .get("holder")
                .and_then(Value::as_i64)
                .ok_or_else(|| Error::BadParams("wake: holder is required".into()))?;
            let (session, sid) = (word(params, "session"), word(params, "sid"));
            let turn = write(core, now, |st| {
                Ok(queue::take(st, out, tr, now, holder, session, sid))
            })?;
            let lines: Vec<String> = turn.iter().map(Value::to_string).collect();
            Ok(json!({"turn": turn, "lines": lines}))
        }
        "wake.done" => {
            let run = need(params, "run")?;
            let how = word(params, "how")
                .filter(|s| !s.is_empty())
                .unwrap_or("answered");
            let done = write(core, now, |st| Ok(queue::commit(st, now, run, how)))?;
            Ok(json!({"done": done, "lines": []}))
        }
        "wake.back" => {
            let run = need(params, "run")?;
            let held = params.get("held").and_then(Value::as_bool).unwrap_or(false);
            let done = write(core, now, |st| Ok(queue::give_back(st, now, run, held)))?;
            Ok(json!({"done": done, "lines": []}))
        }
        "wake.queueList" => read(core, now, |st| {
            let items: Vec<Value> = st.order().iter().map(|s| st.items[s].to_json()).collect();
            let waiters: Vec<Value> = st.waiters.values().map(Waiter::to_json).collect();
            json!({"items": items, "waiters": waiters, "lines": queue::ls_lines(st)})
        }),
        "wake.queueStatus" => {
            let id = need(params, "id")?;
            read(core, now, |st| {
                let (state, run) = queue::status(st, id);
                let line = run
                    .as_deref()
                    .filter(|r| !r.is_empty())
                    .map_or_else(|| state.clone(), |r| format!("{state} {r}"));
                json!({"id": id, "state": state, "run": run.filter(|r| !r.is_empty()), "lines": [line]})
            })
        }
        "wake.wait" => {
            let note = strings(params, "note").join(" ");
            let ask = Ask {
                on: need(params, "on")?,
                session: word(params, "session"),
                agent: word(params, "agent"),
                why: word(params, "why").unwrap_or_default(),
                note: &note,
                timeout: word(params, "timeout").unwrap_or(WAIT_TIMEOUT),
                via: word(params, "via"),
                person: word(params, "person"),
            };
            let w = write(core, now, |st| waiters::wait(st, out, now, &ask))?;
            Ok(json!({"waiter": w.to_json(), "lines": [waiters::told(&w)]}))
        }
        _ => Err(refused(format!("{method} is not a wake method"))),
    }
}

/// The tick without its process starts: waiters whose thing happened become requests, then turns whose holder died are
/// recovered (`kith-wake drain` does these two, then starts the run-door sessions that are owed and free). The daemon does
/// not call this until the switch; a test does. Returns how many waiters fired.
pub(crate) fn tick(core: &mut Core, out: &dyn Outside, tr: &dyn Transcripts) -> Result<usize> {
    let now = out.now();
    write(core, now, |st| {
        let fired = waiters::poll(st, out, now);
        queue::recover_all(st, out, tr, now);
        Ok(fired)
    })
}
