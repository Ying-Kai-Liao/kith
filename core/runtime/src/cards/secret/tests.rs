#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Secret cards on the real envelope with a fake key store (`MemoryKeyStore`): the invariants of
//! `verification/secrets/Secrets.tla` as deterministic tests, then a random sequence that checks them after every step.
//! Writes are counted by triggers on `secret_records` (a rolled-back write leaves no row in the log, as it leaves none
//! in the table), and a write is made to fail on demand by a trigger too.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::{Arc, Barrier, Mutex};

use serde_json::json;
use zeroize::Zeroizing;

use super::{
    CLAIM_MS, Held, Provided, Refused, begin, commit, plan_for, provide, provide_row_with, spec_of,
};
use crate::cards::{
    Answered, Card, NewCard, Raised, Reply, State, answer, call, card, claim, park_run, raise,
    release, sweep, withdraw,
};
use crate::db::Core;
use crate::error::Error;
use crate::gate::Liveness;
use crate::layout::Layout;
use crate::secrets::Envelope;
use crate::secrets::keystore::MemoryKeyStore;
use crate::tests::{machine, open_all};

fn core() -> (tempfile::TempDir, Layout, Core) {
    let (t, m) = machine();
    let l = Layout::new(&m);
    let c = open_all(&l);
    (t, l, c)
}

/// No start time can be read and no process is ever reported gone.
struct Nobody;

impl Liveness for Nobody {
    fn start(&self, _: u32) -> Option<String> {
        None
    }
    fn gone(&self, _: u32) -> bool {
        false
    }
}

/// The system says every process is gone: every writer has crashed.
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
/// The value no output may ever contain.
const SECRET: &str = "hunter2-hunter2-hunter2";

fn env(store: &MemoryKeyStore) -> Envelope {
    Envelope::with_store(Box::new(store.clone()))
}

/// Logs every committed write of the envelope's table and lets a test make the write of a `.../B` record fail.
fn instrument(c: &Core) {
    c.conn()
        .execute_batch(
            "CREATE TABLE wlog (name TEXT NOT NULL);
             CREATE TABLE failflag (on_ INTEGER NOT NULL);
             INSERT INTO failflag VALUES (0);
             CREATE TRIGGER w_ins AFTER INSERT ON secret_records BEGIN INSERT INTO wlog VALUES (new.name); END;
             CREATE TRIGGER w_upd AFTER UPDATE ON secret_records BEGIN INSERT INTO wlog VALUES (new.name); END;
             CREATE TRIGGER f_ins BEFORE INSERT ON secret_records
                WHEN (SELECT on_ FROM failflag) = 1 AND new.name LIKE '%/B'
                BEGIN SELECT RAISE(ABORT, 'disk full'); END;
             CREATE TRIGGER f_upd BEFORE UPDATE ON secret_records
                WHEN (SELECT on_ FROM failflag) = 1 AND new.name LIKE '%/B'
                BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
        )
        .unwrap();
}

fn fail_writes(c: &Core, on: bool) {
    c.conn()
        .execute("UPDATE failflag SET on_ = ?1", [i64::from(on)])
        .unwrap();
}

/// How many committed writes the records of service `svc` took.
fn writes(c: &Core, svc: &str) -> i64 {
    c.conn()
        .query_row(
            "SELECT COUNT(*) FROM wlog WHERE name LIKE ?1",
            [format!("key/{svc}/%")],
            |r| r.get(0),
        )
        .unwrap()
}

fn vals(pairs: &[(&str, &str)]) -> BTreeMap<String, Zeroizing<String>> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), Zeroizing::new((*v).to_owned())))
        .collect()
}

fn two() -> BTreeMap<String, Zeroizing<String>> {
    vals(&[("A", SECRET), ("B", "second-value")])
}

fn pair(a: &str, b: &str) -> BTreeMap<String, Zeroizing<String>> {
    vals(&[("A", a), ("B", b)])
}

fn key_card_with(c: &mut Core, svc: &str, deadline: i64, run: &str) -> Card {
    key_card_of(c, svc, deadline, run, "")
}

fn key_card_of(c: &mut Core, svc: &str, deadline: i64, run: &str, person: &str) -> Card {
    let n = NewCard {
        session: "s1".into(),
        source: "run",
        bot_id: None,
        run_id: Some(run.into()),
        person: person.into(),
        agent: "echo".into(),
        answerable_by: None,
        in_grants: false,
        tool: "request_secret".into(),
        input: json!({"service": svc, "vars": ["A", "B"]}).to_string(),
        resource: svc.into(),
        shown: svc.into(),
        deadline,
        reply_to: None,
        asked_by: None,
        options: None,
    };
    match raise(c, &n, T0, &Nobody).unwrap() {
        Raised::New(k) => k,
        other => panic!("{other:?}"),
    }
}

fn key_card(c: &mut Core, svc: &str) -> Card {
    key_card_with(c, svc, DEADLINE, "r1")
}

fn stored(c: &mut Core, id: &str) -> Card {
    card(c, id).unwrap().expect("card")
}

fn prov(
    c: &mut Core,
    e: &mut Envelope,
    id: &str,
    values: &BTreeMap<String, Zeroizing<String>>,
    now: i64,
    live: &dyn Liveness,
) -> Result<Provided, Error> {
    provide(c, e, id, Some("work"), values, &|| now, live)
}

fn saved(p: Result<Provided, Error>) -> (Card, String) {
    match p.unwrap() {
        Provided::Saved { card, profile, .. } => (*card, profile),
        Provided::Refused(why) => panic!("{why:?}"),
    }
}

