#![allow(
    clippy::unwrap_used,
    clippy::assert_is_empty,
    clippy::needless_pass_by_value,
    clippy::format_collect
)]
//! The host's paths in the crate (`terminal`, `listing`) and the wrapper (`run`, `recent`, `idle`, `clock`). The scenarios of
//! the first two are those of the host's own `terminal::tests` (the same names, in the host's words); the host side of the
//! check, the host's code and these side by side on one fixture, is `serve/host/src/terminal/kernel_twin.rs`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::resolve::NoHost;
use super::tests::{Sleeper, Tmp, ctx, dead_pid, me, no_env};
use super::*;
use crate::threads::terminal::{Stopped, holders_of, stop_holder, sweep, writer_holder};
use crate::threads::{clock, idle, listing, recent};

const GRACE: Duration = Duration::from_millis(300);

fn shell_with_child() -> (Child, i64) {
    use std::io::BufRead as _;
    let mut sh = Command::new("sh")
        .args(["-c", "sleep 300 & echo $!; wait"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    std::io::BufReader::new(sh.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    (sh, line.trim().parse().unwrap())
}

fn gone(pid: i64) -> bool {
    for _ in 0..40 {
        if !os::pid_runs(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn take_as(c: &Ctx, sid: &str, kind: &str, pid: i64, person: Option<&str>, takeover: bool) {
    take_claim(
        c,
        sid,
        pid,
        &Take {
            person,
            takeover,
            ..Take::new(kind)
        },
    )
    .unwrap()
    .unwrap();
}

// --- terminal: the holder, the sweep, the stop -------------------------------------------------------------------------

#[test]
fn a_host_turns_own_claim_does_not_stop_the_hosts_sends() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let sleeper = Sleeper::new();
    take_as(&c, "s-1", "app", sleeper.pid(), None, false);
    assert!(writer_holder(&t.state(), "s-1").is_none());
    // The crate's plain holder counts it (the CLI's rule); the host's rule is this module's.
    assert!(live_holder(&t.reg(), "s-1").is_some());
    take_as(&c, "s-2", "terminal", sleeper.pid(), None, false);
    assert!(writer_holder(&t.state(), "s-2").is_some());
    take_as(&c, "s-3", "app", sleeper.pid(), None, true);
    assert!(
        writer_holder(&t.state(), "s-3").is_some(),
        "a take-over's claim is one"
    );
    for kind in ["bot", "oneshot"] {
        take_as(&c, &format!("k-{kind}"), kind, sleeper.pid(), None, false);
        assert!(writer_holder(&t.state(), &format!("k-{kind}")).is_some());
    }
    t.put("old.claim", &format!("{{\"pid\": {}}}", dead_pid()));
    assert!(writer_holder(&t.state(), "old").is_none());
    assert!(writer_holder(&t.state(), "nothing").is_none());
}

#[test]
fn the_sweep_ends_a_left_over_guest_holder_and_its_child_and_keeps_the_owners() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let (mut guest, guest_child) = shell_with_child();
    let (owner, other_guest) = (Sleeper::new(), Sleeper::new());
    let gpid = i64::from(guest.id());
    take_as(&c, "g1", "oneshot", gpid, Some("bot-sam"), false);
    take_as(&c, "o1", "terminal", owner.pid(), None, false);
    take_as(&c, "x1", "oneshot", other_guest.pid(), Some("ann"), false);
    let persons = ["bot-sam".to_owned(), "sam".to_owned()];
    assert_eq!(holders_of(&t.state(), &persons).len(), 1);
    let ended = sweep(&t.state(), &persons, &|_| false, GRACE);
    assert_eq!(ended, [gpid]);
    guest.wait().unwrap();
    assert!(gone(guest_child), "what the holder started goes with it");
    assert!(
        os::pid_runs(owner.pid()),
        "the owner's claim is never touched"
    );
    assert!(
        os::pid_runs(other_guest.pid()),
        "another person's holder is left alone"
    );
    assert!(read_claim(&claim_path(&t.reg(), "g1", false)).is_none());
    assert!(read_claim(&claim_path(&t.reg(), "o1", false)).is_some());
    // The revoke wrote its intent before the signal (F12 rule 8), and nothing for the sessions it left alone.
    let why = |sid: &str| {
        intent::read_intent(&t.state(), sid).and_then(|m| m["why"].as_str().map(str::to_owned))
    };
    assert_eq!(why("g1").as_deref(), Some("access revoked"));
    assert_eq!((why("o1"), why("x1")), (None, None));
    assert!(sweep(&t.state(), &["bot-sam".to_owned()], &|_| false, GRACE).is_empty());
}

#[test]
fn a_narrowing_sweep_ends_only_the_holders_on_agents_the_person_lost() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let (keeps, loses) = (Sleeper::new(), Sleeper::new());
    for (sid, pid, agent) in [("k1", keeps.pid(), "coder"), ("l1", loses.pid(), "writer")] {
        take_claim(
            &c,
            sid,
            pid,
            &Take {
                person: Some("sam"),
                agent: Some(agent),
                ..Take::new("oneshot")
            },
        )
        .unwrap()
        .unwrap();
    }
    let ended = sweep(
        &t.state(),
        &["sam".to_owned()],
        &|c| c.agent.as_deref() == Some("coder"),
        GRACE,
    );
    assert_eq!(ended, [loses.pid()]);
    assert!(os::pid_runs(keeps.pid()));
    let mut loses = loses;
    loses.0.wait().unwrap();
}

#[test]
fn the_sweep_leaves_a_reused_pid_alone_and_ends_a_proc_claim() {
    let t = Tmp::new();
    let stranger = Sleeper::new();
    // A live pid that started at another time than the claim says is not the process that took the claim.
    t.put(
        "g2.claim",
        &json!({"pid": stranger.pid(), "procStart": "Mon Jan  1 00:00:00 2001", "at": "t", "kind": "oneshot", "person": "sam"})
            .to_string(),
    );
    assert!(sweep(&t.state(), &["sam".to_owned()], &|_| false, GRACE).is_empty());
    assert!(os::pid_runs(stranger.pid()));
    t.put(
        "g2.claim",
        &json!({"pid": dead_pid(), "at": "t", "kind": "oneshot", "person": "sam"}).to_string(),
    );
    assert!(sweep(&t.state(), &["sam".to_owned()], &|_| false, GRACE).is_empty());
    // A person's warm process between turns: a proc claim, which names no session (no intent) and is given back.
    let warm = Sleeper::new();
    proc_claim(&ctx(&t, &no_env), warm.pid(), "sam", "coder").unwrap();
    let ended = sweep(&t.state(), &["sam".to_owned()], &|_| false, GRACE);
    assert_eq!(ended, [warm.pid()]);
    assert!(!claim_path(&t.reg(), &format!("proc-{}", warm.pid()), false).exists());
    assert!(!t.state().join("sessions").join("intent").exists());
    let mut warm = warm;
    warm.0.wait().unwrap();
}

#[test]
fn a_takeover_holder_is_swept_and_its_own_file_given_back() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let h = Sleeper::new();
    take_as(&c, "tk", "app", h.pid(), Some("sam"), true);
    assert!(claim_path(&t.reg(), "tk", true).exists());
    assert_eq!(
        sweep(&t.state(), &["sam".to_owned()], &|_| false, GRACE),
        [h.pid()]
    );
    assert!(!claim_path(&t.reg(), "tk", true).exists());
    // Its intent names the session, not the take-over file.
    assert!(intent::read_intent(&t.state(), "tk").is_some());
    let mut h = h;
    h.0.wait().unwrap();
}

#[test]
fn stopping_a_holder_writes_its_intent_first_and_gives_the_claim_back() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    assert_eq!(stop_holder(&t.state(), "h1", GRACE), Stopped::Nobody);
    // The holder is the shell's child, so it is not this process' own turn.
    let (mut shell, held) = shell_with_child();
    take_as(&c, "h1", "terminal", held, None, false);
    assert!(matches!(
        stop_holder(&t.state(), "h1", GRACE),
        Stopped::Ended(_)
    ));
    shell.wait().unwrap();
    let why = intent::read_intent(&t.state(), "h1").unwrap();
    assert_eq!(why["why"], "stopped from the app");
    assert!(!claim_path(&t.reg(), "h1", false).exists());
    // The host's own turn (its child holds the claim) is not stopped from here.
    let own = Sleeper::new();
    take_as(&c, "h2", "terminal", own.pid(), None, false);
    assert!(matches!(
        stop_holder(&t.state(), "h2", GRACE),
        Stopped::OwnTurn(_)
    ));
    assert!(os::pid_runs(own.pid()));
    assert!(claim_path(&t.reg(), "h2", false).exists());
    // A turn's claim of the host's (`app`, no take-over) is the host's own; a stale one is nobody's.
    t.put("h3.claim", &format!("{{\"pid\": {}}}", dead_pid()));
    assert_eq!(stop_holder(&t.state(), "h3", GRACE), Stopped::Nobody);
    let _ = me();
}

// --- listing: the registry the host reads ------------------------------------------------------------------------------

fn base_entry() -> Value {
    json!({
        "v": 1, "sessionId": "abc-1", "agent": "builder", "cwd": "/w", "transcriptPath": "/p/abc-1.jsonl",
        "pid": 7, "project": null, "source": "startup", "resumedFrom": null, "startedAt": "2026-10-23T01:00:00Z",
    })
}

#[test]
fn a_registry_file_is_only_read_when_it_is_a_listable_sessions() {
    let ok = base_entry();
    assert!(listing::parse(&ok.to_string()).is_some());
    let with = |k: &str, v: Value| {
        let mut x = ok.clone();
        x[k] = v;
        x.to_string()
    };
    assert!(listing::parse("not json").is_none());
    assert!(listing::parse("[]").is_none());
    assert!(listing::parse(&with("person", json!("sam"))).is_none());
    assert!(listing::parse(&with("personId", json!("sam"))).is_none());
    assert!(listing::parse(&with("person", Value::Null)).is_some());
    assert!(listing::parse(&with("v", json!(2))).is_none());
    assert!(listing::parse(&with("v", json!(1.0))).is_none());
    assert!(listing::parse(&with("sessionId", json!("../x"))).is_none());
    assert!(listing::parse(&with("transcriptPath", json!("/p/other.jsonl"))).is_none());
    assert!(listing::parse(&with("cwd", json!("relative"))).is_none());
    assert!(listing::parse(&with("agent", json!(""))).is_none());
    assert!(listing::parse(&with("kind", json!("serve"))).is_none());
    assert!(listing::parse(&with("kind", Value::Null)).is_none());
    assert!(listing::parse(&with("pid", json!(1))).is_none());
    assert!(listing::parse(&with("pid", json!(-4))).is_none());
    assert!(listing::parse(&with("pid", json!(me()))).is_none());
    assert!(listing::parse(&with("pid", Value::Null)).is_none());
    assert!(listing::parse(&with("project", json!(3))).is_none());
    let r = listing::parse(&with("kind", json!("ask"))).unwrap();
    assert!(r.one_shot());
    let r = listing::parse(&ok.to_string()).unwrap();
    assert_eq!((r.kind.as_str(), r.one_shot()), ("terminal", false));
    assert_eq!(r.started_ms(), 1_792_717_200_000);
    let mut gone = ok.clone();
    gone.as_object_mut().unwrap().remove("sessionId");
    assert!(listing::parse(&gone.to_string()).is_none());
}

fn register(t: &Tmp, sid: &str, pid: i64, started: &str, extra: Value) {
    let mut v = json!({
        "v": 1, "sessionId": sid, "agent": "builder", "cwd": t.0.to_string_lossy(),
        "transcriptPath": t.0.join(format!("{sid}.jsonl")).to_string_lossy(), "pid": pid, "project": null,
        "source": "startup", "resumedFrom": null, "startedAt": started,
    });
    for (k, x) in extra.as_object().unwrap() {
        v[k] = x.clone();
    }
    t.put(&format!("{sid}.json"), &v.to_string());
}

#[test]
fn every_readable_file_is_listed_oldest_first_and_big_or_damaged_ones_are_skipped() {
    let t = Tmp::new();
    let me_pid = Sleeper::new();
    register(
        &t,
        "b-2",
        me_pid.pid(),
        "2026-10-23T02:00:00+00:00",
        json!({}),
    );
    register(
        &t,
        "a-1",
        me_pid.pid(),
        "2026-10-23T01:00:00.250Z",
        json!({}),
    );
    register(&t, "c-3", me_pid.pid(), "garbage", json!({}));
    t.put("d-4.json", "{");
    t.put("e-5.json", &" ".repeat(70_000));
    t.put("f-6.txt", &base_entry().to_string());
    let ids: Vec<String> = listing::read_all(&t.state())
        .into_iter()
        .map(|r| r.session_id)
        .collect();
    assert_eq!(ids, ["c-3", "a-1", "b-2"], "no usable start sorts first");
    assert_eq!(
        listing::read_one(&t.state(), "a-1").unwrap().session_id,
        "a-1"
    );
    assert!(listing::read_one(&t.state(), "e-5").is_none());
    assert!(listing::read_one(&t.state(), "none").is_none());
    assert!(listing::read_all(&t.0.join("nowhere")).is_empty());
}

fn session_file(dir: &Path, pid: i64, sid: &str, status: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join(format!("{pid}.json")),
        json!({"pid": pid, "sessionId": sid, "status": status}).to_string(),
    )
    .unwrap();
}

