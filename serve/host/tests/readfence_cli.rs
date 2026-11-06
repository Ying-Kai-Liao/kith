#![allow(clippy::unwrap_used)]
#![cfg(unix)]
//! `kith kernel readfence` as a process: the payload on stdin, the deny line on stdout, exit status 0.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

fn run(args: &[&str], stdin: &str, cwd: &Path) -> (String, Option<i32>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_kith"))
        .args(["kernel", "readfence"])
        .args(args)
        .current_dir(cwd)
        .env("HOME", cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let o = child.wait_with_output().unwrap();
    (String::from_utf8(o.stdout).unwrap(), o.status.code())
}

#[test]
fn readfence_verb_prints_the_decision() {
    let tmp = std::env::temp_dir().join(format!("readfence-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    // A root whose name starts with a dash: `--root -wd` must still be a root.
    std::fs::create_dir_all(tmp.join("-wd")).unwrap();
    std::fs::write(tmp.join("-wd/f"), "x").unwrap();
    let tmp = std::fs::canonicalize(&tmp).unwrap();
    let wd = tmp.join("-wd");
    let w = wd.to_str().unwrap();
    let inside = format!(r#"{{"tool_name":"Read","tool_input":{{"file_path":"{w}/f"}},"cwd":"{w}"}}"#);
    let outside = format!(r#"{{"tool_name":"Read","tool_input":{{"file_path":"/etc/hosts"}},"cwd":"{w}"}}"#);

    let (out, code) = run(&["--root", w], &inside, &wd);
    assert_eq!((out.as_str(), code), ("", Some(0)));
    // The relative root `-wd` is read against the working directory (tmp).
    let (out, code) = run(&["--root", "-wd"], &inside, &tmp);
    assert_eq!((out.as_str(), code), ("", Some(0)));
    let (out, code) = run(&["--root", w], &outside, &wd);
    assert_eq!(
        out,
        "{\"hookSpecificOutput\": {\"hookEventName\": \"PreToolUse\", \"permissionDecision\": \"deny\", \"permissionDecisionReason\": \"outside the folders this run was given (/etc/hosts)\"}}\n"
    );
    assert_eq!(code, Some(0));
    let (out, code) = run(&["--root", w], "{", &wd);
    assert!(out.contains("the read check could not run"));
    assert_eq!(code, Some(0));
    let (out, _) = run(&["--help"], "", &wd);
    assert!(out.contains("kith kernel readfence"));
    let _ = std::fs::remove_dir_all(&tmp);
}
