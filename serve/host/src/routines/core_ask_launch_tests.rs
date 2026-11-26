//! The ask kind of the run door through the REAL launch (architect's review of #1222, MUST 1 and 2): the real core (the claims
//! stepped), the door's own command and environment, the built `kith` / `kernel/bin/kith` as the launch, and only the engine a
//! stand-in (`routines/launch_turn_e2e_tests.rs` has the world). A bg ask's turn sits on the core's made-up conversation
//! `ask:<id>`; what is proved here is that the launch attaches the claim naming it (`KITH_TURN_CONVERSATION`, set by the door),
//! that the claim is `running` on a process, that the task reaches the engine once, that a Claude callee's record keeps its real
//! session (so `-c` continues it), and that a host restarted mid-run leaves the callee running.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::core_ask::{World as Door, take};
use super::launch_turn_e2e_tests::{World, built_kith, repo_kernel};
use crate::terminal::core_writer::CoreWriter;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn until(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(60);
    while !f() {
        assert!(Instant::now() < end, "never happened: {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `kith ask --bg house` as the core records it, opened by agent `boss` in session `sess-1`.
fn open(w: &World, id: &str, extra: &Value) -> Value {
    let var = |k: &str| w.env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap();
    let mut p = json!({"id": id, "asked": "house", "mode": "bg", "task": "do the ask task",
        "home": var("KITH_HOME"), "kernel": repo_kernel(),
        "env": {"KITH_HOME": var("KITH_HOME"), "KITH_STATE": var("KITH_STATE"), "KITH_AGENTS": var("KITH_AGENTS"),
                "KITH_AGENT": "boss", "CLAUDE_CODE_SESSION_ID": "sess-1", "KITH_VIA": "terminal"}});
    for (k, v) in extra.as_object().into_iter().flatten() {
        p[k] = v.clone();
    }
    w.core("ask.open", &p)
}

/// The door starts the callee: `bin` is the entry a Claude callee runs, `launch` the program the Rust launch starts from for another
/// engine, as production chooses it (`KITH_BIN`, else the host's own binary); the engine's world is the test's.
fn door(w: &World, host: &str, bin: std::path::PathBuf, launch: Option<std::path::PathBuf>, harness: &'static str) {
    let writer = Arc::new(CoreWriter::with_instance(w.folders.clone(), host.to_owned()));
    let turns = writer.queued_from(&["ask"]).expect("the core lists the ask turns");
    let world = Door {
        bin: Some(bin),
        launch,
        kernel: Some(repo_kernel()),
        harness: &move |_| Some(harness.to_owned()),
        runtime: None,
        env: w
            .env
            .iter()
            .cloned()
            .chain([("ANSWER".to_owned(), r#"{"result":"the answer","is_error":false,"total_cost_usd":0}"#.to_owned())])
            .collect(),
    };
    take(&writer, &turns, &world);
}

/// The claim of the ask's turn once the callee's launch attached it (running, on a process), as the core shows it.
fn attached(w: &World, id: &str) -> Value {
    let conversation = format!("ask:{id}");
    let end = Instant::now() + Duration::from_secs(60);
    loop {
        let seen =
            crate::core_front::call_as_host(&w.folders, "hostclaim.show", &json!({"conversation": conversation}))
                .map_or(Value::Null, |v| v["claim"].clone());
        if seen["state"] == "running" && seen["pid"].is_i64() {
            return seen;
        }
        if Instant::now() >= end {
            // What the launch said is in the run's own files: the out file and the log beside it.
            let said = |ext: &str| {
                std::fs::read_to_string(w.folders.machine.join("core/asks").join(format!("{id}.{ext}")))
                    .unwrap_or_default()
            };
            panic!(
                "the launch did not attach the ask's claim: {seen}\nout: {}\nerr: {}\nengine runs: {:?}",
                said("out"),
                said("err"),
                w.runs()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn alive(pid: i64) -> bool {
    // SAFETY: signal 0 only asks whether the process exists.
    unsafe { libc::kill(i32::try_from(pid).unwrap(), 0) == 0 }
}

fn gone_vars(run: &str) {
    for gone in ["KITH_TURN_CLAIM", "HQ_TURN_CLAIM", "KITH_TURN_CONVERSATION", "HQ_TURN_CONVERSATION"] {
        assert!(!run.lines().any(|l| l.starts_with(&format!("{gone}="))), "{gone} reached the engine");
    }
}

#[test]
fn a_claude_callee_attaches_naming_ask_conversation_keeps_its_session_and_survives_a_host_restart() {
    let w = World::start();
    open(&w, "r1", &json!({}));
    std::fs::write(w.dir.join("hold"), "").unwrap();
    door(&w, "kith-serve-host/1-a", repo_kernel().join("bin/kith"), None, "claude");
    let claim = attached(&w, "r1");
    assert_eq!(claim["conversation"], "ask:r1", "the claim stays the turn's: {claim}");
    let pid = claim["pid"].as_i64().unwrap();
    // The host is restarted while the callee runs: the attached claim is the run's own, nothing is given back or run twice.
    w.core_host("hostclaim.adopt", &json!({"instance": "kith-serve-host/2-b"}));
    assert!(alive(pid), "the callee keeps running");
    assert_eq!(w.show(claim["claimId"].as_i64().unwrap())["state"], "running");
    std::fs::remove_file(w.dir.join("hold")).unwrap();
    until("the engine ran", || w.runs().len() == 1);
    let runs = w.runs();
    assert!(runs[0].contains("do the ask task"), "the task reached the engine: {}", runs[0]);
    gone_vars(&runs[0]);
    // Its record keeps the engine's real session (the one the launch gave Claude), so `-c` resumes it.
    // The stand-in prints its arguments with `$*`, and the system prompt among them spans lines: the whole record is searched.
    let session = runs[0]
        .split_whitespace()
        .skip_while(|x| *x != "--session-id")
        .nth(1)
        .unwrap_or_else(|| panic!("no --session-id in the engine's arguments: {}", runs[0]))
        .to_owned();
    let record = w.core("ask.record", &json!({"id": "r1"}));
    assert_eq!(record["session"], session.as_str(), "{record}");
    until("the ask is settled", || w.core("ask.record", &json!({"id": "r1"}))["state"] != "open");
    let out = std::fs::read_to_string(w.folders.machine.join("core/asks/r1.out")).unwrap_or_default();
    assert_eq!(w.core("ask.record", &json!({"id": "r1"}))["state"], "answered", "the callee's out file: {out}");
    let rows = w.core("ask.read", &json!({"id": "r1"}))["rows"].clone();
    assert!(rows.to_string().contains("the answer"), "the answer of the callee: {rows}");
    assert_eq!(w.runs().len(), 1, "the task reached an engine once");
}

#[test]
fn another_engine_attaches_naming_ask_conversation_through_the_kernels_launch() {
    let w = World::start();
    open(&w, "r2", &json!({"engine": "codex"}));
    std::fs::write(w.dir.join("hold"), "").unwrap();
    // The bash entry is the Claude entry here, as on a Mac without `KITH_KERNEL_BIN`: it has no `kernel` verb, so the launch must not
    // be started from it.
    let launch = crate::kernel::launch_program_of(Some(built_kith().into_os_string()), None);
    door(&w, "kith-serve-host/1-a", repo_kernel().join("bin/kith"), launch, "claude");
    let claim = attached(&w, "r2");
    assert_eq!(claim["conversation"], "ask:r2", "{claim}");
    assert!(alive(claim["pid"].as_i64().unwrap()));
    std::fs::remove_file(w.dir.join("hold")).unwrap();
    until("the engine ran", || w.runs().len() == 1);
    // The engine records its start; the task reaches it later, over its conversation with the launch. The ask is settled when the
    // launch has ended, which is when everything the launch sent it is in the file.
    until("the ask is settled", || w.core("ask.record", &json!({"id": "r2"}))["state"] != "open");
    assert_eq!(w.core("ask.record", &json!({"id": "r2"}))["state"], "answered");
    assert_eq!(w.core("ask.read", &json!({"id": "r2"}))["rows"].as_array().map(Vec::len), Some(1));
    let sent = std::fs::read_to_string(w.dir.join("acp-in")).unwrap();
    assert!(sent.contains("do the ask task"), "the task reached the engine: {sent}");
    gone_vars(&w.runs()[0]);
    assert_eq!(w.runs().len(), 1);
}
