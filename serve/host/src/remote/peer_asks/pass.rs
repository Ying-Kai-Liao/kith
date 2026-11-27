//! A's pass (design 2026-11-24-ask-ledger §9, D3, D4): what the core says the link owes, carried to the other computer, and what
//! came back told to the core. One pass is sends, then stops, then `--to` messages, then follows, each repeatable.
//!
//! - **A computer that cannot be reached** is skipped for [`AWAY_FOR`] after the first failure, so a computer that is off costs
//!   one failed call, not one per ask per pass. Nothing is settled: the core cuts the ask off past its wait (seven days). The
//!   first failed try marks each ask it held up as waiting on the computer (`ask.peerApply {away}`, D4: no row, no wake), and the
//!   next call the computer answers clears it.
//! - **A computer that answers with a refusal of one call** (a bad parameter, "still settling") is not away: that item stays
//!   owed and goes again next pass, and the other items for the computer go on.
//! - **A computer that says it is not paired** (or this host has no pairing at the epoch of the row) cuts its asks off at once:
//!   `ask.endPeer {dir: out, epoch}` under the pairing lock (the epoch the item was read at, so a pairing made since is never
//!   ended by a stale answer), which gives each ask one notice row, and drops what was owed to it.
//! - **Two passes at once** do no harm (every call is repeatable) but waste calls, so a pass that finds one running does nothing.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use kith_kernel::paths::Paths;
use serde_json::{Value, json};

use super::{Reach, Said, Wire, core, fence, folders_of};
use crate::LockExt;
use crate::hub::Hub;
use crate::terminal::core_writer::Mode;

/// How often the pass runs.
const EVERY: Duration = Duration::from_secs(3);
/// How long a computer that did not answer is left alone.
const AWAY_FOR: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(crate) struct Pass {
    running: AtomicBool,
    away: Mutex<HashMap<String, Instant>>,
    /// The asks marked "waiting on computer" in this process, with their computer: a mark is told to the core once.
    marked: Mutex<HashMap<String, String>>,
}

impl Pass {
    fn skip(&self, computer: &str) -> bool {
        let mut away = self.away.locked();
        away.retain(|_, until| *until > Instant::now());
        away.contains_key(computer)
    }

    fn lost(&self, computer: &str) {
        self.away.locked().insert(computer.to_owned(), Instant::now() + AWAY_FOR);
    }

    fn found(&self, computer: &str) {
        self.away.locked().remove(computer);
        // The core clears the mark itself with the answer it takes (`ask.peerApply`); this only lets a later failure mark again.
        self.marked.locked().retain(|_, c| c != computer);
    }

    /// The ask of an item that could not be tried is shown as waiting on its computer (once per away spell).
    async fn mark_away(&self, folders: &Paths, item: &Value) {
        let (Some(id), Some(computer)) = (item["id"].as_str(), item["computer"].as_str()) else { return };
        if self.marked.locked().insert(id.to_owned(), computer.to_owned()).is_some() {
            return;
        }
        let params = json!({"id": id, "epoch": item["epoch"], "away": true});
        if let Err(Said::Down(why)) = core(folders, "ask.peerApply", params).await {
            // Not told: tried again with the next pass.
            self.marked.locked().remove(id);
            tracing::debug!(why, "peer asks: the core did not take the away mark");
        }
    }

    /// One pass over what the core says the link owes. A pass that is already running makes this one a no-op.
    pub(crate) async fn run(&self, folders: &Paths, wire: &dyn Wire) {
        if self.running.swap(true, Ordering::AcqRel) {
            return;
        }
        self.pass(folders, wire).await;
        self.running.store(false, Ordering::Release);
    }

    async fn pass(&self, folders: &Paths, wire: &dyn Wire) {
        let owed = match core(folders, "ask.peerOwed", json!({})).await {
            Ok(v) => v,
            Err(e) => return tracing::debug!(why = %e, "peer asks: the core's owed list can't be read"),
        };
        let list = |k: &str| owed[k].as_array().cloned().unwrap_or_default();
        for item in list("send") {
            self.send(folders, wire, &item).await;
        }
        for item in list("stop") {
            self.stop(folders, wire, &item).await;
        }
        for item in list("to") {
            self.to(folders, wire, &item).await;
        }
        for item in list("follow") {
            self.follow(folders, wire, &item).await;
        }
    }

    /// One call of the other computer for an owed item: `None` when it cannot be reached (the item stays owed), else its answer.
    /// A computer that is not paired gets its asks cut off.
    async fn reach(
        &self,
        folders: &Paths,
        wire: &dyn Wire,
        item: &Value,
        method: &'static str,
        params: Value,
    ) -> Option<Value> {
        let (computer, epoch) = (item["computer"].as_str()?, item["epoch"].as_str()?);
        if self.skip(computer) {
            self.mark_away(folders, item).await;
            return None;
        }
        match wire.call(computer, epoch, method, params).await {
            Ok(v) if v["paired"].as_bool() == Some(false) => {
                self.cut_off(folders, computer, epoch).await;
                None
            }
            Ok(v) => {
                self.found(computer);
                Some(v)
            }
            Err(Reach::NotPaired) => {
                self.cut_off(folders, computer, epoch).await;
                None
            }
            Err(Reach::Refused(why)) => {
                // The computer is there; this one call it would not take. It stays owed, and the next item goes.
                tracing::debug!(computer, why, "peer asks: the computer refused one call; it is tried again");
                self.found(computer);
                None
            }
            Err(Reach::Away(why)) => {
                tracing::debug!(computer, why, "peer asks: the computer did not answer");
                self.lost(computer);
                self.mark_away(folders, item).await;
                None
            }
        }
    }

