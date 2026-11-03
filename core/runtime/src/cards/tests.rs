#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};

use serde_json::{Value, json};

use super::{
    Answered, Card, Claim, NewCard, Raised, Reply, State, answer, call, card, claim, listing,
    mark_live, park_run, raise, release, saved, sweep, withdraw,
};
use crate::db::Core;
use crate::error::Error;
use crate::gate::Liveness;
use crate::layout::Layout;
use crate::tests::{machine, open};

fn core() -> (tempfile::TempDir, Layout, Core) {
    let (t, m) = machine();
    let l = Layout::new(&m);
    let c = open(&l);
    (t, l, c)
}

/// No start time can be read and no process is ever reported gone: a claim is never dropped by it.
struct Nobody;

impl Liveness for Nobody {
    fn start(&self, _: u32) -> Option<String> {
        None
    }
    fn gone(&self, _: u32) -> bool {
        false
    }
}

/// Start times unreadable, and the system says every process is gone.
struct Gone;

impl Liveness for Gone {
    fn start(&self, _: u32) -> Option<String> {
        None
    }
    fn gone(&self, _: u32) -> bool {
        true
    }
}

/// Every pid reads this start time.
struct Started(&'static str);

impl Liveness for Started {
    fn start(&self, _: u32) -> Option<String> {
        Some(self.0.to_owned())
    }
    fn gone(&self, _: u32) -> bool {
        false
    }
}

const T0: i64 = 1_000;
const DEADLINE: i64 = 10_000;

fn ask(session: &str, resource: &str) -> NewCard {
    NewCard {
        session: session.into(),
        source: "run",
        bot_id: None,
        run_id: Some("r1".into()),
        person: String::new(),
        agent: "echo".into(),
        answerable_by: None,
        in_grants: false,
        tool: "Bash".into(),
        input: "{}".into(),
        resource: resource.into(),
        shown: resource.into(),
        deadline: DEADLINE,
        reply_to: None,
        asked_by: None,
        options: None,
    }
}

fn raised(c: &mut Core, n: &NewCard) -> Card {
    match raise(c, n, T0, &Nobody).unwrap() {
        Raised::New(card) => card,
        other => panic!("{other:?}"),
    }
}

fn new(c: &mut Core, session: &str, resource: &str) -> Card {
    raised(c, &ask(session, resource))
}

fn reply(id: &str, allow: bool) -> Reply<'_> {
    Reply {
        id,
        allow,
        answer: if allow { "allow" } else { "deny" },
        by: "owner",
        spend: false,
    }
}

fn yes(c: &mut Core, id: &str, now: i64) -> Answered {
    answer(c, &reply(id, true), now, &Nobody).unwrap()
}

fn no(c: &mut Core, id: &str, now: i64) -> Answered {
    answer(c, &reply(id, false), now, &Nobody).unwrap()
}

fn stored(c: &mut Core, id: &str) -> Card {
    card(c, id).unwrap().expect("card")
}

fn state(c: &mut Core, id: &str) -> State {
    stored(c, id).state
}

fn hold(holder: &str, until: i64) -> Claim<'_> {
    Claim {
        holder,
        pid: None,
        until,
    }
}

// ---- raise

#[test]
fn a_new_ask_is_pending_and_the_same_ask_joins_it() {
    let (_t, _l, mut c) = core();
    let first = new(&mut c, "s1", "/etc/hosts");
    assert_eq!(first.state, State::Pending);
    assert_eq!(first.approver, "owner");
    assert_eq!(first.scope, "once");
    let again = raise(&mut c, &ask("s1", "/etc/hosts"), T0 + 1, &Nobody).unwrap();
    assert_eq!(again, Raised::Joined(first.clone()));
    // Another resource or another session is another card.
    assert_ne!(new(&mut c, "s1", "/etc/passwd").id, first.id);
    assert_ne!(new(&mut c, "s2", "/etc/hosts").id, first.id);
}

#[test]
fn an_expired_card_never_joins_a_new_ask() {
    let (_t, _l, mut c) = core();
    let first = new(&mut c, "s1", "r");
    let later = raise(&mut c, &ask("s1", "r"), DEADLINE + 1, &Nobody);
    // The ask's own deadline is in the past by then, which is refused; ask with a fresh one.
    assert!(matches!(later, Err(Error::BadParams(_))));
    let fresh = NewCard {
        deadline: DEADLINE + 10,
        ..ask("s1", "r")
    };
    let got = raise(&mut c, &fresh, DEADLINE + 1, &Nobody).unwrap();
    let Raised::New(second) = got else {
        panic!("{got:?}")
    };
    assert_ne!(second.id, first.id);
    assert_eq!(state(&mut c, &first.id), State::Expired);
}

#[test]
fn a_yes_that_nobody_used_is_spent_by_the_next_ask_once() {
    let (_t, _l, mut c) = core();
    let first = new(&mut c, "s1", "r");
    yes(&mut c, &first.id, T0 + 1);
    assert_eq!(state(&mut c, &first.id), State::Approved);
    let got = raise(&mut c, &ask("s1", "r"), T0 + 2, &Nobody).unwrap();
    let Raised::Granted(g) = got else {
        panic!("{got:?}")
    };
    assert_eq!((g.id.as_str(), g.state), (first.id.as_str(), State::Used));
    // Used once: the third ask is a card of its own.
    assert!(matches!(
        raise(&mut c, &ask("s1", "r"), T0 + 3, &Nobody).unwrap(),
        Raised::New(_)
    ));
}

