//! The resume half on a real hub: one resume per card, a held session stays owed, revoke and expiry end it, the -p door
//! carries the card's person, and the owner's `stopHolder` (L2). The doors do not start processes or actors in tests
//! (`resume::tests_seen` records what they would have done).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::resume::{Door, Step, attempt, door_of, spawn_for, tests_seen};
use super::tests::{ask, entry_status, joined, ls, owner_run, shown, window};
use super::*;
use crate::api::rig::Rig;
use crate::api::{devices::Caller, dispatch};
use crate::runauth::TokenScope;
use crate::terminal::claim::{self, Kind};
use std::process::{Command, Stdio};

fn on(hub: &Hub) {
    hub.store.kv_set(RESUME_KEY, "on").unwrap();
}

fn off(hub: &Hub) {
    hub.store.kv_set(RESUME_KEY, "off").unwrap();
}

/// A temp state folder for the writer claims, so no test reads or writes Kai's real one.
fn claims(r: &Rig) -> std::path::PathBuf {
    let state = r.dir.join("claims");
    r.hub.terminal.set_env(crate::terminal::Env { state: state.clone(), sessions: None });
    state
}

fn guest(person: &str, session: &str) -> RunIdentity {
    RunIdentity {
        person: person.into(),
        agent: "house".into(),
        room: Some(format!("1:1:house:{person}")),
        run_id: format!("run-{person}-{session}"),
        scope: TokenScope::Guest,
        session: format!("run:g:{}:{person}:{session}", person.len()),
        project: None,
        reply: None,
    }
}

/// A run asks, its wait ends, its process exits (the card parks) and the approver says yes: an owed resume.
async fn approved_while_gone(r: &Rig, who: RunIdentity, input: Value) -> Card {
    window(&r.hub, 0);
    let run_id = who.run_id.clone();
    assert!(!request_permission(&r.hub, &who, "Bash", &input).await.allow);
    let card = shown(&r.hub).await;
    run_exited(&r.hub, &run_id);
    answer(&r.hub, card.entry_id.as_ref().unwrap(), Some("allow"), None).unwrap();
    let kept = r.hub.store.card(&card.id).unwrap();
    assert!(kept.state == CardState::Approved && kept.answered_parked, "{kept:?}");
    kept
}

async fn eventually<F: Fn() -> bool>(what: &str, f: F) {
    for _ in 0..200 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("never happened: {what}");
}

/// `sleep` started under a shell, so its parent is not this test process (the host's own children are never stopped).
fn sleeper() -> (std::process::Child, u32) {
    let mut sh = Command::new("sh").args(["-c", "sleep 60 & echo $!; wait"]).stdout(Stdio::piped()).spawn().unwrap();
    let mut line = String::new();
    std::io::BufRead::read_line(&mut std::io::BufReader::new(sh.stdout.as_mut().unwrap()), &mut line).unwrap();
    let pid = line.trim().parse().unwrap();
    (sh, pid)
}

fn blank() -> Card {
    Card {
        id: "c".into(),
        session: String::new(),
        source: "run".into(),
        bot_id: None,
        run_id: None,
        entry_id: None,
        person: String::new(),
        agent: "house".into(),
        approver: "owner".into(),
        answerable_by: None,
        in_grants: false,
        tool: "Bash".into(),
        input: "{}".into(),
        resource: "r".into(),
        shown: "ls".into(),
        scope: "once".into(),
        state: CardState::Approved,
        created_at: 0,
        deadline: i64::MAX,
        reply_to: None,
        asked_by: None,
        answer: None,
        allowed: Some(true),
        answered_by: None,
        answered_at: None,
        answered_parked: true,
        granted_at: Some(0),
        resumed_at: None,
        applied_at: None,
    }
}

fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(libc::pid_t::try_from(pid).unwrap(), 0) == 0 }
}

