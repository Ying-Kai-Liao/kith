//! Guest domains, "just this once" and `bridge_held` in the core (M2-1b): the domains a package declares for every guest
//! (`guest_domains:`) and the ones the owner kept on a card (`agent_access`) join a guest's access; a "just this
//! once" grant is used up in the transaction that decided; `bridge_held` follows kith serve's sync file. Python parity
//! for the same cases is the difftest corpus (`secrets-guest-domains*`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::{Value, json};

use super::FUTURE;
use super::writes::Rig;

const PKG: &str = "guest_domains: [pkg.example, a.example, \"*.wild.example\", 10.0.0.1, pkg.example]  # the package\n";

fn world() -> Value {
    json!({
        "people": {
            "kai": {"owner": true},
            "sam": {"name": "Sam"},
            "pat": {},
            "bot-a": {"bot_of": "sam"},
        },
        "members": {
            "house": {
                "sam": {"preset": "member", "access": {"domains": ["own.example", "pkg.example"]},
                        "helper_once": {"garden": FUTURE}},
                "pat": {"preset": "guest", "helper_once": {"garden": FUTURE}},
                "bot-a": {},
            },
            "garden": {"*": {"preset": "guest"}},
            "sound": {"sam": {"preset": "member"}},
        },
        "agent_access": {
            "house": {"domains": ["kept.example", "pkg.example", "*.bad.example", "host.example:80", 7],
                      "since": {"kept.example": 1}, "by": {"kept.example": null}},
            "sound": {"domains": []},
        },
    })
}

fn rig() -> Rig {
    let rig = Rig::new(&world());
    std::fs::write(
        rig.machine_dir
            .path()
            .join("agents/house/role/capabilities.yaml"),
        PKG,
    )
    .unwrap();
    rig
}

fn domains(r: &Value) -> Vec<&str> {
    r["access"]["domains"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d.as_str().unwrap())
        .collect()
}

fn resolve(rig: &mut Rig, agent: &str, person: &str) -> Value {
    rig.call("people.resolve", &json!({"agent": agent, "person": person}))
        .unwrap()
}

fn take(rig: &mut Rig, agent: &str, person: &str) -> Value {
    rig.call(
        "people.resolve",
        &json!({"agent": agent, "person": person, "via": "house", "takeOnce": true}),
    )
    .unwrap()
}

#[test]
fn a_guest_gets_the_packages_and_the_kept_domains_after_their_own() {
    let mut rig = rig();
    // their own, then the package's (a repeat, a wildcard and an address left out), then the owner's kept ones
    let r = resolve(&mut rig, "house", "sam");
    assert_eq!(
        domains(&r),
        ["own.example", "pkg.example", "a.example", "kept.example"]
    );
    // a guest with no domains of their own gets them too, and a bot never more than its owner holds
    assert_eq!(
        domains(&resolve(&mut rig, "house", "pat")),
        ["pkg.example", "a.example", "kept.example"]
    );
    assert_eq!(
        domains(&resolve(&mut rig, "house", "bot-a")),
        ["pkg.example", "a.example", "kept.example"]
    );
}

#[test]
fn the_owner_is_given_nothing_extra() {
    let mut rig = rig();
    let r = resolve(&mut rig, "house", "kai");
    assert!(r.get("access").is_none(), "{r}");
}

#[test]
fn an_agent_access_row_with_no_domains_is_not_a_refusal() {
    let mut rig = rig();
    let r = resolve(&mut rig, "sound", "sam");
    assert_eq!(r["allowed"], true, "{r}");
    assert_eq!(domains(&r), Vec::<&str>::new(), "{r}");
}

#[test]
fn a_package_with_no_capabilities_file_adds_only_the_kept_domains() {
    let mut rig = rig();
    std::fs::remove_file(
        rig.machine_dir
            .path()
            .join("agents/house/role/capabilities.yaml"),
    )
    .unwrap();
    assert_eq!(
        domains(&resolve(&mut rig, "house", "pat")),
        ["kept.example", "pkg.example"]
    );
}

#[test]
fn a_guest_domains_line_that_is_no_list_is_left_out_whole() {
    let mut rig = rig();
    std::fs::write(
        rig.machine_dir
            .path()
            .join("agents/house/role/capabilities.yaml"),
        "guest_domains: pkg.example\n",
    )
    .unwrap();
    assert_eq!(
        domains(&resolve(&mut rig, "house", "pat")),
        ["kept.example", "pkg.example"]
    );
}

