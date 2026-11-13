//! The secret writers `kith-auth` routes here on the tombstone (S-b): `people.secretSet`, `secretGrant`, `secretRevoke`
//! and `secretEnv`. The byte-for-byte parity with the file is `kernel/tests/kith-auth-router-check`; these hold the
//! core's side: the rules in the one locked step, the fence of `viaSwitch` and of a damaged record, that a value is never
//! asked for or kept, and what the import and the rollback do with `helper_since`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::{Value, json};

use super::writes::Rig;
use crate::cutover::state::{self, Phase, Switch};
use crate::layout::Layout;

fn world() -> Value {
    json!({
        "people": {
            "kai": {"owner": true},
            "sam": {"name": "Sam"},
            "pat": {"name": "Pat", "revoked": {"at": 5}},
        },
        "members": {"house": {"sam": {"preset": "member"}}},
        "profiles": {
            "github/work": {"GITHUB_TOKEN": "keychain:kith/github/work#GITHUB_TOKEN"},
            "slack/shared": {"person": "*", "SLACK_TOKEN": "keychain:kith/slack/shared#SLACK_TOKEN",
                             "with": ["sam"]},
        },
        "grants": {"house": {"github": "github/work"}},
    })
}

fn spec(rig: &Rig, dir: &str, name: &str, body: &Value) {
    let d = rig.machine_dir.path().join(dir).join("services");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join(format!("{name}.json")), body.to_string()).unwrap();
}

/// The test loader keeps no profiles or grants (the import does): put the world's in as the import would.
fn seed(rig: &Rig, auth: &Value) {
    let conn = rig.core.conn();
    for (name, p) in auth["profiles"].as_object().unwrap() {
        let person = p.get("person").and_then(Value::as_str);
        let lent = p.get("with").map(Value::to_string);
        conn.execute(
            "INSERT INTO secret_profiles (profile, person, lent) VALUES (?1, ?2, ?3)",
            rusqlite::params![name, person, lent],
        )
        .unwrap();
        for (var, src) in p.as_object().unwrap() {
            if !["person", "with", "browser"].contains(&var.as_str()) {
                conn.execute(
                    "INSERT INTO secret_vars (profile, var, source) VALUES (?1, ?2, ?3)",
                    rusqlite::params![name, var, src.as_str()],
                )
                .unwrap();
            }
        }
    }
    for (agent, by_service) in auth["grants"].as_object().unwrap() {
        for (service, profile) in by_service.as_object().unwrap() {
            conn.execute(
                "INSERT INTO secret_grants (agent, service, profile, seq) VALUES (?1, ?2, ?3, 0)",
                rusqlite::params![agent, service, profile.as_str()],
            )
            .unwrap();
        }
    }
}

fn rig() -> Rig {
    let rig = Rig::new(&world());
    seed(&rig, &world());
    let layout = Layout::new(&crate::authority::revoke::Life::beside(&rig.c).machine);
    let s = Switch {
        phase: Phase::Switched,
        snapshot: None,
        at: 1,
    };
    state::write(&layout, "authority", &s).unwrap();
    spec(
        &rig,
        "kernel",
        "github",
        &json!({"secrets": ["GITHUB_TOKEN", "GITHUB_ORG"]}),
    );
    spec(
        &rig,
        "kernel",
        "slack",
        &json!({"secrets": ["SLACK_TOKEN"]}),
    );
    spec(
        &rig,
        "kernel",
        "chatgpt",
        &json!({"kind": "login", "secrets": ["SESSION"]}),
    );
    spec(
        &rig,
        "kernel",
        "trello",
        &json!({"kind": "login", "personal": false, "secrets": ["KEY"]}),
    );
    spec(&rig, "machine", "calendar", &json!({"kind": "connector"}));
    rig
}

fn set(profile: &str, vars: &[&str], grant: Option<&str>) -> Value {
    let mut p = json!({"profile": profile, "vars": vars, "viaSwitch": true, "dies": true});
    if let Some(g) = grant {
        p["grant"] = json!(g);
    }
    p
}

fn grant(agents: &[&str], service: &str, profiles: &[&str]) -> Value {
    json!({"agents": agents, "service": service, "profiles": profiles, "viaSwitch": true, "dies": true})
}

