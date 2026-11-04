//! The authority facts the event filter needs (M2-15): one seam between `roles::event_for` and whoever knows who is
//! granted which agent. Chat facts (bots, rooms, entries, cards) stay in the host store; only "may person P use agent
//! A right now" goes through [`Authority`].
//!
//! Two answers, one of them chosen per event: [`HostData`] (today's, the people mirror in the host store) and
//! [`CoreData`] (kith-core's authority, over the owner socket: `people.may`). Nothing is switched: the core answers
//! only when `KITH_CORE_AUTHORITY=1` is set. The core's answer fails CLOSED: a core that is unreachable, a refusal, or
//! an empty core database (people rows written before the import, M2-8) all say "not granted", so an invitee sees nothing
//! until the core knows them. The owner (`who` None) never asks, so owner events are unchanged.
//!
//! A live event asks afresh: a revoke must stop the next event. A catch-up batch asks once per (person, agent). The core's
//! answer costs one owner-socket round trip per live event that reaches an invitee's own chat; the stream runs it off
//! the async threads and drops the event (fails closed) if the core takes longer than a few seconds.
//!
//! M2-16 puts the request guard on the same switch. `roles::resolve` (so `may_use`, `may_use_checked`, `granted_agents`
//! and `access::member`) asks [`core_verdict`] instead of kith-auth's `resolve` when `KITH_CORE_AUTHORITY=1`. Moved: is
//! person P granted agent A (`guard`'s `own_bot`, the `botguests` ask/thread/room/new-bot gates and `enforce`, the sweep
//! of claims), is a bot guest's owner still allowed (the core's `may` for a bot also needs its owner's access), is P
//! revoked or suspended (the core says no), does P hold the member preset (who may answer a card derived from grants:
//! `access::derive`, and the live check of `respondPermission` in `guard`), and the preset shown in `granted_agents`.
//! The ask is bounded: at most [`MAX_ASKS`] blocking threads in flight (the thread keeps its permit until the ask
//! returns, so a core that hangs cannot grow the pool), a request waits no longer than [`ASK_WAIT`] for its turn and its
//! answer, and then fails closed. The owner never asks: the guard is for invited people only, and a core answer of
//! "owner" is a no here. With an empty core database (before the import, M2-8) every member is refused and `rederive` reads
//! every open card of theirs as "no longer allowed": import first, then set the switch.
//!
//! Still on host data, not behind this seam: who a card names to answer it (`answerable_by` in `route`, stored when the
//! card opens and kept fresh by `access::rederive`, which itself asks the moved fact), the bot-guest-owner arm of
//! `route` and `owned_bot_chat_agent` (`store.bot_guest_of_person`, `owner_person`: chat/bot data, not a grant), the bot
//! guest's state in `event_for_bot` and `bot_call` (`BotState`, set by `enforce`/`end_bot` from the moved fact), the
//! person's shared projects (`store.person(..).projects`: the host's, not an access grant), and the member / bot /
//! unproven method lists (the contract). `api/people.rs setPersonAccess` and `remote.rs decideAccessRequest` are M2-17.

use super::roles;
use crate::store::Store;
use kith_kernel::paths::Paths;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// The opt-in: set to `1` and the event filter asks the core's authority.
pub const SWITCH: &str = "KITH_CORE_AUTHORITY";

pub trait Authority {
    /// Whether `person` may use the hq agent `agent` now (a person who is unknown, revoked or suspended may not).
    fn granted(&self, person: &str, agent: &str) -> bool;
}

/// Today's answer: the host's people mirror.
pub struct HostData<'a>(pub &'a Store);

impl Authority for HostData<'_> {
    fn granted(&self, person: &str, agent: &str) -> bool {
        self.0.person(person).is_some_and(|p| p.agents.contains_key(agent))
    }
}

/// The core's answer, from `people.may` (the kernel's `may`: a revoke beats every row, then the person's own row, then
/// the `*` row). With a memo it asks once per (person, agent) and remembers, for one catch-up batch only.
pub struct CoreData {
    pub folders: Paths,
    pub home: PathBuf,
    pub kernel: PathBuf,
    /// The variables `paths` reads, as the caller has them (where the agent folders are).
    pub env: serde_json::Map<String, Value>,
    memo: Option<Mutex<HashMap<(String, String), bool>>>,
}

