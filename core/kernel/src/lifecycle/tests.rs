#![allow(clippy::unwrap_used)]

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

use super::cli;
use super::sweep::Sweep;
use super::transcript::{self, FLOOR_TOKENS};
use crate::threads::core::{Core, CoreErr};
use crate::threads::resolve::NoHost;
use crate::threads::writer::Taker;
use crate::threads::{Ctx, Host};

static N: AtomicU32 = AtomicU32::new(0);

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Self {
        let d = std::env::temp_dir().join(format!(
            "kith-lifecycle-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Self(d)
    }

    fn state(&self) -> PathBuf {
        self.0.join("state")
    }

    fn reg(&self) -> PathBuf {
        self.state().join("live").join("terminal")
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn no_env(_: &str) -> Option<String> {
    None
}

fn ctx<'a>(t: &Tmp, env: &'a dyn Fn(&str) -> Option<String>) -> Ctx<'a> {
    Ctx {
        state: Some(t.state()),
        machine: Some(t.0.join("machine")),
        home: t.0.join("home"),
        env,
    }
}

const SINCE: f64 = 1_767_225_600.0; // 2026-01-01T00:00:00Z

fn turn(text: &str, tokens: i64) -> String {
    json!({"type": "assistant", "timestamp": "2026-01-01T00:00:00.000Z",
           "message": {"content": [{"type": "text", "text": text}], "usage": {"input_tokens": 10, "cache_read_input_tokens": tokens, "cache_creation_input_tokens": 5}}})
    .to_string()
        + "\n"
}

/// A registry entry and its transcript; returns the transcript's path.
fn session(t: &Tmp, sid: &str, kind: &str, transcript: &str) -> PathBuf {
    let tr =
        t.0.join("home/.claude/projects/p")
            .join(format!("{sid}.jsonl"));
    std::fs::create_dir_all(tr.parent().unwrap()).unwrap();
    std::fs::write(&tr, transcript).unwrap();
    std::fs::create_dir_all(t.reg()).unwrap();
    std::fs::write(
        t.reg().join(format!("{sid}.json")),
        json!({"v": 1, "sessionId": sid, "agent": "designer", "cwd": t.0, "transcriptPath": tr, "pid": null, "kind": kind}).to_string(),
    )
    .unwrap();
    tr
}

fn run_sweep(
    c: &Ctx,
    host: &dyn Host,
    only: Option<&str>,
    at_sleep: bool,
) -> Vec<(String, String, String)> {
    let taker = Taker::new(c, None);
    Sweep {
        c,
        host,
        taker: &taker,
        kith_bin: None,
        said: RefCell::new(String::new()),
    }
    .sweep(only, at_sleep)
}

fn decision(t: &Tmp, sid: &str) -> Option<Value> {
    serde_json::from_str(
        &std::fs::read_to_string(
            t.state()
                .join("sessions/decisions")
                .join(format!("{sid}.json")),
        )
        .ok()?,
    )
    .ok()
}

#[test]
fn the_cache_life_is_one_hour_unless_the_env_says_otherwise() {
    let env = |v: &'static str| move |k: &str| (k == "KITH_CACHE_LIFE").then(|| v.to_owned());
    assert_eq!(transcript::cache_life(&no_env), 3600);
    assert_eq!(transcript::cache_life(&env("300")), 300);
    assert_eq!(transcript::cache_life(&env("-5")), 0);
    assert_eq!(transcript::cache_life(&env("soon")), 3600);
    assert_eq!(transcript::cache_life(&env("")), 3600);
}

#[test]
fn decide_follows_rule_five() {
    let t = Tmp::new();
    let write = |name: &str, body: String| {
        let p = t.0.join(name);
        std::fs::write(&p, body).unwrap();
        p
    };
    let big_q = write("bq", turn("shall I go on?", 150_000));
    let big = write("b", turn("done.", 150_000));
    let small = write("s", turn("done.", 100));
    let day = SINCE + 86_400.0;
    let d = |p: &Path, kind: &str, waiting, at, life| {
        transcript::decide(p, kind, waiting, at, life, day)
    };
    assert_eq!(d(&big_q, "terminal", false, None, 3600), Some("compact"));
    assert_eq!(d(&big, "terminal", false, None, 3600), Some("sleep"));
    assert_eq!(d(&big, "terminal", true, None, 3600), Some("compact"));
    assert_eq!(d(&small, "terminal", false, None, 3600), Some("sleep"));
    assert_eq!(d(&small, "ask", false, None, 3600), Some("end"));
    assert_eq!(d(&small, "ask", true, None, 3600), Some("sleep"));
    assert_eq!(d(&big_q, "helper", false, None, 3600), Some("compact"));
    // too early: 0.9 of the life after the last turn
    assert_eq!(
        transcript::decide(&big_q, "terminal", false, None, 3600, SINCE + 3239.0),
        None
    );
    assert_eq!(
        transcript::decide(&big_q, "terminal", false, None, 3600, SINCE + 3240.0),
        Some("compact")
    );
    // a 5-minute cache has no timer: only a caller that names the moment gets an answer
    assert_eq!(d(&big_q, "terminal", false, None, 300), None);
    assert_eq!(
        d(&big_q, "terminal", false, Some(SINCE + 300.0), 300),
        Some("compact")
    );
    assert!(
        transcript::context_tokens(&big) >= FLOOR_TOKENS
            && transcript::context_tokens(&small) < FLOOR_TOKENS
    );
}

#[test]
fn a_compaction_writes_no_turn_and_a_boundary_counts() {
    let t = Tmp::new();
    let p = t.0.join("t.jsonl");
    let boundary = json!({"type": "system", "subtype": "compact_boundary", "timestamp": "2026-01-02T00:00:00.000Z"});
    let summary = json!({"type": "user", "isCompactSummary": true, "timestamp": "2026-01-02T00:00:00.000Z", "message": {"content": "This session is being continued"}});
    let echo = json!({"type": "user", "timestamp": "2026-01-02T00:00:00.000Z", "message": {"content": "<command-name>/compact</command-name>"}});
    std::fs::write(
        &p,
        format!("{}{boundary}\n{summary}\n{echo}\n", turn("hi", 1)),
    )
    .unwrap();
    assert_eq!(transcript::last_turn_at(&p), Some(SINCE));
    assert!(transcript::compacted_since(&p, SINCE));
    assert!(!transcript::compacted_since(&p, SINCE + 100_000.0));
}

#[test]
fn work_still_running_is_outstanding() {
    let t = Tmp::new();
    let p = t.0.join("t.jsonl");
    let lines = [
        json!({"type": "assistant", "timestamp": "2026-01-01T00:00:00Z", "message": {"content": [
            {"type": "tool_use", "id": "a", "name": "Bash"}, {"type": "tool_use", "id": "b", "name": "Task"}, {"type": "tool_use", "id": "c", "name": "Read"}]}}),
        json!({"type": "user", "timestamp": "2026-01-01T00:00:01Z", "message": {"content": [
            {"type": "tool_result", "tool_use_id": "c", "content": "x"},
            {"type": "tool_result", "tool_use_id": "a", "content": [{"text": "running in background with ID: bg1 now"}]}]}}),
    ];
    std::fs::write(
        &p,
        lines
            .iter()
            .map(|l| l.to_string() + "\n")
            .collect::<String>(),
    )
    .unwrap();
    assert_eq!(
        transcript::pending_work(&p),
        [
            "a subagent is running",
            "background shell bg1 has not reported"
        ]
    );
    let note = json!({"type": "user", "timestamp": "2026-01-01T00:00:02Z", "message": {"content": "<task-notification><task-id>bg1</task-id></task-notification>"}});
    let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
    std::io::Write::write_all(&mut f, format!("{note}\n").as_bytes()).unwrap();
    assert_eq!(transcript::pending_work(&p), ["a subagent is running"]);
}

#[test]
fn a_sleeping_session_is_decided_once_and_a_quiet_one_is_not_read_again() {
    let t = Tmp::new();
    session(&t, "s1", "terminal", &turn("done.", 100));
    let c = ctx(&t, &no_env);
    assert_eq!(
        run_sweep(&c, &NoHost, None, false),
        [("s1".to_owned(), "sleep".to_owned(), "asleep".to_owned())]
    );
    assert_eq!(decision(&t, "s1").unwrap()["decision"], "sleep");
    assert_eq!(run_sweep(&c, &NoHost, None, false).len(), 0);
    assert!(
        run_sweep(&c, &NoHost, Some("s1"), false).is_empty(),
        "the same period is not decided twice"
    );
}

#[test]
fn an_ask_with_nothing_to_wait_for_ends_under_the_claim_and_the_claim_is_given_back() {
    let t = Tmp::new();
    session(&t, "s2", "ask", &turn("done.", 100));
    let c = ctx(&t, &no_env);
    assert_eq!(
        run_sweep(&c, &NoHost, Some("s2"), false),
        [("s2".to_owned(), "end".to_owned(), "ended".to_owned())]
    );
    let d = decision(&t, "s2").unwrap();
    assert_eq!(
        (d["decision"].as_str(), d["ended"].as_bool()),
        (Some("end"), Some(true))
    );
    let claims = std::fs::read_dir(t.reg())
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "claim"))
        .count();
    assert_eq!(claims, 0);
    assert_eq!(run_sweep(&c, &NoHost, Some("s2"), false).len(), 0);
}

