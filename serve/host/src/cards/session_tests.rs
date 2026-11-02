//! "This session" on the owner's auto-mode card: a repeat of the same action in the same conversation runs with no card;
//! a new task, 24 h, a settings change or the owner's Remove ends it; must-ask actions and calls carrying a key are never
//! offered it; and `kith cards history` (an agent with `cards: read` only).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::tests::{guest_run, ls, owner_run, shown, window};
use super::*;
use crate::api::devices::Caller;
use crate::api::dispatch;
use crate::api::rig::Rig;

const REASON: &str = "[Data Exfiltration]";

fn block(hub: &Arc<Hub>, who: &RunIdentity, claude: &'static str, input: Value) -> tokio::task::JoinHandle<Decision> {
    let (hub, who) = (hub.clone(), who.clone());
    tokio::spawn(async move {
        request_auto_denied(&hub, &who, "Bash", &input, AutoDenied { reason: REASON, claude_session: claude }).await
    })
}

fn data(r: &Rig, card: &Card) -> Value {
    r.hub.store.entry(card.entry_id.as_ref().unwrap()).unwrap().data
}

fn ids(d: &Value) -> Vec<String> {
    d["options"].as_array().unwrap().iter().map(|o| o["optionId"].as_str().unwrap().to_owned()).collect()
}

fn agent_file(r: &Rig, name: &str) -> std::path::PathBuf {
    r.dir.join("home").join("agents").join("house").join(name)
}

/// Opens a card for `ls /tmp` in `who`'s conversation and taps This session; returns the card.
async fn grant_ls(r: &Rig, who: &RunIdentity, claude: &'static str) -> Card {
    let call = block(&r.hub, who, claude, ls());
    let card = shown(&r.hub).await;
    assert_eq!(ids(&data(r, &card)), ["allow", session::SESSION, autoallow::ALWAYS, "deny"]);
    answer(&r.hub, card.entry_id.as_ref().unwrap(), Some(session::SESSION), None).unwrap();
    assert!(call.await.unwrap().allow, "the tap lets the blocked call retry");
    card
}

/// The decision of a block that must not open a card: it answers at once.
async fn no_card(r: &Rig, who: &RunIdentity, claude: &'static str, input: Value) -> Decision {
    let d = tokio::time::timeout(Duration::from_secs(5), block(&r.hub, who, claude, input)).await.unwrap().unwrap();
    assert!(r.hub.store.cards_open(None).is_empty(), "no card was opened");
    d
}

async fn deny_open(r: &Rig) {
    let card = shown(&r.hub).await;
    answer(&r.hub, card.entry_id.as_ref().unwrap(), Some("deny"), None).unwrap();
}

#[tokio::test]
async fn this_session_lets_a_repeat_of_that_action_run_with_no_card_and_nothing_else() {
    let r = Rig::new().await;
    window(&r.hub, 20);
    let who = owner_run("chat-1");
    let card = grant_ls(&r, &who, "c1").await;
    let d = data(&r, &card);
    assert_eq!(d["sessionGrant"]["state"], "live");
    assert_eq!(d["outcome"], "Allowed this session");

    let again = no_card(&r, &who, "c1", ls()).await;
    assert!(again.allow, "{}", again.message);
    // The description is the model's own words: the same command with another one is the same action.
    assert!(no_card(&r, &who, "c1", json!({"command": "ls /tmp", "description": "again"})).await.allow);

    // Another command is another action: a card.
    let other = block(&r.hub, &who, "c1", json!({"command": "ls /etc"}));
    deny_open(&r).await;
    assert!(!other.await.unwrap().allow);
    // Another conversation of the same agent has no grant.
    let elsewhere = block(&r.hub, &owner_run("chat-2"), "c1", ls());
    deny_open(&r).await;
    assert!(!elsewhere.await.unwrap().allow);
    // Nothing was remembered for always.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!agent_file(&r, "allowed.json").exists());
}

#[tokio::test]
async fn a_new_task_ends_the_grant_and_the_card_says_so() {
    let r = Rig::new().await;
    window(&r.hub, 20);
    let who = owner_run("chat-3");
    let card = grant_ls(&r, &who, "c1").await;
    // A new Claude session in the same conversation (a new task, a clear): asked again.
    let next = block(&r.hub, &who, "c2", ls());
    deny_open(&r).await;
    assert!(!next.await.unwrap().allow);
    let d = data(&r, &card);
    assert_eq!(d["sessionGrant"]["state"], "ended");
    assert_eq!(d["sessionGrant"]["endReason"], "task");
    assert_eq!(d["outcome"], "Allowed this session · ended with the task");
    // And the old session's grant doesn't come back.
    let back = block(&r.hub, &who, "c1", ls());
    deny_open(&r).await;
    assert!(!back.await.unwrap().allow);
}

