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
//!
//! An owed cut (`ReconcileOwed.tla`, design 2026-11-17-rename-failcut): a revoke whose read of the person's ids failed
//! (the core down) cuts only the id it was given, while the host keeps a renamed person's rows under the old one, and
//! the core shows no revoke, so the reconcile above never cuts those. [`owe_cut`] keeps a durable row for the person,
//! and each tick pays it under `AUTH_WRITES` once the ids can be read again ([`pay_owed`]). Every undo clears it in its
//! own locked section ([`clear_owed`]), or the retry would cut a person the owner gave back (`undo-keeps.cfg`).

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
    /// One tick of the poll. `changed`: the core's revision moved since the last tick. Pays the owed cuts, then does
    /// nothing more unless switched.
    pub(crate) async fn tick(&mut self, hub: &Arc<Hub>, changed: bool) {
        // Before the switch test, so a row kept while switched is settled after a rollback too: the id then reads alone,
        // so it is cut again by that id and the row dropped; its former ids are given up (kith-auth knows no rename).
        pay_owed(hub).await;
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

/// One person as the host may hold them: every id ([`host_ids`]) and the first of them. A person is one identity across
/// all of these (R-rename, design 2026-11-16-rooms-rename), so a narrowing acts on every one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Held {
    /// The person's first id: the oldest former one (`was[0]`), or the id itself when never renamed. A rename only adds
    /// ids, so this one never changes: a room stay is written under it, and two joins that read the ids on either side
    /// of a rename still write one stay.
    pub first: String,
    /// The current id and every former one.
    pub ids: Vec<String>,
}

impl Held {
    /// Someone the core lists under no other id: `person` alone.
    pub(crate) fn alone(person: &str) -> Self {
        Self { first: person.to_owned(), ids: vec![person.to_owned()] }
    }
}

/// [`Held`] of `person` from the core's `listPeople` rows (`withWas`); `person` alone when no row names them.
fn held_in(rows: &Value, person: &str) -> Held {
    let Some(row) = rows.as_array().into_iter().flatten().find(|r| host_ids(r).iter().any(|i| i == person)) else {
        return Held::alone(person);
    };
    let first = row["was"].as_array().and_then(|w| w.first()).and_then(Value::as_str).or_else(|| row["id"].as_str());
    Held { first: first.unwrap_or(person).to_owned(), ids: host_ids(row) }
}

/// [`Held`] of `person` as the core lists them now; `person` alone when the host is not switched. An error when it is
/// and the core can't say: a caller that would write by these ids (a join, a removal) refuses rather than guess.
pub(crate) async fn held(person: &str) -> anyhow::Result<Held> {
    if !switched_now() {
        return Ok(Held::alone(person));
    }
    let said = owner_call("people.listPeople", json!({"viaSwitch": true, "withWas": true}), true).await?;
    Ok(held_in(&said["rows"], person))
}

/// [`host_ids`] of `person` as the core lists them now; just `person` when the host is not switched or the core can't say.
pub(crate) async fn held_under(person: &str) -> Vec<String> {
    held(person).await.map_or_else(|_| vec![person.to_owned()], |h| h.ids)
}

/// The host store's key of one owed cut: a revoke of the person whose ids could not be read, so some may be uncut.
const OWED: &str = "revoke.owed_cut.";

/// Records that `person` was cut off under the given id alone because their ids could not be read. Durable: a host
/// restart before the core is back still owes it. A failed write is logged; the owner was told to revoke again.
pub(crate) fn owe_cut(hub: &Hub, person: &str) {
    if let Err(e) = hub.store.kv_set(&format!("{OWED}{person}"), &crate::store::now_ms().to_string()) {
        tracing::warn!(error = format!("{e:#}"), person, "couldn't keep an owed cut: a former id may stay uncut");
    }
}

/// Who is owed a cut ([`owe_cut`]).
pub(crate) fn owed(hub: &Hub) -> Vec<String> {
    hub.store.kv_prefix(OWED).into_iter().filter_map(|k| k.strip_prefix(OWED).map(str::to_owned)).collect()
}

/// The host store's key of one undo that could not read the person's ids: what the host set aside under a former id may
/// not have been given back.
const UNREAD: &str = "revoke.undo_unread.";

