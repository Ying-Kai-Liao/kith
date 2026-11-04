//! The device-pairing access requests (M2-3) against core.db. The parity script `access_requests_parity.json` is
//! replayed here and, step for step, against the host's own `settle` in `serve/host/src/remote/cloud.rs` (test
//! `access_request_parity_script`): the same steps must give the same answers on both sides. The rest are the rules the
//! script cannot say: the answer shapes, who may answer, two devices at once, what a refusal leaves behind.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation
)]

use rusqlite::Connection;
use serde_json::{Value, json};

use super::writes::{Rig, base};
use crate::authority::requests::{self, NewRequest};

const SCRIPT: &str =
    include_str!("../../../../../kernel/tests/fixtures/access_requests_parity.json");
const HOUR: i64 = 3_600_000;

fn world() -> Value {
    let mut w = base();
    w["people"]["ghost"] = json!({});
    w
}

fn now(rig: &Rig) -> i64 {
    rig.c.now_ms as i64
}

/// Files a request the way the host's `sync_requests` does, then reveals its code when the script says it has one.
fn file(rig: &mut Rig, step: &Value, n: i64) -> Result<(), String> {
    let id = step["id"].as_str().unwrap();
    let device = format!("key-{}", step["device"]);
    let platform = step["platform"].as_str().unwrap();
    let t = now(rig);
    requests::create(
        &mut rig.core,
        &rig.c,
        &NewRequest {
            id,
            device_key: &device,
            device_name: "Phone",
            platform,
            email: None,
            person: step["person"].as_str(),
            created_at: t + n,
            expires_at: t + HOUR,
        },
    )
    .map_err(|e| e.to_string())?;
    if step["code"] == true {
        requests::reveal(&mut rig.core, id, "123456").map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// A call as the owner at the computer makes it: `local` on (the host sends it for a loopback owner caller, the auto-approval and the membership-gone deny). A call
/// without it is the narrow answer, tested on its own.
fn at_computer(rig: &mut Rig, method: &str, params: &Value) -> Result<Value, String> {
    let mut p = params.clone();
    if let Some(o) = p.as_object_mut() {
        o.insert("local".into(), true.into());
    }
    rig.call(method, &p)
}

fn ids(rig: &mut Rig, params: &Value) -> Vec<String> {
    at_computer(rig, "people.requests", params).unwrap()["requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["requestId"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn the_parity_script_gives_the_answers_the_host_gives() {
    let script: Value = serde_json::from_str(SCRIPT).unwrap();
    for scenario in script["scenarios"].as_array().unwrap() {
        let name = scenario["name"].as_str().unwrap();
        let mut rig = Rig::new(&world());
        for (n, step) in scenario["steps"].as_array().unwrap().iter().enumerate() {
            let at = format!("{name}: step {n} {step}");
            if let Some(f) = step.get("file") {
                file(&mut rig, f, i64::try_from(n).unwrap())
                    .unwrap_or_else(|e| panic!("{at}: {e}"));
            } else if let Some(p) = step["suspend"].as_str() {
                at_computer(&mut rig, "people.suspend", &json!({ "id": p })).unwrap();
            } else if let Some(p) = step["undo"].as_str() {
                at_computer(&mut rig, "people.undo", &json!({ "id": p })).unwrap();
            } else if let Some(p) = step["membership_gone"].as_str().or(step["sync"].as_str()) {
                // The cloud's event is the host's: it removes the membership and, on its next membership sync, denies
                // the pending requests of a person with no membership (`cloud::deny_without_membership`), here through
                // `people.deny`. The core cannot see the event itself. After a suspension the sync does the same, and
                // the list no longer shows those rows, so they are found in the table.
                let waiting: Vec<String> = {
                    let mut q = rig
                        .core
                        .conn()
                        .prepare("SELECT id FROM access_requests WHERE person = ?1 AND status = 'pending'")
                        .unwrap();
                    q.query_map([p], |r| r.get(0))
                        .unwrap()
                        .map(Result::unwrap)
                        .collect()
                };
                for id in waiting {
                    at_computer(&mut rig, "people.deny", &json!({ "id": id })).unwrap();
                }
            } else if let Some(id) = step["expire"].as_str() {
                // the cloud stops listing it; here the clock passes its time
                rig.core
                    .conn()
                    .execute(
                        "UPDATE access_requests SET expires_at = ?2 WHERE id = ?1",
                        rusqlite::params![id, now(&rig) - 1],
                    )
                    .unwrap();
            } else if let Some(want) = step.get("list") {
                assert_eq!(&json!(ids(&mut rig, &json!({}))), want, "{at}");
            } else {
                let (method, id) = if let Some(id) = step["approve"].as_str() {
                    ("people.approve", id)
                } else {
                    ("people.deny", step["deny"].as_str().unwrap())
                };
                let got = at_computer(&mut rig, method, &json!({ "id": id }));
                let expect = &step["expect"];
                match (&expect["err"], got) {
                    (Value::String(want), Err(e)) => assert_eq!(&e, want, "{at}"),
                    (Value::Null, Ok(v)) => {
                        if let Some(scopes) = expect.get("scopes") {
                            assert_eq!(&v["device"]["scopes"], scopes, "{at}");
                        }
                    }
                    (want, got) => panic!("{at}: wanted {want}, got {got:?}"),
                }
            }
        }
    }
}

fn arrive(rig: &mut Rig, id: &str, person: Option<&str>, code: bool) {
    let mut step = json!({"id": id, "device": id.len(), "platform": "ios", "code": code});
    step["person"] = person.map_or(Value::Null, Into::into);
    file(rig, &step, 0).unwrap();
}

#[test]
fn a_list_has_the_hosts_shape_and_the_approval_names_the_device_to_add() {
    let mut rig = Rig::new(&world());
    let t = now(&rig);
    requests::create(
        &mut rig.core,
        &rig.c,
        &NewRequest {
            id: "req_1",
            device_key: "S0VZ",
            device_name: "Sam's iPhone",
            platform: "ios",
            email: Some("sam@example.com"),
            person: Some("sam"),
            created_at: t,
            expires_at: t + HOUR,
        },
    )
    .unwrap();
    assert_eq!(
        at_computer(&mut rig, "people.requests", &json!({})).unwrap(),
        json!({"requests": [{
            "requestId": "req_1", "deviceKey": "S0VZ", "deviceName": "Sam's iPhone", "platform": "ios",
            "email": "sam@example.com", "personId": "sam", "code": null, "createdAt": t, "expiresAt": t + HOUR,
        }]})
    );
    requests::reveal(&mut rig.core, "req_1", "246810").unwrap();
    assert_eq!(
        at_computer(&mut rig, "people.requests", &json!({})).unwrap()["requests"][0]["code"],
        "246810"
    );
    assert_eq!(
        at_computer(&mut rig, "people.approve", &json!({"id": "req_1"})).unwrap(),
        json!({"requestId": "req_1", "status": "approved", "device": {
            "key": "S0VZ", "name": "Sam's iPhone", "platform": "ios", "personId": "sam", "scopes": ["control"],
        }})
    );
    assert_eq!(
        at_computer(&mut rig, "people.deny", &json!({"id": "req_1"})).unwrap_err(),
        "unknown request"
    );
}

#[test]
fn the_owners_other_devices_answer_members_requests_only() {
    let mut rig = Rig::new(&world());
    arrive(&mut rig, "mine", None, true);
    arrive(&mut rig, "sams", Some("sam"), true);
    let only = json!({"members": true});
    assert_eq!(ids(&mut rig, &only), ["sams"]);
    assert_eq!(ids(&mut rig, &json!({})), ["mine", "sams"]);
    let forbidden = "This can only be done at the computer itself.";
    for id in ["mine", "nope"] {
        let p = json!({"id": id, "members": true});
        assert_eq!(
            at_computer(&mut rig, "people.approve", &p).unwrap_err(),
            forbidden,
            "{id}"
        );
        assert_eq!(
            at_computer(&mut rig, "people.deny", &p).unwrap_err(),
            forbidden,
            "{id}"
        );
    }
    assert_eq!(
        requests::status_of(rig.core.conn(), "mine").as_deref(),
        Some("pending"),
        "a refusal changes nothing"
    );
    at_computer(
        &mut rig,
        "people.approve",
        &json!({"id": "sams", "members": true}),
    )
    .unwrap();
    // once decided it is not a member's request any more: the host's `is_member_request` says no too
    assert_eq!(
        at_computer(
            &mut rig,
            "people.deny",
            &json!({"id": "sams", "members": true})
        )
        .unwrap_err(),
        forbidden
    );
}

#[test]
fn a_refusal_that_ends_the_request_is_kept() {
    let mut rig = Rig::new(&world());
    arrive(&mut rig, "w", None, true);
    rig.core
        .conn()
        .execute(
            "UPDATE access_requests SET platform = 'web' WHERE id = 'w'",
            [],
        )
        .unwrap();
    arrive(&mut rig, "ghosts", Some("sam"), true);
    at_computer(&mut rig, "people.removePerson", &json!({"id": "sam"})).unwrap();
    for id in ["w", "ghosts"] {
        at_computer(&mut rig, "people.approve", &json!({ "id": id })).unwrap_err();
        assert_eq!(
            requests::status_of(rig.core.conn(), id).as_deref(),
            Some("denied"),
            "{id}"
        );
    }
    // a refusal that leaves it waiting changes nothing
    arrive(&mut rig, "early", None, false);
    at_computer(&mut rig, "people.approve", &json!({"id": "early"})).unwrap_err();
    assert_eq!(
        requests::status_of(rig.core.conn(), "early").as_deref(),
        Some("pending")
    );
}

#[test]
fn a_person_undone_after_a_suspension_can_have_their_request_approved_again() {
    let mut rig = Rig::new(&world());
    arrive(&mut rig, "r", Some("sam"), true);
    at_computer(&mut rig, "people.suspend", &json!({"id": "sam"})).unwrap();
    at_computer(&mut rig, "people.undo", &json!({"id": "sam"})).unwrap();
    assert!(at_computer(&mut rig, "people.approve", &json!({"id": "r"})).is_ok());
}

#[test]
fn filing_has_the_rules_of_the_hosts_sync() {
    let mut rig = Rig::new(&world());
    let mut step =
        json!({"id": "a", "device": 1, "platform": "ios", "person": "ghost2", "code": false});
    // someone kith does not know is not a member: the host denies such a request on the cloud
    assert!(
        file(&mut rig, &step, 0)
            .unwrap_err()
            .contains("not a member")
    );
    step["person"] = Value::Null;
    file(&mut rig, &step, 0).unwrap();
    // one unrevealed request per device: the next waits its turn
    step["id"] = "b".into();
    assert!(
        file(&mut rig, &step, 1)
            .unwrap_err()
            .contains("waiting for its code")
    );
    requests::reveal(&mut rig.core, "a", "111111").unwrap();
    file(&mut rig, &step, 1).unwrap();
    // an id the cloud lists again is not filed twice, even after it was decided
    at_computer(&mut rig, "people.deny", &json!({"id": "b"})).unwrap();
    step["device"] = 9.into();
    assert!(
        file(&mut rig, &step, 2)
            .unwrap_err()
            .contains("already known")
    );
    // a code is revealed once
    assert!(requests::reveal(&mut rig.core, "a", "222222").is_err());
    assert_eq!(
        at_computer(&mut rig, "people.requests", &json!({})).unwrap()["requests"][0]["code"],
        "111111"
    );
}

#[test]
fn old_decided_requests_are_forgotten_when_the_next_one_is_filed() {
    let mut rig = Rig::new(&world());
    arrive(&mut rig, "old", None, true);
    at_computer(&mut rig, "people.deny", &json!({"id": "old"})).unwrap();
    rig.core
        .conn()
        .execute(
            "UPDATE access_requests SET decided_at = decided_at - ?1",
            [8 * 24 * HOUR],
        )
        .unwrap();
    arrive(&mut rig, "new", None, true);
    assert_eq!(requests::status_of(rig.core.conn(), "old"), None);
    assert_eq!(
        requests::status_of(rig.core.conn(), "new").as_deref(),
        Some("pending")
    );
}

#[test]
fn two_devices_answering_at_once_one_wins() {
    for (first, second) in [(true, true), (true, false), (false, true), (false, false)] {
        let mut rig = Rig::new(&world());
        arrive(&mut rig, "r", Some("sam"), true);
        let path = rig.core.conn().path().unwrap().to_owned();
        let mut other = Connection::open(path).unwrap();
        other
            .busy_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let mut me = Connection::open(rig.core.conn().path().unwrap()).unwrap();
        me.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
        let a = requests::decide_on(&mut me, &rig.c, "r", first);
        let b = requests::decide_on(&mut other, &rig.c, "r", second);
        assert!(a.is_ok(), "{first} {second}");
        assert_eq!(
            b.unwrap_err().to_string(),
            "authority: unknown request",
            "{first} {second}"
        );
        assert_eq!(
            requests::status_of(&me, "r").as_deref(),
            Some(if first { "approved" } else { "denied" }),
            "the first answer stands"
        );
    }
}

#[test]
fn the_rows_are_owner_only_experimental_and_read_the_clock() {
    let mut rig = Rig::new(&world());
    arrive(&mut rig, "r", None, true);
    // the same request, a minute after it ran out
    rig.c.now_ms += 7_200_000.0;
    assert_eq!(ids(&mut rig, &json!({})), Vec::<String>::new());
    assert_eq!(
        at_computer(&mut rig, "people.approve", &json!({"id": "r"})).unwrap_err(),
        "unknown request"
    );
    assert_eq!(
        requests::status_of(rig.core.conn(), "r").as_deref(),
        Some("pending"),
        "expired is read from the clock, not stored"
    );
}

/// Puts a request naming `person` straight into the table, past `create`'s refusal (a row a bug or an older core left).
fn plant(rig: &mut Rig, id: &str, person: &str) {
    let t = now(rig);
    rig.core
        .conn()
        .execute(
            "INSERT INTO access_requests(id, device_key, device_name, platform, person, code, status, created_at, expires_at) \
             VALUES (?1, ?1, 'Phone', 'ios', ?2, '123456', 'pending', ?3, ?4)",
            rusqlite::params![id, person, t, t + HOUR],
        )
        .unwrap();
}

const NOT_A_MEMBER: &str = "That account is no longer a member of this computer.";

#[test]
fn a_bot_is_never_a_member_device() {
    let mut rig = Rig::new(&world());
    let step = json!({"id": "b", "device": 1, "platform": "ios", "person": "bot-a", "code": true});
    assert!(
        file(&mut rig, &step, 0)
            .unwrap_err()
            .contains("not a member")
    );
    plant(&mut rig, "b", "bot-a");
    assert_eq!(
        at_computer(&mut rig, "people.approve", &json!({"id": "b"})).unwrap_err(),
        NOT_A_MEMBER
    );
    assert_eq!(
        requests::status_of(rig.core.conn(), "b").as_deref(),
        Some("denied")
    );
}

#[test]
fn the_owners_own_id_is_never_a_member_device() {
    let mut rig = Rig::new(&world());
    let step = json!({"id": "o", "device": 1, "platform": "ios", "person": "kai", "code": true});
    assert!(
        file(&mut rig, &step, 0)
            .unwrap_err()
            .contains("not a member")
    );
    // a row that names the owner, answered from the computer and from another of the owner's devices
    for (id, members) in [("o1", false), ("o2", true)] {
        plant(&mut rig, id, "kai");
        assert_eq!(
            at_computer(
                &mut rig,
                "people.approve",
                &json!({"id": id, "members": members})
            )
            .unwrap_err(),
            NOT_A_MEMBER,
            "members: {members}"
        );
        assert_eq!(
            requests::status_of(rig.core.conn(), id).as_deref(),
            Some("denied")
        );
    }
}

#[test]
fn without_local_only_members_requests_are_answered() {
    let mut rig = Rig::new(&world());
    arrive(&mut rig, "mine", None, true);
    arrive(&mut rig, "sams", Some("sam"), true);
    // the narrow answer is the default: the owner's own device is neither listed nor decidable
    let list = rig.call("people.requests", &json!({})).unwrap();
    assert_eq!(list["requests"].as_array().unwrap().len(), 1);
    assert_eq!(list["requests"][0]["requestId"], "sams");
    for m in ["people.approve", "people.deny"] {
        assert_eq!(
            rig.call(m, &json!({"id": "mine"})).unwrap_err(),
            "This can only be done at the computer itself.",
            "{m}"
        );
    }
    assert_eq!(
        requests::status_of(rig.core.conn(), "mine").as_deref(),
        Some("pending")
    );
    // `members` stays narrow even next to `local`
    let both = rig
        .call("people.requests", &json!({"members": true, "local": true}))
        .unwrap();
    assert_eq!(both["requests"].as_array().unwrap().len(), 1);
    // a member's request can be answered without it
    assert!(rig.call("people.approve", &json!({"id": "sams"})).is_ok());
}

#[test]
fn filing_by_the_host_is_idempotent_and_adds_the_code_when_it_comes() {
    let mut rig = Rig::new(&world());
    let t = now(&rig);
    let file = json!({
        "id": "r", "deviceKey": "S0VZ", "deviceName": "Phone", "platform": "ios", "person": "sam",
        "createdAt": t, "expiresAt": t + HOUR,
    });
    assert_eq!(
        rig.call("people.fileRequest", &file).unwrap()["filed"],
        true
    );
    assert_eq!(
        rig.call("people.fileRequest", &file).unwrap()["filed"],
        false
    );
    let mut shown = file.clone();
    shown["code"] = "654321".into();
    rig.call("people.fileRequest", &shown).unwrap();
    assert_eq!(
        at_computer(&mut rig, "people.requests", &json!({})).unwrap()["requests"][0]["code"],
        "654321"
    );
    // a known id filed with anything different is refused, and the row stays as it was
    for (field, other) in [
        ("code", "000000"),
        ("deviceKey", "T1RIRVI="),
        ("person", "sasha"),
        ("platform", "android"),
        ("deviceName", "Other"),
    ] {
        let mut changed = shown.clone();
        changed[field] = other.into();
        assert!(
            rig.call("people.fileRequest", &changed)
                .unwrap_err()
                .contains("already known as a different request"),
            "{field}"
        );
    }
    let mut no_person = shown.clone();
    no_person["person"] = Value::Null;
    assert!(rig.call("people.fileRequest", &no_person).is_err());
    assert_eq!(
        at_computer(&mut rig, "people.requests", &json!({})).unwrap()["requests"][0]["code"],
        "654321"
    );
    // a request already decided stays decided when the host files it again
    at_computer(&mut rig, "people.deny", &json!({"id": "r"})).unwrap();
    assert_eq!(
        rig.call("people.fileRequest", &shown).unwrap()["filed"],
        false
    );
    assert_eq!(
        requests::status_of(rig.core.conn(), "r").as_deref(),
        Some("denied")
    );
    // a member who is not one is refused, a request without a device key is a usage error
    let mut ghost = file.clone();
    ghost["id"] = "g".into();
    ghost["person"] = "nobody".into();
    assert!(
        rig.call("people.fileRequest", &ghost)
            .unwrap_err()
            .contains("not a member")
    );
    assert!(rig.call("people.fileRequest", &json!({"id": "x"})).is_err());
}