#[test]
fn nonsense_asks_are_refused() {
    let (_t, _l, mut c) = core();
    for bad in [
        NewCard {
            session: String::new(),
            ..ask("s", "r")
        },
        NewCard {
            resource: String::new(),
            ..ask("s", "r")
        },
        NewCard {
            tool: String::new(),
            ..ask("s", "r")
        },
        NewCard {
            source: "email",
            ..ask("s", "r")
        },
        NewCard {
            deadline: T0,
            ..ask("s", "r")
        },
    ] {
        assert!(
            matches!(raise(&mut c, &bad, T0, &Nobody), Err(Error::BadParams(_))),
            "{bad:?}"
        );
    }
    assert_eq!(listing(&mut c, 10).unwrap().0.len(), 0);
}

// ---- answer

#[test]
fn a_yes_is_a_once_grant_and_a_no_is_not() {
    let (_t, _l, mut c) = core();
    let a = new(&mut c, "s1", "a");
    let Answered::Applied {
        card: got,
        was_live,
    } = yes(&mut c, &a.id, T0 + 5)
    else {
        panic!("not applied")
    };
    assert!(was_live, "a pending card's asker is up");
    assert_eq!(got.state, State::Approved);
    assert_eq!(
        (
            got.allowed,
            got.answer.as_deref(),
            got.answered_by.as_deref()
        ),
        (Some(true), Some("allow"), Some("owner"))
    );
    assert_eq!(
        (got.answered_at, got.granted_at),
        (Some(T0 + 5), Some(T0 + 5))
    );
    assert!(!got.answered_parked);

    let b = new(&mut c, "s1", "b");
    let Answered::Applied { card: got, .. } = no(&mut c, &b.id, T0 + 6) else {
        panic!("not applied")
    };
    assert_eq!(got.state, State::Denied);
    assert_eq!((got.allowed, got.granted_at), (Some(false), None));
}

#[test]
fn a_parked_cards_answer_is_marked_parked_and_a_spent_yes_goes_straight_to_used() {
    let (_t, _l, mut c) = core();
    let a = new(&mut c, "s1", "a");
    assert_eq!(park_run(&mut c, "r1").unwrap().len(), 1);
    assert_eq!(state(&mut c, &a.id), State::Parked);
    let Answered::Applied {
        card: got,
        was_live,
    } = yes(&mut c, &a.id, T0 + 1)
    else {
        panic!("not applied")
    };
    assert!(!was_live);
    assert!(got.answered_parked);

    let b = new(&mut c, "s2", "b");
    mark_live(&mut c, &b.id, Some("e1"), None).unwrap();
    let r = Reply {
        spend: true,
        ..reply(&b.id, true)
    };
    let Answered::Applied {
        card: got,
        was_live,
    } = answer(&mut c, &r, T0 + 2, &Nobody).unwrap()
    else {
        panic!("not applied")
    };
    assert!(was_live);
    assert_eq!(got.state, State::Used);
}

#[test]
fn a_second_answer_a_closed_card_and_an_unknown_card_change_nothing() {
    let (_t, _l, mut c) = core();
    let a = new(&mut c, "s1", "a");
    yes(&mut c, &a.id, T0 + 1);
    let before = stored(&mut c, &a.id);
    assert_eq!(
        no(&mut c, &a.id, T0 + 2),
        Answered::Nothing(Some(State::Approved))
    );
    assert_eq!(
        yes(&mut c, &a.id, T0 + 3),
        Answered::Nothing(Some(State::Approved))
    );
    assert_eq!(stored(&mut c, &a.id), before, "the first answer stands");

    let w = new(&mut c, "s1", "w");
    assert!(withdraw(&mut c, &w.id).unwrap());
    assert_eq!(
        yes(&mut c, &w.id, T0 + 4),
        Answered::Nothing(Some(State::Withdrawn))
    );
    assert_eq!(state(&mut c, &w.id), State::Withdrawn);

    let d = new(&mut c, "s1", "d");
    no(&mut c, &d.id, T0 + 5);
    assert_eq!(
        yes(&mut c, &d.id, T0 + 6),
        Answered::Nothing(Some(State::Denied))
    );

    assert_eq!(yes(&mut c, "no-such-card", T0), Answered::Nothing(None));
}

#[test]
fn an_answer_after_the_deadline_expires_the_card_and_records_nothing() {
    let (_t, _l, mut c) = core();
    let a = new(&mut c, "s1", "a");
    assert_eq!(
        yes(&mut c, &a.id, DEADLINE + 1),
        Answered::Nothing(Some(State::Expired))
    );
    let got = stored(&mut c, &a.id);
    assert_eq!(got.state, State::Expired);
    assert_eq!(
        (got.answer, got.allowed, got.answered_at),
        (None, None, None)
    );
    // The deadline itself is still in time.
    let b = new(&mut c, "s1", "b");
    assert!(matches!(
        yes(&mut c, &b.id, DEADLINE),
        Answered::Applied { .. }
    ));
}

#[test]
fn answers_must_say_who_and_what() {
    let (_t, _l, mut c) = core();
    let a = new(&mut c, "s1", "a");
    for r in [
        Reply {
            by: "",
            ..reply(&a.id, true)
        },
        Reply {
            answer: "",
            ..reply(&a.id, true)
        },
    ] {
        assert!(matches!(
            answer(&mut c, &r, T0, &Nobody),
            Err(Error::BadParams(_))
        ));
    }
    assert_eq!(state(&mut c, &a.id), State::Pending);
}

// ---- closers

