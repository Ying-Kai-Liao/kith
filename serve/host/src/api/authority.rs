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
//! Still on host data, not behind this seam (authority-derived, owners M2-16/M2-17): who a card names to answer it
//! (`answerable_by` in `route`, `mark_can_answer` / `access::can_answer`), the bot-guest-owner arm of `route`
//! (`store.bot_guest_of_person`, the owner's own person), and the bot guest's state in `event_for_bot`
//! (`BotState::Active`).

use super::roles;
use crate::store::Store;
use kith_kernel::paths::Paths;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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

    fn ask(&self, person: &str, agent: &str) -> bool {
        let (Some(home), Some(kernel)) = (self.home.to_str(), self.kernel.to_str()) else { return false };
        let params = json!({"person": person, "agent": agent, "home": home, "kernel": kernel, "env": self.env});
        crate::core_front::ask(&self.folders, "people.may", &params).is_some_and(|a| a["allowed"] == true)
    }
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
