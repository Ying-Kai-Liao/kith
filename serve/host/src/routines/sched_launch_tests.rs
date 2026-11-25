//! The host's start of a cron job's run as the launch (`sched_launch.rs`, called by `core_cron.rs`): the token decides which
//! program line is run, the launch gets the turn on stdin and the run's log as stdout and stderr, a start that fails cancels the
//! turn with its reason, and the end of the run is reported (the retries are `core_cron_retry_tests.rs`). The core is scripted
//! (`terminal::core_claims_tests::register`); the program is a shell script. The real core and the real program are
//! `tests/sched_turn_e2e.rs`.

#![allow(clippy::unwrap_used)]

use crate::core_front::Answer;
use crate::routines::core_cron::{Live, TEST_KERNEL};
use crate::routines::core_runs::Ops;
use crate::routines::run_door::Started;
use crate::routines::sched_launch::TEST_PROGRAM;
use crate::routines::switch_tests::hub;
use crate::terminal::core_claims::tests::{Fake, register};
use crate::terminal::core_writer::CoreWriter;
use kith_kernel::paths::Paths;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The two stand-ins are the process's: one test holds them at a time.
static SERIAL: Mutex<()> = Mutex::new(());

struct Lab {
    dir: PathBuf,
    folders: Paths,
    /// Every call the scripted core took: (method, params).
    log: Arc<Mutex<Vec<(String, Value)>>>,
    _held: std::sync::MutexGuard<'static, ()>,
}

impl Lab {
    fn new() -> Self {
        let held = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = std::env::temp_dir().join(format!("kith-sched-launch-{}", uuid::Uuid::new_v4()));
        let state = dir.join("state");
        let folders = Paths {
            home: dir.clone(),
            state: state.clone(),
            agents: dir.join("agents"),
            home_real: dir.clone(),
            state_real: state.clone(),
            agents_real: dir.join("agents"),
            machine: dir.join("machine"),
            secrets: dir.join("secrets"),
            logs: dir.join("logs"),
            cache: dir.join("cache"),
            store: dir.join("store"),
        };
        let log = Arc::new(Mutex::new(Vec::new()));
        let seen = log.clone();
        let fake: Fake = Arc::new(move |method: &str, p: &Value| {
            seen.lock().unwrap().push((method.to_owned(), p.clone()));
            Answer::Done(match method {
                "hostclaim.epoch" => json!({"epoch": 1}),
                "hostclaim.claim" => json!({"claimId": 9}),
                _ => json!({}),
            })
        });
        register(&state, fake);
        std::fs::create_dir_all(dir.join("k/bin")).unwrap();
        *TEST_KERNEL.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(dir.join("k"));
        Self { dir, folders, log, _held: held }
    }