struct Owned;

impl Host for Owned {
    fn resolve_thread(&self, thread: &str) -> Option<Value> {
        Some(json!({"sessionId": thread}))
    }
}

#[test]
fn a_session_the_host_keeps_a_conversation_for_is_left_to_the_hosts_compactor() {
    let t = Tmp::new();
    session(&t, "s3", "terminal", &turn("done.", 100));
    let c = ctx(&t, &no_env);
    assert_eq!(run_sweep(&c, &Owned, Some("s3"), false).len(), 0);
    assert!(decision(&t, "s3").is_none());
}

#[test]
fn a_second_sweep_at_once_does_nothing() {
    use fs2::FileExt as _;
    let t = Tmp::new();
    session(&t, "s4", "terminal", &turn("done.", 100));
    let c = ctx(&t, &no_env);
    std::fs::create_dir_all(t.state().join("sessions")).unwrap();
    let lock = std::fs::File::create(t.state().join("sessions/lifecycle.lock")).unwrap();
    lock.lock_exclusive().unwrap();
    assert_eq!(run_sweep(&c, &NoHost, None, false).len(), 0);
    assert!(decision(&t, "s4").is_none());
}

/// A core that answers every call with `answer` and keeps what it was asked.
struct Fake {
    answer: Result<Value, CoreErr>,
    calls: RefCell<Vec<(String, Value)>>,
}

