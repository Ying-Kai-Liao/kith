//! Run tokens and revoke (M2-13), at the core: a suspension or a removal ends the tokens of the person and their
//! bots; the run door refuses a revoked person even for a token that resolved a moment before; undo revives nothing.
#![allow(clippy::unwrap_used)]

use serde_json::{Value, json};

use super::writes::{Rig, base};
use crate::authority::call::call_with;
use crate::runs::{RunIdentity, RunTokens};

fn who(person: &str) -> RunIdentity {
    RunIdentity {
        run_id: String::new(),
        agent: "echo".into(),
        person: person.into(),
        conversation: "t1".into(),
        claim: None,
        room: None,
        project: Some("alpha".into()),
        members: Vec::new(),
    }
}

fn yes(_: u32, _: u32) -> bool {
    true
}

#[allow(clippy::unnecessary_wraps)]
fn started(_: u32) -> Option<String> {
    Some("t0".into())
}

/// A table with one bound token for each of these people, and the tokens in the same order.
fn table(people: &[&str]) -> (RunTokens, Vec<String>) {
    let t = RunTokens::with(yes, started);
    let tokens = people
        .iter()
        .map(|p| {
            let (token, id) = t.mint(who(p)).unwrap();
            assert!(t.bind(&id.run_id, 4242));
            token
        })
        .collect();
    (t, tokens)
}

fn live(t: &RunTokens, token: &str) -> bool {
    t.resolve(token, Some(4242)).is_some()
}

fn write(rig: &mut Rig, tokens: &RunTokens, method: &str, params: &Value) -> usize {
    let answer = call_with(&mut rig.core, &rig.c, method, params).unwrap();
    tokens.after_authority_write(method, params, &answer)
}

#[test]
fn a_suspension_ends_the_tokens_of_the_person_and_their_bots_and_no_others() {
    let mut rig = Rig::new(&base());
    let (t, tok) = table(&["sam", "bot-a", "amy", "sam"]);
    assert!(tok.iter().all(|k| live(&t, k)));
    // sam has two runs, one bot run: three tokens end, amy's stays.
    assert_eq!(
        write(&mut rig, &t, "people.suspend", &json!({"id": "sam"})),
        3
    );
    assert!(!live(&t, &tok[0]) && !live(&t, &tok[1]) && !live(&t, &tok[3]));
    assert!(live(&t, &tok[2]));
    assert_eq!(t.len(), 1);
}

#[test]
fn undo_revives_no_token_and_a_new_run_mints_a_new_one() {
    let mut rig = Rig::new(&base());
    let (t, tok) = table(&["sam"]);
    write(&mut rig, &t, "people.suspend", &json!({"id": "sam"}));
    assert_eq!(write(&mut rig, &t, "people.undo", &json!({"id": "sam"})), 0);
    assert!(!live(&t, &tok[0]));
    let (fresh, id) = t.mint(who("sam")).unwrap();
    assert!(t.bind(&id.run_id, 4242));
    assert!(live(&t, &fresh) && fresh != tok[0]);
}

#[test]
fn removing_a_person_or_a_bot_ends_their_tokens() {
    let mut rig = Rig::new(&base());
    let (t, tok) = table(&["sam", "bot-a", "amy"]);
    assert_eq!(
        write(&mut rig, &t, "people.removeBot", &json!({"id": "bot-a"})),
        1
    );
    assert!(!live(&t, &tok[1]) && live(&t, &tok[0]));
    assert_eq!(
        write(&mut rig, &t, "people.removePerson", &json!({"id": "amy"})),
        1
    );
    assert!(!live(&t, &tok[2]) && live(&t, &tok[0]));
    // A write that is not a revoke touches nothing.
    assert_eq!(
        write(&mut rig, &t, "people.addPerson", &json!({"id": "zed"})),
        0
    );
    assert!(live(&t, &tok[0]));
}

