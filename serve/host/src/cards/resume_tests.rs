//! The resume half on a real hub: one resume per card, a held session stays owed, revoke and expiry end it, the -p door
//! carries the card's person, and the owner's `stopHolder` (L2). The doors do not start processes or actors in tests
//! (`resume::tests_seen` records what they would have done).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::resume::{Door, Step, attempt, door_of, drive, spawn_for, tests_seen};
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

fn state(r: &Rig) -> std::path::PathBuf {
    r.hub.terminal.env().state
}

/// Where the kernel's queue has a request: queued, running, or how it was answered.
fn queue_says(r: &Rig, card: &Card) -> String {
    crate::wake::status(&state(r), &format!("card:{}", card.id)).unwrap_or_default()
}

async fn queue_says_of(r: &Rig, id: &str, want: &str) {
    for _ in 0..200 {
        if crate::wake::status(&state(r), id).as_deref() == Some(want) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the queue never said {want} of {id}: {:?}", crate::wake::status(&state(r), id));
}

async fn queue_says_eventually(r: &Rig, card: &Card, want: &str) {
    queue_says_of(r, &format!("card:{}", card.id), want).await;
}

/// The queue's items, edited as a crashed host would leave them: the turn's holder is gone.
fn host_crashed(r: &Rig) {
    let (mut gone, pid) = sleeper();
    signal(pid);
    gone.wait().unwrap();
    edit_items(r, |it| {
        if !it["turn"].is_null() {
            it["turn"]["pid"] = pid.into();
            it["turn"]["procStart"] = Value::Null;
        }
    });
    resume::forget_in_flight();
}

/// The queue's pause is over (a held session waits ten seconds, a failed try longer).
fn pause_over(r: &Rig) {
    edit_items(r, |it| it["next"] = 0.into());
}

fn edit_items(r: &Rig, f: impl Fn(&mut Value)) {
    let dir = state(r).join("sessions/queue");
    for entry in std::fs::read_dir(&dir).unwrap().flatten() {
        let path = entry.path();
        if path.file_name().is_some_and(|n| n == "answered.json") || path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let Ok(mut it) = serde_json::from_slice::<Value>(&std::fs::read(&path).unwrap()) else { continue };
        f(&mut it);
        std::fs::write(&path, it.to_string()).unwrap();
    }
}

/// A yes to a card of a bot's chat (or one of its threads) that arrived while the process was gone.
fn bot_card_approved(r: &Rig, chat: &str, thread: &str) -> Card {
    bot_card_in(r, chat, chat, thread)
}

/// An approved card of `bot`'s turn in `chat` (a room, or the bot's own chat).
fn bot_card_in(r: &Rig, chat: &str, bot: &str, thread: &str) -> Card {
    let lane = if thread.is_empty() { Lane::main(chat) } else { Lane::in_thread(chat, thread) };
    let entry = r
        .hub
        .add_entry(
            &lane,
            EntryKind::Permission,
            1,
            &json!({"status": "pending", "options": [{"optionId": "allow", "kind": "allow_once"}]}),
        )
        .unwrap();
    let new = NewCard {
        session: format!("acp:{chat}:{thread}"),
        source: "acp",
        bot_id: Some(bot.to_owned()),
        run_id: None,
        person: String::new(),
        agent: "house".into(),
        answerable_by: None,
        in_grants: false,
        tool: "Bash".into(),
        input: "{}".into(),
        resource: format!("Bash:{}", entry.id),
        shown: format!("ls {}", entry.id),
        deadline: now_ms() + 60_000,
        reply_to: None,
        asked_by: None,
    };
    let Opened::New(card) = r.hub.store.card_open(&new, now_ms()).unwrap() else { panic!() };
    r.hub.store.card_live(&card.id, Some(&entry.id), None).unwrap();
    bot_exited(&r.hub, bot);
    answer(&r.hub, &entry.id, Some("allow"), None).unwrap();
    let kept = r.hub.store.card(&card.id).unwrap();
    assert!(kept.state == CardState::Approved && kept.answered_parked, "{kept:?}");
    kept
}

fn bot_seen(card: &Card) -> Vec<tests_seen::Seen> {
    tests_seen::of(&card.session).into_iter().filter(|s| s.door == "bot").collect()
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
    assert_eq!(queue_says(&r, &card), "queued", "the kernel's queue holds it, once");
    assert_eq!(crate::wake::ls(&state(&r)).iter().map(|i| i["requests"].as_array().unwrap().len()).sum::<usize>(), 1);
    assert!(r.hub.store.card(&card.id).unwrap().resumed_at.is_some());
    assert!(r.hub.store.card(&card.id).unwrap().applied_at.is_none(), "the kernel's turn has not ended yet (W2)");
    assert_eq!(r.hub.store.cards_awaiting_resume(now_ms()).len(), 0);
    // A restart offers it again under the same id: the queue drops the copy, so there is still one wake.
    assert!(r.hub.store.cards_restart(now_ms()).is_ok());
    assert_eq!(r.hub.store.cards_awaiting_resume(now_ms()).len(), 1);
    assert_eq!(attempt(&r.hub, &card.id).await, Step::Done);
    assert_eq!(tests_seen::of(&card.session).len(), 1);
    assert_eq!(crate::wake::ls(&state(&r)).iter().map(|i| i["requests"].as_array().unwrap().len()).sum::<usize>(), 1);
    // Once the kernel's turn commits, the card is delivered and a restart owes it no more.
    let turn = crate::wake::take_session(&state(&r), "once-a");
    assert!(turn.unwrap().is_none(), "the run door's item is the kernel's, not the host's to take");
    commit_kernel_turn(&r, &card);
    resume::settle_queue(&r.hub).await;
    assert!(r.hub.store.card(&card.id).unwrap().applied_at.is_some());
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
async fn a_held_session_keeps_its_wake_in_the_queue_and_shows_the_holder_until_it_exits() {
    let r = Rig::new().await;
    r.owner_bot("house");
    let state = claims(&r);
    off(&r.hub);
    let card = approved_while_gone(&r, owner_run("held-a"), ls()).await;
    let (mut sh, pid) = sleeper();
    claim::take(&state, "held-a", Kind::Terminal, pid, false).unwrap().unwrap();

    // The kernel's queue waits for the writer (the host does not), and the card says who holds the session.
    assert_eq!(attempt(&r.hub, &card.id).await, Step::Done);
    assert_eq!(queue_says(&r, &card), "queued");
    assert!(r.hub.store.card(&card.id).unwrap().resumed_at.is_some());
    let held = r.hub.store.entry(card.entry_id.as_ref().unwrap()).unwrap().data["held"].clone();
    assert_eq!(
        (held["pid"].as_u64(), held["kind"].as_str(), held["session"].as_str()),
        (Some(u64::from(pid)), Some("terminal"), Some("held-a"))
    );
    // Its deadline passes while it waits: the yes is not silently lost to the sweep.
    sweep_at(&r.hub, now_ms() + 25 * 3_600_000);
    assert_eq!(r.hub.store.card(&card.id).unwrap().state, CardState::Approved, "an enqueued yes is not expired");

    // The holder exits: the card no longer says held, and a second try finds nothing owed.
    signal(pid);
    sh.wait().unwrap();
    resume::settle_queue(&r.hub).await;
    assert!(r.hub.store.entry(card.entry_id.as_ref().unwrap()).unwrap().data["held"].is_null());
    assert_eq!(attempt(&r.hub, &card.id).await, Step::Done);
    assert_eq!(tests_seen::of(&card.session).len(), 1, "one wake");
    commit_kernel_turn(&r, &card);
    resume::settle_queue(&r.hub).await;
    assert!(r.hub.store.card(&card.id).unwrap().applied_at.is_some());
}

/// The kernel delivered a run-door turn and its turn ended (W2): what the run door's `deliver` does, without a claude.
fn commit_kernel_turn(r: &Rig, card: &Card) {
    let item = crate::wake::ls(&state(r))
        .into_iter()
        .find(|i| i["requests"].as_array().unwrap().iter().any(|q| q["id"] == format!("card:{}", card.id)));
    let session = item.unwrap()["session"].as_str().unwrap().to_owned();
    let st = state(r);
    // deliver = take for the run door: the test is the deliverer.
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../kernel/bin/kith-wake");
    let out = Command::new("python3")
        .arg("-c")
        .arg(
            "import sys, importlib.machinery as m, importlib.util as u\n\
             l = m.SourceFileLoader('w', sys.argv[1]); sp = u.spec_from_loader('w', l); w = u.module_from_spec(sp); l.exec_module(w)\n\
             p = w.item_path(sys.argv[2]); it = w.read_json(p, {})\n\
             t = w.start_turn(p, it, 1)\n\
             w.commit(t['run'])",
        )
        .arg(&script)
        .arg(&session)
        .env("KITH_STATE", &st)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
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
async fn a_channel_wake_with_its_connection_down_stays_in_the_queue_and_is_not_lost() {
    let r = Rig::new().await;
    r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let card = approved_while_gone(&r, owner_run("tg:1#house"), ls()).await;
    assert!(matches!(door_of(&card), Some(Door::Channel(_))));
    assert_eq!(attempt(&r.hub, &card.id).await, Step::Done);
    assert_eq!(queue_says(&r, &card), "queued");
    let kept = r.hub.store.card(&card.id).unwrap();
    assert!(kept.resumed_at.is_some() && kept.applied_at.is_none(), "in the queue, no turn yet: {kept:?}");
    // No channel core in this test: the host takes the turn, finds the connection offline and gives it back, held.
    drive(&r.hub).await;
    queue_says_eventually(&r, &card, "queued").await;
    assert!(crate::wake::take(&state(&r)).unwrap().is_none(), "it waits its pause instead of being taken in a loop");
    assert!(r.hub.store.card(&card.id).unwrap().applied_at.is_none());
}

#[tokio::test]
async fn a_channel_turn_that_started_settles_its_card_and_one_that_did_not_comes_back() {
    let r = Rig::new().await;
    r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let ok = approved_while_gone(&r, owner_run("tg:1#house"), ls()).await;
    let lost = approved_while_gone(&r, owner_run("tg:2#house"), ls()).await;
    attempt(&r.hub, &ok.id).await;
    attempt(&r.hub, &lost.id).await;
    // `Core::wake` lists the turn's run beside the cards; the channel run says what happened to it.
    let ran = crate::wake::take_session(&state(&r), "tg:1#house").unwrap().unwrap();
    let never = crate::wake::take_session(&state(&r), "tg:2#house").unwrap().unwrap();
    resume::channel_started(&r.hub, &[ok.id.clone(), format!("run:{}", ran.run)]);
    resume::channel_failed(&r.hub, &[lost.id.clone(), format!("run:{}", never.run)], true);
    assert!(r.hub.store.card(&ok.id).unwrap().applied_at.is_some());
    queue_says_eventually(&r, &ok, "answered").await;
    assert!(r.hub.store.card(&lost.id).unwrap().applied_at.is_none());
    queue_says_eventually(&r, &lost, "queued").await;
}

#[tokio::test]
async fn a_host_that_stops_before_the_wake_turn_starts_delivers_it_once_after_the_restart() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let started = bot_card_approved(&r, &chat, "");
    let lost = bot_card_approved(&r, &chat, "thr");
    attempt(&r.hub, &started.id).await;
    attempt(&r.hub, &lost.id).await;
    drive(&r.hub).await;
    let (a, b) = (bot_seen(&started), bot_seen(&lost));
    assert_eq!((a.len(), b.len()), (1, 1));
    // Only the first one's turn started before the host stopped.
    resume::turn_started(&r.hub, &[a[0].entry.clone().unwrap()]);
    assert!(r.hub.store.card(&started.id).unwrap().applied_at.is_some());
    queue_says_eventually(&r, &started, "answered").await;
    assert_eq!(queue_says(&r, &lost), "running");
    host_crashed(&r);
    r.hub.store.cards_restart(now_ms()).unwrap();
    let owed: Vec<String> = r.hub.store.cards_awaiting_resume(now_ms()).into_iter().map(|c| c.id).collect();
    assert_eq!(owed, vec![lost.id.clone()], "only the wake whose turn never started is owed again");
    // The restart offers it again under the same id: the queue drops the copy, recovers the dead turn and hands it out.
    assert_eq!(attempt(&r.hub, &lost.id).await, Step::Done);
    crate::wake::recover(&state(&r));
    pause_over(&r);
    drive(&r.hub).await;
    assert_eq!(bot_seen(&started).len(), 1, "the one that started is not delivered again");
    assert_eq!(bot_seen(&lost).len(), 2, "the one that never started is delivered, once more");
    resume::turn_started(&r.hub, &[bot_seen(&lost)[1].entry.clone().unwrap()]);
    queue_says_eventually(&r, &lost, "answered").await;
    drive(&r.hub).await;
    assert_eq!(bot_seen(&lost).len(), 2, "exactly one delivery after the restart");
}

#[tokio::test]
async fn a_bot_turn_whose_writer_is_held_stays_in_the_queue_and_is_delivered_once_after_the_pause() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let card = bot_card_approved(&r, &chat, "");
    attempt(&r.hub, &card.id).await;
    drive(&r.hub).await;
    let first = bot_seen(&card);
    assert_eq!(first.len(), 1);
    // The actor's start found the session's writer held.
    resume::refused(&r.hub, &[first[0].entry.clone().unwrap()], None);
    assert_eq!(r.hub.store.cards_awaiting_resume(now_ms()).len(), 0, "the queue owns it, not the store");
    queue_says_eventually(&r, &card, "queued").await;
    drive(&r.hub).await;
    assert_eq!(bot_seen(&card).len(), 1, "a held session waits its pause");
    pause_over(&r);
    drive(&r.hub).await;
    let again = bot_seen(&card);
    assert_eq!(again.len(), 2);
    resume::turn_started(&r.hub, &[again[1].entry.clone().unwrap()]);
    queue_says_eventually(&r, &card, "answered").await;
    drive(&r.hub).await;
    assert_eq!(bot_seen(&card).len(), 2, "delivered once");
}

#[tokio::test]
async fn stop_wins_over_a_yes_already_given_also_across_a_restart() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let card = bot_card_approved(&r, &chat, "");
    attempt(&r.hub, &card.id).await;
    assert_eq!(queue_says(&r, &card), "queued");
    // The Stop button: `Hub::stop_place` calls this first.
    resume::stopped_lane(&r.hub, &chat, &Lane::main(&chat));
    assert_eq!(r.hub.store.card(&card.id).unwrap().state, CardState::Withdrawn, "closed, not merely released");
    queue_says_eventually(&r, &card, "withdrawn").await;
    // Neither a restart nor the host's look brings the stopped turn back.
    r.hub.store.cards_restart(now_ms()).unwrap();
    assert_eq!(r.hub.store.cards_awaiting_resume(now_ms()).len(), 0);
    drive(&r.hub).await;
    assert!(bot_seen(&card).is_empty(), "no turn: {:?}", bot_seen(&card));
    assert_eq!(attempt(&r.hub, &card.id).await, Step::Done);
}