impl CoreData {
    pub fn new(folders: Paths, home: PathBuf, kernel: PathBuf, env: serde_json::Map<String, Value>) -> Self {
        Self { folders, home, kernel, env, memo: None }
    }

    /// This process's view, as `kith people may` sends it.
    pub fn from_process() -> Self {
        let home = kith_kernel::paths::process_home();
        let folders = crate::kernel::folders(&home);
        let kernel = crate::kernel::current_kernel().unwrap_or_else(|| home.join("kith/kernel"));
        Self::new(folders, home, kernel, crate::core_front::caller_env())
    }

    /// The same view with a memo: for the catch-up of one stream, where a hundred entries of one chat are one question.
    pub fn with_memo(&self) -> Self {
        Self {
            folders: self.folders.clone(),
            home: self.home.clone(),
            kernel: self.kernel.clone(),
            env: self.env.clone(),
            memo: Some(Mutex::new(HashMap::new())),
        }
    }

    /// The core's answer for one pair; None when it could not be asked (no daemon, a refusal, a lost connection).
    /// Blocking: the async callers go through [`core_verdict`].
    pub fn verdict(&self, person: &str, agent: &str) -> Option<Verdict> {
        // Nobody to ask about: the owner's own events and calls carry no person.
        if person.is_empty() {
            return None;
        }
        let (home, kernel) = (self.home.to_str()?, self.kernel.to_str()?);
        let params = json!({"person": person, "agent": agent, "home": home, "kernel": kernel, "env": self.env});
        let a = crate::core_front::ask(&self.folders, "people.may", &params)?;
        Some(Verdict {
            allowed: a["allowed"] == true,
            owner: a["owner"] == true || a["why"] == "owner",
            preset: a["preset"].as_str().map(str::to_owned),
        })
    }

    fn ask(&self, person: &str, agent: &str) -> bool {
        // The owner is never a guest of this seam: the core can not grant more than the mirror does.
        self.verdict(person, agent).is_some_and(|v| v.allowed && !v.owner)
    }
}

/// What the core says of one (person, agent): `people.may`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub allowed: bool,
    /// The person is the computer's owner (who never comes through the guard).
    pub owner: bool,
    /// The preset their membership carries, when they hold the agent.
    pub preset: Option<String>,
}

/// Blocking threads asking the core for the request guard at once. The thread owns its permit until the ask returns, so
/// a core that hangs holds at most this many threads however many requests time out.
pub const MAX_ASKS: usize = 4;

/// The longest a request waits for its turn and the core's answer before it is refused.
pub const ASK_WAIT: Duration = Duration::from_secs(3);

static ASKS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();

/// The permits of [`MAX_ASKS`] (a test takes them all to stand for a core that hangs).
pub(super) fn asks() -> Arc<tokio::sync::Semaphore> {
    ASKS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(MAX_ASKS))).clone()
}

/// The process's view for the guard, built once (`from_process` probes the filesystem).
fn process_core() -> Arc<CoreData> {
    static CORE: OnceLock<Arc<CoreData>> = OnceLock::new();
    #[cfg(test)]
    if let Some(c) = TEST_CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone() {
        return c;
    }
    CORE.get_or_init(|| Arc::new(CoreData::from_process())).clone()
}

/// A test's core (a daemon on a folder of its own) in place of the process's, and the guard's switch on while it is
/// set. Tests that set it hold `account::ONE_AT_A_TIME`.
#[cfg(test)]
pub static TEST_CORE: Mutex<Option<Arc<CoreData>>> = Mutex::new(None);

/// What the core says of `person` and `agent` for the request guard (`KITH_CORE_AUTHORITY=1`). None: it could not be
/// asked in time (a caller reads that as "no", or "try again" where it can). Off the async threads, [`MAX_ASKS`] at a
/// time, [`ASK_WAIT`] at most.
pub async fn core_verdict(person: &str, agent: &str) -> Option<Verdict> {
    if person.is_empty() {
        return None;
    }
    let core = process_core();
    let (person, agent) = (person.to_owned(), agent.to_owned());
    let sem = asks();
    let run = async move {
        let permit = sem.acquire_owned().await.ok()?;
        tokio::task::spawn_blocking(move || {
            let _held = permit;
            core.verdict(&person, &agent)
        })
        .await
        .ok()
        .flatten()
    };
    tokio::time::timeout(ASK_WAIT, run).await.ok().flatten()
}