#[test]
fn live_needs_the_process_and_a_session_file_that_names_the_session() {
    let t = Tmp::new();
    let sleeper = Sleeper::new();
    let sessions = t.0.join("sessions");
    register(
        &t,
        "sess-1",
        sleeper.pid(),
        "2026-10-23T01:00:00Z",
        json!({}),
    );
    let reg = listing::read_all(&t.state()).pop().unwrap();
    let live = |r: &listing::Registry| listing::liveness(r, Some(&sessions));
    assert_eq!(live(&reg), listing::Live::Closed, "no session file");
    session_file(&sessions, sleeper.pid(), "sess-1", "idle");
    assert_eq!(live(&reg), listing::Live::Idle);
    session_file(&sessions, sleeper.pid(), "sess-1", "busy");
    assert_eq!(live(&reg), listing::Live::Busy);
    session_file(&sessions, sleeper.pid(), "sess-1", "something-new");
    assert_eq!(
        live(&reg),
        listing::Live::Busy,
        "an unknown state counts as working"
    );
    session_file(&sessions, sleeper.pid(), "other-session", "idle");
    assert_eq!(
        live(&reg),
        listing::Live::Closed,
        "a reused pid names another session"
    );
    assert!(listing::Live::Idle.is_live() && !listing::Live::Closed.is_live());
    let dead = dead_pid();
    register(&t, "sess-1", dead, "2026-10-23T01:00:00Z", json!({}));
    session_file(&sessions, dead, "sess-1", "idle");
    let reg = listing::read_all(&t.state()).pop().unwrap();
    assert_eq!(live(&reg), listing::Live::Closed, "the process is gone");
}