fn refused(p: Result<Provided, Error>) -> Refused {
    match p.unwrap() {
        Provided::Refused(r) => r,
        Provided::Saved { card, .. } => panic!("saved {}", card.id),
    }
}

fn plan_of(c: &mut Core, id: &str, values: &BTreeMap<String, Zeroizing<String>>) -> super::Plan {
    let k = stored(c, id);
    plan_for(&spec_of(&k).unwrap(), Some("work"), values).unwrap()
}

fn read(c: &Core, e: &mut Envelope, name: &str) -> Option<String> {
    e.get(c.conn(), name)
        .unwrap()
        .map(|v| String::from_utf8(v.to_vec()).unwrap())
}

fn no(c: &mut Core, id: &str, now: i64) -> Answered {
    let r = Reply {
        id,
        allow: false,
        answer: "deny",
        by: "owner",
        spend: false,
    };
    answer(c, &r, now, &Nobody).unwrap()
}

// ---- the happy path and what it leaves

#[test]
fn a_key_is_sealed_under_its_record_name_and_the_card_ends_saved() {
    let (_t, _l, mut c) = core();
    instrument(&c);
    let store = MemoryKeyStore::default();
    let mut e = env(&store);
    let k = key_card(&mut c, "github");
    let (done, profile) = saved(prov(&mut c, &mut e, &k.id, &two(), T0 + 1, &Nobody));
    assert_eq!(profile, "github/work");
    assert_eq!(done.state, State::Used);
    assert_eq!(done.answer.as_deref(), Some("saved"));
    assert_eq!(done.answered_by.as_deref(), Some("owner"));
    assert!(done.claim_by.is_none() && done.claim_until.is_none() && done.claim_pid.is_none());
    // The values open under their record names, and under no other.
    assert_eq!(
        read(&c, &mut e, "key/github/work/A").as_deref(),
        Some(SECRET)
    );
    assert_eq!(
        read(&c, &mut e, "key/github/work/B").as_deref(),
        Some("second-value")
    );
    c.conn()
        .execute(
            "UPDATE secret_records SET name = 'key/github/work/Z' WHERE name = 'key/github/work/A'",
            [],
        )
        .unwrap();
    assert!(
        e.get(c.conn(), "key/github/work/Z").is_err(),
        "the name is the associated data: a renamed record does not open"
    );
    assert_eq!(
        writes(&c, "github"),
        2 + 1,
        "two records, and the test's own rename"
    );
}

#[test]
fn a_second_provide_finds_the_card_spent_and_writes_nothing() {
    let (_t, _l, mut c) = core();
    instrument(&c);
    let store = MemoryKeyStore::default();
    let mut e = env(&store);
    let k = key_card(&mut c, "github");
    saved(prov(&mut c, &mut e, &k.id, &two(), T0 + 1, &Nobody));
    let again = vals(&[("A", "other-a"), ("B", "other-b")]);
    for n in 0..3 {
        assert_eq!(
            refused(prov(&mut c, &mut e, &k.id, &again, T0 + 2 + n, &Nobody)),
            Refused::Closed(Some(State::Used))
        );
    }
    assert_eq!(writes(&c, "github"), 2);
    assert_eq!(
        read(&c, &mut e, "key/github/work/A").as_deref(),
        Some(SECRET)
    );
}

