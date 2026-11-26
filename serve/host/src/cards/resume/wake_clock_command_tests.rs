//! What a wake turn runs, by engine (`command_with`, `is_claude`): no process is started, so these run on every platform.

use super::{command_with, is_claude};
use serde_json::json;
use std::path::{Path, PathBuf};

const NOTE: &str = "[kith wake run-1] the staging box is ready";

fn env_of<'a>(env: &'a [(String, String)], name: &str) -> Option<&'a str> {
    env.iter().rev().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

fn make(claude: bool, reply: Option<&str>, launch: Option<&str>, kernel: Option<&Path>) -> Option<super::RunCommand> {
    command_with(
        claude,
        Some(PathBuf::from("bin-kith")),
        launch.map(PathBuf::from),
        kernel,
        "house",
        "",
        reply,
        "sess-a",
        NOTE,
    )
}

#[test]
fn the_agents_harness_decides_and_an_engine_nobody_names_is_not_claude() {
    assert!(is_claude(&json!({"engine": "claude"}), None));
    assert!(!is_claude(&json!({"engine": "codex"}), None));
    assert!(!is_claude(&json!({"turn": 1}), None), "an engine the core did not name is not assumed");
    assert!(!is_claude(&json!({"engine": "claude"}), Some("codex")), "the agent's harness decides");
    assert!(is_claude(&json!({"engine": "codex"}), Some("claude-code")));
}

#[test]
fn a_claude_turn_is_kith_agent_resume_session_note_through_the_kith_entry() {
    let c = make(true, None, Some("launch"), Some(Path::new("/k"))).unwrap();
    assert_eq!(c.program, PathBuf::from("bin-kith"));
    assert_eq!(c.args, ["house", "--resume", "sess-a", NOTE]);
    assert_eq!(env_of(&c.env, "KITH_RESUME"), None, "the Claude path resumes by its flag");
    assert_eq!(env_of(&c.env, "KITH_VIA"), Some("wake"));
}

#[test]
fn a_claude_card_reply_keeps_kith_ask_reply() {
    let c = make(true, Some("ask-7"), None, None).unwrap();
    assert_eq!(c.args, ["ask", "--reply", "ask-7", "house", "--resume", "sess-a", "--", NOTE]);
}

#[test]
fn another_engines_turn_is_the_kernel_launch_exec_with_the_session_handed_on() {
    let c = make(false, None, Some("launch"), Some(Path::new("/k"))).unwrap();
    assert_eq!(c.program, PathBuf::from("launch"), "the launch program, not the kith entry");
    assert_eq!(c.args, ["kernel", "launch", "house", "--exec", "--kernel", "/k", "--", NOTE]);
    assert_eq!(env_of(&c.env, "KITH_RESUME"), Some("sess-a"));
    assert_eq!(env_of(&c.env, "KITH_TURN_CONVERSATION"), Some("sess-a"));
    assert_eq!(env_of(&c.env, "KITH_BACKUP_CHAIN"), Some("on"));
    assert_eq!(env_of(&c.env, "KITH_VIA"), Some("wake"), "the wake's own env rides along");
    assert_eq!(env_of(&c.env, "KITH_LAUNCH_ASK_ID"), None);
}

#[test]
fn another_engines_card_reply_names_its_ask_and_a_person_keeps_their_env() {
    let c = command_with(
        false,
        None,
        Some(PathBuf::from("launch")),
        Some(Path::new("/k")),
        "house",
        "ana",
        Some("ask-7"),
        "sess-a",
        NOTE,
    )
    .unwrap();
    assert_eq!(env_of(&c.env, "KITH_LAUNCH_ASK_ID"), Some("ask-7"));
    assert_eq!(env_of(&c.env, "KITH_PERSON"), Some("ana"));
    assert_eq!(env_of(&c.env, "KITH_VIA"), Some("host"));
}

#[test]
fn no_program_to_run_the_turn_is_no_command_so_it_stays_owed() {
    assert!(make(false, None, None, Some(Path::new("/k"))).is_none(), "no launch program");
    assert!(make(false, None, Some("launch"), None).is_none(), "no kernel folder");
    assert!(command_with(true, None, Some(PathBuf::from("launch")), None, "house", "", None, "s", NOTE).is_none());
}