/// What the owner is told on the person's row while [`UNREAD`] stands.
pub(crate) const UNREAD_NOTE: &str = "Undo isn't finished on this computer: Kith's core isn't answering, so what was set \
    aside under this person's earlier name may not be back yet. Start the core and undo the revoke again.";

/// An undo of `person` could not read the ids they are held under: said in the log, the audit and (until a later undo
/// reads them) on the person's row, not only given back under the one id.
fn note_undo_unread(hub: &Hub, person: &str, why: &str) {
    tracing::warn!(error = why, person, "an undo that can't read the person's ids: given back by this id only");
    hub.store.audit_add(person, "", "undo.unread", Some(why), false);
    if let Err(e) = hub.store.kv_set(&format!("{UNREAD}{person}"), &crate::store::now_ms().to_string()) {
        tracing::warn!(error = format!("{e:#}"), person, "couldn't keep the note of an unfinished undo");
    }
}

/// The note to show on `person`'s row: set by an undo that could not read the ids, until a later undo reads them or a
/// new revoke makes it moot.
pub(crate) fn undo_note(hub: &Hub, person: &str) -> Option<&'static str> {
    hub.store.kv_get(&format!("{UNREAD}{person}")).map(|_| UNREAD_NOTE)
}

/// Drops the note of an unfinished undo of `person`.
pub(crate) fn clear_undo_note(hub: &Hub, person: &str) {
    if let Err(e) = hub.store.kv_delete(&format!("{UNREAD}{person}")) {
        tracing::warn!(error = format!("{e:#}"), person, "couldn't drop the note of an unfinished undo");
    }
}

/// An undo's give-back: what the host set aside is restored under every id the person may be held under. When the core
/// can't say which, it is given back under `person` alone and the owner is told it is not finished ([`undo_note`]).
pub(crate) async fn restore_held(hub: &Hub, person: &str) {
    match held(person).await {
        Ok(h) => {
            for id in &h.ids {
                crate::api::people::restored(hub, id);
            }
            clear_undo_note(hub, person);
        }
        Err(e) => {
            note_undo_unread(hub, person, &format!("{e:#}"));
            crate::api::people::restored(hub, person);
        }
    }
}

/// An undo of `person`: no cut is owed any more under any id the host may hold them under. The caller holds
/// `AUTH_WRITES` for the section that writes the undo, so a tick paying the row can't land between the two.
pub(crate) async fn clear_owed(hub: &Hub, person: &str) {
    if owed(hub).is_empty() {
        return;
    }
    // A read that fails leaves the person's own row only; a row under a former id stays, and `pay_owed` refuses to cut a
    // person the core shows unrevoked. The undo's give-back says the same to the owner (`restore_held`).
    let mut ids = match held(person).await {
        Ok(h) => h.ids,
        Err(e) => {
            tracing::warn!(
                error = format!("{e:#}"),
                person,
                "an undo that can't read the person's ids: owed cuts kept"
            );
            Vec::new()
        }
    };
    if !ids.iter().any(|i| i == person) {
        ids.push(person.to_owned());
    }
    for id in ids {
        if let Err(e) = hub.store.kv_delete(&format!("{OWED}{id}")) {
            tracing::warn!(error = format!("{e:#}"), person = id, "couldn't drop an owed cut on an undo");
        }
    }
}

/// Pays every owed cut whose ids can be read now: under `AUTH_WRITES`, the person is cut off under every id and the row
/// dropped, one step against the undo. A read that fails leaves the row for the next tick. The rows are read under the
/// lock: one an undo cleared just before is not paid.
pub(crate) async fn pay_owed(hub: &Hub) {
    if owed(hub).is_empty() {
        return;
    }
    let _w = AUTH_WRITES.lock().await;
    for person in owed(hub) {
        // Never cut a person the owner has since undone: the undo's clear may have missed this row (its ids unread).
        match still_revoked(&person).await {
            Ok(false) => {
                tracing::info!(person, "an owed cut is dropped: the person is not revoked any more");
                if let Err(e) = hub.store.kv_delete(&format!("{OWED}{person}")) {
                    tracing::warn!(error = format!("{e:#}"), person, "couldn't drop an owed cut of an undone person");
                }
                continue;
            }
            Ok(true) => {}
            Err(_) => continue,
        }
        let Ok(held) = held(&person).await else { continue };
        tracing::info!(person, ids = ?held.ids, "an owed cut is paid: the person is cut off under every id");
        for id in &held.ids {
            cut_off(hub, id).await;
        }
        if let Err(e) = hub.store.kv_delete(&format!("{OWED}{person}")) {
            tracing::warn!(error = format!("{e:#}"), person, "couldn't drop a paid owed cut: it is paid again");
        }
    }
}

