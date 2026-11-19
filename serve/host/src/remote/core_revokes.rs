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
    if let Err(e) = hub.store.kv_set(&format!("{OWED}{person}"), &next_seq(hub).to_string()) {
        tracing::warn!(error = format!("{e:#}"), person, "couldn't keep an owed cut: a former id may stay uncut");
    }
}

/// Who is owed a cut ([`owe_cut`]).
pub(crate) fn owed(hub: &Hub) -> Vec<String> {
    hub.store.kv_prefix(OWED).into_iter().filter_map(|k| k.strip_prefix(OWED).map(str::to_owned)).collect()
}

/// The host store's key of the order of the revoke path's durable rows: each owed row and undone marker takes the next
/// number, so which came last is never read off a clock that can step back. Taken in one step under [`SEQ_STEP`]: the
/// owed rows are written both under `AUTH_WRITES` and, after a revoke, without it.
const SEQ: &str = "revoke.seq";

static SEQ_STEP: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn next_seq(hub: &Hub) -> u64 {
    let _step = SEQ_STEP.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let last = hub.store.kv_get(SEQ).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    let next = last.saturating_add(1).max(u64::try_from(crate::store::now_ms()).unwrap_or(0));
    if let Err(e) = hub.store.kv_set(SEQ, &next.to_string()) {
        tracing::warn!(error = format!("{e:#}"), "couldn't keep the revoke order counter");
    }
    next
}

/// The host store's key of an undo whose read of the person's ids failed: an owed cut older than it is of a revoke the
/// owner has since undone, and is dropped, never paid (a revoke that failed leaves no such marker, so it is paid).
const UNDONE: &str = "revoke.undone.";

fn mark_undone(hub: &Hub, person: &str) {
    if let Err(e) = hub.store.kv_set(&format!("{UNDONE}{person}"), &next_seq(hub).to_string()) {
        tracing::warn!(error = format!("{e:#}"), person, "couldn't keep a marker of an undo: an owed cut may be paid");
    }
}

/// The host store's key of one undo that could not read the person's ids: what the host set aside under a former id may
/// not have been given back.
const UNREAD: &str = "revoke.undo_unread.";

/// What the owner is told on the person's row while [`UNREAD`] stands. The core is named only when it was the reason.
const UNREAD_NOTE: &str = "Undo isn't finished on this computer: Kith couldn't read this person's earlier names, so \
    what was set aside under one of them may not be back yet. Revoke them and undo again once Kith is running normally.";
const UNREAD_NOTE_CORE_DOWN: &str = "Undo isn't finished on this computer: Kith's core isn't running, so what was set \
    aside under this person's earlier name may not be back yet. Start the core, then revoke them and undo again.";

/// An undo of `person` could not read the ids they are held under: said in the log, the audit and (until a later undo
/// reads them) on the person's row, and an owed cut older than it is never paid.
fn note_undo_unread(hub: &Hub, person: &str, e: &anyhow::Error) {
    let why = format!("{e:#}");
    tracing::warn!(error = why, person, "an undo that can't read the person's ids: given back by this id only");
    hub.store.audit_add(person, "", "undo.unread", Some(&why), false);
    let kind = if e.downcast_ref::<crate::api::authority::CoreDown>().is_some() { "core" } else { "other" };
    if let Err(e) = hub.store.kv_set(&format!("{UNREAD}{person}"), kind) {
        tracing::warn!(error = format!("{e:#}"), person, "couldn't keep the note of an unfinished undo");
    }
    // No marker here: this runs after the undo's locked section, and a revoke owed in between must still be paid.
    // The undo's marker is written by `clear_owed`, under the lock.
}

/// The note to show on a person's row: set by an undo that could not read the ids, under any id they are held under,
/// until a later undo reads them or a new revoke makes it moot.
pub(crate) fn undo_note(hub: &Hub, ids: &[String]) -> Option<&'static str> {
    let kinds: Vec<String> = ids.iter().filter_map(|i| hub.store.kv_get(&format!("{UNREAD}{i}"))).collect();
    let first = kinds.first()?;
    Some(if first == "core" { UNREAD_NOTE_CORE_DOWN } else { UNREAD_NOTE })
}

/// Whether any note of an unfinished undo exists (a list then looks up each person's ids).
pub(crate) fn any_undo_note(hub: &Hub) -> bool {
    !hub.store.kv_prefix(UNREAD).is_empty()
}

/// Drops the note of an unfinished undo under each of `ids`.
pub(crate) fn clear_undo_note(hub: &Hub, ids: &[String]) {
    for id in ids {
        if let Err(e) = hub.store.kv_delete(&format!("{UNREAD}{id}")) {
            tracing::warn!(error = format!("{e:#}"), person = id, "couldn't drop the note of an unfinished undo");
        }
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
            clear_undo_note(hub, &h.ids);
        }
        Err(e) => {
            note_undo_unread(hub, person, &e);
            crate::api::people::restored(hub, person);
        }
    }
}

/// A revoke's set-aside: what the host keeps for the undo (devices, memberships, open invites) is set aside under every id
/// the person may be held under. The caller holds `AUTH_WRITES` from the flip of the revoked flag to here, as the give-back
/// ([`restore_held`]) does, so a late give-back cannot land after a re-revoke.
pub(crate) async fn set_aside_held(hub: &Hub, person: &str) {
    for id in held_under(person).await {
        match hub.store.suspend_person(&id) {
            Ok(m) => tracing::info!(person = id, ?m, "a revoke set the person's devices aside here"),
            Err(e) => tracing::warn!(error = format!("{e:#}"), person = id, "couldn't set aside a revoked person"),
        }
    }
}

