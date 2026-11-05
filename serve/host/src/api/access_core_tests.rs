//! M2-17 / M2-18a: the owner's `setPersonAccess` with `KITH_CORE_AUTHORITY` on and off, against a real kith-auth and a
//! real core daemon. On: kith-auth is written first (auth.json is the truth until the switch) and the ticks are then
//! mirrored as `people.grant` / `people.revoke` in the core; a grant made in the core shows in the host's listing; a core
//! that can't be reached only misses the mirror. A core-only change would close the gate and let a revoked person back
//! in, so none is made. Off: kith-auth as before and the core untouched.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::authority::{CoreData, TEST_CORE};
use super::authority_props::{Tmp, folders};
use super::devices::Caller;
use super::dispatch;
use super::rig::Rig;
use super::roles;
use crate::remote::members::{self, Listed};
use serde_json::{Value, json};
use std::sync::Arc;

async fn call(r: &Rig, method: &str, b: Value) -> anyhow::Result<Value> {
    dispatch(&r.hub, &Caller::Local, method, b).await
}

/// Sam, invited with `house` and `money` through kith-auth (the switch is off while this runs).
async fn joined(r: &Rig) {
    let made = call(r, "createInvite", json!({"name": "Sam", "agents": {"house": "member", "money": "member"}}))
        .await
        .unwrap();
    let listed = [Listed {
        membership_id: "mem_sam".into(),
        user_id: "user_sam".into(),
        invite_id: made["inviteId"].as_str().unwrap().to_owned(),
        ..Default::default()
    }];
    members::apply(&r.hub, Some(&r.bin), &listed, Some("user_owner")).await.unwrap();
}

/// A core daemon beside the rig's kith-auth, with the owner and `sam`. `on()` makes it the host's authority.
struct Core {
    tmp: Tmp,
    daemon: Option<crate::core_front::TestDaemon>,
    data: Arc<CoreData>,
}

impl Core {
    fn start(r: &Rig) -> Self {
        let tmp = Tmp::new();
        let home = r.dir.join("home");
        let env = json!({"KITH_AGENTS": home.join("agents").to_string_lossy()});
        let data = Arc::new(CoreData::new(
            folders(&tmp.path().join("machine")),
            home.clone(),
            home.join("kernel"),
            env.as_object().unwrap().clone(),
        ));
        let daemon = crate::core_front::serve_for_test(&data.folders);
        let core = Self { tmp, daemon: Some(daemon), data };
        core.ask("people.addPerson", json!({"id": "kai", "owner": true})).expect("owner");
        core.ask("people.addPerson", json!({"id": "sam"})).expect("sam");
        core
    }

    fn ask(&self, method: &str, mut params: Value) -> Option<Value> {
        params["home"] = self.data.home.to_str().unwrap().into();
        params["kernel"] = self.data.kernel.to_str().unwrap().into();
        params["env"] = Value::Object(self.data.env.clone());
        crate::core_front::ask(&self.data.folders, method, &params)
    }

    fn on(&self) {
        *TEST_CORE.lock().unwrap() = Some(self.data.clone());
    }

    /// The gate closes: the guard falls back to kith-auth.
    #[allow(clippy::unused_self)] // kept a method so it reads next to `on`
    fn off(&self) {
        *TEST_CORE.lock().unwrap() = None;
    }