#[tokio::test]
async fn after_24_hours_the_grant_ends_and_the_next_block_asks() {
    let r = Rig::new().await;
    window(&r.hub, 20);
    let who = owner_run("chat-4");
    let card = grant_ls(&r, &who, "c1").await;
    session::sweep(&r.hub, now_ms() + session::LIFETIME_MS - 60_000);
    assert_eq!(data(&r, &card)["sessionGrant"]["state"], "live", "not before its time");
    session::sweep(&r.hub, now_ms() + session::LIFETIME_MS + 1);
    let d = data(&r, &card);
    assert_eq!(
        (d["sessionGrant"]["state"].as_str(), d["sessionGrant"]["endReason"].as_str()),
        (Some("ended"), Some("expired"))
    );
    assert_eq!(d["outcome"], "Allowed this session · ended after 24 h");
    let next = block(&r.hub, &who, "c1", ls());
    deny_open(&r).await;
    assert!(!next.await.unwrap().allow);
}

#[tokio::test]
async fn a_change_to_the_agents_permission_settings_ends_the_grant() {
    let r = Rig::new().await;
    window(&r.hub, 20);
    let who = owner_run("chat-5");
    let card = grant_ls(&r, &who, "c1").await;
    std::fs::write(agent_file(&r, "settings.json"), r#"{"permissions": {"allow": []}}"#).unwrap();
    let next = block(&r.hub, &who, "c1", ls());
    deny_open(&r).await;
    assert!(!next.await.unwrap().allow);
    assert_eq!(data(&r, &card)["sessionGrant"]["endReason"], "settings");
}

#[tokio::test]
async fn the_owner_removes_a_grant_from_the_card_or_a_row_and_a_second_remove_is_not_found() {
    let r = Rig::new().await;
    window(&r.hub, 20);
    let who = owner_run("chat-6");
    let card = grant_ls(&r, &who, "c1").await;
    assert!(no_card(&r, &who, "c1", ls()).await.allow);
    // The repeat's tool entry, as the app chat adds it, gets the tag.
    let mut row = json!({"title": "ls /tmp", "status": "pending"});
    let g = session::tag_tool(&r.hub, "house", "run:o:chat-6", &ls(), &mut row).expect("the call the grant let through is tagged");
    assert_eq!(row["sessionGrant"]["state"], "live");
    let lane = Lane::main("bot-house");
    let entry = r.hub.add_entry(&lane, EntryKind::Tool, 1, &row).unwrap();
    session::tagged(&r.hub, &g, &entry.id);
    // Two calls went through (the retry after the tap, and the repeat): two rows, and no third.
    assert_eq!(session::tag_tool(&r.hub, "house", "run:o:chat-6", &ls(), &mut json!({})).as_deref(), Some(g.as_str()));
    assert!(session::tag_tool(&r.hub, "house", "run:o:chat-6", &ls(), &mut json!({})).is_none(), "one row per call");

    let id = data(&r, &card)["sessionGrant"]["id"].as_str().unwrap().to_owned();
    // Not a member's to remove.
    r.auth(&["person", "sam"]);
    let member = r.device("sam-phone", Some("sam"));
    assert!(dispatch(&r.hub, &member, "removeSessionGrant", json!({"id": id})).await.is_err());
    let gone = dispatch(&r.hub, &Caller::Local, "removeSessionGrant", json!({"id": id})).await.unwrap();
    assert_eq!(gone["ok"], true);
    let d = data(&r, &card);
    assert_eq!(d["sessionGrant"]["state"], "removed");
    assert!(d["sessionGrant"]["endedAt"].is_i64());
    assert_eq!(r.hub.store.entry(&entry.id).unwrap().data["sessionGrant"]["state"], "removed");
    let again = dispatch(&r.hub, &Caller::Local, "removeSessionGrant", json!({"id": id})).await;
    assert!(format!("{:#}", again.unwrap_err()).contains("Not found"));
    for bad in ["", "../x", "a b"] {
        assert!(dispatch(&r.hub, &Caller::Local, "removeSessionGrant", json!({"id": bad})).await.is_err());
    }
    let next = block(&r.hub, &who, "c1", ls());
    deny_open(&r).await;
    assert!(!next.await.unwrap().allow, "removed: the next one asks again");
}

#[tokio::test]
async fn a_must_ask_action_is_offered_neither_this_session_nor_always() {
    let r = Rig::new().await;
    window(&r.hub, 20);
    let call = block(&r.hub, &owner_run("chat-7"), "c1", json!({"command": "rm -rf build"}));
    let card = shown(&r.hub).await;
    assert_eq!(ids(&data(&r, &card)), ["allow", "deny"]);
    answer(&r.hub, card.entry_id.as_ref().unwrap(), Some("deny"), None).unwrap();
    assert!(!call.await.unwrap().allow);
    // An answer naming the option anyway grants nothing.
    let who = owner_run("chat-7b");
    let call = block(&r.hub, &who, "c1", json!({"command": "rm -rf build"}));
    let card = shown(&r.hub).await;
    answer(&r.hub, card.entry_id.as_ref().unwrap(), Some(session::SESSION), None).unwrap();
    assert!(!call.await.unwrap().allow, "not an option of this card");
    assert_eq!(r.hub.store.session_grants_live("house", &who.session), Vec::new());
}

#[tokio::test]
async fn key_shaped_text_is_redacted_before_the_card_is_shown_or_stored() {
    let r = Rig::new().await;
    window(&r.hub, 20);
    let key = "ghp_abcdefghijklmnopqrstuvwxyz0123";
    let call = block(
        &r.hub,
        &owner_run("chat-8"),
        "c1",
        json!({"command": format!("curl -H 'Authorization: token {key}' https://api.github.com/user")}),
    );
    let card = shown(&r.hub).await;
    let d = data(&r, &card);
    assert!(!d.to_string().contains(key), "the entry shows no key: {d}");
    assert!(!card.input.contains(key) && !card.shown.contains(key) && !card.resource.contains(key), "{card:?}");
    assert!(d["command"].as_str().unwrap().contains("[redacted]"), "{d}");
    assert_eq!(ids(&d), ["allow", "deny"], "a call carrying a key is never remembered, not even for the session");
    answer(&r.hub, card.entry_id.as_ref().unwrap(), Some("deny"), None).unwrap();
    assert!(!call.await.unwrap().allow);
}

// ---- kith cards history

#[tokio::test]
async fn card_history_answers_an_agent_with_cards_read_and_refuses_one_without() {
    let r = Rig::new().await;
    window(&r.hub, 20);
    let who = owner_run("chat-9");
    grant_ls(&r, &who, "c1").await;
    let refused = history::list(&r.hub, &who, 10).await;
    assert_eq!(refused["ok"], false);
    assert!(refused["error"].as_str().unwrap().contains("cards: read"), "{refused}");
    assert!(refused.get("cards").is_none());
    std::fs::write(agent_file(&r, "capabilities.yaml"), "needs: []\ncards: read\n").unwrap();
    let ok = history::list(&r.hub, &who, 10).await;
    let rows = ok["cards"].as_array().unwrap_or_else(|| panic!("{ok}"));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["agent"], "house");
    assert_eq!(rows[0]["kind"], "execute");
    assert_eq!(rows[0]["button"], "This session");
    assert_eq!(rows[0]["auto"], true);
    assert!(!ok.to_string().contains("ls /tmp"), "no command in the history: {ok}");
}