/// An undo of `person`: no cut is owed any more under any id the host may hold them under. The caller holds
/// `AUTH_WRITES` for the section that writes the undo, so a tick paying the row can't land between the two.
pub(crate) async fn clear_owed(hub: &Hub, person: &str) {
    if owed(hub).is_empty() {
        return;
    }
    // A read that fails clears the person's own row only; a row under a former id stays, and the marker tells `pay_owed`
    // that the undo came after it.
    let mut ids = match held(person).await {
        Ok(h) => h.ids,
        Err(e) => {
            tracing::warn!(error = format!("{e:#}"), person, "an undo that can't read the person's ids: marked undone");
            mark_undone(hub, person);
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

/// The newest undo marker on any of `ids` (0 when none).
fn undone_at(hub: &Hub, ids: &[String]) -> u64 {
    ids.iter().filter_map(|i| hub.store.kv_get(&format!("{UNDONE}{i}"))?.parse().ok()).max().unwrap_or(0)
}

/// Pays every owed cut whose ids can be read now: under `AUTH_WRITES`, the person is cut off under every id and the row
/// dropped, one step against the undo. A read that fails leaves the row for the next tick. The ids are read once; a row
/// is dropped unpaid only when an undo whose own read failed (a marker on one of the ids) came after it. The rows are
/// read under the lock: one an undo cleared just before is not paid.
pub(crate) async fn pay_owed(hub: &Hub) {
    if owed(hub).is_empty() {
        return;
    }
    let _w = AUTH_WRITES.lock().await;
    let mut handled: Vec<String> = Vec::new();
    // A row whose ids could not be read waits for a later tick and may still need its person's markers.
    let mut unread = false;
    for person in owed(hub) {
        let Ok(held) = held(&person).await else {
            unread = true;
            continue;
        };
        let row = hub.store.kv_get(&format!("{OWED}{person}")).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
        if undone_at(hub, &held.ids) > row {
            tracing::info!(person, ids = ?held.ids, "an owed cut is dropped: the person was undone after it");
        } else {
            tracing::info!(person, ids = ?held.ids, "an owed cut is paid: the person is cut off under every id");
            for id in &held.ids {
                cut_off(hub, id).await;
            }
        }
        if let Err(e) = hub.store.kv_delete(&format!("{OWED}{person}")) {
            tracing::warn!(error = format!("{e:#}"), person, "couldn't drop a handled owed cut: it is handled again");
        }
        handled.extend(held.ids);
    }
    // After every row: a marker one row used must still stand for another row of the same person (owed under each id).
    for id in handled.into_iter().filter(|_| !unread) {
        let _ = hub.store.kv_delete(&format!("{UNDONE}{id}"));
    }
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
    fn the_note_of_an_unfinished_undo_stands_until_a_later_undo_reads_the_ids() {
        let store = crate::store::Store::open(std::path::Path::new(":memory:")).unwrap();
        let dir = std::env::temp_dir().join(format!("kith-undo-note-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let identity = crate::remote::identity::Identity::load_or_create(&dir).unwrap();
        let hub = Hub::new(store, "t".into(), identity, "t".into(), 0);
        let sam = vec!["samuel".to_owned(), "sam".to_owned()];
        assert_eq!(undo_note(&hub, &sam), None);
        note_undo_unread(&hub, "sam", &anyhow::anyhow!("odd"));
        assert_eq!(undo_note(&hub, &sam), Some(UNREAD_NOTE), "found under a former id");
        assert_eq!(undo_note(&hub, &["amy".to_owned()]), None);
        let down = anyhow::Error::new(crate::api::authority::CoreDown("x".into()));
        note_undo_unread(&hub, "amy", &down);
        assert_eq!(
            undo_note(&hub, &["amy".to_owned()]),
            Some(UNREAD_NOTE_CORE_DOWN),
            "core named only when it was down"
        );
        let audit = hub.store.audit_list(0, Some("sam")).unwrap();
        assert!(audit.iter().any(|a| a.method == "undo.unread" && !a.allowed), "{audit:?}");
        clear_undo_note(&hub, &sam);
        assert_eq!(undo_note(&hub, &sam), None);
    }

    #[test]
    fn the_order_counter_never_steps_back_and_a_marker_is_newer_only_than_an_older_row() {
        let store = crate::store::Store::open(std::path::Path::new(":memory:")).unwrap();
        let dir = std::env::temp_dir().join(format!("kith-undo-seq-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let identity = crate::remote::identity::Identity::load_or_create(&dir).unwrap();
        let hub = Hub::new(store, "t".into(), identity, "t".into(), 0);
        let a = next_seq(&hub);
        let b = next_seq(&hub);
        assert!(b > a);
        let ids = vec!["sam".to_owned()];
        assert_eq!(undone_at(&hub, &ids), 0);
        owe_cut(&hub, "sam");
        mark_undone(&hub, "sam");
        let row: u64 = hub.store.kv_get(&format!("{OWED}sam")).unwrap().parse().unwrap();
        assert!(undone_at(&hub, &ids) > row, "an undo after the row");
        owe_cut(&hub, "sam");
        let row: u64 = hub.store.kv_get(&format!("{OWED}sam")).unwrap().parse().unwrap();
        assert!(undone_at(&hub, &ids) < row, "a revoke after the undo is paid");
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
