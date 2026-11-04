#![allow(clippy::unwrap_used, clippy::result_large_err)]

use super::resolve::{Host, NoHost, resolve};
use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::AtomicU32;

static N: AtomicU32 = AtomicU32::new(0);

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let dir = std::env::temp_dir().join(format!(
            "kith-threads-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Tmp(dir)
    }

    fn state(&self) -> PathBuf {
        self.0.join("state")
    }

    fn reg(&self) -> PathBuf {
        registry_dir(&self.state())
    }

    fn put(&self, name: &str, text: &str) {
        fs::create_dir_all(self.reg()).unwrap();
        fs::write(self.reg().join(name), text).unwrap();
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
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

/// A process that lives until the guard drops: a claimer that is not the test itself.
struct Sleeper(Child);

impl Sleeper {
    fn new() -> Self {
        Self(
            Command::new("sleep")
                .arg("60")
                .stdin(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }

    fn pid(&self) -> i64 {
        i64::from(self.0.id())
    }
}

impl Drop for Sleeper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A pid that ran and is gone.
fn dead_pid() -> i64 {
    let mut child = Command::new("true").spawn().unwrap();
    let pid = i64::from(child.id());
    child.wait().unwrap();
    pid
}

fn me() -> i64 {
    i64::from(std::process::id())
}

fn take(c: &Ctx, sid: &str, pid: i64) -> Result<(), Held> {
    take_claim(c, sid, pid, &Take::new("terminal")).unwrap()
}

fn on_disk(t: &Tmp, sid: &str) -> Claim {
    read_claim(&claim_path(&t.reg(), sid, false)).unwrap()
}

#[test]
fn a_retarget_moves_the_claim_to_another_pid_only_while_it_is_the_givers() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let a = Sleeper::new();
    assert!(
        take_claim(&c, "r1", me(), &Take::new("starting"))
            .unwrap()
            .is_ok()
    );
    // Not the giver's: nothing changes.
    assert!(!retarget(&c, "r1", a.pid(), 1, "terminal").unwrap());
    assert_eq!(on_disk(&t, "r1").kind, "starting");
    assert!(retarget(&c, "r1", me(), a.pid(), "terminal").unwrap());
    let got = on_disk(&t, "r1");
    assert_eq!((got.pid, got.kind.as_str()), (Some(a.pid()), "terminal"));
    assert_eq!(got.proc_start, os::proc_start(a.pid()));
    // The new holder keeps the session; the old pid has nothing left to give back.
    assert!(take(&c, "r1", me()).is_err());
    release_one(&c, "r1", me(), false);
    assert!(claim_path(&t.reg(), "r1", false).is_file());
    release_one(&c, "r1", a.pid(), false);
    assert!(!claim_path(&t.reg(), "r1", false).exists());
}

// --- the claim ---------------------------------------------------------------------------------------------------------

#[test]
fn a_free_thread_is_claimed_and_the_file_is_what_kith_sessions_writes() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    take(&c, "s-1", me()).unwrap();
    let raw = fs::read_to_string(claim_path(&t.reg(), "s-1", false)).unwrap();
    // Python's json.dump: ", " and ": ", keys in this order, no takeover or person key.
    assert!(
        raw.starts_with(&format!("{{\"pid\": {}, \"procStart\": ", me())),
        "{raw}"
    );
    assert!(raw.ends_with("\"kind\": \"terminal\"}"), "{raw}");
    let v: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        v.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["pid", "procStart", "at", "kind"]
    );
    assert!(v["at"].as_str().unwrap().ends_with('Z') && v["at"].as_str().unwrap().len() == 20);
    assert_eq!(
        fs::metadata(claim_path(&t.reg(), "s-1", false))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(t.reg()).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[test]
fn a_person_and_an_agent_are_written_only_together() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    take_claim(
        &c,
        "s-1",
        me(),
        &Take {
            person: Some("p1"),
            agent: Some("coder"),
            ..Take::new("oneshot")
        },
    )
    .unwrap()
    .unwrap();
    take_claim(
        &c,
        "s-2",
        me(),
        &Take {
            agent: Some("coder"),
            ..Take::new("oneshot")
        },
    )
    .unwrap()
    .unwrap();
    let (a, b) = (on_disk(&t, "s-1"), on_disk(&t, "s-2"));
    assert_eq!(
        (a.person.as_deref(), a.agent.as_deref(), a.kind.as_str()),
        (Some("p1"), Some("coder"), "oneshot")
    );
    assert_eq!((b.person, b.agent), (None, None));
}

#[test]
fn a_live_claim_of_another_writer_holds_the_thread() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let other = Sleeper::new();
    take(&c, "s-1", other.pid()).unwrap();
    let Held(h) = take(&c, "s-1", me()).unwrap_err();
    assert_eq!((h.pid, h.kind.as_str()), (Some(other.pid()), "terminal"));
    // The refused taker leaves the holder's file alone and no temp file behind.
    assert_eq!(on_disk(&t, "s-1").pid, Some(other.pid()));
    let leftovers: Vec<_> = fs::read_dir(t.reg())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn a_claim_whose_pid_is_gone_is_taken_over() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let dead = dead_pid();
    t.put("s-1.claim", &format!("{{\"pid\": {dead}, \"procStart\": null, \"at\": \"2026-01-01T00:00:00Z\", \"kind\": \"terminal\"}}"));
    take(&c, "s-1", me()).unwrap();
    assert_eq!(on_disk(&t, "s-1").pid, Some(me()));
}

#[test]
fn a_reused_pid_is_stale_when_it_has_another_start_stamp() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let other = Sleeper::new();
    // The same form as this system's stamps, another start: a reused pid.
    let stamp = other_start(&os::proc_start(other.pid()).unwrap());
    t.put(
        "s-1.claim",
        &format!(
            "{{\"pid\": {}, \"procStart\": \"{stamp}\", \"at\": \"x\", \"kind\": \"terminal\"}}",
            other.pid()
        ),
    );
    take(&c, "s-1", me()).unwrap();
    assert_eq!(on_disk(&t, "s-1").pid, Some(me()));
}

