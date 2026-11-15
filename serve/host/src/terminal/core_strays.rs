//! The turns this host enqueued that nothing runs (P9; designs/2026-11-14-m3-cutover, the host half of the restart requeue).
//!
//! A core restart, a host restart or a late `exit` can leave a host's turn `queued` in the core with no item in this host to run it:
//! `cancel_waiting_in` leaves a running turn alone, and the requeue that follows puts it back in the queue. Nothing in the host runs a
//! requeued core turn, so it would sit there for ever. The sweep cancels it.
//!
//! The rule is "this host has no item for it", keyed by `turns.enq_key` (the host always sends it, so its enqueue takes the basis path):
//! - only turns that carry a host key are listed (`hostclaim.strays`), never one the core enqueued itself (a schedule's fire, a wake);
//! - the core's list is read FIRST and this host's live keys SECOND, and a key is registered before its enqueue, so a turn that is
//!   listed has its key here if an item holds it;
//! - turn rows have no instance column, and there is one host per Mac (`host.lock`), so every listed turn whose key is not live here
//!   is a stray, an earlier host start's too.
//!
//! A key is live from the moment it is made until its turn is cancelled, or its end report went through (`forget_turn`). A key that
//! never reached an enqueue is dropped after [`UNBOUND_FOR`]: its item is waiting its turn in the actor's queue, and registers again
//! just before its enqueue.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::json;

use super::{Answer, CoreWriter};

/// How long a key with no turn is kept: the item has not begun, or its begin was dropped.
const UNBOUND_FOR: Duration = Duration::from_secs(3600);

struct Entry {
    turn: Option<i64>,
    at: Instant,
}

static LIVE: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();

fn live() -> std::sync::MutexGuard<'static, HashMap<String, Entry>> {
    LIVE.get_or_init(Mutex::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A key an item holds, before any enqueue of it. Repeating it changes nothing for a key that has its turn.
pub(super) fn register(key: &str) {
    let mut map = live();
    if let Some(e) = map.get_mut(key) {
        // Not yet enqueued: the time restarts, so an item that waited in the actor's queue for long is not dropped just as it
        // enqueues, and a sweep that lands between the core's insert and `bind` finds the key.
        if e.turn.is_none() {
            e.at = Instant::now();
        }
        return;
    }
    map.insert(key.to_owned(), Entry { turn: None, at: Instant::now() });
}

/// The core answered the enqueue of `key` with `turn`.
pub(super) fn bind(key: &str, turn: i64) {
    live().insert(key.to_owned(), Entry { turn: Some(turn), at: Instant::now() });
}

/// The turn is over for this host: cancelled, or its end was reported.
pub(super) fn forget_turn(turn: i64) {
    live().retain(|_, e| e.turn != Some(turn));
}

fn live_keys() -> HashSet<String> {
    let mut map = live();
    map.retain(|_, e| e.turn.is_some() || e.at.elapsed() < UNBOUND_FOR);
    map.keys().cloned().collect()
}

impl CoreWriter {
    /// Cancels the host turns of the core that no item of this host holds. Returns how many it cancelled; a core that does not answer
    /// cancels nothing (the next sweep looks again). Blocking.
    pub fn sweep_strays(&self) -> usize {
        let Answer::Done(v) = self.call("hostclaim.strays", &json!({})) else { return 0 };
        let listed: Vec<(i64, String)> = v["turns"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| Some((t["turn"].as_i64()?, t["key"].as_str()?.to_owned())))
            .collect();
        if listed.is_empty() {
            return 0;
        }
        // After the list, never before it: see the module doc.
        let keys = live_keys();
        listed
            .into_iter()
            .filter(|(_, key)| !keys.contains(key))
            .filter(|(turn, _)| self.cancel(*turn, "no item of this host is waiting for it"))
            .count()
    }
}

/// How often a running host looks for strays after its first look.
#[cfg(not(test))]
pub(crate) const EVERY: Duration = Duration::from_secs(60);

#[cfg(test)]
#[path = "core_strays_tests.rs"]
mod tests;