#[test]
fn a_cards_session_names_its_door() {
    let mut c = blank();
    c.session = "acp:chat1:thr1".into();
    c.bot_id = Some("bot1".into());
    assert_eq!(
        door_of(&c),
        Some(Door::Bot { bot: "bot1".into(), lane: Lane { chat: "chat1".into(), thread: Some("thr1".into()) } })
    );
    c.session = "acp:chat1:".into();
    assert!(matches!(door_of(&c), Some(Door::Bot { lane: Lane { thread: None, .. }, .. })));
    c.session = "run:o:abc-123".into();
    assert_eq!(door_of(&c), Some(Door::Run { session: "abc-123".into() }));
    c.session = "run:o:tg:1#house".into();
    assert_eq!(door_of(&c), Some(Door::Channel("tg:1#house".into())));
    c.person = "sam".into();
    c.session = "run:g:3:sam:abc-123".into();
    assert_eq!(door_of(&c), Some(Door::Run { session: "abc-123".into() }));
    c.session = "run:g:3:sam:tg:1#house".into();
    assert_eq!(door_of(&c), Some(Door::Channel("tg:1#house".into())));
    c.session = "run:g:3:bob:abc".into(); // the person in the key is not the card's
    assert_eq!(door_of(&c), None);
    c.session = "run:nonsense".into();
    assert_eq!(door_of(&c), None);
}

#[tokio::test]
async fn an_approval_after_the_park_resumes_the_session_exactly_once_even_with_a_door_copy_and_a_restart() {
    let r = Rig::new().await;
    r.owner_bot("house");
    claims(&r);
    // The yes finds the setting on: the answer itself offers the resume.
    on(&r.hub);
    window(&r.hub, 0);
    let who = owner_run("once-a");
    assert!(!request_permission(&r.hub, &who, "Bash", &ls()).await.allow);
    let card = shown(&r.hub).await;
    run_exited(&r.hub, &who.run_id);
    answer(&r.hub, card.entry_id.as_ref().unwrap(), Some("allow"), None).unwrap();
    eventually("the resume", || !tests_seen::of(&card.session).is_empty()).await;
    // A door's copy, a retry and a restart's re-derive all race: the claim lets one through.
    let hub = r.hub.clone();
    let (a, b, c) = tokio::join!(attempt(&hub, &card.id), attempt(&hub, &card.id), attempt(&hub, &card.id));
    assert_eq!((a, b, c), (Step::Done, Step::Done, Step::Done));
    start(&r.hub);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let seen = tests_seen::of(&card.session);
    assert_eq!(seen.len(), 1, "one resume per card: {seen:?}");
    assert!(seen[0].note.contains("allowed") && seen[0].note.contains("go on"), "{}", seen[0].note);
    assert_eq!(seen[0].door, "run");
    assert!(r.hub.store.card(&card.id).unwrap().resumed_at.is_some());
    assert!(r.hub.store.card(&card.id).unwrap().applied_at.is_some(), "the run started: the wake is applied (M5)");
    assert_eq!(r.hub.store.cards_awaiting_resume(now_ms()).len(), 0);
    // A restart owes it no more: the run that carried the yes started.
    assert!(r.hub.store.cards_restart(now_ms()).is_ok());
    assert_eq!(r.hub.store.cards_awaiting_resume(now_ms()).len(), 0);
    // The grant is still there for the resumed run's own ask, which uses it once (L3: no profile carries it).
    assert_eq!(r.hub.store.card(&card.id).unwrap().state, CardState::Approved);
    assert!(request_permission(&r.hub, &owner_run("once-a"), "Bash", &ls()).await.allow);
}

#[tokio::test]
async fn a_live_yes_is_not_resumed() {
    let r = Rig::new().await;
    r.owner_bot("house");
    claims(&r);
    on(&r.hub);
    window(&r.hub, 20);
    let call = ask(&r.hub, owner_run("live-a"), "Bash", ls());
    let card = shown(&r.hub).await;
    answer(&r.hub, card.entry_id.as_ref().unwrap(), Some("allow"), None).unwrap();
    assert!(call.await.unwrap().allow);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(tests_seen::of(&card.session).is_empty(), "C3: the live run continued itself");
}