/// Whether `end` was asked for `session`, waiting for the task Stop spawned to get there.
async fn ended(session: &str) -> bool {
    for _ in 0..100 {
        let asked = crate::delegation::ENDED.lock().unwrap().iter().any(|(s, _)| s.iter().any(|x| x == session));
        if asked {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

#[tokio::test]
async fn a_stop_in_a_channel_thread_ends_the_delegations_of_its_sessions() {
    let r = Rig::new().await;
    resume::stopped_thread_of(&r.hub, "t:1#house", vec!["thread-sess-1".into()]);
    assert!(ended("thread-sess-1").await, "the thread's asks end with its Stop (R8)");
}

#[tokio::test]
async fn a_stop_of_one_bot_ends_only_that_bots_delegations_in_a_shared_chat() {
    let r = Rig::new().await;
    r.hub.store.kv_set("lane.session.botx@room1", "sess-botx").unwrap();
    r.hub.store.kv_set("lane.session.boty@room1", "sess-boty").unwrap();
    resume::stopped_lane(&r.hub, "botx", &Lane::main("room1"));
    assert!(ended("sess-botx").await);
    assert!(!crate::delegation::ENDED.lock().unwrap().iter().any(|(s, _)| s.iter().any(|x| x == "sess-boty")));
}

#[tokio::test]
async fn after_a_stop_another_yes_makes_only_its_own_turn() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let old = bot_card_approved(&r, &chat, "");
    attempt(&r.hub, &old.id).await;
    resume::stopped_lane(&r.hub, &chat, &Lane::main(&chat));
    queue_says_eventually(&r, &old, "withdrawn").await;
    let new = bot_card_approved(&r, &chat, "");
    assert_eq!(attempt(&r.hub, &new.id).await, Step::Done);
    drive(&r.hub).await;
    let seen = bot_seen(&new);
    assert_eq!(seen.len(), 1);
    assert!(seen[0].note.contains(&new.shown) && !seen[0].note.contains(&old.shown), "{}", seen[0].note);
    assert_eq!(r.hub.store.card(&old.id).unwrap().state, CardState::Withdrawn);
}

#[tokio::test]
async fn a_stop_that_finds_a_turn_taken_but_not_started_drops_it() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let card = bot_card_approved(&r, &chat, "");
    attempt(&r.hub, &card.id).await;
    drive(&r.hub).await;
    let entry = bot_seen(&card)[0].entry.clone().unwrap();
    assert_eq!(queue_says(&r, &card), "running");
    // The actor's queue held the wake when Stop came: it is dropped, and the card ends.
    resume::stopped_lane(&r.hub, &chat, &Lane::main(&chat));
    resume::not_started(&r.hub, &[entry], false);
    queue_says_eventually(&r, &card, "withdrawn").await;
    assert_eq!(r.hub.store.card(&card.id).unwrap().state, CardState::Withdrawn);
    drive(&r.hub).await;
    assert_eq!(bot_seen(&card).len(), 1);
}

#[tokio::test]
async fn a_kith_ask_answer_for_a_persons_session_is_taken_and_delivered_by_the_host() {
    let r = Rig::new().await;
    r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let _sam = joined(&r, "member").await;
    // What `kith ask` writes for an answer to a person's run: a host-door item nobody took before.
    let req = crate::wake::Request {
        session: "ask-sid-1",
        id: "ask:job-9/1",
        agent: "house",
        person: Some("sam"),
        room: None,
        host_door: true,
        why: "architect answered your kith ask job-9",
        text: "use the staging box",
    };
    assert_eq!(crate::wake::enqueue(&state(&r), &req).unwrap(), crate::wake::Queued::New);
    drive(&r.hub).await;
    let spawned = tests_seen::spawns_for("ask-sid-1");
    assert_eq!(spawned.len(), 1);
    assert_eq!(&spawned[0].args[..3], ["house", "--resume", "ask-sid-1"]);
    assert!(spawned[0].args[3].contains("use the staging box") && spawned[0].args[3].contains("[kith wake "));
    assert_eq!(spawned[0].env.iter().find(|(k, _)| k == "KITH_PERSON").map(|(_, v)| v.as_str()), Some("sam"));
    queue_says_of(&r, "ask:job-9/1", "answered").await;
    drive(&r.hub).await;
    assert_eq!(tests_seen::spawns_for("ask-sid-1").len(), 1, "once");
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
    drive(&r.hub).await; // a person's run is a host-door item: the host takes it and starts it
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
    let chat = r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let a = bot_card_approved(&r, &chat, "");
    let b = bot_card_approved(&r, &chat, "");
    assert_ne!(a.id, b.id);
    assert_eq!(attempt(&r.hub, &a.id).await, Step::Done);
    assert_eq!(attempt(&r.hub, &b.id).await, Step::Done);
    drive(&r.hub).await;
    let seen = bot_seen(&a);
    assert_eq!(seen.len(), 1, "L5: one turn answers both");
    assert!(seen[0].note.matches("allowed").count() == 2, "{}", seen[0].note);
    assert!(
        r.hub.store.card(&a.id).unwrap().resumed_at.is_some() && r.hub.store.card(&b.id).unwrap().resumed_at.is_some()
    );
    resume::turn_started(&r.hub, &[seen[0].entry.clone().unwrap()]);
    assert!(
        r.hub.store.card(&a.id).unwrap().applied_at.is_some() && r.hub.store.card(&b.id).unwrap().applied_at.is_some()
    );
}

#[tokio::test]
async fn two_owner_session_cards_are_one_item_of_the_kernels_queue() {
    let r = Rig::new().await;
    r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let a = approved_while_gone(&r, owner_run("merge-a"), json!({"command": "ls /tmp"})).await;
    let b = approved_while_gone(&r, owner_run("merge-a"), json!({"command": "ls /var"})).await;
    assert_eq!(attempt(&r.hub, &a.id).await, Step::Done);
    assert_eq!(attempt(&r.hub, &b.id).await, Step::Done);
    let items = crate::wake::ls(&state(&r));
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!((items[0]["session"].as_str(), items[0]["door"].as_str()), (Some("merge-a"), Some("run")));
    assert_eq!(items[0]["requests"].as_array().unwrap().len(), 2, "kith-wake merges them into one turn");
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
    drive(&r.hub).await; // `kith ask --reply` is the host's to start
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

// ---- review of #473

/// What the queue says of a card the host never enqueued itself: written as the kernel would have left it.
fn answered_in_queue(r: &Rig, id: &str, how: &str) {
    let path = state(r).join("sessions/queue/answered.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut all: Value = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(json!({}));
    all[id] = json!({"how": how, "t": now_ms() / 1000, "run": null});
    std::fs::write(&path, all.to_string()).unwrap();
}

#[tokio::test]
async fn a_card_the_queue_answered_is_applied_but_one_it_failed_or_could_not_be_asked_about_is_not() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let (done, failed) = (bot_card_approved(&r, &chat, "a"), bot_card_approved(&r, &chat, "b"));
    answered_in_queue(&r, &format!("card:{}", done.id), "answered, exit 1");
    answered_in_queue(&r, &format!("card:{}", failed.id), "failed");
    assert_eq!(attempt(&r.hub, &done.id).await, Step::Done);
    assert!(r.hub.store.card(&done.id).unwrap().applied_at.is_some(), "answered: a restart owes it no more");
    assert_eq!(attempt(&r.hub, &failed.id).await, Step::Done);
    let f = r.hub.store.card(&failed.id).unwrap();
    assert!(f.applied_at.is_none() && f.resumed_at.is_some(), "a failed wake is not delivered, so not applied: {f:?}");
    // A status call that fails says nothing about the request: it is None, not "unknown", and the card stays owed.
    let not_a_folder = r.dir.join("a-file");
    std::fs::write(&not_a_folder, "x").unwrap();
    assert_eq!(crate::wake::status(&not_a_folder, "card:x"), None);
}

#[tokio::test]
async fn the_other_requests_of_a_turn_are_said_even_when_its_cards_died() {
    let r = Rig::new().await;
    r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let _sam = joined(&r, "member").await;
    let card = approved_while_gone(&r, guest("sam", "mixed-a"), ls()).await;
    assert_eq!(attempt(&r.hub, &card.id).await, Step::Done);
    // A kith ask answer for the same session merges into the item; then Stop closes the card.
    let ask = crate::wake::Request {
        session: "mixed-a",
        id: "ask:job-1/1",
        agent: "house",
        person: Some("sam"),
        room: None,
        host_door: true,
        why: "architect answered",
        text: "use the staging box",
    };
    assert_eq!(crate::wake::enqueue(&state(&r), &ask).unwrap(), crate::wake::Queued::New);
    r.hub.store.card_resume_close(&card.id).unwrap();
    drive(&r.hub).await;
    let spawned = tests_seen::spawns_for("mixed-a");
    assert_eq!(spawned.len(), 1, "the ask answer is delivered");
    let note = &spawned[0].args[3];
    assert!(note.contains("use the staging box"), "{note}");
    assert!(!note.contains(&card.shown) && !note.contains("0 things"), "{note}");
    assert_eq!(crate::wake::status(&state(&r), "ask:job-1/1").as_deref(), Some("answered"));
    assert_eq!(crate::wake::status(&state(&r), &format!("card:{}", card.id)).as_deref(), Some("answered"));
}

#[tokio::test]
async fn a_stopped_saved_key_card_is_not_woken_later_even_if_its_request_was_on_its_way() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let card = bot_card_approved(&r, &chat, "");
    // Stop closes it while an attempt had already claimed it and is about to enqueue (the stale copy below).
    assert!(r.hub.store.card_resume_claim(&card.id, now_ms()).unwrap());
    resume::stopped_lane(&r.hub, &chat, &Lane::main(&chat));
    let door = door_of(&card).unwrap();
    resume::enqueue(&r.hub, &card, &door).await.unwrap();
    queue_says_eventually(&r, &card, "withdrawn").await;
    drive(&r.hub).await;
    assert!(bot_seen(&card).is_empty(), "the stopped turn did not run");
    // The same for a card that keeps its record: the store marks it not owed instead of withdrawing it.
    let kept = bot_card_approved(&r, &chat, "k");
    assert!(r.hub.store.card_resume_stop(&kept.id, now_ms()).unwrap());
    let after = r.hub.store.card(&kept.id).unwrap();
    assert_eq!(after.state, CardState::Approved, "still shows as saved");
    assert!(after.applied_at.is_some() && r.hub.store.cards_awaiting_resume(now_ms()).is_empty());
    r.hub.store.cards_restart(now_ms()).unwrap();
    assert!(r.hub.store.cards_awaiting_resume(now_ms()).is_empty(), "a restart doesn't bring it back");
}

#[tokio::test]
async fn a_turn_waiting_behind_a_long_actor_turn_is_not_given_back_but_a_lost_one_is() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let card = bot_card_approved(&r, &chat, "");
    attempt(&r.hub, &card.id).await;
    drive(&r.hub).await;
    assert_eq!(bot_seen(&card).len(), 1);
    // Long after, the actor still has it (its entry is known): nothing is sent twice.
    resume::reap_for_test(&r.hub, Duration::ZERO);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(queue_says(&r, &card), "running", "the actor has it");
    // A turn nobody holds a callback for is given back.
    let lost = bot_card_approved(&r, &chat, "t");
    attempt(&r.hub, &lost.id).await;
    let turn = crate::wake::take_session(&state(&r), &format!("acp:{chat}:t")).unwrap().unwrap();
    resume::lose_track_of(&turn.run);
    resume::reap_for_test(&r.hub, Duration::ZERO);
    queue_says_eventually(&r, &lost, "queued").await;
}

