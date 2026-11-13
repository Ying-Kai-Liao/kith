//! `resolve`: name the session `kith <agent> --resume THREAD` continues and take its writer claim, in the order of
//! `resolve()` in `kernel/bin/kith-sessions` (the order the thread-writers model checks: claim first, then ask the host
//! whether it is busy, so a send accepted after the claim cannot start and one accepted before it is counted by the host).
//!
//! The host is asked through [`Host`]: the CLI supplies a loopback client, the host itself and the tests supply their own.
//! The core never calls the host on its own (docs/plans/core-design.md section 2, rule 3), and no claim lock is held
//! across the call.

use std::time::Duration;

use serde_json::Value;

use super::registry::{entry, live_pid, transcript_cwd};
use super::{Ctx, Take, claim_path, claim_waiting, read_claim, release_claims};

/// What the core needs from a running host.
pub trait Host {
    /// `POST /api/resolveThread {"thread": ...}`: the host's answer as JSON, `{"error": "unknown method",
    /// "unknownMethod": true}` for a host too old to have the method (HTTP 404), or `None` when it can't be reached.
    fn resolve_thread(&self, thread: &str) -> Option<Value>;
}

/// A host that is not running: every question gets no answer.
pub struct NoHost;

impl Host for NoHost {
    fn resolve_thread(&self, _thread: &str) -> Option<Value> {
        None
    }
}

/// The session to continue, with its claim held for the claimer.
#[derive(Debug, PartialEq, Eq)]
pub struct Resolved {
    pub sid: String,
    pub cwd: String,
}

impl Resolved {
    /// The shell assignments `bin/kith` evals: `r_sid='..' r_cwd='..' r_from='..'`.
    pub fn shell(&self) -> String {
        let q = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
        format!(
            "r_sid={} r_cwd={} r_from={}",
            q(&self.sid),
            q(&self.cwd),
            q(&self.sid)
        )
    }
}

fn truthy_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// A non-empty JSON object, which is what Python's `if r` means for the host's answer.
fn present(r: Option<&Value>) -> Option<&Value> {
    r.filter(|v| v.as_object().is_some_and(|o| !o.is_empty()))
}

/// Python's truthiness of a JSON value.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn has_error(r: &Value) -> bool {
    r.get("error").is_some_and(truthy)
}

