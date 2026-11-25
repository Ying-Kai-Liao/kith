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
    let call = |m: &str, p: &Value| fake.call(m, p);
    let terminated = || false;
    let watch = || ();
    let settings = |_: &[String]| (0, String::new(), String::new());
    let dir = std::env::temp_dir().join("kith-ask-unit");
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
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run(&host, &args, &mut out, &mut err);
    (
        code,
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    )
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
