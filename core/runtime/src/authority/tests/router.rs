//! The reads `kith-auth` routes here once auth.json is a tombstone (M2-23): `who`, `granted`, `bots-cap --json`, and
//! `may` with the caller and the "just this once" it takes. The byte-for-byte parity with the file is
//! `kernel/tests/kith-auth-router-check`; these hold the core's side: the fields and their order, the refusals of a
//! database that cannot say, and the answer a rule that dies gives.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::{Value, json};

use super::FUTURE;
use super::writes::Rig;

fn world() -> Value {
    json!({
        "people": {
            "kai": {"owner": true, "handles": {"telegram": "1", "kith": "user_abc"}},
            "sam": {"name": "Sam", "handles": {"telegram": "2"}, "was": ["samuel"]},
            "tim": {"handles": {"telegram": "3"}, "revoked": {"at": 5}},
            "pat": {},
            "bot-a": {"bot_of": "kai", "was": ["bot-old"]},
            "bot-b": {"bot_of": "kai"},
            "bot-c": {"bot_of": "sam"},
        },
        "presets": {"member": {"mode": "default"}},
        "members": {
            "house": {
                "sam": {"preset": "member"},
                "pat": {"preset": "gone"},
                "bot-a": {},
            },
            "garden": {"sam": {"preset": "member"}},
            "sound": {"tim": {"preset": "member"}},
        },
        "owner_bots_budget": "50",
    })
}

fn keys(v: &Value) -> Vec<&str> {
    v.as_object().unwrap().keys().map(String::as_str).collect()
}

#[test]
fn who_gives_the_fields_of_kith_auth_who_in_its_order() {
    let mut rig = Rig::new(&world());
    let r = rig
        .call("people.who", &json!({"platform": "telegram", "id": "2"}))
        .unwrap();
    assert_eq!(
        keys(&r),
        [
            "person",
            "owner",
            "was",
            "owner_id",
            "owner_linked",
            "revoked",
            "held"
        ]
    );
    assert_eq!(r["person"], "sam");
    assert_eq!(r["owner"], false);
    assert_eq!(r["was"], json!(["samuel"]));
    assert_eq!(r["owner_id"], "kai");
    assert_eq!(r["owner_linked"], true);
    assert_eq!(r["revoked"], false);
    assert_eq!(r["held"], false);

    let r = rig
        .call("people.who", &json!({"platform": "telegram", "id": "3"}))
        .unwrap();
    assert_eq!(
        (r["person"].as_str(), r["revoked"].as_bool()),
        (Some("tim"), Some(true))
    );

    let r = rig
        .call("people.who", &json!({"platform": "slack", "id": "U1"}))
        .unwrap();
    assert_eq!(
        r,
        json!({"person": null, "owner": false, "was": [], "owner_id": "kai", "owner_linked": false, "revoked": false, "held": false})
    );
    let r = rig
        .call("people.who", &json!({"platform": "telegram", "id": "1"}))
        .unwrap();
    assert_eq!(
        (r["person"].as_str(), r["owner"].as_bool()),
        (Some("kai"), Some(true))
    );
}

#[test]
fn who_refuses_while_the_handles_are_stale() {
    let mut rig = Rig::new(&world());
    rig.core
        .conn()
        .execute(
            "INSERT INTO authority_settings (key, value) VALUES ('handles_stale', '1')",
            [],
        )
        .unwrap();
    let e = rig
        .call("people.who", &json!({"platform": "telegram", "id": "2"}))
        .unwrap_err();
    assert!(e.contains("imported before people.handles"), "{e}");
    // a verb that reads no handle still answers
    assert!(
        rig.call("people.may", &json!({"person": "sam", "agent": "house"}))
            .is_ok()
    );
}

#[test]
fn granted_gives_the_profiles_as_a_list() {
    let mut rig = Rig::new(&world());
    // the test loader leaves lane B's tables to the import; these are what it writes for a `grants` of auth.json
    for (service, profile, seq) in [("notion", "p1", 0), ("notion", "p2", 1), ("slack", "p3", 0)] {
        rig.core
            .conn()
            .execute(
                "INSERT INTO secret_grants (agent, service, profile, seq) VALUES ('house', ?1, ?2, ?3)",
                rusqlite::params![service, profile, seq],
            )
            .unwrap();
    }
    let r = rig
        .call(
            "people.granted",
            &json!({"agent": "house", "service": "notion"}),
        )
        .unwrap();
    assert_eq!(r, json!({"grants": ["p1", "p2"]}));
    let r = rig
        .call(
            "people.granted",
            &json!({"agent": "house", "service": "slack"}),
        )
        .unwrap();
    assert_eq!(r, json!({"grants": ["p3"]}));
    let r = rig
        .call(
            "people.granted",
            &json!({"agent": "house", "service": "none"}),
        )
        .unwrap();
    assert_eq!(r, json!({"grants": []}));
    rig.core
        .conn()
        .execute("INSERT INTO secret_stale (reason) VALUES ('x')", [])
        .unwrap();
    assert!(
        rig.call(
            "people.granted",
            &json!({"agent": "house", "service": "notion"})
        )
        .is_err()
    );
}