#[test]
fn without_the_flag_or_a_via_nothing_is_used() {
    let mut rig = rig();
    let r = rig
        .call(
            "people.resolve",
            &json!({"agent": "garden", "person": "pat", "via": "house"}),
        )
        .unwrap();
    assert_eq!(r["held"], false);
    let r = rig
        .call(
            "people.resolve",
            &json!({"agent": "sound", "person": "pat", "takeOnce": true}),
        )
        .unwrap();
    assert_eq!(r["allowed"], false, "{r}");
    assert_eq!(
        rig.tables()["members"]["house"]["pat"]["helper_once"],
        json!({"garden": FUTURE})
    );
}

#[test]
fn a_grant_that_lets_a_launch_through_is_taken_only_once_in_a_row_of_launches() {
    // `sound` has no row for pat: only the grant on pat's own row of `house` lets the helper run
    let mut rig = Rig::new(&json!({
        "people": {"kai": {"owner": true}, "pat": {}},
        "members": {"house": {"pat": {"preset": "guest", "helper_once": {"sound": FUTURE}}}},
    }));
    let first = take(&mut rig, "sound", "pat");
    assert!(
        first["why"].as_str().unwrap().ends_with("(just this once)"),
        "{first}"
    );
    assert_eq!(
        rig.tables()["members"]["house"]["pat"]["helper_once"],
        json!({})
    );
    let outcomes: Vec<bool> = [first]
        .into_iter()
        .chain((0..2).map(|_| take(&mut rig, "sound", "pat")))
        .map(|r| r["allowed"].as_bool().unwrap())
        .collect();
    assert_eq!(outcomes, [true, false, false]);
}

#[test]
fn a_held_launch_does_not_spend_the_grant() {
    let mut rig = Rig::new(&json!({
        "people": {"kai": {"owner": true}, "pat": {}},
        "members": {"house": {"pat": {"preset": "guest", "helper_once": {"sound": FUTURE}}}},
    }));
    let serve = rig.machine_dir.path().join("serve");
    std::fs::create_dir_all(&serve).unwrap();
    std::fs::write(
        serve.join("cloud-sync.json"),
        r#"{"linked": true, "until": 1}"#,
    )
    .unwrap();
    let r = take(&mut rig, "sound", "pat");
    assert_eq!(r["held"], true);
    assert_eq!(r["allowed"], false, "{r}");
    assert_eq!(
        rig.tables()["members"]["house"]["pat"]["helper_once"],
        json!({"sound": FUTURE})
    );
    // in sync again: the launch that runs spends it
    std::fs::write(
        serve.join("cloud-sync.json"),
        r#"{"linked": true, "until": 4102444800000}"#,
    )
    .unwrap();
    let r = take(&mut rig, "sound", "pat");
    assert_eq!(
        (r["held"].clone(), r["allowed"].clone()),
        (json!(false), json!(true)),
        "{r}"
    );
}

#[test]
fn a_suspended_person_cannot_spend_a_grant_and_gets_it_back_with_undo() {
    let mut rig = Rig::new(&json!({
        "people": {"kai": {"owner": true}, "pat": {}},
        "members": {"house": {"pat": {"preset": "guest", "helper_once": {"sound": FUTURE}}}},
    }));
    rig.lines("people.suspend", &json!({"id": "pat"})).unwrap();
    let r = take(&mut rig, "sound", "pat");
    assert_eq!(r["allowed"], false, "{r}");
    assert_eq!(
        rig.tables()["members"]["house"]["pat"]["helper_once"],
        json!({"sound": FUTURE})
    );
    rig.lines("people.undo", &json!({"id": "pat"})).unwrap();
    assert_eq!(take(&mut rig, "sound", "pat")["allowed"], true);
}

#[test]
fn bridge_held_follows_the_sync_file_and_never_holds_the_owner() {
    let mut rig = rig();
    let held = |rig: &mut Rig, who: &str| {
        rig.call("people.bridgeHeld", &json!({"person": who}))
            .unwrap()["held"]
            .as_bool()
            .unwrap()
    };
    assert!(!held(&mut rig, "sam"), "no file: never linked");
    let serve = rig.machine_dir.path().join("serve");
    std::fs::create_dir_all(&serve).unwrap();
    for (text, sam, kai) in [
        (r#"{"linked": true, "until": 1}"#, true, false),
        (r#"{"linked": true, "until": 4102444800000}"#, false, false),
        (r#"{"linked": false, "until": 1}"#, false, false),
        ("not json", false, false),
    ] {
        std::fs::write(serve.join("cloud-sync.json"), text).unwrap();
        assert_eq!(
            (held(&mut rig, "sam"), held(&mut rig, "kai")),
            (sam, kai),
            "{text}"
        );
    }
}

#[test]
fn the_kept_domains_survive_a_write_beside_them() {
    let mut rig = rig();
    rig.lines(
        "people.grant",
        &json!({"person": "pat", "agent": "garden", "preset": "member"}),
    )
    .unwrap();
    assert_eq!(
        rig.tables()["agent_access"]["house"]["domains"][0],
        "kept.example"
    );
}
