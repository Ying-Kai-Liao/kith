//! A revoke made in the core alone, noticed by the host (a terminal `kith kernel core-call people.suspend`, which tells
//! the host nothing). The core does its half (claims, asks, door rows); the host's half is `people::cut_off`: set the
//! person's devices, memberships and invites aside, stop their turns, withdraw their cards, sweep asks and claims.
//!
//! Narrowing only, as `Reconcile.tla` checks (`verification/revoke/`). The poll (`cards::start`, every 3 s) calls
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

/// A person and the moment of the revoke the core shows (`revoked_at`, as text), so a NEW revoke of the same person always
/// gets a new cut, even when no look saw them unrevoked in between.
type Cut = (String, String);

/// When the next look is due, and whom this host has already cut off for a revoke the core still shows (so a repeat
/// look stops no one twice).
#[derive(Default)]
pub(crate) struct Due {
    last: Option<Instant>,
    done: BTreeSet<Cut>,
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

/// Every id the host may hold the person of one `listPeople` row under: the current id and each former one (`was`). The host
/// never moves its rows when the core renames a person, so a cut, an undo and the "no card" fence all look under all of
/// them (a former id is never handed out again).
pub(crate) fn host_ids(row: &Value) -> Vec<String> {
    let own = row["id"].as_str().into_iter();
    let was = row["was"].as_array().into_iter().flatten().filter_map(Value::as_str);
    own.chain(was).map(str::to_owned).collect()
}

/// [`host_ids`] of `person` as the core lists them now; just `person` when the host is not switched or the core can't say.
pub(crate) async fn held_under(person: &str) -> Vec<String> {
    if switched_now()
        && let Ok(said) = owner_call("people.listPeople", json!({"viaSwitch": true, "withWas": true}), true).await
        && let Some(row) =
            said["rows"].as_array().into_iter().flatten().find(|r| host_ids(r).iter().any(|i| i == person))
    {
        return host_ids(row);
    }
    vec![person.to_owned()]
}

/// Whom the core's rows say to cut: every non-owner person shown revoked, and every non-owner the host still holds live
/// that the core no longer lists (a removal). Empty when the rows are not a usable answer (no owner among them: a core
/// that is empty or not what it was must not cut anyone).
fn to_cut(rows: &Value, held: &[String], done: &BTreeSet<Cut>) -> (Vec<Cut>, BTreeSet<Cut>) {
    let Some(rows) = rows.as_array() else { return (Vec::new(), BTreeSet::new()) };
    if !rows.iter().any(|p| p["owner"] == true) {
        return (Vec::new(), BTreeSet::new());
    }
    // A person renamed in the core is still known to the host under the old id (it never moves its rows): a former id
    // (`was`) is listed too, or the rename would read as a removal.
    let listed: BTreeSet<String> = rows.iter().flat_map(host_ids).collect();
    let revoked: BTreeSet<Cut> = rows
        .iter()
        .filter(|p| p["owner"] != true && p["revoked"] == true)
        .flat_map(|p| {
            // The host keeps a renamed person's rows under the old id, so a revoke is cut under the current id and every
            // former one (a former id is never handed out again).
            let at = p["revoked_at"].to_string();
            host_ids(p).into_iter().map(|id| (id, at.clone())).collect::<Vec<_>>()
        })
        .collect();
    let mut cut: Vec<Cut> = Vec::new();
    for key in &revoked {
        // Once is enough while the host holds nothing live of theirs; anything live is set aside again.
        if !done.contains(key) || held.contains(&key.0) {
            cut.push(key.clone());
        }
    }
    let owners: BTreeSet<&str> = rows.iter().filter(|p| p["owner"] == true).filter_map(|p| p["id"].as_str()).collect();
    for id in held {
        if !listed.contains(id) && !owners.contains(id.as_str()) && !cut.iter().any(|c| &c.0 == id) {
            cut.push((id.clone(), String::new()));
        }
    }
    (cut, revoked)
}

/// Looks at the core's people once and cuts off here whoever it shows revoked or no longer lists. `done` is kept for the
/// ones cut while the core still shows them revoked.
pub(crate) async fn reconcile(hub: &Arc<Hub>, done: &mut BTreeSet<Cut>) {
    let cut = {
        let _w = AUTH_WRITES.lock().await;
        // A read that failed learns nothing: nothing is changed.
        let Ok(said) = owner_call("people.listPeople", json!({"viaSwitch": true, "withWas": true}), true).await else {
            return;
        };
        // No answer about what the host holds is no answer: nothing is cut on a guess that nobody is held.
        let held = match hub.store.held_people() {
            Ok(held) => held,
            Err(e) => {
                tracing::warn!(
                    error = format!("{e:#}"),
                    "couldn't list who this host holds: the core's revokes wait for the next look"
                );
                return;
            }
        };
        let (cut, revoked) = to_cut(&said["rows"], &held, done);
        if said["rows"].is_array() {
            // someone the core has back may be cut again later
            done.retain(|p| revoked.contains(p));
        }
        for (person, _) in &cut {
            match hub.store.suspend_person(person) {
                Ok(m) => tracing::info!(person, ?m, "the core revoked a person: their devices are set aside here"),
                Err(e) => {
                    tracing::warn!(error = format!("{e:#}"), person, "couldn't set aside a person the core revoked");
                }
            }
        }
        cut
    };
    for (person, key) in cut {
        cut_off(hub, &person).await;
        done.insert((person, key));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Value {
        json!([
            {"id": "kai", "owner": true, "revoked": false},
            {"id": "sam", "owner": false, "revoked": true, "revoked_at": 5},
            {"id": "amy", "owner": false, "revoked": false},
        ])
    }

    fn who(cut: &[Cut]) -> Vec<&str> {
        cut.iter().map(|c| c.0.as_str()).collect()
    }

    fn key(id: &str, at: &str) -> Cut {
        (id.to_owned(), at.to_owned())
    }

    #[test]
    fn only_a_revoked_non_owner_or_a_held_person_the_core_dropped_is_cut() {
        let held = vec!["amy".to_owned(), "gone".to_owned(), "kai".to_owned()];
        let (cut, revoked) = to_cut(&rows(), &held, &BTreeSet::new());
        assert_eq!(who(&cut), ["sam", "gone"]);
        assert_eq!(revoked, BTreeSet::from([key("sam", "5")]));
        // already cut, nothing live of theirs: not again
        let (cut, _) = to_cut(&rows(), &[], &BTreeSet::from([key("sam", "5")]));
        assert_eq!(who(&cut), Vec::<&str>::new());
        // already cut but live rows came back (an app undo then a core-only revoke): cut
        let (cut, _) = to_cut(&rows(), &["sam".to_owned()], &BTreeSet::from([key("sam", "5")]));
        assert_eq!(who(&cut), ["sam"]);
    }

    #[test]
    fn a_new_revoke_of_the_same_person_is_cut_again_even_when_no_look_saw_them_unrevoked() {
        // handle-only person: the host holds nothing; revoked at 5, cut; undone and revoked again at 9 inside one poll window
        let done = BTreeSet::from([key("sam", "5")]);
        let mut again = rows();
        again[1]["revoked_at"] = json!(9);
        let (cut, _) = to_cut(&again, &[], &done);
        assert_eq!(cut, [key("sam", "9")]);
    }

    #[test]
    fn a_person_renamed_in_the_core_is_still_listed_under_the_old_id() {
        let renamed = json!([
            {"id": "kai", "owner": true, "revoked": false},
            {"id": "samuel", "owner": false, "revoked": false, "was": ["sam"]},
        ]);
        let (cut, _) = to_cut(&renamed, &["sam".to_owned(), "gone".to_owned()], &BTreeSet::new());
        assert_eq!(who(&cut), ["gone"], "sam is samuel's former id: left alone");
    }

    #[test]
    fn a_renamed_then_revoked_person_is_cut_under_every_id_the_host_may_hold() {
        let rows = json!([
            {"id": "kai", "owner": true, "revoked": false},
            {"id": "samuel", "owner": false, "revoked": true, "revoked_at": 7, "was": ["sam"]},
        ]);
        let (cut, _) = to_cut(&rows, &["sam".to_owned()], &BTreeSet::new());
        assert_eq!(cut, [key("sam", "7"), key("samuel", "7")]);
        // cut once, nothing live under either id: a second look leaves it
        let done = BTreeSet::from([key("samuel", "7"), key("sam", "7")]);
        assert_eq!(to_cut(&rows, &[], &done).0, Vec::<Cut>::new());
    }

    #[test]
    fn the_owner_is_never_cut_and_an_answer_without_one_cuts_no_one() {
        let mut r = rows();
        r[0]["revoked"] = json!(true);
        let (cut, _) = to_cut(&r, &["kai".to_owned()], &BTreeSet::new());
        assert_eq!(who(&cut), ["sam"], "the owner row is skipped even if it says revoked");
        for bad in [json!([]), json!([{"id": "sam", "owner": false, "revoked": true}]), json!(null), json!({})] {
            assert!(to_cut(&bad, &["sam".to_owned()], &BTreeSet::new()).0.is_empty(), "{bad}");
        }
    }
}