    /// The computer is not paired at `epoch`, the epoch the item was read at: the asks of that epoch are cut off. Under the
    /// pairing lock, and naming the epoch, so an answer that is stale (the owner linked again since the item was read) ends nothing
    /// of the newer pairing.
    pub(super) async fn cut_off(&self, folders: &Paths, computer: &str, epoch: &str) {
        tracing::info!(computer, "peer asks: the computer is not paired at the epoch of its asks; cutting them off");
        let _one = fence::lock().await;
        let params = json!({"computer": computer, "dir": "out", "epoch": epoch});
        if let Err(e) = core(folders, "ask.endPeer", params).await {
            tracing::debug!(why = %e, "peer asks: the core did not take the cut-off");
        }
    }

    async fn send(&self, folders: &Paths, wire: &dyn Wire, item: &Value) {
        let params = json!({
            "key": item["key"], "epoch": item["epoch"], "agent": item["agent"], "task": item["task"],
            "lineage": item["lineage"], "opts": item["opts"], "cont": item["cont"],
        });
        if let Some(v) = self.reach(folders, wire, item, "peerAsk", params).await {
            apply(folders, item, &v, &[]).await;
        }
    }

    async fn follow(&self, folders: &Paths, wire: &dyn Wire, item: &Value) {
        let params = json!({"key": item["key"], "epoch": item["epoch"], "after": item["after"]});
        if let Some(v) = self.reach(folders, wire, item, "peerThread", params).await {
            apply(folders, item, &v, &[]).await;
        }
    }

    /// A stop owed: sent until the other computer says what became of the ask; the answer is the end the core waits for.
    async fn stop(&self, folders: &Paths, wire: &dyn Wire, item: &Value) {
        let params = json!({"key": item["key"], "epoch": item["epoch"]});
        let Some(v) = self.reach(folders, wire, item, "peerStop", params).await else { return };
        let state = v["state"].as_str().filter(|s| FINAL.contains(s)).unwrap_or("stopped");
        let told = json!({"paired": true, "found": true, "state": state, "rows": []});
        apply(folders, item, &told, &[]).await;
    }

    /// A `--to` message owed: it is not owed any more once the other computer took it.
    async fn to(&self, folders: &Paths, wire: &dyn Wire, item: &Value) {
        let params = json!({"key": item["key"], "epoch": item["epoch"], "text": item["text"]});
        let Some(v) = self.reach(folders, wire, item, "peerTo", params).await else { return };
        if v["taken"].as_bool() == Some(true) {
            let sent = [item["k"].as_str().unwrap_or_default().to_owned()];
            let told = json!({"paired": true, "found": true, "rows": []});
            apply(folders, item, &told, &sent).await;
        }
    }
}

/// The states of an ask that end it here.
const FINAL: [&str; 5] = ["answered", "capped", "stopped", "lost", "refused"];

/// `ask.peerApply`: what the other computer said of the ask, in one transaction. A state it did not say, or one this core does not
/// know, is `open` (the ask goes on); a final one carries the cost it reported.
async fn apply(folders: &Paths, item: &Value, said: &Value, to_sent: &[String]) {
    let state = said["state"].as_str().filter(|s| FINAL.contains(s) || *s == "open");
    let mut params = json!({
        "id": item["id"], "epoch": item["epoch"],
        "rows": said["rows"].as_array().cloned().unwrap_or_default(),
    });
    if let Some(s) = state {
        params["state"] = json!(s);
        if s != "open"
            && let Some(cost) = said["cost"].as_f64()
        {
            params["cost"] = json!(cost);
        }
    }
    if !to_sent.is_empty() {
        params["toSent"] = json!(to_sent);
    }
    match core(folders, "ask.peerApply", params).await {
        Ok(_) => {}
        Err(Said::Refused(why)) => tracing::warn!(why, "peer asks: the core refused what the computer said"),
        Err(Said::Down(why)) => tracing::debug!(why, "peer asks: the core did not take what the computer said"),
    }
}

/// The pass on a running host: after the reconcile (so the pairings agree first), every few seconds, in Writer mode only (the
/// rows are the core's, and only the host that writes them carries them).
pub(crate) fn start(hub: &std::sync::Arc<Hub>) {
    let weak = std::sync::Arc::downgrade(hub);
    tokio::spawn(async move {
        let pass = Pass::default();
        let mut clock = tokio::time::interval(EVERY);
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            clock.tick().await;
            let Some(hub) = weak.upgrade() else { break };
            if hub.routines.stopping() {
                break;
            }
            let Some(folders) = folders_of(&hub) else { continue };
            let probe = folders.clone();
            let writer =
                tokio::task::spawn_blocking(move || Mode::select(&probe) == Mode::Writer).await.unwrap_or(false);
            if !writer {
                continue;
            }
            fence::reconcile(&hub).await;
            pass.run(&folders, &super::HubWire(hub.clone())).await;
        }
    });
}
