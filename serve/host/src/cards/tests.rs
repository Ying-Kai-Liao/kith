//! Cards on a real hub: the run socket's `requestPermission`, answers, and the edges (A1-A9, C3, C4, C6, C8).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::api::devices::Caller;
use crate::api::rig::Rig;
use crate::api::{dispatch, roles};
use crate::remote::members::{self, Listed};
use crate::runauth::{TokenScope, ask_identity};
use std::time::Duration;

pub(super) fn owner_run(session: &str) -> RunIdentity {
    ask_identity("house", session, None)
}

pub(super) fn guest_run(person: &str, room: &str) -> RunIdentity {
    RunIdentity {
        person: person.into(),
        agent: "house".into(),
        room: Some(room.into()),
        run_id: format!("run-{person}"),
        scope: TokenScope::Guest,
        session: format!("run:{room}"),
        project: None,
        reply: None,
    }
}

pub(super) fn window(hub: &Hub, seconds: u64) {
    hub.store.kv_set(WINDOW_KEY, &seconds.to_string()).unwrap();
}

pub(super) fn ls() -> Value {
    json!({"command": "ls /tmp"})
}

/// Starts `requestPermission` in the background, as the run socket would.
pub(super) fn ask(
    hub: &Arc<Hub>,
    who: RunIdentity,
    tool: &'static str,
    input: Value,
) -> tokio::task::JoinHandle<Decision> {
    let hub = hub.clone();
    tokio::spawn(async move { request_permission(&hub, &who, tool, &input).await })
}