/// A stamp of the same form as `real` that is not it.
fn other_start(real: &str) -> String {
    match real.split_once(':') {
        Some((boot, _)) => format!("{boot}:1"),
        None => "e1".to_owned(),
    }
}

#[test]
fn an_old_lstart_text_holds_only_while_it_matches_the_process() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let other = Sleeper::new();
    // Written before stamps: compared the old way. Another start time is a reused pid, and the claim is free.
    t.put(
        "s-1.claim",
        &format!(
            "{{\"pid\": {}, \"procStart\": \"Mon Jan  1 00:00:00 1990\", \"at\": \"x\", \"kind\": \"terminal\"}}",
            other.pid()
        ),
    );
    assert!(take(&c, "s-1", me()).is_ok());
}

#[test]
fn an_old_lstart_text_that_matches_the_process_holds() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let other = Sleeper::new();
    let text = os::lstart(other.pid(), false).expect("ps knows the process");
    t.put(
        "s-1.claim",
        &format!(
            "{{\"pid\": {}, \"procStart\": \"{text}\", \"at\": \"x\", \"kind\": \"terminal\"}}",
            other.pid()
        ),
    );
    assert!(take(&c, "s-1", me()).is_err());
}

#[test]
fn the_sweep_leaves_an_old_text_claim_on_a_reused_pid_alone() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let other = Sleeper::new();
    t.put(
        "s-1.claim",
        &format!(
            "{{\"pid\": {}, \"procStart\": \"Mon Jan  1 00:00:00 1990\", \"at\": \"x\", \"kind\": \"oneshot\", \"person\": \"p1\"}}",
            other.pid()
        ),
    );
    assert_eq!(
        sweep_persons(&c, &["p1".to_owned()], Duration::ZERO),
        Vec::<i64>::new()
    );
    assert!(os::pid_runs(other.pid()), "nothing was signalled");
}

#[test]
fn a_claim_with_the_processs_own_stamp_holds() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let other = Sleeper::new();
    let start = os::proc_start(other.pid()).unwrap();
    assert!(crate::procstart::is_stamp(&start), "{start}");
    t.put(
        "s-1.claim",
        &format!(
            "{{\"pid\": {}, \"procStart\": \"{start}\", \"at\": \"x\", \"kind\": \"terminal\"}}",
            other.pid()
        ),
    );
    assert!(take(&c, "s-1", me()).is_err());
}

#[test]
fn old_claims_with_fewer_fields_still_hold() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let other = Sleeper::new();
    // Before procStart and kind existed: the pid alone.
    t.put("s-1.claim", &format!("{{\"pid\": {}}}", other.pid()));
    let Held(h) = take(&c, "s-1", me()).unwrap_err();
    assert_eq!(
        (h.pid, h.kind.as_str(), h.proc_start),
        (Some(other.pid()), "terminal", None)
    );
    // An empty start time means unknown, not "a different time".
    t.put(
        "s-2.claim",
        &format!(
            "{{\"pid\": {}, \"procStart\": \"\", \"kind\": \"oneshot\"}}",
            other.pid()
        ),
    );
    assert!(take(&c, "s-2", me()).is_err());
}

