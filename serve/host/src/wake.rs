//! The host's side of the kernel's wake queue (`kernel/bin/kith-wake`): the one owner of wakes (lifecycle F12 rule 1).
//!
//! The host keeps no queue of its own. A card answer, a reply to a `kith ask` and the like are `enqueue`d; the host takes
//! the host-door items (`take`), runs them through its bot door, channel door or person's run door, and says `done`
//! when the turn has started or `back` when it could not start. `withdraw` is Stop. Every call is one `kith-wake`
//! process: the queue's files and lock are the kernel's, so the kernel's tick and the host never disagree about them.
//!
//! The calls block (a process each); async callers go through [`blocking`].

use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A host-door turn `kith-wake take` handed over: what to start, as which person, and which requests it carries.
#[derive(Debug, Clone, Deserialize)]
pub struct Turn {
    pub run: String,
    pub session: String,
    pub agent: String,
    #[serde(default)]
    pub person: Option<String>,
    #[serde(default)]
    pub ids: Vec<String>,
    /// What was asked, one entry per request, so the host can leave out the ones that are no longer wanted.
    #[serde(default)]
    pub requests: Vec<Asked>,
    /// The words the agent is told, starting with the `[kith wake RUN]` marker kith-wake proves delivery by.
    pub note: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Asked {
    pub id: String,
    #[serde(default)]
    pub why: String,
    pub text: String,
}

/// A request for one turn of a session.
pub struct Request<'a> {
    pub session: &'a str,
    pub id: &'a str,
    pub agent: &'a str,
    pub person: Option<&'a str>,
    pub room: Option<&'a str>,
    pub host_door: bool,
    pub why: &'a str,
    pub text: &'a str,
}

/// How `enqueue` ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Queued {
    New,
    /// The id was queued, answered or withdrawn before: dropped.
    Duplicate,
}

#[cfg_attr(test, allow(clippy::unnecessary_wraps))]
fn script() -> Option<PathBuf> {
    #[cfg(test)]
    {
        Some(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../kernel/bin/kith-wake"))
    }
    #[cfg(not(test))]
    {
        let own = crate::kernel::current_kernel().map(|k| k.join("bin/kith-wake")).filter(|p| p.is_file());
        own.or_else(|| crate::agent::backends::which("kith-wake"))
    }
}

fn command(state: &Path) -> Option<Command> {
    let mut cmd = Command::new(script()?);
    cmd.env("KITH_STATE", state).stdin(Stdio::null());
    // A test host never starts the kernel's run door.
    #[cfg(test)]
    cmd.env("KITH_WAKE_NO_KICK", "1");
    Some(cmd)
}

fn run(state: &Path, args: &[&str]) -> anyhow::Result<std::process::Output> {
    let Some(mut cmd) = command(state) else {
        // Said once, loudly: every card answer waits on it, and the taker would repeat this every few seconds.
        static SAID: std::sync::Once = std::sync::Once::new();
        SAID.call_once(|| tracing::error!("kith-wake not found: card answers and wakes can't reach their sessions"));
        anyhow::bail!("no kith-wake found");
    };
    Ok(cmd.args(args).output()?)
}

fn said(out: &std::process::Output) -> String {
    let err = String::from_utf8_lossy(&out.stderr);
    let text = if err.trim().is_empty() { String::from_utf8_lossy(&out.stdout) } else { err };
    text.trim().to_owned()
}

/// One more wake owed to `r.session`; merges with what the session is already owed.
pub fn enqueue(state: &Path, r: &Request<'_>) -> anyhow::Result<Queued> {
    let mut args = vec!["enqueue", r.session, "--id", r.id, "--agent", r.agent, "--door"];
    args.push(if r.host_door { "host" } else { "run" });
    for (flag, v) in [("--person", r.person), ("--room", r.room), ("--why", Some(r.why).filter(|w| !w.is_empty()))] {
        if let Some(v) = v {
            args.extend([flag, v]);
        }
    }
    args.extend(["--", r.text]);
    let out = run(state, &args)?;
    anyhow::ensure!(out.status.success(), "kith-wake enqueue: {}", said(&out));
    Ok(if String::from_utf8_lossy(&out.stdout).trim() == "duplicate" { Queued::Duplicate } else { Queued::New })
}

/// Where request `id` is: queued, running, how it was answered (answered, recovered, failed, withdrawn) or unknown.
/// None: the call itself failed, which says nothing about the request.
pub fn status(state: &Path, id: &str) -> Option<String> {
    let out = run(state, &["status", id]).ok().filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&out.stdout);
    Some(text.split_whitespace().next()?.trim_end_matches(',').to_owned())
}