#[test]
fn a_token_that_resolved_before_the_revoke_is_refused_at_the_run_door() {
    let mut rig = Rig::new(&base());
    let me = who("bot-a");
    let ask = |rig: &Rig| {
        rig.core
            .call_run(&me, "run.projectId", &json!({"args": []}), true)
    };
    assert_eq!(ask(&rig).unwrap()["stdout"], "alpha\n");
    // The core call that revokes sam; the table is not told here, as if the request had resolved just before.
    call_with(
        &mut rig.core,
        &rig.c,
        "people.suspend",
        &json!({"id": "sam"}),
    )
    .unwrap();
    assert!(matches!(ask(&rig), Err(crate::error::Error::NotAllowed(_))));
    call_with(&mut rig.core, &rig.c, "people.undo", &json!({"id": "sam"})).unwrap();
    assert_eq!(ask(&rig).unwrap()["stdout"], "alpha\n");
}

#[test]
fn a_removed_person_is_refused_at_the_run_door_and_a_ghost_never_mints() {
    let mut rig = Rig::new(&base());
    let amy = who("amy");
    let ask = |rig: &Rig| {
        rig.core
            .call_run(&amy, "run.projectId", &json!({"args": []}), true)
    };
    assert!(ask(&rig).is_ok());
    call_with(
        &mut rig.core,
        &rig.c,
        "people.removePerson",
        &json!({"id": "amy"}),
    )
    .unwrap();
    assert!(matches!(ask(&rig), Err(crate::error::Error::NotAllowed(_))));
    let allowed = |rig: &Rig, id: &str, switched| {
        crate::call::run_identity_allowed(rig.core.conn(), &who(id), switched, "run.mint")
    };
    assert!(
        allowed(&rig, "ghost", true).is_err(),
        "an id that was never a person, once the authority is the core's"
    );
    assert!(allowed(&rig, "sam", true).is_ok());
    // Before the switch core.db knows only the imported people: a person invited since is let through, a removed one
    // (retired) and a revoked one are not.
    assert!(
        allowed(&rig, "ghost", false).is_ok(),
        "invited after the import"
    );
    assert!(allowed(&rig, "amy", false).is_err(), "removed: retired");
}

#[test]
fn a_room_persons_basis_decides_and_a_revoked_member_ends_the_token() {
    let mut rig = Rig::new(&base());
    let room = |person: &str, room: &str, members: &[&str]| RunIdentity {
        room: Some(room.into()),
        members: members.iter().map(|m| (*m).to_owned()).collect(),
        ..who(person)
    };
    let allowed = |rig: &Rig, r: &RunIdentity| {
        crate::call::run_identity_allowed(rig.core.conn(), r, true, "run.mint")
    };
    assert!(allowed(&rig, &room("room-grp1-9", "grp1", &["sam", "bot-a"])).is_ok());
    assert!(
        allowed(&rig, &room("room-grp1-9", "grp1", &[])).is_err(),
        "a room person is for somebody"
    );
    assert!(
        allowed(&rig, &room("room-grp2-9", "grp1", &["sam"])).is_err(),
        "another room's person"
    );
    assert!(
        allowed(&rig, &room("sam", "grp1", &["sam"])).is_err(),
        "a basis is a room person's only"
    );
    assert!(
        allowed(&rig, &room("room-grp1-9", "grp1", &["sam", "ghost"])).is_err(),
        "every member counts"
    );
    // A suspended member ends the room token (the tokens of a revoke are matched on the basis too).
    let t = RunTokens::with(yes, |_| Some("s".into()));
    let (_, minted) = t
        .mint(room("room-grp1-9", "grp1", &["sam", "bot-a"]))
        .unwrap();
    call_with(
        &mut rig.core,
        &rig.c,
        "people.suspend",
        &json!({"id": "sam"}),
    )
    .unwrap();
    assert!(allowed(&rig, &minted).is_err());
    assert_eq!(t.end_people(&["sam"]), 1);
    assert!(!t.is_live(&minted.run_id));
}

#[test]
fn removing_an_unused_person_still_ends_the_named_id_and_a_token_is_live_until_ended() {
    let mut rig = Rig::new(&base());
    let (t, tok) = table(&["amy"]);
    let run_id = t.mint(who("amy")).unwrap().1.run_id;
    assert!(t.is_live(&run_id));
    // `--unused` answers no ids: the id in the params is what ends.
    assert_eq!(
        write(
            &mut rig,
            &t,
            "people.removePerson",
            &json!({"id": "amy", "unused": true})
        ),
        2
    );
    assert!(!live(&t, &tok[0]) && !t.is_live(&run_id));
}