impl Core for Fake {
    fn call(&self, method: &str, params: &Value, _: Option<Duration>) -> Result<Value, CoreErr> {
        self.calls
            .borrow_mut()
            .push((method.to_owned(), params.clone()));
        match &self.answer {
            Ok(v) => Ok(v.clone()),
            Err(CoreErr::NoAnswer(w)) => Err(CoreErr::NoAnswer(w.clone())),
            Err(CoreErr::Refused(w)) => Err(CoreErr::Refused(w.clone())),
        }
    }
}

/// Writer mode (the claims are the core's) with a decision to compact and an intent on record; the compaction asks the core for
/// its claim and gives up when it is held.
fn writer_mode_compact(answer: Result<Value, CoreErr>) -> (String, Vec<(String, Value)>, Tmp) {
    let t = Tmp::new();
    session(&t, "s5", "terminal", &turn("shall I go on?", 150_000));
    std::fs::write(t.reg().join("claims.tombstone"), "").unwrap();
    let st = t.state();
    crate::threads::intent::write_decision(
        &st,
        "s5",
        &json!({"decision": "compact", "at": 0, "idleSince": SINCE, "compacted": false}),
    );
    crate::threads::intent::write_intent(&st, "s5", "idle", 1);
    let env = |k: &str| (k == "KITH_CLAUDE").then(|| "/nonexistent/claude".to_owned());
    let c = ctx(&t, &env);
    let core = Fake {
        answer,
        calls: RefCell::new(Vec::new()),
    };
    let taker = Taker::new(&c, Some(&core));
    let sweep = Sweep {
        c: &c,
        host: &NoHost,
        taker: &taker,
        kith_bin: None,
        said: RefCell::new(String::new()),
    };
    let entry = sweep.entries().remove(0);
    let got = sweep.compact(&entry, SINCE, None).unwrap();
    let calls = core.calls.borrow().clone();
    (got, calls, t)
}

