//! The host's readers of the writer claims in Writer mode (designs/2026-11-14-m3-cutover section 1.6, package P8): who holds a
//! conversation, the owner's Stop of a holder, the revoke sweep of a person's runs. In `Files` and `Shadow` mode every one is
//! the claim-file code of `claim.rs`, untouched; in `Writer` mode the files are not read at all (nothing writes them any more,
//! and they are neither deleted nor rewritten here: the claims step of P9 makes them external or tombstones them).
//!
//! **One mode decision.** A [`Reader`] asks the same gate a turn's begin asks ([`Mode::select`], `cutover.mayServe`), once per
//! call site's pass (not once per session id): a reader that read files while begin claimed on the core would see no holder, and
//! a sweep would end nothing. There is no shortcut on the switch file: the core also records the switch, so the file can be
//! missing while the gate says `ok: true`. A state folder that is not this process's (no core registered for it, not the process
//! folders) never asks: it is `Files`.
//!
//! **Fail closed.** Writer mode with a core that does not answer, or a gate that is unclear while a switch file exists
//! (`Mode::Held`), reads as [`Reading::Unreadable`], never as [`Reading::Free`]: a second writer must not start on a guess.
//! Per caller: `sync.rs` `writer()` refuses a bot and parks the owner's send; `cards/resume.rs` keeps a wake owed; the owner's
//! `stopHolder` answers `unreadable`; a sweep the core did not take is [`owe`]d (a row in the host store the slow tick retries,
//! re-judged each time) and is never reported as "nothing to end".
//!
//! **What is ours.** A turn claim is this host's own only when its `holderInstance` is this start's [`core_writer::host_instance`]:
//! the owner's turn of this start is not a holder (it only keeps a terminal out, as `Kind::App` does today). A turn left by an
//! earlier host start, or named with the shadow's fixed `HOST_INSTANCE`, is a live writer (its stub outlives the host until
//! adopt's kill lands) and reads as a holder that Stop can stop.

use super::claim::{self, Claim, Kind, Stopped};
use super::core_claims::{claim_of, live_settled, row};
use super::core_writer::{self, Mode};
use crate::hub::Hub;
use kith_kernel::paths::Paths;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

/// The core's row of a claim a reader saw: what Stop and take-over act on, so they end exactly that claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub id: i64,
    pub state: String,
    pub instance: Option<String>,
}

/// What a reader found.
#[derive(Debug, PartialEq, Eq)]
pub enum Reading {
    /// No live writer.
    Free,
    /// A live writer. The row is the core's (`None` for the files path).
    By(Claim, Option<Row>),
    /// The core could not say (down, refused, an unclear gate): neither free nor held.
    Unreadable(String),
}

impl Reading {
    /// The claim of a holder, whatever the mode.
    pub fn holder(&self) -> Option<&Claim> {
        match self {
            Self::By(c, _) => Some(c),
            _ => None,
        }
    }
}

/// This start's holder instance, made once ([`core_writer::host_instance`] reads the process table).
fn own_instance() -> &'static str {
    static I: OnceLock<String> = OnceLock::new();
    I.get_or_init(core_writer::host_instance)
}

/// One mode decision for a pass over the readers.
pub struct Reader {
    state: PathBuf,
    mode: Mode,
    folders: Option<Paths>,
}

impl Reader {
    /// Reads the gate now (blocking: a core row). A state folder with no core of its own is `Files`.
    pub fn new(state: &Path) -> Self {
        let folders = folders_of(state);
        let mode = folders.as_ref().map_or(Mode::Files, Mode::select);
        Self { state: state.to_path_buf(), mode, folders }
    }

    #[cfg(test)]
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Whether the core, not the files, is (or may be) the writer: Writer, or a gate that cannot be read.
    pub fn core_decides(&self) -> bool {
        matches!(self.mode, Mode::Writer | Mode::Held)
    }