impl Authority for CoreData {
    fn granted(&self, person: &str, agent: &str) -> bool {
        let Some(memo) = &self.memo else { return self.ask(person, agent) };
        let key = (person.to_owned(), agent.to_owned());
        if let Some(known) = memo.lock().ok().and_then(|m| m.get(&key).copied()) {
            return known;
        }
        let answer = self.ask(person, agent);
        if let Ok(mut m) = memo.lock() {
            m.insert(key, answer);
        }
        answer
    }
}

/// The answer chosen for one stream (or one call): built once, from the opt-in.
pub enum Selected {
    Host,
    Core(Arc<CoreData>),
}

impl Selected {
    pub fn from_env() -> Self {
        if core_selected() { Self::Core(Arc::new(CoreData::from_process())) } else { Self::Host }
    }

    pub fn is_core(&self) -> bool {
        matches!(self, Self::Core(_))
    }

    /// For a catch-up batch: one core question per (person, agent) instead of one per event. A live event never uses
    /// this, so a revoke still stops the next one.
    pub fn for_catch_up(&self) -> Self {
        match self {
            Self::Host => Self::Host,
            Self::Core(c) => Self::Core(Arc::new(c.with_memo())),
        }
    }

    pub fn event(&self, store: &Store, who: Option<&str>, v: Value) -> Option<Value> {
        match self {
            Self::Host => roles::event_for_with(&HostData(store), store, who, v),
            Self::Core(c) => roles::event_for_with(c.as_ref(), store, who, v),
        }
    }
}

/// Whether the opt-in is set AND the core's gate (`cutover.mayServe` for `authority`, asked as the Host) says it may serve.
/// The variable alone is not enough (M2-18a): while the core has not imported auth.json, or auth.json changed in a way
/// the core has not, the host stays on kith-auth. Until the first answer (the daemon starts beside the host) it is off.
pub fn core_selected() -> bool {
    opted_in() && gate_open(&current_fingerprint())
}

fn opted_in() -> bool {
    std::env::var(SWITCH).is_ok_and(|v| v == "1")
}

/// The gate's last answer, and what auth.json looked like when it was given.
struct Gate {
    ok: bool,
    fingerprint: String,
}

static GATE: Mutex<Option<Gate>> = Mutex::new(None);

/// How often the gate is asked again. A change of auth.json closes it at once (the fingerprint no longer matches) until
/// the next answer, so this is how long a switched-on host can stay on kith-auth after a harmless write, not how long a
/// stale core can answer.
const GATE_EVERY: Duration = Duration::from_secs(2);

fn gate_open(now: &str) -> bool {
    GATE.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .is_some_and(|g| g.ok && g.fingerprint == now)
}

/// auth.json's inode, size and mtime (the framework's own fingerprint of it), or `none`.
fn current_fingerprint() -> String {
    let home = kith_kernel::paths::process_home();
    let kernel = crate::kernel::current_kernel().unwrap_or_else(|| home.join("kith/kernel"));
    let auth = kith_kernel::secrets::Ctx::from_process(&kernel).auth;
    match std::fs::metadata(&auth) {
        Ok(md) => {
            use std::os::unix::fs::MetadataExt as _;
            format!("{}:{}:{}:{}", md.ino(), md.len(), md.mtime(), md.mtime_nsec())
        }
        Err(_) => "none".into(),
    }
}

/// What the gate's answer means for the host: whether to use the core, and the line to log when it is no (or a warning
/// with a yes). `None` is a core that did not answer.
pub fn read_gate(answer: Option<&Value>) -> (bool, Option<String>) {
    let Some(a) = answer else {
        return (false, Some("the core did not answer the authority gate; staying on kith-auth".into()));
    };
    let warning = a["warning"].as_str().map(str::to_owned);
    if a["ok"] == true {
        return (true, warning);
    }
    let msg = match warning {
        // "authority: N difference(s) between the core and the old store (...)": the import is behind the file.
        Some(w) => match w.split_once(" difference(s)").and_then(|(head, _)| head.rsplit(' ').next()?.parse::<usize>().ok()) {
            Some(n) => format!(
                "core.db missed {n} changes, run `kith kernel core-migrate --authority --force`; staying on kith-auth ({w})"
            ),
            None => format!("{w}; staying on kith-auth"),
        },
        None => "authority is not switched to the core (run `kith kernel core-migrate --authority`, then `kith migrate step authority`); staying on kith-auth"
            .into(),
    };
    (false, Some(msg))
}