#[tokio::test]
async fn a_stop_that_races_a_take_runs_nothing() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let card = bot_card_approved(&r, &chat, "");
    attempt(&r.hub, &card.id).await;
    // The card was closed after the queue had it but before the host's door looked (the withdraw is still on its way).
    r.hub.store.card_resume_close(&card.id).unwrap();
    drive(&r.hub).await;
    assert!(bot_seen(&card).is_empty());
    queue_says_eventually(&r, &card, "withdrawn").await;
}

#[tokio::test]
async fn a_card_whose_wake_failed_or_was_forgotten_ends_instead_of_staying_approved() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    // A host-door wake that failed after its tries, and a kernel run-door one: both end.
    let host = bot_card_approved(&r, &chat, "");
    let kernel = approved_while_gone(&r, owner_run("fail-k"), ls()).await;
    attempt(&r.hub, &host.id).await;
    attempt(&r.hub, &kernel.id).await;
    for card in [&host, &kernel] {
        let session = if card.id == host.id { format!("acp:{chat}:") } else { "fail-k".into() };
        crate::wake::withdraw(&state(&r), &session, None); // the item is gone; now it is recorded as failed
        answered_in_queue(&r, &format!("card:{}", card.id), "failed");
    }
    resume::settle_queue(&r.hub).await;
    assert_eq!(r.hub.store.card(&host.id).unwrap().state, CardState::Withdrawn);
    assert_eq!(r.hub.store.card(&kernel.id).unwrap().state, CardState::Withdrawn);
    // The queue forgot the id (its memory is a week) and the deadline passed: the card ends too; before that it waits.
    let forgotten = bot_card_approved(&r, &chat, "f");
    assert!(r.hub.store.card_resume_claim(&forgotten.id, now_ms()).unwrap());
    resume::settle_queue(&r.hub).await;
    assert_eq!(r.hub.store.card(&forgotten.id).unwrap().state, CardState::Approved, "before its deadline it waits");
    // One miss is not enough (a commit in progress is in neither file for a moment): the second pass ends it.
    resume::settle_queue_at(&r.hub, now_ms() + 120_000).await;
    assert_eq!(r.hub.store.card(&forgotten.id).unwrap().state, CardState::Approved, "one miss is not an answer");
    resume::settle_queue_at(&r.hub, now_ms() + 120_000).await;
    assert_eq!(r.hub.store.card(&forgotten.id).unwrap().state, CardState::Withdrawn);
}