fn died(ans: &Value) -> (String, i64) {
    (
        ans["why"].as_str().unwrap().to_owned(),
        ans["died"].as_i64().unwrap(),
    )
}

fn profile(rig: &mut Rig, name: &str) -> Value {
    rig.tables()["profiles"][name].clone()
}

#[test]
fn a_set_records_references_for_the_names_and_a_grant_when_asked() {
    let mut rig = rig();
    let ans = rig
        .call(
            "people.secretSet",
            &set("github/play", &["GITHUB_TOKEN"], Some("house")),
        )
        .unwrap();
    assert_eq!(ans, json!({"ok": true}));
    let t = rig.tables();
    assert_eq!(
        t["profiles"]["github/play"],
        json!({"GITHUB_TOKEN": "keychain:kith/github/play#GITHUB_TOKEN"}),
        "a shared service's new profile has no owner"
    );
    // the grant keeps the first profile and adds this one (a list of two)
    assert_eq!(
        t["grants"]["house"]["github"],
        json!(["github/work", "github/play"])
    );
    // the same set again is the same tables
    let before = rig.tables();
    rig.call(
        "people.secretSet",
        &set("github/play", &["GITHUB_TOKEN"], Some("house")),
    )
    .unwrap();
    assert_eq!(rig.tables(), before);
}

#[test]
fn a_new_profile_of_a_personal_service_is_the_owners_and_an_old_one_keeps_what_it_has() {
    let mut rig = rig();
    rig.call("people.secretSet", &set("chatgpt/main", &["SESSION"], None))
        .unwrap();
    assert_eq!(profile(&mut rig, "chatgpt/main")["person"], "kai");
    // a login service marked `personal: false` is shared, so no owner
    rig.call("people.secretSet", &set("trello/main", &["KEY"], None))
        .unwrap();
    assert_eq!(profile(&mut rig, "trello/main").get("person"), None);
    // an existing profile keeps its `*` and its `with`
    rig.call(
        "people.secretSet",
        &set("slack/shared", &["SLACK_TOKEN"], None),
    )
    .unwrap();
    let kept = profile(&mut rig, "slack/shared");
    assert_eq!(kept["person"], "*");
    assert_eq!(kept["with"], json!(["sam"]));
    // a second variable lands beside the first
    rig.call(
        "people.secretSet",
        &set("github/work", &["GITHUB_ORG"], None),
    )
    .unwrap();
    let w = profile(&mut rig, "github/work");
    assert_eq!(w["GITHUB_ORG"], "keychain:kith/github/work#GITHUB_ORG");
    assert_eq!(w["GITHUB_TOKEN"], "keychain:kith/github/work#GITHUB_TOKEN");
}

#[test]
fn a_set_is_refused_for_a_bad_name_a_variable_the_service_does_not_declare_and_a_caller_who_is_not_the_owner()
 {
    let mut rig = rig();
    let before = rig.tables();
    for (p, why) in [
        (set("github", &["GITHUB_TOKEN"], None), "profiles are named"),
        (
            set("Github/work", &["GITHUB_TOKEN"], None),
            "profiles are named",
        ),
        (
            set("github/work", &["NOPE"], None),
            "a name is not one github declares",
        ),
        (
            set("github/work", &["GITHUB-TOKEN"], None),
            "a name is not one github declares",
        ),
        (
            set("github/work", &[], None),
            "stdin must be one JSON object",
        ),
        (
            set("github/work", &["GITHUB_TOKEN"], Some("no space")),
            "not an agent name",
        ),
        (set("nosuch/work", &["A"], None), "no service 'nosuch'"),
    ] {
        let ans = rig.call("people.secretSet", &p).unwrap();
        let (msg, code) = died(&ans);
        assert!(msg.contains(why), "{p}: {msg}");
        assert!(code == 1 || code == 2);
    }
    // a guest's MCP server runs outside the box with its own KITH_PERSON
    rig.c.person = Some("sam".to_owned());
    let ans = rig
        .call(
            "people.secretSet",
            &set("github/work", &["GITHUB_TOKEN"], Some("house")),
        )
        .unwrap();
    assert_eq!(
        died(&ans),
        ("refused: not available to this run".to_owned(), 2)
    );
    assert_eq!(rig.tables(), before, "a refusal changes nothing");
}