    /// [`Self::new`] then [`Self::read`] for a sync caller that may run on an async worker: a multi-thread runtime is told the
    /// thread blocks (the gate is a core row, not a file read), so the other tasks move on.
    pub fn read_now(state: &Path, sid: &str) -> Reading {
        let go = || Self::new(state).read(sid);
        match tokio::runtime::Handle::try_current() {
            Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(go)
            }
            _ => go(),
        }
    }

    /// Who holds `sid` now.
    pub fn read(&self, sid: &str) -> Reading {
        match (self.mode, &self.folders) {
            (Mode::Writer, Some(f)) => writer_read(f, sid),
            (Mode::Held, _) => Reading::Unreadable("the claims gate cannot be read".into()),
            _ => claim::holder(&self.state, sid).map_or(Reading::Free, |c| Reading::By(c, None)),
        }
    }
}

/// The core folders for `state`: a registered one (Shadow, and the tests' scripted core), else this process's own when `state`
/// is its state folder. Anything else has no core to ask.
fn folders_of(state: &Path) -> Option<Paths> {
    super::core_claims::folders_for(state).or_else(|| {
        let own = crate::core_front::process_folders();
        (own.state == state).then_some(own)
    })
}

fn writer_read(f: &Paths, sid: &str) -> Reading {
    let c = match live_settled(f, sid) {
        Ok(Some(c)) => c,
        Ok(None) => return Reading::Free,
        Err(error) => return Reading::Unreadable(error.to_string()),
    };
    let instance = c["holderInstance"].as_str().map(str::to_owned);
    let mut claim = claim_of(&c);
    let turn = c["kind"] == "turn";
    if turn && claim.kind == Kind::App {
        // The owner's turn: this start's own is no holder; an earlier start's still writes.
        if instance.as_deref() == Some(own_instance()) {
            return Reading::Free;
        }
        claim.kind = Kind::Other;
    }
    let row = Row {
        id: c["claimId"].as_i64().unwrap_or_default(),
        state: c["state"].as_str().unwrap_or_default().to_owned(),
        instance,
    };
    Reading::By(claim, Some(row))
}

/// The terminal's or external core claim a take-over stops, or why the core could not say. Writer mode only (the caller checked).
pub fn terminal_claim(f: &Paths, sid: &str) -> Result<Option<i64>, String> {
    let c = live_settled(f, sid).map_err(|e| e.to_string())?;
    Ok(c.filter(|c| matches!(c["kind"].as_str(), Some("terminal" | "external"))).and_then(|c| c["claimId"].as_i64()))
}

/// The Writer-mode core folders, when the gate says Writer; an unclear gate is an error (a take-over must not guess).
pub fn writer_folders(state: &Path) -> Result<Option<Paths>, String> {
    let r = Reader::new(state);
    match (r.mode, r.folders) {
        (Mode::Writer, Some(f)) => Ok(Some(f)),
        (Mode::Held, _) => Err("the claims gate cannot be read".into()),
        _ => Ok(None),
    }
}

// MARK: Stop

/// The owner's Stop of the holder of `sid` in Writer mode; `None` for any other mode (the caller runs the files path).
/// The claim read is the claim stopped: `hostclaim.stop {claimId}` is a compare-and-set in the core, so a claim released or
/// replaced between the read and the stop is left alone. `Ended` only once the core shows the claim `ended`.
pub fn stop(state: &Path, sid: &str, grace: Duration) -> Option<Stopped> {
    let reader = Reader::new(state);
    if !reader.core_decides() {
        return None;
    }
    Some(stop_with(&reader, sid, grace))
}

