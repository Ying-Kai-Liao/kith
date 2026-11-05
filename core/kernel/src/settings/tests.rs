#![allow(clippy::unwrap_used)]

use super::{Env, Out, pyval, run, valid_call_model};
use crate::accounts::{Ctx, System};
use serde_json::json;
use std::path::{Path, PathBuf};

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| (*s).to_owned()).collect()
}

/// A throwaway machine folder, with `text` as its settings file when given.
fn world(name: &str, text: Option<&str>) -> (PathBuf, System) {
    let root =
        std::env::temp_dir().join(format!("kith-settings-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    if let Some(t) = text {
        std::fs::write(root.join("settings.json"), t).unwrap();
    }
    (
        root,
        System {
            kernel: PathBuf::from("/nonexistent"),
        },
    )
}

fn go(root: &Path, sys: &System, a: &[&str]) -> Out {
    let ctx = Ctx {
        machine: root.to_path_buf(),
        agents: root.join("agents"),
        kernel: PathBuf::from("/nonexistent"),
        home: root.to_path_buf(),
        claude_config_dir: None,
        out: sys,
    };
    run(&ctx, Env::default(), &args(a))
}

fn file(root: &Path) -> String {
    std::fs::read_to_string(root.join("settings.json")).unwrap()
}

#[test]
fn a_limit_is_written_and_forgotten_without_losing_other_keys() {
    let (root, sys) = world(
        "limit",
        Some(r#"{"plugins": ["a@b"], "agents": {"x": {"permission_mode": "plan"}}}"#),
    );
    assert_eq!(
        go(&root, &sys, &["set", "limit", "x", "$12.345"]),
        Out::default()
    );
    let s: serde_json::Value = serde_json::from_str(&file(&root)).unwrap();
    assert_eq!(s["agents"]["x"]["limit"], json!(12.35));
    assert_eq!(s["plugins"], json!(["a@b"]));
    assert_eq!(
        go(&root, &sys, &["limit", "x"]).stdout,
        "{\"limit\": 12.35, \"at\": \"ask\"}\n"
    );
    assert_eq!(
        go(&root, &sys, &["set", "limit", "x", "none"]),
        Out::default()
    );
    assert!(!file(&root).contains("limit"));
}

#[test]
fn a_broken_file_refuses_a_write_and_reads_as_empty() {
    let (root, sys) = world("broken", Some(""));
    let out = go(&root, &sys, &["set", "limit", "x", "5"]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr
            .contains("is unreadable (Expecting value: line 1 column 1 (char 0))"),
        "{out:?}"
    );
    assert_eq!(go(&root, &sys, &["permission_mode"]).stdout, "auto\n");
    assert_eq!(file(&root), "");
}

#[test]
fn old_keys_read_as_the_new_ones_and_a_write_persists_the_new_shape() {
    let (root, sys) = world(
        "old",
        Some(r#"{"account": "codex", "backups": ["claude"]}"#),
    );
    assert_eq!(go(&root, &sys, &["accounts"]).stdout, "codex\nclaude\n");
    assert_eq!(
        go(&root, &sys, &["set", "call_model", "opus"]),
        Out::default()
    );
    let s: serde_json::Value = serde_json::from_str(&file(&root)).unwrap();
    assert_eq!(s["accounts"], json!(["codex", "claude"]));
    assert!(s.get("account").is_none());
}

#[test]
fn call_model_names() {
    for ok in ["sonnet", "opus[1m]", "claude-sonnet-5", "own", "default"] {
        assert!(valid_call_model(ok), "{ok}");
    }
    for bad in [
        "",
        "gpt",
        "claude-",
        "Sonnet",
        "claude-x[1m]x",
        "default[1m]",
    ] {
        assert!(!valid_call_model(bad), "{bad}");
    }
}

#[test]
fn python_text() {
    assert_eq!(pyval::str_repr("it's"), "\"it's\"");
    assert_eq!(
        pyval::repr(&json!({"a": [1, null, true, 2.5]})),
        "{'a': [1, None, True, 2.5]}"
    );
    assert_eq!(pyval::float(" 1_000 "), Some(1000.0));
    assert_eq!(pyval::float("1__0"), None);
}
