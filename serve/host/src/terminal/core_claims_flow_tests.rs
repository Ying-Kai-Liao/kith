//! The claims through the core (M3-10f), against a scripted core (`core_claims_tests.rs`); the rows' own rules are tested on a
//! real core in `core/runtime/src/hostclaim_tests.rs`. A child of `tests.rs`, which owns the `Rig`.

use super::*;
use crate::terminal::claim::{self, Kind};
use crate::terminal::core_claims::tests::FakeCore;

#[tokio::test]
async fn a_turn_claim_is_the_claim_file_with_the_core_claim_as_its_shadow() {
    let r = Rig::new();
    let core = FakeCore::start(&r.state());
    let pid = r.sleeper();
    assert!(claim::take_as(&r.state(), "s1", Kind::App, pid, false, None, Some("builder")).unwrap().is_ok());
    assert_eq!(
        core.log(),
        ["hostclaim.ensure", "hostclaim.epoch", "hostclaim.enqueue", "hostclaim.turn", "hostclaim.redeem"],
        "enqueue, then claim_next's row, then the redeem on the agent's pid; never a direct claim"
    );
    // The kernel's takers (kith-sessions, lifecycle, wake) only look at the file: it is the gate, the core claim its shadow.
    assert_eq!(claim::read(&claim::path(&r.state(), "s1", false)).map(|c| c.pid), Some(pid));
    // The owner's own turn only keeps a terminal out, as a Kind::App file claim does.
    assert!(claim::holder(&r.state(), "s1").is_none());
    // Another claimer is refused as held by the first, and its turn is cancelled, not left queued.
    let other = r.sleeper();
    let held = claim::take_as(&r.state(), "s1", Kind::Terminal, other, false, None, None).unwrap().unwrap_err();
    assert_eq!(held.0.pid, pid);
    let held = claim::take_as(&r.state(), "s1", Kind::Bot, other, false, Some("bot-sam"), Some("builder"));
    assert_eq!(held.unwrap().unwrap_err().0.pid, pid);
    assert_eq!(core.turn_states(), ["claimed"], "the file refused them before the core was asked");
    // Release is the claimer's end report; another pid's release changes nothing.
    claim::release(&r.state(), "s1", other, false);
    assert_eq!(core.claim_state(1), "running");
    claim::release(&r.state(), "s1", pid, false);
    assert_eq!(core.claim_state(1), "ended");
    assert_eq!(core.turn_states(), ["done"], "a host turn's end is `done`: the core must not requeue it");
    assert!(!claim::path(&r.state(), "s1", false).exists(), "the file goes after the core claim");
    assert!(claim::take_as(&r.state(), "s1", Kind::Bot, other, false, Some("bot-sam"), None).unwrap().is_ok());
    assert_eq!(claim::holder(&r.state(), "s1").map(|c| (c.kind, c.pid)), Some((Kind::Bot, other)));
}

#[tokio::test]
async fn a_revoked_person_is_refused_and_nothing_is_left_queued() {
    let r = Rig::new();
    let core = FakeCore::start(&r.state());
    core.world.lock().unwrap().revoked = true;
    let pid = r.sleeper();
    let e = claim::take_as(&r.state(), "s1", Kind::Bot, pid, false, Some("bot-sam"), None).unwrap_err();
    assert!(e.to_string().contains("revoked"), "{e}");
    assert!(crate::terminal::core_claims::is_revoked(&e), "the turn path refuses on this error");
    assert!(core.turn_states().is_empty(), "refused at enqueue: nothing was queued");
    assert!(!claim::path(&r.state(), "s1", false).exists(), "no claim is left behind for a refused turn");
    assert!(claim::holder(&r.state(), "s1").is_none());
}

/// The decision `claim_for_turn` makes on what `take_for_turn` returned (the turn path calls exactly these two).
#[tokio::test]
async fn the_turn_path_refuses_a_revoked_person_and_runs_under_the_file_for_other_core_failures() {
    use crate::terminal::claim::{TurnClaim, judge_turn_claim, take_for_turn};
    let r = Rig::new();
    let core = FakeCore::start(&r.state());
    let pid = r.sleeper();
    let judge = |sid: &str, main: bool| {
        let taken =
            take_for_turn(&r.state(), sid, Kind::Bot, pid, false, Some("bot-sam"), Some("builder"), Some("/work"));
        judge_turn_claim(&taken, pid, main, true)
    };
    core.world.lock().unwrap().revoked = true;
    assert_eq!(judge("s1", false), TurnClaim::Revoked, "a thread lane refuses");
    assert_eq!(judge("s1", true), TurnClaim::Revoked, "so does the main lane: it is not a held claim");
    assert!(!claim::path(&r.state(), "s1", false).exists());
    core.world.lock().unwrap().revoked = false;
    core.world.lock().unwrap().down = true;
    assert_eq!(judge("s2", false), TurnClaim::Held, "core down: the file claim is the writer");
    core.world.lock().unwrap().down = false;
    // Another writer in the core (not in a file): refused on a thread lane, run unclaimed on the main lane.
    core.hold("s3", "external", std::process::id());
    assert_eq!(judge("s3", false), TurnClaim::Refused("heldByOtherWriter"));
    assert_eq!(judge("s3", true), TurnClaim::Unclaimed);
}