#[test]
fn secrets_view_is_the_tables_in_auth_shape_without_values_handles_or_invite_codes() {
    let mut rig = Rig::new(&json!({
        "people": {"kai": {"owner": true, "handles": {"telegram": "1"}}, "sam": {"handles": {"telegram": "2"}}},
        "presets": {"member": {"mode": "default"}},
        "members": {"house": {"sam": {"preset": "member"}}},
        "invites": {"ABC234": {"person": "sam", "expires": FUTURE}},
    }));
    rig.core
        .conn()
        .execute_batch(
            "INSERT INTO secret_profiles (profile, person, lent, browser) VALUES \
               ('github/work', NULL, NULL, NULL), ('chrome/main', 'kai', '[\"sam\"]', 'chrome'); \
             INSERT INTO secret_vars (profile, var, source) VALUES ('github/work', 'TOKEN', 'keychain:kith/github/work#TOKEN'); \
             INSERT INTO secret_grants (agent, service, profile, seq) VALUES ('house', 'github', 'github/work', 0), \
               ('house@sam', 'chrome', 'chrome/main', 0); \
             INSERT INTO secret_env (agent, service, var, value, seq) VALUES \
               ('house', 'svc', 'B', '\"2\"', 0), ('house', 'svc', 'A', '1', 1);",
        )
        .unwrap();
    let r = rig.call("people.secretsView", &json!({})).unwrap();
    assert_eq!(keys(&r), ["auth"]);
    let auth = &r["auth"];
    assert_eq!(auth["env"], json!({"house": {"svc": {"B": "2", "A": 1}}}));
    assert_eq!(
        keys(&auth["env"]["house"]["svc"]),
        ["B", "A"],
        "the file's order"
    );
    assert_eq!(
        auth["profiles"]["github/work"],
        json!({"person": "*", "TOKEN": "keychain:kith/github/work#TOKEN"}),
        "a profile with no owner reads as shared, not as the owner's"
    );
    assert_eq!(
        auth["profiles"]["chrome/main"],
        json!({"person": "kai", "with": ["sam"], "browser": "chrome"})
    );
    assert_eq!(
        auth["grants"]["house@sam"],
        json!({"chrome": "chrome/main"})
    );
    assert!(auth["people"]["sam"].get("handles").is_none());
    assert!(auth.get("invites").is_none(), "no invite codes");
    // a database imported before profiles, grants and env were kept cannot say what is granted: refused, with the way out
    rig.core
        .conn()
        .execute("INSERT INTO secret_stale (reason) VALUES ('x')", [])
        .unwrap();
    let e = rig.call("people.secretsView", &json!({})).unwrap_err();
    assert!(e.contains("import again before the step"), "{e}");
}

#[test]
fn bots_cap_json_is_the_cap_and_every_id_of_the_owners_bots() {
    let mut rig = Rig::new(&world());
    let r = rig.call("people.botsCap", &json!({"json": true})).unwrap();
    assert_eq!(keys(&r), ["cap", "bots"]);
    assert_eq!(r["cap"].as_f64(), Some(50.0));
    assert_eq!(r["bots"], json!(["bot-a", "bot-old", "bot-b"]));
    // without the owner's cap: null
    let mut rig = Rig::new(&json!({"people": {"kai": {"owner": true}}}));
    let r = rig.call("people.botsCap", &json!({"json": true})).unwrap();
    assert_eq!(r, json!({"cap": null, "bots": []}));
}

#[test]
fn may_takes_the_caller_and_the_once() {
    let mut rig = Rig::new(&json!({
        "people": {"kai": {"owner": true}, "pat": {}},
        "presets": {"guest": {"mode": "default"}},
        "members": {
            "house": {"pat": {"preset": "guest", "helper_once": {"sound": FUTURE}}},
        },
    }));
    let may = |rig: &mut Rig, extra: Value| {
        let mut p = json!({"person": "pat", "agent": "sound"});
        p.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        rig.call("people.may", &p).unwrap()
    };
    // pat has no row on sound: no, and the words name the caller whose helpers were looked at
    let r = may(&mut rig, json!({}));
    assert_eq!(r["allowed"], false);
    let r = may(&mut rig, json!({"via": "house"}));
    assert_eq!(r["allowed"], false);
    assert!(
        r["why"]
            .as_str()
            .unwrap()
            .contains("not in their house helpers"),
        "{r}"
    );
    // the "just this once" on house counts only when asked for, and reading it uses nothing up
    let r = may(&mut rig, json!({"via": "house", "once": true}));
    assert_eq!(r["allowed"], true, "{r}");
    assert_eq!(r["why"], "helper of house");
    let r = may(&mut rig, json!({"via": "house", "once": true}));
    assert_eq!(r["allowed"], true, "{r}");
    assert_eq!(
        rig.tables()["members"]["house"]["pat"]["helper_once"],
        json!({"sound": FUTURE})
    );
}

#[test]
fn a_rule_that_dies_is_an_error_or_the_exit_code_and_line_when_asked() {
    let mut rig = Rig::new(&world());
    // pat is a member of house on a preset that is gone: `resolve` has no answer
    let e = rig
        .call(
            "people.resolve",
            &json!({"agent": "house", "person": "pat"}),
        )
        .unwrap_err();
    assert!(e.contains("no preset 'gone'"), "{e}");
    let r = rig
        .call(
            "people.resolve",
            &json!({"agent": "house", "person": "pat", "dies": true}),
        )
        .unwrap();
    assert_eq!(r["died"], 1);
    assert!(
        r["why"].as_str().unwrap().contains("no preset 'gone'"),
        "{r}"
    );
    assert!(r.get("allowed").is_none());
}
