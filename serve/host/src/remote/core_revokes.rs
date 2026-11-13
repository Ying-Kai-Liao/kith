//! A revoke made in the core alone, noticed by the host (a terminal `kith kernel core-call people.suspend`, which tells
//! the host nothing). The core does its half (claims, asks, door rows); the host's half is `people::cut_off`: set the
//! person's devices, memberships and invites aside, stop their turns, withdraw their cards, sweep asks and claims.
//!
//! Narrowing only, as `Reconcile.tla` checks (`verification/revoke-access/`). The poll (`cards::start`, every 3 s) calls
//! [`Due::tick`], which runs [`reconcile`] on a change of the core's revision, at host start, and at least every 60 s.
//! - The read of the core's people and the set-aside are ONE step under `AUTH_WRITES`, the lock the app's revoke and undo
//!   take for their core write. Read outside it, an app undo could land between the two and the person would stay set aside
//!   here while the core has them back (`broken-split-read.cfg`).
//! - It never gives anything back. A revoke whose core write failed is cut here fail-closed with the core unchanged, and a
//!   tick that restored what the core does not show revoked would undo that (`broken-restore-too.cfg`). An undo is the
//!   owner's, from the app.
//! - It acts only on a read that worked, and never on the owner.
//! - The stops (`cut_off`) come after the lock is let go, as in the app's revoke.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::api::authority::{owner_call, switched_now};
use crate::api::people::{AUTH_WRITES, cut_off};
use crate::hub::Hub;

/// How long a quiet host waits before it looks again (a revoke in the core changes its revision, which is seen sooner).
const EVERY: Duration = Duration::from_secs(60);

/// When the next look is due, and whom this host has already cut off for a revoke the core still shows (so a repeat
/// look stops no one twice).
#[derive(Default)]
pub(crate) struct Due {
    last: Option<Instant>,
    done: BTreeSet<String>,
}

impl Due {
    /// One tick of the poll. `changed`: the core's revision moved since the last tick. Does nothing unless switched.
    pub(crate) async fn tick(&mut self, hub: &Arc<Hub>, changed: bool) {
        if !switched_now() {
            self.last = None;
            self.done.clear();
            return;
        }
        if !(changed || self.last.is_none_or(|at| at.elapsed() >= EVERY)) {
            return;
        }
        self.last = Some(Instant::now());
        reconcile(hub, &mut self.done).await;
    }
}

/// Whom the core's rows say to cut: every non-owner person shown revoked, and every non-owner the host still holds live
/// that the core no longer lists (a removal). Empty when the rows are not a usable answer (no owner among them: a core
/// that is empty or not what it was must not cut anyone).
fn to_cut(rows: &Value, held: &[String], done: &BTreeSet<String>) -> (Vec<String>, BTreeSet<String>) {
    let Some(rows) = rows.as_array() else { return (Vec::new(), BTreeSet::new()) };
    if !rows.iter().any(|p| p["owner"] == true) {
        return (Vec::new(), BTreeSet::new());
    }
    let listed: BTreeSet<&str> = rows.iter().filter_map(|p| p["id"].as_str()).collect();
    let revoked: BTreeSet<String> = rows
        .iter()
        .filter(|p| p["owner"] != true && p["revoked"] == true)
        .filter_map(|p| p["id"].as_str().map(str::to_owned))
        .collect();
    let mut cut: Vec<String> = Vec::new();
    for id in &revoked {
        // Once is enough while the host holds nothing live of theirs; anything live is set aside again.
        if !done.contains(id) || held.contains(id) {
            cut.push(id.clone());
        }
    }
    let owners: BTreeSet<&str> = rows.iter().filter(|p| p["owner"] == true).filter_map(|p| p["id"].as_str()).collect();
    for id in held {
        if !listed.contains(id.as_str()) && !owners.contains(id.as_str()) && !cut.contains(id) {
            cut.push(id.clone());
        }
    }
    (cut, revoked)
}

/// Looks at the core's people once and cuts off here whoever it shows revoked or no longer lists. `done` is kept for the
/// ones cut while the core still shows them revoked.
pub(crate) async fn reconcile(hub: &Arc<Hub>, done: &mut BTreeSet<String>) {
    let cut = {
        let _w = AUTH_WRITES.lock().await;
        // A read that failed learns nothing: nothing is changed.
        let Ok(said) = owner_call("people.listPeople", json!({"viaSwitch": true}), true).await else { return };
        let held = hub.store.held_people().unwrap_or_default();
        let (cut, revoked) = to_cut(&said["rows"], &held, done);
        if said["rows"].is_array() {
            // someone the core has back may be cut again later
            done.retain(|p| revoked.contains(p));
        }
        for person in &cut {
            match hub.store.suspend_person(person) {
                Ok(m) => tracing::info!(person, ?m, "the core revoked a person: their devices are set aside here"),
                Err(e) => {
                    tracing::warn!(error = format!("{e:#}"), person, "couldn't set aside a person the core revoked");
                }
            }
        }
        cut
    };
    for person in cut {
        cut_off(hub, &person).await;
        done.insert(person);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Value {
        json!([
            {"id": "kai", "owner": true, "revoked": false},
            {"id": "sam", "owner": false, "revoked": true},
            {"id": "amy", "owner": false, "revoked": false},
        ])
    }

    #[test]
    fn only_a_revoked_non_owner_or_a_held_person_the_core_dropped_is_cut() {
        let held = vec!["amy".to_owned(), "gone".to_owned(), "kai".to_owned()];
        let (cut, revoked) = to_cut(&rows(), &held, &BTreeSet::new());
        assert_eq!(cut, ["sam", "gone"]);
        assert_eq!(revoked, BTreeSet::from(["sam".to_owned()]));
        // already cut, nothing live of theirs: not again
        let (cut, _) = to_cut(&rows(), &[], &BTreeSet::from(["sam".to_owned()]));
        assert_eq!(cut, Vec::<String>::new());
        // already cut but live rows came back (an app undo then a core-only revoke): cut
        let (cut, _) = to_cut(&rows(), &["sam".to_owned()], &BTreeSet::from(["sam".to_owned()]));
        assert_eq!(cut, ["sam"]);
    }

    #[test]
    fn the_owner_is_never_cut_and_an_answer_without_one_cuts_no_one() {
        let mut r = rows();
        r[0]["revoked"] = json!(true);
        let (cut, _) = to_cut(&r, &["kai".to_owned()], &BTreeSet::new());
        assert_eq!(cut, ["sam"], "the owner row is skipped even if it says revoked");
        for bad in [json!([]), json!([{"id": "sam", "owner": false, "revoked": true}]), json!(null), json!({})] {
            assert!(to_cut(&bad, &["sam".to_owned()], &BTreeSet::new()).0.is_empty(), "{bad}");
        }
    }
}
