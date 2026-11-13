//! Admission, capacity and the wait graph for bot requests.
//!
//! A requester is a place's actor key (`agent::bot::actor_key`): a bot's chat, threads and groups each
//! have their own turns, hops and outgoing requests. A recipient is a bot id, the actor of its own chat.
//! Message quotas count per bot.

use crate::LockExt;
use crate::agent::bot::bot_of;
use anyhow::{Result, bail};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use tokio::sync::watch;

use super::{BotRequest, MAX_PENDING_MESSAGES};

/// A bot's chain of requests is as deep as the kernel lets a chain of asks go on one Mac.
const MAX_REQUEST_HOPS: u8 = kith_kernel::delegation::MAX_LOCAL_HOPS;
pub(super) const MAX_PENDING_MESSAGES_PER_SENDER: usize = 16;

pub(super) struct Request {
    from: String,
    to: String,
    cancel: watch::Sender<bool>,
}

/// The wait graph includes queued requests, not only running recipients.
/// Reserving an edge and checking for cycles happen under the same lock.
#[derive(Default)]
pub struct Requests(pub(super) Mutex<RequestState>);

#[derive(Default)]
pub(super) struct RequestState {
    pub(super) pending: HashMap<String, Request>,
    pub(super) active: HashMap<String, u8>,
    pub(super) messages: usize,
    pub(super) messages_by_sender: HashMap<String, usize>,
}

impl Requests {
    pub(super) fn reserve_message(&self, from: &str) -> Result<u8> {
        let mut state = self.0.locked();
        let hops = state.next_hop(from)?;
        if state.messages >= MAX_PENDING_MESSAGES {
            bail!("too many pending bot messages");
        }
        let count = state.messages_by_sender.entry(bot_of(from).to_owned()).or_default();
        if *count >= MAX_PENDING_MESSAGES_PER_SENDER {
            bail!("too many pending bot messages from this bot (limit {MAX_PENDING_MESSAGES_PER_SENDER})");
        }
        *count += 1;
        state.messages += 1;
        Ok(hops)
    }

    pub(super) fn release_message(&self, from: &str) {
        let mut state = self.0.locked();
        state.messages -= 1;
        if let Some(count) = state.messages_by_sender.get_mut(from) {
            *count -= 1;
            if *count == 0 {
                state.messages_by_sender.remove(from);
            }
        }
    }

    pub(super) fn begin(&self, id: &str, from: &str, to: &str) -> Result<(watch::Receiver<bool>, u8)> {
        let mut state = self.0.locked();
        let hops = state.next_hop(from)?;
        let requests = &mut state.pending;
        if bot_of(from) == to {
            bail!("a bot cannot ask itself");
        }
        if requests.len() >= 64 {
            bail!("too many pending bot requests");
        }
        if requests.values().any(|r| r.from == from && r.to == to) {
            bail!("already waiting for this bot");
        }
        let mut pending = vec![to];
        let mut seen = HashSet::new();
        while let Some(bot) = pending.pop() {
            if bot == from {
                bail!("this request would make bots wait for each other; finish the current request first");
            }
            if seen.insert(bot) {
                pending.extend(requests.values().filter(|r| r.from == bot).map(|r| r.to.as_str()));
            }
        }
        let (cancel, rx) = watch::channel(false);
        requests.insert(id.to_owned(), Request { from: from.to_owned(), to: to.to_owned(), cancel });
        Ok((rx, hops))
    }

    /// Ends one place's turn: its requests are cancelled and it can't start more until its next turn.
    pub fn cancel_from(&self, actor: &str) {
        let mut state = self.0.locked();
        state.active.remove(actor);
        for request in state.pending.values().filter(|r| r.from == actor) {
            request.cancel.send_replace(true);
        }
    }

    pub fn start_turn(&self, actor: &str, request: Option<&BotRequest>) {
        self.0.locked().active.insert(actor.to_owned(), request.map_or(0, |r| r.hops));
    }

    /// A deleted bot: every place's requests, and every request to it.
    pub fn cancel_bot(&self, bot: &str) {
        let mut state = self.0.locked();
        state.active.retain(|actor, _| bot_of(actor) != bot);
        for request in state.pending.values().filter(|r| bot_of(&r.from) == bot || r.to == bot) {
            request.cancel.send_replace(true);
        }
    }
}

