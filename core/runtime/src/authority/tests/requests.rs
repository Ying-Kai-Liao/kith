//! The access requests (M2-3) against core.db: the states and the repeats by name, two devices answering at once on
//! connections of their own, and a property whose oracle is a table of what each preset and mode means written out here,
//! not the rules the code under test asks.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use rusqlite::Connection;
use serde_json::{Value, json};

use super::{ctx, machine};
use crate::authority::call::call_with;
use crate::authority::load::load;
use crate::authority::requests::{self, NewRequest, decide_on, status_of};
use crate::authority::{Authority, store};
use crate::db::Core;
use crate::error::Error;
use crate::layout::Layout;
use kith_kernel::secrets::Ctx;

struct Rig {
    _core_dir: tempfile::TempDir,
    machine_dir: tempfile::TempDir,
    layout: Layout,
    core: Core,
    c: Ctx,
}

fn base() -> Value {
    json!({
        "people": {
            "kai": {"owner": true},
            "sam": {"name": "Sam"},
            "amy": {},
            "bot-a": {"bot_of": "sam"},
        },
        "presets": {"pro": {"mode": "acceptEdits", "deny": ["Bash"]}},
    })
}

impl Rig {
    fn new(auth: &Value) -> Rig {
        let (core_dir, m) = crate::tests::machine();
        let layout = Layout::new(&m);
        let core = crate::tests::open_all(&layout);
        load(core.conn(), auth);
        let (machine_dir, c) = machine();
        Rig {
            _core_dir: core_dir,
            machine_dir,
            layout,
            core,
            c,
        }
    }

    fn reset(&self, auth: &Value) {
        for t in [
            "people",
            "memberships",
            "presets",
            "authority_settings",
            "invites",
            "retired",
            "access_requests",
        ] {
            self.core
                .conn()
                .execute(&format!("DELETE FROM {t}"), [])
                .unwrap();
        }
        load(self.core.conn(), auth);
    }

    fn file(
        &mut self,
        id: &str,
        person: &str,
        agents: &[&str],
        preset: Option<&str>,
        mode: Option<&str>,
    ) -> Result<Value, Error> {
        requests::create(
            &mut self.core,
            &self.c,
            &NewRequest {
                id,
                person,
                agents,
                preset,
                mode,
                via: Some("invite:ABC234"),
                ttl_ms: 60_000,
            },
        )
    }

    fn call(&mut self, method: &str, params: &Value) -> Result<Value, Error> {
        call_with(&mut self.core, &self.c, method, params)
    }

    fn approve(&mut self, id: &str) -> Result<Value, Error> {
        self.call("people.approve", &json!({"id": id}))
    }

    fn deny(&mut self, id: &str) -> Result<Value, Error> {
        self.call("people.deny", &json!({"id": id}))
    }

    fn status(&self, id: &str) -> Option<String> {
        status_of(self.core.conn(), id)
    }

    fn tables(&mut self) -> Value {
        store::read(&mut self.core, Value::clone).unwrap()
    }

    fn resolve(&self, agent: &str, person: &str) -> Value {
        let a = Authority::new(self.core.conn(), &self.c);
        match a.resolve(agent, person, None, false) {
            Ok(m) => Value::Object(m),
            Err(e) => json!({"refused": e.to_string()}),
        }
    }
}

fn refused(e: &Error) -> String {
    match e {
        Error::Refused(m) => m.clone(),
        other => panic!("expected a refusal of the rules, got {other:?}"),
    }
}

#[test]
fn approving_gives_the_agents_preset_and_mode_that_were_asked() {
    let mut rig = Rig::new(&base());
    rig.file("r1", "sam", &["house", "garden"], Some("pro"), None)
        .unwrap();
    let said = rig.approve("r1").unwrap();
    assert_eq!(said["status"], "approved");
    assert_eq!(rig.status("r1").as_deref(), Some("approved"));
    for agent in ["house", "garden"] {
        let r = rig.resolve(agent, "sam");
        assert_eq!(r["allowed"], true, "{agent}");
        assert_eq!(r["preset"], "pro");
        assert_eq!(r["mode"], "acceptEdits");
        assert_eq!(r["deny"], json!(["Bash"]));
    }
    assert_eq!(rig.resolve("sound", "sam")["allowed"], false, "not asked");
    assert_eq!(
        rig.resolve("house", "amy")["allowed"],
        false,
        "someone else"
    );
}