#[test]
fn withdraw_closes_an_open_card_once() {
    let (_t, _l, mut c) = core();
    let a = new(&mut c, "s1", "a");
    assert!(withdraw(&mut c, &a.id).unwrap());
    assert!(!withdraw(&mut c, &a.id).unwrap(), "already closed");
    assert!(!withdraw(&mut c, "no-such-card").unwrap());
    // An answered card is not open: a withdrawal does not undo the answer.
    let b = new(&mut c, "s1", "b");
    yes(&mut c, &b.id, T0 + 1);
    assert!(!withdraw(&mut c, &b.id).unwrap());
    assert_eq!(state(&mut c, &b.id), State::Approved);
}

#[test]
fn sweep_expires_the_open_and_the_unused_grants_past_their_deadline() {
    let (_t, _l, mut c) = core();
    let open = new(&mut c, "s1", "open");
    let granted = new(&mut c, "s1", "granted");
    yes(&mut c, &granted.id, T0 + 1);
    let denied = new(&mut c, "s1", "denied");
    no(&mut c, &denied.id, T0 + 1);
    assert!(
        sweep(&mut c, DEADLINE, &Nobody).unwrap().is_empty(),
        "not yet"
    );
    let gone = sweep(&mut c, DEADLINE + 1, &Nobody).unwrap();
    let mut ids: Vec<_> = gone.iter().map(|g| g.id.clone()).collect();
    ids.sort();
    let mut want = vec![open.id.clone(), granted.id.clone()];
    want.sort();
    assert_eq!(ids, want);
    assert_eq!(state(&mut c, &open.id), State::Expired);
    assert_eq!(state(&mut c, &granted.id), State::Expired);
    // A no stays a no.
    assert_eq!(state(&mut c, &denied.id), State::Denied);
    assert!(sweep(&mut c, DEADLINE + 2, &Nobody).unwrap().is_empty());
}

#[test]
fn sweep_keeps_an_approved_card_whose_wake_is_queued() {
    let (_t, _l, mut c) = core();
    let a = new(&mut c, "s1", "a");
    yes(&mut c, &a.id, T0 + 1);
    {
        let tx = c.claim_tx().unwrap();
        tx.execute("UPDATE cards SET resumed_at = 5 WHERE id = ?1", [&a.id])
            .unwrap();
        tx.commit().unwrap();
    }
    assert!(sweep(&mut c, DEADLINE + 1, &Nobody).unwrap().is_empty());
    assert_eq!(state(&mut c, &a.id), State::Approved);
}

#[test]
fn an_answer_and_a_sweep_in_either_order_agree() {
    let (_t, _l, mut c) = core();
    // The sweep first: the late answer finds it expired and records nothing.
    let a = new(&mut c, "s1", "a");
    sweep(&mut c, DEADLINE + 1, &Nobody).unwrap();
    assert_eq!(
        yes(&mut c, &a.id, DEADLINE + 2),
        Answered::Nothing(Some(State::Expired))
    );
    assert_eq!(stored(&mut c, &a.id).answered_at, None);
    // The answer first (in time): the sweep later expires the unused grant but the record of the yes stays.
    let b = new(&mut c, "s1", "b");
    yes(&mut c, &b.id, DEADLINE);
    sweep(&mut c, DEADLINE + 1, &Nobody).unwrap();
    let got = stored(&mut c, &b.id);
    assert_eq!(
        (got.state, got.allowed, got.answered_at),
        (State::Expired, Some(true), Some(DEADLINE))
    );
}

#[test]
fn saved_spends_the_card_for_the_holder_of_the_claim() {
    let (_t, _l, mut c) = core();
    let a = new(&mut c, "s1", "a");
    let held = claim(&mut c, &a.id, &hold("w1", DEADLINE + 500), T0 + 1, &Nobody)
        .unwrap()
        .expect("claim");
    assert_eq!(held.claim_by.as_deref(), Some("w1"));
    let got = saved(
        &mut c,
        &a.id,
        "w1",
        "owner",
        T0 + 2,
        DEADLINE + 900,
        &Nobody,
    )
    .unwrap()
    .expect("saved");
    assert_eq!(got.state, State::Used);
    assert_eq!(
        (
            got.answer.as_deref(),
            got.allowed,
            got.answered_by.as_deref()
        ),
        (Some("saved"), Some(true), Some("owner"))
    );
    assert!(got.answered_parked && got.used_by_ask);
    assert_eq!(got.granted_at, Some(T0 + 2));
    assert_eq!(
        got.deadline,
        DEADLINE + 900,
        "the owed resume outlives the write"
    );
    assert_eq!(got.claim_by, None, "the claim ended with it");
    // At most once.
    assert!(
        saved(&mut c, &a.id, "w1", "owner", T0 + 3, DEADLINE, &Nobody)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        yes(&mut c, &a.id, T0 + 4),
        Answered::Nothing(Some(State::Used))
    );
}

#[test]
fn saved_needs_the_claim_and_an_open_card() {
    let (_t, _l, mut c) = core();
    let a = new(&mut c, "s1", "a");
    // No claim at all, or somebody else's.
    assert!(
        saved(&mut c, &a.id, "w1", "owner", T0, DEADLINE, &Nobody)
            .unwrap()
            .is_none()
    );
    claim(&mut c, &a.id, &hold("w1", DEADLINE), T0, &Nobody)
        .unwrap()
        .unwrap();
    assert!(
        saved(&mut c, &a.id, "w2", "owner", T0, DEADLINE, &Nobody)
            .unwrap()
            .is_none()
    );
    assert_eq!(state(&mut c, &a.id), State::Pending);
    // Withdrawn under the holder: the save finds it closed.
    assert!(withdraw(&mut c, &a.id).unwrap());
    assert!(
        saved(&mut c, &a.id, "w1", "owner", T0, DEADLINE, &Nobody)
            .unwrap()
            .is_none()
    );
    assert_eq!(state(&mut c, &a.id), State::Withdrawn);
    // A no closes it for good, too.
    let b = new(&mut c, "s1", "b");
    no(&mut c, &b.id, T0);
    assert!(
        saved(&mut c, &b.id, "w1", "owner", T0, DEADLINE, &Nobody)
            .unwrap()
            .is_none()
    );
    assert_eq!(state(&mut c, &b.id), State::Denied);
    // An empty "by" is refused.
    assert!(matches!(
        saved(&mut c, &b.id, "w1", "", T0, DEADLINE, &Nobody),
        Err(Error::BadParams(_))
    ));
}