impl RequestState {
    fn next_hop(&self, from: &str) -> Result<u8> {
        let hops = *self.active.get(from).ok_or_else(|| anyhow::anyhow!("the requesting bot is no longer working"))?;
        if hops >= MAX_REQUEST_HOPS {
            bail!("bot request hop limit ({MAX_REQUEST_HOPS}) reached; ask the user before starting another chain");
        }
        Ok(hops + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhausted_chain_rejects_messages_and_asks_without_reserving_capacity() {
        let requests = Requests::default();
        requests.0.locked().active.insert("a".into(), MAX_REQUEST_HOPS);
        let message = requests.reserve_message("a");
        assert!(message.unwrap_err().to_string().contains("hop limit"));
        let ask = requests.begin("ask", "a", "b");
        assert!(ask.unwrap_err().to_string().contains("hop limit"));
        let state = requests.0.locked();
        assert_eq!(state.messages, 0);
        assert!(state.pending.is_empty());
        drop(state);
        requests.start_turn("a", None);
        assert_eq!(requests.reserve_message("a").unwrap(), 1);
    }

    #[test]
    fn each_place_of_a_bot_has_its_own_requests() {
        let requests = Requests::default();
        requests.start_turn("a", None);
        requests.start_turn("a|a/root", None);
        let (main, _) = requests.begin("main", "a", "b").unwrap();
        let (thread, _) = requests.begin("thread", "a|a/root", "b").unwrap();
        assert!(requests.begin("self", "a|a/root", "a").unwrap_err().to_string().contains("itself"));

        requests.cancel_from("a");
        assert!(*main.borrow());
        assert!(!*thread.borrow(), "the chat's turn ending leaves the thread's ask waiting");
        assert!(requests.reserve_message("a|a/root").is_ok(), "the thread is still working");
        assert!(requests.reserve_message("a").is_err());
        assert_eq!(requests.0.locked().messages_by_sender["a"], 1, "quotas count per bot");

        requests.cancel_bot("a");
        assert!(*thread.borrow());
        assert!(requests.0.locked().active.is_empty());
    }

    #[test]
    fn last_allowed_handoff_is_admitted() {
        let requests = Requests::default();
        requests.0.locked().active.insert("a".into(), MAX_REQUEST_HOPS - 1);
        assert_eq!(requests.reserve_message("a").unwrap(), MAX_REQUEST_HOPS);
        let (_, hops) = requests.begin("ask", "a", "b").unwrap();
        assert_eq!(hops, MAX_REQUEST_HOPS);
    }

    #[test]
    fn sender_quota_leaves_capacity_for_other_bots() {
        let requests = Requests::default();
        for bot in ["a", "b"] {
            requests.start_turn(bot, None);
        }
        for _ in 0..MAX_PENDING_MESSAGES_PER_SENDER {
            requests.reserve_message("a").unwrap();
        }
        let result = requests.reserve_message("a");
        assert!(result.unwrap_err().to_string().contains("from this bot (limit 16)"));
        assert_eq!(requests.0.locked().messages, MAX_PENDING_MESSAGES_PER_SENDER);
        requests.reserve_message("b").unwrap();
        // Ask capacity is independent of message quotas.
        requests.begin("ask", "a", "b").unwrap();
        requests.release_message("a");
        requests.reserve_message("a").unwrap();
        for _ in 0..MAX_PENDING_MESSAGES_PER_SENDER {
            requests.release_message("a");
        }
        requests.release_message("b");
        let state = requests.0.locked();
        assert_eq!(state.messages, 0);
        assert!(state.messages_by_sender.is_empty());
    }

    #[test]
    fn global_capacity_still_bounds_messages_from_multiple_senders() {
        let requests = Requests::default();
        for bot in ["a", "b", "c", "d"] {
            requests.start_turn(bot, None);
            for _ in 0..MAX_PENDING_MESSAGES_PER_SENDER {
                requests.reserve_message(bot).unwrap();
            }
        }
        assert_eq!(requests.0.locked().messages, MAX_PENDING_MESSAGES);
        requests.start_turn("e", None);
        let result = requests.reserve_message("e");
        assert_eq!(result.unwrap_err().to_string(), "too many pending bot messages");
        assert!(!requests.0.locked().messages_by_sender.contains_key("e"));
        requests.release_message("a");
        requests.reserve_message("e").unwrap();
        assert_eq!(requests.0.locked().messages, MAX_PENDING_MESSAGES);
    }

    #[test]
    fn concurrent_reservations_cannot_exceed_the_sender_quota() {
        let requests = Requests::default();
        requests.start_turn("a", None);
        let barrier = std::sync::Barrier::new(MAX_PENDING_MESSAGES);
        let admitted = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..MAX_PENDING_MESSAGES)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        requests.reserve_message("a").is_ok()
                    })
                })
                .collect();
            handles.into_iter().map(|handle| handle.join().unwrap()).filter(|admitted| *admitted).count()
        });
        assert_eq!(admitted, MAX_PENDING_MESSAGES_PER_SENDER);
        assert_eq!(requests.0.locked().messages, admitted);
        for _ in 0..admitted {
            requests.release_message("a");
        }
        assert!(requests.0.locked().messages_by_sender.is_empty());
    }
}
