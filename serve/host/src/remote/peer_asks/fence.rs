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
use tokio::sync::{Mutex, MutexGuard};

use super::{Said, core, folders_of};
use crate::api::UserError;
use crate::hub::Hub;

static PAIRING: Mutex<()> = Mutex::const_new(());

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
            Ok(())
        }
        Err(Said::Refused(why)) => Err(anyhow!(UserError(format!("This pairing can't start: {why}")))),
    }
}

/// `ask.endPeer`: the pairing in `dir` with `computer` ends in the core (its asks stopped, refused or cut off) before the host
/// removes its row. Safe to repeat.
pub(crate) async fn end(hub: &Hub, computer: &str, dir: &str) -> Result<()> {
    let Some(folders) = folders_of(hub) else { return Ok(()) };
    match core(&folders, "ask.endPeer", json!({"computer": computer, "dir": dir})).await {
        Ok(_) => Ok(()),
        Err(Said::Down(why)) => {
            tracing::warn!(computer, dir, why, "the core did not hear a pairing end; the reconcile ends it");
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
    End { computer: String, dir: &'static str },
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
                steps.push(Step::End { computer: held.0.clone(), dir });
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
    let Ok(listed) = core(&folders, "ask.peerList", json!({})).await else { return };
    let host_in: Vec<(Held, String)> =
        hub.store.peers_in().unwrap_or_default().into_iter().map(|p| ((p.computer_id, p.epoch), p.name)).collect();
    let host_out: Vec<(Held, String)> = hub
        .store
        .peers_out()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| p.epoch.map(|e| ((p.computer_id, e), p.name)))
        .collect();
    for step in plan(&listed, &host_in, &host_out) {
        match step {
            Step::End { computer, dir } => {
                tracing::info!(computer, dir, "the core holds a pairing this host no longer has; ending it");
                if let Err(e) = end(hub, &computer, dir).await {
                    tracing::warn!(error = format!("{e:#}"), "couldn't end a pairing the host dropped");
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
                    Err(e) => tracing::warn!(error = format!("{e:#}"), "couldn't remove an ended pairing"),
                }
            }
            Step::Tell { computer, dir, epoch, name } => {
                // A pairing from before the link carried asks, or one made while the core was down: the core learns of it.
                if let Err(e) = link(hub, &computer, dir, &epoch, &name).await {
                    tracing::warn!(error = format!("{e:#}"), computer, dir, "couldn't tell the core of a pairing");
                }
            }
        }
    }
}
