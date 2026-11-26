//! The client against a fake core: what each verb sends and says. The real rows are the core's own tests; the same words against
//! the Python are `kernel/tests/difftest/corpus/ask-*.json`.

use std::cell::RefCell;

use serde_json::{Value, json};

use super::{Host, dumps, run, tail};
use crate::wake::Fail;

type Answers = Vec<(&'static str, Result<Value, String>)>;

struct Fake {
    calls: RefCell<Vec<(String, Value)>>,
    answers: Answers,
}

impl Fake {
    fn new(answers: Answers) -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            answers,
        }
    }

    fn call(&self, method: &str, params: &Value) -> Result<Value, Fail> {
        self.calls
            .borrow_mut()
            .push((method.to_owned(), params.clone()));
        match self.answers.iter().find(|(m, _)| *m == method) {
            Some((_, Ok(v))) => Ok(v.clone()),
            Some((_, Err(why))) => Err(Fail::Refused(why.clone())),
            None => Err(Fail::NoAnswer(format!("no fake for {method}"))),
        }
    }
}

fn go(fake: &Fake, args: &[&str]) -> (i32, String, String) {
    go_in(fake, args, &scratch("plain"))
}

/// A folder of this test's own, so no test reads what another left in `ask.json`.
fn scratch(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("kith-ask-unit-{}-{name}", std::process::id()))
}

fn go_in(fake: &Fake, args: &[&str], dir: &std::path::Path) -> (i32, String, String) {
    with_host(fake, dir, |host| {
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(host, &args, &mut out, &mut err);
        (
            code,
            String::from_utf8_lossy(&out).into_owned(),
            String::from_utf8_lossy(&err).into_owned(),
        )
    })
}

fn with_host<T>(fake: &Fake, dir: &std::path::Path, f: impl FnOnce(&Host) -> T) -> T {
    let call = |m: &str, p: &Value| fake.call(m, p);
    let terminated = || false;
    let watch = || ();
    let settings = |_: &[String]| (0, String::new(), String::new());
    let host = Host {
        call: &call,
        kernel: dir.join("kernel"),
        home: dir.join("home"),
        state: dir.join("state"),
        machine: dir.join("machine"),
        env: vec![("KITH_ASK_ID".to_owned(), String::new())],
        terminated: &terminated,
        watch_term: &watch,
        settings: &settings,
    };
    f(&host)
}

