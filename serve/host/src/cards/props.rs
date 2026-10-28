//! Property tests for the card store: random sequences of asks, answers, parks, withdrawals, sweeps and restarts, with
//! the invariants of the design checked after every step (A1, A4, A6, A7, C3, C8). Every property calls the real
//! `Store`; nothing here models it. Run only these: `cargo test --bin kith cards::props`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::store::{Answered, Card, CardState, NewCard, Opened, Store};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestRunner};
use std::collections::HashMap;

const SESSIONS: [&str; 2] = ["s1", "s2"];
const RESOURCES: [&str; 2] = ["Bash:ls", "Write:/tmp/a"];
const PEOPLE: [&str; 2] = ["sam", ""];

#[derive(Debug, Clone)]
enum Op {
    Ask { session: usize, resource: usize, person: usize },
    Answer { pick: usize, allow: bool, late: bool, spend: bool },
    Undeliver { pick: usize },
    ParkRun,
    Withdraw { person: usize },
    Sweep { far: bool },
    Restart,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0..2usize, 0..2usize, 0..2usize).prop_map(|(session, resource, person)| Op::Ask { session, resource, person }),
        4 => (0..16usize, any::<bool>(), any::<bool>(), any::<bool>())
            .prop_map(|(pick, allow, late, spend)| Op::Answer { pick, allow, late, spend }),
        1 => (0..16usize).prop_map(|pick| Op::Undeliver { pick }),
        1 => Just(Op::ParkRun),
        1 => (0..2usize).prop_map(|person| Op::Withdraw { person }),
        1 => any::<bool>().prop_map(|far| Op::Sweep { far }),
        1 => Just(Op::Restart),
    ]
}