/// Where every request the queue knows is: queued, running, or how it was answered. Read from the queue's files, so
/// many cards cost no process; a request the queue has forgotten (its memory is a week) is missing.
pub fn statuses(state: &Path) -> std::collections::HashMap<String, String> {
    let dir = state.join("sessions/queue");
    let mut out = std::collections::HashMap::new();
    let read = |p: &Path| std::fs::read(p).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
    if let Some(answered) = read(&dir.join("answered.json")) {
        for (id, v) in answered.as_object().into_iter().flatten() {
            let how = v["how"].as_str().and_then(|h| h.split_whitespace().next()).unwrap_or_default();
            out.insert(id.clone(), how.trim_end_matches(',').to_owned());
        }
    }
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if !name.ends_with(".json") || name == "answered.json" || name.starts_with('.') {
            continue;
        }
        for r in read(&e.path()).iter().flat_map(|it| it["requests"].as_array().into_iter().flatten()) {
            if let Some(id) = r["id"].as_str() {
                out.insert(id.to_owned(), if r["run"].is_null() { "queued" } else { "running" }.to_owned());
            }
        }
    }
    out
}

/// Whether the queue holds anything at all: a look at its folder, so an idle host starts no process.
pub fn has_items(state: &Path) -> bool {
    std::fs::read_dir(state.join("sessions/queue")).is_ok_and(|dir| {
        dir.flatten().any(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.ends_with(".json") && name != "answered.json" && !name.starts_with('.')
        })
    })
}

/// Starts the next owed host-door turn, held by this process. None: nothing is owed (or nothing is due yet).
pub fn take(state: &Path) -> anyhow::Result<Option<Turn>> {
    let out = run(state, &["take", "--holder", &std::process::id().to_string()])?;
    match out.status.code() {
        Some(0) => Ok(Some(serde_json::from_slice(&out.stdout)?)),
        Some(1) => Ok(None),
        _ => anyhow::bail!("kith-wake take: {}", said(&out)),
    }
}

/// The turn `run` started: what it carried is answered. `how`: "withdrawn" for a turn dropped without delivering.
pub fn done(state: &Path, run_id: &str, how: &str) {
    log_failure("done", run(state, &["done", run_id, "--how", how]));
}

/// The turn `run` did not start: what it carried is owed again. `held`: a writer had the session (not a failed try).
pub fn back(state: &Path, run_id: &str, held: bool) {
    let mut args = vec!["back", run_id];
    if held {
        args.push("--held");
    }
    log_failure("back", run(state, &args));
}

/// Stop: `session`'s requests that no running turn carries are dropped, their ids kept. `id`: only that request.
pub fn withdraw(state: &Path, session: &str, id: Option<&str>) {
    let mut args = vec!["withdraw", session];
    if let Some(id) = id {
        args.extend(["--id", id]);
    }
    log_failure("withdraw", run(state, &args));
}

/// Revoke or removal: everything the sessions of `persons` are owed is withdrawn as Stop does it, and their waiters go.
pub fn withdraw_person(state: &Path, persons: &[String]) {
    if persons.is_empty() {
        return;
    }
    let mut args = vec!["withdraw-person"];
    args.extend(persons.iter().map(String::as_str));
    log_failure("withdraw", run(state, &args));
}

fn log_failure(what: &str, out: anyhow::Result<std::process::Output>) {
    match out {
        Ok(o) if o.status.success() => {}
        // `done` and `back` say 1 for a turn that is gone already (a restart's recovery took it): nothing to do.
        Ok(o) if o.status.code() == Some(1) && what != "withdraw" => {}
        Ok(o) => tracing::warn!(what, said = said(&o), "kith-wake failed"),
        Err(error) => tracing::warn!(what, error = format!("{error:#}"), "kith-wake failed"),
    }
}

/// Runs a blocking call off the async threads.
pub async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::task::spawn_blocking(f).await.ok()
}

/// Fire and forget, for callers that are not async or must not wait (a turn's start, a Stop).
pub fn spawn(f: impl FnOnce() + Send + 'static) {
    if let Ok(rt) = tokio::runtime::Handle::try_current() {
        drop(rt.spawn_blocking(f));
    } else {
        f();
    }
}

/// What the queue holds, as `kith-wake ls --json` says it.
#[cfg(test)]
pub fn ls(state: &Path) -> Vec<serde_json::Value> {
    let out = run(state, &["ls", "--json"]).expect("kith-wake ls");
    serde_json::from_slice(&out.stdout).unwrap_or_default()
}

/// Takes the item of one session, as a door that waits for that thread would.
#[cfg(test)]
pub fn take_session(state: &Path, session: &str) -> anyhow::Result<Option<Turn>> {
    let out = run(state, &["take", "--holder", &std::process::id().to_string(), "--session", session])?;
    Ok(if out.status.success() { Some(serde_json::from_slice(&out.stdout)?) } else { None })
}

/// The kernel's tick: turns whose holder died are recovered.
#[cfg(test)]
pub fn recover(state: &Path) {
    let _ = run(state, &["drain"]);
}