#[test]
fn a_session_file_started_at_another_time_is_not_the_session_but_utc_or_local_is() {
    let t = Tmp::new();
    let sleeper = Sleeper::new();
    let sessions = t.0.join("sessions");
    register(
        &t,
        "sess-1",
        sleeper.pid(),
        "2026-10-23T01:00:00Z",
        json!({}),
    );
    let reg = listing::read_all(&t.state()).pop().unwrap();
    let write = |start: Option<&str>| {
        fs::create_dir_all(&sessions).unwrap();
        fs::write(
            sessions.join(format!("{}.json", sleeper.pid())),
            json!({"pid": sleeper.pid(), "sessionId": "sess-1", "status": "idle", "procStart": start}).to_string(),
        )
        .unwrap();
    };
    let live = || listing::liveness(&reg, Some(&sessions));
    write(Some("Mon Jan  1 00:00:00 2001"));
    assert_eq!(live(), listing::Live::Closed);
    write(os::proc_start(sleeper.pid()).as_deref());
    assert_eq!(live(), listing::Live::Idle, "the Mac's local time");
    write(os::proc_start_utc(sleeper.pid()).as_deref());
    assert_eq!(live(), listing::Live::Idle, "UTC, as Claude Code writes it");
    write(None);
    assert_eq!(live(), listing::Live::Idle, "no start to compare: believed");
}

