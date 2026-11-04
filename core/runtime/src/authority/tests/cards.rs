//! A revoked or removed person takes their cards with them (M2-11b, A7): the open cards and the unused once-grants of
//! the person and of every bot of theirs are withdrawn in the same transaction as the write, so an answer that comes
//! after it is refused, `cards::raise` never picks up a pre-revoke yes, and an undo gives access back and nothing else.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::writes::Rig;
use crate::cards::{Answered, Claim, NewCard, Raised, Reply, State, answer, card, claim, raise};
use crate::gate::Liveness;

struct Nobody;

impl Liveness for Nobody {
    fn start(&self, _: u32) -> Option<String> {
        None
    }
    fn gone(&self, _: u32) -> bool {
        false
    }
}

const T0: i64 = 1_000;
const DEADLINE: i64 = 1_000_000;

fn world() -> Value {
    json!({
        "people": {
            "kai": {"owner": true},
            "sam": {},
            "amy": {},
            "bot-a": {"bot_of": "sam"},
            "bot-b": {"bot_of": "amy"},
        },
        "members": {"house": {"sam": {"preset": "member"}, "amy": {"preset": "member"}}},
    })
}

fn id(p: &str) -> Value {
    json!({ "id": p })
}

fn ask(person: &str, session: &str, resource: &str) -> NewCard {
    NewCard {
        session: session.into(),
        source: "run",
        bot_id: None,
        run_id: Some("r1".into()),
        person: person.into(),
        agent: "house".into(),
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

fn open(rig: &mut Rig, n: &NewCard) -> String {
    match raise(&mut rig.core, n, T0, &Nobody).unwrap() {
        Raised::New(c) | Raised::Joined(c) | Raised::Granted(c) => c.id,
    }
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

fn state(rig: &mut Rig, id: &str) -> State {
    card(&mut rig.core, id).unwrap().unwrap().state
}

/// Not answerable: an answer changes nothing and a new ask for the same thing finds no grant.
fn dead(rig: &mut Rig, id: &str, n: &NewCard) {
    assert_eq!(state(rig, id), State::Withdrawn);
    let got = answer(&mut rig.core, &reply(id, true), T0 + 1, &Nobody).unwrap();
    assert!(matches!(got, Answered::Nothing(_)), "{got:?}");
    assert!(matches!(
        raise(&mut rig.core, n, T0 + 2, &Nobody).unwrap(),
        Raised::New(_)
    ));
}

/// Sam asks, so does sam's bot, so does amy: open ones, approved and unused ones.
struct Cards {
    sam_open: (String, NewCard),
    bot_open: (String, NewCard),
    sam_grant: (String, NewCard),
    bot_grant: (String, NewCard),
    amy_open: String,
    owner_open: String,
}

fn raise_all(rig: &mut Rig) -> Cards {
    let mk = |rig: &mut Rig, p: &str, s: &str, yes: bool| {
        let n = ask(p, s, &format!("Bash:{p}:{s}"));
        let id = open(rig, &n);
        if yes {
            let got = answer(&mut rig.core, &reply(&id, true), T0 + 1, &Nobody).unwrap();
            assert!(matches!(got, Answered::Applied { .. }));
        }
        (id, n)
    };
    let sam_open = mk(rig, "sam", "s1", false);
    let bot_open = mk(rig, "bot-a", "s2", false);
    let sam_grant = mk(rig, "sam", "s3", true);
    let bot_grant = mk(rig, "bot-a", "s4", true);
    let amy_open = mk(rig, "amy", "s5", false).0;
    let owner_open = mk(rig, "", "s6", false).0;
    Cards {
        sam_open,
        bot_open,
        sam_grant,
        bot_grant,
        amy_open,
        owner_open,
    }
}

fn check_withdrawn(rig: &mut Rig, c: &Cards) {
    for (id, n) in [&c.sam_open, &c.bot_open, &c.sam_grant, &c.bot_grant] {
        dead(rig, id, n);
    }
    assert_eq!(state(rig, &c.amy_open), State::Pending, "someone else's");
    assert_eq!(state(rig, &c.owner_open), State::Pending, "the owner's");
}

#[test]
fn a_suspension_withdraws_the_open_cards_and_unused_grants_of_the_person_and_their_bots() {
    let mut rig = Rig::new(&world());
    let c = raise_all(&mut rig);
    let said = rig.call("people.suspend", &id("sam")).unwrap();
    assert_eq!(said["cards"].as_array().unwrap().len(), 4, "{said}");
    check_withdrawn(&mut rig, &c);
}

#[test]
fn an_undo_gives_access_back_and_revives_no_card() {
    let mut rig = Rig::new(&world());
    let c = raise_all(&mut rig);
    rig.call("people.suspend", &id("sam")).unwrap();
    rig.call("people.undo", &id("sam")).unwrap();
    check_withdrawn(&mut rig, &c);
    // a card asked after the undo works as any
    let n = ask("sam", "s9", "Bash:new");
    let fresh = open(&mut rig, &n);
    let got = answer(&mut rig.core, &reply(&fresh, true), T0 + 3, &Nobody).unwrap();
    assert!(matches!(got, Answered::Applied { .. }));
}

#[test]
fn a_removal_of_the_person_withdraws_the_same_cards() {
    let mut rig = Rig::new(&world());
    let c = raise_all(&mut rig);
    rig.call("people.removePerson", &id("sam")).unwrap();
    check_withdrawn(&mut rig, &c);
}

#[test]
fn removing_a_bot_withdraws_its_cards_only() {
    let mut rig = Rig::new(&world());
    let c = raise_all(&mut rig);
    rig.call("people.removeBot", &id("bot-a")).unwrap();
    for (id, n) in [&c.bot_open, &c.bot_grant] {
        dead(&mut rig, id, n);
    }
    assert_eq!(state(&mut rig, &c.sam_open.0), State::Pending);
    assert_eq!(state(&mut rig, &c.sam_grant.0), State::Approved);
}

#[test]
fn a_refused_suspension_withdraws_nothing() {
    let mut rig = Rig::new(&world());
    let c = raise_all(&mut rig);
    assert!(rig.call("people.suspend", &id("kai")).is_err());
    assert!(rig.call("people.suspend", &id("bot-a")).is_err());
    assert_eq!(state(&mut rig, &c.sam_open.0), State::Pending);
    assert_eq!(state(&mut rig, &c.owner_open), State::Pending);
}

#[test]
fn a_held_claim_does_not_keep_a_revoked_persons_card_answerable() {
    let mut rig = Rig::new(&world());
    let n = ask("sam", "s1", "Bash:k");
    let cid = open(&mut rig, &n);
    let held = Claim {
        holder: "me",
        pid: Some(std::process::id()),
        until: DEADLINE,
    };
    assert!(
        claim(&mut rig.core, &cid, &held, T0, &Nobody)
            .unwrap()
            .is_some()
    );
    rig.call("people.suspend", &id("sam")).unwrap();
    assert_eq!(state(&mut rig, &cid), State::Withdrawn);
}

// --- a property over random sequences ---------------------------------------------------------------------------

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

/// Random suspend / undo / ask / answer / pickup: no card that was open or unused when its person was suspended is ever
/// answered, granted or picked up afterwards. The oracle is the list kept here of the cards the suspension closed, not
/// the store's own judgement of a later call.
#[test]
fn no_withdrawn_card_is_ever_answered_or_picked_up() {
    const WHO: [&str; 4] = ["sam", "bot-a", "amy", ""];
    for seed in 0..60u64 {
        let mut rig = Rig::new(&world());
        let mut rng = Rng(seed + 1);
        let mut now = T0;
        let mut cards: Vec<(String, &str)> = Vec::new();
        let mut closed: BTreeSet<String> = BTreeSet::new();
        let mut suspended = false;
        for step in 0..50 {
            now += 1;
            let label = format!("seed {seed} step {step}");
            match rng.below(6) {
                0 => {
                    let p = WHO[usize::try_from(rng.below(4)).unwrap()];
                    let n = ask(
                        p,
                        &format!("s{}", rng.below(2)),
                        &format!("r{}", rng.below(2)),
                    );
                    match raise(&mut rig.core, &n, now, &Nobody).unwrap() {
                        Raised::New(c) => cards.push((c.id, p)),
                        Raised::Joined(c) => {
                            assert!(!closed.contains(&c.id), "{label}: joined a withdrawn card");
                        }
                        Raised::Granted(c) => {
                            assert!(
                                !closed.contains(&c.id),
                                "{label}: picked up a withdrawn yes"
                            );
                        }
                    }
                }
                1 | 2 if !cards.is_empty() => {
                    let at = usize::try_from(rng.below(cards.len() as u64)).unwrap();
                    let cid = cards[at].0.clone();
                    let allow = rng.below(3) > 0;
                    let got = answer(&mut rig.core, &reply(&cid, allow), now, &Nobody).unwrap();
                    if closed.contains(&cid) {
                        assert!(
                            matches!(got, Answered::Nothing(_)),
                            "{label}: answered a withdrawn card"
                        );
                    }
                }
                3 if !suspended => {
                    rig.call("people.suspend", &id("sam")).unwrap();
                    suspended = true;
                    // everything of sam's and bot-a's that was open or approved is gone now
                    for (cid, p) in &cards {
                        if matches!(*p, "sam" | "bot-a") {
                            let s = state(&mut rig, cid);
                            assert!(!s.open() && s != State::Approved, "{label}: {s:?} survived");
                            if s == State::Withdrawn {
                                closed.insert(cid.clone());
                            }
                        }
                    }
                }
                4 if suspended => {
                    rig.call("people.undo", &id("sam")).unwrap();
                    suspended = false;
                }
                _ => {}
            }
            for cid in &closed {
                assert_eq!(
                    state(&mut rig, cid),
                    State::Withdrawn,
                    "{label}: a withdrawn card moved"
                );
            }
        }
    }
}