#[test]
fn a_request_for_nothing_in_particular_is_a_plain_guest() {
    let mut rig = Rig::new(&base());
    rig.file("r1", "sam", &["house"], None, None).unwrap();
    rig.approve("r1").unwrap();
    let r = rig.resolve("house", "sam");
    assert_eq!(
        (r["preset"].as_str(), r["mode"].as_str()),
        (Some("guest"), Some("default"))
    );
}

#[test]
fn a_free_shell_needs_the_owners_trusted_and_a_refusal_leaves_the_request_waiting() {
    let mut rig = Rig::new(&base());
    rig.file(
        "r1",
        "sam",
        &["house"],
        Some("member"),
        Some("bypassPermissions"),
    )
    .unwrap();
    let e = rig.approve("r1").unwrap_err();
    assert!(matches!(e, Error::BadParams(_)), "{e:?}");
    assert_eq!(rig.status("r1").as_deref(), Some("pending"));
    assert_eq!(rig.resolve("house", "sam")["allowed"], false);
    rig.call("people.approve", &json!({"id": "r1", "trusted": true}))
        .unwrap();
    assert_eq!(rig.resolve("house", "sam")["mode"], "bypassPermissions");
}

#[test]
fn a_request_is_decided_once() {
    let mut rig = Rig::new(&base());
    rig.file("a", "sam", &["house"], Some("member"), None)
        .unwrap();
    rig.file("d", "sam", &["garden"], None, None).unwrap();

    rig.approve("a").unwrap();
    let after = rig.tables();
    assert!(refused(&rig.approve("a").unwrap_err()).contains("already approved"));
    assert!(refused(&rig.deny("a").unwrap_err()).contains("already approved"));
    assert_eq!(rig.tables(), after, "a repeat writes nothing");
    assert_eq!(rig.status("a").as_deref(), Some("approved"));

    rig.deny("d").unwrap();
    assert!(refused(&rig.deny("d").unwrap_err()).contains("already denied"));
    assert!(refused(&rig.approve("d").unwrap_err()).contains("already denied"));
    assert_eq!(rig.status("d").as_deref(), Some("denied"));
    assert_eq!(rig.resolve("garden", "sam")["allowed"], false);
    let row: Option<i64> = rig
        .core
        .conn()
        .query_row(
            "SELECT decided_at FROM access_requests WHERE id = 'd'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(row.is_some(), "the decision is recorded");
}

#[test]
fn an_unknown_request_is_a_refusal_and_the_id_is_required() {
    let mut rig = Rig::new(&base());
    assert!(refused(&rig.approve("nope").unwrap_err()).contains("no access request 'nope'"));
    assert!(refused(&rig.deny("nope").unwrap_err()).contains("no access request"));
    assert!(matches!(
        rig.call("people.approve", &json!({})).unwrap_err(),
        Error::BadParams(_)
    ));
    assert!(matches!(
        rig.call("people.deny", &json!({})).unwrap_err(),
        Error::BadParams(_)
    ));
}

#[test]
fn a_revoked_person_gets_nothing_and_the_request_is_denied() {
    let mut rig = Rig::new(&base());
    rig.file("r1", "sam", &["house"], Some("member"), None)
        .unwrap();
    rig.core
        .conn()
        .execute(
            "UPDATE people SET revoked = 1, revoked_at = 5 WHERE id = 'sam'",
            [],
        )
        .unwrap();
    let e = refused(&rig.approve("r1").unwrap_err());
    assert!(e.contains("access was removed"), "{e}");
    assert_eq!(
        rig.status("r1").as_deref(),
        Some("denied"),
        "recorded, as the host denies it"
    );
    assert_eq!(rig.tables()["members"], json!({}));
    // and one can't be filed for them
    assert!(
        refused(&rig.file("r2", "sam", &["house"], None, None).unwrap_err()).contains("removed")
    );
}

#[test]
fn a_bot_of_a_revoked_person_is_cut_off_too() {
    let mut rig = Rig::new(&base());
    rig.file("r1", "bot-a", &["house"], None, None).unwrap();
    rig.core
        .conn()
        .execute("UPDATE people SET revoked = 1 WHERE id = 'sam'", [])
        .unwrap();
    assert!(refused(&rig.approve("r1").unwrap_err()).contains("removed"));
    assert_eq!(rig.status("r1").as_deref(), Some("denied"));
}

#[test]
fn a_person_who_was_removed_gets_nothing_and_the_request_is_denied() {
    let mut rig = Rig::new(&base());
    rig.file("r1", "amy", &["house"], None, None).unwrap();
    rig.core
        .conn()
        .execute("DELETE FROM people WHERE id = 'amy'", [])
        .unwrap();
    assert!(refused(&rig.approve("r1").unwrap_err()).contains("no longer a person"));
    assert_eq!(rig.status("r1").as_deref(), Some("denied"));
    assert_eq!(rig.tables()["members"], json!({}));
}

#[test]
fn an_expired_request_is_refused_and_no_longer_waiting() {
    let mut rig = Rig::new(&base());
    rig.file("r1", "sam", &["house"], None, None).unwrap();
    rig.file("r2", "sam", &["garden"], None, None).unwrap();
    assert_eq!(
        rig.call("people.requests", &json!({})).unwrap()["rows"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    rig.c.now_ms += 61_000.0;
    assert!(refused(&rig.approve("r1").unwrap_err()).contains("expired"));
    assert!(refused(&rig.deny("r2").unwrap_err()).contains("expired"));
    assert_eq!(
        rig.status("r1").as_deref(),
        Some("pending"),
        "nothing is stored for it"
    );
    assert_eq!(rig.tables()["members"], json!({}));
    assert_eq!(
        rig.call("people.requests", &json!({})).unwrap()["rows"],
        json!([])
    );
    let all = rig.call("people.requests", &json!({"all": true})).unwrap();
    assert_eq!(all["rows"][0]["status"], "expired");
}

#[test]
fn an_agent_or_a_preset_that_is_gone_refuses_the_whole_approval() {
    let mut rig = Rig::new(&base());
    // one good agent and one that does not exist: neither is given
    rig.file("r1", "sam", &["house", "vanished"], None, None)
        .unwrap();
    assert!(refused(&rig.approve("r1").unwrap_err()).contains("no agent 'vanished'"));
    assert_eq!(rig.tables()["members"], json!({}));
    assert_eq!(rig.status("r1").as_deref(), Some("pending"));
    // a preset the owner removed while it waited
    rig.file("r2", "sam", &["house"], Some("pro"), None)
        .unwrap();
    rig.call("people.removePreset", &json!({"name": "pro"}))
        .unwrap();
    assert!(refused(&rig.approve("r2").unwrap_err()).contains("no preset 'pro'"));
    assert_eq!(rig.resolve("house", "sam")["allowed"], false);
    assert_eq!(rig.status("r2").as_deref(), Some("pending"));
    // a built-in agent can't be given to anyone
    rig.file("r3", "sam", &["scratch"], None, None).unwrap();
    assert!(matches!(
        rig.approve("r3").unwrap_err(),
        Error::BadParams(_)
    ));
}

#[test]
fn a_bot_may_only_ask_for_what_its_owner_holds() {
    let mut rig = Rig::new(&base());
    rig.file("r1", "bot-a", &["house"], None, None).unwrap();
    // sam does not hold house: the row would wait, the access is not there, so nothing is approved
    let e = refused(&rig.approve("r1").unwrap_err());
    assert!(e.contains("may not use house"), "{e}");
    assert_eq!(rig.status("r1").as_deref(), Some("pending"));
    assert_eq!(rig.tables()["members"], json!({}));
    // a bot's preset is guest
    rig.file("r2", "bot-a", &["house"], Some("member"), None)
        .unwrap();
    assert!(matches!(
        rig.approve("r2").unwrap_err(),
        Error::BadParams(_)
    ));
}

#[test]
fn only_a_person_kith_knows_can_ask_and_for_agents_one_at_a_time() {
    let mut rig = Rig::new(&base());
    assert!(
        refused(&rig.file("r", "ghost", &["house"], None, None).unwrap_err()).contains("no person")
    );
    for (person, agents) in [("*", vec!["house"]), ("sam", vec![]), ("sam", vec!["a,b"])] {
        let e = rig.file("r", person, &agents, None, None).unwrap_err();
        assert!(
            matches!(e, Error::BadParams(_)),
            "{person} {agents:?}: {e:?}"
        );
    }
    rig.file("r", "sam", &["house"], None, None).unwrap();
    assert!(
        rig.file("r", "sam", &["house"], None, None).is_err(),
        "an id is used once"
    );
}

#[test]
fn the_list_shows_what_was_asked() {
    let mut rig = Rig::new(&base());
    rig.file("r1", "sam", &["house", "garden"], Some("pro"), Some("plan"))
        .unwrap();
    let rows = rig.call("people.requests", &json!({})).unwrap();
    let r = &rows["rows"][0];
    assert_eq!(r["id"], "r1");
    assert_eq!(r["person"], "sam");
    assert_eq!(r["agents"], json!(["house", "garden"]));
    assert_eq!(
        (r["preset"].as_str(), r["mode"].as_str()),
        (Some("pro"), Some("plan"))
    );
    assert_eq!(r["status"], "pending");
    assert_eq!(r["via"], "invite:ABC234");
    rig.deny("r1").unwrap();
    assert_eq!(
        rig.call("people.requests", &json!({})).unwrap()["rows"],
        json!([])
    );
    assert_eq!(
        rig.call("people.requests", &json!({"all": true})).unwrap()["rows"][0]["status"],
        "denied"
    );
}

// --- two owner devices at once ------------------------------------------------------------------------------------

fn rival(rig: &Rig) -> (Connection, Ctx) {
    let conn = Connection::open(&rig.layout.db).unwrap();
    conn.busy_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    (conn, ctx(rig.machine_dir.path()))
}

/// Two connections of their own on the same core.db decide the same request at the same moment: the first to take the
/// write lock decides, the other reads that and is refused, and the table shows the winner's decision only.
#[test]
fn two_devices_deciding_at_once_leave_one_decision() {
    let mut rig = Rig::new(&base());
    for round in 0..12 {
        let id = format!("r{round}");
        rig.file(&id, "sam", &["house", "garden"], Some("member"), None)
            .unwrap();
        let (approve, c1) = rival(&rig);
        let (other, c2) = rival(&rig);
        // approve against approve, then (odd rounds) approve against deny
        let second_approves = round % 2 == 0;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let spawn = |mut conn: Connection, c: Ctx, approves: bool| {
            let (id, barrier) = (id.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                decide_on(&mut conn, &c, &id, approves)
            })
        };
        let a = spawn(approve, c1, true);
        let b = spawn(other, c2, second_approves);
        let (a, b) = (a.join().unwrap(), b.join().unwrap());
        assert_eq!(
            u8::from(a.is_ok()) + u8::from(b.is_ok()),
            1,
            "round {round}: exactly one decides: {a:?} {b:?}"
        );
        let loser = a.err().or(b.err()).unwrap();
        assert!(refused(&loser).contains("already"), "{loser:?}");
        let status = rig.status(&id).unwrap();
        let granted = rig.resolve("house", "sam")["allowed"] == true;
        assert_eq!(
            status == "approved",
            granted,
            "round {round}: the grant follows the decision"
        );
        // reset for the next round
        rig.core
            .conn()
            .execute("DELETE FROM memberships", [])
            .unwrap();
    }
}

// --- the property -------------------------------------------------------------------------------------------------

const LIVE: [&str; 3] = ["house", "garden", "sound"];
const ALL_AGENTS: [&str; 5] = ["house", "garden", "sound", "scratch", "vanished"];

#[derive(Debug, Clone)]
struct Case {
    agents: Vec<&'static str>,
    preset: Option<&'static str>,
    mode: Option<&'static str>,
    trusted: bool,
    /// What sam already had: nothing, or a member row with a deny of its own, on these agents.
    before: Vec<&'static str>,
    person: &'static str,
}

fn case() -> impl Strategy<Value = Case> {
    (
        proptest::sample::subsequence(ALL_AGENTS.to_vec(), 1..=4),
        proptest::sample::select(vec![
            None,
            Some("guest"),
            Some("member"),
            Some("pro"),
            Some("gone"),
        ]),
        proptest::sample::select(vec![
            None,
            Some("default"),
            Some("plan"),
            Some("acceptEdits"),
            Some("bypassPermissions"),
            Some("auto"),
        ]),
        any::<bool>(),
        proptest::sample::subsequence(LIVE.to_vec(), 0..=3),
        proptest::sample::select(vec!["sam", "amy"]),
    )
        .prop_map(|(agents, preset, mode, trusted, before, person)| Case {
            agents,
            preset,
            mode,
            trusted,
            before,
            person,
        })
}

/// What a preset means, written out: its mode and the tools it denies.
fn preset_means(name: &str) -> (&'static str, Vec<&'static str>) {
    match name {
        "guest" => ("default", vec![]),
        "member" => ("acceptEdits", vec![]),
        "pro" => ("acceptEdits", vec!["Bash"]),
        other => panic!("{other}"),
    }
}

/// Whether the approval should go through, by the rules of `kith-auth member` as the owner knows them.
fn should_succeed(c: &Case) -> bool {
    let agents_ok = c.agents.iter().all(|a| LIVE.contains(a));
    let preset_ok = c.preset.is_none_or(|p| p != "gone");
    let free_shell = c
        .mode
        .is_some_and(|m| m == "bypassPermissions" || m == "auto");
    agents_ok && preset_ok && (!free_shell || c.trusted)
}

fn view(rig: &Rig) -> Value {
    let mut out = serde_json::Map::new();
    for person in ["kai", "sam", "amy", "bot-a"] {
        for agent in ALL_AGENTS {
            let r = rig.resolve(agent, person);
            let mut deny: Vec<String> = r["deny"]
                .as_array()
                .map(|d| d.iter().map(|x| x.as_str().unwrap().to_owned()).collect())
                .unwrap_or_default();
            deny.sort();
            out.insert(
                format!("{person}/{agent}"),
                json!({"allowed": r["allowed"], "preset": r["preset"], "mode": r["mode"], "deny": deny, "refused": r["refused"]}),
            );
        }
    }
    Value::Object(out)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// Approving gives each requested agent exactly the requested preset and mode, and leaves every other person and
    /// every other agent as it was; an approval that is refused changes nothing at all and the request still waits.
    #[test]
    fn approving_gives_exactly_the_request_and_nothing_more(c in case()) {
        thread_local! {
            static RIG: std::cell::RefCell<Option<Rig>> = const { std::cell::RefCell::new(None) };
        }
        RIG.with(|cell| -> Result<(), TestCaseError> {
            let mut slot = cell.borrow_mut();
            let rig = slot.get_or_insert_with(|| Rig::new(&base()));
            rig.reset(&base());
            for a in &c.before {
                rig.core.conn().execute(
                    "INSERT INTO memberships (agent, person, preset, deny) VALUES (?1, 'sam', 'member', '[\"Edit\"]')",
                    [a],
                ).unwrap();
            }
            rig.file("r", c.person, &c.agents, c.preset, c.mode).unwrap();
            let before = view(rig);
            let tables_before = rig.tables();
            let got = rig.call("people.approve", &json!({"id": "r", "trusted": c.trusted}));
            let after = view(rig);
            if !should_succeed(&c) {
                prop_assert!(got.is_err(), "{c:?}: {got:?}");
                prop_assert_eq!(&after, &before, "{:?}: a refusal changes nothing", c);
                prop_assert_eq!(rig.tables(), tables_before);
                let st = rig.status("r");
                prop_assert_eq!(st.as_deref(), Some("pending"));
                return Ok(());
            }
            prop_assert!(got.is_ok(), "{c:?}: {got:?}");
            let st = rig.status("r");
            prop_assert_eq!(st.as_deref(), Some("approved"));
            let preset = c.preset.unwrap_or("guest");
            let (preset_mode, preset_deny) = preset_means(preset);
            for key in before.as_object().unwrap().keys() {
                let (who, agent) = key.split_once('/').unwrap();
                if who == c.person && c.agents.contains(&agent) {
                    let kept_deny = c.person == "sam" && c.before.contains(&agent);
                    let mut want: Vec<&str> = preset_deny.clone();
                    if kept_deny { want.push("Edit"); }
                    want.sort_unstable();
                    let g = &after[key];
                    prop_assert_eq!(&g["allowed"], &json!(true), "{} {:?}", key, c);
                    prop_assert_eq!(g["preset"].as_str(), Some(preset), "{} {:?}", key, c);
                    prop_assert_eq!(g["mode"].as_str(), Some(c.mode.unwrap_or(preset_mode)), "{} {:?}", key, c);
                    prop_assert_eq!(&g["deny"], &json!(want), "{} {:?}", key, c);
                } else {
                    prop_assert_eq!(&after[key], &before[key], "{} {:?}", key, c);
                }
            }
            Ok(())
        })?;
    }
}