#[test]
fn a_one_shot_without_a_session_file_is_working_while_its_process_runs_and_only_then() {
    let t = Tmp::new();
    let sleeper = Sleeper::new();
    let start = os::proc_start(sleeper.pid()).unwrap();
    let sessions = t.0.join("sessions");
    let reg_with = |kind: &str, proc_start: Value| {
        register(
            &t,
            "ask-1",
            sleeper.pid(),
            "2026-10-23T01:00:00Z",
            json!({"kind": kind, "procStart": proc_start}),
        );
        listing::read_one(&t.state(), "ask-1").unwrap()
    };
    let live = |r: &listing::Registry| listing::liveness(r, Some(&sessions));
    assert_eq!(live(&reg_with("ask", json!(start))), listing::Live::Busy);
    assert_eq!(
        live(&reg_with("ask", json!("Mon Jan  1 00:00:00 2001"))),
        listing::Live::Closed,
        "a pid reused after a crash"
    );
    assert_eq!(live(&reg_with("ask", Value::Null)), listing::Live::Closed);
    assert_eq!(
        live(&reg_with("terminal", json!(start))),
        listing::Live::Closed,
        "a terminal needs its session file"
    );
}

#[test]
fn the_session_file_of_the_login_a_session_was_made_under_counts() {
    let t = Tmp::new();
    let sleeper = Sleeper::new();
    let cfg = t.0.join("acct");
    let reg = listing::parse(
        &json!({
            "v": 1, "sessionId": "w-1", "agent": "builder", "cwd": "/w",
            "transcriptPath": cfg.join("projects/-w/w-1.jsonl"), "pid": sleeper.pid(),
        })
        .to_string(),
    )
    .unwrap();
    assert_eq!(listing::liveness(&reg, None), listing::Live::Closed);
    session_file(&cfg.join("sessions"), sleeper.pid(), "w-1", "busy");
    assert_eq!(listing::liveness(&reg, None), listing::Live::Busy);
    let home = t.0.join("home");
    let env = |k: &str| (k == "CLAUDE_CONFIG_DIR").then(|| "/c".to_owned());
    assert_eq!(
        listing::default_sessions_dir(&env, &home),
        PathBuf::from("/c/sessions")
    );
    assert_eq!(
        listing::default_sessions_dir(&no_env, &home),
        home.join(".claude/sessions")
    );
}