#[test]
fn a_claim_that_ran_out_lets_the_sweep_close_the_card_and_then_nothing_is_saved() {
    let (_t, _l, mut c) = core();
    let a = new(&mut c, "s1", "a");
    claim(&mut c, &a.id, &hold("w1", DEADLINE + 100), T0, &Nobody)
        .unwrap()
        .unwrap();
    // Held: the card's own deadline passed but the claim keeps it.
    assert!(sweep(&mut c, DEADLINE + 50, &Nobody).unwrap().is_empty());
    assert_eq!(state(&mut c, &a.id), State::Pending);
    // Ran out.
    assert_eq!(sweep(&mut c, DEADLINE + 101, &Nobody).unwrap().len(), 1);
    assert!(
        saved(
            &mut c,
            &a.id,
            "w1",
            "owner",
            DEADLINE + 102,
            DEADLINE,
            &Nobody
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(state(&mut c, &a.id), State::Expired);
}

// ---- claim

#[test]
fn a_claim_is_held_by_one_and_refuses_a_second() {
    let (_t, _l, mut c) = core();
    let a = new(&mut c, "s1", "a");
    assert!(
        claim(&mut c, &a.id, &hold("w1", T0 + 100), T0, &Nobody)
            .unwrap()
            .is_some()
    );
    assert!(
        claim(&mut c, &a.id, &hold("w2", T0 + 100), T0 + 1, &Nobody)
            .unwrap()
            .is_none()
    );
    // The holder claiming again extends its own claim.
    let again = claim(&mut c, &a.id, &hold("w1", T0 + 500), T0 + 2, &Nobody)
        .unwrap()
        .unwrap();
    assert_eq!(
        (again.claim_by.as_deref(), again.claim_until),
        (Some("w1"), Some(T0 + 500))
    );
    // The card's own deadline never moved.
    assert_eq!(again.deadline, DEADLINE);
    // Only the holder releases; a stranger's release does nothing.
    assert!(!release(&mut c, &a.id, "w2", &Nobody).unwrap());
    assert!(release(&mut c, &a.id, "w1", &Nobody).unwrap());
    assert!(
        !release(&mut c, &a.id, "w1", &Nobody).unwrap(),
        "released once"
    );
    assert!(
        claim(&mut c, &a.id, &hold("w2", T0 + 100), T0 + 3, &Nobody)
            .unwrap()
            .is_some()
    );
}

#[test]
fn a_claim_that_ran_out_by_time_can_be_taken_over() {
    let (_t, _l, mut c) = core();
    let a = new(&mut c, "s1", "a");
    claim(&mut c, &a.id, &hold("w1", T0 + 100), T0, &Nobody)
        .unwrap()
        .unwrap();
    let taken = claim(&mut c, &a.id, &hold("w2", T0 + 900), T0 + 101, &Nobody)
        .unwrap()
        .unwrap();
    assert_eq!(taken.claim_by.as_deref(), Some("w2"));
    // The first holder lost it: it cannot save, and its release does nothing.
    assert!(
        saved(&mut c, &a.id, "w1", "owner", T0 + 102, DEADLINE, &Nobody)
            .unwrap()
            .is_none()
    );
    assert!(!release(&mut c, &a.id, "w1", &Nobody).unwrap());
}

#[test]
fn only_an_open_card_before_its_deadline_is_claimed() {
    let (_t, _l, mut c) = core();
    let a = new(&mut c, "s1", "a");
    assert!(
        claim(
            &mut c,
            &a.id,
            &hold("w1", DEADLINE + 10),
            DEADLINE + 1,
            &Nobody
        )
        .unwrap()
        .is_none()
    );
    let b = new(&mut c, "s1", "b");
    yes(&mut c, &b.id, T0 + 1);
    assert!(
        claim(&mut c, &b.id, &hold("w1", T0 + 100), T0 + 2, &Nobody)
            .unwrap()
            .is_none()
    );
    assert!(
        claim(&mut c, "no-such-card", &hold("w1", T0 + 100), T0, &Nobody)
            .unwrap()
            .is_none()
    );
    // Nonsense: no holder, a claim that ends before it begins.
    let d = new(&mut c, "s1", "d");
    assert!(matches!(
        claim(&mut c, &d.id, &hold("", T0 + 100), T0, &Nobody),
        Err(Error::BadParams(_))
    ));
    assert!(matches!(
        claim(&mut c, &d.id, &hold("w1", T0), T0, &Nobody),
        Err(Error::BadParams(_))
    ));
}

#[test]
fn nobody_else_answers_while_the_claim_is_held() {
    let (_t, _l, mut c) = core();
    let a = new(&mut c, "s1", "a");
    claim(&mut c, &a.id, &hold("w1", T0 + 100), T0, &Nobody)
        .unwrap()
        .unwrap();
    assert_eq!(yes(&mut c, &a.id, T0 + 1), Answered::Claimed("w1".into()));
    assert_eq!(no(&mut c, &a.id, T0 + 1), Answered::Claimed("w1".into()));
    assert_eq!(state(&mut c, &a.id), State::Pending);
    // After the holder lets go (the write failed) the card is answerable again.
    release(&mut c, &a.id, "w1", &Nobody).unwrap();
    assert!(matches!(
        yes(&mut c, &a.id, T0 + 2),
        Answered::Applied { .. }
    ));
    // After the claim's time the card is answerable by anyone.
    let b = new(&mut c, "s1", "b");
    claim(&mut c, &b.id, &hold("w1", T0 + 100), T0, &Nobody)
        .unwrap()
        .unwrap();
    assert!(matches!(
        yes(&mut c, &b.id, T0 + 101),
        Answered::Applied { .. }
    ));
}

#[test]
fn a_claim_whose_process_is_gone_is_dropped_and_an_unreadable_one_is_kept() {
    let (_t, _l, mut c) = core();
    let a = new(&mut c, "s1", "a");
    let with_pid = Claim {
        holder: "w1",
        pid: Some(4242),
        until: T0 + 100_000,
    };
    claim(&mut c, &a.id, &with_pid, T0, &Started("t1"))
        .unwrap()
        .unwrap();
    // The same start time: alive. A failed probe: kept too.
    assert!(
        claim(&mut c, &a.id, &hold("w2", T0 + 9), T0 + 1, &Started("t1"))
            .unwrap()
            .is_none()
    );
    assert!(
        claim(&mut c, &a.id, &hold("w2", T0 + 9), T0 + 1, &Nobody)
            .unwrap()
            .is_none()
    );
    assert_eq!(yes(&mut c, &a.id, T0 + 2), Answered::Claimed("w1".into()));
    // The pid has another start time now: reused, so the holder is dead and the card is free.
    assert!(matches!(
        answer(&mut c, &reply(&a.id, true), T0 + 3, &Started("t2")).unwrap(),
        Answered::Applied { .. }
    ));

    // Only ESRCH-style death frees a claim whose start time was never read.
    let b = new(&mut c, "s1", "b");
    claim(&mut c, &b.id, &with_pid, T0, &Nobody)
        .unwrap()
        .unwrap();
    assert_eq!(yes(&mut c, &b.id, T0 + 1), Answered::Claimed("w1".into()));
    assert!(matches!(
        answer(&mut c, &reply(&b.id, true), T0 + 2, &Gone).unwrap(),
        Answered::Applied { .. }
    ));

    // A dead holder's claim does not keep the clock off the card either.
    let d = new(&mut c, "s1", "d");
    claim(&mut c, &d.id, &with_pid, T0, &Nobody)
        .unwrap()
        .unwrap();
    sweep(&mut c, DEADLINE + 1, &Nobody).unwrap();
    assert_eq!(
        state(&mut c, &d.id),
        State::Pending,
        "held by a live process"
    );
    sweep(&mut c, DEADLINE + 1, &Gone).unwrap();
    assert_eq!(state(&mut c, &d.id), State::Expired);
}

#[test]
fn threads_claiming_one_card_leave_exactly_one_holder() {
    for round in 0..20 {
        let (_t, _l, mut c) = core();
        let a = new(&mut c, "s1", &format!("r{round}"));
        let c = Arc::new(Mutex::new(c));
        let go = Arc::new(Barrier::new(8));
        let wins = Arc::new(AtomicUsize::new(0));
        let workers: Vec<_> = (0..8)
            .map(|n| {
                let (c, go, wins, id) = (c.clone(), go.clone(), wins.clone(), a.id.clone());
                std::thread::spawn(move || {
                    let me = format!("w{n}");
                    go.wait();
                    let got = claim(
                        &mut c.lock().unwrap(),
                        &id,
                        &hold(&me, DEADLINE),
                        T0,
                        &Nobody,
                    )
                    .unwrap();
                    if got.is_some() {
                        wins.fetch_add(1, Ordering::SeqCst);
                    }
                    got.map(|_| me)
                })
            })
            .collect();
        let winners: Vec<String> = workers
            .into_iter()
            .filter_map(|w| w.join().expect("worker"))
            .collect();
        assert_eq!(wins.load(Ordering::SeqCst), 1, "round {round}: {winners:?}");
        let held = stored(&mut c.lock().unwrap(), &a.id);
        assert_eq!(held.claim_by.as_deref(), Some(winners[0].as_str()));
    }
}

#[test]
fn threads_answering_one_card_record_exactly_one_answer() {
    for round in 0..20 {
        let (_t, _l, mut c) = core();
        let a = new(&mut c, "s1", &format!("r{round}"));
        let c = Arc::new(Mutex::new(c));
        let go = Arc::new(Barrier::new(6));
        let workers: Vec<_> = (0..6)
            .map(|n| {
                let (c, go, id) = (c.clone(), go.clone(), a.id.clone());
                std::thread::spawn(move || {
                    go.wait();
                    let r = Reply {
                        id: &id,
                        allow: n % 2 == 0,
                        answer: if n % 2 == 0 { "allow" } else { "deny" },
                        by: "owner",
                        spend: false,
                    };
                    matches!(
                        answer(&mut c.lock().unwrap(), &r, T0, &Nobody).unwrap(),
                        Answered::Applied { .. }
                    )
                })
            })
            .collect();
        let applied = workers
            .into_iter()
            .map(|w| w.join().expect("worker"))
            .filter(|won| *won)
            .count();
        assert_eq!(applied, 1, "round {round}");
    }
}

// ---- the rows

fn row(c: &mut Core, method: &str, params: &Value) -> Result<Value, Error> {
    call(c, method, params, &Nobody)
}

fn with_options(session: &str, resource: &str) -> NewCard {
    NewCard {
        options: Some(
            json!([
                {"optionId": "once", "kind": "allow_once"},
                {"optionId": "always", "kind": "allow_always"},
                {"optionId": "reject", "kind": "reject_once"},
            ])
            .to_string(),
        ),
        // The row reads the real clock; stay clear of it.
        deadline: i64::MAX / 2,
        ..ask(session, resource)
    }
}

#[test]
fn cards_list_shows_the_open_and_the_recent_closed() {
    let (_t, _l, mut c) = core();
    let open = new(&mut c, "s1", "open");
    let done = new(&mut c, "s1", "done");
    no(&mut c, &done.id, T0 + 1);
    // An invited person's closed card is not the owner's history.
    let theirs = raised(
        &mut c,
        &NewCard {
            person: "sam".into(),
            ..ask("s2", "theirs")
        },
    );
    no(&mut c, &theirs.id, T0 + 1);
    let got = row(&mut c, "cards.list", &json!({})).unwrap();
    let ids = |k: &str| -> Vec<String> {
        got[k]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(ids("open"), vec![open.id.clone()]);
    assert_eq!(ids("recent"), vec![done.id.clone()]);
    assert_eq!(got["open"][0]["secret"], json!(false));
}

#[test]
fn cards_show_gives_the_card_and_refuses_what_is_no_card() {
    let (_t, _l, mut c) = core();
    let a = raised(&mut c, &with_options("s1", "a"));
    let got = row(&mut c, "cards.show", &json!({"id": a.id})).unwrap();
    assert_eq!(got["id"], json!(a.id));
    assert_eq!(got["state"], json!("pending"));
    assert_eq!(got["options"][0]["optionId"], json!("once"));
    assert!(matches!(
        row(&mut c, "cards.show", &json!({"id": "nope"})),
        Err(Error::BadParams(_))
    ));
    for params in [json!({}), json!({"id": ""}), json!({"id": 3})] {
        assert!(
            matches!(row(&mut c, "cards.show", &params), Err(Error::BadParams(_))),
            "{params}"
        );
    }
}

#[test]
fn cards_answer_reads_the_cards_options_for_a_yes_or_a_no() {
    let (_t, _l, mut c) = core();
    let a = raised(&mut c, &with_options("s1", "a"));
    let got = row(
        &mut c,
        "cards.answer",
        &json!({"id": a.id, "answer": "always"}),
    )
    .unwrap();
    assert_eq!(
        got,
        json!({"result": "answered", "id": a.id, "state": "approved", "allowed": true})
    );
    let stored_a = stored(&mut c, &a.id);
    assert_eq!(stored_a.answer.as_deref(), Some("always"));
    assert_eq!(stored_a.answered_by.as_deref(), Some("owner"));

    let b = raised(&mut c, &with_options("s1", "b"));
    let got = row(
        &mut c,
        "cards.answer",
        &json!({"id": b.id, "answer": "reject"}),
    )
    .unwrap();
    assert_eq!(got["state"], json!("denied"));
    assert_eq!(got["allowed"], json!(false));
}

#[test]
fn cards_answer_without_options_takes_allow_deny_cancel_and_refuses_the_rest() {
    let (_t, _l, mut c) = core();
    let far = NewCard {
        deadline: i64::MAX / 2,
        ..ask("s1", "a")
    };
    let a = raised(&mut c, &far);
    // Nonsense, or the button of another card: refused, and the card stays open.
    for word in ["maybe", "once", "ALLOW", " "] {
        assert!(
            matches!(
                row(&mut c, "cards.answer", &json!({"id": a.id, "answer": word})),
                Err(Error::BadParams(_))
            ),
            "{word}"
        );
    }
    assert_eq!(state(&mut c, &a.id), State::Pending);
    let got = row(
        &mut c,
        "cards.answer",
        &json!({"id": a.id, "answer": "cancel"}),
    )
    .unwrap();
    assert_eq!(
        (got["state"].as_str(), got["allowed"].as_bool()),
        (Some("denied"), Some(false))
    );
    let b = raised(
        &mut c,
        &NewCard {
            deadline: i64::MAX / 2,
            ..ask("s1", "b")
        },
    );
    let got = row(
        &mut c,
        "cards.answer",
        &json!({"id": b.id, "answer": "allow"}),
    )
    .unwrap();
    assert_eq!(got["state"], json!("approved"));
}

#[test]
fn cards_answer_requires_both_params_and_a_known_card() {
    let (_t, _l, mut c) = core();
    for params in [
        json!({}),
        json!({"id": "x"}),
        json!({"answer": "allow"}),
        json!({"id": "", "answer": "allow"}),
        json!({"id": "x", "answer": ""}),
        json!({"id": "nope", "answer": "allow"}),
    ] {
        assert!(
            matches!(
                row(&mut c, "cards.answer", &params),
                Err(Error::BadParams(_))
            ),
            "{params}"
        );
    }
}

#[test]
fn cards_answer_says_closed_expired_or_claimed_and_changes_nothing() {
    let (_t, _l, mut c) = core();
    let a = raised(&mut c, &with_options("s1", "a"));
    row(
        &mut c,
        "cards.answer",
        &json!({"id": a.id, "answer": "once"}),
    )
    .unwrap();
    let before = stored(&mut c, &a.id);
    let got = row(
        &mut c,
        "cards.answer",
        &json!({"id": a.id, "answer": "reject"}),
    )
    .unwrap();
    assert_eq!(
        got,
        json!({"result": "closed", "id": a.id, "state": "approved"})
    );
    assert_eq!(stored(&mut c, &a.id), before);

    let w = raised(&mut c, &with_options("s1", "w"));
    withdraw(&mut c, &w.id).unwrap();
    let got = row(
        &mut c,
        "cards.answer",
        &json!({"id": w.id, "answer": "once"}),
    )
    .unwrap();
    assert_eq!(
        got,
        json!({"result": "closed", "id": w.id, "state": "withdrawn"})
    );

    // The deadline is the real clock's here: one in the past is refused at raise, so age a card by its row.
    let e = raised(&mut c, &with_options("s1", "e"));
    {
        let tx = c.claim_tx().unwrap();
        tx.execute("UPDATE cards SET deadline = 1 WHERE id = ?1", [&e.id])
            .unwrap();
        tx.commit().unwrap();
    }
    let got = row(
        &mut c,
        "cards.answer",
        &json!({"id": e.id, "answer": "once"}),
    )
    .unwrap();
    assert_eq!(
        got,
        json!({"result": "expired", "id": e.id, "state": "expired"})
    );
    assert_eq!(stored(&mut c, &e.id).answer, None);

    let k = raised(&mut c, &with_options("s1", "k"));
    claim(&mut c, &k.id, &hold("app", i64::MAX / 2), T0, &Nobody)
        .unwrap()
        .unwrap();
    let got = row(
        &mut c,
        "cards.answer",
        &json!({"id": k.id, "answer": "once"}),
    )
    .unwrap();
    assert_eq!(got, json!({"result": "claimed", "id": k.id, "by": "app"}));
    assert_eq!(state(&mut c, &k.id), State::Pending);
}

#[test]
fn a_card_whose_options_cannot_be_read_is_not_answered() {
    let (_t, _l, mut c) = core();
    for (n, junk) in ["{not json", "{\"a\": 1}"].into_iter().enumerate() {
        let a = raised(
            &mut c,
            &NewCard {
                options: Some(junk.into()),
                deadline: i64::MAX / 2,
                ..ask("s1", &format!("a{n}"))
            },
        );
        assert!(matches!(
            row(
                &mut c,
                "cards.answer",
                &json!({"id": a.id, "answer": "allow"})
            ),
            Err(Error::Refused(_))
        ));
        assert_eq!(state(&mut c, &a.id), State::Pending);
    }
}

#[test]
fn a_secret_card_is_not_answered_here_and_stays_open() {
    let (_t, _l, mut c) = core();
    for tool in ["request_secret", "request_access"] {
        let a = raised(
            &mut c,
            &NewCard {
                tool: tool.into(),
                deadline: i64::MAX / 2,
                ..ask("s1", tool)
            },
        );
        for word in ["allow", "deny", "cancel", "whatever"] {
            let err =
                row(&mut c, "cards.answer", &json!({"id": a.id, "answer": word})).expect_err(tool);
            assert!(
                matches!(&err, Error::NotAllowed(m) if m.contains("Kith app")),
                "{tool} {word}: {err}"
            );
        }
        let still = stored(&mut c, &a.id);
        assert_eq!(still.state, State::Pending, "{tool}");
        assert_eq!(still.answer, None, "{tool}");
        // It reads as secret to whoever lists it, and show still works (the owner may look).
        let shown = row(&mut c, "cards.show", &json!({"id": a.id})).unwrap();
        assert_eq!(shown["secret"], json!(true));
    }
}

#[test]
fn the_rows_not_in_the_cards_family_are_not_served() {
    let (_t, _l, mut c) = core();
    assert!(matches!(
        row(&mut c, "cards.history", &json!({})),
        Err(Error::NotServed(_))
    ));
}

// ---- the table the import will fill

#[test]
fn a_row_with_the_host_columns_fits_the_table() {
    let (_t, _l, mut c) = core();
    {
        let tx = c.claim_tx().unwrap();
        tx.execute(
            "INSERT INTO cards(id, session, source, bot_id, run_id, entry_id, person, agent, approver, tool, input,
                resource, shown, scope, state, created_at, deadline, answer, allowed, answered_by, answered_at,
                answered_parked, granted_at, resumed_at, used_by_ask, reply_to, asked_by, answerable_by, in_grants,
                applied_at)
             VALUES('h1', 's', 'acp', 'b', NULL, 'e', 'sam', 'echo', 'member', 'Bash', '{}', 'r', 'r', 'once',
                'approved', 1, 2, 'once', 1, 'sam', 3, 1, 3, NULL, 0, NULL, NULL, 'sam', 1, NULL)",
            [],
        )
        .unwrap();
        tx.commit().unwrap();
    }
    let got = stored(&mut c, "h1");
    assert_eq!(
        (got.state, got.approver.as_str()),
        (State::Approved, "member")
    );
    assert_eq!((got.options, got.claim_by), (None, None));
}

#[test]
fn the_table_keeps_the_host_rules_the_store_relies_on() {
    let (_t, _l, mut c) = core();
    let a = new(&mut c, "s1", "a");
    let tx = c.claim_tx().unwrap();
    // One open card per session and resource, enforced by the table.
    let dup = tx.execute(
        "INSERT INTO cards(id, session, source, approver, tool, input, resource, state, created_at, deadline)
         VALUES('dup', 's1', 'run', 'owner', 'Bash', '{}', 'a', 'pending', 1, 2)",
        [],
    );
    assert!(dup.is_err());
    // A claim has both its holder and its end, or neither.
    let half = tx.execute("UPDATE cards SET claim_by = 'w' WHERE id = ?1", [&a.id]);
    assert!(half.is_err());
}

// ---- a randomised sequence

/// A small generator, seeded: the same seed is the same run.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % n
    }
}