#[test]
fn the_writers_need_viaswitch_a_switched_core_and_refuse_a_damaged_record_except_a_removal() {
    let mut rig = rig();
    let mut p = set("github/work", &["GITHUB_TOKEN"], None);
    p.as_object_mut().unwrap().remove("viaSwitch");
    for m in [
        "people.secretSet",
        "people.secretGrant",
        "people.secretRevoke",
        "people.secretEnv",
    ] {
        let e = rig.call(m, &p).unwrap_err();
        assert!(e.contains("needs viaSwitch"), "{m}: {e}");
    }
    // not switched (no record: rolled back): everything is refused, a removal too
    let layout = Layout::new(&crate::authority::revoke::Life::beside(&rig.c).machine);
    let revoke = json!({"agent": "house", "service": "github", "viaSwitch": true});
    state::clear(&layout, "authority").unwrap();
    let set_p = set("github/play", &["GITHUB_TOKEN"], None);
    for (m, p) in [
        ("people.secretSet", &set_p),
        ("people.secretRevoke", &revoke),
    ] {
        let e = rig.call(m, p).unwrap_err();
        assert!(e.contains("not switched"), "{m}: {e}");
    }
    // a damaged record: a set and a grant are refused, a revoke goes through
    std::fs::write(state::path(&layout, "authority").unwrap(), "garbage").unwrap();
    let before = rig.tables();
    for (m, p) in [
        ("people.secretSet", set_p),
        (
            "people.secretGrant",
            grant(&["garden"], "github", &["github/work"]),
        ),
        (
            "people.secretEnv",
            json!({"agent": "house", "service": "github", "pairs": [["A", "1"]], "viaSwitch": true}),
        ),
    ] {
        let e = rig.call(m, &p).unwrap_err();
        assert!(e.contains("not switched"), "{m}: {e}");
    }
    assert_eq!(rig.tables(), before, "a refused write leaves nothing");
    rig.call("people.secretRevoke", &revoke).unwrap();
    assert!(rig.tables()["grants"].get("house").is_none());
}

#[test]
fn a_grant_checks_the_person_the_prefix_and_marks_a_host_connectors_profile() {
    let mut rig = rig();
    let ans = rig
        .call(
            "people.secretGrant",
            &grant(&["house@ghost"], "github", &["github/work"]),
        )
        .unwrap();
    assert!(died(&ans).0.contains("no person 'ghost'"));
    let ans = rig
        .call(
            "people.secretGrant",
            &grant(&["house"], "github", &["slack/x"]),
        )
        .unwrap();
    assert_eq!(
        died(&ans),
        (
            "a github grant takes github/<account> profiles".to_owned(),
            2
        )
    );
    // a known-only grant names real agents (and bridge, serve)
    let mut p = grant(&["nobody"], "github", &["github/work"]);
    p["knownOnly"] = json!(true);
    assert!(
        died(&rig.call("people.secretGrant", &p).unwrap())
            .0
            .contains("no agent 'nobody'")
    );
    p["agents"] = json!(["bridge", "serve"]);
    rig.call("people.secretGrant", &p).unwrap();
    // the person must exist now; a grant for one person's own account
    rig.call(
        "people.secretGrant",
        &grant(&["house@sam"], "github", &["github/work", "github/play"]),
    )
    .unwrap();
    let t = rig.tables();
    assert_eq!(
        t["grants"]["house@sam"]["github"],
        json!(["github/work", "github/play"])
    );
    assert_eq!(t["grants"]["bridge"]["github"], "github/work");
    // a host connector's `<id>/host` profile is the host's own
    rig.call(
        "people.secretGrant",
        &grant(&["house"], "calendar", &["calendar/host"]),
    )
    .unwrap();
    assert_eq!(profile(&mut rig, "calendar/host"), json!({"store": "host"}));
    // a grant to a keyed profile of a personal service starts owned by the owner
    rig.call(
        "people.secretGrant",
        &grant(&["house"], "chatgpt", &["chatgpt/new"]),
    )
    .unwrap();
    assert_eq!(profile(&mut rig, "chatgpt/new")["person"], "kai");
}