#[tokio::test]
async fn a_core_that_holds_another_writer_gives_the_file_back_and_cancels_the_turn() {
    let r = Rig::new();
    let core = FakeCore::start(&r.state());
    core.hold("s1", "external", std::process::id());
    let pid = r.sleeper();
    let held = claim::take_as(&r.state(), "s1", Kind::Bot, pid, false, Some("bot-sam"), None).unwrap().unwrap_err();
    assert_eq!(held.0.pid, std::process::id());
    assert!(!claim::path(&r.state(), "s1", false).exists(), "the file claim was given back");
    assert_eq!(core.turn_states(), ["cancelled"]);
}

#[tokio::test]
async fn any_other_core_failure_leaves_the_file_claim_as_the_writer() {
    for case in ["down", "exhausted", "redeem_fails"] {
        let r = Rig::new();
        let core = FakeCore::start(&r.state());
        {
            let mut w = core.world.lock().unwrap();
            match case {
                "down" => w.down = true,
                "exhausted" => w.exhausted = true,
                _ => w.redeem_fails = true,
            }
        }
        let pid = r.sleeper();
        let got = claim::take_as(&r.state(), "s1", Kind::Bot, pid, false, Some("bot-sam"), None);
        assert!(got.unwrap().is_ok(), "{case}: the turn goes on under its claim file");
        assert_eq!(claim::holder(&r.state(), "s1").map(|c| c.pid), Some(pid), "{case}: and the file keeps others out");
        assert!(core.turn_states().iter().all(|s| s != "queued" && s != "claimed"), "{case}: {:?}", core.turn_states());
        claim::release(&r.state(), "s1", pid, false);
        assert!(claim::holder(&r.state(), "s1").is_none(), "{case}");
    }
}

#[tokio::test]
async fn a_redeem_whose_reply_was_lost_stands_when_the_core_shows_it_running() {
    let r = Rig::new();
    let core = FakeCore::start(&r.state());
    core.world.lock().unwrap().redeem_lost = true;
    let pid = r.sleeper();
    assert!(claim::take_as(&r.state(), "s1", Kind::Bot, pid, false, Some("bot-sam"), None).unwrap().is_ok());
    assert_eq!(core.claim_state(1), "running");
    assert_eq!(core.turn_states(), ["claimed"], "not cancelled: the redeem did happen");
    claim::release(&r.state(), "s1", pid, false);
    assert_eq!(core.turn_states(), ["done"]);
}

#[tokio::test]
async fn a_kernel_claim_file_taken_during_the_flag_off_window_is_the_same_gate() {
    // The kernel's taker links the file exclusively; the host's turn takes it the same way and loses or wins, never both.
    let r = Rig::new();
    let core = FakeCore::start(&r.state());
    let (a, b) = (r.sleeper(), r.sleeper());
    assert!(claim::take_as(&r.state(), "s1", Kind::App, a, false, None, Some("builder")).unwrap().is_ok());
    assert!(claim::take(&r.state(), "s1", Kind::Terminal, b, false).unwrap().is_err());
    assert_eq!(core.log().iter().filter(|m| *m == "hostclaim.turn").count(), 1);
}

#[tokio::test]
async fn a_live_claim_file_from_before_the_flag_keeps_the_core_out() {
    let r = Rig::new();
    let pid = r.sleeper();
    // Taken while the flag was off.
    assert!(claim::take(&r.state(), "s1", Kind::Terminal, pid, false).unwrap().is_ok());
    let core = FakeCore::start(&r.state());
    let other = r.sleeper();
    let held = claim::take_as(&r.state(), "s1", Kind::Bot, other, false, Some("bot-sam"), None).unwrap().unwrap_err();
    assert_eq!(held.0.pid, pid, "the old writer still holds: two writers is never allowed");
    assert!(core.log().iter().all(|m| m != "hostclaim.enqueue" && m != "hostclaim.claim"), "{:?}", core.log());
    assert_eq!(claim::holder(&r.state(), "s1").map(|c| c.pid), Some(pid));
}

#[tokio::test]
async fn a_holder_that_crashed_with_its_claim_is_ended_by_the_core_and_the_claim_goes_through() {
    let r = Rig::new();
    let core = FakeCore::start(&r.state());
    let id = core.hold("s1", "external", 2_000_000_000);
    let pid = r.sleeper();
    assert!(claim::take_as(&r.state(), "s1", Kind::Bot, pid, false, Some("bot-sam"), None).unwrap().is_ok());
    assert_eq!(core.claim_state(id), "ended");
    assert!(core.log().contains(&"hostclaim.settle".to_owned()));
}

/// An idle terminal registered for `s1` whose process is `child`, with the core holding its claim.
fn idle_terminal(r: &Rig, core: &FakeCore, child: &std::process::Child) -> (String, i64) {
    let pid = child.id();
    r.append("s1", &user("2026-10-23T01:00:01Z", "first"));
    r.register("s1", "builder", pid, None);
    r.session_file(pid, "s1", "idle");
    r.scan();
    (r.roots()[0].id.clone(), core.hold("s1", "terminal", pid))
}