// --- clock -------------------------------------------------------------------------------------------------------------

#[test]
fn dates_read_as_python_and_the_host_read_them() {
    let at = |t: &str| clock::parse(t).map(clock::Stamp::epoch);
    assert_eq!(at("1970-01-01T00:00:00Z"), Some(0.0));
    assert_eq!(at("2026-11-03T04:05:06Z"), Some(1_793_678_706.0));
    assert_eq!(at("2026-11-03T04:05:06.5+01:00"), Some(1_793_675_106.5));
    assert_eq!(at("2026-11-03 04:05:06-0230"), Some(1_793_687_706.0));
    assert_eq!(at("2026-11-03"), Some(1_793_664_000.0));
    assert_eq!(at("2024-02-29T00:00:00Z"), Some(1_709_164_800.0));
    for bad in [
        "",
        "None",
        "2026-13-01",
        "2026-02-30",
        "2026-11-03T25:00:00Z",
        "2026-11-03T04",
        "2026-11-03T04:05:06+1",
        "x",
    ] {
        assert_eq!(at(bad), None, "{bad}");
    }
    assert_eq!(
        clock::rfc3339_ms("2026-10-23T01:00:00Z"),
        Some(1_792_717_200_000)
    );
    assert_eq!(
        clock::rfc3339_ms("2026-10-23T01:00:00.123456+00:00"),
        Some(1_792_717_200_123)
    );
    assert_eq!(
        clock::rfc3339_ms("2026-10-23T01:00:00"),
        None,
        "RFC 3339 names its offset"
    );
    assert_eq!(clock::rfc3339_ms("2026-10-23"), None);
    // A naive text is the clock reading as UTC (the ledger's `started`).
    assert_eq!(clock::parse("2026-11-03T04:05:06").unwrap().offset, None);
}

// --- idle: the transcript's clock and its outstanding work -------------------------------------------------------------

fn transcript(t: &Tmp, lines: &[Value]) -> PathBuf {
    let p = t.0.join("t.jsonl");
    fs::write(
        &p,
        lines.iter().map(|l| format!("{l}\n")).collect::<String>(),
    )
    .unwrap();
    p
}

fn turn(kind: &str, ts: &str, content: Value) -> Value {
    json!({"type": kind, "timestamp": ts, "message": {"content": content}})
}

#[test]
fn the_idle_clock_is_the_last_real_turn_not_what_a_command_or_a_compaction_writes() {
    let t = Tmp::new();
    let p = transcript(
        &t,
        &[
            turn("user", "2026-11-01T10:00:00Z", json!("hi")),
            turn(
                "assistant",
                "2026-11-01T10:00:05Z",
                json!([{"type": "text", "text": "yo"}]),
            ),
            turn(
                "user",
                "2026-11-01T11:00:00Z",
                json!("<command-name>/compact</command-name>"),
            ),
            json!({"type": "user", "timestamp": "2026-11-01T11:00:01Z", "isCompactSummary": true, "message": {"content": "sum"}}),
            json!({"type": "user", "timestamp": "2026-11-01T11:00:02Z", "isMeta": true, "message": {"content": "m"}}),
            json!({"type": "assistant", "timestamp": "2026-11-01T11:00:03Z", "isSidechain": true, "message": {"content": "s"}}),
            json!({"type": "system", "subtype": "compact_boundary", "timestamp": "2026-11-01T11:00:04Z"}),
        ],
    );
    assert_eq!(idle::last_turn_at(&p), Some(1_793_527_205.0));
    assert_eq!(
        idle::idle_since(Some(&p), 1_793_600_000.0, None),
        1_793_600_000.0
    );
    assert_eq!(
        idle::idle_since(Some(&p), 5.0, Some(1_793_700_000.0)),
        1_793_700_000.0
    );
    assert_eq!(idle::idle_since(None, 5.0, None), 5.0);
    assert_eq!(idle::last_turn_at(&t.0.join("none.jsonl")), None);
}