/// The open card once the call has made it live and shown it.
pub(super) async fn shown(hub: &Hub) -> Card {
    for _ in 0..200 {
        if let Some(c) = hub.store.cards_open(None).into_iter().find(|c| c.state == CardState::Live) {
            return c;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("no live card appeared");
}

pub(super) fn entry_status(hub: &Hub, card: &Card) -> String {
    hub.store.entry(card.entry_id.as_ref().unwrap()).unwrap().data["status"].as_str().unwrap().to_owned()
}

pub(super) async fn joined(r: &Rig, preset: &str) -> Caller {
    let made = dispatch(&r.hub, &Caller::Local, "createInvite", json!({"name": "Sam", "agents": {"house": preset}}))
        .await
        .unwrap();
    let listed = [Listed {
        membership_id: "mem_sam".into(),
        user_id: "user_sam".into(),
        invite_id: made["inviteId"].as_str().unwrap().to_owned(),
        ..Default::default()
    }];
    members::apply(&r.hub, Some(&r.bin), &listed, Some("user_owner")).await.unwrap();
    r.device("sam-phone", Some("sam"))
}

#[tokio::test]
async fn an_owner_run_gets_a_card_and_a_yes_in_the_window_continues_the_same_run_once() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    window(&r.hub, 20);
    let call = ask(&r.hub, owner_run("s1"), "Bash", ls());
    let card = shown(&r.hub).await;
    assert_eq!((card.approver.as_str(), card.source.as_str(), card.person.as_str()), ("owner", "run", ""));
    let entry = r.hub.store.entry(card.entry_id.as_ref().unwrap()).unwrap();
    assert_eq!(
        (entry.kind.as_str(), entry.bot_id.as_str()),
        ("permission", chat.as_str()),
        "the owner's chat with the agent"
    );
    assert_eq!(entry.data["command"], "ls /tmp");
    assert!(roles::offered(&entry.data, "allow") && roles::offered(&entry.data, "deny"));

    assert_eq!(answer(&r.hub, &entry.id, Some("allow"), "owner").unwrap(), Outcome::Recorded { forward: false });
    assert_eq!(call.await.unwrap(), Decision { allow: true, message: "Approved.".into(), waiting: false });
    // C3: the live yes spent the grant; no resume, nothing left to use (A4).
    assert_eq!(r.hub.store.card(&card.id).unwrap().state, CardState::Used);
    assert_eq!(r.hub.store.cards_awaiting_resume(0).len(), 0);
    assert_eq!(entry_status(&r.hub, &card), "answered");
    // A second device, a retry: nothing (A1).
    assert_eq!(answer(&r.hub, &entry.id, Some("deny"), "owner").unwrap(), Outcome::Closed);
    assert_eq!(r.hub.store.card(&card.id).unwrap().state, CardState::Used);
}

#[tokio::test]
async fn no_answer_in_the_window_tells_the_run_to_end_its_turn_and_a_later_yes_is_kept_for_its_session() {
    let r = Rig::new().await;
    r.owner_bot("house");
    window(&r.hub, 1);
    let first = request_permission(&r.hub, &owner_run("s1"), "Bash", &ls()).await;
    assert!(!first.allow);
    assert!(first.message.contains("waiting on") && first.message.contains("End your turn"), "{}", first.message);
    // The window passing parks nothing: the process is still up (C6).
    let card = shown(&r.hub).await;
    assert_eq!(entry_status(&r.hub, &card), "pending");
    // The process exits: now it parks, and the entry stays answerable.
    run_exited(&r.hub, &card.run_id.clone().unwrap());
    assert_eq!(r.hub.store.card(&card.id).unwrap().state, CardState::Parked);
    let entry_id = card.entry_id.clone().unwrap();
    assert_eq!(r.hub.store.entry(&entry_id).unwrap().data["parked"], true);

    // The yes arrives after the process is gone: recorded as an approved card with its grant, not dropped.
    assert_eq!(answer(&r.hub, &entry_id, Some("allow"), "owner").unwrap(), Outcome::Recorded { forward: false });
    let kept = r.hub.store.card(&card.id).unwrap();
    assert!(kept.state == CardState::Approved && kept.granted_at.is_some() && kept.answered_parked);
    assert_eq!(entry_status(&r.hub, &card), "answered", "no longer shown as waiting");
    assert_eq!(r.hub.store.cards_awaiting_resume(0).len(), 1, "what the held resume will carry");

    // Another session of the same person can't use it (C8); its own next ask does, once (A4).
    window(&r.hub, 1);
    assert!(!request_permission(&r.hub, &owner_run("other"), "Bash", &ls()).await.allow);
    let again = request_permission(&r.hub, &owner_run("s1"), "Bash", &ls()).await;
    assert_eq!(again, Decision { allow: true, message: "Allowed once.".into(), waiting: false });
    assert!(!request_permission(&r.hub, &owner_run("s1"), "Bash", &ls()).await.allow, "the grant was spent");
}

#[tokio::test]
async fn a_repeat_joins_the_open_card_and_one_yes_is_one_use() {
    let r = Rig::new().await;
    r.owner_bot("house");
    window(&r.hub, 20);
    let (a, b) = (ask(&r.hub, owner_run("s1"), "Bash", ls()), ask(&r.hub, owner_run("s1"), "Bash", ls()));
    let card = shown(&r.hub).await;
    // Wait for the second call to have joined (both listening) before answering.
    for _ in 0..200 {
        if lock_live().get(&card.id).is_some_and(|l| l.len() == 2) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(r.hub.store.cards_open(None).len(), 1, "A6: one open card for the session and resource");
    let entries = r
        .hub
        .store
        .history(r.hub.store.entry(card.entry_id.as_ref().unwrap()).unwrap().bot_id.as_str(), i64::MAX, 50)
        .unwrap();
    assert_eq!(entries.iter().filter(|e| e.kind == "permission").count(), 1, "one entry too");
    answer(&r.hub, card.entry_id.as_ref().unwrap(), Some("allow"), "owner").unwrap();
    let (a, b) = (a.await.unwrap(), b.await.unwrap());
    assert_eq!([a.allow, b.allow].iter().filter(|x| **x).count(), 1, "a: {a:?} b: {b:?}");
}

#[tokio::test]
async fn a_no_and_an_expiry_and_a_withdrawal_never_give_access() {
    let r = Rig::new().await;
    r.owner_bot("house");
    window(&r.hub, 20);
    let call = ask(&r.hub, owner_run("s1"), "Bash", ls());
    let card = shown(&r.hub).await;
    let entry_id = card.entry_id.clone().unwrap();
    answer(&r.hub, &entry_id, Some("deny"), "owner").unwrap();
    let d = call.await.unwrap();
    assert!(!d.allow, "{d:?}");
    assert_eq!(r.hub.store.card(&card.id).unwrap().state, CardState::Denied);
    // Denied is closed: a later yes does nothing, and the next ask is a fresh card, not a grant.
    assert_eq!(answer(&r.hub, &entry_id, Some("allow"), "owner").unwrap(), Outcome::Closed);
    assert_eq!(r.hub.store.card(&card.id).unwrap().state, CardState::Denied);

    // Expiry: the sweep closes it with a deny for any waiter, and the entry shows it.
    let call = ask(&r.hub, owner_run("s1"), "Bash", json!({"command": "ls /var"}));
    let card = shown(&r.hub).await;
    let swept = r.hub.store.cards_sweep(i64::MAX).unwrap();
    assert!(swept.iter().any(|c| c.id == card.id));
    for c in &swept {
        deliver(&c.id, &Verdict::Deny("expired".into()));
        close_entry(&r.hub, c, "expired");
    }
    assert!(!call.await.unwrap().allow);
    assert_eq!(entry_status(&r.hub, &card), "expired");
    assert_eq!(answer(&r.hub, card.entry_id.as_ref().unwrap(), Some("allow"), "owner").unwrap(), Outcome::Closed);
    assert_eq!(r.hub.store.card(&card.id).unwrap().state, CardState::Expired, "expired never gives access");
}

#[tokio::test]
async fn secrets_and_logins_are_denied_with_no_card() {
    let r = Rig::new().await;
    r.owner_bot("house");
    let secret = resource::roots().kith[0].join("secrets/env");
    let d = request_permission(&r.hub, &owner_run("s1"), "Read", &json!({"file_path": secret})).await;
    assert!(!d.allow && d.message.contains("secret or a login"), "{d:?}");
    let d = request_permission(
        &r.hub,
        &owner_run("s1"),
        "Edit",
        &json!({"file_path": resource::roots().kith[0].join("auth.json")}),
    )
    .await;
    assert!(!d.allow);
    assert!(r.hub.store.cards_open(None).is_empty(), "A5: never carded");
    let d = request_permission(&r.hub, &owner_run("s1"), "Read", &json!({"file_path": "relative.txt"})).await;
    assert!(!d.allow && r.hub.store.cards_open(None).is_empty());
}

#[tokio::test]
async fn a_guests_card_goes_to_the_owner_a_members_to_themself_and_a_revoke_applies_at_answer_time() {
    let r = Rig::new().await;
    r.owner_bot("house");
    let sam = joined(&r, "member").await;
    let chat = dispatch(&r.hub, &sam, "startChat", json!({"agent": "house"})).await.unwrap()["bot"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    window(&r.hub, 20);

    // A member's own run: the card is theirs to answer, in their chat (a tool that doesn't run code: F11).
    let call = ask(&r.hub, guest_run("sam", "1:1:house:sam"), "WebFetch", json!({"url": "https://example.com"}));
    let card = shown(&r.hub).await;
    assert_eq!((card.approver.as_str(), card.person.as_str()), ("member", "sam"));
    let entry = r.hub.store.entry(card.entry_id.as_ref().unwrap()).unwrap();
    assert_eq!(entry.bot_id, chat);
    let who_may = |b: Value| {
        let hub = r.hub.clone();
        async move { roles::guard(&hub, "sam", true, "respondPermission", &b).await }
    };
    who_may(json!({"entryId": entry.id})).await.expect("a member answers their own run's card");
    // The run itself has no way to answer: the socket knows no such method (A2).
    let table = crate::runauth::TokenTable::new();
    assert!(
        crate::runauth::authorize(
            &table,
            br#"{"token":"x","method":"respondPermission"}"#,
            crate::runauth::PeerInfo::default()
        )
        .is_err()
    );

    // Sam is made a guest before anyone answers: the card is the owner's now, kith-auth said so at answer time (A7).
    r.auth(&["member", "house", "sam", "preset=guest"]);
    let e = who_may(json!({"entryId": entry.id})).await.unwrap_err();
    assert!(e.is::<crate::api::devices::NotAllowed>(), "{e:#}");
    // Person revoked: their open cards are withdrawn and the waiting run is told.
    withdraw_person(&r.hub, "sam", None);
    let d = call.await.unwrap();
    assert!(!d.allow && d.message.contains("withdrawn"), "{d:?}");
    assert_eq!(r.hub.store.card(&card.id).unwrap().state, CardState::Withdrawn);
    assert_eq!(answer(&r.hub, &entry.id, Some("allow"), "owner").unwrap(), Outcome::Closed, "answers on it do nothing");
    assert_eq!(entry_status(&r.hub, &card), "cancelled");

    // A guest's own run asks the owner.
    let call = ask(&r.hub, guest_run("sam", "1:1:house:sam"), "Bash", json!({"command": "ls /etc"}));
    let card = shown(&r.hub).await;
    assert_eq!(card.approver, "owner");
    let e = who_may(json!({"entryId": card.entry_id})).await.unwrap_err();
    assert!(e.is::<crate::api::devices::NotAllowed>(), "{e:#}");
    answer(&r.hub, card.entry_id.as_ref().unwrap(), Some("allow"), "owner").unwrap();
    assert!(call.await.unwrap().allow);
}

/// F11: an invited person's run executes as the owner's macOS user, so a card for a tool that runs code is the
/// owner's even in a member's own run, and the member's answer is refused by the approver stored on the card.
#[tokio::test]
async fn a_members_own_card_for_code_goes_to_the_owner() {
    let r = Rig::new().await;
    r.owner_bot("house");
    let sam = joined(&r, "member").await;
    dispatch(&r.hub, &sam, "startChat", json!({"agent": "house"})).await.unwrap();
    window(&r.hub, 20);
    let own = roles::grant_roots("sam", "house");
    assert_eq!(own.len(), 3, "memory, files and shared memory of house for sam");
    let mine = format!("{}/f11-x", own[1].display());
    let cases: [(&'static str, Value, &str); 9] = [
        ("Bash", ls(), "owner"),
        ("mcp__x__y", json!({"a": 1}), "owner"),
        ("BashOutput", json!({"bash_id": "1"}), "member"),
        ("KillShell", json!({"shell_id": "1"}), "member"),
        ("WebFetch", json!({"url": "https://example.com"}), "member"),
        ("Edit", json!({"file_path": mine, "old_string": "a", "new_string": "b"}), "member"),
        // Outside the run's grants (F11 step 2a): the owner's, though Edit is not code.
        ("Edit", json!({"file_path": "/tmp/f11-x", "old_string": "a", "new_string": "b"}), "owner"),
        ("Read", json!({"file_path": format!("{}/../../../other/x", own[1].display())}), "owner"),
        // A file tool that names no path searches wherever the run stands (P6).
        ("Grep", json!({"pattern": "x"}), "owner"),
    ];
    let inside = |tool: &str, input: Value| roles::within(&own, tool, &input);
    assert!(inside("Edit", json!({"file_path": mine})));
    assert!(!inside("Grep", json!({"pattern": "x"})), "no path named: the working directory, which may be anywhere");
    assert!(!inside("Glob", json!({"pattern": "*.md"})), "a relative pattern names no folder");
    assert!(inside("WebFetch", json!({"url": "https://example.com"})), "not a file tool");
    assert!(!inside("Edit", json!({"file_path": format!("{}/../x", own[1].display())})), "dotdot is applied");
    assert!(!inside("Read", json!({"file_path": "relative/x"})));
    assert!(!inside("Glob", json!({"pattern": "/etc/**/*.conf"})), "a pattern is bound by the folder it starts in");
    // An app chat's (ACP) call: the claudeCode tool name and rawInput when there is one, else the locations.
    let acp = |name: &str, kind: &str, call: Value| roles::acp_within(&own, name, kind, &call);
    assert!(acp("Edit", "edit", json!({"rawInput": {"file_path": mine}})));
    assert!(!acp("Edit", "edit", json!({"rawInput": {"file_path": "/tmp/f11-x"}, "locations": [{"path": mine}]})));
    assert!(!acp("Grep", "search", json!({"rawInput": {"pattern": "x"}})), "P6 for app chats too");
    assert!(acp("", "read", json!({"locations": [{"path": mine}]})));
    assert!(!acp("", "read", json!({"locations": [{"path": mine}, {"path": "/etc/hosts"}]})), "every location");
    assert!(!acp("", "edit", json!({"rawInput": {"file_path": mine}})), "no tool name and no locations: outside");
    assert!(acp("", "fetch", json!({})), "a kind that touches no file");
    for (tool, input, approver) in cases {
        let call = ask(&r.hub, guest_run("sam", "1:1:house:sam"), tool, input);
        let card = shown(&r.hub).await;
        assert_eq!(card.approver, approver, "{tool}");
        let b = json!({"entryId": card.entry_id});
        let said = roles::guard(&r.hub, "sam", true, "respondPermission", &b).await;
        if approver == "owner" {
            assert!(said.unwrap_err().is::<crate::api::devices::NotAllowed>(), "{tool}: the member is refused");
        } else {
            said.unwrap_or_else(|e| panic!("{tool}: the member may answer: {e:#}"));
        }
        answer(&r.hub, card.entry_id.as_ref().unwrap(), Some("allow"), "owner").unwrap();
        assert!(call.await.unwrap().allow, "{tool}");
    }
    // The predicate itself: code is the owner's whoever the member is.
    assert!(!roles::approves_own("sam", "house", "Bash", "execute").await);
    assert!(!roles::approves_own("sam", "house", "Run", "execute").await, "a shell tool of another harness");
    assert!(!roles::approves_own("sam", "house", "mcp__x__y", "other").await);
    assert!(roles::approves_own("sam", "house", "BashOutput", "execute").await);
    assert!(roles::approves_own("sam", "house", "Edit", "edit").await);
    // An ACP request with no toolName: only a kind that clearly runs no code stays the member's.
    for kind in ["read", "edit", "search", "fetch", "think"] {
        assert!(roles::approves_own("sam", "house", "", kind).await, "{kind}");
    }
    for kind in ["execute", "other", "delete", "move", "switch_mode", "", "made-up"] {
        assert!(!roles::approves_own("sam", "house", "", kind).await, "{kind} with no tool name");
    }
    let call = ask(&r.hub, owner_run("s1"), "Bash", ls());
    assert_eq!(shown(&r.hub).await.approver, "owner");
    call.abort();
}

/// P5: a member answers their bot's run cards under the member rule, with the bot's own folders as the bounds: a file
/// outside them stays the owner's.
#[tokio::test]
async fn a_bots_owner_answers_its_run_cards_only_inside_the_bots_folders() {
    let r = Rig::new().await;
    r.owner_bot("house");
    joined(&r, "member").await;
    window(&r.hub, 20);
    let bot = "bot-0000aaaa";
    r.hub
        .store
        .put_bot_guest(&crate::store::BotGuest {
            bot_id: "bg_rex".into(),
            person_id: bot.into(),
            owner_person: Some("sam".into()),
            name: "Rex".into(),
            provider: "grok".into(),
            agents: vec!["house".into()],
            state: crate::store::BotState::Active,
            device_key: None,
            cloud_device: None,
            rate_per_hour: 0,
            concurrent: 0,
            created_at: 0,
            joined_at: None,
            decided_at: None,
            decided_by: None,
            last_seen_at: None,
            revoked_at: None,
            revoked_why: None,
        })
        .unwrap();
    let roots = roles::grant_roots(bot, "house");
    let inside = format!("{}/p5-x", roots.last().expect("house's shared memory").display());
    let cases = [
        (json!({"file_path": inside, "old_string": "a", "new_string": "b"}), true),
        (json!({"file_path": "/tmp/p5-x", "old_string": "a", "new_string": "b"}), false),
    ];
    for (input, may) in cases {
        let call = ask(&r.hub, guest_run(bot, "1:1:house:bot"), "Edit", input);
        let card = shown(&r.hub).await;
        assert_eq!(card.approver, "owner", "a bot's own run never answers itself");
        let said = roles::guard(&r.hub, "sam", true, "respondPermission", &json!({"entryId": card.entry_id})).await;
        if may {
            said.unwrap_or_else(|e| panic!("inside the bot's folders: {e:#}"));
        } else {
            assert!(said.unwrap_err().is::<crate::api::devices::NotAllowed>(), "outside the bot's folders");
        }
        answer(&r.hub, card.entry_id.as_ref().unwrap(), Some("allow"), "owner").unwrap();
        assert!(call.await.unwrap().allow);
    }
}

#[tokio::test]
async fn a_restart_parks_live_cards_and_keeps_their_entries_answerable() {
    let r = Rig::new().await;
    r.owner_bot("house");
    window(&r.hub, 1);
    let _ = request_permission(&r.hub, &owner_run("s1"), "Bash", &ls()).await;
    let card = shown(&r.hub).await;
    let entry_id = card.entry_id.clone().unwrap();
    // The host restarts: waits are lost, cards persist.
    lock_live().clear();
    r.hub.store.cards_restart(now_ms()).unwrap();
    assert_eq!(r.hub.store.card(&card.id).unwrap().state, CardState::Parked);
    r.hub.store.expire_pending().unwrap();
    assert_eq!(entry_status(&r.hub, &card), "pending", "a card the store holds open is not expired at startup");
    answer(&r.hub, &entry_id, Some("allow"), "owner").unwrap();
    assert_eq!(r.hub.store.card(&card.id).unwrap().state, CardState::Approved);
    // An old entry from before the store has no card: closed, no crash.
    let old = r
        .hub
        .add_entry(
            &Lane::main(&r.owner_bot("house")),
            EntryKind::Permission,
            1,
            &json!({"status": "pending", "options": [{"optionId": "allow", "kind": "allow_once"}]}),
        )
        .unwrap();
    assert_eq!(answer(&r.hub, &old.id, Some("allow"), "owner").unwrap(), Outcome::Closed);
    assert_eq!(answer(&r.hub, "no-such-entry", Some("allow"), "owner").unwrap(), Outcome::Closed);
}

#[tokio::test]
async fn a_bot_chat_card_answered_after_its_process_is_gone_is_recorded() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    let lane = Lane::main(&chat);
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
        session: format!("acp:{chat}:"),
        source: "acp",
        bot_id: Some(chat.clone()),
        run_id: None,
        person: String::new(),
        agent: "house".into(),
        approver: "owner",
        tool: "Bash".into(),
        input: "{}".into(),
        resource: "Bash:x".into(),
        shown: "x".into(),
        deadline: now_ms() + 60_000,
        reply_to: None,
        asked_by: None,
    };
    let Opened::New(card) = r.hub.store.card_open(&new, now_ms()).unwrap() else { panic!() };
    r.hub.store.card_live(&card.id, Some(&entry.id), None).unwrap();
    // Process up: the answer is for the actor to deliver.
    // Process gone first: parked, then the answer is recorded and shown.
    bot_exited(&r.hub, &chat);
    assert_eq!(r.hub.store.card(&card.id).unwrap().state, CardState::Parked);
    assert_eq!(answer(&r.hub, &entry.id, Some("allow"), "owner").unwrap(), Outcome::Recorded { forward: false });
    assert_eq!(r.hub.store.entry(&entry.id).unwrap().data["status"], "answered");
    assert_eq!(r.hub.store.card(&card.id).unwrap().state, CardState::Approved);
}

#[tokio::test]
async fn settings_default_to_a_minute_and_a_half_and_a_day_and_the_resume_is_on_with_a_kill_switch() {
    let r = Rig::new().await;
    r.hub.store.kv_set(RESUME_KEY, "").unwrap(); // the rig switches it off; empty is "not set"
    let s = settings(&r.hub.store);
    assert_eq!((s.window, s.expiry_ms, s.resume), (Duration::from_secs(90), 24 * 3_600_000, true));
    r.hub.store.kv_set(WINDOW_KEY, "30").unwrap();
    r.hub.store.kv_set(EXPIRY_KEY, "48").unwrap();
    r.hub.store.kv_set(RESUME_KEY, "off").unwrap();
    let s = settings(&r.hub.store);
    assert_eq!((s.window, s.expiry_ms, s.resume), (Duration::from_secs(30), 48 * 3_600_000, false));
}

#[tokio::test]
async fn a_yes_after_the_window_but_before_the_process_exits_is_not_lost() {
    let r = Rig::new().await;
    r.owner_bot("house");
    window(&r.hub, 1);
    assert!(!request_permission(&r.hub, &owner_run("s1"), "Bash", &ls()).await.allow);
    // The run's wait ended but its process is still up: nobody is listening for the yes.
    let card = shown(&r.hub).await;
    assert_eq!(
        answer(&r.hub, card.entry_id.as_ref().unwrap(), Some("allow"), "owner").unwrap(),
        Outcome::Recorded { forward: false }
    );
    let kept = r.hub.store.card(&card.id).unwrap();
    assert!(
        kept.state == CardState::Approved && kept.answered_parked,
        "recorded as an answer to a parked card: {kept:?}"
    );
    assert_eq!(r.hub.store.cards_awaiting_resume(0).len(), 1);
    // And the session's next ask uses it, once.
    assert!(request_permission(&r.hub, &owner_run("s1"), "Bash", &ls()).await.allow);
}

#[tokio::test]
async fn a_parallel_ask_cannot_take_the_grant_a_live_asker_is_given() {
    let r = Rig::new().await;
    r.owner_bot("house");
    window(&r.hub, 20);
    let call = ask(&r.hub, owner_run("s1"), "Bash", ls());
    let card = shown(&r.hub).await;
    answer(&r.hub, card.entry_id.as_ref().unwrap(), Some("allow"), "owner").unwrap();
    // The yes is spent at the answer: a parallel ask of the same session opens a card of its own, not a grant.
    assert!(matches!(r.hub.store.card(&card.id).unwrap().state, CardState::Used));
    let again = NewCard {
        session: card.session.clone(),
        source: "run",
        bot_id: None,
        run_id: None,
        person: String::new(),
        agent: "house".into(),
        approver: "owner",
        tool: card.tool.clone(),
        input: card.input.clone(),
        resource: card.resource.clone(),
        shown: card.shown.clone(),
        deadline: now_ms() + 60_000,
        reply_to: None,
        asked_by: None,
    };
    assert!(matches!(r.hub.store.card_open(&again, now_ms()).unwrap(), Opened::New(_)));
    assert!(call.await.unwrap().allow);
}

#[tokio::test]
async fn a_turn_that_ends_with_a_question_open_parks_it() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    let entry = r
        .hub
        .add_entry(
            &Lane::main(&chat),
            EntryKind::Permission,
            1,
            &json!({"status": "pending", "options": [{"optionId": "allow", "kind": "allow_once"}]}),
        )
        .unwrap();
    assert!(!park_entry(&r.hub, &entry.id), "an old entry has no card");
    let new = NewCard {
        session: "acp:x:".into(),
        source: "acp",
        bot_id: Some(chat.clone()),
        run_id: None,
        person: String::new(),
        agent: "house".into(),
        approver: "owner",
        tool: "Bash".into(),
        input: "{}".into(),
        resource: "Bash:y".into(),
        shown: "y".into(),
        deadline: now_ms() + 60_000,
        reply_to: None,
        asked_by: None,
    };
    let Opened::New(card) = r.hub.store.card_open(&new, now_ms()).unwrap() else { panic!() };
    r.hub.store.card_live(&card.id, Some(&entry.id), None).unwrap();
    assert!(park_entry(&r.hub, &entry.id));
    assert_eq!(r.hub.store.card(&card.id).unwrap().state, CardState::Parked);
    assert_eq!(r.hub.store.entry(&entry.id).unwrap().data["status"], "pending", "still answerable");
}