#[test]
fn writer_mode_a_claim_the_core_holds_for_another_is_held_and_the_decision_and_intent_stay() {
    let held = json!({"refused": "held", "holder": {"pid": 77, "kind": "terminal"}});
    let (got, calls, t) = writer_mode_compact(Ok(held));
    assert_eq!(got, "held");
    assert_eq!(
        calls.iter().filter(|(m, _)| m == "claim.external").count(),
        1
    );
    let (_, params) = calls.iter().find(|(m, _)| m == "claim.external").unwrap();
    assert_eq!(
        (
            params["conversation"].as_str(),
            params["person"].as_str(),
            params["agent"].as_str()
        ),
        (Some("s5"), Some(""), Some("designer"))
    );
    assert!(
        decision(&t, "s5").is_some(),
        "the decision of the period stays"
    );
    assert!(
        crate::threads::intent::read_intent(&t.state(), "s5").is_some(),
        "the intent stays"
    );
    assert!(
        !t.reg().join("s5.claim").exists(),
        "Writer mode writes no claim file"
    );
}

#[test]
fn writer_mode_a_core_that_does_not_answer_counts_the_session_as_held() {
    // Only because the compaction is the lifecycle owner: another taker would go on under the conversation lock alone.
    let (got, _, t) = writer_mode_compact(Err(CoreErr::NoAnswer("down".to_owned())));
    assert_eq!(got, "held");
    assert!(decision(&t, "s5").is_some());
    assert!(crate::threads::intent::read_intent(&t.state(), "s5").is_some());
}

#[test]
fn writer_mode_a_claim_the_core_gives_runs_the_checks_under_it_and_a_missing_claude_fails_once() {
    let (got, calls, t) = writer_mode_compact(Ok(json!({"claimId": 1})));
    assert!(got.starts_with("failed: "), "{got}");
    // the release after the reap asks nothing: the pid is gone, its start time unreadable, and the core's reaper ends the claim
    assert_eq!(
        calls.iter().filter(|(m, _)| m == "claim.external").count(),
        1
    );
    let d = decision(&t, "s5").unwrap();
    assert!(
        d["failed"]
            .as_str()
            .unwrap()
            .starts_with("[Errno 2] No such file or directory: '/nonexistent/claude'")
            || d["failed"].as_str().unwrap().contains("kith-stub")
    );
    // lifecycle_owner: the taker did not clear the intent
    assert!(crate::threads::intent::read_intent(&t.state(), "s5").is_some());
}

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| (*s).to_owned()).collect()
}

fn say(t: &Tmp, a: &[&str]) -> crate::threads::cli::Out {
    let c = ctx(t, &no_env);
    cli::run(&c, &NoHost, None, None, &args(a))
}