/// One ask of the gate: the core's `cutover.mayServe` for `authority` on the Host connection, with this process's view of
/// the folders (the core reads the caller's own auth.json, not its own).
fn ask_gate(core: &CoreData) -> (bool, Option<String>) {
    let (Some(home), Some(kernel)) = (core.home.to_str(), core.kernel.to_str()) else {
        return (false, Some("home or kernel folder is not UTF-8; staying on kith-auth".into()));
    };
    let params = json!({"module": "authority", "home": home, "kernel": kernel, "env": core.env});
    read_gate(crate::core_front::call_as_host(&core.folders, "cutover.mayServe", &params).as_ref())
}

/// `KITH_CORE_AUTHORITY=1` at host start: from now on the gate is asked every [`GATE_EVERY`] on a thread of its own and
/// the answer is kept with auth.json's fingerprint. Without the variable nothing runs and the host is on kith-auth.
pub fn start_gate() {
    if !opted_in() {
        return;
    }
    let spawned = std::thread::Builder::new().name("authority-gate".into()).spawn(|| {
        let core = CoreData::from_process();
        let mut last: Option<(bool, Option<String>)> = None;
        loop {
            // Fingerprint first: a write that lands during the ask leaves the answer older than the file, and it is dropped.
            let fingerprint = current_fingerprint();
            let (ok, line) = ask_gate(&core);
            *GATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Gate { ok, fingerprint });
            if last.as_ref() != Some(&(ok, line.clone())) {
                match (ok, &line) {
                    (true, None) => tracing::info!("KITH_CORE_AUTHORITY=1: the core serves authority"),
                    (true, Some(w)) => tracing::warn!("KITH_CORE_AUTHORITY=1: the core serves authority. {w}"),
                    (false, w) => tracing::warn!(
                        "KITH_CORE_AUTHORITY=1 is not honoured: {}",
                        w.as_deref().unwrap_or("the core does not serve authority")
                    ),
                }
                last = Some((ok, line));
            }
            std::thread::sleep(GATE_EVERY);
        }
    });
    if let Err(e) = spawned {
        tracing::warn!(
            "KITH_CORE_AUTHORITY=1 is not honoured: the gate thread would not start ({e}); staying on kith-auth"
        );
    }
}

/// [`core_selected`] for the request guard: also on while a test has given it a core (`TEST_CORE`), so a test does not
/// have to set a variable the other tests of the process would see.
pub fn guard_on_core() -> bool {
    #[cfg(test)]
    if TEST_CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_some() {
        return true;
    }
    core_selected()
}

#[cfg(test)]
mod gate_tests {
    use super::*;

    #[test]
    fn a_denied_gate_names_how_many_changes_the_core_missed() {
        let a = json!({"ok": false, "warning": "authority: 3 difference(s) between the core and the old store (x); not served from the core"});
        let (open, line) = read_gate(Some(&a));
        let line = line.unwrap();
        assert!(!open);
        assert!(
            line.contains("core.db missed 3 changes, run `kith kernel core-migrate --authority --force`"),
            "{line}"
        );
        let (open, line) = read_gate(Some(&json!({"ok": false, "warning": null})));
        assert!(!open && line.unwrap().contains("not switched"));
        assert_eq!(read_gate(Some(&json!({"ok": true, "warning": null}))), (true, None));
        let (open, line) = read_gate(Some(&json!({"ok": true, "warning": "WARNING x"})));
        assert!(open && line.as_deref() == Some("WARNING x"));
    }

    #[test]
    fn the_gate_is_closed_until_asked_and_closes_when_auth_json_changes() {
        // Not asked yet (or no thread running): closed.
        assert!(!gate_open("1:2:3:4"));
        *GATE.lock().unwrap() = Some(Gate { ok: true, fingerprint: "1:2:3:4".into() });
        assert!(gate_open("1:2:3:4"));
        assert!(!gate_open("1:2:9:9"), "a changed auth.json closes it at once");
        *GATE.lock().unwrap() = None;
    }
}
