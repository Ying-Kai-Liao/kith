//! The kernel takers' claim rows (P5) on a real core.db: one writer, a retried claim and release, the warm wait, the
//! sweep over several conversations, and the refusals as data.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;

use kith_api::Principal;
use serde_json::{Value, json};

use super::{call_at, lockable};
use crate::conversations::claims::{self, Kind, NewClaim, reconcile};
use crate::conversations::warm::{self, WarmProc};
use crate::db::{Core, MIGRATIONS};
use crate::error::Error;
use crate::gate::{Liveness, System};
use crate::layout::Layout;
use crate::start_with;
use crate::tests::machine;

const MS: i64 = 1_900_000_000_000;
const HOST: &str = "kith-serve-host/1-e1";

/// The processes that are alive, as (pid, start stamp).
struct Alive(HashSet<(u32, String)>);

impl Alive {
    fn of(procs: &[(u32, &str)]) -> Self {
        Self(procs.iter().map(|(p, s)| (*p, (*s).to_owned())).collect())
    }
}

impl Liveness for Alive {
    fn start(&self, pid: u32) -> Option<String> {
        self.0
            .iter()
            .find(|(p, _)| *p == pid)
            .map(|(_, s)| s.clone())
    }
    fn gone(&self, pid: u32) -> bool {
        self.start(pid).is_none()
    }
}

fn world() -> (tempfile::TempDir, Core) {
    let (t, m) = machine();
    let mut core = start_with(&Layout::new(&m), MIGRATIONS).expect("start");
    reconcile(&mut core, &[], &[], i64::MAX, 1).expect("reconcile");
    (t, core)
}

fn ask(core: &mut Core, live: &Alive, method: &str, p: &Value) -> Value {
    call_at(core, method, p, MS, live).unwrap_or_else(|e| panic!("{method}: {e}"))
}

fn take(core: &mut Core, live: &Alive, sid: &str, pid: u32, started: &str, person: &str) -> Value {
    let mut p = json!({"conversation": sid, "pid": pid, "started": started, "person": person, "agent": "echo"});
    if !person.is_empty() {
        // A person's claim carries the epoch read before the may check.
        p["revokeSeen"] = json!(claims::revoke_epoch(core, person).unwrap());
    }
    ask(core, live, "claim.external", &p)
}

fn state_of(core: &Core, claim: i64) -> String {
    core.conn()
        .query_row(
            "SELECT state FROM claims WHERE claim_id = ?1",
            [claim],
            |r| r.get(0),
        )
        .unwrap()
}

fn conversation(core: &Core, sid: &str) {
    core.conn()
        .execute(
            "INSERT INTO conversations (id, agent, engine, created_at, updated_at) VALUES (?1, 'echo', 'claude', 1, 1)",
            [sid],
        )
        .unwrap();
}

