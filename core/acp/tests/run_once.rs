//! run_once against the fake ACP agent (kernel/tests/fakes/fake-acp-agent).
#![cfg(unix)]

use kith_acp::{Failure, FailureKind, RunSpec, depleted, needs_sign_in, run_once, run_once_spent};
use serde_json::json;
use std::time::Duration;

fn fake() -> String {
    format!(
        "python3 '{}/../../kernel/tests/fakes/fake-acp-agent'",
        env!("CARGO_MANIFEST_DIR")
    )
}

fn spec(env: &[(&str, &str)]) -> RunSpec {
    RunSpec {
        command: fake(),
        cwd: std::env::temp_dir().to_string_lossy().into_owned(),
        dirs: Vec::new(),
        env: env
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        model: None,
        modes: Vec::new(),
        mcp_servers: json!([]),
        task: "say hi".into(),
        timeout: Duration::from_secs(20),
        approve: false,
    }
}

#[tokio::test]
async fn a_run_returns_the_text_stop_reason_usage_and_cost() {
    let out = run_once(spec(&[("FAKE_ACP_COST", "0.0123")]))
        .await
        .unwrap();
    assert_eq!(out.text, "echo: say hi");
    assert_eq!(out.stop_reason.as_deref(), Some("end_turn"));
    assert_eq!(out.session_id, "fake-session-1");
    assert_eq!(out.cost_usd, Some(0.0123));
    let u = out.usage.unwrap();
    assert_eq!((u.input, u.output, u.cache_read), (10, 5, 2));
}

#[tokio::test]
async fn an_engine_that_reports_no_cost_gives_none() {
    assert_eq!(run_once(spec(&[])).await.unwrap().cost_usd, None);
}