#[tokio::test]
async fn a_restart_keeps_the_claim_of_a_card_the_queue_holds_past_its_deadline() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let card = bot_card_approved(&r, &chat, "");
    attempt(&r.hub, &card.id).await;
    assert_eq!(queue_says(&r, &card), "queued");
    host_crashed(&r);
    let queued = resume::queued_cards(&r.hub);
    r.hub.store.cards_restart_keeping(now_ms(), &queued).unwrap();
    // The deadline passes (a long terminal hold): the sweep leaves the claimed card alone and the take delivers it once.
    let late = now_ms() + 3 * 3_600_000;
    r.hub.store.cards_sweep(late).unwrap();
    assert_eq!(r.hub.store.card(&card.id).unwrap().state, CardState::Approved);
    resume::settle_queue_at(&r.hub, late).await;
    resume::settle_queue_at(&r.hub, late).await;
    assert_eq!(r.hub.store.card(&card.id).unwrap().state, CardState::Approved, "the queue still holds it");
    drive(&r.hub).await;
    assert_eq!(bot_seen(&card).len(), 1);
}

#[tokio::test]
async fn a_bot_that_left_the_group_is_known_to_have_left() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    let other = r.owner_bot("other");
    claims(&r);
    off(&r.hub);
    let group: crate::store::BotConfig = serde_json::from_value(json!({
        "id": "", "name": "room", "kind": "group", "backend": "claude", "cwd": r.dir.to_string_lossy(),
        "permission": "default", "tasks": false, "connectors": [], "skills": [], "members": [other],
    }))
    .unwrap();
    let room = r.hub.create_bot(group).unwrap()["id"].as_str().unwrap().to_owned();
    // `chat` is not a member of the room: the door gives the turn up.
    let lane = Lane::main(&room);
    assert!(resume::left_room_for_test(&r.hub, &chat, &lane));
    assert!(!resume::left_room_for_test(&r.hub, &other, &lane));
    assert!(!resume::left_room_for_test(&r.hub, &chat, &Lane::main(&chat)));
}