    fn script(&self, name: &str, body: &str) -> PathBuf {
        let file = self.dir.join(name);
        std::fs::write(&file, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        file
    }

    /// A program that lists these tokens in `kernel version`, and otherwise runs `body`.
    fn program(&self, tokens: &str, body: &str) {
        let file = self.script(
            "kith-program",
            &format!("[ \"$1 $2\" = \"kernel version\" ] && {{ echo '{{\"abi\":1,\"modules\":[{tokens}]}}'; exit 0; }}\n{body}"),
        );
        *TEST_PROGRAM.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(file);
    }

    fn writer(&self) -> Arc<CoreWriter> {
        Arc::new(CoreWriter::with_instance(self.folders.clone(), "test".to_owned()))
    }

    fn calls(&self, method: &str) -> Vec<Value> {
        self.log.lock().unwrap().iter().filter(|(m, _)| m == method).map(|(_, p)| p.clone()).collect()
    }

    fn until(&self, what: &str, mut f: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(20);
        while Instant::now() < end {
            if f() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("gave up waiting for {what}: {:?}", self.log.lock().unwrap());
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.join(name)).unwrap_or_default()
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        *TEST_KERNEL.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        *TEST_PROGRAM.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn fired(person: &str, owner: bool) -> Value {
    json!({"turn": 7, "conversation": "schedule:house:daily:claude", "agent": "house", "engine": "claude", "job": "daily",
           "person": person, "owner": owner, "prompt": "do it", "deadline": null, "parkedReason": null,
           "redeemedBefore": false, "settings": {"model": "m1", "person": null}})
}

fn log_of(folders: &Paths) -> PathBuf {
    let dir = folders.logs.join("sched/runs/house");
    std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).next().expect("the run's log")
}

fn log_text(folders: &Paths) -> String {
    std::fs::read_to_string(log_of(folders)).unwrap()
}

#[test]
fn a_program_that_lists_the_token_runs_the_turn_as_the_launch_with_the_turn_on_stdin_and_the_log_as_output() {
    let lab = Lab::new();
    lab.program(
        "\"launch\",\"sched-turn\"",
        &format!(
            "echo \"$@\" > {0}/argv; cat > {0}/stdin; echo engine-said-this; echo and-this >&2",
            lab.dir.display()
        ),
    );
    let hub = hub();
    let started = Live(lab.writer()).cron(&hub, &fired("kai", true), "daily");
    assert_eq!(started, Started::Ran);
    lab.until("the report", || !lab.calls("hostclaim.report").is_empty());
    assert_eq!(
        lab.read("argv").trim(),
        format!("kernel launch house --exec --kernel {} --turn 7 --claim 9", lab.dir.join("k").display())
    );
    let spec: Value = serde_json::from_str(&lab.read("stdin")).unwrap();
    assert_eq!(spec["prompt"], "do it");
    assert_eq!(spec["settings"], json!({"model": "m1", "person": null}));
    assert_eq!(spec["person"], Value::Null, "the owner is no person");
    assert_eq!(spec["job"]["id"], "daily");
    assert_eq!(spec["origin"], json!({}), "the owner has no app chat with this agent here: no origin, no outbox");
    let log = log_text(&lab.folders);
    assert!(log.contains("engine-said-this") && log.contains("and-this"), "stdout and stderr are the log: {log}");
    let report = &lab.calls("hostclaim.report")[0];
    assert_eq!((report["claimId"].as_i64(), report["kind"].as_str()), (Some(9), Some("done")));
}

#[test]
fn a_guests_turn_names_the_person_and_has_an_outbox_that_is_dropped_never_the_owners() {
    let lab = Lab::new();
    lab.program("\"sched-turn\"", &format!("cat > {}/stdin", lab.dir.display()));
    let hub = hub();
    assert_eq!(Live(lab.writer()).cron(&hub, &fired("sam", false), "daily"), Started::Ran);
    lab.until("the report", || !lab.calls("hostclaim.report").is_empty());
    let spec: Value = serde_json::from_str(&lab.read("stdin")).unwrap();
    assert_eq!(spec["person"], "sam");
    let outbox = spec["origin"]["KITH_RUN_OUTBOX"].as_str().expect("an outbox for a guest with no chat").to_owned();
    assert_eq!(spec["origin"]["HQ_RUN_OUTBOX"], outbox.as_str());
    assert!(spec["origin"].get("KITH_CALL_KEY").is_none(), "no chat, no call key");
    let meta: Value = serde_json::from_str(&std::fs::read_to_string(format!("{outbox}.json")).unwrap()).unwrap();
    assert_eq!(
        (meta["person"].as_str(), meta["via"].as_str(), meta["job"].as_str()),
        (Some("sam"), Some("schedule"), Some("daily"))
    );
    let _ = std::fs::remove_dir_all(&outbox);
    let _ = std::fs::remove_file(format!("{outbox}.json"));
}

#[test]
fn a_program_without_the_token_keeps_kith_sched_run_turn() {
    let lab = Lab::new();
    lab.program("\"launch\"", "echo the-launch-must-not-run > /dev/null; exit 1");
    lab.script("k/bin/kith-sched", &format!("echo \"$@\" > {0}/argv; cat > {0}/stdin", lab.dir.display()));
    let hub = hub();
    assert_eq!(Live(lab.writer()).cron(&hub, &fired("kai", true), "daily"), Started::Ran);
    lab.until("the report", || !lab.calls("hostclaim.report").is_empty());
    assert_eq!(lab.read("argv").trim(), "run-turn house daily --turn 7 --claim 9");
    let spec: Value = serde_json::from_str(&lab.read("stdin")).unwrap();
    assert_eq!(spec, json!({"prompt": "do it", "settings": {"model": "m1", "person": null}, "person": null}));
    assert!(!lab.folders.logs.join("sched").exists(), "the log is kith-sched's to make");
}

#[test]
fn no_program_at_all_is_the_old_path_too() {
    let lab = Lab::new();
    lab.script("k/bin/kith-sched", &format!("echo \"$@\" > {}/argv", lab.dir.display()));
    let hub = hub();
    assert_eq!(Live(lab.writer()).cron(&hub, &fired("kai", true), "daily"), Started::Ran);
    lab.until("the report", || !lab.calls("hostclaim.report").is_empty());
    assert_eq!(lab.read("argv").trim(), "run-turn house daily --turn 7 --claim 9");
}

#[test]
fn a_launch_that_cannot_start_cancels_the_turn_with_the_reason() {
    let lab = Lab::new();
    lab.program("\"sched-turn\"", "true");
    // The folder the logs go to is a file: the run's log cannot be opened, after the claim.
    std::fs::create_dir_all(&lab.folders.logs).unwrap();
    std::fs::write(lab.folders.logs.join("sched"), "in the way").unwrap();
    let hub = hub();
    let started = Live(lab.writer()).cron(&hub, &fired("kai", true), "daily");
    let Started::Cancelled(why) = started else { panic!("{started:?}") };
    assert!(why.contains("can't open the run's log"), "{why}");
    let cancels = lab.calls("hostclaim.cancel");
    assert_eq!(cancels.len(), 1, "{:?}", lab.log.lock().unwrap());
    assert_eq!((cancels[0]["turn"].as_i64(), cancels[0]["reason"].as_str()), (Some(7), Some(why.as_str())));
    assert!(lab.calls("hostclaim.report").is_empty(), "no process, nothing to report");
}

#[test]
fn a_launch_that_refuses_ends_the_turn_with_the_last_line_it_wrote_and_exit_code_is_not_a_clean_end() {
    let lab = Lab::new();
    lab.program("\"sched-turn\"", "echo 'kith: sched turn: claim 9 is gone (ended); nothing ran' >&2; exit 4");
    let hub = hub();
    assert_eq!(Live(lab.writer()).cron(&hub, &fired("kai", true), "daily"), Started::Ran);
    lab.until("the report", || !lab.calls("hostclaim.report").is_empty());
    let cancel = &lab.calls("hostclaim.cancel")[0];
    assert!(
        cancel["reason"].as_str().unwrap().contains("claim 9 is gone"),
        "the reason is read from the log: {cancel}"
    );
    assert_eq!(lab.calls("hostclaim.report")[0]["kind"], "exit");
}