fn stop_with(reader: &Reader, sid: &str, grace: Duration) -> Stopped {
    let Some(f) = reader.folders.as_ref().filter(|_| reader.mode == Mode::Writer) else {
        return Stopped::Unreadable("the claims gate cannot be read".into());
    };
    let (claim, seen) = match reader.read(sid) {
        Reading::Free => return Stopped::Nobody,
        Reading::Unreadable(why) => return Stopped::Unreadable(why),
        Reading::By(c, Some(r)) => (c, r),
        Reading::By(_, None) => return Stopped::Unreadable("no core row for the claim".into()),
    };
    if claim::core_pid() == Some(claim.pid) {
        return Stopped::Busy(claim);
    }
    if claim::parent_of(claim.pid) == Some(std::process::id()) {
        return Stopped::OwnTurn(claim);
    }
    if cfg!(not(unix)) {
        return Stopped::CantStop(claim);
    }
    // A claim not redeemed yet has no process: the core ends it by compare-and-set and nothing is signalled.
    let unspawned = seen.state == "claimed" && matches!(claim_pid(f, seen.id), Ok(None));
    if !unspawned {
        match claim.identity() {
            Some(true) => {}
            // Positively another process (a reused pid): the core's settle ends it once the pid is confirmed gone.
            Some(false) => {
                let _ = row(f, "hostclaim.settle", &json!({}));
                return Stopped::Nobody;
            }
            None => return Stopped::CantStop(claim),
        }
        // Recorded before the signal (F12 rule 8): a session gone with no intent reads as a crash.
        super::intent::write(state_of(reader), sid, "stopped from the app", claim.pid);
    }
    let kill = match row(f, "hostclaim.stop", &json!({"claimId": seen.id, "reason": "stopped from the app"})) {
        Ok(v) => v["kill"].clone(),
        Err(error) => return Stopped::Unreadable(error.to_string()),
    };
    if let (Some(pid), true) = (kill["pid"].as_u64().and_then(|p| u32::try_from(p).ok()), kill.is_object()) {
        let started = kill["started"].as_str().map(str::to_owned);
        claim::end_claimed(&[(pid, started)], grace);
    }
    // Ended only when the core says so: settle ends the claim once its process (and group) are gone.
    let reaped = std::time::Instant::now() + claim::STOP_REAP;
    loop {
        let _ = row(f, "hostclaim.settle", &json!({}));
        match row(f, "hostclaim.show", &json!({"claimId": seen.id})) {
            Ok(v) if v["claim"]["state"].as_str() == Some("ended") => return Stopped::Ended(claim),
            // Claims are never deleted: no claim at the id I stopped is a wrong id, not an end.
            Ok(v) if v["claim"].is_null() => return Stopped::Unreadable("the core shows no claim of that id".into()),
            Ok(_) if std::time::Instant::now() >= reaped => return Stopped::StillEnding(claim),
            Ok(_) => std::thread::sleep(Duration::from_millis(50)),
            Err(error) => return Stopped::Unreadable(error.to_string()),
        }
    }
}

fn state_of(r: &Reader) -> &Path {
    &r.state
}

/// The pid the core shows for claim `id`; an error is not "unspawned" (the caller treats it as spawned).
fn claim_pid(f: &Paths, id: i64) -> std::io::Result<Option<u32>> {
    let v = row(f, "hostclaim.show", &json!({"claimId": id}))?;
    Ok(v["claim"]["pid"].as_u64().and_then(|p| u32::try_from(p).ok()))
}

// MARK: the revoke sweep

/// A live claim of the core as `claim.list` shows it.
#[derive(Debug, Clone)]
struct Listed {
    id: i64,
    person: String,
    agent: Option<String>,
}

/// What a sweep came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Swept {
    /// Writer mode and the core ended what it judged: the pids that were signalled.
    Done(Vec<u32>),
    /// The core did not take it: an owed sweep was recorded, the reason is the warning.
    Owed(String),
}

/// The owed-sweep rows in the host store: `[{"persons": [...], "narrow": bool, "suspended": bool}]`.
const OWED: &str = "claims.sweep.owed";

/// Ends the runs of `persons`' claims through the core (Writer mode): the whole person with `claim.sweepPerson` (a revoke or
/// removal: it bumps the person's epoch, cancels their queued turns, so never for a narrowing); a narrowing (`only_denied`)
/// asks `claim.list`, keeps the holders whose person may still use their agent, and stops each other by its claim id. A claim
/// whose agent the row does not carry cannot be judged and goes (the files path's rule).
pub async fn sweep(hub: &Hub, reader_state: &Path, persons: Vec<String>, only_denied: bool) -> Option<Swept> {
    let state = reader_state.to_path_buf();
    let reader = crate::wake::blocking(move || Reader::new(&state)).await?;
    if !reader.core_decides() {
        return None;
    }
    Some(sweep_in(hub, reader, persons, only_denied).await)
}

