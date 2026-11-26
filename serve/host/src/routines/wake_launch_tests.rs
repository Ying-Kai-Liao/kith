//! A run-door wake of a non-Claude agent through the REAL launch on a real core (architect's review of #1250, MUST 1): a wake
//! continues the session it is owed on, or it does not run. The core, the claims and the built `kith` are real; the engines are the
//! stand-ins of `launch_turn_e2e_tests`. The wake is a real request ticked into a turn and claimed as the host claims it, and the
//! launch is started as the host's door builds it (`cards::resume::wake_clock::command_with`): the session in `KITH_RESUME` and
//! `KITH_TURN_CONVERSATION`, the claim in `KITH_TURN_CLAIM`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::launch_turn_e2e_tests::{World, built_kith, repo_kernel};
use crate::terminal::core_writer::{CoreWriter, RunClaim};
use crate::wake::{self, Request};
use std::process::{Output, Stdio};

const NOTE: &str = "[kith wake run-1] go on";

/// The agent `house` on codex, a wake owed on `session` and claimed (the host's claim): its claim id.
fn owed(w: &World, session: &str) -> i64 {
    let agent = w.dir.join("home/agents/house/role/agent.md");
    std::fs::write(agent, "---\nname: house\ndescription: test agent\nharness: codex\n---\nbody\n").unwrap();
    // The wake module is switched: its queue is rows of the core (as `wake_clock_core_tests` does it).
    crate::terminal::core_claims::enable(&w.folders.state, w.folders.clone());
    let dir = w.folders.machine.join("core/cutover");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("wake.switch"), "state=switched\nat=1\n").unwrap();
    w.reconciled();
    let req = Request {
        session,
        id: "card:c1",
        agent: "house",
        person: None,
        room: None,
        host_door: true,
        why: "a card was answered",
        text: "go on",
    };
    wake::enqueue(&w.folders.state, &req).unwrap();
    wake::tick(&w.folders.state).unwrap();
    let host = CoreWriter::with_instance(w.folders.clone(), "host-1".to_owned());
    let turns = host.queued_from(&["wake"]).expect("the queue");
    let t = turns.iter().find(|t| t["conversation"] == session).unwrap_or_else(|| panic!("no wake turn: {turns:?}"));
    let RunClaim::Claimed(claim) = host.claim_run_as("wakeRun", session, "", t["turn"].as_i64().unwrap()) else {
        panic!("the wake turn is claimed")
    };
    claim
}

/// The cost ledger says `session` was made under `account`.
fn made_under(w: &World, session: &str, account: &str) {
    let db = rusqlite::Connection::open(w.folders.machine.join("cost.db")).unwrap();
    db.execute_batch("create table if not exists runs (session text, provider text, account text, account_id text)")
        .unwrap();
    db.execute(
        "insert into runs (session, provider, account, account_id) values (?, 'codex', null, ?)",
        [session, account],
    )
    .unwrap();
}

/// The host's launch of a codex wake: `kith kernel launch-exec house --kernel K -- NOTE`, with the claim when there is one.
fn launch(w: &World, session: &str, claim: Option<i64>) -> Output {
    let kernel = repo_kernel().to_string_lossy().into_owned();
    let mut cmd = w.command(&built_kith());
    cmd.args(["kernel", "launch-exec", "house", "--kernel", &kernel, "--", NOTE])
        .env("KITH_RESUME", session)
        .current_dir(w.dir.join("home"))
        .stdin(Stdio::null());
    if let Some(claim) = claim {
        cmd.env("KITH_TURN_CLAIM", claim.to_string()).env("KITH_TURN_CONVERSATION", session);
    }
    cmd.output().unwrap()
}

fn said(out: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

#[test]
fn a_claimed_wake_on_a_session_made_under_another_account_is_refused_before_the_attach_not_run_fresh() {
    let w = World::start();
    let claim = owed(&w, "codex:abc");
    made_under(&w, "codex:abc", "codex:other");
    let out = launch(&w, "codex:abc", Some(claim));
    assert_eq!(out.status.code(), Some(1), "{}", said(&out));
    assert!(said(&out).contains("does not run on another session"), "{}", said(&out));
    assert!(w.runs().is_empty(), "no engine ran: {:?}", w.runs());
    assert!(!w.dir.join("acp-in").exists(), "no codex session was started");
    assert_eq!(w.show(claim)["state"], "claimed", "nothing attached the claim: the turn goes back as a failed try");
}

#[test]
fn a_wake_on_its_own_session_on_its_own_account_resumes_it_through_engine_run_and_attaches_once() {
    let w = World::start();
    let claim = owed(&w, "codex:abc");
    made_under(&w, "codex:abc", "codex");
    let out = launch(&w, "codex:abc", Some(claim));
    assert!(out.status.success(), "{}", said(&out));
    assert_eq!(w.runs().len(), 1, "the engine ran once: {:?}", w.runs());
    let sent = std::fs::read_to_string(w.dir.join("acp-in")).unwrap();
    assert!(sent.contains("go on"), "the note reached the engine: {sent}");
    assert!(!said(&out).contains("starting a new one"), "{}", said(&out));
    assert_ne!(w.show(claim)["state"], "claimed", "the launch attached the claim");
}

#[test]
fn an_engine_that_cannot_resume_the_session_refuses_a_claimed_turn_on_it_but_a_hand_run_starts_a_new_one() {
    let w = World::start();
    // Not a codex session at all: with the turn's claim the engine refuses it, never a new session beside it.
    let claim = owed(&w, "bare-claude-id");
    let out = launch(&w, "bare-claude-id", Some(claim));
    assert_eq!(out.status.code(), Some(1), "{}", said(&out));
    assert!(!w.dir.join("acp-in").exists(), "no codex session was started for the wake: {}", said(&out));
    // The same variable by hand, with no turn: a new session, as before.
    let out = launch(&w, "bare-claude-id", None);
    assert!(out.status.success(), "{}", said(&out));
    assert!(said(&out).contains("starting a new one"), "{}", said(&out));
    assert!(w.dir.join("acp-in").exists(), "a hand run starts a new session");
}

#[test]
fn a_codex_agents_wake_on_a_bare_claude_id_never_starts_a_codex_session() {
    let w = World::start();
    let claim = owed(&w, "bare-claude-id");
    // The host's Claude path for a bare id: `kith AGENT --resume SID NOTE` (the script's entry).
    let script = repo_kernel().join("bin/kith");
    let mut cmd = w.command(&script);
    cmd.args(["house", "--resume", "bare-claude-id", NOTE])
        .env("KITH_TURN_CLAIM", claim.to_string())
        .current_dir(w.dir.join("home"))
        .stdin(Stdio::null());
    let out = cmd.output().unwrap();
    assert!(!w.dir.join("acp-in").exists(), "no codex session was started: {}", said(&out));
}