#[test]
fn a_claim_of_an_unknown_kind_is_held_like_any_other() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let other = Sleeper::new();
    t.put(
        "s-1.claim",
        &format!(
            "{{\"pid\": {}, \"kind\": \"compact\", \"future\": [1]}}",
            other.pid()
        ),
    );
    let Held(h) = take(&c, "s-1", me()).unwrap_err();
    assert_eq!(h.kind, "compact");
}

#[test]
fn damaged_claims_are_free() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    for (i, junk) in [
        "",
        "{",
        "not json",
        "[1, 2]",
        "\"text\"",
        "null",
        "{}",
        "{\"pid\": \"12\"}",
        "{\"pid\": 1.5}",
        "{\"pid\": null}",
        "{\"kind\": \"terminal\"}",
    ]
    .iter()
    .enumerate()
    {
        let sid = format!("s-{i}");
        t.put(&format!("{sid}.claim"), junk);
        take(&c, &sid, me()).unwrap();
        assert_eq!(on_disk(&t, &sid).pid, Some(me()), "{junk:?}");
    }
}

#[test]
fn a_pid_no_process_can_have_is_not_a_holder() {
    // kith-sessions would call os.kill(0, 0) and os.kill(-1, 0) alive (they name process groups); a claim is never theirs.
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    for (i, pid) in ["0", "-1", "99999999999"].iter().enumerate() {
        t.put(&format!("s-{i}.claim"), &format!("{{\"pid\": {pid}}}"));
        take(&c, &format!("s-{i}"), me()).unwrap();
    }
}

#[test]
fn a_claim_without_its_json_is_a_claim() {
    // Crash between the claim and the registry file (or a one-shot, which writes none): the claim alone holds the thread.
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let other = Sleeper::new();
    take(&c, "s-1", other.pid()).unwrap();
    assert!(!t.reg().join("s-1.json").exists());
    assert!(take(&c, "s-1", me()).is_err());
    assert!(live_holder(&t.reg(), "s-1").is_some());
}

#[test]
fn a_name_that_could_leave_the_folder_is_refused() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    for sid in ["", "../x", "a/b", "a.b", &"x".repeat(65)] {
        assert!(
            take_claim(&c, sid, me(), &Take::new("terminal")).is_err(),
            "{sid:?}"
        );
    }
    assert!(!t.state().join("live").join("x.claim").exists());
}

#[test]
fn no_state_folder_is_an_error_not_a_default() {
    let c = Ctx {
        state: None,
        machine: None,
        home: PathBuf::from("/nowhere"),
        env: &no_env,
    };
    assert!(take_claim(&c, "s-1", me(), &Take::new("terminal")).is_err());
    assert_eq!(
        claim_start(&c, "s-1", me(), "oneshot", Duration::ZERO, None, None).unwrap_err(),
        "KITH_STATE is not set, so the session cannot be claimed"
    );
}

#[test]
fn the_host_may_read_what_this_writes_and_this_reads_what_the_host_writes() {
    let t = Tmp::new();
    // terminal/claim.rs writes rfc3339 with an offset, a takeover flag and an agent without a person.
    t.put("s-1.claim", &format!("{{\"pid\":{},\"procStart\":null,\"at\":\"2026-11-03T01:02:03.456+00:00\",\"kind\":\"app\",\"takeover\":false,\"agent\":\"coder\"}}", Sleeper::new().pid()));
    let c = read_claim(&t.reg().join("s-1.claim")).unwrap();
    assert_eq!(
        (c.kind.as_str(), c.takeover, c.agent.as_deref(), c.person),
        ("app", false, Some("coder"), None)
    );
    // And the shape the host's serde reads back (all four kinds are lowercase words; extra keys are fine for it).
    let v = Claim {
        pid: Some(5),
        proc_start: None,
        at: "x".into(),
        kind: "bot".into(),
        person: Some("p".into()),
        agent: Some("a".into()),
        takeover: true,
    }
    .to_value();
    assert_eq!(v["takeover"], true);
    assert_eq!(v["procStart"], Value::Null);
}

#[test]
fn only_the_holder_gives_a_claim_back() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let other = Sleeper::new();
    take(&c, "s-1", other.pid()).unwrap();
    take(&c, "s-2", me()).unwrap();
    release_claims(&c, me());
    assert!(claim_path(&t.reg(), "s-1", false).exists());
    assert!(!claim_path(&t.reg(), "s-2", false).exists());
    release_one(&c, "s-1", me(), false);
    assert!(
        claim_path(&t.reg(), "s-1", false).exists(),
        "not the holder's"
    );
    release_one(&c, "s-1", other.pid(), false);
    assert!(!claim_path(&t.reg(), "s-1", false).exists());
}