#[tokio::test]
async fn the_model_and_mcp_servers_reach_the_session() {
    let log = std::env::temp_dir().join(format!("kith-acp-log-{}", std::process::id()));
    let mut s = spec(&[("FAKE_ACP_LOG", log.to_str().unwrap())]);
    s.model = Some("fast-1".into());
    s.mcp_servers = json!([{"name": "m", "command": "x", "args": [], "env": []}]);
    run_once(s).await.unwrap();
    let text = std::fs::read_to_string(&log).unwrap();
    let _ = std::fs::remove_file(&log);
    assert!(text.contains(r#""modelId": "fast-1""#), "{text}");
    assert!(text.contains(r#""name": "m""#), "{text}");
}

/// The `session/set_mode` ids an agent listing `listed` got for a run asking `modes`.
async fn modes_set(name: &str, listed: &str, modes: &[&str]) -> Vec<String> {
    let log = std::env::temp_dir().join(format!("kith-acp-{name}-{}", std::process::id()));
    let mut s = spec(&[
        ("FAKE_ACP_LOG", log.to_str().unwrap()),
        ("FAKE_ACP_MODES", listed),
    ]);
    s.modes = modes.iter().map(|m| (*m).to_owned()).collect();
    assert_eq!(run_once(s).await.unwrap().text, "echo: say hi");
    let text = std::fs::read_to_string(&log).unwrap();
    let _ = std::fs::remove_file(&log);
    text.lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .filter(|m| m["method"] == "session/set_mode")
        .map(|m| m["params"]["modeId"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn the_session_is_set_to_the_first_asked_mode_the_agent_lists() {
    let listed = "agent,read-only,workspace-write,agent-full-access";
    assert_eq!(
        modes_set("mode-full", listed, &["agent-full-access", "full-access"]).await,
        ["agent-full-access"]
    );
    assert_eq!(
        modes_set(
            "mode-older",
            "auto,full-access",
            &["agent-full-access", "full-access"]
        )
        .await,
        ["full-access"]
    );
}

#[tokio::test]
async fn a_session_whose_agent_lists_none_of_the_modes_runs_as_it_started() {
    assert!(
        modes_set("mode-none", "agent", &["workspace-write"])
            .await
            .is_empty()
    );
    assert!(
        modes_set("mode-unlisted", "", &["workspace-write"])
            .await
            .is_empty()
    );
    assert!(
        modes_set("mode-unasked", "agent,workspace-write", &[])
            .await
            .is_empty()
    );
}

/// The params the agent got in `session/new` for a run with these extra folders.
async fn session_new_params(name: &str, takes_dirs: bool) -> serde_json::Value {
    let log = std::env::temp_dir().join(format!("kith-acp-{name}-{}", std::process::id()));
    let mut env = vec![("FAKE_ACP_LOG", log.to_str().unwrap())];
    if takes_dirs {
        env.push(("FAKE_ACP_DIRS", "1"));
    }
    let mut s = spec(&env);
    s.dirs = vec!["/work/notes".to_owned()];
    run_once(s).await.unwrap();
    let text = std::fs::read_to_string(&log).unwrap();
    let _ = std::fs::remove_file(&log);
    text.lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|m| m["method"] == "session/new")
        .unwrap()["params"]
        .clone()
}

#[tokio::test]
async fn extra_folders_reach_an_agent_that_takes_them() {
    let p = session_new_params("dirs-yes", true).await;
    assert_eq!(p["additionalDirectories"], json!(["/work/notes"]));
}

#[tokio::test]
async fn extra_folders_are_left_out_for_an_agent_that_does_not_say_it_takes_them() {
    let p = session_new_params("dirs-no", false).await;
    assert!(p.get("additionalDirectories").is_none(), "{p}");
}

#[tokio::test]
async fn a_signed_out_engine_is_a_sign_in_need() {
    let e = run_once(spec(&[("FAKE_ACP_MODE", "signin")]))
        .await
        .unwrap_err();
    assert!(needs_sign_in(&e), "{e:#}");
}

#[tokio::test]
async fn a_spent_plan_is_depletion_with_its_reset_time() {
    let e = run_once(spec(&[("FAKE_ACP_MODE", "depleted")]))
        .await
        .unwrap_err();
    assert_eq!(depleted(&e).and_then(|d| d.resets_at), Some(1_790_000_000));
}

#[tokio::test]
async fn an_engine_that_dies_says_why() {
    let e = run_once(spec(&[("FAKE_ACP_MODE", "crash")]))
        .await
        .unwrap_err();
    assert_eq!(
        e.downcast_ref::<Failure>().map(|f| f.kind),
        Some(FailureKind::Exited)
    );
    assert!(format!("{e:#}").contains("out of memory"), "{e:#}");
}

#[tokio::test]
async fn an_engine_that_answers_and_exits_at_once_still_gives_its_answer_usage_and_cost() {
    for _ in 0..5 {
        let out = run_once(spec(&[
            ("FAKE_ACP_MODE", "answer-exit"),
            ("FAKE_ACP_COST", "0.5"),
        ]))
        .await
        .unwrap();
        assert_eq!(out.text, "echo: say hi");
        assert_eq!(out.cost_usd, Some(0.5));
        assert_eq!(out.usage.unwrap().output, 5);
    }
}

#[tokio::test]
async fn a_crash_reports_the_cost_seen_so_far() {
    let (result, spent) = run_once_spent(spec(&[
        ("FAKE_ACP_MODE", "crash"),
        ("FAKE_ACP_COST", "0.25"),
    ]))
    .await;
    assert!(result.is_err());
    assert_eq!(spent, Some(0.25));
    let (_, none) = run_once_spent(spec(&[("FAKE_ACP_MODE", "crash")])).await;
    assert_eq!(none, None);
}

#[tokio::test]
async fn a_timeout_kills_the_engine() {
    let pidfile = std::env::temp_dir().join(format!("kith-acp-pid-{}", std::process::id()));
    let mut s = spec(&[
        ("FAKE_ACP_MODE", "hang"),
        ("FAKE_ACP_PIDFILE", pidfile.to_str().unwrap()),
    ]);
    s.timeout = Duration::from_secs(2);
    let e = run_once(s).await.unwrap_err();
    assert_eq!(
        e.downcast_ref::<Failure>().map(|f| f.kind),
        Some(FailureKind::Timeout),
        "{e:#}"
    );
    let pid: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let _ = std::fs::remove_file(&pidfile);
    // run_once returns only once the agent is stopped: no orphan is left.
    // SAFETY: signal 0 only asks whether the process exists.
    assert_ne!(
        unsafe { libc::kill(pid, 0) },
        0,
        "the engine outlived the run"
    );
}
