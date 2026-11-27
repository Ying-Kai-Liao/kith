#![allow(clippy::unwrap_used)]

use super::*;
use serde_json::json;

// The expected argv and env are what kernel/adapters/harness.py's `codex()` and `hermes()` built for the same inputs,
// captured before that file was deleted (dirs are the real paths a Mac gives /tmp and /var).

const PROMPT: &str = "You echo.\nSay \"hi\" \\ done.\n";

fn run(
    dirs: &[&str],
    mcp: &Value,
    model: &str,
    effort: &str,
    bypass: bool,
    global_skills: bool,
) -> RunInputs {
    RunInputs {
        prompt: PROMPT.to_owned(),
        dirs: dirs.iter().map(|d| (*d).to_owned()).collect(),
        mcp: mcp.as_object().cloned().unwrap_or_default(),
        model: model.to_owned(),
        effort: effort.to_owned(),
        bypass,
        network: false,
        global_skills,
    }
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}

const DEV: &str = r#"developer_instructions="You echo.\nSay \"hi\" \\ done.\n""#;

#[test]
fn codex_in_the_workspace_with_no_server() {
    let t = tui_argv(
        "codex",
        &run(&["/private/tmp"], &json!({}), "", "", false, false),
    )
    .unwrap();
    assert_eq!(
        t.argv,
        strs(&[
            "codex",
            "-c",
            DEV,
            "-c",
            "sandbox_mode=\"workspace-write\"",
            "-c",
            "sandbox_workspace_write.writable_roots=[\"/private/tmp\"]",
            "--disable",
            "plugins",
            "--disable",
            "apps",
            "--add-dir",
            "/private/tmp",
        ])
    );
    assert!(t.env.is_empty() && t.notes.is_empty());
}

#[test]
fn codex_in_the_workspace_with_model_effort_and_a_server() {
    let mcp = json!({"a": {"command": "node"}});
    let t = tui_argv(
        "codex",
        &run(
            &["/private/tmp", "/private/var"],
            &mcp,
            "gpt-x",
            "medium",
            false,
            false,
        ),
    )
    .unwrap();
    assert_eq!(
        t.argv,
        strs(&[
            "codex",
            "-c",
            DEV,
            "-c",
            "model_reasoning_effort=\"medium\"",
            "-c",
            "sandbox_mode=\"workspace-write\"",
            "-c",
            "sandbox_workspace_write.writable_roots=[\"/private/tmp\", \"/private/var\"]",
            "-c",
            "mcp_servers.a={\"command\" = \"node\", \"args\" = [], \"default_tools_approval_mode\" = \"approve\"}",
            "-m",
            "gpt-x",
            "--disable",
            "plugins",
            "--disable",
            "apps",
            "--add-dir",
            "/private/tmp",
            "--add-dir",
            "/private/var",
        ])
    );
}

#[test]
fn codex_bypass_with_every_kind_of_server_and_global_skills() {
    let mcp = json!({
        "a": {"command": "node", "args": ["s.js", "--x"], "env": {"K": "v\"q"}},
        "b": {"url": "http://h/mcp"},
        "c d": {"command": "x"},
    });
    let t = tui_argv(
        "codex",
        &run(
            &["/private/tmp", "/private/var"],
            &mcp,
            "gpt-x",
            "max",
            true,
            true,
        ),
    )
    .unwrap();
    assert_eq!(
        t.argv,
        strs(&[
            "codex",
            "-c",
            DEV,
            "-c",
            "model_reasoning_effort=\"xhigh\"",
            "-c",
            "mcp_servers.a={\"command\" = \"node\", \"args\" = [\"s.js\", \"--x\"], \"env\" = {\"K\" = \"v\\\"q\"}, \"default_tools_approval_mode\" = \"approve\"}",
            "-c",
            "mcp_servers.b={\"url\" = \"http://h/mcp\", \"default_tools_approval_mode\" = \"approve\"}",
            "-c",
            "mcp_servers.\"c d\"={\"command\" = \"x\", \"args\" = [], \"default_tools_approval_mode\" = \"approve\"}",
            "-m",
            "gpt-x",
            "--dangerously-bypass-approvals-and-sandbox",
            "--add-dir",
            "/private/tmp",
            "--add-dir",
            "/private/var",
        ])
    );
}

#[test]
fn codex_sandbox_gets_network_only_when_the_launcher_said_so() {
    let owner_run = RunInputs {
        network: true,
        ..run(&["/private/tmp"], &json!({}), "", "", false, false)
    };
    let owner = tui_argv("codex", &owner_run).unwrap();
    assert_eq!(
        owner.argv,
        strs(&[
            "codex",
            "-c",
            DEV,
            "-c",
            "sandbox_mode=\"workspace-write\"",
            "-c",
            "sandbox_workspace_write.writable_roots=[\"/private/tmp\"]",
            "-c",
            "sandbox_workspace_write.network_access=true",
            "--disable",
            "plugins",
            "--disable",
            "apps",
            "--add-dir",
            "/private/tmp",
        ])
    );
    // A run without the flag has no network key, and neither does a bypass run, which has no sandbox at all.
    let guest = tui_argv(
        "codex",
        &run(&["/private/tmp"], &json!({}), "", "", false, false),
    )
    .unwrap();
    assert!(!guest.argv.iter().any(|a| a.contains("network_access")));
    let owner_bypass = RunInputs {
        network: true,
        ..run(&["/private/tmp"], &json!({}), "", "", true, false)
    };
    let bypass = tui_argv("codex", &owner_bypass).unwrap();
    assert!(
        !bypass
            .argv
            .iter()
            .any(|a| a.contains("network_access") || a.starts_with("sandbox_"))
    );
}

#[test]
fn codex_escapes_non_ascii_as_python_did() {
    let mut r = run(&["/w"], &json!({}), "", "", true, true);
    r.prompt = "caf\u{e9} \u{1f600}".to_owned();
    let t = tui_argv("codex", &r).unwrap();
    assert_eq!(
        t.argv[2],
        "developer_instructions=\"caf\\u00e9 \\ud83d\\ude00\""
    );
}

#[test]
fn hermes_plain_and_with_everything() {
    let t = tui_argv(
        "hermes",
        &run(&["/private/tmp"], &json!({}), "", "", false, false),
    )
    .unwrap();
    assert_eq!(t.argv, strs(&["hermes", "chat"]));
    assert_eq!(
        t.env,
        vec![(
            "HERMES_EPHEMERAL_SYSTEM_PROMPT".to_owned(),
            PROMPT.to_owned()
        )]
    );
    assert_eq!(t.notes, Vec::<String>::new());

    let mcp = json!({"a": {"command": "x"}});
    let t = tui_argv(
        "hermes",
        &run(&["/private/tmp"], &mcp, "m1", "max", true, true),
    )
    .unwrap();
    assert_eq!(
        t.argv,
        strs(&[
            "hermes",
            "chat",
            "-m",
            "m1",
            "--reasoning",
            "xhigh",
            "--yolo"
        ])
    );
    assert_eq!(
        t.notes,
        ["hermes takes no MCP servers per run; this run is without a (Claude Code runs have them)"]
    );
}

#[test]
fn other_engines_have_no_terminal_session() {
    for e in ["opencode", "qwen", "pi", "agy", "claude", "nope"] {
        assert!(tui_argv(e, &RunInputs::default()).is_none(), "{e}");
    }
}