fn store() -> Store {
    let dir = std::env::temp_dir().join(format!("kith-cards-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    Store::open(&dir.join("t.db")).unwrap()
}

fn ask(session: usize, resource: usize, person: usize) -> NewCard {
    NewCard {
        session: SESSIONS[session].into(),
        source: "run",
        bot_id: None,
        run_id: Some("r".into()),
        person: PEOPLE[person].into(),
        agent: "echo".into(),
        approver: "owner",
        tool: "Bash".into(),
        input: "{}".into(),
        resource: RESOURCES[resource].into(),
        shown: RESOURCES[resource].into(),
        deadline: 1_000,
        reply_to: None,
        asked_by: None,
    }
}

/// Whether the card design lets `from` become `to`; `used` and the closed states never come back.
fn legal(from: CardState, to: CardState) -> bool {
    use CardState::*;
    from == to
        || matches!(
            (from, to),
            (Pending, Live | Parked | Approved | Denied | Expired | Withdrawn)
                | (Live, Parked | Approved | Denied | Expired | Withdrawn)
                | (Parked, Live | Approved | Denied | Expired | Withdrawn)
                | (Approved, Used | Expired | Withdrawn)
                | (Pending | Live, Used)
                | (Used, Approved)
        )
}

struct World {
    s: Store,
    ids: Vec<String>,
    seen: HashMap<String, CardState>,
    /// per (session, resource): yes answers recorded, and grants taken by an ask
    yes: HashMap<(String, String), usize>,
    taken: HashMap<(String, String), usize>,
    now: i64,
}

impl World {
    fn new() -> Self {
        Self { s: store(), ids: Vec::new(), seen: HashMap::new(), yes: HashMap::new(), taken: HashMap::new(), now: 100 }
    }

    fn cards(&self) -> Vec<Card> {
        self.ids.iter().filter_map(|id| self.s.card(id)).collect()
    }

    fn step(&mut self, op: &Op) {
        self.now += 1;
        match op {
            Op::Ask { session, resource, person } => {
                let n = ask(*session, *resource, *person);
                match self.s.card_open(&n, self.now).unwrap() {
                    Opened::New(c) => {
                        self.ids.push(c.id.clone());
                        self.s.card_live(&c.id, Some("e"), Some("r")).unwrap();
                    }
                    Opened::Joined(_) => {}
                    Opened::Granted(c) => {
                        *self.taken.entry((c.session.clone(), c.resource.clone())).or_default() += 1;
                        assert_eq!(
                            (c.session.as_str(), c.resource.as_str()),
                            (n.session.as_str(), n.resource.as_str()),
                            "C8"
                        );
                    }
                }
            }
            Op::Answer { pick, allow, late, spend } => {
                let Some(id) = self.ids.get(pick % self.ids.len().max(1)).cloned() else { return };
                let before = self.s.card(&id).unwrap();
                let at = if *late { 5_000 } else { self.now };
                let got = self.s.card_answer(&id, *allow, "x", "kai", at, *spend).unwrap();
                match got {
                    Answered::Applied { card, .. } => {
                        assert!(
                            before.state.open() && at <= before.deadline,
                            "A1: only an open card before its deadline answers"
                        );
                        if matches!(card.state, CardState::Approved | CardState::Used) {
                            *self.yes.entry((card.session.clone(), card.resource.clone())).or_default() += 1;
                        }
                    }
                    Answered::Nothing(_) => {
                        let after = self.s.card(&id).unwrap();
                        assert!(
                            !matches!(after.state, CardState::Approved) || before.state == CardState::Approved,
                            "A1"
                        );
                    }
                }
            }
            Op::Undeliver { pick } => {
                if let Some(id) = self.ids.get(pick % self.ids.len().max(1)) {
                    self.s.card_undelivered(id).unwrap();
                }
            }
            Op::ParkRun => {
                self.s.cards_park_run("r").unwrap();
            }
            Op::Withdraw { person } => {
                if !PEOPLE[*person].is_empty() {
                    self.s.cards_withdraw_person(PEOPLE[*person], None).unwrap();
                    // A7: nothing of theirs is open or an unused grant after.
                    for c in self.cards().iter().filter(|c| c.person == PEOPLE[*person]) {
                        assert!(!c.state.open() && c.state != CardState::Approved, "A7: {c:?}");
                    }
                }
            }
            Op::Sweep { far } => {
                let at = if *far { 10_000 } else { self.now };
                self.s.cards_sweep(at).unwrap();
                if *far {
                    self.now = 10_000;
                    for c in self.cards() {
                        assert!(!c.state.open() && c.state != CardState::Approved, "silence is no: {c:?}");
                    }
                }
            }
            Op::Restart => {
                self.s.cards_restart(self.now).unwrap();
                assert!(self.cards().iter().all(|c| !matches!(c.state, CardState::Live | CardState::Pending)), "C4");
            }
        }
    }

    fn check(&mut self) -> Result<(), TestCaseError> {
        let cards = self.cards();
        // A6: at most one open card per (session, resource).
        let mut open: HashMap<(String, String), usize> = HashMap::new();
        for c in cards.iter().filter(|c| c.state.open()) {
            *open.entry((c.session.clone(), c.resource.clone())).or_default() += 1;
        }
        prop_assert!(open.values().all(|n| *n == 1), "A6: {open:?}");
        for c in &cards {
            if let Some(was) = self.seen.insert(c.id.clone(), c.state) {
                prop_assert!(legal(was, c.state), "{was:?} -> {:?} for {}", c.state, c.id);
            }
            // Nothing is a grant unless a yes was recorded (A1, A3).
            if matches!(c.state, CardState::Approved | CardState::Used) {
                prop_assert!(c.allowed == Some(true) && c.granted_at.is_some(), "{c:?}");
            }
            if matches!(c.state, CardState::Denied | CardState::Expired | CardState::Withdrawn) {
                prop_assert!(c.state != CardState::Approved);
            }
        }
        // A4: a yes buys one use: grants taken by later asks never outnumber the yeses, per session and resource.
        for (k, n) in &self.taken {
            prop_assert!(
                *n <= self.yes.get(k).copied().unwrap_or(0),
                "A4: {k:?} used {n}, said yes {:?}",
                self.yes.get(k)
            );
        }
        Ok(())
    }
}

fn runner() -> TestRunner {
    let mut config = Config { failure_persistence: None, ..Config::default() };
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 64;
    }
    TestRunner::new(config)
}

#[test]
fn the_card_invariants_hold_over_random_histories() {
    runner()
        .run(&proptest::collection::vec(op(), 1..40), |ops| {
            let mut w = World::new();
            for o in &ops {
                w.step(o);
                w.check()?;
            }
            Ok(())
        })
        .map_err(|e| e.to_string())
        .unwrap();
}

// ---- secret cards: one write per card, in order (verification/secrets/Secrets.tla is the same model, exhaustively)

#[derive(Debug, Clone)]
enum SecretOp {
    /// A device takes the write claim (slot, then the store's claim) and starts writing.
    Claim { device: usize },
    /// Its write came back: ok saves the card, a failure releases the claim.
    Finish { device: usize, ok: bool },
    /// The owner says no (refused while a write is in flight: the slot is taken).
    Deny,
    /// The clock moves, and the sweep runs.
    Sweep { by: i64 },
}

fn secret_op() -> impl Strategy<Value = SecretOp> {
    prop_oneof![
        4 => (0..2usize).prop_map(|device| SecretOp::Claim { device }),
        4 => (0..2usize, any::<bool>()).prop_map(|(device, ok)| SecretOp::Finish { device, ok }),
        1 => Just(SecretOp::Deny),
        2 => (1..400i64).prop_map(|by| SecretOp::Sweep { by }),
    ]
}

/// open -> saved | denied | expired; the key is written at most once and only through a claim taken while the card
/// was open; a card that is denied or expired is never written afterwards; a failed write leaves the card open with
/// its own deadline.
#[test]
fn a_secret_card_is_written_at_most_once_and_never_after_it_closed() {
    runner()
        .run(&proptest::collection::vec(secret_op(), 1..40), |ops| {
            let s = store();
            let mut n = ask(0, 0, 1);
            n.tool = "request_secret".into();
            let mut now = 100;
            let card = match s.card_open(&n, now).unwrap() {
                Opened::New(c) => c,
                other => panic!("{other:?}"),
            };
            s.card_live(&card.id, Some("e"), Some("r")).unwrap();
            let own_deadline = card.deadline;
            let (mut claimed, mut writes, mut original): (Option<usize>, usize, i64) = (None, 0, own_deadline);
            let mut closed_before_write = false;
            for op in &ops {
                now += 1;
                match op {
                    SecretOp::Claim { device } => {
                        // The slot: a second device finds it taken.
                        if claimed.is_some() {
                            continue;
                        }
                        if let Some(c) = s.card_claim(&card.id, now, now + 100_000).unwrap() {
                            prop_assert!(c.state.open() && c.deadline >= now);
                            claimed = Some(*device);
                            original = c.deadline;
                        }
                    }
                    SecretOp::Finish { device, ok } => {
                        if claimed != Some(*device) {
                            continue;
                        }
                        claimed = None;
                        if *ok {
                            let state = s.card(&card.id).unwrap().state;
                            prop_assert!(state.open(), "a write started on a closed card: {state:?}");
                            writes += 1;
                            prop_assert!(s.card_saved(&card.id, "owner", now, now + 1_000).unwrap().is_some());
                        } else {
                            s.card_claim_release(&card.id, original).unwrap();
                        }
                    }
                    SecretOp::Deny => {
                        if claimed.is_none() {
                            s.card_answer(&card.id, false, "deny", "owner", now, false).unwrap();
                        }
                    }
                    SecretOp::Sweep { by } => {
                        now += by;
                        s.cards_sweep(now).unwrap();
                    }
                }
                let c = s.card(&card.id).unwrap();
                if writes == 0 && !c.state.open() {
                    closed_before_write = true;
                }
                prop_assert!(writes <= 1, "two writes for one card");
                if closed_before_write {
                    prop_assert!(writes == 0, "written after the card closed");
                    prop_assert!(matches!(c.state, CardState::Denied | CardState::Expired), "{c:?}");
                }
                if writes == 1 {
                    prop_assert!(c.state == CardState::Used && c.allowed == Some(true), "saved means spent: {c:?}");
                }
                if claimed.is_some() {
                    prop_assert!(c.state.open(), "the clock or a no closed a card mid-write: {c:?}");
                }
                if claimed.is_none() && writes == 0 && c.state.open() {
                    prop_assert!(c.deadline <= original.max(own_deadline), "a released claim keeps its own deadline");
                }
            }
            Ok(())
        })
        .map_err(|e| e.to_string())
        .unwrap();
}