#[test]
fn taking_clears_the_intent_and_the_decision_except_for_the_lifecycle_owner() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    for (sid, owner) in [("s-1", false), ("s-2", true)] {
        assert!(intent::write_intent(&t.state(), sid, "idle", me()));
        assert!(intent::write_decision(
            &t.state(),
            sid,
            &serde_json::json!({"d": "end"})
        ));
        take_claim(
            &c,
            sid,
            me(),
            &Take {
                lifecycle_owner: owner,
                ..Take::new("oneshot")
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            intent::read_intent(&t.state(), sid).is_some(),
            owner,
            "{sid}"
        );
        assert_eq!(
            intent::read_decision(&t.state(), sid).is_some(),
            owner,
            "{sid}"
        );
    }
    assert!(!intent::write_intent(&t.state(), "../x", "idle", me()));
}

#[test]
fn a_new_claim_prunes_the_stale_ones_beside_it_but_not_proc_or_takeover_claims() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let dead = dead_pid();
    let stale = format!("{{\"pid\": {dead}}}");
    for name in [
        "old.claim",
        "proc-9.claim",
        "gone.takeover.claim",
        "junk.claim",
    ] {
        t.put(name, if name == "junk.claim" { "{" } else { &stale });
    }
    let other = Sleeper::new();
    take(&c, "alive", other.pid()).unwrap();
    take(&c, "new", me()).unwrap();
    let left = |n: &str| t.reg().join(n).exists();
    assert!(!left("old.claim") && !left("junk.claim"));
    assert!(left("proc-9.claim") && left("gone.takeover.claim"));
    assert!(left("alive.claim") && left("new.claim"));
}

#[test]
fn a_takeover_claim_has_its_own_file_and_lock_and_ends_no_intent() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    assert!(intent::write_intent(&t.state(), "s-1", "idle", me()));
    let other = Sleeper::new();
    take_claim(
        &c,
        "s-1",
        other.pid(),
        &Take {
            takeover: true,
            ..Take::new("app")
        },
    )
    .unwrap()
    .unwrap();
    assert!(
        claim_path(&t.reg(), "s-1", true).exists()
            && t.reg().join("s-1.takeover.claim.lock").exists()
    );
    assert!(!claim_path(&t.reg(), "s-1", false).exists());
    assert!(intent::read_intent(&t.state(), "s-1").is_some());
    assert!(
        read_claim(&claim_path(&t.reg(), "s-1", true))
            .unwrap()
            .takeover
    );
    // The terminal's claim is refused while the host's take-over lives.
    assert_eq!(
        claim_start(&c, "s-1", me(), "terminal", Duration::ZERO, None, None).unwrap_err(),
        "that session is being taken over in the app right now; try again in a moment"
    );
    assert!(!claim_path(&t.reg(), "s-1", false).exists(), "given back");
}

#[test]
fn a_one_shot_waits_for_a_held_session_and_a_terminal_does_not() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let other = Sleeper::new();
    take(&c, "s-1", other.pid()).unwrap();
    let began = Instant::now();
    let err = claim_start(&c, "s-1", me(), "terminal", Duration::ZERO, None, None).unwrap_err();
    assert!(began.elapsed() < Duration::from_millis(200));
    assert_eq!(
        err,
        format!(
            "that session is busy: held by terminal (pid {}); try again in a moment",
            other.pid()
        )
    );
    let began = Instant::now();
    claim_start(
        &c,
        "s-1",
        me(),
        "oneshot",
        Duration::from_millis(400),
        None,
        None,
    )
    .unwrap_err();
    assert!(began.elapsed() >= Duration::from_millis(400));
    // The pid that holds it asks again: not held against itself.
    claim_start(
        &c,
        "s-1",
        other.pid(),
        "terminal",
        Duration::ZERO,
        None,
        None,
    )
    .unwrap();
}

#[test]
fn the_claim_wait_comes_from_the_environment() {
    let env = |k: &str| (k == "KITH_CLAIM_WAIT").then(|| "1.5".to_owned());
    assert_eq!(claim_wait(&env), Duration::from_millis(1500));
    assert_eq!(claim_wait(&no_env), Duration::from_secs(5));
    let bad = |_: &str| Some("soon".to_owned());
    assert_eq!(claim_wait(&bad), Duration::from_secs(5));
    let neg = |_: &str| Some("-3".to_owned());
    assert_eq!(claim_wait(&neg), Duration::ZERO);
}

#[test]
fn a_proc_claim_names_the_person_and_prunes_the_dead() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let dead = dead_pid();
    t.put(
        &format!("proc-{dead}.claim"),
        &format!("{{\"pid\": {dead}}}"),
    );
    proc_claim(&c, me(), "p1", "coder").unwrap();
    assert!(!t.reg().join(format!("proc-{dead}.claim")).exists());
    let got = read_claim(&t.reg().join(format!("proc-{}.claim", me()))).unwrap();
    assert_eq!(
        (
            got.kind.as_str(),
            got.person.as_deref(),
            got.agent.as_deref()
        ),
        ("app", Some("p1"), Some("coder"))
    );
    // It names no session, so it holds nothing.
    assert!(live_holder(&t.reg(), "s-1").is_none());
}