#[tokio::test]
async fn a_bot_actor_that_shuts_down_gives_its_queued_wake_back_and_a_lost_tracked_turn_is_given_back_in_the_end() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let card = bot_card_approved(&r, &chat, "");
    attempt(&r.hub, &card.id).await;
    drive(&r.hub).await;
    let entry = bot_seen(&card)[0].entry.clone().unwrap();
    assert_eq!(queue_says(&r, &card), "running");
    resume::actor_gone(&r.hub, &[entry]);
    queue_says_eventually(&r, &card, "queued").await;
    // A tracked turn whose callback never comes is given back after the upper bound (here: no wait at all).
    pause_over(&r);
    drive(&r.hub).await;
    assert_eq!(queue_says(&r, &card), "running");
    resume::reap_for_test(&r.hub, Duration::ZERO);
    queue_says_eventually(&r, &card, "running").await;
    resume::reap_long_for_test(&r.hub);
    queue_says_eventually(&r, &card, "queued").await;
}

// ---- an app message to a thread a terminal holds waits as a wake

/// A task thread of the owner's bot whose session `held-s` a live terminal holds. Keep the child alive.
fn held_thread(r: &Rig, chat: &str) -> (String, std::process::Child, u32) {
    let root = r
        .hub
        .add_entry(&Lane::main(chat), EntryKind::User, 1, &json!({"text": "start", "status": "sent", "task": true}))
        .unwrap()
        .id;
    let lane = Lane::in_thread(chat, &root);
    r.hub.store.kv_set(&crate::store::lane_key("session", chat, &lane), "held-s").unwrap();
    let (child, pid) = sleeper();
    assert!(claim::take(&state(r), "held-s", Kind::Terminal, pid, false).unwrap().is_ok());
    (root, child, pid)
}

