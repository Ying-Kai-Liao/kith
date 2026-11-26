use super::*;

fn ok(session: &str, text: &str, usage: Option<Usage>) -> Outcome {
    Outcome {
        text: text.to_owned(),
        stop_reason: Some("end_turn".to_owned()),
        usage,
        cost_usd: None,
        session_id: session.to_owned(),
    }
}

#[test]
fn instructions_first_is_what_harness_py_makes() {
    // harness.py: '<hq-agent-instructions>\n%s\n</hq-agent-instructions>\n\nThe instructions above are who you are and
    // how you work for this whole session. The task:\n\n%s'
    assert_eq!(
        instructions_first("You echo.\n", "hi"),
        "<hq-agent-instructions>\nYou echo.\n\n</hq-agent-instructions>\n\nThe instructions above are who you are and how you work for this whole session. The task:\n\nhi"
    );
}

#[test]
fn a_good_run_prints_the_keys_of_finish() {
    let usage = Usage { input: 10, output: 5, cache_read: 2, cache_write: 0 };
    let done = Done::of("opencode", Ok(ok("s1", "answer", Some(usage))), Some(0.25));
    assert_eq!(
        done.json("opencode"),
        json!({"type": "result", "subtype": "success", "harness": "opencode", "is_error": false, "result": "answer",
               "session_id": "opencode:s1", "num_turns": 1, "total_cost_usd": 0.25,
               "usage": {"input": 10, "output": 5, "cache_read": 2}})
    );
}

#[test]
fn zero_cost_and_no_usage_are_null_and_empty() {
    let done = Done::of("codex", Ok(ok("s1", "", None)), Some(0.0));
    let v = done.json("codex");
    assert_eq!(v["total_cost_usd"], Value::Null);
    assert_eq!(v["usage"], json!({}));
    assert_eq!(v["result"], "");
    assert_eq!(v["is_error"], false);
}

#[test]
fn a_failure_keeps_what_it_spent_and_has_no_session() {
    let done = Done::of("opencode", Err(anyhow!("agent process exited: out of memory")), Some(0.25));
    let v = done.json("opencode");
    assert_eq!(v["is_error"], true);
    assert_eq!(v["subtype"], "error");
    assert_eq!(v["result"], "agent process exited: out of memory");
    assert_eq!(v["session_id"], Value::Null);
    assert_eq!(v["total_cost_usd"], 0.25);
    assert!(v.get("usage_limit").is_none());
    // No session, no ledger row (harness.py's record returned for it).
    assert!(done.row("opencode", "opencode", "hi", chrono::Utc::now()).is_none());
}

#[test]
fn a_spent_plan_says_when_it_renews() {
    let mut done = Done::of("codex", Err(anyhow!("x")), None);
    done.usage_limit = Some(UsageLimit { resets_at: Some(1_790_000_000) });
    assert_eq!(done.json("codex")["usage_limit"], json!({"resets_at": 1_790_000_000}));
    done.usage_limit = Some(UsageLimit { resets_at: None });
    assert_eq!(done.json("codex")["usage_limit"], json!({"resets_at": null}));
}

#[test]
fn the_ledger_row_is_harness_py_records() {
    let usage = Usage { input: 10, output: 5, cache_read: 0, cache_write: 3 };
    let done = Done::of("opencode", Ok(ok("s1", "a", Some(usage))), Some(0.5));
    let at = chrono::DateTime::from_timestamp(1_790_000_000, 123_456_000).unwrap();
    let task = format!("  first\n second\t{}", "x".repeat(200));
    let row = done.row("opencode", "fast-1", &task, at).unwrap();
    assert_eq!(row["id"], "opencode:s1@20260921T141320.123456");
    assert_eq!(row["session"], "opencode:s1");
    assert_eq!(row["model"], "opencode/fast-1");
    assert_eq!(row["cost"], 0.5);
    assert_eq!(
        (row["input"].as_u64(), row["output"].as_u64(), row["cache_write"].as_u64()),
        (Some(10), Some(5), Some(3))
    );
    assert!(row.get("cache_read").is_none());
    let turn = row["turn"].as_str().unwrap();
    assert_eq!(turn.chars().count(), 100);
    assert!(turn.starts_with("first second xxx"));
    // No model given: just the engine.
    assert_eq!(done.row("opencode", "opencode", "t", at).unwrap()["model"], "opencode");
}