#[test]
fn the_sweep_ends_a_persons_run_and_never_the_owners() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let (theirs, mine) = (Sleeper::new(), Sleeper::new());
    take_claim(
        &c,
        "s-1",
        theirs.pid(),
        &Take {
            person: Some("p1"),
            agent: Some("coder"),
            ..Take::new("oneshot")
        },
    )
    .unwrap()
    .unwrap();
    take(&c, "s-2", mine.pid()).unwrap();
    let ended = sweep_persons(&c, &["p1".to_owned()], Duration::from_millis(500));
    assert_eq!(ended, vec![theirs.pid()]);
    let mut theirs = theirs;
    theirs.0.wait().unwrap();
    assert!(os::pid_runs(mine.pid()));
    assert_eq!(
        sweep_persons(&c, &["nobody".to_owned()], Duration::ZERO),
        Vec::<i64>::new()
    );
}

#[test]
fn two_takers_at_once_never_both_win() {
    // The stale-claim case is the dangerous one: both judge the old claim dead, and one must not delete the other's fresh one.
    let sleepers: Vec<Sleeper> = (0..4).map(|_| Sleeper::new()).collect();
    for round in 0..40 {
        let t = Tmp::new();
        if round % 2 == 0 {
            t.put("s-1.claim", &format!("{{\"pid\": {}}}", dead_pid()));
        }
        let wins = std::thread::scope(|s| {
            let t = &t;
            let hs: Vec<_> = sleepers
                .iter()
                .map(|sl| {
                    s.spawn(move || {
                        let c = ctx(t, &no_env);
                        take(&c, "s-1", sl.pid()).is_ok()
                    })
                })
                .collect();
            hs.into_iter()
                .filter_map(|h| h.join().unwrap().then_some(()))
                .count()
        });
        assert_eq!(wins, 1, "round {round}");
        assert!(live_holder(&t.reg(), "s-1").is_some());
    }
}

// --- resolve -----------------------------------------------------------------------------------------------------------

struct Fake {
    first: Option<Value>,
    again: Option<Value>,
    asked: std::cell::RefCell<Vec<String>>,
}

impl Host for Fake {
    fn resolve_thread(&self, thread: &str) -> Option<Value> {
        let mut asked = self.asked.borrow_mut();
        asked.push(thread.to_owned());
        if asked.len() == 1 {
            self.first.clone()
        } else {
            self.again.clone()
        }
    }
}

fn fake(first: Option<Value>, again: Option<Value>) -> Fake {
    Fake {
        first,
        again,
        asked: std::cell::RefCell::new(Vec::new()),
    }
}

fn transcript(t: &Tmp, sid: &str, cwd: &Path) {
    let dir = t.0.join("home").join(".claude").join("projects").join("p");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(format!("{sid}.jsonl")),
        format!(
            "{{\"type\":\"summary\"}}\n{{\"cwd\":\"{}\"}}\n",
            cwd.display()
        ),
    )
    .unwrap();
}

#[test]
fn a_raw_session_id_resolves_with_no_host_and_takes_the_claim() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let work = t.0.join("work");
    fs::create_dir_all(&work).unwrap();
    transcript(&t, "abc-1", &work);
    let holder = Sleeper::new();
    let r = resolve(&c, &NoHost, "coder", "abc-1", holder.pid(), Duration::ZERO).unwrap();
    assert_eq!(
        r,
        Resolved {
            sid: "abc-1".into(),
            cwd: work.display().to_string()
        }
    );
    assert_eq!(
        r.shell(),
        format!("r_sid='abc-1' r_cwd='{}' r_from='abc-1'", work.display())
    );
    assert_eq!(on_disk(&t, "abc-1").pid, Some(holder.pid()));
    // A second terminal is refused, and its failed attempt does not give the first one's claim back.
    let again = Sleeper::new();
    let err = resolve(&c, &NoHost, "coder", "abc-1", again.pid(), Duration::ZERO).unwrap_err();
    assert_eq!(
        err,
        format!(
            "that session is open in another writer (pid {}, terminal); close it first",
            holder.pid()
        )
    );
    assert_eq!(on_disk(&t, "abc-1").pid, Some(holder.pid()));
}