#[test]
fn the_value_is_in_no_result_card_error_or_log() {
    let (_t, _l, mut c) = core();
    instrument(&c);
    let store = MemoryKeyStore::default();
    let mut e = env(&store);
    let k = key_card(&mut c, "github");
    let params =
        json!({"id": k.id, "account": "work", "values": {"A": SECRET, "B": "second-value"}});
    let out = provide_row_with(&mut c, &mut e, &params, &|| T0 + 1, &Nobody).unwrap();
    assert_eq!(out["result"], "saved");
    assert_eq!(out["profile"], "github/work");
    assert_eq!(out["vars"], json!(["A", "B"]));
    let spent = stored(&mut c, &k.id);
    let everything = format!("{out} {} {spent:?}", spent.json());
    assert!(!everything.contains(SECRET) && !everything.contains("second-value"));
    // The table holds no plaintext either.
    let blobs: Vec<Vec<u8>> = {
        let mut st = c
            .conn()
            .prepare("SELECT ciphertext FROM secret_records")
            .unwrap();
        st.query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(blobs.len(), 2);
    assert!(blobs.iter().all(|b| !b.windows(7).any(|w| w == b"hunter2")));
    // Refusals name the field, never the value: too long, wrong shape, wrong account.
    let long = "x".repeat(2000);
    let k2 = key_card(&mut c, "slack");
    for (account, values) in [
        (Some("work"), vals(&[("A", long.as_str()), ("B", SECRET)])),
        (Some("work"), vals(&[("A", SECRET)])),
        (Some("Bad Name"), vals(&[("A", SECRET), ("B", SECRET)])),
        (Some("work"), vals(&[("A", SECRET), ("B", "")])),
    ] {
        let err = provide(
            &mut c,
            &mut e,
            &k2.id,
            account,
            &values,
            &|| T0 + 1,
            &Nobody,
        )
        .expect_err("refused");
        assert!(matches!(err, Error::BadParams(_)), "{err}");
        assert!(!err.to_string().contains(SECRET), "{err}");
    }
    // The row names the missing param, and a non-text value is refused without printing it.
    let err = call(&mut c, "cards.provide", &json!({"id": k2.id}), &Nobody).expect_err("no values");
    assert!(
        matches!(&err, Error::BadParams(m) if m.contains("values")),
        "{err}"
    );
    let bad = json!({"id": k2.id, "account": "work", "values": {"A": 12345, "B": SECRET}});
    let err = provide_row_with(&mut c, &mut e, &bad, &|| T0 + 1, &Nobody).expect_err("not text");
    assert!(!err.to_string().contains(SECRET) && !err.to_string().contains("12345"));
    // None of that took the claim or wrote anything.
    let held = stored(&mut c, &k2.id);
    assert_eq!((held.state, held.claim_by), (State::Pending, None));
    assert_eq!(writes(&c, "slack"), 0);
}

// ---- only while open; never after a no or the clock

#[test]
fn nothing_is_written_to_a_card_that_closed() {
    let (_t, _l, mut c) = core();
    instrument(&c);
    let store = MemoryKeyStore::default();
    let mut e = env(&store);
    // A no: recorded, and every later provide refuses.
    let denied = key_card(&mut c, "a1");
    assert!(matches!(
        no(&mut c, &denied.id, T0 + 1),
        Answered::Applied { .. }
    ));
    for n in 0..2 {
        assert_eq!(
            refused(prov(
                &mut c,
                &mut e,
                &denied.id,
                &two(),
                T0 + 2 + n,
                &Nobody
            )),
            Refused::Closed(Some(State::Denied))
        );
    }
    // The asker cancelled.
    let gone = key_card(&mut c, "a2");
    assert!(withdraw(&mut c, &gone.id, T0 + 1, &Nobody).unwrap());
    assert_eq!(
        refused(prov(&mut c, &mut e, &gone.id, &two(), T0 + 2, &Nobody)),
        Refused::Closed(Some(State::Withdrawn))
    );
    // The clock closed it.
    let late = key_card(&mut c, "a3");
    assert_eq!(sweep(&mut c, DEADLINE + 1, &Nobody).unwrap().len(), 1);
    assert_eq!(
        refused(prov(
            &mut c,
            &mut e,
            &late.id,
            &two(),
            DEADLINE + 2,
            &Nobody
        )),
        Refused::Closed(Some(State::Expired))
    );
    // Past its deadline but not swept yet: refused as expired, and not written.
    let unswept = key_card(&mut c, "a4");
    assert_eq!(
        refused(prov(
            &mut c,
            &mut e,
            &unswept.id,
            &two(),
            DEADLINE + 5,
            &Nobody
        )),
        Refused::Expired
    );
    assert_eq!(stored(&mut c, &unswept.id).state, State::Pending);
    // No such card.
    assert!(prov(&mut c, &mut e, "nope", &two(), T0, &Nobody).is_err());
    for svc in ["a1", "a2", "a3", "a4"] {
        assert_eq!(writes(&c, svc), 0, "{svc}");
    }
    assert_eq!(Envelope::list(c.conn()).unwrap(), Vec::<String>::new());
}

#[test]
fn only_a_key_card_is_provided() {
    let (_t, _l, mut c) = core();
    let store = MemoryKeyStore::default();
    let mut e = env(&store);
    let n = |tool: &str, input: &str, res: &str| NewCard {
        session: "s1".into(),
        source: "run",
        bot_id: None,
        run_id: None,
        person: String::new(),
        agent: "echo".into(),
        answerable_by: None,
        in_grants: false,
        tool: tool.into(),
        input: input.into(),
        resource: res.into(),
        shown: res.into(),
        deadline: DEADLINE,
        reply_to: None,
        asked_by: None,
        options: None,
    };
    let id_of = |r: Raised| match r {
        Raised::New(k) => k.id,
        other => panic!("{other:?}"),
    };
    let plain = id_of(raise(&mut c, &n("Bash", "{}", "x"), T0, &Nobody).unwrap());
    let escape = id_of(raise(&mut c, &n("request_access", "{}", "y"), T0, &Nobody).unwrap());
    for id in [plain, escape] {
        let err = prov(&mut c, &mut e, &id, &two(), T0, &Nobody).expect_err("not a key card");
        assert!(matches!(err, Error::NotAllowed(_)), "{err}");
        assert_eq!(stored(&mut c, &id).state, State::Pending);
    }
    // A key card that does not say what it asks for is not guessed at.
    for input in [
        "",
        "not json",
        "{}",
        r#"{"service":"gh","vars":[]}"#,
        r#"{"service":"GH!","vars":["A"]}"#,
    ] {
        let id = id_of(
            raise(
                &mut c,
                &n("request_secret", input, &format!("z{input}")),
                T0,
                &Nobody,
            )
            .unwrap(),
        );
        let err = prov(&mut c, &mut e, &id, &two(), T0, &Nobody).expect_err("unreadable");
        assert!(matches!(err, Error::Refused(_)), "{input}: {err}");
        assert_eq!(stored(&mut c, &id).state, State::Pending);
    }
    assert_eq!(Envelope::list(c.conn()).unwrap(), Vec::<String>::new());
}

#[test]
fn a_yes_by_the_answer_row_never_spends_a_secret_card_only_saved_does() {
    let (_t, _l, mut c) = core();
    let k = key_card(&mut c, "github");
    let r = Reply {
        id: &k.id,
        allow: true,
        answer: "allow",
        by: "owner",
        spend: true,
    };
    assert!(matches!(
        answer(&mut c, &r, T0 + 1, &Nobody),
        Err(Error::NotAllowed(_))
    ));
    assert_eq!(stored(&mut c, &k.id).state, State::Pending);
    let store = MemoryKeyStore::default();
    let mut e = env(&store);
    let (done, _) = saved(prov(&mut c, &mut e, &k.id, &two(), T0 + 2, &Nobody));
    assert_eq!(
        (done.state, done.answer.as_deref()),
        (State::Used, Some("saved"))
    );
}

// ---- a failed write leaves the card open

#[test]
fn a_failed_write_changes_nothing_and_leaves_the_card_open_to_a_retry() {
    let (_t, _l, mut c) = core();
    instrument(&c);
    let store = MemoryKeyStore::default();
    let k = key_card(&mut c, "github");
    // The second record fails after the first went in: the whole commit rolls back.
    fail_writes(&c, true);
    let err =
        prov(&mut c, &mut env(&store), &k.id, &two(), T0 + 1, &Nobody).expect_err("disk full");
    assert!(!err.to_string().contains(SECRET), "{err}");
    let open = stored(&mut c, &k.id);
    assert_eq!(open.state, State::Pending, "still open");
    assert!(
        open.claim_by.is_none() && open.answer.is_none(),
        "claim released, nothing recorded"
    );
    assert_eq!(writes(&c, "github"), 0);
    assert!(
        Envelope::list(c.conn()).unwrap().is_empty(),
        "no half a key"
    );
    // A locked key store fails before the write transaction, with the same result.
    fail_writes(&c, false);
    store.set_unreadable(true);
    let err = prov(&mut c, &mut env(&store), &k.id, &two(), T0 + 2, &Nobody).expect_err("locked");
    assert!(!err.to_string().contains(SECRET), "{err}");
    assert_eq!(stored(&mut c, &k.id).state, State::Pending);
    assert_eq!(writes(&c, "github"), 0);
    // The retry is the one write.
    store.set_unreadable(false);
    saved(prov(
        &mut c,
        &mut env(&store),
        &k.id,
        &two(),
        T0 + 3,
        &Nobody,
    ));
    assert_eq!(writes(&c, "github"), 2);
}

// ---- the claim: two devices, a lost claim, a crashed writer

#[test]
fn a_second_device_is_refused_while_the_first_holds_the_claim() {
    let (_t, _l, mut c) = core();
    instrument(&c);
    let store = MemoryKeyStore::default();
    let mut e = env(&store);
    let k = key_card(&mut c, "github");
    let plan = plan_of(&mut c, &k.id, &two());
    let first = begin(&mut c, &k.id, T0 + 1, &Nobody)
        .unwrap()
        .expect("first claims");
    // The second device, a plain answer, a no, the sweep and a withdrawal all find it taken.
    let r = refused(prov(&mut c, &mut e, &k.id, &two(), T0 + 2, &Nobody));
    assert_eq!(r, Refused::Claimed(first.holder.clone()));
    assert_eq!(
        no(&mut c, &k.id, T0 + 2),
        Answered::Claimed(first.holder.clone())
    );
    assert_eq!(sweep(&mut c, DEADLINE + 1, &Nobody).unwrap().len(), 0);
    assert!(!withdraw(&mut c, &k.id, DEADLINE + 1, &Nobody).unwrap());
    // The first writes and saves; the second then finds the card spent.
    commit(&mut c, &mut e, &first, &plan, &two(), T0 + 3, &Nobody).unwrap();
    assert_eq!(
        refused(prov(&mut c, &mut e, &k.id, &two(), T0 + 4, &Nobody)),
        Refused::Closed(Some(State::Used))
    );
    assert_eq!(writes(&c, "github"), 2);
}

#[test]
fn a_writer_that_lost_its_claim_writes_nothing() {
    let (_t, _l, mut c) = core();
    instrument(&c);
    let store = MemoryKeyStore::default();
    let mut e = env(&store);
    let k = key_card(&mut c, "github");
    let plan = plan_of(&mut c, &k.id, &two());
    let zombie = begin(&mut c, &k.id, T0 + 1, &Nobody)
        .unwrap()
        .expect("claims");
    // Its process is gone (crashed): the claim is dropped and a second writer takes the card and saves it.
    let second = begin(&mut c, &k.id, T0 + 2, &Gone)
        .unwrap()
        .expect("takes over");
    assert_ne!(second.holder, zombie.holder);
    let done = commit(&mut c, &mut e, &second, &plan, &two(), T0 + 2, &Nobody).unwrap();
    assert_eq!(done.state, State::Used);
    assert_eq!(writes(&c, "github"), 2);
    // The first writer wakes up after all: its commit finds the claim gone and the card spent, and changes nothing.
    let stale = vals(&[("A", "zombie-a"), ("B", "zombie-b")]);
    let err = commit(&mut c, &mut e, &zombie, &plan, &stale, T0 + 3, &Nobody).expect_err("lost");
    assert!(matches!(err, Error::Refused(_)), "{err}");
    assert_eq!(writes(&c, "github"), 2);
    assert_eq!(
        read(&c, &mut e, "key/github/work/A").as_deref(),
        Some(SECRET)
    );
    // Nor does a writer whose card was closed under a claim that had run out (a claim of no pid) save anything.
    let k2 = key_card(&mut c, "slack");
    let plan2 = plan_of(&mut c, &k2.id, &two());
    let timed = crate::cards::Claim {
        holder: "e1",
        pid: None,
        until: T0 + 10,
    };
    claim(&mut c, &k2.id, &timed, T0 + 1, &Nobody)
        .unwrap()
        .expect("claimed");
    assert_eq!(sweep(&mut c, DEADLINE + 1, &Nobody).unwrap().len(), 1);
    let held = Held {
        id: k2.id.clone(),
        holder: "e1".into(),
    };
    assert!(commit(&mut c, &mut e, &held, &plan2, &two(), DEADLINE + 2, &Nobody).is_err());
    assert_eq!(writes(&c, "slack"), 0);
}

#[test]
fn a_claim_that_ran_out_by_time_is_not_taken_over_while_its_writer_lives() {
    // M2-11 precondition 1. The claim's time and the card's own deadline have both passed; the writer's pid is alive.
    let (_t, _l, mut c) = core();
    instrument(&c);
    let store = MemoryKeyStore::default();
    let mut e = env(&store);
    let k = key_card_with(&mut c, "github", T0 + 100, "r1");
    let plan = plan_of(&mut c, &k.id, &two());
    let first = begin(&mut c, &k.id, T0 + 1, &Started("e1"))
        .unwrap()
        .expect("claims");
    let late = T0 + 1 + CLAIM_MS + 1_000;
    let live = Started("e1");
    // Nobody takes it over, answers it, withdraws it or sweeps it.
    assert_eq!(
        refused(prov(&mut c, &mut e, &k.id, &two(), late, &live)),
        Refused::Claimed(first.holder.clone())
    );
    let other = crate::cards::Claim {
        holder: "w2",
        pid: None,
        until: late + 100,
    };
    assert!(claim(&mut c, &k.id, &other, late, &live).unwrap().is_none());
    assert_eq!(
        answer(
            &mut c,
            &Reply {
                id: &k.id,
                allow: false,
                answer: "deny",
                by: "owner",
                spend: false
            },
            late,
            &live
        )
        .unwrap(),
        Answered::Claimed(first.holder.clone())
    );
    assert!(!withdraw(&mut c, &k.id, late, &live).unwrap());
    assert_eq!(sweep(&mut c, late, &live).unwrap().len(), 0);
    assert!(matches!(
        raise_again(&mut c, "github", late, &live),
        Raised::Joined(_)
    ));
    assert_eq!(
        stored(&mut c, &k.id).claim_by.as_deref(),
        Some(first.holder.as_str())
    );
    // The writer finishes late and its save still lands, once.
    commit(&mut c, &mut e, &first, &plan, &two(), late + 1, &live).unwrap();
    assert_eq!(stored(&mut c, &k.id).state, State::Used);
    assert_eq!(writes(&c, "github"), 2);

    // Its process is gone, or has another start time (pid reuse): the claim is dropped and the clock may close the card.
    let k2 = key_card_with(&mut c, "slack", T0 + 100, "r1");
    begin(&mut c, &k2.id, T0 + 1, &Started("e1"))
        .unwrap()
        .expect("claims");
    assert_eq!(sweep(&mut c, late, &Started("e1")).unwrap().len(), 0);
    assert_eq!(sweep(&mut c, late, &Started("e2")).unwrap().len(), 1);
    assert_eq!(stored(&mut c, &k2.id).state, State::Expired);
    let k3 = key_card_with(&mut c, "teams", T0 + 100, "r1");
    begin(&mut c, &k3.id, T0 + 1, &Nobody)
        .unwrap()
        .expect("claims");
    assert_eq!(sweep(&mut c, late, &Gone).unwrap().len(), 1);
    assert_eq!(writes(&c, "slack") + writes(&c, "teams"), 0);
}

fn raise_again(c: &mut Core, svc: &str, now: i64, live: &dyn Liveness) -> Raised {
    // The same ask while the claimed card is open joins it instead of making a second card.
    let n = NewCard {
        session: "s1".into(),
        source: "run",
        bot_id: None,
        run_id: Some("r1".into()),
        person: String::new(),
        agent: "echo".into(),
        answerable_by: None,
        in_grants: false,
        tool: "request_secret".into(),
        input: json!({"service": svc, "vars": ["A", "B"]}).to_string(),
        resource: svc.into(),
        shown: svc.into(),
        deadline: now + 1_000,
        reply_to: None,
        asked_by: None,
        options: None,
    };
    raise(c, &n, now, live).unwrap()
}

#[test]
fn the_writer_is_the_claimer_and_does_not_outlive_it() {
    // M2-11 precondition 2. The claim names this very process, so it is held exactly as long as the writer lives, and the
    // key store's child tools are waited for (and killed at their timeout) inside the same call.
    let (_t, _l, mut c) = core();
    let k = key_card(&mut c, "github");
    let held = begin(&mut c, &k.id, T0 + 1, &crate::gate::System)
        .unwrap()
        .expect("claims");
    let (pid, started, until): (Option<i64>, String, Option<i64>) = c
        .conn()
        .query_row(
            "SELECT claim_pid, claim_started, claim_until FROM cards WHERE id = ?1",
            [&held.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(pid, Some(i64::from(std::process::id())));
    assert_eq!(until, Some(T0 + 1 + CLAIM_MS));
    if let Some(now_started) = Liveness::start(&crate::gate::System, std::process::id()) {
        assert!(
            started.starts_with(['e', 'b'])
                && crate::gate::holder_alive(&crate::gate::System, std::process::id(), &started),
            "the start stamp is recorded for the pid-reuse check: {started} vs {now_started}"
        );
    }
    // The claim outlasts the key store's tool bound (30 s a tool, at most three tools) twice over.
    assert!(until.unwrap() - (T0 + 1) >= 2 * 3 * 30_000);
}

#[test]
fn a_provide_with_the_writer_parked_or_its_run_gone_still_saves() {
    let (_t, _l, mut c) = core();
    instrument(&c);
    let store = MemoryKeyStore::default();
    let mut e = env(&store);
    // The asker's process is gone: the card parks and is still open.
    let k = key_card(&mut c, "github");
    assert_eq!(park_run(&mut c, "r1").unwrap().len(), 1);
    assert_eq!(stored(&mut c, &k.id).state, State::Parked);
    let (done, _) = saved(prov(&mut c, &mut e, &k.id, &two(), T0 + 1, &Nobody));
    assert_eq!(done.state, State::Used);
    assert!(done.answered_parked, "the owed resume stays owed");
    // Parking and withdrawing while a writer holds the claim leave the card to it.
    let k2 = key_card_with(&mut c, "slack", DEADLINE, "r2");
    let plan = plan_of(&mut c, &k2.id, &two());
    let held = begin(&mut c, &k2.id, T0 + 1, &Nobody)
        .unwrap()
        .expect("claims");
    assert_eq!(park_run(&mut c, "r2").unwrap().len(), 1);
    assert!(!withdraw(&mut c, &k2.id, T0 + 2, &Nobody).unwrap());
    commit(&mut c, &mut e, &held, &plan, &two(), T0 + 3, &Nobody).unwrap();
    assert_eq!(stored(&mut c, &k2.id).state, State::Used);
    assert_eq!(writes(&c, "slack"), 2);
}

#[test]
fn a_new_account_never_replaces_a_key_but_a_replacing_card_does() {
    let (_t, _l, mut c) = core();
    let store = MemoryKeyStore::default();
    let mut e = env(&store);
    let first = key_card(&mut c, "github");
    saved(prov(&mut c, &mut e, &first.id, &two(), T0 + 1, &Nobody));
    // Another card for the same service asking for a new account of the same name is refused, and stays open.
    let k2 = key_card_with(&mut c, "github", DEADLINE, "r9");
    let err = prov(
        &mut c,
        &mut e,
        &k2.id,
        &vals(&[("A", "zzz"), ("B", "yyy")]),
        T0 + 2,
        &Nobody,
    )
    .expect_err("taken");
    assert!(matches!(err, Error::BadParams(_)), "{err}");
    assert_eq!(stored(&mut c, &k2.id).state, State::Pending);
    assert!(stored(&mut c, &k2.id).claim_by.is_none());
    assert_eq!(
        read(&c, &mut e, "key/github/work/A").as_deref(),
        Some(SECRET)
    );
    // A card that replaces names the account itself; the caller's account is ignored.
    let ask = NewCard {
        session: "s2".into(),
        source: "run",
        bot_id: None,
        run_id: None,
        person: String::new(),
        agent: "echo".into(),
        answerable_by: None,
        in_grants: false,
        tool: "request_secret".into(),
        input: json!({"service": "github", "vars": ["A", "B"], "replaces": true, "profile": "github/work"}).to_string(),
        resource: "github-replace".into(),
        shown: "github".into(),
        deadline: DEADLINE,
        reply_to: None,
        asked_by: None,
        options: None,
    };
    let Raised::New(repl) = raise(&mut c, &ask, T0, &Nobody).unwrap() else {
        panic!("new")
    };
    let (_, profile) = saved(provide(
        &mut c,
        &mut e,
        &repl.id,
        Some("elsewhere"),
        &vals(&[("A", "rotated-a"), ("B", "rotated-b")]),
        &|| T0 + 3,
        &Nobody,
    ));
    assert_eq!(profile, "github/work");
    assert_eq!(
        read(&c, &mut e, "key/github/work/A").as_deref(),
        Some("rotated-a")
    );
    assert_eq!(Envelope::list(c.conn()).unwrap().len(), 2);
}

// One Core per process, as in the daemon: the threads share one behind a Mutex. What this checks is that exactly one
// provide writes, decided in the rows (the claim and the save), however the devices interleave.
#[test]
fn eight_devices_providing_one_card_write_it_once() {
    for round in 0..10 {
        let (_t, _l, mut c) = core();
        instrument(&c);
        let k = key_card(&mut c, &format!("svc{round}"));
        let id = k.id;
        let c = Arc::new(Mutex::new(c));
        let store = MemoryKeyStore::default();
        let go = Arc::new(Barrier::new(8));
        let workers: Vec<_> = (0..8)
            .map(|n| {
                let (c, go, id, store) = (c.clone(), go.clone(), id.clone(), store.clone());
                std::thread::spawn(move || {
                    go.wait();
                    let values = pair(&format!("a{n}"), &format!("b{n}"));
                    let mut core = c.lock().unwrap();
                    matches!(
                        prov(&mut core, &mut env(&store), &id, &values, T0 + 1, &Nobody).unwrap(),
                        Provided::Saved { .. }
                    )
                })
            })
            .collect();
        let won = workers
            .into_iter()
            .map(|w| w.join().unwrap())
            .filter(|w| *w)
            .count();
        assert_eq!(won, 1, "round {round}");
        let core = c.lock().unwrap();
        assert_eq!(writes(&core, &format!("svc{round}")), 2, "round {round}");
    }
}

// ---- a randomised sequence

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

struct Flight {
    held: Held,
    plan: super::Plan,
    values: BTreeMap<String, Zeroizing<String>>,
}

#[test]
fn random_sequences_write_a_secret_card_at_most_once_and_never_after_it_closed() {
    for seed in 0..40u64 {
        let (_t, _l, mut c) = core();
        instrument(&c);
        let store = MemoryKeyStore::default();
        let mut rng = Rng(seed + 11);
        let now = Cell::new(T0);
        let mut cards: Vec<(String, String)> = Vec::new(); // (id, service)
        let mut flights: Vec<Flight> = Vec::new();
        let mut seen: BTreeMap<String, Card> = BTreeMap::new();
        for step in 0..80 {
            let op = rng.below(13);
            let label = format!("seed {seed} step {step} op {op}");
            let pick = |rng: &mut Rng| -> Option<usize> {
                (!cards.is_empty()).then(|| {
                    usize::try_from(rng.below(u64::try_from(cards.len()).unwrap())).unwrap()
                })
            };
            // The claims of live writers in flight before the op: only their own commit or a crash may end them.
            let live_before: Vec<(String, String)> = flights
                .iter()
                .filter(|fl| {
                    let cur = stored(&mut c, &fl.held.id);
                    cur.claim_by.as_deref() == Some(fl.held.holder.as_str())
                })
                .map(|fl| (fl.held.id.clone(), fl.held.holder.clone()))
                .collect();
            let mut ends_a_claim = false;
            let mut failed_commit: Option<(String, Card)> = None;
            match op {
                0 => {
                    let svc = format!("s{}", cards.len());
                    let cur = key_card_with(
                        &mut c,
                        &svc,
                        now.get() + 50 + i64::try_from(rng.below(300)).unwrap(),
                        &format!("r{}", rng.below(2)),
                    );
                    cards.push((cur.id, svc));
                }
                1 | 2 => {
                    // provide, with the write or the key store failing now and then
                    let Some(idx) = pick(&mut rng) else { continue };
                    let id = cards[idx].0.clone();
                    let before = stored(&mut c, &id);
                    let flaky = rng.below(4);
                    fail_writes(&c, flaky == 0);
                    store.set_unreadable(flaky == 1);
                    let values = pair(&format!("a-{step}"), &format!("b-{step}"));
                    let res = prov(&mut c, &mut env(&store), &id, &values, now.get(), &Nobody);
                    fail_writes(&c, false);
                    store.set_unreadable(false);
                    match res {
                        Ok(Provided::Saved { .. }) => {
                            assert!(
                                before.state.open(),
                                "{label}: saved a {:?} card",
                                before.state
                            );
                            assert!(
                                before.holder(now.get()).is_none(),
                                "{label}: saved over a claim"
                            );
                        }
                        Ok(Provided::Refused(_)) => {}
                        Err(_) => failed_commit = Some((id, before)),
                    }
                }
                3 => {
                    if let Some(idx) = pick(&mut rng) {
                        no(&mut c, &cards[idx].0, now.get());
                    }
                }
                4 => {
                    sweep(&mut c, now.get(), &Nobody).unwrap();
                }
                5 => {
                    park_run(&mut c, &format!("r{}", rng.below(2))).unwrap();
                }
                6 => {
                    if let Some(idx) = pick(&mut rng) {
                        withdraw(&mut c, &cards[idx].0, now.get(), &Nobody).unwrap();
                    }
                }
                7 => {
                    // a writer takes the claim and is still going
                    if let Some(idx) = pick(&mut rng) {
                        let id = cards[idx].0.clone();
                        let values = pair(&format!("fa-{step}"), &format!("fb-{step}"));
                        let plan = plan_of(&mut c, &id, &values);
                        if let Ok(held) = begin(&mut c, &id, now.get(), &Nobody).unwrap() {
                            flights.push(Flight { held, plan, values });
                        }
                    }
                }
                8 | 9 => {
                    // a writer in flight finishes (or finds it lost)
                    if flights.is_empty() {
                        continue;
                    }
                    let idx =
                        usize::try_from(rng.below(u64::try_from(flights.len()).unwrap())).unwrap();
                    let fl = flights.swap_remove(idx);
                    let cur = stored(&mut c, &fl.held.id);
                    let mine = cur.state.open()
                        && cur.claim_by.as_deref() == Some(fl.held.holder.as_str());
                    let res = commit(
                        &mut c,
                        &mut env(&store),
                        &fl.held,
                        &fl.plan,
                        &fl.values,
                        now.get(),
                        &Nobody,
                    );
                    assert_eq!(
                        res.is_ok(),
                        mine,
                        "{label}: a commit lands exactly when the claim is still ours"
                    );
                    if res.is_err() {
                        release(&mut c, &fl.held.id, &fl.held.holder, &Nobody).unwrap();
                    }
                    ends_a_claim = true;
                }
                10 => {
                    // every writer crashes: their pids are gone
                    sweep(&mut c, now.get(), &Gone).unwrap();
                    ends_a_claim = true;
                }
                11 => now.set(now.get() + i64::try_from(rng.below(120)).unwrap()),
                _ => now.set(now.get() + CLAIM_MS + 1), // the claims' time runs out under live writers
            }
            // A live writer's claim outlasts the clock, the sweep, a no, a withdrawal, a park and a second provide.
            if !ends_a_claim {
                for (id, holder) in &live_before {
                    let cur = stored(&mut c, id);
                    assert!(
                        cur.state.open(),
                        "{label}: card {id} closed under its writer"
                    );
                    assert_eq!(
                        cur.claim_by.as_deref(),
                        Some(holder.as_str()),
                        "{label}: claim on {id} lost"
                    );
                }
            }
            // A failed provide changed nothing the card shows: it is as open as it was, with its claim released.
            if let Some((id, before)) = &failed_commit {
                let cur = stored(&mut c, id);
                if before.holder(now.get()).is_none() && before.state.open() {
                    assert_eq!(
                        cur.state, before.state,
                        "{label}: a failed write moved the card"
                    );
                    assert_eq!(
                        cur.answer, None,
                        "{label}: a failed write recorded an answer"
                    );
                }
            }
            // The invariants, on every card, after every step.
            for (id, svc) in &cards {
                let cur = stored(&mut c, id);
                let n_w = writes(&c, svc);
                assert!(n_w == 0 || n_w == 2, "{label}: card {id} has {n_w} writes");
                assert_eq!(
                    n_w == 2,
                    cur.state == State::Used,
                    "{label}: card {id} written iff saved ({:?})",
                    cur.state
                );
                if matches!(cur.state, State::Denied | State::Expired | State::Withdrawn) {
                    assert_eq!(n_w, 0, "{label}: card {id} written after it closed");
                }
                if let Some(old) = seen.get(id) {
                    if matches!(
                        old.state,
                        State::Denied | State::Expired | State::Withdrawn | State::Used
                    ) {
                        assert_eq!(old.state, cur.state, "{label}: card {id} came back");
                    }
                    if old.state == State::Used {
                        assert_eq!(
                            (old.answered_at, &old.answer),
                            (cur.answered_at, &cur.answer),
                            "{label}: saved twice"
                        );
                    }
                }
                if !cur.state.open() {
                    assert!(
                        cur.claim_by.is_none(),
                        "{label}: a closed card holds a claim"
                    );
                }
                seen.insert(id.clone(), cur);
            }
        }
    }
}

#[test]
fn the_models_secret_rows_stay_off_the_answer_row() {
    // cards.answer still refuses a key card's yes and points to the app; cards.provide is the one way in.
    let (_t, _l, mut c) = core();
    let k = key_card(&mut c, "github");
    let err = call(
        &mut c,
        "cards.answer",
        &json!({"id": k.id, "answer": "allow"}),
        &Nobody,
    )
    .expect_err("refused");
    assert!(
        matches!(&err, Error::NotAllowed(m) if m.contains("Kith app")),
        "{err}"
    );
}

#[test]
fn a_claim_that_could_not_be_released_is_freed_by_the_next_row_call() {
    // The release after a failed write can fail too (a busy database). The claim then names this very live process, which
    // nothing else would ever drop; rows are served one at a time, so at the next call none of ours is in flight.
    let (_t, _l, mut c) = core();
    let k = key_card(&mut c, "github");
    let stale = begin(&mut c, &k.id, T0 + 1, &Nobody)
        .unwrap()
        .expect("claims");
    assert_eq!(
        stored(&mut c, &k.id).claim_by.as_deref(),
        Some(stale.holder.as_str())
    );
    call(&mut c, "cards.list", &json!({}), &Nobody).unwrap();
    assert!(stored(&mut c, &k.id).claim_by.is_none());
    // A claim of another process, or one with no pid, is left alone.
    let other = crate::cards::Claim {
        holder: "provide:elsewhere",
        pid: Some(std::process::id().wrapping_add(1)),
        until: T0 + CLAIM_MS,
    };
    claim(&mut c, &k.id, &other, T0 + 2, &Nobody)
        .unwrap()
        .expect("claims");
    call(&mut c, "cards.list", &json!({}), &Nobody).unwrap();
    assert_eq!(
        stored(&mut c, &k.id).claim_by.as_deref(),
        Some("provide:elsewhere")
    );
}

#[test]
fn a_damaged_record_does_not_make_a_card_unprovidable_and_an_account_is_taken_by_prefix() {
    let (_t, _l, mut c) = core();
    let store = MemoryKeyStore::default();
    let mut e = env(&store);
    // An older key of the account with a variable this card does not ask for: the account is taken.
    e.put(c.conn(), "key/github/work/OLD_VAR", b"x").unwrap();
    let k = key_card(&mut c, "github");
    let err = prov(&mut c, &mut e, &k.id, &two(), T0 + 1, &Nobody).expect_err("taken");
    assert!(matches!(err, Error::BadParams(_)), "{err}");
    assert_eq!(stored(&mut c, &k.id).state, State::Pending);
    // A record that no longer opens does not make the card unprovidable: only the key is read to warm up.
    c.conn()
        .execute(
            "UPDATE secret_records SET ciphertext = x'00' WHERE name = 'key/github/work/OLD_VAR'",
            [],
        )
        .unwrap();
    let mut fresh = env(&store);
    let (done, _) = saved(provide(
        &mut c,
        &mut fresh,
        &k.id,
        Some("home"),
        &two(),
        &|| T0 + 2,
        &Nobody,
    ));
    assert_eq!(done.state, State::Used);
}

#[test]
fn a_revoke_under_a_held_claim_closes_the_card_and_the_holders_write_is_refused() {
    let (_t, _l, mut c) = core();
    instrument(&c);
    let store = MemoryKeyStore::default();
    let mut e = env(&store);
    let k = key_card_of(&mut c, "github", DEADLINE, "r1", "sam");
    let plan = plan_of(&mut c, &k.id, &two());
    let held = begin(&mut c, &k.id, T0 + 1, &Nobody)
        .unwrap()
        .expect("claims");
    // the person's suspension withdraws the card although a device holds the claim
    let tx = c.claim_tx().unwrap();
    let gone = crate::cards::withdraw_people(&tx, &["sam".to_owned()]).unwrap();
    tx.commit().unwrap();
    assert_eq!(gone, vec![k.id.clone()]);
    // the holder's write finds the card closed: nothing is saved, no row lands
    let _ = commit(&mut c, &mut e, &held, &plan, &two(), T0 + 2, &Nobody);
    assert_eq!(stored(&mut c, &k.id).state, State::Withdrawn);
    assert_eq!(writes(&c, "github"), 0);
    assert_eq!(
        refused(prov(&mut c, &mut e, &k.id, &two(), T0 + 3, &Nobody)),
        Refused::Closed(Some(State::Withdrawn))
    );
    assert_eq!(writes(&c, "github"), 0);
}