#[test]
fn work_still_running_in_a_transcript_is_outstanding() {
    let t = Tmp::new();
    let tool = |id: &str, name: &str| {
        turn(
            "assistant",
            "2026-11-01T10:00:00Z",
            json!([{"type": "tool_use", "id": id, "name": name}]),
        )
    };
    let result = |id: &str, body: Value| {
        turn(
            "user",
            "2026-11-01T10:00:01Z",
            json!([{"type": "tool_result", "tool_use_id": id, "content": body}]),
        )
    };
    let p = transcript(
        &t,
        &[
            turn("user", "2026-11-01T10:00:00Z", json!("go")),
            tool("a", "Bash"),
            tool("b", "Agent"),
        ],
    );
    assert_eq!(
        idle::pending_work(&p),
        ["Bash has not returned", "a subagent is running"]
    );
    let p = transcript(
        &t,
        &[
            tool("a", "Bash"),
            result("a", json!("done")),
            tool("b", "Task"),
        ],
    );
    assert_eq!(idle::pending_work(&p), ["a subagent is running"]);
    let p = transcript(
        &t,
        &[
            tool("a", "Bash"),
            result(
                "a",
                json!([{"type": "text", "text": "x running in background with ID: bg_1 y"}]),
            ),
        ],
    );
    assert_eq!(
        idle::pending_work(&p),
        ["background shell bg_1 has not reported"]
    );
    let note = turn(
        "user",
        "2026-11-01T10:00:02Z",
        json!("<task-notification><task-id>bg_1</task-id></task-notification>"),
    );
    let p = transcript(
        &t,
        &[
            tool("a", "Bash"),
            result("a", json!("running in background with ID: bg_1")),
            note,
        ],
    );
    assert!(idle::pending_work(&p).is_empty());
    // An MCP sidecar is outstanding too, and so is a child in another session; an unreadable child is busy.
    assert!(idle::outstanding(Some(&p), None).is_empty());
    let side = t.0.join("t").join("mcp-tasks");
    fs::create_dir_all(&side).unwrap();
    fs::write(side.join("x.meta.json"), "{}").unwrap();
    assert_eq!(idle::outstanding(Some(&p), None), ["an MCP task is open"]);
    assert_eq!(
        idle::outstanding(None, Some(dead_pid())),
        ["a child process runs detached"]
    );
    let plain = Sleeper::new();
    assert!(idle::outstanding(None, Some(plain.pid())).is_empty());
}

#[test]
fn the_sleep_and_cache_settings_read_as_the_lifecycle_does() {
    let env =
        |v: &'static str, k: &'static str| move |name: &str| (name == k).then(|| v.to_owned());
    assert!((idle::sleep_after(&no_env) - 3600.0).abs() < f64::EPSILON);
    assert!((idle::sleep_after(&env("2", "KITH_SLEEP_MINUTES")) - 120.0).abs() < f64::EPSILON);
    assert!((idle::sleep_after(&env("0.001", "KITH_SLEEP_MINUTES")) - 1.0).abs() < f64::EPSILON);
    assert!((idle::sleep_after(&env("soon", "KITH_SLEEP_MINUTES")) - 3600.0).abs() < f64::EPSILON);
    assert_eq!(idle::cache_life(&no_env), 3600);
    assert_eq!(idle::cache_life(&env("300", "KITH_CACHE_LIFE")), 300);
    assert_eq!(idle::cache_life(&env("-5", "KITH_CACHE_LIFE")), 0);
    assert_eq!(idle::cache_life(&env("x", "KITH_CACHE_LIFE")), 3600);
}

// --- recent: pick and latest -------------------------------------------------------------------------------------------