async fn send_to(r: &Rig, chat: &str, root: &str, text: &str, nonce: &str) -> Value {
    let out = dispatch(
        &r.hub,
        &Caller::Local,
        "send",
        json!({"botId": chat, "threadId": root, "text": text, "clientNonce": nonce}),
    )
    .await
    .unwrap();
    out["entry"].clone()
}

/// Waits until the queue's background give-back of a held take has landed (no item has a turn).
async fn given_back(r: &Rig) {
    for _ in 0..200 {
        if crate::wake::ls(&state(r)).iter().all(|i| i["turn"].is_null()) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn waiting_ids(r: &Rig) -> Vec<String> {
    let mut ids: Vec<String> = crate::wake::ls(&state(r))
        .iter()
        .flat_map(|i| i["requests"].as_array().unwrap().iter().map(|q| q["id"].as_str().unwrap().to_owned()))
        .collect();
    ids.sort();
    ids
}

fn messages_seen(chat: &str, root: &str) -> Vec<tests_seen::Seen> {
    tests_seen::of(&format!("acp:{chat}:{root}")).into_iter().filter(|s| s.door == "bot").collect()
}

#[tokio::test]
async fn messages_to_a_held_thread_wait_and_go_out_as_one_turn_once_the_terminal_lets_go() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let (root, _terminal, pid) = held_thread(&r, &chat);
    let (a, b) = (send_to(&r, &chat, &root, "first", "n1").await, send_to(&r, &chat, &root, "second", "n2").await);
    for e in [&a, &b] {
        assert_eq!(e["data"]["status"], "queued", "queued, not failed: {e}");
        assert_eq!(e["data"]["waitsFor"], "terminal");
    }
    let ids = |e: &Value| format!("msg:{}", e["id"].as_str().unwrap());
    let mut both = vec![ids(&a), ids(&b)];
    both.sort();
    assert_eq!(waiting_ids(&r), both, "one item for the thread, one request each");
    // A double send is the same message.
    assert_eq!(send_to(&r, &chat, &root, "first", "n1").await["id"], a["id"]);
    assert_eq!(waiting_ids(&r).len(), 2);
    // The terminal still holds it: the take is given back, held, and nothing reaches an actor.
    drive(&r.hub).await;
    assert!(messages_seen(&chat, &root).is_empty());
    given_back(&r).await;
    pause_over(&r);
    drive(&r.hub).await;
    assert!(messages_seen(&chat, &root).is_empty(), "still held");
    // The give-back of this second try is written in the background: let it land before the pause is cleared again,
    // or it would put the pause back after the terminal ended.
    given_back(&r).await;
    // The terminal ends.
    claim::release(&state(&r), "held-s", pid, false);
    pause_over(&r);
    drive(&r.hub).await;
    let seen = messages_seen(&chat, &root);
    assert_eq!(seen.iter().map(|s| s.note.as_str()).collect::<Vec<_>>(), vec!["first", "second"], "in order");
    let entries: Vec<String> = seen.iter().map(|s| s.entry.clone().unwrap()).collect();
    assert_eq!(entries, vec![a["id"].as_str().unwrap(), b["id"].as_str().unwrap()]);
    // The actor starts the turn for both, once.
    resume::turn_started(&r.hub, &entries);
    queue_says_of(&r, &ids(&a), "answered").await;
    queue_says_of(&r, &ids(&b), "answered").await;
    drive(&r.hub).await;
    resume::kick();
    drive(&r.hub).await;
    assert_eq!(messages_seen(&chat, &root).len(), 2, "delivered once, not again when the queue runs");
}

#[tokio::test]
async fn a_message_and_a_card_answered_meanwhile_share_one_turn() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let (root, _terminal, pid) = held_thread(&r, &chat);
    let e = send_to(&r, &chat, &root, "carry on", "n1").await;
    let card = bot_card_approved(&r, &chat, &root);
    attempt(&r.hub, &card.id).await;
    claim::release(&state(&r), "held-s", pid, false);
    drive(&r.hub).await;
    let seen = tests_seen::of(&card.session);
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert!(seen[0].note.contains(&card.shown) && !seen[0].note.contains("carry on"), "the notice is the card's only");
    assert_eq!(seen[1].note, "carry on");
    assert_eq!(seen[1].entry.as_deref(), e["id"].as_str());
}

#[tokio::test]
async fn stop_cancels_the_messages_that_wait_and_withdraws_their_wake() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let (root, _terminal, pid) = held_thread(&r, &chat);
    let e = send_to(&r, &chat, &root, "never mind", "n1").await;
    let id = e["id"].as_str().unwrap().to_owned();
    dispatch(&r.hub, &Caller::Local, "stop", json!({"botId": chat, "threadId": root})).await.unwrap();
    let after = r.hub.store.entry(&id).unwrap();
    assert_eq!(after.data["status"], "cancelled");
    assert!(after.data["waitsFor"].is_null());
    queue_says_of(&r, &format!("msg:{id}"), "withdrawn").await;
    claim::release(&state(&r), "held-s", pid, false);
    drive(&r.hub).await;
    assert!(messages_seen(&chat, &root).is_empty(), "Stop wins: the freed thread runs nothing");
}

