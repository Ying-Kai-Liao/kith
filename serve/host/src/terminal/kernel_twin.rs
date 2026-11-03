//! The host's claim and registry code next to its twin in `kith_kernel::threads` (docs/plans/core-design.md section 11,
//! "Host-side check"): the same fixture on two state folders, the host's function on one and the crate's on the other, and
//! what each answered and left behind compared. The crate's functions are not called by the host yet; this is the proof
//! that a later switch changes nothing the host's own tests above pin down. Where the two differ on purpose (the crate holds
//! a claim of a kind the host's reader drops, for one) the test says so and pins both sides.

use super::claim::{self, Stopped};
use super::{intent, registry};
use kith_kernel::threads::{listing, terminal};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const GRACE: Duration = Duration::from_millis(300);

/// One state folder with the processes its fixture needs.
struct Side {
    state: PathBuf,
    kids: Vec<Child>,
}

impl Side {
    fn new() -> Self {
        let state = std::env::temp_dir().join(format!("kith-twin-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(registry::dir(&state)).unwrap();
        Self { state, kids: Vec::new() }
    }

    /// A process that lives for the test.
    fn sleeper(&mut self) -> u32 {
        let child = Command::new("sleep").arg("300").stdout(Stdio::null()).spawn().unwrap();
        let pid = child.id();
        self.kids.push(child);
        pid
    }

    /// A shell with a `sleep` child, the shell's pid and the child's.
    fn shell_with_child(&mut self) -> (u32, u32) {
        use std::io::BufRead as _;
        let mut sh = Command::new("sh").args(["-c", "sleep 300 & echo $!; wait"]).stdout(Stdio::piped()).spawn().unwrap();
        let mut line = String::new();
        std::io::BufReader::new(sh.stdout.take().unwrap()).read_line(&mut line).unwrap();
        let pid = sh.id();
        self.kids.push(sh);
        (pid, line.trim().parse().unwrap())
    }

    fn put(&self, name: &str, body: &Value) {
        std::fs::write(registry::dir(&self.state).join(name), body.to_string()).unwrap();
    }

    fn text(&self, name: &str, body: &str) {
        std::fs::write(registry::dir(&self.state).join(name), body).unwrap();
    }

    /// What is left in the registry folder and the intents, by name, with what differs between the sides (pids, times,
    /// key order) taken out.
    fn tree(&self) -> BTreeMap<String, Value> {
        let mut out = BTreeMap::new();
        for (sub, dir) in [("terminal", registry::dir(&self.state)), ("intent", self.state.join("sessions").join("intent"))] {
            for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.ends_with(".lock") || name.starts_with('.') {
                    continue;
                }
                let mut v: Value = serde_json::from_str(&std::fs::read_to_string(e.path()).unwrap_or_default()).unwrap_or(Value::Null);
                if let Some(o) = v.as_object_mut() {
                    for k in ["at", "pid", "procStart"] {
                        o.remove(k);
                    }
                }
                out.insert(format!("{sub}/{name}"), v);
            }
        }
        out
    }
}

impl Drop for Side {
    fn drop(&mut self) {
        for k in &mut self.kids {
            let _ = k.kill();
            let _ = k.wait();
        }
        let _ = std::fs::remove_dir_all(&self.state);
    }
}

fn gone(pid: u32) -> bool {
    for _ in 0..40 {
        if !registry::pid_runs(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn claim_body(pid: u32, kind: &str, extra: &Value) -> Value {
    let mut v = json!({"pid": pid, "procStart": Value::Null, "at": "2026-11-03T00:00:00Z", "kind": kind});
    for (k, x) in extra.as_object().unwrap() {
        v[k] = x.clone();
    }
    v
}

// --- who holds a session ------------------------------------------------------------------------------------------------

/// `(kind, takeover)` of the claim that keeps the host from writing, by each implementation.
fn holders(spec: &(&str, &str, Value, &str)) -> (Option<(String, bool)>, Option<(String, bool)>) {
    let (name, kind, extra, pid_kind) = spec;
    let mut answers = Vec::new();
    for _ in 0..2 {
        let mut side = Side::new();
        let pid = match *pid_kind {
            "dead" => {
                let mut c = Command::new("true").spawn().unwrap();
                c.wait().unwrap();
                c.id()
            }
            _ => side.sleeper(),
        };
        let mut body = claim_body(pid, kind, extra);
        if *pid_kind == "reused" {
            body["procStart"] = json!("Mon Jan  1 00:00:00 2001");
        }
        side.put(name, &body);
        answers.push(side);
    }
    let sid = name.trim_end_matches(".claim").trim_end_matches(".takeover");
    let host = claim::holder(&answers[0].state, sid).map(|c| (serde_json::to_value(c.kind).unwrap().as_str().unwrap().to_owned(), c.takeover));
    let krate = terminal::writer_holder(&answers[1].state, sid).map(|c| (c.kind, c.takeover));
    (host, krate)
}

#[test]
fn the_claim_that_keeps_the_host_from_writing_is_the_same() {
    let none = json!({});
    let cases: Vec<(&str, &str, Value, &str)> = vec![
        ("s1.claim", "terminal", none.clone(), "live"),
        ("s2.claim", "app", none.clone(), "live"),
        ("s3.takeover.claim", "app", json!({"takeover": true}), "live"),
        ("s4.claim", "bot", none.clone(), "live"),
        ("s5.claim", "oneshot", json!({"person": "sam", "agent": "coder"}), "live"),
        ("s6.claim", "terminal", none.clone(), "dead"),
        ("s7.claim", "terminal", none.clone(), "reused"),
        ("s8.takeover.claim", "terminal", json!({"takeover": true}), "dead"),
    ];
    for case in &cases {
        let (host, krate) = holders(case);
        assert_eq!(host, krate, "{}", case.0);
    }
    // A host turn's own claim is not a holder, a take-over's is: pinned on both sides.
    assert_eq!(holders(&cases[1]).1, None);
    assert_eq!(holders(&cases[2]).1, Some(("app".to_owned(), true)));
}

#[test]
fn where_the_crate_reads_a_claim_more_leniently_than_the_host_it_holds() {
    // Difference 2: the host's reader accepts four kinds and drops the rest as garbage; the crate (and kith-sessions)
    // hold the session for a claim of any kind. Difference 3: an empty `procStart` is another start time to the host,
    // an unknown one to the crate.
    let (host, krate) = holders(&("u1.claim", "serve", json!({}), "live"));
    assert_eq!((host, krate), (None, Some(("serve".to_owned(), false))));
    let (host, krate) = holders(&("u2.claim", "terminal", json!({"procStart": ""}), "live"));
    assert_eq!((host.is_some(), krate.is_some()), (false, true));
}

// --- the registry ---------------------------------------------------------------------------------------------------------

#[test]
fn the_registry_is_read_alike() {
    let ok = json!({
        "v": 1, "sessionId": "abc-1", "agent": "builder", "cwd": "/w", "transcriptPath": "/p/abc-1.jsonl",
        "pid": 7, "project": null, "source": "startup", "resumedFrom": null, "startedAt": "2026-10-23T01:00:00Z",
    });
    let with = |k: &str, v: Value| {
        let mut x = ok.clone();
        x[k] = v;
        x
    };
    let mut cases: Vec<Value> = vec![
        ok.clone(),
        with("kind", json!("ask")),
        with("kind", json!("helper")),
        with("kind", json!("serve")),
        with("kind", Value::Null),
        with("person", json!("sam")),
        with("personId", json!("sam")),
        with("accountPerson", Value::Null),
        with("v", json!(2)),
        with("v", json!(1.0)),
        with("sessionId", json!("../x")),
        with("sessionId", json!("")),
        with("transcriptPath", json!("/p/other.jsonl")),
        with("cwd", json!("relative")),
        with("agent", json!("")),
        with("pid", json!(1)),
        with("pid", json!(0)),
        with("pid", json!(-4)),
        with("pid", json!(4_294_967_295_u64)),
        with("pid", json!(4_294_967_296_u64)),
        with("pid", json!("7")),
        with("pid", Value::Null),
        with("project", json!("kith")),
        with("project", json!(3)),
        with("source", Value::Null),
        with("startedAt", json!("garbage")),
        with("startedAt", json!("2026-10-23T01:00:00.123456+10:00")),
        with("startedAt", json!("2026-10-23T01:00:00")),
        with("procStart", json!("Mon Jan  1 00:00:00 2001")),
        with("account", json!("claude:work")),
        with("label", json!(5)),
    ];
    let mut no_id = ok.clone();
    no_id.as_object_mut().unwrap().remove("sessionId");
    cases.push(no_id);
    for (i, v) in cases.iter().enumerate() {
        let raw = v.to_string();
        let host = registry::parse(&raw).map(|r| {
            format!(
                "{} {} {} {} {} {:?} {} {:?} {} {} {:?} {:?} {:?} {}",
                r.v, r.session_id, r.agent, r.cwd, r.transcript_path, r.project, r.source, r.resumed_from, r.started_at, r.kind, r.label,
                r.proc_start, r.account, r.pid
            ) + &r.started_ms().to_string()
        });
        let krate = listing::parse(&raw).map(|r| {
            format!(
                "{} {} {} {} {} {:?} {} {:?} {} {} {:?} {:?} {:?} {}",
                r.v, r.session_id, r.agent, r.cwd, r.transcript_path, r.project, r.source, r.resumed_from, r.started_at, r.kind, r.label,
                r.proc_start, r.account, r.pid
            ) + &r.started_ms().to_string()
        });
        assert_eq!(host, krate, "case {i}: {raw}");
    }
    for raw in ["", "not json", "[]", "null", "{}", "{\"v\":1}"] {
        assert_eq!(registry::parse(raw).is_some(), listing::parse(raw).is_some(), "{raw}");
    }
}

fn fill_registry(side: &mut Side) -> Vec<String> {
    let pid = side.sleeper();
    let dir = side.state.clone();
    let entry = |sid: &str, started: &str, extra: Value| {
        let mut v = json!({
            "v": 1, "sessionId": sid, "agent": "builder", "cwd": dir.to_string_lossy(),
            "transcriptPath": dir.join(format!("{sid}.jsonl")).to_string_lossy(), "pid": pid, "project": null,
            "source": "startup", "resumedFrom": null, "startedAt": started,
        });
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    };
    side.put("b-2.json", &entry("b-2", "2026-10-23T02:00:00+00:00", json!({})));
    side.put("a-1.json", &entry("a-1", "2026-10-23T01:00:00.250Z", json!({"kind": "ask"})));
    side.put("c-3.json", &entry("c-3", "garbage", json!({})));
    side.put("p-4.json", &entry("p-4", "2026-10-23T03:00:00Z", json!({"person": "sam"})));
    side.put("same-5.json", &entry("same-5", "2026-10-23T02:00:00+00:00", json!({})));
    side.text("d-6.json", "{");
    side.text("e-7.json", &" ".repeat(70_000));
    side.text("f-8.txt", "{}");
    vec!["b-2".into(), "a-1".into(), "c-3".into(), "same-5".into()]
}

#[test]
fn a_registry_folder_lists_the_same_sessions_in_the_same_order() {
    let (mut host, mut krate) = (Side::new(), Side::new());
    fill_registry(&mut host);
    fill_registry(&mut krate);
    let h: Vec<String> = registry::read_all(&host.state).into_iter().map(|r| r.session_id).collect();
    let k: Vec<String> = listing::read_all(&krate.state).into_iter().map(|r| r.session_id).collect();
    assert_eq!(h, k);
    assert_eq!(h, ["c-3", "a-1", "b-2", "same-5"]);
    for sid in ["a-1", "e-7", "p-4", "d-6", "nothing"] {
        assert_eq!(registry::read_one(&host.state, sid).map(|r| r.session_id), listing::read_one(&krate.state, sid).map(|r| r.session_id), "{sid}");
    }
    assert!(registry::read_all(Path::new("/nonexistent/state")).is_empty() && listing::read_all(Path::new("/nonexistent/state")).is_empty());
}

fn write_session_file(dir: &Path, pid: u32, sid: &str, status: &str, proc_start: Option<&str>) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(format!("{pid}.json")), json!({"pid": pid, "sessionId": sid, "status": status, "procStart": proc_start}).to_string()).unwrap();
}

fn live_name(h: registry::Live) -> &'static str {
    match h {
        registry::Live::Closed => "closed",
        registry::Live::Idle => "idle",
        registry::Live::Busy => "busy",
    }
}

fn live_name_k(l: listing::Live) -> &'static str {
    match l {
        listing::Live::Closed => "closed",
        listing::Live::Idle => "idle",
        listing::Live::Busy => "busy",
    }
}