/// Whether the core's `listPeople` rows show `person` (by any id) revoked. None when no row names them: not known.
fn revoked_in(rows: &Value, person: &str) -> Option<bool> {
    let row = rows.as_array()?.iter().find(|r| host_ids(r).iter().any(|i| i == person))?;
    Some(row["revoked"] == true)
}

/// Whether `person` is revoked now, for an owed cut. Switched: the core's rows. Not switched (a rolled back core):
/// kith-auth's. A person no row names counts as revoked (a removal is cut as before). An error when it can't be read.
async fn still_revoked(person: &str) -> anyhow::Result<bool> {
    if switched_now() {
        let said = owner_call("people.listPeople", json!({"viaSwitch": true, "withWas": true}), true).await?;
        return Ok(revoked_in(&said["rows"], person).unwrap_or(true));
    }
    let bin = crate::remote::account::bin().await.ok_or_else(|| anyhow::anyhow!("kith-auth wasn't found"))?;
    Ok(crate::remote::members::auth_person(&bin, person).await?.is_none_or(|p| p.revoked_at.is_some()))
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
    fn a_person_the_core_shows_unrevoked_is_not_revoked_for_an_owed_cut() {
        let r = json!([
            {"id": "kai", "owner": true},
            {"id": "samuel", "revoked": false, "was": ["sam"]},
            {"id": "amy", "revoked": true},
        ]);
        assert_eq!(revoked_in(&r, "sam"), Some(false), "asked by the former id: undone");
        assert_eq!(revoked_in(&r, "amy"), Some(true));
        assert_eq!(revoked_in(&r, "gone"), None, "no row: unknown");
        assert_eq!(revoked_in(&json!(null), "sam"), None);
    }

    #[test]
    fn the_note_of_an_unfinished_undo_stands_until_a_later_undo_reads_the_ids() {
        let store = crate::store::Store::open(std::path::Path::new(":memory:")).unwrap();
        let dir = std::env::temp_dir().join(format!("kith-undo-note-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let identity = crate::remote::identity::Identity::load_or_create(&dir).unwrap();
        let hub = Hub::new(store, "t".into(), identity, "t".into(), 0);
        assert_eq!(undo_note(&hub, "sam"), None);
        note_undo_unread(&hub, "sam", "core down");
        assert_eq!(undo_note(&hub, "sam"), Some(UNREAD_NOTE));
        assert_eq!(undo_note(&hub, "amy"), None);
        let audit = hub.store.audit_list(0, Some("sam")).unwrap();
        assert!(audit.iter().any(|a| a.method == "undo.unread" && !a.allowed), "{audit:?}");
        clear_undo_note(&hub, "sam");
        assert_eq!(undo_note(&hub, "sam"), None);
    }

    #[test]
    fn the_first_id_is_the_oldest_former_one_and_never_moves_on_a_rename() {
        let once = json!([{"id": "kai", "owner": true}, {"id": "samuel", "was": ["sam"]}, {"id": "amy"}]);
        let want = Held { first: "sam".into(), ids: vec!["samuel".into(), "sam".into()] };
        assert_eq!(held_in(&once, "samuel"), want, "asked by the current id");
        assert_eq!(held_in(&once, "sam"), want, "asked by the old one");
        let twice = json!([{"id": "sammy", "was": ["sam", "samuel"]}]);
        assert_eq!(held_in(&twice, "samuel").first, "sam", "a second rename keeps the first id");
        assert_eq!(held_in(&once, "amy"), Held::alone("amy"), "never renamed: the id itself");
        assert_eq!(held_in(&once, "gone"), Held::alone("gone"), "not listed: alone");
        assert_eq!(held_in(&json!(null), "sam"), Held::alone("sam"));
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