#[test]
fn an_unknown_thread_says_whether_the_host_was_there() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let e = resolve(&c, &NoHost, "coder", "nope", me(), Duration::ZERO).unwrap_err();
    assert_eq!(
        e,
        "unknown thread \"nope\" (the Kith host is not running, so only a Claude session id can be resumed)"
    );
    let host = fake(Some(serde_json::json!({"error": "unknown thread"})), None);
    assert_eq!(
        resolve(&c, &host, "coder", "nope", me(), Duration::ZERO).unwrap_err(),
        "unknown thread \"nope\""
    );
    // Text that is no session id is never a path to look under.
    assert!(resolve(&c, &NoHost, "coder", "../../etc", me(), Duration::ZERO).is_err());
    assert!(resolve(&c, &NoHost, "coder", "*", me(), Duration::ZERO).is_err());
}

#[test]
fn the_host_names_the_thread_and_is_asked_again_with_the_claim_held() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let work = t.0.join("work");
    fs::create_dir_all(&work).unwrap();
    let answer =
        serde_json::json!({"sessionId": "s-9", "agent": "coder", "cwd": work, "claims": 1});
    let host = fake(Some(answer.clone()), Some(answer.clone()));
    let holder = Sleeper::new();
    let r = resolve(&c, &host, "coder", "thread-x", holder.pid(), Duration::ZERO).unwrap();
    assert_eq!(r.sid, "s-9");
    assert_eq!(*host.asked.borrow(), ["thread-x", "s-9"]);
    assert_eq!(on_disk(&t, "s-9").pid, Some(holder.pid()));
}

#[test]
fn a_busy_host_or_the_wrong_agent_or_a_silent_host_refuses_and_gives_the_claim_back() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let base = serde_json::json!({"sessionId": "s-9", "agent": "coder", "cwd": "", "claims": 1});
    let busy = serde_json::json!({"sessionId": "s-9", "agent": "coder", "cwd": "", "claims": 1, "hostBusy": true});
    let cases = [
        (
            fake(Some(base.clone()), Some(busy)),
            "the app is answering in that thread right now; try again when it has finished",
        ),
        (
            fake(Some(base.clone()), None),
            "the Kith host stopped answering; try again",
        ),
        (
            fake(
                Some(base.clone()),
                Some(serde_json::json!({"error": "gone"})),
            ),
            "the Kith host stopped answering; try again",
        ),
        (
            fake(
                Some(serde_json::json!({"sessionId": "s-9", "agent": "other", "claims": 1})),
                None,
            ),
            "that thread belongs to other, not coder: kith other --resume t",
        ),
    ];
    for (host, want) in cases {
        let err = resolve(&c, &host, "coder", "t", me(), Duration::ZERO).unwrap_err();
        assert_eq!(err, want);
        assert!(
            !claim_path(&t.reg(), "s-9", false).exists(),
            "{want}: the claim is given back"
        );
    }
}

#[test]
fn a_host_from_before_claims_only_resumes_what_a_terminal_started() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let work = t.0.join("work");
    fs::create_dir_all(&work).unwrap();
    // Names a session but has no `claims: 1`: it cannot vouch for it.
    let old = serde_json::json!({"sessionId": "s-9", "agent": "coder"});
    let e = resolve(
        &c,
        &fake(Some(old.clone()), None),
        "coder",
        "s-9",
        me(),
        Duration::ZERO,
    )
    .unwrap_err();
    assert!(
        e.starts_with("this Kith host is too old to continue app threads"),
        "{e}"
    );
    t.put(
        "s-9.json",
        &format!(
            "{{\"v\":1,\"sessionId\":\"s-9\",\"agent\":\"coder\",\"cwd\":\"{}\"}}",
            work.display()
        ),
    );
    let r = resolve(
        &c,
        &fake(Some(old), None),
        "coder",
        "s-9",
        me(),
        Duration::ZERO,
    )
    .unwrap();
    assert_eq!(r.cwd, work.display().to_string());
    let missing = serde_json::json!({"error": "unknown method", "unknownMethod": true});
    t.put("s-8.json", "{\"agent\":\"other\"}");
    let e = resolve(
        &c,
        &fake(Some(missing), None),
        "coder",
        "s-8",
        me(),
        Duration::ZERO,
    )
    .unwrap_err();
    assert_eq!(e, "that session belongs to other, not coder");
}