#[tokio::test]
async fn a_waiting_message_can_be_taken_back_and_a_restart_keeps_it() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let (root, _terminal, pid) = held_thread(&r, &chat);
    let kept = send_to(&r, &chat, &root, "keep", "n1").await;
    let unsent = send_to(&r, &chat, &root, "undo", "n2").await;
    // A restart closes ordinary queued sends, not these: the queue is their record.
    let plain =
        r.hub.add_entry(&Lane::main(&chat), EntryKind::User, 3, &json!({"text": "x", "status": "queued"})).unwrap();
    r.hub.store.expire_pending().unwrap();
    assert_eq!(r.hub.store.entry(&plain.id).unwrap().data["status"], "failed");
    assert_eq!(r.hub.store.entry(kept["id"].as_str().unwrap()).unwrap().data["status"], "queued");
    let out =
        dispatch(&r.hub, &Caller::Local, "unsend", json!({"botId": chat, "entryId": unsent["id"]})).await.unwrap();
    assert_eq!(out["entry"]["data"]["status"], "cancelled");
    claim::release(&state(&r), "held-s", pid, false);
    drive(&r.hub).await;
    let seen = messages_seen(&chat, &root);
    assert_eq!(seen.iter().map(|s| s.note.as_str()).collect::<Vec<_>>(), vec!["keep"]);
}

#[tokio::test]
async fn a_message_the_actor_found_the_thread_held_for_becomes_a_wake_instead_of_failing() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let (root, _terminal, _pid) = held_thread(&r, &chat);
    // The message was accepted before the terminal took the thread: queued with no wake, as `send` leaves it.
    let lane = Lane::in_thread(&chat, &root);
    let e = r
        .hub
        .add_entry(&lane, EntryKind::User, 2, &json!({"text": "late", "status": "queued", "attachments": []}))
        .unwrap();
    resume::wait_for_terminal(&r.hub, "house", &e.id).await.unwrap();
    let kept = r.hub.store.entry(&e.id).unwrap();
    assert_eq!((kept.data["status"].as_str(), kept.data["waitsFor"].as_str()), (Some("queued"), Some("terminal")));
    assert_eq!(waiting_ids(&r), vec![format!("msg:{}", e.id)]);
    // Asked again for the same message (the start was refused once more): the queue already has it.
    resume::wait_for_terminal(&r.hub, "house", &e.id).await.unwrap();
    assert_eq!(waiting_ids(&r).len(), 1);
}

#[tokio::test]
async fn a_guest_or_another_writer_still_gets_the_refusal() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let (root, _terminal, pid) = held_thread(&r, &chat);
    let refused = crate::terminal::send_gate(&r.hub, &chat, &root, false).await.unwrap_err();
    assert_eq!(refused.downcast_ref::<crate::terminal::HeldByWriter>().unwrap().code, "liveInTerminal");
    // A bot guest's turn holds it: even the owner's wait is not for that writer.
    claim::release(&state(&r), "held-s", pid, false);
    assert!(claim::take(&state(&r), "held-s", Kind::Bot, pid, false).unwrap().is_ok());
    let refused = crate::terminal::send_gate(&r.hub, &chat, &root, true).await.unwrap_err();
    assert_eq!(refused.downcast_ref::<crate::terminal::HeldByWriter>().unwrap().code, "heldByOtherWriter");
    // An invited person's chat is not the owner's: no wait either.
    claim::release(&state(&r), "held-s", pid, false);
    assert!(claim::take(&state(&r), "held-s", Kind::Terminal, pid, false).unwrap().is_ok());
    let cfg: crate::store::BotConfig = serde_json::from_value(json!({
        "id": "", "name": "mia's house", "backend": "claude", "cwd": r.dir.to_string_lossy(), "hqAgent": "house",
        "person": "mia", "permission": "default", "tasks": false, "connectors": [], "skills": [],
    }))
    .unwrap();
    let theirs = r.hub.create_bot(cfg).unwrap()["id"].as_str().unwrap().to_owned();
    // The same live claim holds their thread's session too.
    let their_root = r
        .hub
        .add_entry(&Lane::main(&theirs), EntryKind::User, 1, &json!({"text": "start", "status": "sent", "task": true}))
        .unwrap()
        .id;
    let their_lane = Lane::in_thread(&theirs, &their_root);
    r.hub.store.kv_set(&crate::store::lane_key("session", &theirs, &their_lane), "held-s").unwrap();
    let member =
        dispatch(&r.hub, &Caller::Local, "send", json!({"botId": theirs, "threadId": their_root, "text": "hi"}))
            .await
            .unwrap_err();
    assert!(member.to_string().starts_with("liveInTerminal"), "{member}");
    assert_eq!(waiting_ids(&r).len(), 0);
}

#[tokio::test]
async fn a_bot_that_left_the_room_closes_the_cards_its_turn_had_claimed() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    let other = r.owner_bot("other");
    claims(&r);
    off(&r.hub);
    let group: crate::store::BotConfig = serde_json::from_value(json!({
        "id": "", "name": "room", "kind": "group", "backend": "claude", "cwd": r.dir.to_string_lossy(),
        "permission": "default", "tasks": false, "connectors": [], "skills": [], "members": [chat],
    }))
    .unwrap();
    let room = r.hub.create_bot(group).unwrap()["id"].as_str().unwrap().to_owned();
    let card = bot_card_in(&r, &room, &chat, "");
    assert_eq!(attempt(&r.hub, &card.id).await, Step::Done);
    assert_eq!(queue_says(&r, &card), "queued");
    r.hub.update_bot(&json!({"id": room, "members": [other]})).unwrap();
    drive(&r.hub).await;
    assert_eq!(r.hub.store.card(&card.id).unwrap().state, CardState::Withdrawn, "claimed and still closed");
    queue_says_eventually(&r, &card, "withdrawn").await;
    assert_eq!(r.hub.store.cards_awaiting_resume(now_ms()).len(), 0);
}

#[tokio::test]
async fn the_forgotten_cards_miss_is_dropped_when_the_card_leaves_the_set() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let card = bot_card_approved(&r, &chat, "");
    assert!(r.hub.store.card_resume_claim(&card.id, now_ms()).unwrap());
    let late = now_ms() + 120_000;
    resume::settle_queue_at(&r.hub, late).await;
    assert!(resume::missed_for_test(&card.id), "one miss is remembered");
    // The card ends some other way (a Stop): its miss goes with it, not kept for ever.
    r.hub.store.card_resume_close(&card.id).unwrap();
    resume::settle_queue_at(&r.hub, late).await;
    assert!(!resume::missed_for_test(&card.id));
}