#[tokio::test]
async fn a_held_session_keeps_the_resume_owed_and_shows_the_holder_until_it_exits() {
    let r = Rig::new().await;
    r.owner_bot("house");
    let state = claims(&r);
    off(&r.hub);
    let card = approved_while_gone(&r, owner_run("held-a"), ls()).await;
    let (mut sh, pid) = sleeper();
    claim::take(&state, "held-a", Kind::Terminal, pid, false).unwrap().unwrap();

    assert_eq!(attempt(&r.hub, &card.id).await, Step::Retry);
    assert!(tests_seen::of(&card.session).is_empty());
    assert!(r.hub.store.card(&card.id).unwrap().resumed_at.is_none(), "a refused claim does not set the marker");
    let held = r.hub.store.entry(card.entry_id.as_ref().unwrap()).unwrap().data["held"].clone();
    assert_eq!(
        (held["pid"].as_u64(), held["kind"].as_str(), held["session"].as_str()),
        (Some(u64::from(pid)), Some("terminal"), Some("held-a"))
    );

    // The holder exits: the next try goes through, once, and the card no longer says held.
    signal(pid);
    sh.wait().unwrap();
    assert_eq!(attempt(&r.hub, &card.id).await, Step::Done);
    assert_eq!(tests_seen::of(&card.session).len(), 1);
    assert!(r.hub.store.entry(card.entry_id.as_ref().unwrap()).unwrap().data["held"].is_null());
}

fn signal(pid: u32) {
    // SAFETY: a pid this test started.
    unsafe { libc::kill(libc::pid_t::try_from(pid).unwrap(), libc::SIGKILL) };
}

#[tokio::test]
async fn a_refused_wake_turn_gives_the_claim_back_and_stays_owed() {
    let r = Rig::new().await;
    r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let card = approved_while_gone(&r, owner_run("refused-a"), ls()).await;
    assert!(r.hub.store.card_resume_claim(&card.id, now_ms()).unwrap());
    assert!(r.hub.store.cards_awaiting_resume(now_ms()).is_empty(), "claimed");
    // The bot actor's turn start was refused (HeldByWriter): `refused` is what it calls.
    resume::refused(&r.hub, &["no-such-entry".into()], Some("refused-a"));
    assert!(r.hub.store.cards_awaiting_resume(now_ms()).is_empty(), "an unknown entry changes nothing");
    assert!(r.hub.store.card_resume_release(&card.id).unwrap());
    assert_eq!(r.hub.store.cards_awaiting_resume(now_ms()).len(), 1);
}

#[tokio::test]
async fn a_channel_wake_whose_turn_never_started_stays_owed() {
    let r = Rig::new().await;
    r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let card = approved_while_gone(&r, owner_run("tg:1#house"), ls()).await;
    assert!(matches!(door_of(&card), Some(Door::Channel(_))));
    // The wake went into the channel inbox: claimed. The turn then fails to start.
    assert!(r.hub.store.card_resume_claim(&card.id, now_ms()).unwrap());
    assert_eq!(r.hub.store.cards_awaiting_resume(now_ms()).len(), 0);
    resume::channel_failed(&r.hub, std::slice::from_ref(&card.id), true);
    assert_eq!(r.hub.store.cards_awaiting_resume(now_ms()).len(), 1, "owed again, so the retry can deliver it");
    // No channel core in this test: each retry finds the connection offline and waits, the card stays owed.
    assert_eq!(attempt(&r.hub, &card.id).await, Step::Retry);
    assert_eq!(r.hub.store.cards_awaiting_resume(now_ms()).len(), 1);
}

#[tokio::test]
async fn a_host_that_stops_before_the_wake_turn_starts_offers_it_again() {
    let r = Rig::new().await;
    r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let started = approved_while_gone(&r, owner_run("tg:1#house"), ls()).await;
    let lost = approved_while_gone(&r, owner_run("tg:2#house"), ls()).await;
    // Both wakes went into a door's queue; only the first one's turn started before the host stopped.
    assert!(r.hub.store.card_resume_claim(&started.id, now_ms()).unwrap());
    assert!(r.hub.store.card_resume_claim(&lost.id, now_ms()).unwrap());
    resume::channel_started(&r.hub, std::slice::from_ref(&started.id));
    assert!(r.hub.store.card(&started.id).unwrap().applied_at.is_some());
    assert!(r.hub.store.card(&lost.id).unwrap().applied_at.is_none());
    r.hub.store.cards_restart(now_ms()).unwrap();
    let owed: Vec<String> = r.hub.store.cards_awaiting_resume(now_ms()).into_iter().map(|c| c.id).collect();
    assert_eq!(owed, vec![lost.id.clone()], "only the wake whose turn never started is owed again");
    // A Stop gives the claim back without offering it now: it waits, owed, for the next offer.
    assert!(r.hub.store.card_resume_claim(&lost.id, now_ms()).unwrap());
    resume::channel_failed(&r.hub, std::slice::from_ref(&lost.id), false);
    assert_eq!(r.hub.store.cards_awaiting_resume(now_ms()).len(), 1);
}

