//! The pairing's two ends agree (design 2026-11-24-ask-ledger D5, `PeerFence.tla`): the host's `peers_in` / `peers_out` rows and
//! the core's `ask_peers` fence.
//!
//! - **Every path that removes or replaces a pairing tells the core first** ([`end`]: `ask.endPeer`), so no ask of that epoch
//!   starts after the host row is gone (model variant v1: the host first leaves a window in which a queued ask starts).
//! - **Pairing writes the core first** ([`link`]: `ask.peerLink`, which also ends an older epoch of that direction), then the host.
//! - **One lock** ([`lock`]) holds pair, unpair and the reconcile apart, or the reconcile reads between the two writes of a
//!   pairing and undoes it (variant v4).
//! - **The reconcile** ([`reconcile`], level-triggered: host start and every pass) finishes what a crash between the two writes
//!   left: a live fence with no host row at its epoch is ended in the core; a host row whose epoch the core ended is removed; a
//!   host row the core never heard of (a pairing from before the link carried asks) is made known to it.
//!
//! A core that does not answer is no reason to hold a pairing the owner asked to end: the call is skipped (with a warning) and
//! the reconcile ends the epoch when the core is back, before the next ask can start from it (the core is down meanwhile, so
//! nothing starts).

use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tokio::sync::{Mutex, MutexGuard};

use super::{Said, core, folders_of};
use crate::api::UserError;
use crate::hub::Hub;

static PAIRING: Mutex<()> = Mutex::const_new(());

/// Whether the reconcile has run to its end since the host last failed to reach the core (false at start). The core starts
/// nothing of a pairing it was not told of again in its own run (`asks::peer_b::busy_origin_in`), and the callee answers nothing
/// until this is true ([`settled`]): so nothing for a pairing the host ended while the core was away starts in between.
static SETTLED: AtomicBool = AtomicBool::new(false);

/// How many times a call of the core was skipped because it did not answer; a reconcile that saw one go by is not settled.
static SKIPPED: AtomicU64 = AtomicU64::new(0);

fn owe() {
    SKIPPED.fetch_add(1, Ordering::AcqRel);
    SETTLED.store(false, Ordering::Release);
}

/// Whether the reconcile has agreed the pairings since the host last failed to tell the core of one ([`settled`] without the
/// reconcile). The run door reads it before it claims an ask that came from a computer: a `Busy` core that did not hear an unlink
/// is alive, and still holds the epoch, so nothing of that computer starts until a clean reconcile ended it (M1).
pub(crate) fn is_settled() -> bool {
    SETTLED.load(Ordering::Acquire)
}

/// The callee's gate: true when the pairings agree (the reconcile runs first when they have not been shown to).
///
/// Only a Writer host starts asks (the run door pulls them in Writer mode alone), and the pass's reconcile runs there every few
/// seconds, which is what confirms the pairings again after the core restarts (`ask.peerLink`: the core starts nothing of a
/// pairing it was not told of in its own run). A host that is not the Writer answers what it is asked (the rows are read and
/// written through the core all the same) but claims nothing, so nothing starts from a confirmation it never refreshed.
/// An ask it accepts waits queued for the Writer's run door, which confirms first (SHOULD 5: the same on any host that is not the
/// Writer, so there is no confirmation to run for one that never claims); if no Writer comes, the 24 h expiry ends it as lost.
pub(crate) async fn settled(hub: &Hub) -> bool {
    if SETTLED.load(Ordering::Acquire) {
        return true;
    }
    reconcile(hub).await;
    SETTLED.load(Ordering::Acquire)
}

/// The pairing lock: pair, unpair and the reconcile take it, one at a time.
pub(crate) async fn lock() -> MutexGuard<'static, ()> {
    PAIRING.lock().await
}

/// `ask.peerLink`: this pairing is the live one for `computer` in `dir` (`in`: asks it sends here; `out`: asks sent there), written
/// before the host writes its row. An older epoch of that direction ends in the same step.
pub(crate) async fn link(hub: &Hub, computer: &str, dir: &str, epoch: &str, name: &str) -> Result<()> {
    let Some(folders) = folders_of(hub) else { return Ok(()) };
    let params = json!({"computer": computer, "dir": dir, "epoch": epoch, "name": name});
    match core(&folders, "ask.peerLink", params).await {
        Ok(_) => Ok(()),
        Err(Said::Down(why)) => {
            tracing::warn!(computer, dir, why, "the core did not hear of a pairing; the reconcile tells it");
            owe();
            Ok(())
        }
        Err(Said::Refused(why)) => Err(anyhow!(UserError(format!("This pairing can't start: {why}")))),
    }
}

/// `ask.endPeer`: the pairing in `dir` with `computer` ends in the core (its asks stopped, refused or cut off) before the host
/// removes its row. Safe to repeat.
pub(crate) async fn end(hub: &Hub, computer: &str, dir: &str) -> Result<()> {
    end_at(hub, computer, dir, None).await
}

