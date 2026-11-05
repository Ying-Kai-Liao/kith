//! The claims through the core (M3-10f), against a scripted core (`core_claims_tests.rs`); the rows' own rules are tested on a
//! real core.db in `core/runtime/src/hostclaim_tests.rs`. A child of `tests.rs`, which owns the `Rig`.

use super::*;
use crate::terminal::claim::{self, Kind};
use crate::terminal::core_claims::tests::FakeCore;

#[tokio::test]
async fn a_turn_claim_is_made_by_claim_next_and_redeemed_and_no_claim_file_is_written() {
    let r = Rig::new();
    let core = FakeCore::start(&r.state());
    let pid = r.sleeper();
    assert!(claim::take_as(&r.state(), "s1", Kind::App, pid, false, None, Some("builder")).unwrap().is_ok());
    assert_eq!(
        core.log(),
        ["hostclaim.ensure", "hostclaim.epoch", "hostclaim.enqueue", "hostclaim.turn", "hostclaim.redeem"],
        "enqueue, then claim_next's row, then the redeem on the agent's pid; never a direct claim"
    );
    assert!(!claim::path(&r.state(), "s1", false).exists(), "the claim file is not the writer with the flag on");
    // The owner's own turn only keeps a terminal out, as a Kind::App file claim does.
    assert!(claim::holder(&r.state(), "s1").is_none());
    // Another claimer is refused as held by the first, and its turn is cancelled, not left queued.
    let other = r.sleeper();
    let held = claim::take_as(&r.state(), "s1", Kind::Terminal, other, false, None, None).unwrap().unwrap_err();
    assert_eq!(held.0.pid, pid);
    let held = claim::take_as(&r.state(), "s1", Kind::Bot, other, false, Some("bot-sam"), Some("builder"));
    assert_eq!(held.unwrap().unwrap_err().0.pid, pid);
    assert_eq!(core.turn_states(), ["claimed", "cancelled"], "the refused turn is cancelled by the caller");
    // Release is the claimer's exit report; another pid's release changes nothing.
    claim::release(&r.state(), "s1", other, false);
    assert_eq!(core.claim_state(1), "running");
    claim::release(&r.state(), "s1", pid, false);
    assert_eq!(core.claim_state(1), "ended");
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
    assert!(core.turn_states().is_empty(), "refused at enqueue: nothing was queued");
    assert!(claim::holder(&r.state(), "s1").is_none());
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