    /// What the core says sam holds by their own row: (agent, preset).
    fn rows(&self) -> Vec<(String, String)> {
        let list = self.ask("people.list", json!({"person": "sam"})).expect("list");
        list["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (r["agent"].as_str().unwrap().to_owned(), r["preset"].as_str().unwrap().to_owned()))
            .collect()
    }

    /// The gate is closed (a write by kith-auth moved auth.json a moment ago) while the opt-in stands.
    #[allow(clippy::unused_self)] // kept a method so it reads next to `on`
    fn shut_gate(&self) {
        crate::api::authority::TEST_GATE_SHUT.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// auth.json is a tombstone: the core is the only store, with or without the env.
    fn switched(&self) {
        let auth = self.tmp.path().join("auth.json");
        std::fs::write(&auth, r#"{"kith_tombstone": 1, "module": "authority", "rollback": "kith rollback authority"}"#)
            .unwrap();
        *crate::api::authority::TEST_AUTH_FILE.lock().unwrap() = Some(auth);
    }

    fn stop(&mut self) {
        if let Some(d) = self.daemon.take() {
            d.stop();
        }
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        crate::api::authority::TEST_GATE_SHUT.store(false, std::sync::atomic::Ordering::SeqCst);
        *crate::api::authority::TEST_AUTH_FILE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        *TEST_CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        self.stop();
    }
}

#[tokio::test]
async fn with_the_switch_on_kith_auth_is_written_first_and_the_core_is_told_after() {
    let r = Rig::new().await;
    joined(&r).await;
    let core = Core::start(&r);
    core.on();

    let out = call(&r, "setPersonAccess", json!({"personId": "sam", "agents": {"money": "member"}})).await.unwrap();
    assert_eq!(core.rows(), [("money".to_owned(), "member".to_owned())], "the core has the grant");
    assert_eq!(out["person"]["agents"], json!({"money": "member"}), "the answer has its old shape");
    assert!(!r.allowed("sam", "house"), "kith-auth took house away: the file is written");
    assert!(r.allowed("sam", "money"));

    // Sent twice, the same.
    let again = call(&r, "setPersonAccess", json!({"personId": "sam", "agents": {"money": "member"}})).await.unwrap();
    assert_eq!(again["person"]["agents"], out["person"]["agents"]);
    assert_eq!(core.rows().len(), 1);

    // A grant made in the core shows in the listing and survives the 10-second refresh.
    core.ask("people.grant", json!({"person": "sam", "agent": "house", "preset": "guest"})).unwrap();
    let listed = call(&r, "people", json!({})).await.unwrap();
    let sam = listed["people"].as_array().unwrap().iter().find(|p| p["personId"] == "sam").unwrap();
    assert_eq!(sam["agents"], json!({"house": "guest", "money": "member"}), "{sam}");
    members::refresh_mirror(&r.hub).await;
    assert_eq!(r.hub.store.person("sam").unwrap().agents.len(), 2);
    core.ask("people.revoke", json!({"person": "sam", "agent": "house"})).unwrap();

    // Unticking takes the rows away in the file and in the core.
    let none = call(&r, "setPersonAccess", json!({"personId": "sam", "agents": {}})).await.unwrap();
    assert_eq!(none["person"]["agents"], json!({}));
    assert_eq!(core.rows(), Vec::<(String, String)>::new());
    assert!(!r.allowed("sam", "house") && !r.allowed("sam", "money"));
}

#[tokio::test]
async fn a_second_change_while_the_gate_is_closed_still_reaches_the_core() {
    let r = Rig::new().await;
    joined(&r).await;
    let core = Core::start(&r);
    core.on();
    call(&r, "setPersonAccess", json!({"personId": "sam", "agents": {"money": "member"}})).await.unwrap();

    // The first write closed the gate for up to 2 s; the opt-in stands, so the second is mirrored all the same.
    core.shut_gate();
    assert!(!super::authority::guard_on_core());
    call(&r, "setPersonAccess", json!({"personId": "sam", "agents": {"money": "member", "house": "guest"}}))
        .await
        .unwrap();
    let mut rows = core.rows();
    rows.sort();
    assert_eq!(rows, [("house".to_owned(), "guest".to_owned()), ("money".to_owned(), "member".to_owned())]);
    call(&r, "setPersonAccess", json!({"personId": "sam", "agents": {"money": "member"}})).await.unwrap();
    assert_eq!(core.rows(), [("money".to_owned(), "member".to_owned())], "an unticked row goes there too");
}

#[tokio::test]
async fn with_the_switch_on_a_core_that_cannot_be_reached_only_misses_the_mirror() {
    let r = Rig::new().await;
    joined(&r).await;
    let mut core = Core::start(&r);
    core.on();
    core.stop();

    // kith-auth is the truth and was written; the core is behind, which closes the gate (not tested here) and is logged.
    call(&r, "setPersonAccess", json!({"personId": "sam", "agents": {"money": "member"}})).await.unwrap();
    assert!(!r.allowed("sam", "house"), "the file took house away");
    assert!(r.allowed("sam", "money"));
}

#[tokio::test]
async fn a_revoke_through_the_host_cannot_be_undone_by_the_gate_closing() {
    let r = Rig::new().await;
    joined(&r).await;
    let core = Core::start(&r);
    core.on();

    call(&r, "setPersonAccess", json!({"personId": "sam", "agents": {"money": "member"}})).await.unwrap();
    // The core moved, so the gate closes within seconds and the guard asks kith-auth. Sam is refused there too: the
    // revoke was made in the file first. (A core-only revoke would leave `house` in the file and let them back in.)
    core.off();
    assert!(!roles::may_use("sam", "house").await, "kith-auth no longer has the row");
    assert!(roles::may_use("sam", "money").await);
    assert!(!r.allowed("sam", "house"));
}

#[tokio::test]
async fn with_the_switch_off_kith_auth_is_written_and_the_core_is_not_touched() {
    let r = Rig::new().await;
    joined(&r).await;
    let core = Core::start(&r);

    let out = call(&r, "setPersonAccess", json!({"personId": "sam", "agents": {"money": "member"}})).await.unwrap();
    assert_eq!(out["person"]["agents"], json!({"money": "member"}));
    assert!(!r.allowed("sam", "house"), "kith-auth took house away");
    assert!(r.allowed("sam", "money"));
    assert!(core.rows().is_empty(), "the core was never asked");
}

// M2-18 P3b: the host decides from the core's gate (a tombstoned auth.json), not the env alone, and a live stream asks
// the gate for every event.

type Events = std::pin::Pin<Box<dyn futures::Stream<Item = Value> + Send>>;

/// Sam's own chat on `house`, the host's mirror of Sam emptied (so only the core can grant them `house`), and Sam in the
/// core with `house`.
fn sam_only_in_the_core(r: &Rig, core: &Core) {
    r.hub.store.save_bot(&super::authority_props::chat("sam-house", Some("sam"), "house")).unwrap();
    r.hub.store.set_person_agents("sam", &std::collections::BTreeMap::new()).unwrap();
    core.ask("people.grant", json!({"person": "sam", "agent": "house", "preset": "member"})).expect("grant");
}

fn say(r: &Rig, rev: i64) {
    let entry = json!({
        "id": format!("e{rev}"), "rev": rev, "turn": 1, "createdAt": 5, "kind": "agent",
        "data": {"text": "hello"}, "botId": "sam-house", "threadId": null,
    });
    r.hub.events.send(json!({"type": "entry", "rev": rev, "entry": entry})).unwrap();
}

/// Opens Sam's stream and takes the hello.
async fn sam_stream(r: &Rig) -> Events {
    use futures::StreamExt as _;
    let sam = r.device("sam-phone", Some("sam"));
    let mut s: Events = Box::pin(super::events_stream(&r.hub, 0, None, &sam).unwrap());
    assert_eq!(s.next().await.unwrap()["type"], "hello");
    s
}

/// The next event within a moment, or None.
async fn next_in_a_moment(s: &mut Events) -> Option<Value> {
    use futures::StreamExt as _;
    tokio::time::timeout(std::time::Duration::from_millis(400), s.next()).await.ok().flatten()
}

#[tokio::test]
async fn a_stream_opened_with_the_gate_open_stops_a_removed_members_events_when_the_gate_closes() {
    let r = Rig::new().await;
    joined(&r).await;
    let core = Core::start(&r);
    sam_only_in_the_core(&r, &core);
    core.on();
    let mut s = sam_stream(&r).await;

    say(&r, 10);
    assert!(next_in_a_moment(&mut s).await.is_some(), "the core grants them, the gate is open");

    // A file-only write shut the gate; the host's copy has no row for Sam: the next event of this same stream is the host's.
    core.shut_gate();
    say(&r, 11);
    assert!(next_in_a_moment(&mut s).await.is_none(), "a stream chosen once would keep asking the core");
}

#[tokio::test]
async fn a_switched_host_without_the_env_answers_from_the_core() {
    let r = Rig::new().await;
    joined(&r).await;
    let core = Core::start(&r);
    sam_only_in_the_core(&r, &core);
    core.on();
    core.switched();
    // The latch is shut and no env stands: the tombstone alone selects the core.
    core.shut_gate();
    assert!(super::authority::guard_on_core());
    let mut s = sam_stream(&r).await;
    say(&r, 10);
    assert!(next_in_a_moment(&mut s).await.is_some());

    // The host's kith-auth calls go to the core, and kith-auth is not touched.
    super::authority::run_auth(&r.bin, &["member", "house", "sam", "preset=guest"]).await.unwrap();
    assert_eq!(core.rows(), [("house".to_owned(), "guest".to_owned())]);
    let said = super::authority::run_auth(&r.bin, &["people", "sam"]).await;
    assert!(said.is_ok(), "{said:?}");
    assert!(super::authority::run_auth(&r.bin, &["people", "ghost"]).await.is_err());
    assert!(super::authority::run_auth(&r.bin, &["retired", "sam"]).await.is_err(), "not retired");
    super::authority::run_auth(&r.bin, &["bot", "sam-bot", "--of", "sam", "name=Bot", "provider=claude"])
        .await
        .unwrap();
    super::authority::run_auth(&r.bin, &["bot", "sam-bot", "--rm"]).await.unwrap();
    super::authority::run_auth(&r.bin, &["member", "house", "sam", "--rm"]).await.unwrap();
    assert_eq!(core.rows(), Vec::<(String, String)>::new());
    assert!(r.allowed("sam", "house"), "the file still has the row: kith-auth was never asked");
    // A call the core has no method for is refused, not run on the tombstone.
    assert!(super::authority::run_auth(&r.bin, &["ledger"]).await.is_err());
}

#[tokio::test]
async fn a_switched_host_with_the_core_down_denies_and_never_reads_kith_auth() {
    let r = Rig::new().await;
    joined(&r).await;
    let mut core = Core::start(&r);
    // The host's copy and the file both still grant Sam `house`: only the fail-closed rule can deny.
    r.hub.store.save_bot(&super::authority_props::chat("sam-house", Some("sam"), "house")).unwrap();
    core.on();
    core.switched();
    let mut s = sam_stream(&r).await;
    core.stop();
    say(&r, 11);
    assert!(next_in_a_moment(&mut s).await.is_none(), "no core, no events");
    assert!(super::authority::run_auth(&r.bin, &["member", "house", "sam", "--rm"]).await.is_err());
    assert!(r.allowed("sam", "house"), "nothing was written another way");
    assert!(!roles::may_use("sam", "house").await, "the guard denies too");
}

#[tokio::test]
async fn unswitched_the_gate_decides_and_kith_auth_is_written() {
    let r = Rig::new().await;
    joined(&r).await;
    let core = Core::start(&r);
    r.hub.store.save_bot(&super::authority_props::chat("sam-house", Some("sam"), "house")).unwrap();
    core.on();
    assert!(!super::authority::switched());
    // The host's copy grants Sam, the core does not know the row: the open gate reads the core and says no.
    let mut s = sam_stream(&r).await;
    say(&r, 10);
    assert!(next_in_a_moment(&mut s).await.is_none());
    core.shut_gate();
    say(&r, 11);
    assert!(next_in_a_moment(&mut s).await.is_some(), "a shut gate is the host's copy, as before");

    super::authority::run_auth(&r.bin, &["member", "house", "sam", "--rm"]).await.unwrap();
    assert!(!r.allowed("sam", "house"), "kith-auth was written");
    assert_eq!(core.rows(), Vec::<(String, String)>::new(), "the core was not");
}

#[tokio::test]
async fn a_switched_host_reads_the_people_from_the_core_and_revokes_there() {
    let r = Rig::new().await;
    joined(&r).await;
    let core = Core::start(&r);
    sam_only_in_the_core(&r, &core);
    core.on();
    core.switched();
    // `auth_people` parses the core's rows like kith-auth's.
    let people = members::auth_people(&r.bin).await.unwrap();
    assert!(people.iter().any(|p| p.id == "sam" && !p.owner), "{people:?}");
    assert!(people.iter().any(|p| p.id == "kai" && p.owner));
    // A cloud revoke (`apply_revoked`) goes the same way as the owner's: people.suspend, kith-auth untouched.
    super::people::local_revoke(&r.hub, &r.bin, "sam", true).await.unwrap();
    let said = core.ask("people.may", json!({"person": "sam", "agent": "house"})).unwrap();
    assert_eq!(said["allowed"], false, "the core revoked them: {said}");
    assert!(r.allowed("sam", "house"), "kith-auth was not asked");
    super::people::local_revoke(&r.hub, &r.bin, "sam", false).await.unwrap();
    let said = core.ask("people.may", json!({"person": "sam", "agent": "house"})).unwrap();
    assert_eq!(said["allowed"], true);
    let none = members::apply_revoked(Some(&r.bin), Some(&[])).await.unwrap();
    assert_eq!(none.cut.len() + none.restored.len(), 0);
    // The chat handle has no core method: refused, never run on the tombstone.
    let e = members::locked_run(&r.bin, &["handle", "sam", "kith", "user_x"]).await.unwrap_err();
    assert!(format!("{e:#}").contains("no core method"), "{e:#}");
}

#[tokio::test]
async fn a_switched_revoke_with_the_core_down_still_cuts_the_person_off_here() {
    let r = Rig::new().await;
    joined(&r).await;
    let mut core = Core::start(&r);
    core.on();
    core.switched();
    core.stop();
    assert!(super::people::local_revoke(&r.hub, &r.bin, "sam", true).await.is_err());
    assert!(r.hub.store.suspended_people().unwrap().contains(&"sam".to_owned()), "cut_off ran");
}

#[tokio::test]
async fn a_tombstone_file_is_what_switches_the_host_and_taking_it_away_switches_back() {
    let r = Rig::new().await;
    let core = Core::start(&r);
    core.on();
    assert!(!super::authority::switched());
    core.switched();
    assert!(super::authority::switched() && super::authority::guard_on_core());
    // The way back (`kith rollback authority`) puts a plain file back: unswitched at once.
    let auth = crate::api::authority::TEST_AUTH_FILE.lock().unwrap().clone().unwrap();
    std::fs::remove_file(&auth).unwrap();
    std::fs::write(&auth, r#"{"people": {}}"#).unwrap();
    assert!(!super::authority::switched());
    // An auth.json that exists but can't be read is not "not a tombstone": it fails closed.
    std::fs::remove_file(&auth).unwrap();
    std::fs::create_dir(&auth).unwrap();
    assert!(super::authority::switched());
    std::fs::remove_dir(&auth).unwrap();
    assert!(!super::authority::switched(), "missing is the unswitched rules");
}