#[tokio::test]
async fn a_person_revoked_before_the_resume_gets_none_and_their_cards_end() {
    let r = Rig::new().await;
    r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let _sam = joined(&r, "member").await;
    let card = approved_while_gone(&r, guest("sam", "rev-a"), ls()).await;
    assert_eq!(card.person, "sam");
    r.auth(&["member", "house", "sam", "--rm"]);
    assert_eq!(attempt(&r.hub, &card.id).await, Step::Done);
    assert!(tests_seen::of(&card.session).is_empty(), "A7: no spawn for a revoked person");
    assert_eq!(r.hub.store.card(&card.id).unwrap().state, CardState::Withdrawn);
    assert_eq!(r.hub.store.cards_awaiting_resume(now_ms()).len(), 0);
}

#[tokio::test]
async fn an_expired_card_is_not_resumed() {
    let r = Rig::new().await;
    r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let card = approved_while_gone(&r, owner_run("exp-a"), ls()).await;
    // A day passes: silence is no.
    sweep_at(&r.hub, now_ms() + 25 * 3_600_000);
    assert_eq!(attempt(&r.hub, &card.id).await, Step::Done);
    assert!(tests_seen::of(&card.session).is_empty());
    assert_eq!(r.hub.store.card(&card.id).unwrap().state, CardState::Expired);
    // Even before the sweep runs, the deadline alone keeps a late card from being owed.
    assert_eq!(r.hub.store.cards_awaiting_resume(now_ms() + 25 * 3_600_000).len(), 0);
}

fn sweep_at(hub: &Hub, at: i64) {
    hub.store.cards_sweep(at).unwrap();
}

#[tokio::test]
async fn the_p_door_resumes_as_the_cards_person() {
    let r = Rig::new().await;
    r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let _sam = joined(&r, "member").await;
    let card = approved_while_gone(&r, guest("sam", "pdoor-a"), ls()).await;
    assert_eq!(attempt(&r.hub, &card.id).await, Step::Done);
    let spawned = tests_seen::spawns_for("pdoor-a");
    assert_eq!(spawned.len(), 1);
    let env = |s: &resume::Spawn, k: &str| s.env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    assert_eq!(env(&spawned[0], "KITH_PERSON").as_deref(), Some("sam"));
    assert_eq!(env(&spawned[0], "HQ_PERSON").as_deref(), Some("sam"));
    assert_eq!(env(&spawned[0], "KITH_CLAIM_WAIT").as_deref(), Some("5"), "tied to the 8 s delivered rule");
    assert_eq!(&spawned[0].args[..3], ["house", "--resume", "pdoor-a"]);
    // The owner's resume carries no person at all.
    let owner = approved_while_gone(&r, owner_run("pdoor-b"), ls()).await;
    let s = spawn_for(&owner, "pdoor-b", "note");
    assert!(env(&s, "KITH_PERSON").is_none() && env(&s, "HQ_PERSON").is_none());
    assert!(
        env(&s, "KITH_RUN_TOKEN").is_none() && env(&s, "KITH_ASK_TOKEN").is_none(),
        "tokens are added by the caller"
    );
}

#[tokio::test]
async fn a_grant_used_by_a_new_message_first_still_gets_its_wake_without_the_grant() {
    let r = Rig::new().await;
    r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let card = approved_while_gone(&r, owner_run("used-a"), ls()).await;
    // A new message runs while the card is parked, and its ask finds the yes: the grant is spent (C7, A9).
    assert!(request_permission(&r.hub, &owner_run("used-a"), "Bash", &ls()).await.allow);
    let used = r.hub.store.card(&card.id).unwrap();
    assert_eq!(used.state, CardState::Used);
    assert_eq!(r.hub.store.cards_awaiting_resume(now_ms()).len(), 1, "the wake is still owed");
    assert_eq!(attempt(&r.hub, &card.id).await, Step::Done);
    let seen = tests_seen::of(&card.session);
    assert_eq!(seen.len(), 1);
    assert!(seen[0].note.contains("already used"), "{}", seen[0].note);
    // The resumed run asks again, and there is no grant to find: a new card.
    window(&r.hub, 0);
    assert!(!request_permission(&r.hub, &owner_run("used-a"), "Bash", &ls()).await.allow);
    assert!(r.hub.store.cards_open(None).iter().any(|c| c.session == card.session));
}

