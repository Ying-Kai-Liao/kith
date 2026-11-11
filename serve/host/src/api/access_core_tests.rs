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
pub(super) struct Core {
    tmp: Tmp,
    daemon: Option<crate::core_front::TestDaemon>,
    data: Arc<CoreData>,
}

impl Core {
    pub(super) fn start(r: &Rig) -> Self {
        Self::start_at(&r.dir)
    }

    /// The same beside any test folder that has (or is given) a `home/agents`.
    pub(super) fn start_at(dir: &std::path::Path) -> Self {
        let tmp = Tmp::new();
        let home = dir.join("home");
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

    pub(super) fn ask(&self, method: &str, mut params: Value) -> Option<Value> {
        params["home"] = self.data.home.to_str().unwrap().into();
        params["kernel"] = self.data.kernel.to_str().unwrap().into();
        params["env"] = Value::Object(self.data.env.clone());
        crate::core_front::ask(&self.data.folders, method, &params)
    }

    pub(super) fn on(&self) {
        *TEST_CORE.lock().unwrap() = Some(self.data.clone());
    }

    /// The gate closes: the guard falls back to kith-auth.
    #[allow(clippy::unused_self)] // kept a method so it reads next to `on`
    pub(super) fn off(&self) {
        *TEST_CORE.lock().unwrap() = None;
    }

    /// What the core says sam holds by their own row: (agent, preset).
    pub(super) fn rows(&self) -> Vec<(String, String)> {
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
    pub(super) fn switched(&self) {
        self.switched_over(&self.tmp.path().join("auth.json"));
        self.phase("switched");
    }

    /// The core's cutover record for authority (`state=` of `core/cutover/authority.switch`): what it checks a
    /// `viaSwitch` write against.
    pub(super) fn phase(&self, state: &str) {
        let dir = self.tmp.path().join("machine/core/cutover");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("authority.switch"), format!("state={state}\nat=1\n")).unwrap();
    }

    /// The rollback ended: the core's cutover record for authority is gone.
    pub(super) fn clear_phase(&self) {
        std::fs::remove_file(self.tmp.path().join("machine/core/cutover/authority.switch")).ok();
    }

    /// The tombstone is the file `auth` (the one kith-auth reads in the rig), so anything that still runs kith-auth finds it.
    #[allow(clippy::unused_self)] // kept a method so it reads next to `switched`
    pub(super) fn switched_over(&self, auth: &std::path::Path) {
        std::fs::write(auth, r#"{"kith_tombstone": 1, "module": "authority", "rollback": "kith rollback authority"}"#)
            .unwrap();
        *crate::api::authority::TEST_AUTH_FILE.lock().unwrap() = Some(auth.to_path_buf());
    }

    pub(super) fn stop(&mut self) {
        if let Some(d) = self.daemon.take() {
            d.stop();
        }
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        crate::api::authority::TEST_GATE_SHUT.store(false, std::sync::atomic::Ordering::SeqCst);
        *crate::api::authority::TEST_AUTH_FILE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        *crate::api::authority::TEST_STALE_SWITCHED.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
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
    // The chat handle is the core's (`people.handle`): bound and let go there, never run on the tombstone.
    members::locked_run(&r.bin, &["handle", "sam", "kith", "user_x"]).await.unwrap();
    assert_eq!(crate::remote::account::owner_handle(&r.bin).await, None, "the owner's own handle is not sam's");
    let sam = |rows: Value| rows["rows"].as_array().unwrap().iter().find(|p| p["id"] == "sam").cloned().unwrap();
    assert_eq!(sam(core.ask("people.listPeople", json!({})).unwrap())["kith"], "user_x");
    let e = members::locked_run(&r.bin, &["handle", "kai", "kith", "user_x"]).await.unwrap_err();
    assert!(format!("{e:#}").contains("already sam"), "{e:#}");
    members::locked_run(&r.bin, &["handle", "kai", "kith", "user_k"]).await.unwrap();
    assert_eq!(crate::remote::account::owner_handle(&r.bin).await.as_deref(), Some("user_k"));
    members::locked_run(&r.bin, &["handle", "sam", "kith", "--rm"]).await.unwrap();
    assert_eq!(sam(core.ask("people.listPeople", json!({})).unwrap())["kith"], "", "unbound reads as no account");
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
    // An auth.json that exists but can't be read is neither: no access, and never the core (M1, 2026-11-06).
    std::fs::remove_file(&auth).unwrap();
    std::fs::create_dir(&auth).unwrap();
    assert!(!super::authority::switched() && !super::authority::switched_now());
    assert!(!super::authority::guard_on_core(), "an unreadable file is not handed to the core");
    assert!(matches!(super::authority::Selected::now(), super::authority::Selected::Closed));
    std::fs::remove_dir(&auth).unwrap();
    assert!(!super::authority::switched(), "missing is the unswitched rules");
}

#[tokio::test]
async fn a_revoke_just_after_a_rollback_reaches_the_file_the_host_is_back_on() {
    let r = Rig::new().await;
    joined(&r).await;
    let core = Core::start(&r);
    core.on();
    core.switched();
    // The cached read still says switched (the 2 s window after `kith rollback authority`), the file is plain again.
    *crate::api::authority::TEST_STALE_SWITCHED.lock().unwrap() = Some(true);
    let auth = crate::api::authority::TEST_AUTH_FILE.lock().unwrap().clone().unwrap();
    std::fs::remove_file(&auth).unwrap();
    std::fs::write(&auth, r#"{"people": {}}"#).unwrap();
    assert!(super::authority::switched() && !super::authority::switched_now(), "reads are stale, writes are not");
    assert!(r.allowed("sam", "house"));
    super::authority::run_auth(&r.bin, &["member", "house", "sam", "--rm"]).await.unwrap();
    assert!(!r.allowed("sam", "house"), "kith-auth took the revoke");
    // A second write in quick succession goes the same way.
    super::authority::run_auth_noted(&r.bin, &["member", "house", "sam", "--rm"]).await.unwrap();
}

#[tokio::test]
async fn a_write_just_after_the_switch_goes_to_the_core_not_to_kith_auth() {
    let r = Rig::new().await;
    joined(&r).await;
    let core = Core::start(&r);
    core.on();
    *crate::api::authority::TEST_STALE_SWITCHED.lock().unwrap() = Some(false);
    core.switched();
    assert!(!super::authority::switched() && super::authority::switched_now());
    super::authority::run_auth(&r.bin, &["member", "house", "sam", "preset=guest"]).await.unwrap();
    assert_eq!(core.rows(), [("house".to_owned(), "guest".to_owned())]);
}

#[tokio::test]
async fn the_core_refuses_a_switched_route_write_once_the_phase_is_not_switched() {
    let r = Rig::new().await;
    joined(&r).await;
    let core = Core::start(&r);
    core.on();
    let marked = |preset: &str| json!({"person": "sam", "agent": "house", "preset": preset, "viaSwitch": true});
    // No record at all (rolled back and cleared): refused, nothing written.
    assert!(core.ask("people.grant", marked("guest")).is_none());
    assert_eq!(core.rows().len(), 0);
    core.phase("switched");
    assert!(core.ask("people.grant", marked("guest")).is_some());
    assert_eq!(core.rows(), [("house".to_owned(), "guest".to_owned())]);
    // A stuck rollback keeps the tombstone and the core is still the store: the write lands.
    core.phase("rolling-back");
    assert!(core.ask("people.grant", marked("member")).is_some(), "RollingBack");
    assert_eq!(core.rows(), [("house".to_owned(), "member".to_owned())]);
    // Any other phase refuses it.
    core.phase("stepping");
    assert!(core.ask("people.grant", marked("guest")).is_none(), "Stepping");
    assert_eq!(core.rows(), [("house".to_owned(), "member".to_owned())]);
    core.phase("switched");
    assert!(core.ask("people.grant", marked("guest")).is_some());
    assert_eq!(core.rows(), [("house".to_owned(), "guest".to_owned())]);
    // Unmarked (kith-auth's mirror, the terminal) is as it was.
    let plain = json!({"person": "sam", "agent": "house", "preset": "member"});
    assert!(core.ask("people.grant", plain).is_some());
    assert_eq!(core.rows(), [("house".to_owned(), "member".to_owned())]);
}

#[tokio::test]
async fn the_host_falls_back_to_kith_auth_when_the_core_says_the_rollback_got_there_first() {
    let r = Rig::new().await;
    joined(&r).await;
    let core = Core::start(&r);
    core.on();
    core.switched();
    // The host's look at the file saw the tombstone; the rollback then finished (a stale phase left behind: no
    // record; the file is not back yet in this test, as in the race: kith-auth is the rig's own file).
    core.clear_phase();
    assert!(r.allowed("sam", "house"));
    super::authority::run_auth(&r.bin, &["member", "house", "sam", "--rm"]).await.unwrap();
    assert!(!r.allowed("sam", "house"), "the plain file got the revoke");
}

#[tokio::test]
async fn a_removal_during_a_stuck_rollback_is_done_by_the_core_and_repeats_harmlessly() {
    let r = Rig::new().await;
    joined(&r).await;
    let core = Core::start(&r);
    core.on();
    core.switched();
    super::authority::run_auth(&r.bin, &["member", "house", "sam", "preset=guest"]).await.unwrap();
    assert_eq!(core.rows(), [("house".to_owned(), "guest".to_owned())]);
    // The rollback failed half-way: RollingBack and the tombstone still in auth.json.
    core.phase("rolling-back");
    super::authority::run_auth(&r.bin, &["member", "house", "sam", "--rm"]).await.unwrap();
    assert!(core.rows().is_empty(), "the core took the removal (a kith-auth run would leave the core's row)");
    // A retry changes nothing and still answers.
    super::authority::run_auth(&r.bin, &["member", "house", "sam", "--rm"]).await.unwrap();
    assert_eq!(core.rows().len(), 0);
}

#[tokio::test]
async fn a_suspend_is_refused_by_the_core_when_not_switched_and_the_host_falls_back() {
    let r = Rig::new().await;
    joined(&r).await;
    let core = Core::start(&r);
    core.on();
    core.switched();
    core.clear_phase();
    // The core on its own refuses the marked suspend.
    assert!(core.ask("people.suspend", json!({"id": "sam", "viaSwitch": true})).is_none());
    assert!(r.allowed("sam", "house"));
    // The host takes the refusal as "the rollback got there first" and runs kith-auth.
    super::authority::run_auth(&r.bin, &["person", "sam", "--revoke", "--local"]).await.unwrap();
    assert!(!r.allowed("sam", "house"), "kith-auth got the suspend");
}

#[tokio::test]
async fn a_write_decides_from_a_fresh_stat_not_the_read_cache() {
    let r = Rig::new().await;
    joined(&r).await;
    let core = Core::start(&r);
    core.on();
    // Right after a switch the read cache still says "not switched": the write goes to the core, not to kith-auth.
    *crate::api::authority::TEST_STALE_SWITCHED.lock().unwrap() = Some(false);
    core.switched();
    assert!(!super::authority::switched());
    super::authority::run_auth_noted(&r.bin, &["member", "house", "sam", "preset=guest"]).await.unwrap();
    assert_eq!(core.rows(), [("house".to_owned(), "guest".to_owned())]);
    // run_auth decides the same way: stale-false on a tombstoned host, the write still goes to the core (a kith-auth
    // run on the tombstone would fail, and would leave the core's row).
    super::authority::run_auth(&r.bin, &["member", "house", "sam", "--rm"]).await.unwrap();
    assert!(core.rows().is_empty(), "run_auth wrote to the core");
    super::authority::run_auth(&r.bin, &["member", "house", "sam", "preset=guest"]).await.unwrap();
    assert_eq!(core.rows(), [("house".to_owned(), "guest".to_owned())]);
    // Right after a rollback the cache still says "switched" but the file is back: the write goes to kith-auth.
    let auth = core.tmp.path().join("auth.json");
    std::fs::write(&auth, r#"{"people": {}}"#).unwrap();
    *crate::api::authority::TEST_STALE_SWITCHED.lock().unwrap() = Some(true);
    assert!(super::authority::switched() && !super::authority::switched_now());
    super::authority::run_auth_noted(&r.bin, &["member", "house", "sam", "--rm"]).await.unwrap();
    assert_eq!(core.rows(), [("house".to_owned(), "guest".to_owned())], "the core was not asked");
    *crate::api::authority::TEST_STALE_SWITCHED.lock().unwrap() = None;
}

#[tokio::test]
async fn a_failed_revoke_while_unswitched_still_cuts_the_person_off() {
    let r = Rig::new().await;
    joined(&r).await;
    let nowhere = r.dir.join("no-such-kith-auth");
    assert!(super::people::local_revoke(&r.hub, &nowhere, "sam", true).await.is_err());
    assert!(r.hub.store.suspended_people().unwrap().contains(&"sam".to_owned()), "cut_off ran");
}

// MARK: S3: a marked read is refused by the core unless it is switched

#[tokio::test]
async fn the_core_refuses_a_marked_read_unless_the_phase_is_switched_or_rolling_back() {
    let r = Rig::new().await;
    joined(&r).await;
    let core = Core::start(&r);
    core.on();
    let reads: [(&str, Value); 7] = [
        ("people.may", json!({"person": "sam", "agent": "house"})),
        ("people.listPeople", json!({})),
        ("people.list", json!({"person": "sam"})),
        ("people.invites", json!({})),
        ("people.retired", json!({"id": "sam"})),
        ("people.budgets", json!({"agent": "house"})),
        ("people.ledger", json!({})),
    ];
    let marked = |p: &Value| {
        let mut p = p.clone();
        p["viaSwitch"] = true.into();
        p
    };
    // No record (rolled back and cleared): every marked read is refused; the unmarked ones are as they were.
    for (m, p) in &reads {
        assert!(core.ask(m, p.clone()).is_some(), "{m}: unmarked");
        assert!(core.ask(m, marked(p)).is_none(), "{m}: no record");
    }
    for state in ["switched", "rolling-back"] {
        core.phase(state);
        for (m, p) in &reads {
            assert!(core.ask(m, marked(p)).is_some(), "{m}: {state}");
        }
    }
    core.phase("stepping");
    for (m, p) in &reads {
        assert!(core.ask(m, marked(p)).is_none(), "{m}: stepping");
    }
    // A record nobody can read: a read is refused (a write that only removes access still goes through).
    core.phase("switched");
    let record = core.tmp.path().join("machine/core/cutover/authority.switch");
    std::fs::write(&record, b"\xff\xfe not a record").unwrap();
    for (m, p) in &reads {
        assert!(core.ask(m, marked(p)).is_none(), "{m}: damaged");
    }
    assert!(
        core.ask("people.suspend", json!({"id": "sam", "viaSwitch": true})).is_some(),
        "a removal still goes through"
    );
}

#[tokio::test]
async fn after_a_rollback_a_stale_switched_cache_does_not_let_the_guard_read_the_core() {
    use super::authority::Authority;
    let r = Rig::new().await;
    joined(&r).await;
    let core = Core::start(&r);
    sam_only_in_the_core(&r, &core);
    core.on();
    core.switched();
    assert!(roles::may_use("sam", "house").await, "switched: the core's row answers");
    // The rollback finished (record cleared, file plain) inside the cache's 2 s: it still says switched. Sam is then
    // revoked in the file; the core, no longer the store, never heard of it.
    core.clear_phase();
    let auth = core.tmp.path().join("auth.json");
    std::fs::write(&auth, r#"{"people": {}}"#).unwrap();
    *crate::api::authority::TEST_STALE_SWITCHED.lock().unwrap() = Some(true);
    assert!(super::authority::switched() && !super::authority::switched_now());
    super::authority::run_auth(&r.bin, &["member", "house", "sam", "--rm"]).await.unwrap();
    assert!(!r.allowed("sam", "house"), "the file revoked Sam");
    assert!(core.rows().iter().any(|(a, _)| a == "house"), "the core still grants");
    // The core refuses the marked read, and the guard answers from the file.
    assert_eq!(super::authority::core_heard("sam", "house").await, super::authority::Heard::NotSwitched);
    assert!(!roles::may_use("sam", "house").await, "the file's revoke wins over a stale cache");
    // The events' seam fails closed on the refusal instead of answering from the core.
    assert!(!core.data.granted("sam", "house"));
}

#[tokio::test]
async fn a_stale_unswitched_cache_reads_unmarked_and_the_core_answers() {
    let r = Rig::new().await;
    joined(&r).await;
    let core = Core::start(&r);
    sam_only_in_the_core(&r, &core);
    core.on();
    core.switched();
    // Right after the switch the cache still says "not switched": the read carries no marker and the core, the only
    // store now, answers.
    *crate::api::authority::TEST_STALE_SWITCHED.lock().unwrap() = Some(false);
    assert!(roles::may_use("sam", "house").await);
}

// MARK: S4: a switched suspend or removal ends the person's live core claims

fn exits(child: &mut std::process::Child) -> bool {
    for _ in 0..100 {
        if child.try_wait().unwrap().is_some() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}

#[test]
fn a_claimed_group_leaders_orphaned_child_that_ignores_sigterm_goes_with_it() {
    use std::os::unix::process::CommandExt as _;
    let file = std::env::temp_dir().join(format!("kith-orphan-{}", std::process::id()));
    let _ = std::fs::remove_file(&file);
    // The subshell leaves its sleeper (SIGTERM ignored) to the init process: not a descendant of the claimer, but in its group.
    let script = format!("(trap '' TERM; sleep 60 & echo $! > '{}'); exec sleep 60", file.display());
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c").arg(script);
    // SAFETY: setsid is async-signal-safe and the closure does nothing else.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        })
    };
    let mut leader = cmd.spawn().unwrap();
    let orphan = loop {
        if let Some(pid) = std::fs::read_to_string(&file).ok().and_then(|t| t.trim().parse::<i32>().ok()) {
            break pid;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    std::thread::sleep(std::time::Duration::from_millis(200));
    let started = kith_kernel::procstart::start_stamp(leader.id()).expect("the leader's start");
    crate::terminal::claim::end_claimed(&[(leader.id(), Some(started))], std::time::Duration::from_millis(300));
    let _ = leader.wait();
    let gone = (0..100).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(50));
        // SAFETY: signal 0 only tests for existence.
        unsafe { libc::kill(orphan, 0) != 0 }
    });
    let _ = std::fs::remove_file(&file);
    if !gone {
        // SAFETY: cleanup of the test's own sleeper.
        unsafe { libc::kill(orphan, libc::SIGKILL) };
    }
    assert!(gone, "the orphaned child of a group leader is gone");
}

/// A stamp of the same form as `s`, for a process started long after it.
fn later_stamp(s: &str) -> String {
    if let Some((boot, ticks)) = s.strip_prefix('b').and_then(|r| r.split_once(':')) {
        return format!("b{boot}:{}", ticks.parse::<u64>().unwrap() + 100_000);
    }
    let ms: i64 = s.strip_prefix('e').expect("an e or b stamp").parse().unwrap();
    format!("e{}", ms + 1_000_000)
}

/// The `kill` entry the core answers for a claim of a process (`people.suspend` / `people.removePerson`).
fn kill_entry(pid: u32, started: &str) -> Value {
    json!({"claimId": 1, "pid": pid, "started": started})
}

#[tokio::test]
async fn the_kill_list_of_a_suspend_or_removal_ends_the_claimed_process_once() {
    let mut child = std::process::Command::new("sleep").arg("60").spawn().unwrap();
    let started = kith_kernel::procstart::start_stamp(child.id()).expect("the sleeper's start");
    // A claim whose recorded start is another process's: a reused pid, never signalled. Made from the sleeper's own
    // stamp, moved far off: two real processes started this close can share a stamp (Linux counts 10 ms ticks).
    let other = later_stamp(&started);
    let said = json!({"lines": [], "kill": [kill_entry(child.id(), &other)]});
    assert_eq!(super::authority::finish_revoke(&said, "sam").await, "");
    assert!(child.try_wait().unwrap().is_none(), "a pid with another start stamp is left alone");
    // The recorded process: gone.
    let said = json!({"lines": [], "kill": [kill_entry(child.id(), &started)]});
    assert_eq!(super::authority::finish_revoke(&said, "sam").await, "");
    assert!(exits(&mut child), "the revoked person's process is gone");
    // A retry: the pid is gone (reaped), nothing is signalled and nothing fails.
    assert_eq!(super::authority::finish_revoke(&said, "sam").await, "");
}

#[tokio::test]
async fn a_kill_with_no_recorded_start_is_never_signalled() {
    let mut child = std::process::Command::new("sleep").arg("60").spawn().unwrap();
    for started in [None, Some(String::new()), Some("  ".to_owned())] {
        let said = json!({"kill": [{"claimId": 1, "pid": child.id(), "started": started}]});
        assert_eq!(super::authority::finish_revoke(&said, "sam").await, "");
        assert!(child.try_wait().unwrap().is_none(), "a pid alone is not the claimer: {started:?}");
    }
    child.kill().unwrap();
    child.wait().unwrap();
}

#[tokio::test]
async fn a_switched_suspend_through_the_host_reports_nothing_pending_when_the_order_finished() {
    let r = Rig::new().await;
    joined(&r).await;
    let core = Core::start(&r);
    core.on();
    core.switched();
    let (core_updated, pending) = super::people::local_revoke(&r.hub, &r.bin, "sam", true).await.unwrap();
    assert!(core_updated && pending.is_none(), "{pending:?}");
    let (_, pending) = super::people::local_revoke(&r.hub, &r.bin, "sam", true).await.unwrap();
    assert!(pending.is_none(), "a retried suspend is harmless");
    // The core's answer is what `finish_revoke` reads: `kill` is a list of {claimId, pid, started}.
    let said = core.ask("people.suspend", json!({"id": "sam", "viaSwitch": true})).unwrap();
    assert!(said.get("kill").is_none_or(Value::is_array), "{said}");
}

#[tokio::test]
async fn a_revoke_the_core_could_not_finish_is_told_to_the_owner() {
    let said = json!({"lines": [], "revokePending": "claims: the database is locked"});
    let line = super::authority::finish_revoke(&said, "sam").await;
    assert!(
        line.contains("sam") && line.contains("part of the revoke is pending") && line.contains("database is locked"),
        "{line}"
    );
    assert_eq!(super::authority::finish_revoke(&json!({"lines": []}), "sam").await, "");
    // A kill entry with no pid, or for a pid that is not running, is left alone.
    let gone = json!({"kill": [kill_entry(999_999, "e1"), {"claimId": 2, "pid": null}]});
    assert_eq!(super::authority::finish_revoke(&gone, "sam").await, "");
}

/// C-b: once authority is on the core, auth.json is a tombstone whose change time never moves, so the cards watch the
/// core's revision. A write made in the core (never through kith-auth) re-derives who answers each open card on the next
/// poll, and a poll that sees nothing new does not.
#[tokio::test]
async fn a_core_side_write_changes_who_answers_a_card_within_one_poll() {
    let r = Rig::new().await;
    let mut core = Core::start(&r);
    core.on();
    core.switched();
    core.ask("people.grant", json!({"person": "sam", "agent": "house", "preset": "member", "viaSwitch": true}))
        .expect("grant");
    let new = crate::store::NewCard {
        session: "s-poll".into(),
        source: "acp",
        bot_id: None,
        run_id: None,
        person: "sam".into(),
        agent: "house".into(),
        answerable_by: Some("sam".into()),
        in_grants: true,
        tool: "WebFetch".into(),
        input: "{}".into(),
        resource: "k-poll".into(),
        shown: String::new(),
        deadline: i64::MAX,
        reply_to: None,
        asked_by: None,
    };
    let (crate::store::Opened::New(row) | crate::store::Opened::Joined(row) | crate::store::Opened::Granted(row)) =
        r.hub.store.card_open(&new, 1).unwrap();
    let answerable = || r.hub.store.card(&row.id).unwrap().answerable_by;
    let mut seen = crate::cards::authority_stamp().await;
    assert!(
        matches!(seen, Some(crate::cards::AuthStamp::Core(_))),
        "a tombstone is watched through the core: {seen:?}"
    );
    assert!(!crate::cards::poll_authority(&r.hub, &mut seen).await, "nothing changed: nothing re-derived");
    // an always written in the core moves the revision
    core.ask(
        "people.access",
        json!({"person": "sam", "agent": "house", "kind": "domain", "value": "docs.example.com", "viaSwitch": true}),
    )
    .expect("always");
    assert!(crate::cards::poll_authority(&r.hub, &mut seen).await, "the next poll sees the write");
    assert_eq!(answerable().as_deref(), Some("sam"), "a member still answers");
    // sam is made a guest in the core: within one poll the card is the owner's alone
    core.ask("people.grant", json!({"person": "sam", "agent": "house", "preset": "guest", "viaSwitch": true}))
        .expect("demote");
    assert!(crate::cards::poll_authority(&r.hub, &mut seen).await);
    assert_eq!(answerable(), None, "the owner alone answers now");
    assert!(!crate::cards::poll_authority(&r.hub, &mut seen).await);
    // the core down: this tick learns nothing, and re-derives nothing
    core.stop();
    assert_eq!(crate::cards::authority_stamp().await, None);
    assert!(!crate::cards::poll_authority(&r.hub, &mut seen).await);
}
