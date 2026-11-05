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
        person_was: String::new(),
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
    assert_eq!(run(&p, &fixed_clock(), &args(&["agents"])).code, 2);
    assert_eq!(run(&p, &fixed_clock(), &args(&["tag"])).code, 2);
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
