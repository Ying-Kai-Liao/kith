//! `kith permit` on Windows: the pipe to the host, the named refusals, the owner-only run folder. Run by the Windows CI job.

#![allow(clippy::unwrap_used)]

use super::host::{self, call_host};
use super::{Env, hooks, issue, refused};
use serde_json::{Map, Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::time::Duration;

/// A new folder of this test's own under the temp folder (no `tempfile` in this crate), removed when dropped.
struct Tmp(std::path::PathBuf);

impl Tmp {
    fn new(tag: &str) -> Self {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let p = std::env::temp_dir().join(format!("kith-permit-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn pipe_name(tag: &str) -> String {
    format!(r"\\.\pipe\kith-permit-test-{tag}-{}", std::process::id())
}

#[test]
fn a_request_reaches_the_host_over_the_pipe_and_its_answer_comes_back() {
    let name = pipe_name("call");
    let listener = kith_winsys::pipe::listen(&name).unwrap();
    let server = std::thread::spawn(move || {
        let mut s = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&mut s).read_line(&mut line).unwrap();
        let said: Value = serde_json::from_str(&line).unwrap();
        let ok = said["token"] == "t0k" && said["method"] == "requestPermission";
        s.write_all(format!("{}\n", json!({"ok": ok, "behavior": "allow"})).as_bytes())
            .unwrap();
    });
    let env = Env::with(&[("KITH_ASK_TOKEN", "t0k"), ("KITH_ASK_SOCKET", &name)]);
    let reply = call_host(
        &env,
        "requestPermission",
        &Map::new(),
        Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(reply["ok"], true);
    assert_eq!(reply["behavior"], "allow");
    server.join().unwrap();
}

#[test]
fn no_host_on_the_pipe_is_no_answer() {
    let env = Env::with(&[
        ("KITH_ASK_TOKEN", "t0k"),
        ("KITH_ASK_SOCKET", &pipe_name("nobody")),
    ]);
    assert!(call_host(&env, "x", &Map::new(), Duration::from_millis(300)).is_none());
}

#[test]
fn a_host_that_is_down_is_named_and_never_restarted_through_launchctl() {
    // a port nobody listens on, a data folder with no token
    let tmp = Tmp::new("down");
    let env = Env::with(&[
        ("KITH_SERVE_DIR", &tmp.0.to_string_lossy()),
        ("KITH_SERVE_PORT", "1"),
    ]);
    let out = issue::issue(&env, Path::new(""), "echo", "s1", "1234");
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains(host::RESTART_REFUSED), "{}", out.stderr);
    assert!(!out.stderr.contains("launchctl"), "{}", out.stderr);
    assert_eq!(out.stdout, "");
}

#[test]
fn a_run_that_does_not_know_the_host_says_so() {
    let out = issue::issue(&Env::with(&[]), Path::new(""), "echo", "s1", "1234");
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains(host::ENDPOINT_UNKNOWN),
        "{}",
        out.stderr
    );
}

#[test]
fn the_restart_says_no_and_does_nothing() {
    let env = Env::with(&[]);
    assert!(!host::restart_host(
        &env,
        Path::new(""),
        true,
        Duration::from_millis(1)
    ));
    assert_eq!(
        host::RESTART_REFUSED,
        "not on Windows yet: host restart (W11)"
    );
}

#[test]
fn the_guest_door_and_bug_halves_are_refused_by_name() {
    let (env, none) = (Env::with(&[]), Map::new());
    assert!(refused::run_door(&none, &env, Path::new("")).contains("(P3-12)"));
    assert!(refused::report_bug(&none, &env, Path::new("")).contains("(P4-17)"));
    assert!(refused::ask_helper(&none, &env, Path::new("")).contains("WI6"));
    let applied = super::run(
        &env,
        Path::new(""),
        &["apply".to_owned(), "settings.json".to_owned()],
        &mut std::io::empty(),
    );
    assert_eq!(applied.code, 1);
    assert!(applied.stderr.contains("WI6"), "{}", applied.stderr);
}

#[test]
fn a_run_folder_for_one_shots_is_owner_only() {
    let tmp = Tmp::new("once");
    let dir = tmp.0.join("once").join("run1");
    hooks::keep_once(&dir, "k1", 1_700_000_000.0, false).unwrap();
    assert_eq!(
        kith_winsys::acl::check(&dir).unwrap(),
        kith_winsys::acl::Check::OwnerOnly
    );
    // a second one-shot in the same folder goes through the existing-folder path
    hooks::keep_once(&dir, "k2", 1_700_000_001.0, true).unwrap();
    assert!(dir.join("k1").is_file() && dir.join("k2").is_file());
}