#[test]
fn random_sequences_answer_a_card_at_most_once_and_never_after_it_closes() {
    const HOLDERS: [&str; 3] = ["h0", "h1", "h2"];
    for seed in 0..40u64 {
        let (_t, _l, mut c) = core();
        let mut rng = Rng(seed + 1);
        let mut now = T0;
        // Every card ever raised, and how many answers or saves each took.
        let mut ids: Vec<String> = Vec::new();
        let mut taken: HashMap<String, u32> = HashMap::new();
        let mut seen: HashMap<String, Card> = HashMap::new();
        for step in 0..70 {
            let pick = |rng: &mut Rng, ids: &Vec<String>| -> Option<String> {
                (!ids.is_empty()).then(|| {
                    ids[usize::try_from(rng.below(u64::try_from(ids.len()).unwrap())).unwrap()]
                        .clone()
                })
            };
            let op = rng.below(9);
            let before: Option<Card> = pick(&mut rng, &ids).map(|id| stored(&mut c, &id));
            let label = format!("seed {seed} step {step} op {op}");
            match (op, &before) {
                (0, _) => {
                    let n = NewCard {
                        deadline: now + 50 + i64::try_from(rng.below(300)).unwrap(),
                        ..ask(&format!("s{}", rng.below(3)), &format!("r{}", rng.below(2)))
                    };
                    match raise(&mut c, &n, now, &Nobody).unwrap() {
                        Raised::New(card) => {
                            ids.push(card.id);
                        }
                        Raised::Joined(card) | Raised::Granted(card) => {
                            if !ids.contains(&card.id) {
                                ids.push(card.id);
                            }
                        }
                    }
                }
                (1 | 2, Some(b)) => {
                    let allow = op == 1;
                    let got = answer(&mut c, &reply(&b.id, allow), now, &Nobody).unwrap();
                    if let Answered::Applied { .. } = got {
                        *taken.entry(b.id.clone()).or_default() += 1;
                        assert!(
                            b.state.open(),
                            "{label}: answered a card that was {:?}",
                            b.state
                        );
                        assert!(b.holder(now).is_none(), "{label}: answered over a claim");
                        assert!(b.deadline >= now, "{label}: answered after the deadline");
                    }
                }
                (3, Some(b)) => {
                    let who = HOLDERS[usize::try_from(rng.below(3)).unwrap()];
                    let until = now + 1 + i64::try_from(rng.below(200)).unwrap();
                    let other_holds = b.holder(now).is_some_and(|h| h != who);
                    let got = claim(&mut c, &b.id, &hold(who, until), now, &Nobody).unwrap();
                    if got.is_some() {
                        assert!(!other_holds, "{label}: two holders");
                        assert!(
                            b.state.open() && b.deadline >= now,
                            "{label}: claimed a closed card"
                        );
                    }
                }
                (4, Some(b)) => {
                    let who = HOLDERS[usize::try_from(rng.below(3)).unwrap()];
                    release(&mut c, &b.id, who, &Nobody).unwrap();
                }
                (5, Some(b)) => {
                    let who = HOLDERS[usize::try_from(rng.below(3)).unwrap()];
                    let got = saved(&mut c, &b.id, who, "owner", now, now + 100, &Nobody).unwrap();
                    if got.is_some() {
                        *taken.entry(b.id.clone()).or_default() += 1;
                        assert!(
                            b.state.open(),
                            "{label}: saved a card that was {:?}",
                            b.state
                        );
                        assert_eq!(
                            b.claim_by.as_deref(),
                            Some(who),
                            "{label}: saved without the claim"
                        );
                    }
                }
                (6, Some(b)) => {
                    withdraw(&mut c, &b.id).unwrap();
                }
                (7, _) => {
                    sweep(&mut c, now, &Nobody).unwrap();
                }
                _ => now += i64::try_from(rng.below(120)).unwrap(),
            }
            for id in &ids {
                let now_card = stored(&mut c, id);
                assert!(
                    taken.get(id).copied().unwrap_or(0) <= 1,
                    "{label}: card {id} answered twice"
                );
                if let Some(old) = seen.get(id) {
                    // Once answered, the answer is never rewritten.
                    if old.answered_at.is_some() {
                        assert_eq!(
                            (&old.answer, old.allowed, &old.answered_by, old.answered_at),
                            (
                                &now_card.answer,
                                now_card.allowed,
                                &now_card.answered_by,
                                now_card.answered_at
                            ),
                            "{label}: card {id} was answered again"
                        );
                    }
                    // Once closed for good, closed for good.
                    if matches!(
                        old.state,
                        State::Denied | State::Expired | State::Withdrawn | State::Used
                    ) {
                        assert_eq!(old.state, now_card.state, "{label}: card {id} came back");
                    }
                    // An answer is only ever recorded on a card that was open or already answered.
                    if old.answered_at.is_none() && now_card.answered_at.is_some() {
                        assert!(
                            old.state.open(),
                            "{label}: card {id} answered while {:?}",
                            old.state
                        );
                    }
                }
                // A claim is a holder and an end, or neither, and never on a closed card.
                if !now_card.state.open() {
                    assert_eq!(
                        now_card.claim_by, None,
                        "{label}: a closed card holds a claim"
                    );
                }
                seen.insert(id.clone(), now_card);
            }
        }
    }
}
