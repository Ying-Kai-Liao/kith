//! The detached runs (`detached.rs`): a wake's claim goes through the same two steps as a schedule's, and a launcher's
//! `done` or `exit` report never ends the claim while its process group lives (R3a, `WakeS4.tla` `broken-ReportToEnding`).
//! Every row is called the way the daemon calls it: `call::check` (the table's own strict param check, which a stub
//! core-call cannot see: lesson #1072) first, then the row.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::RefCell;
use std::collections::HashSet;

use kith_api::Principal;
use serde_json::{Value, json};

use crate::conversations::claims;
use crate::conversations::external;
use crate::db::{Core, MIGRATIONS};
use crate::gate::Liveness;
use crate::layout::Layout;
use crate::start_with;
use crate::tests::machine;

const NOW: i64 = 1_900_000_000_000;
const HOST: &str = "kith-serve-host/1-a";

/// Made-up processes: `alive` ones answer a start time, `groups` are pids that are gone but whose process group still has a
/// member (the engine a killed launcher left running).
#[derive(Default)]
struct Sys {
    alive: RefCell<HashSet<u32>>,
    groups: RefCell<HashSet<u32>>,
}

impl Liveness for Sys {
    fn start(&self, pid: u32) -> Option<String> {
        self.alive.borrow().contains(&pid).then(|| "e7".to_owned())
    }
    fn gone(&self, pid: u32) -> bool {
        !self.alive.borrow().contains(&pid)
    }
    fn orphan_group(&self, pid: u32, _recorded: &str) -> bool {
        self.groups.borrow().contains(&pid)
    }
}

fn world() -> (tempfile::TempDir, Core) {
    let (t, m) = machine();
    let layout = Layout::new(&m);
    let mut core = start_with(&layout, MIGRATIONS).expect("start");
    // A wake's turn is claimed only while the wake queue is the core's (WK6): these runs are of a switched Mac.
    #[cfg(unix)]
    {
        core = core.with_folders(&crate::tests::paths(&m));
        crate::cutover::modules::wake::write_switched(&layout, &mut core, 1)
            .expect("the wake switch");
    }
    claims::reconcile(&mut core, &[], &[], i64::MAX, 1).expect("reconcile");
    (t, core)
}

fn host(core: &mut Core, method: &str, p: &Value) -> Value {
    crate::call::check(Principal::Host, method, p).unwrap_or_else(|e| panic!("{method}: {e}"));
    crate::hostclaim::call_at(core, method, p, NOW).unwrap_or_else(|e| panic!("{method}: {e}"))
}

fn owner(core: &mut Core, live: &Sys, method: &str, p: &Value) -> Value {
    crate::call::check(Principal::Owner, method, p).unwrap_or_else(|e| panic!("{method}: {e}"));
    external::call_at(core, method, p, NOW + 5, live).unwrap_or_else(|e| panic!("{method}: {e}"))
}

fn enqueue(core: &mut Core, conversation: &str, source: &str) -> i64 {
    host(
        core,
        "hostclaim.enqueue",
        &json!({"conversation": conversation, "agent": "echo", "engine": "claude", "source": source,
                "person": "owner", "prompt": "the note", "revokeSeen": 0}),
    )["turn"]
        .as_i64()
        .unwrap()
}

/// A turn of `source` in `conversation`, claimed and attached by the process 700 (the launcher). Answers (turn, claim).
fn running(core: &mut Core, live: &Sys, conversation: &str, source: &str) -> (i64, i64) {
    let turn = enqueue(core, conversation, source);
    let kind = if source == "wake" {
        "wakeRun"
    } else {
        "scheduleRun"
    };
    let claimed = host(
        core,
        "hostclaim.claim",
        &json!({"conversation": conversation, "kind": kind, "turn": turn, "person": "owner",
                "revokeSeen": 0, "holderInstance": HOST}),
    );
    let claim = claimed["claimId"]
        .as_i64()
        .unwrap_or_else(|| panic!("{claimed}"));
    live.alive.borrow_mut().insert(700);
    let said = owner(
        core,
        live,
        "claim.attach",
        &json!({"claimId": claim, "conversation": conversation, "pid": 700, "started": "e7"}),
    );
    assert_eq!(said, json!({"attached": true}), "{source}");
    (turn, claim)
}

fn report(core: &mut Core, claim: i64, kind: &str) -> Value {
    host(
        core,
        "hostclaim.report",
        &json!({"claimId": claim, "kind": kind, "body": json!({"pid": 700, "out_of_usage": false}).to_string()}),
    )
}