/// Names the session to resume and takes its writer claim for `claimer`. `wait` is how long a held session is waited
/// for (a `-p` one-shot waits a little; a terminal is refused at once). The error is the line `kith-sessions` dies with,
/// after `kith: `; the claim is given back first.
pub fn resolve(
    c: &Ctx,
    host: &dyn Host,
    agent: &str,
    thread: &str,
    claimer: i64,
    wait: Duration,
) -> Result<Resolved, String> {
    let Some(dir) = c.reg_dir() else {
        return Err("KITH_STATE is not set, so the session cannot be claimed".to_owned());
    };
    let mut holding = false;
    let die = |holding: bool, m: String| -> String {
        if holding {
            release_claims(c, claimer);
        }
        m
    };
    let kind = if wait.is_zero() {
        "terminal"
    } else {
        "oneshot"
    };
    // Takes the claim on sid; refuses while another writer holds it or the host is taking the session over.
    let claim = |sid: &str, holding: &mut bool| -> Result<(), String> {
        let how = Take::new(kind);
        let held = claim_waiting(c, sid, claimer, &how, wait).map_err(|e| e.to_string())?;
        if let Some(h) = held {
            return Err(format!(
                "that session is open in another writer (pid {}, {}); close it first",
                h.pid_text(),
                h.kind
            ));
        }
        *holding = true;
        // The host ending a terminal holds its own claim until the session is imported.
        if read_claim(&claim_path(&dir, sid, true))
            .as_ref()
            .is_some_and(super::Claim::is_live)
        {
            return Err(
                "that session is being taken over in the app right now; try again in a moment"
                    .to_owned(),
            );
        }
        Ok(())
    };

    let mut r = host.resolve_thread(thread);
    // A host from before the claims cannot vouch for its threads either.
    if let Some(v) = present(r.as_ref())
        && !has_error(v)
        && truthy_str(v, "sessionId").is_some()
        && v.get("claims") != Some(&Value::from(1))
    {
        r = Some(serde_json::json!({"error": "no claims", "unknownMethod": true}));
    }
    let (sid, mut cwd);
    if let Some(v) = present(r.as_ref())
        && !has_error(v)
        && let Some(named) = truthy_str(v, "sessionId")
    {
        if v.get("agent").and_then(Value::as_str) != Some(agent) {
            let theirs = v.get("agent").map_or_else(
                || "None".to_owned(),
                |a| a.as_str().map_or_else(|| a.to_string(), str::to_owned),
            );
            return Err(format!(
                "that thread belongs to {theirs}, not {agent}: kith {theirs} --resume {thread}"
            ));
        }
        sid = named.to_owned();
        cwd = truthy_str(v, "cwd").unwrap_or_default().to_owned();
        claim(&sid, &mut holding).map_err(|m| die(holding, m))?;
        // Asked again with the claim held: this answer is the one that counts (the first only named the session).
        let again = host.resolve_thread(&sid);
        let ok = present(again.as_ref())
            .is_some_and(|a| !has_error(a) && truthy_str(a, "sessionId") == Some(sid.as_str()));
        if !ok {
            return Err(die(
                holding,
                "the Kith host stopped answering; try again".to_owned(),
            ));
        }
        if present(again.as_ref()).is_some_and(|a| a.get("hostBusy").is_some_and(truthy)) {
            return Err(die(
                holding,
                "the app is answering in that thread right now; try again when it has finished"
                    .to_owned(),
            ));
        }
    } else if present(r.as_ref()).is_some_and(|v| v.get("unknownMethod").is_some_and(truthy)) {
        // A host older than continue-anywhere cannot say whether it is writing: only a session a terminal started (it is
        // in the registry) is safe; app threads are never there.
        sid = thread.to_owned();
        if entry(c, &sid).is_empty() {
            return Err("this Kith host is too old to continue app threads in the terminal (update kith serve to do that); \
                        only sessions started in a terminal can be resumed until then"
                .to_owned());
        }
        cwd = String::new();
        claim(&sid, &mut holding).map_err(|m| die(holding, m))?;
    } else {
        // Host not running, or not one of its threads: a raw Claude session id will do.
        sid = thread.to_owned();
        let Some(found) = transcript_cwd(c, thread) else {
            let why = if r.is_some() {
                ""
            } else {
                " (the Kith host is not running, so only a Claude session id can be resumed)"
            };
            return Err(format!("unknown thread \"{thread}\"{why}"));
        };
        cwd = found;
        claim(&sid, &mut holding).map_err(|m| die(holding, m))?;
    }
    let e = entry(c, &sid);
    if !e.is_empty() {
        if let Some(pid) = live_pid(c, &e) {
            return Err(die(
                holding,
                format!("that session is open in another terminal (pid {pid}); close it first"),
            ));
        }
        if let Some(theirs) = e
            .get("agent")
            .filter(|a| !a.is_null())
            .filter(|a| a.as_str() != Some(agent))
        {
            let theirs = theirs
                .as_str()
                .map_or_else(|| theirs.to_string(), str::to_owned);
            return Err(die(
                holding,
                format!("that session belongs to {theirs}, not {agent}"),
            ));
        }
    }
    if cwd.is_empty() {
        cwd = truthy_str(&Value::Object(e.clone()), "cwd")
            .unwrap_or_default()
            .to_owned();
    }
    if !cwd.is_empty() && !std::path::Path::new(&cwd).is_dir() {
        return Err(die(holding, format!("the session's folder {cwd} is gone")));
    }
    Ok(Resolved { sid, cwd })
}
