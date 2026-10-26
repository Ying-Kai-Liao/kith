//! The host RPC contract (serve/contract/host/contract.json) as Rust reads it: `src/rpc.rs` is made from the contract
//! as it is now, the host's real answers (fixtures/, written by the host's `api::contract_tests`) and older hosts'
//! (compat/) decode, fields this version doesn't know are ignored, and a word it doesn't know decodes as its enum
//! declares (W3).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use kith_wire::check::Mode;
use kith_wire::rpc::{
    self, AgentRole, BotAnswer, BotConversations, BotEntryKind, BotErrorCode, BotGuestState, BotGuests, BotKind,
    BotProvider, BotSelf, BotStatus, BotThread, Citation, CitationError, ConversationState, Device, Entries, EntryKind,
    Hello, Permission, ProjectsBy, SCHEMA, ThreadProjects,
};
use serde_json::Value;
use std::fmt::Write as _;
use std::path::PathBuf;

const CONTRACT: &[u8] = include_bytes!("../../../../contract/host/contract.json");

/// Each fixture in `dir`: its path, method, type and body.
fn fixtures(dir: &str) -> Vec<(String, String, String, Value)> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../contract/host/fixtures").join(dir);
    let mut out: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .map(|p| {
            let f: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
            let s = |k: &str| f[k].as_str().unwrap().to_owned();
            (p.display().to_string(), s("method"), s("type"), f["body"].clone())
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn fixture<T: serde::de::DeserializeOwned>(dir: &str, name: &str) -> T {
    let (_, _, _, body) = fixtures(dir).into_iter().find(|(p, ..)| p.ends_with(name)).unwrap();
    serde_json::from_value(body).unwrap()
}

#[test]
fn rpc_rs_is_made_from_the_contract_as_it_is_now() {
    let hex = kith_wire::crypto::sha256(&[CONTRACT]).iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    });
    assert_eq!(hex, rpc::CONTRACT_SHA256, "the contract changed: run `node serve/contract/gen.mjs`");
}

#[test]
fn every_answer_decodes_today_from_an_older_host_and_from_a_newer_one() {
    for dir in ["", "compat", "unknown"] {
        let all = fixtures(dir);
        assert!(!all.is_empty(), "{dir}");
        for (p, method, ty, body) in all {
            let m = SCHEMA.method(&method).unwrap_or_else(|| panic!("{p}: no method {method}"));
            assert_eq!(m.result, ty, "{p}");
            let decoded = rpc::decode(&ty, body.clone()).unwrap_or_else(|| panic!("{p}: no contract type {ty}"));
            assert!(decoded.is_ok(), "{p}: {decoded:?}");
            assert_eq!(SCHEMA.check(&ty, &body, Mode::Consumer), Vec::<String>::new(), "{p}");
        }
    }
}

#[test]
fn the_hosts_own_answers_are_what_a_producer_may_send() {
    for (p, _, ty, body) in fixtures("") {
        assert_eq!(SCHEMA.check(&ty, &body, Mode::Producer), Vec::<String>::new(), "{p}");
    }
    // A newer host's words are not this version's to send.
    for (p, _, ty, body) in fixtures("unknown") {
        assert!(!SCHEMA.check(&ty, &body, Mode::Producer).is_empty(), "{p}");
    }
}

#[test]
fn a_newer_hosts_words_read_as_their_enum_declares() {
    let started: BotAnswer = fixture("unknown", "start-chat-new-words.json");
    assert_eq!(
        (started.bot.kind, started.bot.status, started.bot.permission),
        (BotKind::Agent, BotStatus::Idle, Permission::Unknown)
    );
    let history: Entries = fixture("unknown", "history-new-kind.json");
    assert_eq!(history.entries[0].kind, EntryKind::Unknown);
    let hello: Hello = fixture("unknown", "hello-new-words.json");
    assert_eq!(hello.device, Some(Device::Desktop));
    assert_eq!(
        hello.granted_agents.unwrap()[0].role,
        AgentRole::Guest,
        "a role this version doesn't know is the least"
    );
    let bots: BotGuests = fixture("unknown", "my-bots-new-words.json");
    assert_eq!((bots.bots[0].provider, bots.bots[0].state), (BotProvider::Other, BotGuestState::Unknown));
    let projects: ThreadProjects = fixture("unknown", "thread-projects-new-by.json");
    assert_eq!(projects.by, ProjectsBy::Inferred);
    let citation: Citation = fixture("unknown", "citation-new-error.json");
    assert_eq!(citation.error, Some(CitationError::NotShared), "an error this version doesn't know shows nothing");
}

#[test]
fn a_newer_hosts_words_to_a_bot_read_as_their_enum_declares() {
    let thread: BotThread = fixture("unknown", "bot-thread-new-words.json");
    assert_eq!(thread.entries[0].kind, BotEntryKind::Unknown, "a bot skips a kind it doesn't know");
    assert_eq!(thread.state, ConversationState::Running, "a state this version doesn't know is a turn still running");
    let me: BotSelf = fixture("unknown", "bot-status-new-words.json");
    assert_eq!((me.state, me.agents[0].role), (BotGuestState::Unknown, AgentRole::Guest));
    let conversations: BotConversations = fixture("unknown", "bot-conversations-new-state.json");
    assert_eq!(conversations.conversations[0].state, ConversationState::Running);
    let code: BotErrorCode = serde_json::from_value(serde_json::json!("overQuota")).unwrap();
    assert_eq!(code, BotErrorCode::Unknown, "a bot acts on the status of an error it doesn't know");
}

#[test]
fn an_older_hosts_answers_leave_out_what_it_didnt_have() {
    let hello: Hello = fixture("compat", "hello-before-granted-agents.json");
    assert_eq!(hello.granted_agents, None);
    let started: BotAnswer = fixture("compat", "start-chat-before-people.json");
    assert_eq!((started.bot.person, started.bot.hq_agent), (None, None));
}

#[test]
fn the_member_and_bot_lists_name_contract_methods() {
    let lists = [rpc::MEMBER_METHODS, rpc::UNPROVEN_METHODS, rpc::BOT_METHODS, rpc::BOT_PENDING_METHODS];
    for m in lists.into_iter().flatten() {
        assert!(SCHEMA.method(m).is_some(), "{m}");
    }
    assert!(rpc::MEMBER_METHODS.contains(&rpc::method::SEND));
    assert_eq!(rpc::UNPROVEN_METHODS, [rpc::method::PROVE_INVITE]);
    assert!(!rpc::BOT_METHODS.contains(&rpc::method::SEND), "a bot asks; it never sends as a person");
    for m in rpc::BOT_PENDING_METHODS {
        assert!(rpc::BOT_METHODS.contains(m), "{m}: a pending bot may call only what an allowed one may");
    }
}