async fn sweep_in(hub: &Hub, reader: Reader, persons: Vec<String>, only_denied: bool) -> Swept {
    let Some(f) = reader.folders.clone().filter(|_| reader.mode == Mode::Writer) else {
        return owe(hub, &persons, only_denied, "the claims gate cannot be read".into());
    };
    let grace = Duration::from_secs(2);
    // What the core handed out is ended even when a later row failed: the failure is owed, the kills are not dropped.
    let (held, failed) = if only_denied {
        narrowed(&f, persons.clone()).await
    } else {
        let (f2, p2) = (f.clone(), persons.clone());
        match crate::wake::blocking(move || whole(&f2, &p2)).await {
            Some(Ok(held)) => (held, None),
            Some(Err(error)) => (Vec::new(), Some(error)),
            None => (Vec::new(), Some(std::io::Error::other("sweep failed"))),
        }
    };
    let ended = if held.is_empty() {
        Vec::new()
    } else {
        crate::wake::blocking(move || claim::end_claimed(&held, grace)).await.unwrap_or_default()
    };
    match failed {
        None => Swept::Done(ended),
        Some(error) => owe(hub, &persons, only_denied, error.to_string()),
    }
}

fn kills(v: &Value) -> Vec<(u32, Option<String>)> {
    v["kill"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|k| Some((u32::try_from(k["pid"].as_u64()?).ok()?, k["started"].as_str().map(str::to_owned))))
        .collect()
}

fn whole(f: &Paths, persons: &[String]) -> std::io::Result<Vec<(u32, Option<String>)>> {
    Ok(kills(&row(f, "claim.sweepPerson", &json!({"persons": persons}))?))
}

fn list(f: &Paths, persons: &[String]) -> std::io::Result<Vec<Listed>> {
    let v = row(f, "claim.list", &json!({}))?;
    Ok(v["live"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| matches!(c["state"].as_str(), Some("claimed" | "running" | "ending")))
        .filter_map(|c| {
            let person = c["person"].as_str().filter(|p| persons.iter().any(|x| x == p))?;
            Some(Listed {
                id: c["claimId"].as_i64()?,
                person: person.to_owned(),
                agent: c["agent"].as_str().filter(|a| !a.is_empty()).map(str::to_owned),
            })
        })
        .collect())
}

async fn narrowed(f: &Paths, persons: Vec<String>) -> (Vec<(u32, Option<String>)>, Option<std::io::Error>) {
    let f1 = f.clone();
    let listed = match crate::wake::blocking(move || list(&f1, &persons)).await {
        Some(Ok(l)) => l,
        Some(Err(error)) => return (Vec::new(), Some(error)),
        None => return (Vec::new(), Some(std::io::Error::other("list failed"))),
    };
    let mut allowed: std::collections::BTreeSet<(String, String)> = std::collections::BTreeSet::new();
    for l in &listed {
        if let Some(agent) = &l.agent
            && crate::api::roles::may_use(&l.person, agent).await
        {
            allowed.insert((l.person.clone(), agent.clone()));
        }
    }
    let go: Vec<i64> = listed
        .iter()
        .filter(|l| !l.agent.as_ref().is_some_and(|a| allowed.contains(&(l.person.clone(), a.clone()))))
        .map(|l| l.id)
        .collect();
    let f2 = f.clone();
    crate::wake::blocking(move || {
        let mut held = Vec::new();
        for id in go {
            // Only the claim judged: a claim that ended meanwhile gives no kill.
            match row(&f2, "hostclaim.stop", &json!({"claimId": id, "reason": "access narrowed"})) {
                Ok(v) => held.extend(kills(&json!({"kill": [v["kill"].clone()]}))),
                Err(error) => return (held, Some(error)),
            }
        }
        (held, None)
    })
    .await
    .unwrap_or_else(|| (Vec::new(), Some(std::io::Error::other("stop failed"))))
}

// MARK: owed sweeps

/// Every read and write of the owed rows goes through this lock (never held across an await), so a row owed while a retry is
/// under way, or by two threads at once, is not lost.
static OWED_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn locked() -> std::sync::MutexGuard<'static, ()> {
    OWED_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Records a sweep the core did not take. Retried by [`retry_owed`]; never reported as "nothing to end".