#[tokio::test]
async fn one_attempt_marks_every_card_of_its_session_and_counts_per_queue() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let (a, b) = (bot_card_approved(&r, &chat, ""), bot_card_approved(&r, &chat, ""));
    let before = resume::enqueued_for_test(&r.hub);
    // Another host's queue (here: a state folder of nobody's) finishing enqueues does not make this one's settle unsure.
    resume::enqueue_elsewhere_for_test(&r.dir.join("elsewhere"));
    assert_eq!(resume::enqueued_for_test(&r.hub), before, "per queue");
    assert_eq!(attempt(&r.hub, &a.id).await, Step::Done);
    assert_eq!((queue_says(&r, &a), queue_says(&r, &b)), ("queued".into(), "queued".into()), "one attempt, both cards");
    assert!(!resume::enqueuing_for_test(&a.id) && !resume::enqueuing_for_test(&b.id), "no mark is left behind");
    assert!(resume::enqueued_for_test(&r.hub) >= before + 2);
}

fn status_of(r: &Rig, e: &Value) -> (String, Option<String>) {
    let now = r.hub.store.entry(e["id"].as_str().unwrap()).unwrap();
    (now.data["status"].as_str().unwrap().to_owned(), now.data["waitsFor"].as_str().map(str::to_owned))
}

#[tokio::test]
async fn a_wake_turn_that_fails_to_start_leaves_its_messages_waiting_not_sent() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let (root, _terminal, pid) = held_thread(&r, &chat);
    let (a, b) = (send_to(&r, &chat, &root, "one", "n1").await, send_to(&r, &chat, &root, "two", "n2").await);
    claim::release(&state(&r), "held-s", pid, false);
    drive(&r.hub).await;
    // With the actor: still queued (its prompt has not gone), not waiting for the terminal any more.
    assert_eq!(status_of(&r, &a), ("queued".into(), Some("turn".into())));
    let entries: Vec<String> = messages_seen(&chat, &root).iter().map(|s| s.entry.clone().unwrap()).collect();
    assert_eq!(entries.len(), 2);
    // The start failed (the terminal got in again): both go back to waiting for it, and the queue has them.
    resume::refused(&r.hub, &entries, None);
    assert_eq!(status_of(&r, &a), ("queued".into(), Some("terminal".into())));
    assert_eq!(status_of(&r, &b), ("queued".into(), Some("terminal".into())));
    queue_says_of(&r, &format!("msg:{}", a["id"].as_str().unwrap()), "queued").await;
    pause_over(&r);
    drive(&r.hub).await;
    assert_eq!(messages_seen(&chat, &root).len(), 4, "offered again as one turn");
}

#[tokio::test]
async fn the_run_is_answered_only_when_every_message_of_it_has_started() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let (root, _terminal, pid) = held_thread(&r, &chat);
    let (a, b) = (send_to(&r, &chat, &root, "one", "n1").await, send_to(&r, &chat, &root, "two", "n2").await);
    claim::release(&state(&r), "held-s", pid, false);
    drive(&r.hub).await;
    let (ia, ib) = (a["id"].as_str().unwrap().to_owned(), b["id"].as_str().unwrap().to_owned());
    // The actor started a turn with the first only: the second is still in its queue.
    resume::turn_started(&r.hub, std::slice::from_ref(&ia));
    assert_eq!(status_of(&r, &a).0, "sent");
    assert_eq!(
        status_of(&r, &b),
        ("queued".into(), Some("turn".into())),
        "not marked sent while it waits in the actor"
    );
    queue_says_of(&r, &format!("msg:{ib}"), "running").await;
    resume::turn_started(&r.hub, std::slice::from_ref(&ib));
    assert_eq!(status_of(&r, &b).0, "sent");
    queue_says_of(&r, &format!("msg:{ib}"), "answered").await;
}

#[tokio::test]
async fn a_message_taken_back_from_the_actor_frees_its_run_for_later_wakes() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let (root, _terminal, pid) = held_thread(&r, &chat);
    let a = send_to(&r, &chat, &root, "one", "n1").await;
    claim::release(&state(&r), "held-s", pid, false);
    drive(&r.hub).await;
    assert_eq!(messages_seen(&chat, &root).len(), 1);
    let id = a["id"].as_str().unwrap().to_owned();
    // The actor's unsend: the run is settled, not held for hours.
    resume::unsent(&r.hub, &id);
    assert!(!resume::wake_carries(&id));
    queue_says_of(&r, &format!("msg:{id}"), "withdrawn").await;
    // A message sent afterwards is not blocked behind it.
    assert!(claim::take(&state(&r), "held-s", Kind::Terminal, pid, false).unwrap().is_ok());
    let c = send_to(&r, &chat, &root, "later", "n3").await;
    claim::release(&state(&r), "held-s", pid, false);
    drive(&r.hub).await;
    let seen = messages_seen(&chat, &root);
    assert_eq!(seen.iter().map(|s| s.note.as_str()).collect::<Vec<_>>(), ["one", "later"], "{seen:?}");
    assert_eq!(seen.last().unwrap().entry.as_deref(), c["id"].as_str());
}

#[tokio::test]
async fn a_refused_start_of_a_card_and_messages_together_puts_each_back_where_it_came_from() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    claims(&r);
    off(&r.hub);
    let (root, _terminal, pid) = held_thread(&r, &chat);
    let e = send_to(&r, &chat, &root, "carry on", "n1").await;
    let card = bot_card_approved(&r, &chat, &root);
    attempt(&r.hub, &card.id).await;
    claim::release(&state(&r), "held-s", pid, false);
    drive(&r.hub).await;
    let entries: Vec<String> = tests_seen::of(&card.session).iter().map(|s| s.entry.clone().unwrap()).collect();
    assert_eq!(entries.len(), 2, "the card's notice and the message");
    // The actor batched both, the gate refused: the message waits again, the card's wake is owed again.
    resume::refused(&r.hub, &entries, Some("held-s"));
    assert_eq!(status_of(&r, &e), ("queued".into(), Some("terminal".into())));
    queue_says_eventually(&r, &card, "queued").await;
    queue_says_of(&r, &format!("msg:{}", e["id"].as_str().unwrap()), "queued").await;
}