fn claim_state(core: &Core, claim: i64) -> (String, Option<String>) {
    core.conn()
        .query_row(
            "SELECT state, end_reason FROM claims WHERE claim_id = ?1",
            [claim],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
}

fn turn_state(core: &Core, turn: i64) -> String {
    core.conn()
        .query_row("SELECT state FROM turns WHERE id = ?1", [turn], |r| {
            r.get(0)
        })
        .unwrap()
}

/// What a terminal that wants the conversation is told: it must be refused while anything of the run lives.
fn terminal_takes(core: &mut Core, live: &Sys, conversation: &str) -> Value {
    live.alive.borrow_mut().insert(900);
    owner(
        core,
        live,
        "claim.external",
        &json!({"conversation": conversation, "pid": 900, "started": "e7", "person": "", "agent": "echo"}),
    )
}

#[test]
fn a_wake_turn_is_claimed_and_attached_like_a_schedules_and_no_other_source_is() {
    let (_t, mut core) = world();
    let live = Sys::default();
    let (turn, claim) = running(&mut core, &live, "w1", "wake");
    assert_eq!(turn_state(&core, turn), "running");
    assert_eq!(claim_state(&core, claim).0, "running");
    // A turn of another source is refused by the claim step, as data.
    let app = enqueue(&mut core, "a1", "app");
    let refused = crate::hostclaim::call_at(
        &mut core,
        "hostclaim.claim",
        &json!({"conversation": "a1", "kind": "wakeRun", "turn": app, "person": "owner", "revokeSeen": 0}),
        NOW,
    )
    .unwrap();
    assert_eq!(refused["refused"], "notWaiting", "{refused}");
    assert!(
        refused["why"]
            .as_str()
            .unwrap()
            .contains("as a detached run's"),
        "{refused}"
    );
}

#[test]
fn attach_is_a_check_and_set_on_the_claim_the_conversation_and_the_process() {
    let (_t, mut core) = world();
    let attach = |core: &mut Core, claim: i64, conversation: &str, pid: u32, started: &str| {
        owner(
            core,
            &Sys::default(),
            "claim.attach",
            &json!({"claimId": claim, "conversation": conversation, "pid": pid, "started": started}),
        )
    };
    let claim_of = |core: &mut Core, conversation: &str| {
        let turn = enqueue(core, conversation, "wake");
        let c = host(
            core,
            "hostclaim.claim",
            &json!({"conversation": conversation, "kind": "wakeRun", "turn": turn, "person": "owner",
                    "revokeSeen": 0, "holderInstance": HOST}),
        );
        (turn, c["claimId"].as_i64().unwrap())
    };
    // Another conversation's name does not attach, and leaves the claim attachable.
    let (_turn, claim) = claim_of(&mut core, "w1");
    assert_eq!(attach(&mut core, claim, "w2", 700, "e7")["refused"], "gone");
    assert_eq!(
        attach(&mut core, claim, "w1", 700, "e7"),
        json!({"attached": true})
    );
    // The same process again is told so; another one loses.
    assert_eq!(attach(&mut core, claim, "w1", 700, "e7")["again"], true);
    assert_eq!(attach(&mut core, claim, "w1", 701, "e8")["refused"], "gone");
    // Only a schedule's turn may attach without naming its conversation.
    let (_t2, nameless) = claim_of(&mut core, "w5");
    let said = owner(
        &mut core,
        &Sys::default(),
        "claim.attach",
        &json!({"claimId": nameless, "pid": 704, "started": "eb"}),
    );
    assert_eq!(said["refused"], "gone");
    assert_eq!(
        attach(&mut core, nameless, "w5", 704, "eb"),
        json!({"attached": true})
    );
    // A cancelled wake turn's claim (the rollback, a revoke) is never attached afterwards.
    let (turn, claim) = claim_of(&mut core, "w3");
    let tx = core.claim_tx().unwrap();
    claims::cancel_waiting_in(&tx, &[turn], "rollback", NOW + 1).unwrap();
    tx.commit().unwrap();
    assert_eq!(attach(&mut core, claim, "w3", 702, "e9")["refused"], "gone");
    // So is the claim an adopt gave back (the host that made it was replaced).
    let (turn, claim) = claim_of(&mut core, "w4");
    host(
        &mut core,
        "hostclaim.adopt",
        &json!({"instance": "kith-serve-host/2-b"}),
    );
    assert_eq!(turn_state(&core, turn), "queued");
    assert_eq!(attach(&mut core, claim, "w4", 703, "ea")["refused"], "gone");
}

fn killed_launcher_keeps_the_conversation_held(source: &str) {
    let (_t, mut core) = world();
    let live = Sys::default();
    let conversation = format!("{source}-1");
    let (turn, claim) = running(&mut core, &live, &conversation, source);
    // Only the launcher died; its engine runs on in the group.
    live.alive.borrow_mut().remove(&700);
    live.groups.borrow_mut().insert(700);
    let said = report(&mut core, claim, "exit");
    assert_eq!(said["applied"], "exit", "{said}");
    assert_eq!(
        claim_state(&core, claim),
        ("ending".into(), Some("exit".into()))
    );
    assert_eq!(
        turn_state(&core, turn),
        "running",
        "{source}: the turn is still the run's"
    );
    assert_eq!(
        claims::end_gone(&mut core, &live, NOW + 6).unwrap(),
        Vec::<i64>::new()
    );
    let taken = terminal_takes(&mut core, &live, &conversation);
    assert!(
        taken.get("claimId").is_none() && taken["refused"].is_string(),
        "{taken}"
    );
    // A second report, and a `done` after the `exit`, change nothing.
    assert!(report(&mut core, claim, "exit")["applied"].is_null());
    assert!(report(&mut core, claim, "done")["applied"].is_null());
    assert_eq!(
        claim_state(&core, claim),
        ("ending".into(), Some("exit".into()))
    );
    // The group's last process ends: now the claim ends and the turn is failed, never put back.
    live.groups.borrow_mut().clear();
    assert_eq!(
        claims::end_gone(&mut core, &live, NOW + 7).unwrap(),
        vec![claim]
    );
    assert_eq!(
        claim_state(&core, claim),
        ("ended".into(), Some("exit".into()))
    );
    assert_eq!(turn_state(&core, turn), "failed");
}

#[test]
fn a_wake_runs_exit_report_with_its_group_alive_holds_the_conversation() {
    killed_launcher_keeps_the_conversation_held("wake");
}

#[test]
fn a_schedule_runs_exit_report_with_its_group_alive_holds_the_conversation() {
    killed_launcher_keeps_the_conversation_held("schedule");
}

#[test]
fn a_done_report_ends_the_claim_only_when_gone_and_then_the_turn_is_done() {
    for source in ["wake", "schedule"] {
        let (_t, mut core) = world();
        let live = Sys::default();
        let (turn, claim) = running(&mut core, &live, "c1", source);
        // The launcher still lives, as the host sees it exit a moment before the system lets go of it.
        report(&mut core, claim, "done");
        assert_eq!(
            claim_state(&core, claim),
            ("ending".into(), Some("done".into())),
            "{source}"
        );
        assert_eq!(turn_state(&core, turn), "running", "{source}");
        assert_eq!(
            claims::end_gone(&mut core, &live, NOW + 6).unwrap(),
            Vec::<i64>::new()
        );
        live.alive.borrow_mut().clear();
        assert_eq!(
            claims::end_gone(&mut core, &live, NOW + 7).unwrap(),
            vec![claim]
        );
        assert_eq!(claim_state(&core, claim).0, "ended");
        assert_eq!(turn_state(&core, turn), "done", "{source}");
        // An `exit` that comes after the `done` does not flip what it said.
        assert!(report(&mut core, claim, "exit")["applied"].is_null());
        assert_eq!(
            claim_state(&core, claim),
            ("ended".into(), Some("done".into()))
        );
    }
}

#[test]
fn a_run_whose_probe_cannot_tell_stays_held() {
    struct Blind;
    impl Liveness for Blind {
        fn start(&self, _pid: u32) -> Option<String> {
            None
        }
        fn gone(&self, _pid: u32) -> bool {
            false
        }
    }
    let (_t, mut core) = world();
    let live = Sys::default();
    let (turn, claim) = running(&mut core, &live, "b1", "wake");
    report(&mut core, claim, "done");
    assert_eq!(
        claims::end_gone(&mut core, &Blind, NOW + 6).unwrap(),
        Vec::<i64>::new()
    );
    assert_eq!(claim_state(&core, claim).0, "ending");
    assert_eq!(turn_state(&core, turn), "running");
}

#[test]
fn a_report_that_names_another_process_ends_nothing() {
    let (_t, mut core) = world();
    let live = Sys::default();
    let (turn, claim) = running(&mut core, &live, "o1", "wake");
    let said = host(
        &mut core,
        "hostclaim.report",
        &json!({"claimId": claim, "kind": "exit", "body": json!({"pid": 999}).to_string()}),
    );
    assert!(said["applied"].is_null(), "{said}");
    assert_eq!(claim_state(&core, claim).0, "running");
    assert_eq!(turn_state(&core, turn), "running");
}

#[test]
fn queued_lists_the_wake_turns_with_their_door_person_request_ids_and_note() {
    let (_t, mut core) = world();
    let turn = enqueue(&mut core, "w1", "wake");
    let other = enqueue(&mut core, "w2", "wake");
    let c = core.conn();
    c.execute(
        "INSERT INTO wake_sessions (session, agent, person, door, turn_run, turn_at)
         VALUES ('w1', 'echo', 'owner', 'host', 'r1', 'x')",
        [],
    )
    .unwrap();
    for (seq, id) in [(1, "req-b"), (2, "req-a")] {
        c.execute(
            "INSERT INTO wake_items (seq, id, session, text, at, run, state) VALUES (?1, ?2, 'w1', 't', 'x', 'r1', 'taken')",
            rusqlite::params![seq, id],
        )
        .unwrap();
    }
    c.execute(
        "INSERT INTO wake_turns (run, session, turn_id) VALUES ('r1', 'w1', ?1)",
        [turn],
    )
    .unwrap();
    let listed = host(
        &mut core,
        "hostclaim.queued",
        &json!({"sources": ["schedule", "wake"]}),
    );
    let turns = listed["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 2, "{listed}");
    let one = &turns[0];
    assert_eq!(one["turn"], turn);
    assert_eq!(one["source"], "wake");
    assert_eq!(one["door"], "host");
    assert_eq!(one["person"], "owner");
    assert_eq!(
        one["requestIds"],
        json!(["req-b", "req-a"]),
        "in the order they came"
    );
    assert_eq!(one["note"], "the note");
    assert_eq!(one["redeemedBefore"], false);
    // A turn with no link row still lists, with nothing to say about its door.
    assert_eq!(turns[1]["turn"], other);
    assert_eq!(turns[1]["requestIds"], json!([]));
    assert!(turns[1]["door"].is_null());
}

#[test]
fn queued_says_which_wake_turn_ran_before_and_every_item_carries_its_source() {
    use crate::conversations::claims::{Kind, NewClaim};
    let (_t, mut core) = world();
    let sched = enqueue(&mut core, "schedule:echo:daily:claude", "schedule");
    let ran = enqueue(&mut core, "w9", "wake");
    let req = NewClaim {
        conversation: "w9",
        kind: Kind::Turn,
        person: "owner",
        turn: Some(ran),
        holder_instance: Some(HOST),
        account: None,
        process: None,
        revoke_seen: 0,
    };
    let id = claims::claim(&mut core, &req, NOW).unwrap().claim_id;
    claims::redeem(&mut core, id, 900, "e900", HOST, NOW).unwrap();
    // Put back by a restart: its prompt may have gone, so the host must not run it again.
    host(
        &mut core,
        "hostclaim.stop",
        &json!({"claimId": id, "reason": "test", "requeue": true}),
    );
    let listed = host(
        &mut core,
        "hostclaim.queued",
        &json!({"sources": ["schedule", "wake"]}),
    );
    let by = |t: i64| {
        listed["turns"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["turn"] == t)
            .cloned()
            .unwrap_or_else(|| panic!("{listed}"))
    };
    assert_eq!(by(sched)["source"], "schedule");
    assert!(by(sched).get("door").is_none());
    assert_eq!(by(ran)["source"], "wake");
    assert_eq!(by(ran)["redeemedBefore"], true, "{listed}");
}

#[test]
fn queued_without_sources_lists_schedule_turns_only_and_refuses_other_sources() {
    let (_t, mut core) = world();
    let sched = enqueue(&mut core, "schedule:echo:daily:claude", "schedule");
    let _wake = enqueue(&mut core, "w1", "wake");
    let listed = host(&mut core, "hostclaim.queued", &json!({}));
    let turns: Vec<_> = listed["turns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["turn"].clone())
        .collect();
    assert_eq!(
        turns,
        vec![json!(sched)],
        "a host that only runs cron jobs never sees a wake's turn"
    );
    let p = json!({"sources": ["app"]});
    crate::call::check(Principal::Host, "hostclaim.queued", &p).unwrap();
    assert!(crate::hostclaim::call_at(&mut core, "hostclaim.queued", &p, NOW).is_err());
}

#[test]
fn the_launch_asks_external_after_attach_and_gets_its_claim_again_and_its_exit_leaves_it() {
    // The calls `kith AGENT --resume SESSION NOTE` makes with KITH_TURN_CLAIM set (threads/writer.rs, S4 WK3): attach,
    // then claim.external for the same pid and stamp (resolve, then the one-shot's claim step), then the release at exit.
    let (_t, mut core) = world();
    let live = Sys::default();
    let (turn, claim) = running(&mut core, &live, "w1", "wake");
    let ask =
        json!({"conversation": "w1", "pid": 700, "started": "e7", "person": "", "agent": "echo"});
    for _ in 0..2 {
        let said = owner(&mut core, &live, "claim.external", &ask);
        assert_eq!(said["claimId"], claim, "{said}");
        assert_eq!(said["again"], true, "{said}");
    }
    let said = owner(
        &mut core,
        &live,
        "claim.release",
        &json!({"pid": 700, "started": "e7"}),
    );
    assert_eq!(said["ended"], json!([]), "{said}");
    assert_eq!(claim_state(&core, claim).0, "running");
    assert_eq!(turn_state(&core, turn), "running");
}

/// A channel wake's turn is on the work key `tg:7#house`; the launch holds the thread's Claude session `s1`. Claims the turn
/// on the key (the pull), no process yet. Answers (turn, claim).
fn claimed_on_the_key(core: &mut Core, wk: &str) -> (i64, i64) {
    let turn = enqueue(core, wk, "wake");
    let claimed = host(
        core,
        "hostclaim.claim",
        &json!({"conversation": wk, "kind": "wakeRun", "turn": turn, "person": "owner",
                "revokeSeen": 0, "holderInstance": HOST}),
    );
    (
        turn,
        claimed["claimId"]
            .as_i64()
            .unwrap_or_else(|| panic!("{claimed}")),
    )
}

fn turn_rows(core: &Core) -> i64 {
    core.conn()
        .query_row("SELECT COUNT(*) FROM turns", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn a_channel_launch_takes_its_session_first_then_attaches_the_work_key_and_the_wake_is_one_turn() {
    // WK4c (O3): two conversations, the wake turn's (`wk`) and the session's (`sid`). The launch's writer claim on `sid` comes
    // first; the attach names `wk`. One `turns` row for the whole story, two claims held by one process.
    let (_t, mut core) = world();
    let live = Sys::default();
    let wk = "tg:7#house";
    let (turn, wake) = claimed_on_the_key(&mut core, wk);
    live.alive.borrow_mut().insert(700);
    let ask =
        json!({"conversation": "s1", "pid": 700, "started": "e7", "person": "", "agent": "echo"});
    let writer = owner(&mut core, &live, "claim.external", &ask);
    let session = writer["claimId"]
        .as_i64()
        .unwrap_or_else(|| panic!("{writer}"));
    assert_ne!(session, wake, "the session's claim is its own");
    let said = owner(
        &mut core,
        &live,
        "claim.attach",
        &json!({"claimId": wake, "conversation": wk, "session": "s1", "pid": 700, "started": "e7"}),
    );
    assert_eq!(said, json!({"attached": true}));
    assert_eq!(
        turn_rows(&core),
        1,
        "the session's claim made no turn of its own"
    );
    assert_eq!(turn_state(&core, turn), "running");
    assert_eq!(claim_state(&core, wake).0, "running");
    assert_ne!(
        claim_state(&core, session).0,
        "ended",
        "the session's claim is held while the launch runs"
    );
    // A terminal wants the session: held. Another wake of the thread cannot be claimed while the first holds the key.
    assert!(
        terminal_takes(&mut core, &live, "s1")
            .get("claimId")
            .is_none()
    );
    let second = enqueue(&mut core, wk, "wake");
    let again = host(
        &mut core,
        "hostclaim.claim",
        &json!({"conversation": wk, "kind": "wakeRun", "turn": second, "person": "owner",
                "revokeSeen": 0, "holderInstance": HOST}),
    );
    assert!(
        again.get("claimId").is_none(),
        "one wake at a time in a thread: {again}"
    );
    // The launch ends: its session claim goes, the wake claim stays for the host's report.
    owner(
        &mut core,
        &live,
        "claim.release",
        &json!({"pid": 700, "started": "e7", "conversation": "s1"}),
    );
    assert_eq!(claim_state(&core, session).0, "ended");
    assert_eq!(claim_state(&core, wake).0, "running");
    assert_eq!(turn_state(&core, turn), "running");
}

#[test]
fn an_attach_that_names_the_session_instead_of_the_work_key_is_refused_and_the_wake_stays_claimed()
{
    // An older launch attaches with the session's id (version skew): the core's check of the turn's conversation refuses it,
    // the launch exits 10, and the host requeues the claim rather than freeing it.
    let (_t, mut core) = world();
    let live = Sys::default();
    let (turn, wake) = claimed_on_the_key(&mut core, "tg:7#house");
    live.alive.borrow_mut().insert(700);
    let said = owner(
        &mut core,
        &live,
        "claim.attach",
        &json!({"claimId": wake, "conversation": "s1", "pid": 700, "started": "e7"}),
    );
    assert_eq!(said["refused"], "gone", "{said}");
    assert_eq!(claim_state(&core, wake).0, "claimed");
    assert_eq!(turn_state(&core, turn), "claimed");
}

#[test]
fn a_terminal_on_the_session_refuses_the_writer_claim_so_the_wake_is_never_attached_and_the_next_adopt_requeues_it()
 {
    // The attach before the writer claim would redeem a wake nobody gets (the model's `broken-AttachBeforeWriter`). The launch
    // asks for the session first; this is the refusal it exits on, and the claim that nothing attached goes back to queued.
    let (_t, mut core) = world();
    let live = Sys::default();
    live.alive.borrow_mut().insert(900);
    let held = owner(
        &mut core,
        &live,
        "claim.external",
        &json!({"conversation": "s1", "pid": 900, "started": "e7", "person": "", "agent": "echo"}),
    );
    assert!(held["claimId"].is_i64(), "{held}");
    let (turn, wake) = claimed_on_the_key(&mut core, "tg:7#house");
    live.alive.borrow_mut().insert(700);
    let refused = owner(
        &mut core,
        &live,
        "claim.external",
        &json!({"conversation": "s1", "pid": 700, "started": "e7", "person": "", "agent": "echo"}),
    );
    assert!(
        refused.get("claimId").is_none(),
        "the session is held: {refused}"
    );
    assert_eq!(
        claim_state(&core, wake),
        ("claimed".to_owned(), None),
        "nothing attached"
    );
    assert_eq!(turn_state(&core, turn), "claimed");
    // The host's restart: nobody attached, so the turn is queued again, not redeemed.
    crate::conversations::warm::adopt(&mut core, "kith-serve-host/2-b", NOW + 10).unwrap();
    assert_eq!(turn_state(&core, turn), "queued");
    assert_eq!(turn_rows(&core), 1);
}

#[test]
fn an_attach_naming_a_session_the_process_holds_no_writer_claim_on_is_refused() {
    // The core itself refuses an attach that came before the writer claim (the model's `broken-AttachBeforeWriter`): the session
    // is named, no running external claim of this process is on it, so the wake claim stays `claimed` and its turn too.
    let (_t, mut core) = world();
    let live = Sys::default();
    let (turn, wake) = claimed_on_the_key(&mut core, "tg:7#house");
    live.alive.borrow_mut().insert(700);
    let said = owner(
        &mut core,
        &live,
        "claim.attach",
        &json!({"claimId": wake, "conversation": "tg:7#house", "session": "s1", "pid": 700, "started": "e7"}),
    );
    assert_eq!(said["refused"], "gone", "{said}");
    assert_eq!(claim_state(&core, wake).0, "claimed");
    assert_eq!(turn_state(&core, turn), "claimed");
    // Another process's writer claim on the session is no help.
    live.alive.borrow_mut().insert(701);
    let other = owner(
        &mut core,
        &live,
        "claim.external",
        &json!({"conversation": "s1", "pid": 701, "started": "e8", "person": "", "agent": "echo"}),
    );
    assert!(other["claimId"].is_i64(), "{other}");
    let said = owner(
        &mut core,
        &live,
        "claim.attach",
        &json!({"claimId": wake, "conversation": "tg:7#house", "session": "s1", "pid": 700, "started": "e7"}),
    );
    assert_eq!(said["refused"], "gone", "{said}");
    assert_eq!(claim_state(&core, wake).0, "claimed");
}