#[tokio::test]
async fn a_take_over_waits_for_ended_before_the_host_writes() {
    let r = Rig::new();
    let core = FakeCore::start(&r.state());
    let mut child = std::process::Command::new("sleep").arg("60").spawn().unwrap();
    let (root, id) = idle_terminal(&r, &core, &child);
    r.append("s1", &agent("2026-10-23T01:00:05Z", "its last line"));
    let reaper = std::thread::spawn(move || child.wait().unwrap());
    let v = sync::take_over(&r.hub, &r.bot, &root).await.unwrap();
    reaper.join().unwrap();
    assert_eq!(v["tookOver"], true);
    assert_eq!(core.claim_state(id), "ended", "promoted only once the core showed it ended");
    let log = core.log();
    let at = |m: &str| log.iter().position(|x| x == m).unwrap();
    assert!(at("hostclaim.stop") < at("hostclaim.settle"), "{log:?}");
    assert!(r.texts(&root).contains(&"its last line".to_owned()));
    assert!(!claim::path(&r.state(), "s1", true).exists(), "no take-over claim file: the core's claim is the writer");
}

#[tokio::test]
async fn a_take_over_that_the_core_never_ends_promotes_nothing() {
    let r = Rig::new();
    let core = FakeCore::start(&r.state());
    core.world.lock().unwrap().stuck = true;
    let mut child = std::process::Command::new("sleep").arg("60").spawn().unwrap();
    let (root, id) = idle_terminal(&r, &core, &child);
    let reaper = std::thread::spawn(move || child.wait().unwrap());
    let e = sync::take_over(&r.hub, &r.bot, &root).await.unwrap_err();
    reaper.join().unwrap();
    assert!(e.to_string().starts_with("terminalBusy"), "{e:#}");
    assert_eq!(core.claim_state(id), "ending", "still the writer: nobody else may write");
    let changes: Vec<String> =
        r.thread(&root).iter().filter_map(|e| e.data["writerChange"].as_str().map(str::to_owned)).collect();
    assert_eq!(changes, ["startedInTerminal"], "the host did not take the thread");
    let again = claim::take_as(&r.state(), "s1", Kind::Bot, r.sleeper(), false, Some("bot-sam"), None).unwrap();
    assert!(again.is_err(), "and a new claim is refused while it is ending");
}

#[tokio::test]
async fn a_release_the_core_never_heard_does_not_turn_the_next_turn_into_a_claimless_one() {
    for (case, same_pid) in [("the warm agent's own pid", true), ("another host pid", false)] {
        let r = Rig::new();
        let core = FakeCore::start(&r.state());
        let pid = r.sleeper();
        assert!(
            claim::take_as(&r.state(), "s1", Kind::Bot, pid, false, Some("bot-sam"), None).unwrap().is_ok(),
            "{case}"
        );
        // The turn ends while the core is down: the file goes, the core's claim stays `running`.
        core.world.lock().unwrap().down = true;
        claim::release(&r.state(), "s1", pid, false);
        core.world.lock().unwrap().down = false;
        assert_eq!(core.claim_state(1), "running", "{case}");
        assert!(!claim::path(&r.state(), "s1", false).exists(), "{case}");
        // The next turn: the stale claim is ended as `done` and the claim made again; it is a real claim, not a claimless run.
        let next = if same_pid { pid } else { r.sleeper() };
        let taken = claim::take_as(&r.state(), "s1", Kind::Bot, next, false, Some("bot-sam"), None);
        assert!(matches!(taken, Ok(Ok(()))), "{case}: {taken:?}");
        assert_eq!(core.claim_state(1), "ended", "{case}");
        assert_eq!(core.claim_state(2), "running", "{case}");
        assert_eq!(core.turn_states()[0], "done", "{case}: the stale turn is finished, never requeued");
        // The file is held throughout: a kernel-style taker is refused.
        assert_eq!(claim::read(&claim::path(&r.state(), "s1", false)).map(|c| c.pid), Some(next), "{case}");
        let other = r.sleeper();
        let refused = claim::take(&r.state(), "s1", Kind::Terminal, other, false).unwrap();
        assert_eq!(refused.unwrap_err().0.pid, next, "{case}");
        claim::release(&r.state(), "s1", next, false);
        assert_eq!(core.claim_state(2), "ended", "{case}");
    }
}

#[tokio::test]
async fn a_claim_only_the_core_knows_still_refuses_the_turn() {
    let r = Rig::new();
    let core = FakeCore::start(&r.state());
    let theirs = r.sleeper();
    core.hold("s1", "external", theirs);
    let pid = r.sleeper();
    let held = claim::take_as(&r.state(), "s1", Kind::Bot, pid, false, Some("bot-sam"), None).unwrap().unwrap_err();
    assert_eq!(held.0.pid, theirs);
    assert_eq!(core.claim_state(1), "running", "not ours: left alone");
    assert!(!claim::path(&r.state(), "s1", false).exists(), "and the file is given back");
}
