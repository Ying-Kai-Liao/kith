//! The authority facts the event filter needs (M2-15): one seam between `roles::event_for` and whoever knows who is
//! granted which agent. Chat facts (bots, rooms, entries, cards) stay in the host store; only "may person P use agent
//! A right now" goes through [`Authority`].
//!
//! Two answers, one of them chosen per event: [`HostData`] (today's, the people mirror in the host store) and
//! [`CoreData`] (kith-core's authority, over the owner socket: `people.list`). Nothing is switched: the core answers
//! only when `KITH_CORE_AUTHORITY=1` is set. The core's answer fails CLOSED: a core that is unreachable, a refusal, or
//! an empty core database (people rows written before the import, M2-8) all say "not granted", so an invitee sees nothing
//! until the core knows them. The owner (`who` None) never asks, so owner events are unchanged.
//!
//! Nothing is cached: a revoke must stop events at once, so every question is asked afresh. The core's answer costs one
//! owner-socket round trip, asked once per event that reaches an invitee's own chat.

use crate::store::Store;
use kith_kernel::paths::Paths;
use serde_json::{Value, json};
use std::path::PathBuf;

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

/// The core's answer, from `people.list` for this person and agent.
pub struct CoreData {
    pub folders: Paths,
    pub home: PathBuf,
    pub kernel: PathBuf,
    /// The variables `paths` reads, as the caller has them (where the agent folders are).
    pub env: serde_json::Map<String, Value>,
}

impl CoreData {
    /// This process's view, as `kith people access` sends it.
    pub fn from_process() -> Self {
        let home = kith_kernel::paths::process_home();
        let folders = crate::kernel::folders(&home);
        let kernel = crate::kernel::current_kernel().unwrap_or_else(|| home.join("kith/kernel"));
        Self { folders, home, kernel, env: crate::core_front::caller_env() }
    }
}

impl Authority for CoreData {
    fn granted(&self, person: &str, agent: &str) -> bool {
        let (Some(home), Some(kernel)) = (self.home.to_str(), self.kernel.to_str()) else { return false };
        let params = json!({"person": person, "agent": agent, "home": home, "kernel": kernel, "env": self.env});
        crate::core_front::ask(&self.folders, "people.list", &params).is_some_and(|answer| {
            answer["rows"].as_array().is_some_and(|rows| {
                rows.iter()
                    .any(|r| r["agent"] == agent && r["person"] == person && r["via"] == "own" && r["allowed"] == true)
            })
        })
    }
}

/// Whether the opt-in is set.
pub fn core_selected() -> bool {
    std::env::var(SWITCH).is_ok_and(|v| v == "1")
}

/// Runs `f` with the answer in force: the core's when opted in, else the host's.
pub fn with_selected<R>(store: &Store, f: impl FnOnce(&dyn Authority) -> R) -> R {
    if core_selected() { f(&CoreData::from_process()) } else { f(&HostData(store)) }
}