#[test]
fn a_session_open_in_another_terminal_or_with_its_folder_gone_is_refused() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let gone = t.0.join("gone");
    transcript(&t, "abc-2", &gone);
    let e = resolve(&c, &NoHost, "coder", "abc-2", me(), Duration::ZERO).unwrap_err();
    assert_eq!(
        e,
        format!("the session's folder {} is gone", gone.display())
    );
    assert!(!claim_path(&t.reg(), "abc-2", false).exists());
    // The registry's claude is alive and names the session in its own sessions folder.
    let claude = Sleeper::new();
    let work = t.0.join("work");
    fs::create_dir_all(&work).unwrap();
    transcript(&t, "abc-3", &work);
    let sessions = t.0.join("home").join(".claude").join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join(format!("{}.json", claude.pid())),
        "{\"sessionId\":\"abc-3\"}",
    )
    .unwrap();
    t.put(
        "abc-3.json",
        &format!(
            "{{\"v\":1,\"sessionId\":\"abc-3\",\"agent\":\"coder\",\"pid\":{}}}",
            claude.pid()
        ),
    );
    let e = resolve(&c, &NoHost, "coder", "abc-3", me(), Duration::ZERO).unwrap_err();
    assert_eq!(
        e,
        format!(
            "that session is open in another terminal (pid {}); close it first",
            claude.pid()
        )
    );
    assert!(!claim_path(&t.reg(), "abc-3", false).exists());
}

#[test]
fn a_takeover_in_progress_refuses_a_resume() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let work = t.0.join("work");
    fs::create_dir_all(&work).unwrap();
    transcript(&t, "abc-4", &work);
    let host = Sleeper::new();
    take_claim(
        &c,
        "abc-4",
        host.pid(),
        &Take {
            takeover: true,
            ..Take::new("app")
        },
    )
    .unwrap()
    .unwrap();
    let e = resolve(&c, &NoHost, "coder", "abc-4", me(), Duration::ZERO).unwrap_err();
    assert_eq!(
        e,
        "that session is being taken over in the app right now; try again in a moment"
    );
    assert!(!claim_path(&t.reg(), "abc-4", false).exists());
}

// --- record ------------------------------------------------------------------------------------------------------------

