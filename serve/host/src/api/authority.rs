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
//! "owner" is a no here. With an empty core.db (before the import, M2-8) every member is refused and `rederive` reads
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

/// Whether the opt-in is set.
pub fn core_selected() -> bool {
    std::env::var(SWITCH).is_ok_and(|v| v == "1")
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