#[test]
fn connect_refuses_a_grant_that_changed_since_it_looked() {
    let mut rig = rig();
    let mut p = grant(&["house"], "github", &["github/play"]);
    p["ifHave"] = json!({"house": ["github/other"]});
    let (msg, code) = died(&rig.call("people.secretGrant", &p).unwrap());
    assert!(msg.contains("changed while this ran"), "{msg}");
    assert_eq!(code, 1);
    p["ifHave"] = json!({"house": ["github/work"]});
    rig.call("people.secretGrant", &p).unwrap();
    assert_eq!(rig.tables()["grants"]["house"]["github"], "github/play");
}

#[test]
fn a_revoke_drops_the_grant_or_one_profile_and_a_disconnect_keeps_keyed_profiles() {
    let mut rig = rig();
    rig.call(
        "people.secretGrant",
        &grant(&["house", "garden"], "calendar", &["calendar/host"]),
    )
    .unwrap();
    rig.call(
        "people.secretSet",
        &set("github/play", &["GITHUB_TOKEN"], Some("house")),
    )
    .unwrap();
    // --profile: one of two goes, the other stays a plain name
    let r = |extra: Value| {
        let mut p = json!({"agent": "house", "service": "github", "viaSwitch": true, "dies": true});
        for (k, v) in extra.as_object().unwrap() {
            p[k] = v.clone();
        }
        p
    };
    rig.call("people.secretRevoke", &r(json!({"profile": "github/work"})))
        .unwrap();
    assert_eq!(rig.tables()["grants"]["house"]["github"], "github/play");
    // a disconnect of a keyed profile's grant leaves the profile (it is the owner's to keep)
    rig.call("people.secretRevoke", &r(json!({"dropHostProfiles": true})))
        .unwrap();
    let t = rig.tables();
    assert!(t["grants"]["house"].get("github").is_none());
    assert!(t["profiles"]["github/play"].is_object());
    // a host profile held by two agents stays until the last lets go
    let d = |agent: &str| json!({"agent": agent, "service": "calendar", "viaSwitch": true, "dropHostProfiles": true});
    rig.call("people.secretRevoke", &d("house")).unwrap();
    assert!(rig.tables()["profiles"]["calendar/host"].is_object());
    rig.call("people.secretRevoke", &d("garden")).unwrap();
    let t = rig.tables();
    assert!(
        t["profiles"].get("calendar/host").is_none(),
        "the last grant took the host profile"
    );
    assert!(t["grants"].get("garden").is_none());
    // nothing connected: nothing to do
    rig.call("people.secretRevoke", &d("garden")).unwrap();
    // the plain revoke of a service leaves the profiles
    rig.call(
        "people.secretRevoke",
        &json!({"agent": "house", "service": "github", "viaSwitch": true}),
    )
    .unwrap();
}

