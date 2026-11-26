//! What a wake turn runs, by session (`way_of`, `command_with`): no process is started, so these run on every platform. The
//! launch's own behaviour on a resumed session is in `core/kernel/src/launch` and `engine_run` tests.

use super::{Way, command_with, way_of};
use serde_json::json;
use std::path::{Path, PathBuf};

const NOTE: &str = "[kith wake run-1] the staging box is ready";

fn env_of<'a>(env: &'a [(String, String)], name: &str) -> Option<&'a str> {
    env.iter().rev().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

fn make(way: &Way, reply: Option<&str>, launch: Option<&str>, kernel: Option<&Path>) -> Option<super::RunCommand> {
    command_with(
        way,
        Some(PathBuf::from("bin-kith")),
        launch.map(PathBuf::from),
        kernel,
        "house",
        "",
        reply,
        "codex:sess-a",
        NOTE,
    )
}

#[test]
fn the_session_decides_the_way_not_the_agents_harness() {
    let codex = json!({"engine": "codex"});
    assert_eq!(way_of(&codex, Some("codex"), "3f2a-claude-id"), Way::Claude, "a bare id is a Claude session");
    assert_eq!(way_of(&codex, Some("codex"), "codex:abc"), Way::Launch);
    assert_eq!(way_of(&codex, Some("codex"), "opencode:abc"), Way::Refused, "another engine's session");
    assert_eq!(way_of(&codex, Some("claude"), "ask:job-1"), Way::Claude, "a Claude agent's sessions are as they were");
    assert_eq!(
        way_of(&codex, None, "codex:abc"),
        Way::Launch,
        "the conversation's engine answers for an unreadable agent"
    );
    assert_eq!(way_of(&json!({"turn": 1}), None, "codex:abc"), Way::Refused, "an engine nobody names is not assumed");
    assert_eq!(way_of(&json!({"turn": 1}), None, "abc"), Way::Claude);
}

#[test]
fn a_claude_turn_is_kith_agent_resume_session_note_through_the_kith_entry() {
    let c = make(&Way::Claude, None, Some("launch"), Some(Path::new("/k"))).unwrap();
    assert_eq!(c.program, PathBuf::from("bin-kith"));
    assert_eq!(c.args, ["house", "--resume", "codex:sess-a", NOTE]);
    assert_eq!(env_of(&c.env, "KITH_RESUME"), None, "the Claude path resumes by its flag");
    assert_eq!(env_of(&c.env, "KITH_VIA"), Some("wake"));
}

#[test]
fn a_claude_card_reply_keeps_kith_ask_reply() {
    let c = make(&Way::Claude, Some("ask-7"), None, None).unwrap();
    assert_eq!(c.args, ["ask", "--reply", "ask-7", "house", "--resume", "codex:sess-a", "--", NOTE]);
}

#[test]
fn another_engines_turn_is_the_kernel_launch_exec_on_its_own_session_without_an_account_walk() {
    let c = make(&Way::Launch, None, Some("launch"), Some(Path::new("/k"))).unwrap();
    assert_eq!(c.program, PathBuf::from("launch"), "the launch program, not the kith entry");
    assert_eq!(c.args, ["kernel", "launch", "house", "--exec", "--kernel", "/k", "--", NOTE]);
    assert_eq!(env_of(&c.env, "KITH_RESUME"), Some("codex:sess-a"));
    assert_eq!(
        env_of(&c.env, "KITH_TURN_CONVERSATION"),
        Some("codex:sess-a"),
        "the launch refuses a new session for it"
    );
    assert_eq!(env_of(&c.env, "KITH_BACKUP_CHAIN"), None, "a wake runs on the session's own account");
    assert_eq!(env_of(&c.env, "KITH_VIA"), Some("wake"), "the wake's own env rides along");
    assert_eq!(env_of(&c.env, "KITH_LAUNCH_ASK_ID"), None);
}

#[test]
fn another_engines_card_reply_names_its_ask_and_a_person_keeps_their_env() {
    let c = command_with(
        &Way::Launch,
        None,
        Some(PathBuf::from("launch")),
        Some(Path::new("/k")),
        "house",
        "ana",
        Some("ask-7"),
        "codex:sess-a",
        NOTE,
    )
    .unwrap();
    assert_eq!(env_of(&c.env, "KITH_LAUNCH_ASK_ID"), Some("ask-7"));
    assert_eq!(env_of(&c.env, "KITH_PERSON"), Some("ana"));
    assert_eq!(env_of(&c.env, "KITH_VIA"), Some("host"));
}

#[test]
fn no_program_to_run_the_turn_is_no_command_so_it_stays_owed() {
    assert!(make(&Way::Launch, None, None, Some(Path::new("/k"))).is_none(), "no launch program");
    assert!(make(&Way::Launch, None, Some("launch"), None).is_none(), "no kernel folder");
    assert!(
        command_with(&Way::Claude, None, Some(PathBuf::from("launch")), None, "house", "", None, "s", NOTE).is_none()
    );
}