#[tokio::test]
async fn two_cards_owed_in_one_session_make_one_wake_turn() {
    let r = Rig::new().await;
    r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let a = approved_while_gone(&r, owner_run("merge-a"), json!({"command": "ls /tmp"})).await;
    let b = approved_while_gone(&r, owner_run("merge-a"), json!({"command": "ls /var"})).await;
    assert_ne!(a.id, b.id);
    assert_eq!(attempt(&r.hub, &a.id).await, Step::Done);
    assert_eq!(attempt(&r.hub, &b.id).await, Step::Done);
    let seen = tests_seen::of(&a.session);
    assert_eq!(seen.len(), 1, "L5: one turn answers both");
    assert!(seen[0].note.matches("allowed").count() == 2, "{}", seen[0].note);
    assert!(
        r.hub.store.card(&a.id).unwrap().resumed_at.is_some() && r.hub.store.card(&b.id).unwrap().resumed_at.is_some()
    );
}

#[tokio::test]
async fn a_restart_rebuilds_owed_resumes_from_the_store() {
    let r = Rig::new().await;
    r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let card = approved_while_gone(&r, owner_run("restart-a"), ls()).await;
    assert!(tests_seen::of(&card.session).is_empty(), "the setting was off: still owed");
    on(&r.hub);
    start(&r.hub); // the host starts again with the setting on
    eventually("the re-derived resume", || tests_seen::of(&card.session).len() == 1).await;
    assert_eq!(entry_status(&r.hub, &card), "answered");
}

#[tokio::test]
async fn stop_holder_is_the_owners_alone_and_ends_only_the_live_holder_it_judged() {
    let r = Rig::new().await;
    let state = claims(&r);
    let sam = joined(&r, "member").await;
    let (mut sh, pid) = sleeper();
    claim::take(&state, "stop-a", Kind::Terminal, pid, false).unwrap().unwrap();

    // A member, a bot and a paired phone's guest identity never reach it.
    let denied = dispatch(&r.hub, &sam, "stopHolder", json!({"session": "stop-a"})).await.unwrap_err();
    assert!(denied.is::<crate::api::devices::NotAllowed>(), "{denied:#}");
    assert!(!crate::api::roles::member_may("stopHolder") && !crate::api::roles::bot_may("stopHolder"));
    assert!(claim::holder(&state, "stop-a").is_some(), "nothing was signalled");

    // The owner ends it and is told who.
    let out = dispatch(&r.hub, &Caller::Local, "stopHolder", json!({"session": "stop-a"})).await.unwrap();
    assert_eq!(
        (out["stopped"]["pid"].as_u64(), out["stopped"]["kind"].as_str()),
        (Some(u64::from(pid)), Some("terminal"))
    );
    sh.wait().unwrap();
    assert!(claim::holder(&state, "stop-a").is_none());
    let again = dispatch(&r.hub, &Caller::Local, "stopHolder", json!({"session": "stop-a"})).await.unwrap();
    assert!(again["stopped"].is_null());

    // A claim whose pid was reused (it started at another time) is left alone.
    let (mut sh2, pid2) = sleeper();
    let reused = json!({"pid": pid2, "procStart": "Mon Jan 1 00:00:00 2001", "at": "2001-01-01T00:00:00Z", "kind": "terminal", "takeover": false});
    std::fs::create_dir_all(claim::path(&state, "stop-b", false).parent().unwrap()).unwrap();
    std::fs::write(claim::path(&state, "stop-b", false), reused.to_string()).unwrap();
    let out = dispatch(&r.hub, &Caller::Local, "stopHolder", json!({"session": "stop-b"})).await.unwrap();
    assert!(out["stopped"].is_null());
    assert!(alive(pid2), "the reused pid's process was not signalled");
    signal(pid2);
    sh2.wait().unwrap();

    // A path is no session id.
    assert!(dispatch(&r.hub, &Caller::Local, "stopHolder", json!({"session": "../x"})).await.is_err());
}

// ---- rule 7: a `kith ask` run that parks is a job; its card answers the asker

fn asked_run(session: &str, id: &str) -> RunIdentity {
    crate::runauth::ask_identity("house", session, crate::runauth::AskReply::parse(id, Some("architect")))
}