#[test]
fn the_env_writer_keeps_the_order_clears_and_tidies() {
    let mut rig = rig();
    let env = |pairs: Value| json!({"agent": "house", "service": "github", "pairs": pairs, "viaSwitch": true, "dies": true});
    rig.call("people.secretEnv", &env(json!([["A", "1"], ["B", "2"]])))
        .unwrap();
    // a changed variable keeps its place, a new one goes last
    rig.call("people.secretEnv", &env(json!([["A", "3"], ["C", "4"]])))
        .unwrap();
    let t = rig.tables();
    let keys: Vec<&String> = t["env"]["house"]["github"]
        .as_object()
        .unwrap()
        .keys()
        .collect();
    assert_eq!(keys, ["A", "B", "C"]);
    assert_eq!(t["env"]["house"]["github"]["A"], "3");
    // set, clear, set again moves it last, as the file does
    rig.call("people.secretEnv", &env(json!([["A", ""], ["A", "5"]])))
        .unwrap();
    let keys: Vec<String> = rig.tables()["env"]["house"]["github"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(keys, ["B", "C", "A"]);
    // clearing everything removes the service and the agent
    rig.call(
        "people.secretEnv",
        &env(json!([["A", ""], ["B", ""], ["C", ""]])),
    )
    .unwrap();
    assert!(rig.tables().get("env").is_none());
    let (msg, _) = died(
        &rig.call("people.secretEnv", &json!({"agent": "house", "service": "nosuch", "pairs": [["A", "1"]], "viaSwitch": true, "dies": true}))
            .unwrap(),
    );
    assert!(msg.contains("no service 'nosuch'"));
}

#[test]
fn no_value_is_in_core_db_and_a_keyed_profile_cannot_be_dropped_by_the_store() {
    let mut rig = rig();
    rig.call(
        "people.secretSet",
        &set("github/play", &["GITHUB_TOKEN"], None),
    )
    .unwrap();
    // the table holds the reference and nothing else
    let source: String = rig
        .core
        .conn()
        .query_row(
            "SELECT source FROM secret_vars WHERE profile = 'github/play'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(source, "keychain:kith/github/play#GITHUB_TOKEN");
    // the store refuses to drop a profile that holds keys, whatever wrote the change
    let mut before = rig.tables();
    let mut after = before.clone();
    after["profiles"]
        .as_object_mut()
        .unwrap()
        .shift_remove("github/play");
    let tx = rig.core.claim_tx().unwrap();
    let e = crate::authority::store::store(&tx, &before, &after).unwrap_err();
    assert!(e.to_string().contains("cannot be removed"), "{e}");
    drop(tx);
    before["profiles"].as_object_mut().unwrap();
}

#[test]
fn a_set_that_carries_a_value_is_refused_whole() {
    let mut rig = rig();
    let before = rig.tables();
    for key in ["value", "values", "secret"] {
        let mut p = set("github/play", &["GITHUB_TOKEN"], None);
        p[key] = json!("sentinel-9f3a");
        let (msg, code) = died(&rig.call("people.secretSet", &p).unwrap());
        assert_eq!(
            (msg.as_str(), code),
            (
                "the core takes the names of the variables, never a value",
                2
            )
        );
    }
    assert_eq!(rig.tables(), before);
}

#[test]
fn the_callers_person_is_read_from_the_env_it_sends() {
    use crate::authority::call::person_of;
    assert_eq!(
        person_of(&json!({"env": {"KITH_PERSON": "sam"}})).as_deref(),
        Some("sam")
    );
    assert_eq!(person_of(&json!({"env": {"KITH_PERSON": ""}})), None);
    assert_eq!(person_of(&json!({"env": {"KITH_HOME": "/x"}})), None);
    assert_eq!(person_of(&json!({})), None);
}

#[test]
fn agent_access_answers_a_refusal_as_an_answer_and_still_needs_a_switched_core() {
    let mut rig = rig();
    let p =
        |domain: &str| json!({"agent": "house", "domain": domain, "viaSwitch": true, "dies": true});
    let said = rig
        .call("people.agentAccess", &p("kept.example.com"))
        .unwrap();
    assert_eq!(
        said["lines"],
        json!(["every guest of house may reach kept.example.com beyond the box"])
    );
    // a refusal comes back as `{died, why}` and writes nothing
    let before = rig.tables();
    let (msg, code) = died(&rig.call("people.agentAccess", &p("*.example.com")).unwrap());
    assert!(msg.contains("not a plain host name"), "{msg}");
    assert_eq!(code, 2);
    assert_eq!(rig.tables(), before);
    // the removal passes on a damaged record, an add does not
    let layout = Layout::new(&crate::authority::revoke::Life::beside(&rig.c).machine);
    std::fs::write(state::path(&layout, "authority").unwrap(), "garbage").unwrap();
    let rm = json!({"agent": "house", "domain": "kept.example.com", "remove": true, "viaSwitch": true, "dies": true});
    rig.call("people.agentAccess", &rm).unwrap();
    let e = rig
        .call("people.agentAccess", &p("again.example.com"))
        .unwrap_err();
    assert!(e.contains("not switched"), "{e}");
}

#[test]
fn a_helper_since_survives_the_core_and_comes_back() {
    let mut rig = rig();
    rig.call(
        "people.helper",
        &json!({"agent": "house", "person": "sam", "helper": "garden", "viaSwitch": true, "dies": true}),
    )
    .unwrap();
    let since = rig.tables()["members"]["house"]["sam"]["helper_since"]["garden"].clone();
    assert!(since.is_number(), "{since}");
    // removed with the helper
    rig.call(
        "people.helper",
        &json!({"agent": "house", "person": "sam", "helper": "garden", "remove": true, "viaSwitch": true, "dies": true}),
    )
    .unwrap();
    let row = rig.tables()["members"]["house"]["sam"].clone();
    assert!(
        row.get("helper_since")
            .is_none_or(|h| h.get("garden").is_none()),
        "{row}"
    );
}