#[test]
fn json_is_written_the_way_python_writes_it() {
    let v = json!({"a": [1, "é"], "b": {"c": null}, "d": []});
    assert_eq!(dumps(&v), r#"{"a": [1, "é"], "b": {"c": null}, "d": []}"#);
}

#[test]
fn tail_counts_characters() {
    assert_eq!(tail("héllo", 3), "llo");
    assert_eq!(tail("ab", 9), "ab");
}

#[test]
fn an_answer_says_woke_a_note_says_kept_and_a_repeat_says_duplicate() {
    let said = |row: Value, args: &[&str]| go(&Fake::new(vec![("ask.post", Ok(row))]), args);
    let a = ["--answer", "a1", "--", "the", "answer"];
    assert_eq!(
        said(json!({"id": "a1", "row": 1}), &a),
        (0, "woke\n".into(), String::new())
    );
    assert_eq!(said(json!({"id": "a1", "row": null}), &a).1, "duplicate\n");
    assert_eq!(
        said(
            json!({"id": "a1", "row": 2}),
            &["--answer", "a1", "--progress", "--", "x"]
        )
        .1,
        "kept\n"
    );
    assert_eq!(
        said(json!({"id": "a1", "stopped": true}), &a).1,
        "stopped\n"
    );
}

#[test]
fn usage_errors_exit_one_with_the_python_words() {
    let none = Fake::new(vec![]);
    assert_eq!(
        go(&none, &["--answer", "a1"]),
        (
            1,
            String::new(),
            "usage: kith ask --answer ID [--as AGENT] [--progress] -- TEXT\n".into()
        )
    );
    assert_eq!(
        go(&none, &["--to", "b1"]),
        (
            1,
            String::new(),
            "usage: kith ask --to ID \"message\"\n".into()
        )
    );
    assert_eq!(
        go(&none, &["--cap", "1", "2"]).2,
        "usage: kith ask --cap [N|none]\n"
    );
    assert_eq!(
        go(&none, &["--cap-told"]).2,
        "usage: kith ask --cap-told CHAIN\n"
    );
    assert_eq!(
        go(&none, &["echo", "-e", "huge", "--", "t"]).2,
        "kith ask: effort must be one of low, medium, high, xhigh, max\n"
    );
    assert_eq!(go(&none, &["echo"]).0, 1);
}

#[test]
fn wait_with_no_time_and_no_rows_says_not_yet() {
    let fake = Fake::new(vec![(
        "ask.read",
        Ok(json!({"id": "a1", "state": "open", "rows": []})),
    )]);
    assert_eq!(
        go(&fake, &["--wait", "a1", "--timeout", "0"]),
        (
            3,
            String::new(),
            "kith ask: no answer to a1 yet; ask again with: kith ask --wait a1\n".into()
        )
    );
}

#[test]
fn wait_prints_progress_then_the_answer_and_a_failed_run_exits_one() {
    let rows = json!([
        {"n": 1, "as": "echo", "kind": "progress", "text": "working", "at": 1},
        {"n": 2, "as": "echo", "kind": "answer", "text": "done", "at": 2},
    ]);
    let ok = Fake::new(vec![(
        "ask.read",
        Ok(json!({"id": "a1", "state": "answered", "rows": rows})),
    )]);
    let got = go(&ok, &["--wait", "a1", "--timeout", "0"]);
    assert_eq!((got.0, got.1.as_str()), (0, "(progress) working\ndone\n"));
    let bad = Fake::new(vec![(
        "ask.read",
        Ok(json!({"id": "a1", "state": "stopped", "rows": rows})),
    )]);
    assert_eq!(go(&bad, &["--wait", "a1", "--timeout", "0"]).0, 1);
}

#[test]
fn a_cap_refusal_is_exit_four_and_any_other_refusal_exit_one() {
    let capped = Fake::new(vec![(
        "ask.open",
        Err("delegation cost cap: chain c1 has spent US$9.00 of its US$5 cap; ask the user before starting another ask".into()),
    )]);
    let (code, _, err) = go(&capped, &["--bg", "echo", "hi"]);
    assert_eq!(code, 4);
    assert!(
        err.starts_with("kith ask: delegation cost cap: chain c1"),
        "{err}"
    );
    let stopped = Fake::new(vec![(
        "ask.open",
        Err("kith-core: asks: ask p1 was stopped; it cannot open another".into()),
    )]);
    let (code, _, err) = go(&stopped, &["--bg", "echo", "hi"]);
    assert_eq!(code, 1);
    assert_eq!(
        err,
        "kith ask: ask p1 was stopped; it cannot open another\n"
    );
}

#[test]
fn a_bg_ask_prints_its_start_line_and_sends_the_task_whole() {
    let fake = Fake::new(vec![(
        "ask.open",
        Ok(json!({"decision": null, "record": {}})),
    )]);
    let (code, out, _) = go(
        &fake,
        &["--bg", "echo", "-H", "codex", "--", "do", "the", "thing"],
    );
    assert_eq!(code, 0);
    let id = out
        .trim()
        .strip_prefix("started echo in the background as ")
        .and_then(|r| r.split(';').next())
        .unwrap_or_default()
        .to_owned();
    assert!(id.starts_with("bg-"), "{out}");
    assert_eq!(
        out,
        format!("started echo in the background as {id}; its answer: kith ask --wait {id}\n")
    );
    let calls = fake.calls.borrow();
    let (method, p) = &calls[0];
    assert_eq!(method, "ask.open");
    assert_eq!(
        (p["mode"].as_str(), p["task"].as_str(), p["engine"].as_str()),
        (Some("bg"), Some("do the thing"), Some("codex"))
    );
    assert_eq!(p["brief"], "thing");
}

#[test]
fn bg_to_another_computer_is_refused_for_now() {
    let (code, _, err) = go(&Fake::new(vec![]), &["--bg", "echo@mini", "hi"]);
    assert_eq!(code, 1);
    assert!(err.contains("use the foreground form"), "{err}");
}

#[test]
fn bg_carries_model_effort_project_and_the_folder_on_the_row() {
    let fake = Fake::new(vec![(
        "ask.open",
        Ok(json!({"decision": null, "record": {}})),
    )]);
    let (code, _, err) = go(
        &fake,
        &[
            "--bg", "coder", "-m", "opus", "-e", "high", "-H", "codex", "task",
        ],
    );
    assert_eq!((code, err.as_str()), (0, ""));
    let calls = fake.calls.borrow();
    let p = &calls[0].1;
    assert_eq!(
        (
            p["model"].as_str(),
            p["effort"].as_str(),
            p["engine"].as_str(),
            p["project"].as_str()
        ),
        (Some("opus"), Some("high"), Some("codex"), None)
    );
    // The folder the call works in, as an absolute path: the host's run has none of its own.
    let here = std::env::current_dir().expect("cwd");
    assert_eq!(p["folder"].as_str(), here.to_str());
}

#[test]
fn bg_continuing_a_conversation_keeps_its_project_model_and_effort() {
    let root = scratch("kept");
    let dir = root.join("machine");
    std::fs::create_dir_all(&dir).expect("folder");
    let key = format!("coder|{}", std::env::current_dir().expect("cwd").display());
    let kept = json!({ key: {"agent": "coder", "session_id": "s1",
                             "opts": {"-p": "kith", "-m": "opus"}, "dir": "/"} });
    std::fs::write(dir.join("ask.json"), kept.to_string()).expect("file");
    let fake = Fake::new(vec![(
        "ask.open",
        Ok(json!({"decision": null, "record": {}})),
    )]);
    // This call names a new effort; the kept project and model stay, and a new model would win over the kept one.
    let (code, _, err) = go_in(&fake, &["--bg", "coder", "-c", "-e", "low", "next"], &root);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!((code, err.as_str()), (0, ""));
    let calls = fake.calls.borrow();
    let p = &calls[0].1;
    assert_eq!(
        (
            p["project"].as_str(),
            p["model"].as_str(),
            p["effort"].as_str(),
            p["conversation"].as_str()
        ),
        (Some("kith"), Some("opus"), Some("low"), Some("s1"))
    );
}

fn claims(answer: Result<Value, String>) -> Fake {
    Fake::new(vec![("claim.list", answer)])
}

#[test]
fn a_writer_is_known_from_the_cores_claims() {
    let dir = scratch("writer");
    let live = claims(Ok(json!({"live": [{"conversation": "s1", "pid": 1}]})));
    assert!(with_host(&live, &dir, |h| super::fg::writer_live(h, "s1")));
    assert!(!with_host(&live, &dir, |h| super::fg::writer_live(
        h, "other"
    )));
}

#[test]
fn a_core_that_cannot_answer_is_a_writer_but_unswitched_claims_read_the_files() {
    let dir = scratch("claimfiles");
    let _ = std::fs::remove_dir_all(&dir);
    // down, busy, lost, any other refusal: fail closed
    for fake in [
        Fake::new(vec![]),
        claims(Err("kith-core: the core is busy".into())),
    ] {
        assert!(with_host(&fake, &dir, |h| super::fg::writer_live(h, "s1")));
    }
    // claims not switched: the kernel's files are the record (as kith-wake reads them): none, no writer
    let unswitched = claims(Err(
        "kith-core: claim.list: the claims are not switched to the core on this Mac".into(),
    ));
    assert!(!with_host(&unswitched, &dir, |h| super::fg::writer_live(
        h, "s1"
    )));
    // a claim file of a live process (this one): a writer
    let terminal = dir.join("state").join("live").join("terminal");
    std::fs::create_dir_all(&terminal).expect("folder");
    std::fs::write(
        terminal.join("s1.claim"),
        format!("{{\"pid\": {}}}", std::process::id()),
    )
    .expect("file");
    assert!(with_host(&unswitched, &dir, |h| super::fg::writer_live(
        h, "s1"
    )));
    assert!(!with_host(&unswitched, &dir, |h| super::fg::writer_live(
        h, "s2"
    )));
    // a claim file of a process that is gone: no writer
    #[cfg(unix)]
    {
        let mut child = std::process::Command::new("true").spawn().expect("child");
        let dead = child.id();
        child.wait().expect("wait");
        std::fs::write(terminal.join("s3.claim"), format!("{{\"pid\": {dead}}}")).expect("file");
        assert!(!with_host(&unswitched, &dir, |h| super::fg::writer_live(
            h, "s3"
        )));
    }
    // a recorded start that is not this process's (the pid was reused): no writer, when the stamps can be compared
    if let crate::procstart::Probe::Alive(Some(now)) = crate::procstart::probe(std::process::id()) {
        let other = format!("{now}0");
        if crate::procstart::comparable(&other, &now) {
            std::fs::write(
                terminal.join("s4.claim"),
                format!(
                    "{{\"pid\": {}, \"procStart\": \"{other}\"}}",
                    std::process::id()
                ),
            )
            .expect("file");
            assert!(!with_host(&unswitched, &dir, |h| super::fg::writer_live(
                h, "s4"
            )));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ls_orders_same_second_conversations_by_key_not_by_file() {
    let root = scratch("lsties");
    let dir = root.join("machine");
    std::fs::create_dir_all(&dir).expect("folder");
    let row = |at: &str| json!({"agent": "echo", "at": at, "turns": 1, "cost": 0, "last": ""});
    let kept = json!({
        "echo|/z": row("2026-01-01 00:00:01"),
        "echo|/b": row("2026-01-01 00:00:02"),
        "echo|/a": row("2026-01-01 00:00:01"),
        "echo|/n": {"agent": "echo"},
    });
    std::fs::write(dir.join("ask.json"), kept.to_string()).expect("file");
    let none = Fake::new(vec![]);
    let (code, out, _) = go_in(&none, &["ls", "--json"], &root);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(code, 0);
    let got: Value = serde_json::from_str(out.trim()).expect("json");
    let keys: Vec<&str> = got["conversations"]
        .as_array()
        .expect("rows")
        .iter()
        .map(|r| r["key"].as_str().expect("key"))
        .collect();
    assert_eq!(keys, ["echo|/b", "echo|/a", "echo|/z", "echo|/n"]);
}