fn ledger(t: &Tmp, rows: &[(&str, &str, &str)]) {
    let machine = t.0.join("machine");
    fs::create_dir_all(&machine).unwrap();
    let db = rusqlite::Connection::open(machine.join("cost.db")).unwrap();
    db.execute_batch(
        "create table runs (session text, agent text, via text, project text, started text, person text);",
    )
    .unwrap();
    for (sid, agent, started) in rows {
        db.execute(
            "insert into runs values (?, ?, 'terminal', 'kith', ?, null)",
            [sid, agent, started],
        )
        .unwrap();
    }
}

fn claude_transcript(t: &Tmp, sid: &str, cwd: &str, user: &str) {
    let dir = t.0.join("home/.claude/projects/-w");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(format!("{sid}.jsonl")),
        format!(
            "{}\n{}\n",
            json!({"cwd": cwd}),
            turn("user", "2026-11-01T10:00:00Z", json!(user))
        ),
    )
    .unwrap();
}

#[test]
fn latest_names_the_newest_session_begun_in_the_folder_and_pick_a_numbered_one() {
    let t = Tmp::new();
    let work = t.0.join("work");
    fs::create_dir_all(&work).unwrap();
    let work = fs::canonicalize(&work).unwrap();
    let w = work.to_string_lossy().into_owned();
    ledger(
        &t,
        &[
            ("old", "coder", "2026-11-01T09:00:00"),
            ("new", "coder", "2026-11-02T09:00:00"),
            ("far", "coder", "2026-11-03T09:00:00"),
            ("theirs", "writer", "2026-11-03T10:00:00"),
            ("codex:x", "coder", "2026-11-03T11:00:00"),
        ],
    );
    claude_transcript(&t, "old", &w, "  first\n  thing ");
    claude_transcript(&t, "new", &w, "later");
    claude_transcript(&t, "far", "/elsewhere", "far");
    claude_transcript(&t, "theirs", &w, "theirs");
    let c = ctx(&t, &no_env);
    assert_eq!(recent::latest(&c, "coder", &w).unwrap(), "new");
    assert!(
        recent::latest(&c, "coder", "/nowhere")
            .unwrap_err()
            .starts_with("kith: no coder session started in /nowhere")
    );
    let said = std::cell::RefCell::new(String::new());
    let err = |s: &str| said.borrow_mut().push_str(s);
    let picked = recent::pick(&c, "coder", &err, &|| "3\n".to_owned());
    assert_eq!(picked, Ok("old".to_owned()));
    let said = said.into_inner();
    assert!(said.lines().next().unwrap().starts_with("  1  "), "{said}");
    assert!(said.contains("terminal  kith         later"));
    assert!(said.contains("first thing"));
    assert!(said.ends_with("resume which? (number, Enter to cancel) "));
    assert_eq!(
        recent::pick(&c, "coder", &|_| (), &|| "\n".to_owned()),
        Err(None)
    );
    assert_eq!(
        recent::pick(&c, "coder", &|_| (), &|| "9\n".to_owned()),
        Err(Some("kith: no session 9 in the list".to_owned()))
    );
    assert!(
        recent::pick(&c, "nobody", &|_| (), &String::new)
            .unwrap_err()
            .unwrap()
            .contains("no nobody sessions to resume")
    );
}

#[test]
fn the_first_thing_typed_is_one_line_cut_to_the_width() {
    let t = Tmp::new();
    let p = t.0.join("x.jsonl");
    let user = |c: Value| json!({"type": "user", "message": {"content": c}}).to_string();
    fs::write(&p, [
        user(json!("<command-name>/x")),
        json!({"type": "user", "isMeta": true, "message": {"content": "meta"}}).to_string(),
        user(json!([{"type": "text", "text": "<system-reminder>"}, {"type": "text", "text": "a\u{1c}b  c"}])),
    ].join("\n")).unwrap();
    assert_eq!(recent::first_text(&p, 70), "a b c");
    fs::write(&p, user(json!("x".repeat(80)))).unwrap();
    assert_eq!(recent::first_text(&p, 5), "xxxx\u{2026}");
    assert_eq!(recent::first_text(&p, 80).len(), 80);
    assert_eq!(recent::first_text(&t.0.join("none"), 70), "");
}

// --- run: the wrapper --------------------------------------------------------------------------------------------------

fn run_with(t: &Tmp, env: &dyn Fn(&str) -> Option<String>, cmd: &[&str]) -> (i32, String) {
    let c = ctx(t, env);
    let said = std::cell::RefCell::new(String::new());
    let cmd: Vec<String> = cmd.iter().map(|s| (*s).to_owned()).collect();
    let code = run::run(&c, &NoHost, &t.0.join("kernel"), "coder", &cmd, &|s| {
        said.borrow_mut().push_str(s)
    });
    (code, said.into_inner())
}