fn count(core: &Core, table: &str) -> i64 {
    core.conn()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn the_rows_are_the_owners_and_the_hosts() {
    for name in ["claim.external", "claim.release", "claim.sweepPerson"] {
        let m = kith_api::method(name).unwrap_or_else(|| panic!("{name} is not in the table"));
        assert!(
            m.allows(Principal::Owner) && m.allows(Principal::Host),
            "{name}"
        );
        assert!(
            !m.allows(Principal::RunToken),
            "{name}: a run token reaches none of them"
        );
        assert!(m.cli.hidden, "{name} has no command line");
    }
}

#[test]
fn one_writer_and_a_repeated_claim_is_the_same_claim() {
    let (_t, mut core) = world();
    let live = Alive::of(&[(100, "e1"), (200, "e2")]);
    let first = take(&mut core, &live, "s1", 100, "e1", "");
    assert!(first["claimId"].is_i64(), "{first}");
    // The same process again (a retry after a lost answer, two calls at once): the same claim, no second row.
    let again = take(&mut core, &live, "s1", 100, "e1", "");
    assert_eq!(again["claimId"], first["claimId"], "{again}");
    assert_eq!(count(&core, "claims"), 1);
    // Another process is refused with the holder named.
    let other = take(&mut core, &live, "s1", 200, "e2", "");
    assert_eq!(other["refused"], "held", "{other}");
    assert_eq!(other["holder"]["pid"], 100);
    assert_eq!(other["holder"]["kind"], "external");
    // The conversation row was made for a session the core did not know.
    let agent: String = core
        .conn()
        .query_row("SELECT agent FROM conversations WHERE id = 's1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(agent, "echo");
}

#[test]
fn a_session_id_the_stub_cannot_lock_is_refused() {
    let (_t, mut core) = world();
    let live = Alive::of(&[(100, "e1")]);
    let long = "a".repeat(129);
    for id in [
        "",
        "A",
        "a/b",
        "../x",
        "..",
        ".",
        "a b",
        "\u{e9}",
        long.as_str(),
    ] {
        assert!(!lockable(id), "{id:?}");
        let e = call_at(
            &mut core,
            "claim.external",
            &json!({"conversation": id, "pid": 100, "started": "e1"}),
            MS,
            &live,
        );
        assert!(matches!(e, Err(Error::BadParams(_))), "{id:?}: {e:?}");
    }
    assert!(lockable("0a1b2c3d-4e5f.6_7"));
    assert_eq!(
        count(&core, "conversations"),
        0,
        "a refused id leaves no row"
    );
}

#[test]
fn a_dead_claimer_frees_the_conversation() {
    let (_t, mut core) = world();
    let live = Alive::of(&[(100, "e1")]);
    let first = take(&mut core, &live, "s1", 100, "e1", "");
    assert!(first["claimId"].is_i64());
    // The CLI died between claim and release: its pid is reused by another process (another start stamp).
    let after = Alive::of(&[(200, "e2"), (100, "e9")]);
    let next = take(&mut core, &after, "s1", 200, "e2", "");
    assert!(next["claimId"].is_i64(), "{next}");
    assert_eq!(state_of(&core, first["claimId"].as_i64().unwrap()), "ended");
}

#[test]
fn release_is_idempotent_and_touches_only_its_process() {
    let (_t, mut core) = world();
    let live = Alive::of(&[(100, "e1"), (200, "e2")]);
    let a = take(&mut core, &live, "s1", 100, "e1", "")["claimId"]
        .as_i64()
        .unwrap();
    let b = take(&mut core, &live, "s2", 200, "e2", "")["claimId"]
        .as_i64()
        .unwrap();
    // The wrong start stamp (a reused pid) releases nothing.
    let no = ask(
        &mut core,
        &live,
        "claim.release",
        &json!({"pid": 100, "started": "other"}),
    );
    assert_eq!(no["ended"], json!([]));
    assert_eq!(state_of(&core, a), "claimed");
    let yes = ask(
        &mut core,
        &live,
        "claim.release",
        &json!({"pid": 100, "started": "e1"}),
    );
    assert_eq!(yes["ended"], json!([a]));
    // A retried release, and a release of a claim that was never made, are quiet.
    for p in [
        json!({"pid": 100, "started": "e1"}),
        json!({"pid": 999, "started": "x"}),
    ] {
        assert_eq!(
            ask(&mut core, &live, "claim.release", &p)["ended"],
            json!([])
        );
    }
    assert_eq!(state_of(&core, b), "claimed");
    // The conversation is free again.
    assert!(take(&mut core, &live, "s1", 200, "e2", "")["refused"].is_null());
}

#[test]
fn release_leaves_a_terminals_claim_alone() {
    let (_t, mut core) = world();
    let live = Alive::of(&[(100, "e1")]);
    conversation(&core, "s1");
    let seen = claims::revoke_epoch(&core, "owner").unwrap();
    let req = NewClaim {
        conversation: "s1",
        kind: Kind::Terminal,
        person: "owner",
        turn: None,
        holder_instance: None,
        account: None,
        process: None,
        revoke_seen: seen,
    };
    let id = claims::claim(&mut core, &req, MS).unwrap().claim_id;
    // A terminal holds s1: a taker is told so, and a release by pid finds nothing of the terminal's.
    let held = take(&mut core, &live, "s1", 100, "e1", "");
    assert_eq!(held["refused"], "held");
    assert_eq!(held["holder"]["kind"], "terminal");
    let r = ask(
        &mut core,
        &live,
        "claim.release",
        &json!({"pid": 100, "started": "e1"}),
    );
    assert_eq!(r["ended"], json!([]));
    assert_eq!(state_of(&core, id), "claimed");
}

#[test]
fn a_queued_takeover_refuses_the_taker() {
    let (_t, mut core) = world();
    let live = Alive::of(&[(100, "e1")]);
    conversation(&core, "s1");
    core.conn()
        .execute(
            "INSERT INTO claims (claim_id, conversation, generation, kind, state, created_at)
             VALUES (9000, 's1', 1, 'terminal', 'queued', 1)",
            [],
        )
        .unwrap();
    let r = take(&mut core, &live, "s1", 100, "e1", "");
    assert_eq!(r["refused"], "takeover", "{r}");
}

fn warm_proc(core: &mut Core, pid: u32, started: &str, sid: &str) {
    warm::register(
        core,
        &WarmProc {
            pid,
            started,
            instance: HOST,
            person: "",
            agent: "echo",
            conversation: Some(sid),
            revoke_seen: None,
        },
    )
    .unwrap();
}

fn kill_at(core: &Core, pid: i64) -> Option<i64> {
    core.conn()
        .query_row(
            "SELECT kill_at FROM warm_procs WHERE pid = ?1",
            [pid],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn an_idle_warm_process_is_stopped_first_and_the_claim_follows_once_it_is_gone() {
    let (_t, mut core) = world();
    let live = Alive::of(&[(100, "e1"), (500, "w1")]);
    conversation(&core, "s1");
    warm_proc(&mut core, 500, "w1", "s1");
    let first = take(&mut core, &live, "s1", 100, "e1", "");
    assert_eq!(first["refused"], "waiting", "{first}");
    assert_eq!(
        kill_at(&core, 500),
        Some(MS),
        "the idle process is marked for the kill re-send"
    );
    assert_eq!(
        count(&core, "claims"),
        0,
        "no claim while the process lives"
    );
    // Still alive on the retry: still waiting.
    assert_eq!(
        take(&mut core, &live, "s1", 100, "e1", "")["refused"],
        "waiting"
    );
    // The process (and its group) is gone: the retry settles the row and claims.
    let after = Alive::of(&[(100, "e1")]);
    let done = take(&mut core, &after, "s1", 100, "e1", "");
    assert!(done["claimId"].is_i64(), "{done}");
    assert_eq!(count(&core, "warm_procs"), 0);
}

#[test]
fn a_running_turn_is_held_and_its_process_is_not_marked() {
    let (_t, mut core) = world();
    let live = Alive::of(&[(100, "e1"), (500, "w1")]);
    conversation(&core, "s1");
    warm_proc(&mut core, 500, "w1", "s1");
    core.conn()
        .execute(
            "INSERT INTO claims (claim_id, conversation, generation, kind, state, holder_instance, pid, pid_started, created_at)
             VALUES (9001, 's1', 1, 'turn', 'running', ?1, 500, 'w1', 1)",
            [HOST],
        )
        .unwrap();
    let r = take(&mut core, &live, "s1", 100, "e1", "");
    assert_eq!(r["refused"], "held", "{r}");
    assert_eq!(kill_at(&core, 500), None);
}

#[test]
fn claims_wait_for_the_reconcile() {
    let (_t, m) = machine();
    let mut core = start_with(&Layout::new(&m), MIGRATIONS).expect("start");
    let live = Alive::of(&[(100, "e1")]);
    let r = take(&mut core, &live, "s1", 100, "e1", "");
    assert_eq!(r["refused"], "waiting", "{r}");
}

#[test]
fn a_person_revoked_after_the_may_check_is_refused() {
    let (_t, mut core) = world();
    let live = Alive::of(&[(100, "e1")]);
    let seen = claims::revoke_epoch(&core, "sam").unwrap();
    ask(
        &mut core,
        &live,
        "claim.sweepPerson",
        &json!({"persons": ["sam"]}),
    );
    let stale = json!({"conversation": "s1", "pid": 100, "started": "e1", "person": "sam", "agent": "echo", "revokeSeen": seen});
    let r = ask(&mut core, &live, "claim.external", &stale);
    assert_eq!(r["refused"], "revoked", "{r}");
    // A fresh read of the epoch (the caller's may check came after the revoke) goes through.
    let fresh = take(&mut core, &live, "s1", 100, "e1", "sam");
    assert!(fresh["claimId"].is_i64(), "{fresh}");
    // The sweep that follows ends it; the same process asking again with the old epoch is refused, not handed its own row.
    ask(
        &mut core,
        &live,
        "claim.sweepPerson",
        &json!({"persons": ["sam"]}),
    );
    let retry = ask(&mut core, &live, "claim.external", &stale);
    assert_eq!(retry["refused"], "revoked", "{retry}");
    let again = take(&mut core, &live, "s1", 100, "e1", "sam");
    assert!(
        again["claimId"].is_null(),
        "an ending claim is not handed back: {again}"
    );
    assert_eq!(again["refused"], "held", "{again}");
}

#[test]
fn a_persons_claim_without_the_epoch_is_refused() {
    let (_t, mut core) = world();
    let live = Alive::of(&[(100, "e1")]);
    let e = call_at(
        &mut core,
        "claim.external",
        &json!({"conversation": "s1", "pid": 100, "started": "e1", "person": "sam", "agent": "echo"}),
        MS,
        &live,
    );
    assert!(matches!(e, Err(Error::BadParams(_))), "{e:?}");
    let epoch = |core: &mut Core, who: &str| {
        ask(core, &live, "claim.epoch", &json!({"person": who}))["epoch"].clone()
    };
    assert_eq!(epoch(&mut core, "sam"), 0);
    ask(
        &mut core,
        &live,
        "claim.sweepPerson",
        &json!({"persons": ["sam"]}),
    );
    assert_eq!(epoch(&mut core, "sam"), 1);
    assert_eq!(epoch(&mut core, ""), 0);
}

#[test]
fn a_conversation_has_an_agent() {
    let (_t, mut core) = world();
    let live = Alive::of(&[(100, "e1"), (200, "e2")]);
    // An unknown conversation with no agent is not made with an empty one.
    let e = call_at(
        &mut core,
        "claim.external",
        &json!({"conversation": "s1", "pid": 100, "started": "e1"}),
        MS,
        &live,
    );
    assert!(matches!(e, Err(Error::BadParams(_))), "{e:?}");
    assert_eq!(count(&core, "conversations"), 0);
    assert!(take(&mut core, &live, "s1", 100, "e1", "")["claimId"].is_i64());
    ask(
        &mut core,
        &live,
        "claim.release",
        &json!({"pid": 100, "started": "e1"}),
    );
    // The agent of a known conversation is kept, and another one is refused.
    let other = ask(
        &mut core,
        &live,
        "claim.external",
        &json!({"conversation": "s1", "pid": 200, "started": "e2", "agent": "kai"}),
    );
    assert_eq!(other["refused"], "agent", "{other}");
}

#[test]
fn a_persons_claim_is_not_given_back_by_a_release() {
    let (_t, mut core) = world();
    let live = Alive::of(&[(100, "e1")]);
    let id = take(&mut core, &live, "s1", 100, "e1", "sam")["claimId"]
        .as_i64()
        .unwrap();
    let r = ask(
        &mut core,
        &live,
        "claim.release",
        &json!({"pid": 100, "started": "e1"}),
    );
    assert_eq!(r["ended"], json!([]));
    assert_eq!(state_of(&core, id), "claimed");
    // It ends when its process is gone.
    claims::end_gone(&mut core, &Alive::of(&[]), MS).unwrap();
    assert_eq!(state_of(&core, id), "ended");
}

fn take_real(core: &mut Core, sid: &str, pid: u32, started: &str) -> Value {
    call_at(
        core,
        "claim.external",
        &json!({"conversation": sid, "pid": pid, "started": started, "agent": "echo"}),
        MS,
        &System,
    )
    .unwrap()
}

#[test]
fn a_live_process_is_not_read_as_gone_and_a_dead_one_is() {
    // The real process table, with the stamps the kernel's Python writes (core/procstart/tests/parity.rs holds the two
    // spellings equal): this test's own process is alive, a child that exited is gone.
    let (_t, mut core) = world();
    let me = std::process::id();
    let stamp = kith_kernel::procstart::start_stamp(me).expect("own stamp");
    let first = take_real(&mut core, "s1", me, &stamp);
    assert!(first["claimId"].is_i64(), "{first}");
    let other = take_real(&mut core, "s1", me + 1, "e0");
    assert_eq!(other["refused"], "held", "a live claimer keeps it: {other}");
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let dead = child.id();
    let dead_stamp = kith_kernel::procstart::start_stamp(dead).unwrap_or_else(|| "e1".into());
    child.wait().unwrap();
    let gone = take_real(&mut core, "s2", dead, &dead_stamp);
    assert!(gone["claimId"].is_i64(), "{gone}");
    // The next claim on s2 ends the dead claimer's row first.
    let next = take_real(&mut core, "s2", me, &stamp);
    assert!(
        next["claimId"].is_i64(),
        "a dead claimer is ended by the next claim: {next}"
    );
}

#[cfg(unix)]
#[test]
fn the_rows_serve_only_once_the_claims_are_switched() {
    use crate::cutover::state::{self, Phase, Switch};
    let (_t, m) = machine();
    let mut core = start_with(&Layout::new(&m), MIGRATIONS)
        .expect("start")
        .with_folders(&crate::tests::paths(&m));
    reconcile(&mut core, &[], &[], i64::MAX, 1).expect("reconcile");
    let p =
        json!({"conversation": "s1", "pid": std::process::id(), "started": "e1", "agent": "echo"});
    // Not switched: refused, whatever a marker file says.
    std::fs::create_dir_all(m.join("live/terminal")).unwrap();
    std::fs::write(m.join("live/terminal/claims.tombstone"), "{}").unwrap();
    for method in ["claim.external", "claim.list", "claim.epoch"] {
        let e = super::call(&mut core, method, &p);
        assert!(matches!(e, Err(Error::Refused(_))), "{method}: {e:?}");
    }
    assert_eq!(count(&core, "claims"), 0);
    // Switched (the file and the marker `kith migrate step` writes): served.
    state::write(
        &Layout::new(&m),
        "claims",
        &Switch {
            phase: Phase::Switched,
            snapshot: None,
            at: 5,
        },
    )
    .unwrap();
    crate::cutover::marker::write(&mut core, "claims", 5, "d", None).unwrap();
    let ok = super::call(&mut core, "claim.external", &p).unwrap();
    assert!(ok["claimId"].is_i64(), "{ok}");
    // A switch file with no marker in core.db (forged) denies again.
    crate::cutover::marker::clear(&mut core, "claims").unwrap();
    let e = super::call(&mut core, "claim.list", &json!({}));
    assert!(matches!(e, Err(Error::Refused(_))), "{e:?}");
}

#[test]
fn the_sweep_ends_a_persons_claims_in_every_conversation_and_not_the_owners() {
    let (_t, mut core) = world();
    let live = Alive::of(&[(100, "e1"), (200, "e2"), (300, "e3"), (400, "e4")]);
    let a = take(&mut core, &live, "s1", 100, "e1", "sam")["claimId"]
        .as_i64()
        .unwrap();
    let b = take(&mut core, &live, "s2", 200, "e2", "sam")["claimId"]
        .as_i64()
        .unwrap();
    let mine = take(&mut core, &live, "s3", 300, "e3", "")["claimId"]
        .as_i64()
        .unwrap();
    let bot = take(&mut core, &live, "s4", 400, "e4", "sam-bot")["claimId"]
        .as_i64()
        .unwrap();
    let out = ask(
        &mut core,
        &live,
        "claim.sweepPerson",
        &json!({"persons": ["sam"]}),
    );
    let mut pids: Vec<i64> = out["kill"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["pid"].as_i64().unwrap())
        .collect();
    pids.sort_unstable();
    assert_eq!(pids, vec![100, 200], "{out}");
    assert_eq!(state_of(&core, a), "ending");
    assert_eq!(state_of(&core, b), "ending");
    assert_eq!(state_of(&core, mine), "claimed");
    assert_eq!(
        state_of(&core, bot),
        "claimed",
        "a bot is swept by its own principal"
    );
    // Repeating answers the same kills; the rows end once the processes are gone.
    let again = ask(
        &mut core,
        &live,
        "claim.sweepPerson",
        &json!({"persons": ["sam", "sam-bot"]}),
    );
    assert_eq!(again["kill"].as_array().unwrap().len(), 3, "{again}");
    claims::end_gone(&mut core, &Alive::of(&[(300, "e3")]), MS).unwrap();
    assert_eq!(state_of(&core, a), "ended");
    assert_eq!(state_of(&core, mine), "claimed");
}

#[test]
fn the_owner_is_never_swept() {
    let (_t, mut core) = world();
    let live = Alive::of(&[]);
    for who in ["owner", ""] {
        let e = call_at(
            &mut core,
            "claim.sweepPerson",
            &json!({"persons": [who]}),
            MS,
            &live,
        );
        assert!(matches!(e, Err(Error::Refused(_))), "{who:?}: {e:?}");
    }
}

#[test]
fn list_shows_the_live_and_the_recent() {
    let (_t, mut core) = world();
    let live = Alive::of(&[(100, "e1"), (200, "e2")]);
    take(&mut core, &live, "s1", 100, "e1", "");
    take(&mut core, &live, "s2", 200, "e2", "");
    ask(
        &mut core,
        &live,
        "claim.release",
        &json!({"pid": 200, "started": "e2"}),
    );
    let l = ask(&mut core, &live, "claim.list", &json!({}));
    assert_eq!(l["live"].as_array().unwrap().len(), 1, "{l}");
    assert_eq!(l["live"][0]["conversation"], "s1");
    assert_eq!(l["live"][0]["pid"], 100);
    assert_eq!(l["recent"].as_array().unwrap().len(), 1);
}

/// MUST 4 of the P5 review: the revoke's sweep with no daemon, through the door the kernel's `kith-sessions sweep` really
/// uses (`call_authority_embedded`), not through `call_at`.
#[cfg(unix)]
#[test]
fn the_sweep_runs_in_an_embedded_core_when_switched_and_is_refused_when_not() {
    use crate::cutover::state::{self, Phase, Switch};
    use crate::{EmbeddedCall, call_authority_embedded};
    let (_t, m) = machine();
    let pid = std::process::id();
    let started = System.start(pid).expect("our own start stamp");
    let sweep = json!({"persons": ["sam"]});
    let paths = crate::tests::paths(&m);
    let claim;
    {
        let mut core = start_with(&Layout::new(&m), MIGRATIONS)
            .expect("start")
            .with_folders(&paths);
        reconcile(&mut core, &[], &[], i64::MAX, 1).expect("reconcile");
        state::write(
            &Layout::new(&m),
            "claims",
            &Switch {
                phase: Phase::Switched,
                snapshot: None,
                at: 5,
            },
        )
        .unwrap();
        crate::cutover::marker::write(&mut core, "claims", 5, "d", None).unwrap();
        let seen = claims::revoke_epoch(&core, "sam").unwrap();
        let took = super::call(
            &mut core,
            "claim.external",
            &json!({"conversation": "s1", "pid": pid, "started": started, "person": "sam", "agent": "echo", "revokeSeen": seen}),
        )
        .unwrap();
        claim = took["claimId"].as_i64().expect("a claim");
    } // the core is closed: no daemon holds the lock
    for round in 0..2 {
        // The second round is the repeat: the claim stays `ending`, and the kill is answered again.
        let EmbeddedCall::Answered(out) =
            call_authority_embedded(&paths, "claim.sweepPerson", &sweep).expect("embedded sweep")
        else {
            panic!("the sweep did not run in the embedded core");
        };
        let kills = out["kill"].as_array().expect("kills");
        assert_eq!(kills.len(), 1, "round {round}: {out}");
        assert_eq!(kills[0]["pid"].as_i64(), Some(i64::from(pid)));
        let core = start_with(&Layout::new(&m), MIGRATIONS).expect("reopen");
        assert_eq!(state_of(&core, claim), "ending", "round {round}");
    }
    // Not switched (the marker gone): the same call is refused and ends nothing.
    {
        let mut core = start_with(&Layout::new(&m), MIGRATIONS)
            .expect("reopen")
            .with_folders(&paths);
        crate::cutover::marker::clear(&mut core, "claims").unwrap();
    }
    let e = call_authority_embedded(&paths, "claim.sweepPerson", &sweep);
    assert!(matches!(e, Err(Error::Refused(_))), "{e:?}");
}