#[tokio::test]
async fn another_thread_of_the_agent_gets_no_this_session_tag_on_the_same_command() {
    let r = Rig::new().await;
    window(&r.hub, 20);
    let who = owner_run("chat-7");
    grant_ls(&r, &who, "c1").await;
    let mut row = json!({"title": "ls /tmp"});
    assert!(session::tag_tool(&r.hub, "house", "run:o:chat-8", &ls(), &mut row).is_none());
    assert!(row.get("sessionGrant").is_none());
    assert!(session::tag_tool(&r.hub, "house", "run:o:chat-7", &ls(), &mut row).is_some());
}

#[tokio::test]
async fn a_hook_without_a_claude_session_is_not_offered_this_session_and_cannot_grant_it() {
    let r = Rig::new().await;
    window(&r.hub, 20);
    let who = owner_run("chat-11");
    let call = block(&r.hub, &who, "", ls());
    let card = shown(&r.hub).await;
    assert_eq!(ids(&data(&r, &card)), ["allow", autoallow::ALWAYS, "deny"]);
    // A tap on the option anyway (an old app, a forged answer) keeps nothing.
    answer(&r.hub, card.entry_id.as_ref().unwrap(), Some(session::SESSION), None).unwrap();
    assert!(!call.await.unwrap().allow, "an option the card never offered is no yes");
    assert!(r.hub.store.session_grants_live("house", "run:o:chat-11").is_empty());
}

#[tokio::test]
async fn the_card_history_lists_the_owners_cards_only() {
    let r = Rig::new().await;
    window(&r.hub, 20);
    grant_ls(&r, &owner_run("chat-12"), "c1").await;
    let call = super::tests::ask(
        &r.hub,
        guest_run("sam", "1:1:house:sam"),
        "SandboxNetworkAccess",
        json!({"host": "github.com"}),
    );
    let guest = shown(&r.hub).await;
    assert_eq!(guest.person, "sam");
    let seen = r.hub.store.cards_recent(10);
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(seen[0].person.is_empty() && seen[0].approver == "owner");
    call.abort();
}