#[test]
fn the_wrapper_passes_the_exit_code_and_a_signal_death_on() {
    let t = Tmp::new();
    assert_eq!(run_with(&t, &no_env, &["sh", "-c", "exit 3"]).0, 3);
    assert_eq!(run_with(&t, &no_env, &["sh", "-c", "kill -9 $$"]).0, 137);
    let (code, said) = run_with(&t, &no_env, &["no-such-program-here"]);
    assert_eq!(code, 127);
    assert_eq!(
        said,
        "kith: cannot run no-such-program-here: [Errno 2] No such file or directory\n"
    );
}

#[test]
fn the_wrapper_gives_back_its_claims_records_a_quit_and_says_how_to_resume() {
    let t = Tmp::new();
    let transcript = t.0.join("home/.claude/projects/-w/r-1.jsonl");
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    fs::write(&transcript, "{}\n").unwrap();
    // The child writes the registry entry of a session it runs, and a claim for its parent, the wrapper.
    let script = format!(
        "printf '{{\"v\":1,\"sessionId\":\"r-1\",\"agent\":\"coder\",\"cwd\":\"/w\",\"transcriptPath\":\"{}\",\"pid\":%s,\"startedAt\":\"2026-11-03T04:05:06Z\"}}' $$ > {}/r-1.json; \
         printf '{{\"pid\": %s, \"procStart\": null, \"at\": \"x\", \"kind\": \"terminal\"}}' $PPID > {}/s-1.claim",
        transcript.display(),
        t.reg().display(),
        t.reg().display()
    );
    fs::create_dir_all(t.reg()).unwrap();
    t.put(
        "other.claim",
        "{\"pid\": 1, \"procStart\": null, \"at\": \"x\", \"kind\": \"terminal\"}",
    );
    let (code, said) = run_with(&t, &no_env, &["sh", "-c", &script]);
    assert_eq!(code, 0);
    assert_eq!(said, "Resume it as coder with: kith coder --resume r-1\n");
    assert!(
        !claim_path(&t.reg(), "s-1", false).exists(),
        "the wrapper's own claim is given back"
    );
    assert!(
        claim_path(&t.reg(), "other", false).exists(),
        "somebody else's is not"
    );
    assert_eq!(
        intent::read_intent(&t.state(), "r-1").unwrap()["why"],
        "quit"
    );
    // A failed exit is no quit; and the host having taken the session over says so instead.
    fs::remove_file(intent::intent_path(&t.state(), "r-1")).unwrap();
    let (code, _) = run_with(
        &t,
        &no_env,
        &[
            "sh",
            "-c",
            &format!("{script}; touch {}/r-1.taken; exit 2", t.reg().display()),
        ],
    );
    assert_eq!(code, 2);
    assert!(intent::read_intent(&t.state(), "r-1").is_none());
    assert!(
        !t.reg().join("r-1.taken").exists(),
        "the note takes the marker"
    );
}

#[test]
fn an_idle_session_with_nothing_outstanding_is_put_to_sleep_by_its_wrapper() {
    let t = Tmp::new();
    let transcript = t.0.join("home/.claude/projects/-w/r-1.jsonl");
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    fs::write(&transcript, "{}\n").unwrap();
    fs::create_dir_all(t.reg()).unwrap();
    let script = format!(
        "printf '{{\"v\":1,\"sessionId\":\"r-1\",\"agent\":\"coder\",\"cwd\":\"/w\",\"transcriptPath\":\"{}\",\"pid\":%s,\"startedAt\":\"2026-11-03T04:05:06Z\"}}' $$ > {}/r-1.json; exec sleep 60",
        transcript.display(),
        t.reg().display()
    );
    let env = |k: &str| match k {
        "KITH_SLEEP_MINUTES" => Some("0.001".to_owned()),
        "KITH_IDLE_POLL" => Some("0.05".to_owned()),
        _ => None,
    };
    let started = Instant::now();
    let (code, _) = run_with(&t, &env, &["sh", "-c", &script]);
    assert_eq!(code, 128 + 15, "its own child got SIGTERM");
    assert!(started.elapsed() < Duration::from_secs(30));
    assert_eq!(
        intent::read_intent(&t.state(), "r-1").unwrap()["why"],
        "idle"
    );
}
