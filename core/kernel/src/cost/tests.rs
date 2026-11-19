#![allow(clippy::unwrap_used)]

use super::{Env, Out, Paths, pyfmt, run};
use serde_json::{Value, json};
use std::path::PathBuf;

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| (*s).to_owned()).collect()
}

/// A throwaway machine and home, with one transcript of two requests.
fn world(name: &str) -> (Paths, PathBuf) {
    let root = std::env::temp_dir().join(format!("kith-cost-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (machine, home) = (root.join("machine"), root.join("home"));
    std::fs::create_dir_all(home.join(".claude/projects/-w")).unwrap();
    std::fs::create_dir_all(&machine).unwrap();
    let line = |id: &str, ts: &str, model: &str, i: u64, o: u64| {
        json!({"type": "assistant", "sessionId": "s1", "timestamp": ts, "cwd": "/w",
               "message": {"id": id, "model": model, "usage": {"input_tokens": i, "output_tokens": o}}})
        .to_string()
    };
    let text = [
        line(
            "a",
            "2026-11-03T10:00:00.000Z",
            "claude-sonnet-5",
            1_000_000,
            500_000,
        ),
        "{not json".to_owned(),
        line("b", "2026-11-04T10:00:00.000Z", "gpt-9", 1, 1),
        line(
            "a",
            "2026-11-03T10:00:00.000Z",
            "claude-sonnet-5",
            2_000_000,
            500_000,
        ),
    ]
    .join("\n");
    std::fs::write(home.join(".claude/projects/-w/s1.jsonl"), text).unwrap();
    (
        Paths {
            machine,
            home,
            kernel: root.join("kernel"),
        },
        root,
    )
}

fn fixed_clock() -> Env {
    Env {
        now: Some("2026-11-05T12:00:00+00:00".to_owned()),
        ..Env::default()
    }
}

#[test]
fn fixed_rounds_the_exact_value_half_to_even() {
    assert_eq!(pyfmt::fixed(0.125, 2), "0.12");
    assert_eq!(pyfmt::fixed(0.375, 2), "0.38");
    assert_eq!(pyfmt::fixed(2.5, 0), "2");
    assert_eq!(pyfmt::fixed(12.5, 0), "12");
    assert_eq!(pyfmt::fixed(13.5, 0), "14");
    assert_eq!(pyfmt::fixed(0.1 + 0.2, 4), "0.3000");
    assert_eq!(pyfmt::fixed(9.999_96, 4), "10.0000");
    assert_eq!(pyfmt::fixed(0.0, 4), "0.0000");
    assert_eq!(pyfmt::fixed(21.0, 4), "21.0000");
}

#[test]
fn round_and_commas_read_like_python() {
    assert_eq!(pyfmt::round(30.900_05, 4), 30.9001);
    assert_eq!(pyfmt::round(0.03125, 4), 0.0312);
    assert_eq!(pyfmt::commas(1_234_567.891, 2), "1,234,567.89");
    assert_eq!(pyfmt::commas(999.5, 0), "1,000");
    assert_eq!(pyfmt::commas(12.0, 2), "12.00");
}

#[test]
fn floor_division_and_quoting() {
    assert_eq!(pyfmt::floordiv(7.0, 2.0), 3.0);
    assert_eq!(pyfmt::floordiv(-7.0, 2.0), -4.0);
    assert_eq!(pyfmt::floordiv(-6.0, 2.0), -3.0);
    assert_eq!(pyfmt::shell_quote("tell"), "tell");
    assert_eq!(pyfmt::shell_quote("12.5"), "12.5");
    assert_eq!(pyfmt::shell_quote("a b"), "'a b'");
    assert_eq!(pyfmt::shell_quote("it's $5"), "'it'\"'\"'s $5'");
    assert_eq!(pyfmt::shell_quote(""), "''");
}

#[test]
fn a_request_costs_its_list_price() {
    let u = json!({"input_tokens": 1_000_000, "output_tokens": 500_000, "cache_read_input_tokens": 2_000_000,
                   "cache_creation_input_tokens": 300_000,
                   "cache_creation": {"ephemeral_1h_input_tokens": 100_000, "ephemeral_5m_input_tokens": 200_000}});
    let sonnet5 = super::price(Some("claude-sonnet-5"), &u).unwrap();
    assert!((sonnet5 - 8.3).abs() < 1e-9, "{sonnet5}");
    // the longer key wins (opus-5-5 before opus), fast mode doubles, an unknown model has no price
    let opus55 =
        super::price(Some("claude-opus-5-5"), &json!({"input_tokens": 1_000_000})).unwrap();
    assert!((opus55 - 4.0).abs() < 1e-9);
    let fast = super::price(
        Some("claude-opus-4-1"),
        &json!({"input_tokens": 1_000_000, "speed": "fast"}),
    )
    .unwrap();
    assert!((fast - 10.0).abs() < 1e-9);
    assert_eq!(super::price(Some("gpt-9"), &u), None);
    assert_eq!(super::price(None, &u), None);
    // without a 5m figure the cache writes that are not 1h count as 5m
    let rest = json!({"cache_creation_input_tokens": 1_000_000, "cache_creation": {"ephemeral_1h_input_tokens": 400_000}});
    let cost = super::price(Some("claude-sonnet-5"), &rest).unwrap();
    assert!((cost - (600_000.0 * 2.5 + 400_000.0 * 4.0) / 1e6).abs() < 1e-9);
}

#[test]
fn sync_keeps_the_last_copy_of_a_streamed_message() {
    let (p, root) = world("sync");
    let out = run(&p, &fixed_clock(), &args(&["sync"]));
    assert_eq!(
        out,
        Out {
            stdout: "synced 1 transcripts, 2 requests updated\n".to_owned(),
            stderr: String::new(),
            code: 0
        }
    );
    let again = run(&p, &fixed_clock(), &args(&["sync"]));
    assert_eq!(again.stdout, "synced 1 transcripts, 0 requests updated\n");
    let usage = run(&p, &fixed_clock(), &args(&["usage", "all", "--json"]));
    let v: Value = serde_json::from_str(&usage.stdout).unwrap();
    // 2M in + 0.5M out at sonnet-5: 2*2 + 0.5*10 = 9; gpt-9 has no price
    assert_eq!(v["total"], json!(9.0));
    assert_eq!(v["label"], json!("All time"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn limit_without_a_limit_answers_without_a_ledger() {
    let (p, root) = world("nolimit");
    let json_out = run(&p, &fixed_clock(), &args(&["limit", "alpha", "--json"]));
    assert_eq!(
        json_out.stdout,
        "{\"agent\": \"alpha\", \"limit\": null, \"at\": \"ask\", \"over\": false}\n"
    );
    assert_eq!(
        run(&p, &fixed_clock(), &args(&["limit", "alpha", "--sh"])).stdout,
        ""
    );
    assert!(!p.machine.join("cost.db").exists());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn limit_and_spent_read_the_month() {
    let (p, root) = world("month");
    std::fs::write(
        p.machine.join("settings.json"),
        r#"{"agents": {"alpha": {"limit": 10, "at_limit": "tell"}}}"#,
    )
    .unwrap();
    let c = super::ledger::open(&p).unwrap();
    c.execute_batch(
        "insert into runs (session, agent, via, person) values ('s1', 'alpha', 'terminal', 'sam');",
    )
    .unwrap();
    drop(c);
    let out = run(&p, &fixed_clock(), &args(&["limit", "alpha", "--sh"]));
    assert_eq!(
        out.stdout,
        "l_over=0 l_at=tell l_limit=10 l_spent=9.00 l_text='alpha has used $9.00 of its $10 for November (at API prices)'\n"
    );
    assert_eq!(
        run(&p, &fixed_clock(), &args(&["spent", "sam"])).stdout,
        "9.0000\n"
    );
    assert_eq!(
        run(&p, &fixed_clock(), &args(&["spent", "sam", "beta"])).stdout,
        "0.0000\n"
    );
    let mut with_old = fixed_clock();
    with_old.person_was = "sam".to_owned();
    assert_eq!(
        run(&p, &with_old, &args(&["spent", "nobody"])).stdout,
        "9.0000\n"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_line_the_core_does_not_know_is_usage_or_not_ported() {
    let (p, root) = world("usage");
    let bad = run(&p, &fixed_clock(), &args(&["bogus"]));
    assert_eq!((bad.code, bad.stdout.as_str()), (1, ""));
    assert!(
        bad.stderr
            .starts_with("kith-cost \u{2014} what Claude Code usage costs")
    );
    let help = run(&p, &fixed_clock(), &args(&["--help"]));
    assert_eq!(help.code, 0);
    assert_eq!(help.stdout, bad.stderr);
    // `session` alone, and `agent` alone, are no line either
    assert_eq!(run(&p, &fixed_clock(), &args(&["session"])).code, 1);
    assert_eq!(run(&p, &fixed_clock(), &args(&["agent"])).code, 1);
    let _ = std::fs::remove_dir_all(root);
}

fn hook_env(pairs: &[(&str, &str)], stdin: &str) -> Env {
    Env {
        vars: pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        stdin: Some(stdin.to_owned()),
        ..Env::default()
    }
}

/// A `runs` row: session, agent, version, via, person.
type Run = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn runs(p: &Paths) -> Vec<Run> {
    let c = super::ledger::open(p).unwrap();
    let mut st = c
        .prepare("select session, agent, version, via, person from runs order by session")
        .unwrap();
    st.query_map([], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

#[test]
fn tag_writes_the_run_and_never_fails_the_session() {
    let (p, root) = world("tag");
    let env = hook_env(
        &[
            ("KITH_AGENT", "alpha"),
            ("KITH_VIA", ""),
            ("KITH_PERSON", "sam"),
            ("KITH_COST_PERSON", "owner"),
        ],
        r#"{"session_id": "s-new"}"#,
    );
    let out = run(&p, &env, &args(&["tag"]));
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (0, "", "")
    );
    // unset version is NULL, an empty KITH_VIA stays empty, `owner` in KITH_COST_PERSON is nobody
    assert_eq!(
        runs(&p),
        [(
            "s-new".into(),
            Some("alpha".into()),
            None,
            Some(String::new()),
            None
        )]
    );
    // twice is one row
    run(&p, &env, &args(&["tag"]));
    assert_eq!(runs(&p).len(), 1);
    // no agent, no session, empty or odd stdin: nothing written, exit 0, one line at most
    for (vars, stdin) in [
        (vec![], r#"{"session_id": "s-x"}"#),
        (vec![("KITH_AGENT", "alpha")], "{}"),
        (vec![("KITH_AGENT", "alpha")], r#"{"session_id": ""}"#),
        (vec![("KITH_AGENT", "alpha")], ""),
        (vec![("KITH_AGENT", "alpha")], "[1]"),
        (vec![("KITH_AGENT", "alpha")], r#"{"session_id": [1]}"#),
    ] {
        let out = run(&p, &hook_env(&vars, stdin), &args(&["tag"]));
        assert_eq!(out.code, 0, "{stdin}");
        assert_eq!(runs(&p).len(), 1, "{stdin}");
    }
    let bad = run(
        &p,
        &hook_env(&[("KITH_AGENT", "a")], "{nope"),
        &args(&["tag"]),
    );
    assert_eq!(
        bad.stderr,
        "kith-cost tag: Expecting property name enclosed in double quotes: line 1 column 2 (char 1)\n"
    );
    let list = run(
        &p,
        &hook_env(&[("KITH_AGENT", "a")], "[1]"),
        &args(&["tag"]),
    );
    assert_eq!(
        list.stderr,
        "kith-cost tag: 'list' object has no attribute 'get'\n"
    );
    // a malformed clock does not stop the hook
    let mut clock = hook_env(&[("KITH_AGENT", "a")], r#"{"session_id": "s-2"}"#);
    clock.now = Some("garbage".to_owned());
    assert_eq!(run(&p, &clock, &args(&["tag"])).code, 0);
    assert_eq!(runs(&p).len(), 2);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn record_writes_one_request_and_keeps_the_first_run_row() {
    let (p, root) = world("record");
    let row = r#"{"id": "r1", "session": "s-r", "model": "gpt-5", "input": 1.5, "cost": null}"#;
    let env = hook_env(&[("KITH_AGENT", "codex-agent")], row);
    assert_eq!(run(&p, &env, &args(&["record"])).code, 0);
    let again = hook_env(&[("KITH_AGENT", "other")], row);
    assert_eq!(run(&p, &again, &args(&["record"])).code, 0);
    let c = super::ledger::open(&p).unwrap();
    let (n, input, cost): (i64, f64, Option<f64>) = c
        .query_row("select count(*), max(input), max(cost) from req", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!((n, input, cost), (1, 1.5, None));
    // insert or ignore: the session keeps its first agent
    assert_eq!(runs(&p)[0].1.as_deref(), Some("codex-agent"));
    // an object value is no SQL value: the whole record is refused, nothing is written
    let bad = hook_env(&[], r#"{"id": "r2", "session": "s-q", "model": {"a": 1}}"#);
    let out = run(&p, &bad, &args(&["record"]));
    assert_eq!(out.code, 0);
    assert!(out.stderr.starts_with("kith-cost record: "));
    let n: i64 = c
        .query_row("select count(*) from req", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
    // a missing id is a KeyError's text
    let none = run(
        &p,
        &hook_env(&[], r#"{"session": "s"}"#),
        &args(&["record"]),
    );
    assert_eq!(none.stderr, "kith-cost record: 'id'\n");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn csv_quotes_like_python_and_ends_lines_with_crlf() {
    let (p, root) = world("csv");
    let row = r#"{"id": "r1", "session": "s-r", "turn": "a,b \"c\"", "cost": 2}"#;
    run(&p, &hook_env(&[], row), &args(&["record"]));
    let out = run(&p, &Env::default(), &args(&["csv"]));
    assert!(out.stdout.starts_with("id,session,ts,project,model,agent,turn,input,output,cache_read,cache_write,cost,hq_agent,version,via,effort,person\r\n"));
    assert!(out.stdout.contains(",\"a,b \"\"c\"\"\",0,0,0,0,2.0,"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_same_message_in_two_files_ends_as_the_later_path() {
    // Python's glob order is the file system's, so no golden pins this; the core reads in path order.
    let (p, root) = world("dup");
    let line = |session: &str, i: u64| {
        json!({"type": "assistant", "sessionId": session, "timestamp": "2026-11-04T05:00:00.000Z",
               "message": {"id": "dup", "model": "claude-sonnet-5", "usage": {"input_tokens": i}}})
        .to_string()
    };
    for (dir, session) in [("-w1", "first"), ("-w2", "second")] {
        std::fs::create_dir_all(p.home.join(".claude/projects").join(dir)).unwrap();
        std::fs::write(
            p.home.join(".claude/projects").join(dir).join("s.jsonl"),
            line(session, 1),
        )
        .unwrap();
    }
    run(&p, &fixed_clock(), &args(&["sync"]));
    let c = super::ledger::open(&p).unwrap();
    let who: String = c
        .query_row("select session from req where id = 'dup'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(who, "second");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_tiny_pace_never_reaches_the_limit() {
    // The Python raises OverflowError here (days out of range); the core answers "stays under".
    let at = chrono::DateTime::parse_from_rfc3339("2026-11-01T10:30:00+00:00").unwrap();
    let now = super::Now::at(at);
    let days = [(at.date_naive(), 0.00012)];
    let limit = serde_json::Number::from(100);
    let f = super::views::forecast(&now, &days, 0.00012, Some(&limit));
    assert_eq!(
        f["text"],
        json!("About $0.00 a day since Nov 1, so it stays under $100 in November.")
    );
    assert_eq!(f["reach"]["100"], Value::Null);
    assert_eq!(f["monthEnd"], json!("2026-11-30"));
}

#[test]
fn the_clock_takes_epoch_seconds_and_iso_text() {
    let epoch = super::Now::from_env(Some("1793880000")).unwrap();
    assert_eq!(epoch.0.timestamp(), 1_793_880_000);
    let iso = super::Now::from_env(Some(" 2026-11-05T12:00:00+10:00 ")).unwrap();
    assert_eq!(iso.0.timestamp(), 1_793_844_000);
    assert!(super::Now::from_env(Some("not a time")).is_err());
    // blank is the real clock
    assert!(super::Now::from_env(Some("  ")).is_ok());
}

/// A kernel folder whose `bin/kith-auth` prints `body` and exits `code`, whatever it is asked.
#[cfg(unix)]
fn kernel_with_auth(p: &Paths, code: i32, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let bin = p.kernel.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let f = bin.join("kith-auth");
    std::fs::write(
        &f,
        format!("#!/bin/sh\nprintf '%s' '{body}'\nexit {code}\n"),
    )
    .unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[test]
fn members_says_when_the_budgets_cannot_be_read() {
    let (p, root) = world("members-budgets");
    let members = |p: &Paths| -> Value {
        serde_json::from_str(&run(p, &fixed_clock(), &args(&["members", "alpha"])).stdout).unwrap()
    };
    kernel_with_auth(&p, 0, r#"{"sam": 5.0}"#);
    let ok = members(&p);
    assert_eq!(ok["budgets"], json!({"sam": 5.0}));
    assert!(ok.get("budgetsError").is_none());
    // A kith-auth that fails or answers text is not "nobody has a budget"; nor is a tombstone (exit 3) when the core
    // can't be asked from here.
    for (code, body) in [(1, ""), (0, "not json"), (3, "")] {
        kernel_with_auth(&p, code, body);
        let bad = members(&p);
        assert_eq!(bad["budgets"], json!({}), "exit {code}");
        assert_eq!(
            bad["budgetsError"],
            json!("can't read budgets"),
            "exit {code}"
        );
    }
    let _ = std::fs::remove_dir_all(root);
}