#[test]
fn a_session_is_live_idle_or_busy_alike() {
    // One scenario per row: what the session file says (status, session, start), the kind, and the start the registry holds.
    struct Row(&'static str, &'static str, Option<&'static str>, &'static str, bool, Option<&'static str>);
    let rows = [
        Row("idle", "same", None, "terminal", true, None),
        Row("busy", "same", None, "terminal", true, None),
        Row("something-new", "same", None, "terminal", true, None),
        Row("idle", "other", None, "terminal", true, None),
        Row("idle", "same", Some("Mon Jan  1 00:00:00 2001"), "terminal", true, None),
        Row("idle", "same", Some("local"), "terminal", true, None),
        Row("idle", "same", Some("utc"), "terminal", true, None),
        Row("idle", "same", None, "terminal", false, None),
        Row("-", "-", None, "terminal", true, None),
        Row("-", "-", None, "ask", true, Some("local")),
        Row("-", "-", None, "ask", true, Some("Mon Jan  1 00:00:00 2001")),
        Row("-", "-", None, "ask", true, None),
        Row("-", "-", None, "ask", false, Some("local")),
    ];
    for (i, Row(status, named, start, kind, alive, one_shot_start)) in rows.iter().enumerate() {
        let mut answers = Vec::new();
        for _ in 0..2 {
            let mut side = Side::new();
            let pid = if *alive {
                side.sleeper()
            } else {
                let mut c = Command::new("true").spawn().unwrap();
                c.wait().unwrap();
                c.id()
            };
            let sessions = side.state.join("claude-sessions");
            let resolve = |s: &str| match s {
                "local" => ps_start(pid, false),
                "utc" => ps_start(pid, true),
                other => Some(other.to_owned()),
            };
            if *status != "-" {
                let sid = if *named == "same" { "sess-1" } else { "another" };
                write_session_file(&sessions, pid, sid, status, start.and_then(resolve).as_deref());
            }
            let mut reg = json!({
                "v": 1, "sessionId": "sess-1", "agent": "builder", "cwd": "/w",
                "transcriptPath": format!("/p/{}/projects/-w/sess-1.jsonl", side.state.display()), "pid": pid, "kind": kind,
            });
            if let Some(s) = one_shot_start {
                reg["procStart"] = json!(resolve(s));
            }
            side.text("sess-1.json", &reg.to_string());
            answers.push((side, sessions));
        }
        let h = registry::read_one(&answers[0].0.state, "sess-1").map(|r| live_name(registry::liveness(&r, Some(&answers[0].1))));
        let k = listing::read_one(&answers[1].0.state, "sess-1").map(|r| live_name_k(listing::liveness(&r, Some(&answers[1].1))));
        assert_eq!(h, k, "row {i}");
        assert!(h.is_some(), "row {i} is a listable session");
    }
}

fn ps_start(pid: u32, utc: bool) -> Option<String> {
    let mut ps = Command::new("ps");
    ps.env("LC_ALL", "C");
    if utc {
        ps.env("TZ", "UTC");
    }
    let out = ps.args(["-o", "lstart=", "-p", &pid.to_string()]).output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then_some(text)
}

// --- the revoke sweep and the owner's stop ----------------------------------------------------------------------------------

/// The fixture of the sweep: a guest's shell with a child, the owner's claim, another person's holder, a warm proc claim.
/// Returns the pids by role.
struct Swept {
    guest: u32,
    guest_child: u32,
    owner: u32,
    other: u32,
    warm: u32,
}

fn sweep_fixture(side: &mut Side) -> Swept {
    let (guest, guest_child) = side.shell_with_child();
    let (owner, other, warm) = (side.sleeper(), side.sleeper(), side.sleeper());
    side.put("g1.claim", &claim_body(guest, "oneshot", &json!({"person": "bot-sam", "agent": "coder"})));
    side.put("g2.takeover.claim", &claim_body(warm, "app", &json!({"person": "sam", "takeover": true})));
    side.put("o1.claim", &claim_body(owner, "terminal", &json!({})));
    side.put("x1.claim", &claim_body(other, "oneshot", &json!({"person": "ann"})));
    side.put(&format!("proc-{warm}.claim"), &claim_body(warm, "app", &json!({"person": "sam"})));
    Swept { guest, guest_child, owner, other, warm }
}

#[test]
fn the_sweep_ends_the_same_holders_and_leaves_the_same_files() {
    for narrowing in [false, true] {
        let (mut host, mut krate) = (Side::new(), Side::new());
        let (hf, kf) = (sweep_fixture(&mut host), sweep_fixture(&mut krate));
        let persons = vec!["bot-sam".to_owned(), "sam".to_owned()];
        // A narrowed access keeps the holders still allowed on their agent; the proc claim names none.
        let keep_h = |c: &claim::Claim| narrowing && c.agent.as_deref() == Some("coder");
        let keep_k = |c: &kith_kernel::threads::Claim| narrowing && c.agent.as_deref() == Some("coder");
        let (ended_h, ended_k) = (claim::sweep(&host.state, &persons, &keep_h, GRACE).len(), terminal::sweep(&krate.state, &persons, &keep_k, GRACE).len());
        assert_eq!(ended_h, ended_k, "narrowing {narrowing}");
        // What is left that a second sweep would consider.
        assert_eq!(claim::holders_of(&host.state, &persons).len(), terminal::holders_of(&krate.state, &persons).len());
        assert_eq!(host.tree(), krate.tree(), "narrowing {narrowing}");
        for (h, k) in [(&hf, &kf)] {
            for (name, a, b) in [("guest", h.guest, k.guest), ("owner", h.owner, k.owner), ("other", h.other, k.other), ("warm", h.warm, k.warm)] {
                assert_eq!(gone(a), gone(b), "{name}, narrowing {narrowing}");
            }
            assert_eq!(gone(h.guest_child), gone(k.guest_child), "the guest's child");
        }
        if !narrowing {
            assert_eq!(host.tree()["intent/g1.json"], json!({"why": "access revoked"}));
            assert_eq!(intent::read(&host.state, "g1").as_deref(), Some("access revoked"));
            assert_eq!(kith_kernel::threads::intent::read_intent(&krate.state, "g1").unwrap()["why"], "access revoked");
            assert!(registry::pid_runs(hf.owner) && registry::pid_runs(kf.owner), "the owner's claim is never touched");
        }
    }
}

#[test]
fn the_sweep_leaves_a_reused_pid_and_a_dead_one_alone_on_both_sides() {
    let (mut host, mut krate) = (Side::new(), Side::new());
    for side in [&mut host, &mut krate] {
        let stranger = side.sleeper();
        let mut body = claim_body(stranger, "oneshot", &json!({"person": "sam"}));
        body["procStart"] = json!("Mon Jan  1 00:00:00 2001");
        side.put("g2.claim", &body);
    }
    let persons = vec!["sam".to_owned()];
    assert_eq!(claim::sweep(&host.state, &persons, &|_| false, GRACE).len(), 0);
    assert_eq!(terminal::sweep(&krate.state, &persons, &|_| false, GRACE).len(), 0);
    assert_eq!(host.tree(), krate.tree());
    assert!(host.kids.iter().chain(&krate.kids).all(|k| registry::pid_runs(k.id())));
}

#[test]
fn stopping_a_holder_does_the_same_on_both_sides() {
    let (mut host, mut krate) = (Side::new(), Side::new());
    // Nobody holds it.
    assert_eq!(claim::stop_holder(&host.state, "h1", GRACE), Stopped::Nobody);
    assert_eq!(terminal::stop_holder(&krate.state, "h1", GRACE), terminal::Stopped::Nobody);
    // The holder is the shell's child, so it is not this process' own turn.
    let ((_, hc), (_, kc)) = (host.shell_with_child(), krate.shell_with_child());
    host.put("h1.claim", &claim_body(hc, "terminal", &json!({})));
    krate.put("h1.claim", &claim_body(kc, "terminal", &json!({})));
    assert!(matches!(claim::stop_holder(&host.state, "h1", GRACE), Stopped::Ended(_)));
    assert!(matches!(terminal::stop_holder(&krate.state, "h1", GRACE), terminal::Stopped::Ended(_)));
    assert_eq!(host.tree(), krate.tree());
    assert_eq!(host.tree()["intent/h1.json"], json!({"why": "stopped from the app"}));
    assert!(gone(hc) && gone(kc), "the claimer is ended");
    // The host's own turn: its child holds the claim.
    let (h2, k2) = (host.sleeper(), krate.sleeper());
    host.put("h2.claim", &claim_body(h2, "app", &json!({"takeover": true})));
    krate.put("h2.claim", &claim_body(k2, "app", &json!({"takeover": true})));
    // These sleepers are children of this process, as the host's own turn's claimer is.
    assert!(matches!(claim::stop_holder(&host.state, "h2", GRACE), Stopped::OwnTurn(_)));
    assert!(matches!(terminal::stop_holder(&krate.state, "h2", GRACE), terminal::Stopped::OwnTurn(_)));
    assert!(registry::pid_runs(h2) && registry::pid_runs(k2));
    // A host turn's own `app` claim (not a take-over) is nobody's to stop.
    let (h3, k3) = (host.sleeper(), krate.sleeper());
    host.put("h3.claim", &claim_body(h3, "app", &json!({})));
    krate.put("h3.claim", &claim_body(k3, "app", &json!({})));
    assert_eq!(claim::stop_holder(&host.state, "h3", GRACE), Stopped::Nobody);
    assert_eq!(terminal::stop_holder(&krate.state, "h3", GRACE), terminal::Stopped::Nobody);
}