#[test]
fn only_this_engines_session_resumes() {
    assert_eq!(resume_id("opencode", Some("opencode:abc")).as_deref(), Some("abc"));
    // Ids can hold colons themselves.
    assert_eq!(resume_id("opencode", Some("opencode:a:b")).as_deref(), Some("a:b"));
    assert_eq!(resume_id("opencode", Some("codex:abc")), None);
    assert_eq!(resume_id("opencode", Some("abc")), None);
    assert_eq!(resume_id("opencode", Some("opencode:")), None);
    assert_eq!(resume_id("opencode", None), None);
    assert_eq!(resume_id("opencode", Some("")), None);
}

#[test]
fn mcp_files_merge_and_a_later_one_wins() {
    let dir = std::env::temp_dir().join(format!("kith-engine-run-mcp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (a, b) = (dir.join("a.json"), dir.join("b.json"));
    std::fs::write(&a, r#"{"mcpServers": {"x": {"command": "one"}, "y": {"command": "y"}}}"#).unwrap();
    std::fs::write(&b, r#"{"mcpServers": {"x": {"command": "two"}}}"#).unwrap();
    let missing = dir.join("none.json");
    let merged = merge_mcp(&format!("{}\n\n{}\n{}", a.display(), missing.display(), b.display()));
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(Value::Object(merged), json!({"x": {"command": "two"}, "y": {"command": "y"}}));
}

#[test]
fn dirs_are_real_paths_once_each() {
    let dir = std::env::temp_dir().join(format!("kith-engine-run-dirs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let real = plain(&std::fs::canonicalize(&dir).unwrap().to_string_lossy());
    let got = real_dirs(&format!("{}\n\n{}\n{}/.", dir.display(), dir.display(), dir.display()));
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(got, vec![real]);
}

#[test]
fn the_engine_does_not_get_the_runs_own_variables() {
    let names = ["KITH_RUN_PROMPT", "KITH_RUN_MCP", "KITH_MODEL", "PATH", "KITH_RUNNER"].map(str::to_owned);
    assert_eq!(run_vars(names.into_iter()), vec!["KITH_RUN_PROMPT", "KITH_RUN_MCP"]);
}

#[tokio::test]
async fn a_hermes_run_without_bypass_exits_cannot_hold() {
    if !engines::HERMES_REFUSES_WITHOUT_BYPASS {
        return;
    }
    let args = Args {
        engine: "hermes".into(),
        json: false,
        resume: None,
        permission_mode: None,
        interactive: false,
        task: vec!["hi".into()],
    };
    assert_eq!(run(args).await.unwrap(), runonce::CANNOT_HOLD);
}

#[tokio::test]
async fn an_agent_stored_on_the_dropped_dsh_engine_gets_a_clear_refusal() {
    let args = Args {
        engine: "dsh".into(),
        json: false,
        resume: None,
        permission_mode: None,
        interactive: false,
        task: vec!["hi".into()],
    };
    assert!(run(args).await.is_err_and(|e| format!("{e:#}").contains("no engine called dsh")));
}

#[test]
fn only_a_network_flag_of_1_gives_the_sandbox_network() {
    assert!(run_network(Some("1")));
    assert!(!run_network(Some("0")));
    assert!(!run_network(Some("true")));
    // KITH_RUN_OWNER=1 alone leaves KITH_RUN_NETWORK unset, which is no network: the old name is gone.
    assert!(!run_network(None));
}
