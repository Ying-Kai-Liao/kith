//! The host's start of a cron job's run (`core_cron.rs`): the command line, the environment it passes (the permission mode of
//! `kith-sched`'s environment among it), the turn on stdin, and what the child's end is read as. The claim and the end are the
//! core's (`conversations/sched_run_tests.rs`).

#![allow(clippy::unwrap_used)]

use super::{PERMISSION_MODE, command_with, spawn_cmd};
use serde_json::json;
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// A kernel whose `bin/kith-sched` is this shell script.
fn kernel(body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kith-cron-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    let file = dir.join("bin/kith-sched");
    std::fs::write(&file, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

fn run(kernel: &Path, mode: Option<&str>, spec: &serde_json::Value) -> (std::process::ExitStatus, String) {
    let cmd = command_with(kernel, "house", "daily", 7, 9, mode.map(OsString::from));
    let mut child = spawn_cmd(cmd, spec).unwrap();
    let mut err = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut err).unwrap();
    (child.wait().unwrap(), err)
}

#[test]
fn the_run_is_kith_sched_run_turn_with_the_turn_on_stdin_and_the_claim_on_the_line() {
    let out = std::env::temp_dir().join(format!("kith-cron-out-{}", uuid::Uuid::new_v4()));
    let k = kernel(&format!(
        "echo \"$@\" > {0}.args; cat > {0}.stdin; echo \"[$KITH_SCHED_PERMISSION_MODE]\" > {0}.env",
        out.display()
    ));
    let spec = json!({"prompt": "do it", "settings": {"model": "m1", "person": "sam"}});
    let (status, _) = run(&k, Some("plan"), &spec);
    assert!(status.success());
    assert_eq!(
        std::fs::read_to_string(format!("{}.args", out.display())).unwrap().trim(),
        "run-turn house daily --turn 7 --claim 9"
    );
    let sent: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{}.stdin", out.display())).unwrap()).unwrap();
    assert_eq!(sent, spec, "the prompt and the settings fixed at fire time, as the core listed them");
    assert_eq!(std::fs::read_to_string(format!("{}.env", out.display())).unwrap().trim(), "[plan]");
}

#[test]
fn the_permission_mode_is_the_hosts_own_environments_and_is_not_invented() {
    let k = kernel("echo \"[$KITH_SCHED_PERMISSION_MODE]\" >&2");
    let spec = json!({"prompt": "x", "settings": null});
    assert_eq!(run(&k, Some("acceptEdits"), &spec).1.trim(), "[acceptEdits]");
    let cmd = command_with(&k, "house", "daily", 7, 9, None);
    assert!(
        cmd.get_envs().all(|(k, _)| k != PERMISSION_MODE),
        "unset stays unset: the agent's mode, else kith's default"
    );
}

#[test]
fn a_refusal_is_the_last_line_of_stderr_and_a_non_zero_exit() {
    let k = kernel("echo 'kith-sched: run-turn: claim 9 is gone (ended); nothing ran' >&2; exit 4");
    let (status, err) = run(&k, None, &json!({"prompt": "x", "settings": null}));
    assert_eq!(status.code(), Some(4));
    assert!(err.contains("claim 9 is gone"), "{err}");
}

#[test]
fn the_run_leads_a_process_group_of_its_own() {
    let k = kernel("ps -o pid=,pgid= -p $$ >&2");
    let (_, err) = run(&k, None, &json!({"prompt": "x", "settings": null}));
    let mut f = err.split_whitespace();
    let (lead, group) = (f.next().unwrap(), f.next().unwrap());
    assert_eq!(lead, group, "a host stop signals the host's group, not this one");
}
