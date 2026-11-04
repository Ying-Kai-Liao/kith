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
            .call_run(&me, "run.projectId", &json!({"args": []}))
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