fn owe(hub: &Hub, persons: &[String], only_denied: bool, why: String) -> Swept {
    let suspended = persons.iter().any(|p| hub.store.is_suspended(p));
    let row = json!({"persons": persons, "narrow": only_denied, "suspended": suspended});
    {
        let _lock = locked();
        let mut rows = owed(hub);
        if !rows.contains(&row) {
            rows.push(row);
        }
        if let Err(error) = hub.store.kv_set(OWED, &Value::Array(rows).to_string()) {
            tracing::error!(%error, "a revoke sweep the core did not take could not be recorded");
        }
    }
    tracing::warn!(persons = ?persons, why, "warning: the core could not end a revoked person's runs; the sweep is owed and retried");
    Swept::Owed(why)
}

fn owed(hub: &Hub) -> Vec<Value> {
    hub.store.kv_get(OWED).and_then(|s| serde_json::from_str::<Vec<Value>>(&s).ok()).unwrap_or_default()
}

/// Removes one owed row (a fresh read under the lock), once its sweep was taken or judged unneeded.
fn settle_row(hub: &Hub, row: &Value) {
    let _lock = locked();
    let mut rows = owed(hub);
    if let Some(at) = rows.iter().position(|r| r == row) {
        rows.remove(at);
        let _ = if rows.is_empty() {
            hub.store.kv_delete(OWED)
        } else {
            hub.store.kv_set(OWED, &Value::Array(rows).to_string())
        };
    }
}

/// How many sweeps are owed.
#[cfg(test)]
pub fn owed_count(hub: &Hub) -> usize {
    let _lock = locked();
    owed(hub).len()
}

/// Tries every owed sweep again, each re-judged from now: a whole sweep of someone who was suspended and has been restored
/// (undo) is dropped, a narrowing asks `may_use` again. A row leaves the store only once its sweep was taken (or dropped):
/// a retry that fails leaves it as it was. Switched back to Files or Shadow: the rows are dropped with a warning, as the
/// files are the truth again (runs started under core claims have no claim file: a rollback must end them by hand).
pub async fn retry_owed(hub: &Hub) {
    let rows = {
        let _lock = locked();
        owed(hub)
    };
    if rows.is_empty() {
        return;
    }
    let state = hub.terminal.env().state;
    let st = state.clone();
    let Some(reader) = crate::wake::blocking(move || Reader::new(&st)).await else { return };
    match reader.mode {
        Mode::Held => return,
        Mode::Writer => {}
        Mode::Files | Mode::Shadow => {
            tracing::warn!(
                owed = rows.len(),
                "warning: owed revoke sweeps are dropped because the claims module is no longer in Writer mode; runs started under core claims have no claim file and must be ended by hand"
            );
            for r in &rows {
                settle_row(hub, r);
            }
            return;
        }
    }
    for r in rows {
        let persons: Vec<String> =
            r["persons"].as_array().into_iter().flatten().filter_map(|p| p.as_str().map(str::to_owned)).collect();
        let narrow = r["narrow"].as_bool().unwrap_or(false);
        if !narrow && r["suspended"].as_bool().unwrap_or(false) && !persons.iter().any(|p| hub.store.is_suspended(p)) {
            tracing::info!(?persons, "an owed revoke sweep was dropped: the person was restored");
            settle_row(hub, &r);
            continue;
        }
        let again = Reader { state: state.clone(), mode: reader.mode, folders: reader.folders.clone() };
        if let Swept::Done(ended) = sweep_in(hub, again, persons, narrow).await {
            settle_row(hub, &r);
            if !ended.is_empty() {
                tracing::info!(pids = ?ended, "ended runs left over after access was removed (owed sweep)");
            }
        }
    }
}

/// The slow tick of the owed sweeps.
pub async fn owed_loop(hub: std::sync::Arc<Hub>) {
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    loop {
        tick.tick().await;
        retry_owed(&hub).await;
    }
}

#[cfg(test)]
#[path = "core_holders_tests.rs"]
mod tests;