fn run_env(pairs: &[(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
    let pairs: Vec<_> = pairs.to_vec();
    move |k| {
        pairs
            .iter()
            .find(|(n, _)| *n == k)
            .map(|(_, v)| (*v).to_owned())
    }
}

#[test]
fn record_lists_a_terminal_run_in_the_order_the_host_reads() {
    let t = Tmp::new();
    let env = run_env(&[
        ("KITH_AGENT", "coder"),
        ("KITH_TERMINAL_SESSION", "1"),
        ("KITH_PROJECT", "kith"),
        ("KITH_ACCOUNT_ID", "claude"),
    ]);
    let c = ctx(&t, &env);
    let hook = r#"{"session_id":"s-1","cwd":"/work/x","transcript_path":"/h/.claude/projects/p/s-1.jsonl","source":"startup"}"#;
    registry::record(&c, hook).unwrap();
    let raw = fs::read_to_string(t.reg().join("s-1.json")).unwrap();
    let v: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        v.as_object().unwrap().keys().collect::<Vec<_>>(),
        [
            "v",
            "sessionId",
            "agent",
            "cwd",
            "transcriptPath",
            "pid",
            "project",
            "source",
            "resumedFrom",
            "startedAt",
            "account",
            "kind",
            "label",
            "procStart"
        ]
    );
    assert_eq!(
        (
            v["kind"].as_str(),
            v["agent"].as_str(),
            v["project"].as_str(),
            v["resumedFrom"].is_null()
        ),
        (Some("terminal"), Some("coder"), Some("kith"), true)
    );
    assert!(
        raw.starts_with("{\"v\": 1, \"sessionId\": \"s-1\""),
        "{raw}"
    );
}

#[test]
fn record_lists_nobody_else() {
    let t = Tmp::new();
    let hook = r#"{"session_id":"s-1","cwd":"/work/x"}"#;
    let silent: [&[(&str, &str)]; 6] = [
        &[("KITH_AGENT", "coder")],
        &[
            ("KITH_AGENT", "coder"),
            ("KITH_TERMINAL_SESSION", "1"),
            ("KITH_PERSON", "p1"),
        ],
        &[
            ("KITH_AGENT", "coder"),
            ("KITH_TERMINAL_SESSION", "1"),
            ("KITH_HIDE_SESSION", "1"),
        ],
        &[
            ("KITH_AGENT", "coder"),
            ("KITH_TERMINAL_SESSION", "1"),
            ("KITH_VIA", "serve"),
        ],
        &[("KITH_AGENT", "coder"), ("KITH_RUN_KIND", "bridge")],
        &[("KITH_TERMINAL_SESSION", "1")],
    ];
    for pairs in silent {
        let env = run_env(pairs);
        registry::record(&ctx(&t, &env), hook).unwrap();
    }
    let env = run_env(&[("KITH_AGENT", "coder"), ("KITH_TERMINAL_SESSION", "1")]);
    registry::record(
        &ctx(&t, &env),
        r#"{"session_id":"s-1","cwd":"/w/.claude/worktrees/a"}"#,
    )
    .unwrap();
    assert!(!t.reg().join("s-1.json").exists());
    assert!(registry::record(&ctx(&t, &env), "not json").is_err());
}

#[test]
fn record_labels_a_one_shot_and_keeps_when_a_compacted_session_began() {
    let t = Tmp::new();
    let env = run_env(&[
        ("KITH_AGENT", "coder"),
        ("KITH_RUN_KIND", "ask"),
        ("KITH_RUN_LABEL", " architect "),
    ]);
    let c = ctx(&t, &env);
    registry::record(&c, r#"{"session_id":"s-1","cwd":"/w"}"#).unwrap();
    let first: Value =
        serde_json::from_str(&fs::read_to_string(t.reg().join("s-1.json")).unwrap()).unwrap();
    assert_eq!(
        (first["kind"].as_str(), first["label"].as_str()),
        (Some("ask"), Some("asked by architect"))
    );
    let mut old = first.clone();
    old["startedAt"] = "2020-01-02T03:04:05Z".into();
    fs::write(t.reg().join("s-1.json"), old.to_string()).unwrap();
    registry::record(&c, r#"{"session_id":"s-1","cwd":"/w","source":"compact"}"#).unwrap();
    let second: Value =
        serde_json::from_str(&fs::read_to_string(t.reg().join("s-1.json")).unwrap()).unwrap();
    assert_eq!(
        (second["startedAt"].as_str(), second["source"].as_str()),
        (Some("2020-01-02T03:04:05Z"), Some("compact"))
    );
}

#[test]
fn record_finds_the_claude_above_it() {
    let t = Tmp::new();
    // The test's own parent is the "claude": its sessions file names the session.
    let sessions = t.0.join("home").join(".claude").join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join(format!("{}.json", os::parent_pid())),
        "{\"sessionId\":\"s-1\"}",
    )
    .unwrap();
    let env = run_env(&[("KITH_AGENT", "coder"), ("KITH_RUN_KIND", "schedule")]);
    registry::record(&ctx(&t, &env), r#"{"session_id":"s-1","cwd":"/w"}"#).unwrap();
    let v: Value =
        serde_json::from_str(&fs::read_to_string(t.reg().join("s-1.json")).unwrap()).unwrap();
    assert_eq!(v["pid"], os::parent_pid());
    assert!(
        v["procStart"].is_string(),
        "a one-shot carries its process' start for the host's liveness"
    );
}

#[test]
fn the_iso_clock_is_python_shaped() {
    let s = iso_utc();
    assert_eq!(s.len(), 20);
    assert!(s.starts_with("20") && s.ends_with('Z') && s.as_bytes()[10] == b'T');
}

// --- the verbs ---------------------------------------------------------------------------------------------------------

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn the_verbs_answer_like_kith_sessions() {
    let t = Tmp::new();
    let env = run_env(&[("KITH_CLAIM_WAIT", "0")]);
    let c = ctx(&t, &env);
    let stdin = || String::new();
    let other = Sleeper::new();
    let pid = other.pid().to_string();
    let go = |a: &[&str]| cli::run(&c, &NoHost, &args(a), &stdin);
    assert_eq!(go(&["claim", "s-1", &pid]), cli::Out::default());
    let busy = go(&["claim", "s-1", &me().to_string()]);
    assert_eq!((busy.code, busy.stdout.as_str()), (1, ""));
    assert_eq!(
        busy.stderr,
        format!("kith: that session is busy: held by oneshot (pid {pid}); try again in a moment\n")
    );
    assert_eq!(go(&["terminal", "s-2", &pid, "p1", "coder"]).code, 0);
    assert_eq!(on_disk(&t, "s-2").person.as_deref(), Some("p1"));
    let holder = go(&["holder", "s-1"]);
    assert_eq!(holder.code, 0);
    assert!(
        holder.stdout.starts_with(&format!("{{\"pid\": {pid}, ")),
        "{}",
        holder.stdout
    );
    assert_eq!(go(&["holder", "s-0"]).code, 1);
    assert_eq!(go(&["release", &pid]), cli::Out::default());
    assert!(
        fs::read_dir(t.reg())
            .unwrap()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().ends_with(".claim"))
    );
    assert_eq!(go(&["sweep", "nobody"]).stdout, "swept 0\n");
    assert_eq!(go(&["proc", &pid, "p1", "coder"]).code, 0);
    assert_eq!(go(&["claim", "s-1"]).code, 1, "usage");
    assert_eq!(go(&[]).code, 1);
    assert_eq!(
        go(&["claim", "s-1", "x"]).code,
        1,
        "a pid that is no number"
    );
}
