//! The bot's side of the host RPC contract: the host's real answers to a bot (serve/contract/host/fixtures/, written by
//! the host's `api::contract_tests`) and a newer host's (`unknown/`) read through the guest's own functions, so a change
//! to what the host sends a bot is seen here (W2), and a word this version doesn't know is handled as the contract says (W3).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use kith_guest::config::State;
use kith_guest::exit::Exit;
use kith_guest::session::answer;
use kith_guest::thread::{Reply, parse_thread};
use kith_wire::rpc::{BotAsked, BotConversations, BotEntryKind, BotProof, BotSelf, ConversationState};
use serde_json::{Value, json};
use std::path::PathBuf;

/// The body of one fixture, after checking it is the answer to `method`.
fn body(name: &str, method: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../contract/host/fixtures").join(name);
    let f: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(f["method"], method, "{name}");
    f["body"].clone()
}

#[test]
fn the_hosts_answers_to_a_bot_read_as_the_bot_reads_them() {
    let me: BotSelf = answer(body("botStatus.json", "botStatus")).unwrap();
    assert_eq!(State::of(me.state), Some(State::Active));
    assert!(!me.agents.is_empty() && !me.owner.name.is_empty());

    let proof: BotProof = answer(body("proveBotInvite.json", "proveBotInvite")).unwrap();
    assert!(State::of(proof.state).is_some() && proof.bot_id.is_some());

    let asked: BotAsked = answer(body("botAsk.json", "botAsk")).unwrap();
    assert!(!asked.conversation.is_empty() && asked.turn > 0);

    let mine: BotConversations = answer(body("botConversations.json", "botConversations")).unwrap();
    assert_eq!(mine.conversations[0].r#ref.as_deref(), Some("grok:8841"));

    // The thread drives a reply: the agent's final words, the wait for the owner, and the notice that ends it badly.
    let thread = parse_thread(&body("botThread.json", "botThread")).unwrap();
    let turn = thread.turn.unwrap();
    let mut reply = Reply::new(turn, 0);
    for e in &thread.entries {
        reply.apply(e);
    }
    assert!(reply.has_final());
    assert_eq!(reply.text(), "Milk and eggs.");
    assert!(reply.failed.is_some());
    assert!(thread.entries.iter().any(|e| e.kind == BotEntryKind::User && e.sender.is_some()));
}

#[test]
fn a_newer_hosts_words_to_a_bot_are_handled_as_the_contract_says() {
    let thread = parse_thread(&body("unknown/bot-thread-new-words.json", "botThread")).unwrap();
    assert_eq!(thread.state, ConversationState::Running, "a state this version doesn't know is a turn still running");
    assert_eq!(thread.entries[0].kind, BotEntryKind::Unknown);
    let mut reply = Reply::new(thread.turn.unwrap(), 0);
    assert!(!reply.apply(&thread.entries[0]), "a kind this version doesn't know is not part of the reply");
    reply.apply(&thread.entries[1]);
    assert_eq!(reply.text(), "Milk and eggs.");

    let me: BotSelf = answer(body("unknown/bot-status-new-words.json", "botStatus")).unwrap();
    assert_eq!(State::of(me.state), None, "a state this version doesn't know is not saved as one it does");

    let mine: BotConversations = answer(body("unknown/bot-conversations-new-state.json", "botConversations")).unwrap();
    assert_eq!(mine.conversations[0].state, ConversationState::Running);
}

#[test]
fn an_answer_the_bot_cant_read_is_a_version_mismatch_not_an_empty_one() {
    assert_eq!(answer::<BotSelf>(json!({"botId": "bg_1"})).unwrap_err().exit, Exit::Version);
    assert_eq!(parse_thread(&json!({"entries": "no"})).unwrap_err().exit, Exit::Version);
}
