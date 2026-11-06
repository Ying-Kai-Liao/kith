//! The budget, the grants ledger and a person's chat handle in the core (the "switched gaps"): ports of `kith-auth
//! budgets`, `ledger [--rm]` and `handle`, with the refusals they make. A switched host sends these here instead of to a
//! `kith-auth` that exits 3 on the tombstone.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::{Value, json};

use super::writes::Rig;

const PKG: &str = "guest_domains: [pkg.example]\n";

fn world() -> Value {
    json!({
        "people": {
            "kai": {"owner": true, "name": "Kai"},
            "sam": {"name": "Sam", "was": ["samuel"], "handles": {"telegram": "111"}},
            "amy": {"name": "Amy"},
            "bot-a": {"bot_of": "sam"},
        },
        "members": {
            "house": {
                "sam": {"preset": "member", "budget": "20",
                        "access": {"read": ["/tmp/docs"], "domains": ["own.example"]},
                        "access_since": {"read:/tmp/docs": 5}, "access_by": {"read:/tmp/docs": null, "domain:own.example": "amy"},
                        "jobs": {"daily": {"since": 7}}, "helpers": ["garden"]},
                "amy": {"preset": "guest"},
                "bot-a": {"preset": "guest"},
                "*": {"preset": "guest", "budget": "5"},
            },
        },
        "agent_access": {"house": {"domains": ["kept.example"], "since": {"kept.example": 9}, "by": {"kept.example": null}}},
        "grants": {"house@sam": {"notion": ["p1", "p2"], "slack": "p3"}, "house@kai": {"notion": "own"}},
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
    // the test loader leaves lane B's tables to the import; these are what the import writes for `world()`'s `grants`
    for (agent, service, profile, seq) in [
        ("house@sam", "notion", "p1", 0),
        ("house@sam", "notion", "p2", 1),
        ("house@sam", "slack", "p3", 0),
        ("house@kai", "notion", "own", 0),
    ] {
        rig.core
            .conn()
            .execute(
                "INSERT INTO secret_grants (agent, service, profile, seq) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![agent, service, profile, seq],
            )
            .unwrap();
    }
    rig
}

fn keys(rig: &mut Rig, params: &Value) -> Vec<String> {
    rig.call("people.ledger", params).unwrap()["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["key"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn budgets_are_read_set_and_cleared_through_the_core() {
    let mut rig = rig();
    let said = rig
        .call("people.budgets", &json!({"agent": "house"}))
        .unwrap();
    // the `*` row is left out; a person with none is null
    assert_eq!(
        said["budgets"],
        json!({"sam": 20.0, "amy": null, "bot-a": null})
    );
    assert_eq!(
        said["lines"][0].as_str().unwrap().parse::<Value>().unwrap(),
        said["budgets"]
    );
    // set, set again (the same), clear
    for _ in 0..2 {
        rig.lines(
            "people.grant",
            &json!({"person": "amy", "agent": "house", "budget": "12.5"}),
        )
        .unwrap();
    }
    let said = rig
        .call("people.budgets", &json!({"agent": "house"}))
        .unwrap();
    assert_eq!(said["budgets"]["amy"], json!(12.5));
    rig.lines(
        "people.grant",
        &json!({"person": "amy", "agent": "house", "budget": ""}),
    )
    .unwrap();
    let said = rig
        .call("people.budgets", &json!({"agent": "house"}))
        .unwrap();
    assert_eq!(said["budgets"]["amy"], Value::Null);
    // an agent nobody has a row on: nothing, not an error
    assert_eq!(
        rig.call("people.budgets", &json!({"agent": "garden"}))
            .unwrap()["budgets"],
        json!({})
    );
}

#[test]
fn a_budget_that_is_no_number_is_an_error_never_no_limit() {
    let mut rig = Rig::new(&json!({
        "people": {"kai": {"owner": true}, "sam": {}},
        "members": {"house": {"sam": {"preset": "member", "budget": "lots"}}},
    }));
    let e = rig
        .call("people.budgets", &json!({"agent": "house"}))
        .unwrap_err();
    assert!(e.contains("is not a number"), "{e}");
    assert!(
        rig.call("people.budgets", &json!({}))
            .unwrap_err()
            .contains("agent is required")
    );
}

#[test]
fn a_member_row_that_is_no_object_is_an_error_in_the_budgets_read() {
    // The loader refuses such a row at import; the read must not turn one into "no limit" if it ever meets one.
    let auth = json!({"members": {"house": {"sam": "member"}}});
    let e = crate::authority::write::budgets(&auth, "house").unwrap_err();
    assert!(e.msg.contains("not an object"), "{}", e.msg);
}

#[test]
fn a_marked_ledger_handle_and_cap_write_is_refused_unless_the_phase_is_switched() {
    use crate::cutover::state::{self, Phase, Switch};
    let mut rig = rig();
    let layout =
        crate::layout::Layout::new(&crate::authority::revoke::Life::beside(&rig.c).machine);
    let marked = [
        (
            "people.removeGrant",
            json!({"key": "read:house:sam:/tmp/docs", "viaSwitch": true}),
        ),
        (
            "people.handle",
            json!({"person": "sam", "platform": "telegram", "remove": true, "viaSwitch": true}),
        ),
        ("people.botsCap", json!({"amount": "5", "viaSwitch": true})),
    ];
    let before = keys(&mut rig, &json!({}));
    // RollingBack is not here: a stuck rollback keeps the tombstone and takes marked writes (removals must work).
    for p in [None, Some(Phase::Stepping)] {
        match p {
            Some(p) => state::write(
                &layout,
                "authority",
                &Switch {
                    phase: p,
                    snapshot: None,
                    at: 1,
                },
            )
            .unwrap(),
            None => state::clear(&layout, "authority").unwrap(),
        }
        for (method, params) in &marked {
            let e = rig.call(method, params).unwrap_err();
            assert!(e.contains("not switched"), "{method}: {e}");
        }
    }
    std::fs::write(state::path(&layout, "authority").unwrap(), "garbage").unwrap();
    for (method, params) in &marked {
        assert!(
            rig.call(method, params)
                .unwrap_err()
                .contains("not switched"),
            "{method}: damaged record"
        );
    }
    assert_eq!(keys(&mut rig, &json!({})), before, "nothing was removed");
    let sam = rig.call("people.listPeople", &json!({})).unwrap();
    assert!(sam.to_string().contains("111"), "the handle stayed: {sam}");
}

#[test]
fn the_budget_writes_keep_the_refusals_of_kith_auth() {
    let mut rig = rig();
    for bad in ["lots", "-3", "nan", "inf", "-inf", "infinity"] {
        let e = rig
            .lines(
                "people.grant",
                &json!({"person": "amy", "agent": "house", "budget": bad}),
            )
            .unwrap_err();
        assert!(e.contains("budget is dollars a month"), "{e}");
    }
    let e = rig
        .lines(
            "people.grant",
            &json!({"person": "bot-a", "agent": "house", "budget": "5"}),
        )
        .unwrap_err();
    assert!(e.contains("a bot spends from its owner's budget"), "{e}");
}

#[test]
fn the_ledger_lists_what_outlives_a_run_for_anyone_but_the_owner() {
    let mut rig = rig();
    let all = keys(&mut rig, &json!({}));
    assert_eq!(
        all,
        [
            "domain:house:*:kept.example",
            "domain:house:*:pkg.example",
            "domain:house:sam:own.example",
            "helper:house:sam:garden",
            "job:house:sam:daily",
            "read:house:sam:/tmp/docs",
            "service:house:sam:p1",
            "service:house:sam:p2",
            "service:house:sam:p3",
        ],
        "the owner's own grants (house@kai) are not in it"
    );
    let rows = rig.call("people.ledger", &json!({})).unwrap();
    let by = |key: &str| {
        rows["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["key"] == key)
            .unwrap()
            .clone()
    };
    assert_eq!(by("read:house:sam:/tmp/docs")["approved_by"], "owner");
    assert_eq!(by("read:house:sam:/tmp/docs")["since"], 5);
    assert_eq!(by("read:house:sam:/tmp/docs")["name"], "Sam");
    assert_eq!(by("domain:house:sam:own.example")["approved_by"], "amy");
    assert_eq!(by("domain:house:*:pkg.example")["approved_by"], "package");
    assert_eq!(by("domain:house:*:kept.example")["since"], 9);
    assert_eq!(by("job:house:sam:daily")["since"], 7);
    // the same rows as the one JSON line `kith-auth ledger --json` prints
    let line: Value = rows["lines"][0].as_str().unwrap().parse().unwrap();
    assert_eq!(line, rows["rows"]);
    assert_eq!(
        keys(&mut rig, &json!({"person": "samuel"})).len(),
        7,
        "an old id names the person"
    );
    assert_eq!(
        keys(&mut rig, &json!({"agent": "garden"})),
        Vec::<String>::new()
    );
    assert!(
        rig.call("people.ledger", &json!({"person": "-x"}))
            .unwrap_err()
            .contains("ledger [--person")
    );
}

#[test]
fn a_ledger_entry_is_removed_once_and_the_second_try_is_refused() {
    let mut rig = rig();
    for (key, said) in [
        (
            "read:house:sam:/tmp/docs",
            "sam no longer has read /tmp/docs for house",
        ),
        (
            "domain:house:sam:own.example",
            "sam no longer has domain own.example for house",
        ),
        (
            "job:house:sam:daily",
            "sam no longer has job daily for house",
        ),
        (
            "helper:house:sam:garden",
            "sam no longer has helper garden for house",
        ),
        (
            "domain:house:*:kept.example",
            "* no longer has domain kept.example for house",
        ),
        (
            "service:house:sam:p2",
            "sam no longer has service p2 for house",
        ),
    ] {
        assert_eq!(
            rig.lines("people.removeGrant", &json!({"key": key}))
                .unwrap(),
            [said]
        );
        let e = rig
            .lines("people.removeGrant", &json!({"key": key}))
            .unwrap_err();
        assert!(e.contains("removed already?"), "{key}: {e}");
    }
    // a service grant that shared a service with another keeps the other; the last one takes the service row with it
    let g = rig.tables()["grants"]["house@sam"].clone();
    assert_eq!(g, json!({"notion": "p1", "slack": "p3"}));
    rig.lines(
        "people.removeGrant",
        &json!({"key": "service:house:sam:p1"}),
    )
    .unwrap();
    rig.lines(
        "people.removeGrant",
        &json!({"key": "service:house:sam:p3"}),
    )
    .unwrap();
    assert!(rig.tables()["grants"].get("house@sam").is_none());
    assert_eq!(keys(&mut rig, &json!({})), ["domain:house:*:pkg.example"]);
    // a host the package declares is not the owner's to take back from a card
    let e = rig
        .lines(
            "people.removeGrant",
            &json!({"key": "domain:house:*:pkg.example"}),
        )
        .unwrap_err();
    assert!(e.contains("is declared by the package of house"), "{e}");
}

#[test]
fn a_ledger_key_that_is_none_is_refused_as_a_usage_error() {
    let mut rig = rig();
    for bad in [
        "",
        "read",
        "read:house:sam",
        "bogus:house:sam:x",
        "read::sam:x",
        "read:house::x",
    ] {
        let e = rig
            .lines("people.removeGrant", &json!({"key": bad}))
            .unwrap_err();
        assert!(e.contains("not a ledger key"), "{bad:?}: {e}");
    }
    // nothing was written
    assert_eq!(keys(&mut rig, &json!({})).len(), 9);
}

#[test]
fn a_chat_handle_is_bound_changed_and_let_go() {
    let mut rig = rig();
    let bind = |rig: &mut Rig, who: &str, plat: &str, uid: &str| {
        rig.lines(
            "people.handle",
            &json!({"person": who, "platform": plat, "uid": uid}),
        )
    };
    assert_eq!(
        bind(&mut rig, "amy", "telegram", "222").unwrap(),
        ["amy is telegram 222"]
    );
    assert_eq!(
        bind(&mut rig, "amy", "kith", "user_2abc").unwrap(),
        ["amy is kith user_2abc"]
    );
    assert_eq!(
        bind(&mut rig, "amy", "slack", "U0123ABC").unwrap(),
        ["amy is slack U0123ABC"]
    );
    assert_eq!(
        bind(&mut rig, "amy", "discord", "-5").unwrap(),
        ["amy is discord -5"]
    );
    // again, and over an earlier account of the same person
    assert_eq!(
        bind(&mut rig, "amy", "kith", "user_2abc").unwrap(),
        ["amy is kith user_2abc"]
    );
    assert_eq!(
        bind(&mut rig, "amy", "kith", "user_9new").unwrap(),
        ["amy is kith user_9new"]
    );
    let t = rig.tables();
    assert_eq!(t["people"]["amy"]["handles"]["kith"], "user_9new");
    let listed = rig.call("people.listPeople", &json!({})).unwrap();
    let amy = listed["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "amy")
        .unwrap()
        .clone();
    assert_eq!(amy["kith"], "user_9new", "{amy}");
    // another person's account, and ids that are no ids
    assert_eq!(
        bind(&mut rig, "amy", "telegram", "111").unwrap_err(),
        "telegram 111 is already sam"
    );
    for (plat, uid) in [
        ("telegram", "@amy"),
        ("telegram", ""),
        ("kith", "2abc"),
        ("kith", "user_"),
        ("slack", "u0123"),
    ] {
        let e = bind(&mut rig, "amy", plat, uid).unwrap_err();
        assert!(e.contains("user ids are"), "{plat} {uid:?}: {e}");
    }
    assert!(
        bind(&mut rig, "ghost", "telegram", "5")
            .unwrap_err()
            .starts_with("no person 'ghost'")
    );
    let gone = rig.lines(
        "people.handle",
        &json!({"person": "amy", "platform": "kith", "remove": true}),
    );
    assert_eq!(gone.unwrap(), ["amy: no kith account"]);
    assert!(
        rig.tables()["people"]["amy"]["handles"]
            .get("kith")
            .is_none()
    );
    // letting go of what was never bound is not an error, as in kith-auth; an unknown person is
    let again = rig.lines(
        "people.handle",
        &json!({"person": "amy", "platform": "kith", "remove": true}),
    );
    assert_eq!(again.unwrap(), ["amy: no kith account"]);
    let e = rig
        .lines(
            "people.handle",
            &json!({"person": "ghost", "platform": "kith", "remove": true}),
        )
        .unwrap_err();
    assert!(e.starts_with("no person 'ghost'"), "{e}");
    // no uid and no --rm
    assert!(
        rig.lines(
            "people.handle",
            &json!({"person": "amy", "platform": "kith"})
        )
        .unwrap_err()
        .contains("uid is required")
    );
}