#[test]
fn the_command_line_is_argparses() {
    let t = Tmp::new();
    let err = |a: &[&str], text: &str| {
        let out = say(&t, a);
        assert_eq!((out.code, out.stdout.as_str()), (2, ""));
        assert!(out.stderr.ends_with(text), "{}", out.stderr);
    };
    err(
        &[],
        "kith-lifecycle: error: the following arguments are required: cmd\n",
    );
    err(
        &["frob"],
        "argument cmd: invalid choice: 'frob' (choose from 'sweep', 'status', 'intent')\n",
    );
    err(
        &["intent", "a"],
        "kith-lifecycle intent: error: the following arguments are required: why, pid\n",
    );
    err(
        &["intent", "a", "b", "x"],
        "argument pid: invalid int value: 'x'\n",
    );
    err(
        &["status"],
        "kith-lifecycle status: error: the following arguments are required: session\n",
    );
    // the difftest's hidden verb is no verb of a normal run
    err(
        &["compact", "x"],
        "argument cmd: invalid choice: 'compact' (choose from 'sweep', 'status', 'intent')\n",
    );
    err(
        &["sweep", "--bogus"],
        "kith-lifecycle: error: unrecognized arguments: --bogus\n",
    );
    err(
        &["sweep", "--session"],
        "kith-lifecycle sweep: error: argument --session: expected one argument\n",
    );
    // the unique prefix and `=` are accepted
    let out = say(&t, &["sweep", "--sess=nobody", "--at"]);
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (0, "", "")
    );
    assert!(say(&t, &["-h"]).stdout.starts_with("usage: kith-lifecycle"));
}

#[test]
fn status_of_an_unknown_session_and_an_intent_for_a_known_one() {
    let t = Tmp::new();
    let out = say(&t, &["status", "nosuch", "--json"]);
    assert_eq!(
        out.stdout,
        "{\"session\": \"nosuch\", \"ended\": null, \"decision\": null, \"outstanding\": []}\n"
    );
    session(&t, "s6", "terminal", &turn("done.", 100));
    std::fs::write(
        t.reg().join("s6.json"),
        json!({"sessionId": "s6", "transcriptPath": t.0.join("home/.claude/projects/p/s6.jsonl"), "pid": 4242}).to_string(),
    )
    .unwrap();
    assert_eq!(say(&t, &["intent", "s6", "idle", "4242"]).code, 0);
    let out = say(&t, &["status", "s6"]);
    let v: Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(v["ended"], "idle");
    // an unsafe name writes nothing
    assert_eq!(say(&t, &["intent", "a/b", "idle", "1"]).code, 0);
    assert!(!t.state().join("sessions/intent").join("a").exists());
}

#[test]
fn a_failed_exec_is_told_the_way_python_tells_it() {
    let _one = super::proc::exclusive();
    let (r, w) = super::proc::pipe().unwrap();
    super::proc::close(r);
    super::proc::close(w);
    let p = super::proc::parked(
        &["/nonexistent/claude".into(), "x".into()],
        Path::new("/"),
        &[],
        false,
        None,
    )
    .unwrap();
    super::proc::write_byte(p.go, b'1');
    let said = super::proc::read_to_end(p.err);
    assert_eq!(
        p.why(&said),
        "[Errno 2] No such file or directory: '/nonexistent/claude'"
    );
    assert_eq!(
        super::proc::reap(p.pid, Some(Duration::from_secs(5)), false),
        Some(127)
    );
    super::proc::close(p.go);
    super::proc::close(p.err);
    // closing `go` unwritten: the child exits without running anything
    let q = super::proc::parked(&["/bin/echo".into()], Path::new("/"), &[], false, None).unwrap();
    super::proc::close(q.go);
    assert_eq!(
        super::proc::reap(q.pid, Some(Duration::from_secs(5)), false),
        Some(127)
    );
    super::proc::close(q.err);
}