#[tokio::test]
async fn a_kith_ask_run_that_parks_is_told_it_waits_and_a_no_is_the_askers_answer() {
    let r = Rig::new().await;
    r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    window(&r.hub, 0);
    let who = asked_run("job-a", "ask-p1-a");
    let first = request_permission(&r.hub, &who, "Bash", &ls()).await;
    assert!(!first.allow && first.waiting, "{first:?}");
    let card = shown(&r.hub).await;
    assert_eq!((card.reply_to.as_deref(), card.asked_by.as_deref()), (Some("ask-p1-a"), Some("architect")));
    let entry = r.hub.store.entry(card.entry_id.as_ref().unwrap()).unwrap();
    assert_eq!(entry.data["chain"], "house, asked by architect", "the approver sees whose request it is");
    assert_eq!(chain_of(&blank()), None, "a card no kith ask opened shows no chain");

    run_exited(&r.hub, &who.run_id);
    answer(&r.hub, &entry.id, Some("deny"), None).unwrap();
    let told = tests_seen::answers_to("ask-p1-a");
    assert_eq!(told.len(), 1, "{told:?}");
    assert_eq!(&told[0][..6], ["ask", "--answer", "ask-p1-a", "--as", "house", "--"]);
    assert!(told[0][6].contains("was denied"), "{}", told[0][6]);
}

#[tokio::test]
async fn a_no_the_live_run_hears_is_not_sent_to_the_asker_too() {
    let r = Rig::new().await;
    r.owner_bot("house");
    window(&r.hub, 20);
    let call = ask(&r.hub, asked_run("job-b", "ask-p1-b"), "Bash", ls());
    let card = shown(&r.hub).await;
    answer(&r.hub, card.entry_id.as_ref().unwrap(), Some("deny"), None).unwrap();
    let d = call.await.unwrap();
    assert!(!d.allow && !d.waiting, "{d:?}");
    assert!(tests_seen::answers_to("ask-p1-b").is_empty(), "the run answers in its own turn");
}

#[tokio::test]
async fn an_expired_card_of_a_kith_ask_run_answers_the_asker() {
    let r = Rig::new().await;
    r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    window(&r.hub, 0);
    let who = asked_run("job-c", "ask-p1-c");
    assert!(request_permission(&r.hub, &who, "Bash", &ls()).await.waiting);
    let card = shown(&r.hub).await;
    run_exited(&r.hub, &who.run_id);
    expire(&r.hub, now_ms() + 25 * 3_600_000);
    assert_eq!(r.hub.store.card(&card.id).unwrap().state, CardState::Expired);
    let told = tests_seen::answers_to("ask-p1-c");
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(told[0][6].contains("expired without an answer"), "{}", told[0][6]);
}

#[tokio::test]
async fn a_yes_to_a_kith_ask_run_resumes_it_through_kith_ask_reply() {
    let r = Rig::new().await;
    r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let card = approved_while_gone(&r, asked_run("job-d", "ask-p1-d"), ls()).await;
    assert_eq!(attempt(&r.hub, &card.id).await, Step::Done);
    let spawned = tests_seen::spawns_for("job-d");
    assert_eq!(spawned.len(), 1);
    assert_eq!(&spawned[0].args[..7], ["ask", "--reply", "ask-p1-d", "house", "--resume", "job-d", "--"]);
    assert!(tests_seen::answers_to("ask-p1-d").is_empty(), "the resumed turn's words are the answer, not the card's");
}

#[tokio::test]
async fn an_ask_token_takes_only_a_kith_ask_id_to_reply_to() {
    let r = Rig::new().await;
    let bad = json!({"agent": "house", "session": "s", "pid": std::process::id(), "replyTo": "../x"});
    let err = dispatch(&r.hub, &Caller::Local, "issueAskToken", bad).await.unwrap_err();
    assert!(format!("{err:#}").contains("replyTo"), "{err:#}");
    let parse = crate::runauth::AskReply::parse;
    assert_eq!(parse("ask-1", Some("architect")).unwrap().asked_by.as_deref(), Some("architect"));
    assert_eq!(parse("ask-1", Some("")).unwrap().asked_by, None, "a terminal");
    assert_eq!(parse("ask-1", Some("Not An Agent")).unwrap().asked_by, None);
    assert!(parse("", None).is_none() && parse(&"a".repeat(65), None).is_none() && parse("a/b", None).is_none());
}