/// [`end`] for the epoch the caller read: a newer pairing of that direction is left alone (the reconcile reads the core's list,
/// and a pairing may have been made since).
pub(crate) async fn end_at(hub: &Hub, computer: &str, dir: &str, epoch: Option<&str>) -> Result<()> {
    let Some(folders) = folders_of(hub) else { return Ok(()) };
    let mut params = json!({"computer": computer, "dir": dir});
    if let Some(e) = epoch {
        params["epoch"] = json!(e);
    }
    match core(&folders, "ask.endPeer", params).await {
        Ok(_) => Ok(()),
        Err(Said::Down(why)) => {
            tracing::warn!(computer, dir, why, "the core did not hear a pairing end; the reconcile ends it");
            owe();
            Ok(())
        }
        Err(Said::Refused(why)) => Err(anyhow!("the core would not end the pairing with {computer}: {why}")),
    }
}

/// One pairing as one side holds it: the computer and the epoch.
pub(crate) type Held = (String, String);

/// What the reconcile does about one difference between the host's rows and the core's.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// The core holds a live pairing the host has no row for: `ask.endPeer`.
    End { computer: String, dir: &'static str, epoch: String },
    /// The core ended the epoch the host's row still names (a crash between the two writes): the host removes its row.
    Remove { computer: String, dir: &'static str },
    /// The host holds a pairing the core never heard of (from before the link carried asks): `ask.peerLink`.
    Tell { computer: String, dir: &'static str, epoch: String, name: String },
}

fn field(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_owned()
}

/// The reconcile's decision from what the core listed (`ask.peerList`) and what the host holds: `(held, name)` for `peers_in` and for
/// `peers_out` (only a pairing with an epoch, that is, one the other computer granted). Narrowing on the core's side, widening only
/// for a host row the core has never heard of; each step names the epoch it read, so it never ends a newer one. Pure.
pub(crate) fn plan(listed: &Value, host_in: &[(Held, String)], host_out: &[(Held, String)]) -> Vec<Step> {
    let rows = |k: &str| -> Vec<Value> { listed[k].as_array().cloned().unwrap_or_default() };
    let mut steps = Vec::new();
    for (dir, host) in [("in", host_in), ("out", host_out)] {
        let live: Vec<Held> = rows(dir).iter().map(|r| (field(r, "computer"), field(r, "epoch"))).collect();
        let ended: Vec<Held> = rows("ended")
            .iter()
            .filter(|r| r["dir"] == dir)
            .map(|r| (field(r, "computer"), field(r, "epoch")))
            .collect();
        for held in &live {
            if !host.iter().any(|(h, _)| h == held) {
                steps.push(Step::End { computer: held.0.clone(), dir, epoch: held.1.clone() });
            }
        }
        for (held, name) in host {
            if live.contains(held) {
                continue;
            }
            steps.push(if ended.contains(held) {
                Step::Remove { computer: held.0.clone(), dir }
            } else {
                Step::Tell { computer: held.0.clone(), dir, epoch: held.1.clone(), name: name.clone() }
            });
        }
    }
    steps
}

/// The reconcile (D5), level-triggered: reads both ends under the pairing lock and does what [`plan`] says.
pub(crate) async fn reconcile(hub: &Hub) {
    let Some(folders) = folders_of(hub) else { return };
    let _one = lock().await;
    let Ok(listed) = core(&folders, "ask.peerList", json!({})).await else {
        owe();
        return;
    };
    let host_in: Vec<(Held, String)> =
        hub.store.peers_in().unwrap_or_default().into_iter().map(|p| ((p.computer_id, p.epoch), p.name)).collect();
    let host_out: Vec<(Held, String)> = hub
        .store
        .peers_out()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| p.epoch.map(|e| ((p.computer_id, e), p.name)))
        .collect();
    let skipped = SKIPPED.load(Ordering::Acquire);
    let mut clean = true;
    // A pairing both ends hold is told to the core again: that is what lets the core start its asks after a restart.
    for (dir, held) in [("in", &host_in), ("out", &host_out)] {
        for ((computer, epoch), name) in held {
            let live = listed[dir].as_array().is_some_and(|rows| {
                rows.iter().any(|r| field(r, "computer") == *computer && field(r, "epoch") == *epoch)
            });
            if live && link(hub, computer, dir, epoch, name).await.is_err() {
                clean = false;
            }
        }
    }
    for step in plan(&listed, &host_in, &host_out) {
        match step {
            Step::End { computer, dir, epoch } => {
                tracing::info!(computer, dir, "the core holds a pairing this host no longer has; ending it");
                if let Err(e) = end_at(hub, &computer, dir, Some(&epoch)).await {
                    tracing::warn!(error = format!("{e:#}"), "couldn't end a pairing the host dropped");
                    clean = false;
                }
            }
            Step::Remove { computer, dir } => {
                tracing::info!(computer, dir, "the core ended a pairing this host still holds; removing it");
                let removed = if dir == "in" {
                    hub.store.remove_peer_in(&computer)
                } else {
                    hub.store.remove_peer_out(&computer)
                };
                match removed {
                    Ok(true) if dir == "in" => hub.auth_changed(),
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(error = format!("{e:#}"), "couldn't remove an ended pairing");
                        clean = false;
                    }
                }
            }
            Step::Tell { computer, dir, epoch, name } => {
                // A pairing from before the link carried asks, or one made while the core was down: the core learns of it.
                if let Err(e) = link(hub, &computer, dir, &epoch, &name).await {
                    tracing::warn!(error = format!("{e:#}"), computer, dir, "couldn't tell the core of a pairing");
                    clean = false;
                }
            }
        }
    }
    if clean && SKIPPED.load(Ordering::Acquire) == skipped {
        SETTLED.store(true, Ordering::Release);
    }
}
