#![allow(clippy::unwrap_used, clippy::many_single_char_names)]

use std::time::{Duration, Instant};

use kith_holder::proto::HeldId;
use serde_json::json;

use super::*;

fn held(id: u32) -> HeldId {
    HeldId {
        instance: "i".into(),
        id,
    }
}

// --- tickets ---------------------------------------------------------------------------------------------------------

#[test]
fn a_ticket_works_once() {
    let t = Tickets::default();
    let ticket = t.issue(Role::Owner, held(7), "sid", "kai").unwrap();
    assert_eq!(ticket.len(), 64);
    assert_eq!(
        t.redeem(&ticket),
        Ok(Redeemed {
            role: Role::Owner,
            held: held(7),
            session: "sid".into(),
            agent: "kai".into()
        })
    );
    assert_eq!(t.redeem(&ticket), Err(TicketRefused::Unknown));
}

#[test]
fn an_unknown_or_expired_ticket_is_refused_and_an_expired_one_is_used_up_too() {
    let t = Tickets::default();
    assert_eq!(t.redeem("nope"), Err(TicketRefused::Unknown));
    let now = Instant::now();
    let ticket = t
        .issue_at(now, Duration::from_secs(60), Role::Owner, held(1), "s", "a")
        .unwrap();
    assert_eq!(
        t.redeem_at(now + Duration::from_secs(61), &ticket),
        Err(TicketRefused::Expired)
    );
    assert_eq!(t.redeem_at(now, &ticket), Err(TicketRefused::Unknown));
}

#[test]
fn a_ticket_fixes_the_role_an_attach_can_get() {
    let t = Tickets::default();
    let grant = |role, asked, takeover| {
        let ticket = t.issue(role, held(1), "s", "a").unwrap();
        t.redeem(&ticket).unwrap().grant(asked, takeover)
    };
    assert_eq!(grant(Role::Owner, Role::Owner, false), Some(Role::Owner));
    assert_eq!(grant(Role::Owner, Role::Owner, true), Some(Role::Owner));
    assert_eq!(
        grant(Role::Owner, Role::Observer, false),
        Some(Role::Observer)
    );
    assert_eq!(
        grant(Role::Observer, Role::Observer, false),
        Some(Role::Observer)
    );
    assert_eq!(
        grant(Role::Observer, Role::Owner, false),
        None,
        "never a higher role"
    );
    assert_eq!(
        grant(Role::Observer, Role::Observer, true),
        None,
        "only an owner ticket takes over"
    );
}

#[test]
fn tickets_are_random_kept_hashed_and_bounded() {
    let t = Tickets::default();
    let a = t.issue(Role::Owner, held(1), "s", "a").unwrap();
    let b = t.issue(Role::Owner, held(1), "s", "a").unwrap();
    assert_ne!(a, b);
    {
        let map = t.by_hash.lock().unwrap();
        assert!(
            map.keys().all(|k| *k != a && *k != b),
            "the ticket itself is not stored"
        );
        assert!(map.contains_key(&hash(&a)));
    }
    for i in 0..(MAX_TICKETS + 10) {
        t.issue(Role::Owner, held(u32::try_from(i).unwrap()), "s", "a")
            .unwrap();
    }
    assert!(t.by_hash.lock().unwrap().len() <= MAX_TICKETS);
}

// --- the env filter --------------------------------------------------------------------------------------------------

#[test]
fn the_env_filter_drops_run_tokens_and_their_sockets_and_keeps_the_rest() {
    let env = json!({
        "KITH_RUN_TOKEN": "t", "KITH_RUN_SOCKET": "s", "KITH_RUN_KIND": "ask", "KITH_RUN_ANYTHING": "x",
        "KITH_ASK_TOKEN": "t", "KITH_ASK_SOCKET": "s", "HQ_ASK_TOKEN": "t", "HQ_ASK_SOCKET": "s",
        "HQ_RUN_OUTBOX": "o", "HQ_RUN_TOKEN": "t",
        "PATH": "/bin", "KITH_AGENT": "kai", "ANTHROPIC_API_KEY": "k", "KITH_RUNNER": "kept", "N": 3
    });
    let got = filter_env(env.as_object().unwrap());
    let names: Vec<&str> = got.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        ["ANTHROPIC_API_KEY", "KITH_AGENT", "KITH_RUNNER", "PATH"]
    );
}

#[test]
fn only_plain_names_are_session_ids() {
    for ok in ["a", "0b1c-2_d", "123e4567-e89b-12d3-a456-426614174000"] {
        assert!(safe_id(ok), "{ok}");
    }
    for bad in ["", ".x", "../x", "a/b", "a b", "a\0b", &"x".repeat(129)] {
        assert!(!safe_id(bad), "{bad:?}");
    }
}

// --- the claim file --------------------------------------------------------------------------------------------------

#[cfg(unix)]
mod claims {
    use std::path::{Path, PathBuf};

    use serde_json::Value;

    use super::super::claim;

    fn file(state: &Path, sid: &str) -> PathBuf {
        state.join("live/terminal").join(format!("{sid}.claim"))
    }

    #[test]
    fn a_live_claim_keeps_a_second_taker_out_and_release_frees_it() {
        let d = tempfile::tempdir().unwrap();
        let me = std::process::id();
        assert!(claim::take(d.path(), "s1", me, "terminal").unwrap().is_ok());
        let rec: Value =
            serde_json::from_str(&std::fs::read_to_string(file(d.path(), "s1")).unwrap()).unwrap();
        assert_eq!(rec["pid"], me);
        assert_eq!(rec["kind"], "terminal");
        assert!(rec["at"].as_str().unwrap().ends_with('Z'));
        let held = claim::take(d.path(), "s1", me, "terminal")
            .unwrap()
            .expect_err("held");
        assert_eq!(held.pid, me);
        claim::release(d.path(), "s1", me + 1); // not the holder: leaves it
        assert!(file(d.path(), "s1").is_file());
        claim::release(d.path(), "s1", me);
        assert!(!file(d.path(), "s1").exists());
        assert!(claim::take(d.path(), "s1", me, "terminal").unwrap().is_ok());
    }

    #[test]
    fn the_starting_record_is_kind_starting_and_a_retarget_makes_it_a_terminal_claim() {
        let d = tempfile::tempdir().unwrap();
        let me = std::process::id();
        assert!(claim::take(d.path(), "s2", me, "starting").unwrap().is_ok());
        let kind = || -> Value {
            serde_json::from_str(&std::fs::read_to_string(file(d.path(), "s2")).unwrap()).unwrap()
        };
        assert_eq!(kind()["kind"], "starting");
        let held = claim::take(d.path(), "s2", me, "terminal")
            .unwrap()
            .expect_err("held");
        assert_eq!(held.kind, "starting");
        // Not ours to move: nothing changes.
        assert!(!claim::retarget(d.path(), "s2", me + 1, 1).unwrap());
        assert_eq!(kind()["kind"], "starting");
        assert!(claim::retarget(d.path(), "s2", me, me + 1).unwrap());
        assert_eq!(kind()["kind"], "terminal");
    }

    #[test]
    fn a_live_pid_whose_start_time_is_not_known_keeps_its_claim() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("live/terminal");
        std::fs::create_dir_all(&dir).unwrap();
        // Pid 1 runs (and is not ours to signal): with no recorded start there is nothing to say it is not the claimer.
        std::fs::write(dir.join("p1.claim"), r#"{"pid":1,"kind":"terminal"}"#).unwrap();
        let me = std::process::id();
        let held = claim::take(d.path(), "p1", me, "terminal")
            .unwrap()
            .expect_err("live");
        assert_eq!(held.pid, 1);
        assert!(dir.join("p1.claim").is_file(), "never deleted");
    }

    #[test]
    fn taking_a_claim_ends_the_sessions_intent_and_decision() {
        let d = tempfile::tempdir().unwrap();
        for sub in ["sessions/intent", "sessions/decisions"] {
            std::fs::create_dir_all(d.path().join(sub)).unwrap();
            std::fs::write(d.path().join(sub).join("s3.json"), "{}").unwrap();
            std::fs::write(d.path().join(sub).join("other.json"), "{}").unwrap();
        }
        assert!(
            claim::take(d.path(), "s3", std::process::id(), "terminal")
                .unwrap()
                .is_ok()
        );
        for sub in ["sessions/intent", "sessions/decisions"] {
            assert!(!d.path().join(sub).join("s3.json").exists(), "{sub}");
            assert!(d.path().join(sub).join("other.json").exists(), "{sub}");
        }
    }

    #[test]
    fn a_stale_claim_is_replaced_and_a_retarget_moves_it_to_the_stub() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("live/terminal");
        std::fs::create_dir_all(&dir).unwrap();
        // A pid that is surely not running, and garbage.
        std::fs::write(
            dir.join("old.claim"),
            r#"{"pid":4000000,"procStart":"x","kind":"terminal"}"#,
        )
        .unwrap();
        std::fs::write(dir.join("junk.claim"), "not json").unwrap();
        let me = std::process::id();
        assert!(
            claim::take(d.path(), "old", me, "terminal")
                .unwrap()
                .is_ok()
        );
        assert!(
            claim::take(d.path(), "junk", me, "terminal")
                .unwrap()
                .is_ok()
        );
        let mut sleeper = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        assert!(claim::retarget(d.path(), "old", me, sleeper.id()).unwrap());
        let rec: Value =
            serde_json::from_str(&std::fs::read_to_string(file(d.path(), "old")).unwrap()).unwrap();
        assert_eq!(rec["pid"], sleeper.id());
        // Another taker now sees the sleeper's claim as live.
        assert_eq!(
            claim::take(d.path(), "old", me, "terminal")
                .unwrap()
                .err()
                .unwrap()
                .pid,
            sleeper.id()
        );
        sleeper.kill().unwrap();
        sleeper.wait().unwrap();
        assert!(
            claim::take(d.path(), "old", me, "terminal")
                .unwrap()
                .is_ok(),
            "dead holder, free claim"
        );
    }
}

// --- through a real daemon and a real holder -------------------------------------------------------------------------

#[cfg(unix)]
pub(crate) mod daemon {
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};

    use std::sync::atomic::Ordering;
    use std::sync::mpsc;

    use super::super::{Layout, Placed, Req, SpawnReq, Terminals, TicketRefused, claim};
    use crate::daemon::{Config, Daemon};
    use crate::holder_link::HolderConfig;
    use crate::{Reached, reach};

    /// A short folder: a Unix socket path holds about a hundred bytes.
    pub(crate) fn sandbox() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("kto")
            .tempdir_in("/tmp")
            .unwrap()
    }

    pub(crate) fn holder_bin() -> PathBuf {
        let exe = std::env::current_exe().unwrap();
        let profile = exe.parent().and_then(Path::parent).unwrap().to_owned();
        let beside = profile.join("kith-holder");
        if !beside.is_file() {
            let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
            let built = std::process::Command::new(cargo)
                .args(["build", "--manifest-path"])
                .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../holder/Cargo.toml"))
                .status()
                .unwrap();
            assert!(
                built.success() && beside.is_file(),
                "kith-holder is not built"
            );
        }
        beside
    }

    /// Held while a test writes its stub script and while a holder is started. A fork while another thread has the
    /// script open for writing lets the forked process keep that write handle for a moment, and the script cannot be
    /// executed (ETXTBSY) until it lets go: the holder's spawn of the stub then fails, and the test with it.
    static FORKING: std::sync::Mutex<()> = std::sync::Mutex::new(());

    pub(crate) fn forking() -> std::sync::MutexGuard<'static, ()> {
        FORKING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A script that behaves like the start stub for these tests: `--lock-dir D --key K -- engine...`.
    pub(crate) fn fake_stub(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("kith-stub");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// Records its own arguments in the lock dir, then runs the engine as the real stub does.
    pub(crate) const RUNNING: &str = r#"lock="$2"; key="$4"; shift 5
printf '%s\n' "$lock" "$key" "$@" > "$lock/$key.args"
"$@""#;

    pub(crate) fn config(stub: Option<PathBuf>) -> Config {
        Config {
            holder_on: true,
            key_load: None,
            host: false,
            wait_for_embedded: Duration::from_secs(10),
            holder: HolderConfig {
                exe: Some(holder_bin()),
                idle: Duration::from_secs(5),
                poll: Duration::from_millis(100),
                retry_min: Duration::from_millis(50),
                retry_max: Duration::from_millis(200),
                kill_wait: Duration::from_secs(5),
                command: None,
                handshake: Duration::from_secs(5),
                stub,
                lease: Duration::from_secs(30),
            },
        }
    }

    pub(crate) fn until(what: &str, mut f: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(15);
        while Instant::now() < end {
            if f() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("gave up waiting for {what}");
    }

    pub(crate) fn serving(m: &Path, cfg: Config) -> Daemon {
        let p = crate::tests::paths(m);
        let d = Daemon::spawn_with(&p, cfg);
        d.ready(Duration::from_secs(10)).unwrap();
        d
    }

    pub(crate) fn open(m: &Path, session: &str, argv: &[&str], env: &Value) -> Reached {
        let params =
            json!({"agent": "kai", "session": session, "argv": argv, "cwd": "/tmp", "env": env});
        reach(&crate::tests::paths(m), "terminal.open", &params, false)
    }

    pub(crate) fn answered(r: Reached) -> Value {
        match r {
            Reached::Answered(v) => v,
            other => panic!("{other:?}"),
        }
    }

    pub(crate) fn plain() -> Value {
        json!({"PATH": "/usr/bin:/bin"})
    }

    /// The daemon, its holder, and the folder everything is in; the holder is ended when the test is.
    pub(crate) struct Rig {
        pub(crate) t: tempfile::TempDir,
        pub(crate) m: PathBuf,
        pub(crate) d: Daemon,
    }

    impl Rig {
        pub(crate) fn new(stub_body: &str) -> Rig {
            let _forking = forking();
            let t = sandbox();
            let m = t.path().join("m");
            let stub = fake_stub(t.path(), stub_body);
            let d = serving(&m, config(Some(stub)));
            let p = crate::tests::paths(&m);
            until("the holder link", || {
                crate::status(&p)["holder"]["state"] == "connected"
            });
            Rig { t, m, d }
        }

        fn claim_file(&self, sid: &str) -> PathBuf {
            self.m.join("live/terminal").join(format!("{sid}.claim"))
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            // The holder outlives the daemon by design; this test's one must not outlive the test.
            let status = crate::status(&crate::tests::paths(&self.m));
            if let Some(pid) = status["holder"]["pid"].as_u64() {
                let _ = std::process::Command::new("kill")
                    .arg(pid.to_string())
                    .status();
            }
            self.d.stop();
        }
    }

    #[test]
    fn open_runs_the_engine_under_the_stub_in_the_holder_and_hands_back_one_ticket() {
        let rig = Rig::new(RUNNING);
        let out = rig.t.path().join("env.txt");
        let script = format!("env > '{}'; echo ready; cat", out.display());
        let env = json!({
            "PATH": std::env::var("PATH").unwrap_or_default(), "KITH_AGENT": "kai", "FOO": "bar",
            "KITH_RUN_TOKEN": "secret-run", "KITH_RUN_SOCKET": "/s", "KITH_ASK_TOKEN": "ask", "KITH_RUN_KIND": "ask"
        });
        let a = answered(open(&rig.m, "sess-1", &["sh", "-c", &script], &env));
        assert_eq!(a["status"], "open", "{a}");
        let ticket = a["ticket"].as_str().unwrap().to_owned();

        // The stub got its lock dir and key, then the engine's command after `--`.
        let locks = Layout::new(&rig.m).dir.join("locks");
        until("the stub's record", || locks.join("sess-1.args").is_file());
        let args = std::fs::read_to_string(locks.join("sess-1.args")).unwrap();
        let lines: Vec<&str> = args.lines().collect();
        assert_eq!(lines[0], locks.to_string_lossy());
        assert_eq!(&lines[1..4], ["sess-1", "sh", "-c"]);

        // The engine's env: the launch's, less the run tokens.
        until("the engine's env", || {
            std::fs::metadata(&out).is_ok_and(|m| m.len() > 0)
        });
        let seen = std::fs::read_to_string(&out).unwrap();
        assert!(seen.contains("FOO=bar") && seen.contains("KITH_AGENT=kai"));
        for gone in [
            "KITH_RUN_TOKEN",
            "KITH_RUN_SOCKET",
            "KITH_ASK_TOKEN",
            "KITH_RUN_KIND",
            "secret-run",
        ] {
            assert!(!seen.contains(gone), "{gone} reached the terminal:\n{seen}");
        }

        // Today's claim file names the stub's pid, and a second open of the session is busy.
        let claim: Value =
            serde_json::from_str(&std::fs::read_to_string(rig.claim_file("sess-1")).unwrap())
                .unwrap();
        assert_eq!(claim["pid"], a["pid"]);
        let again = answered(open(&rig.m, "sess-1", &["sh", "-c", "cat"], &plain()));
        assert_eq!(again["status"], "busy", "{again}");
        assert_eq!(again["held_by"]["pid"], a["pid"]);

        // The ticket is single-use, and bound to the held process.
        let t = &rig.d.shared().terminals;
        let got = t.redeem(&ticket).unwrap();
        assert_eq!(
            (got.session.as_str(), got.agent.as_str()),
            ("sess-1", "kai")
        );
        assert_eq!(json!(got.held.id), a["held_id"]["id"]);
        assert_eq!(t.redeem(&ticket), Err(TicketRefused::Unknown));
    }

    #[test]
    fn a_claim_that_cannot_be_handed_to_the_stub_ends_the_terminal_and_is_given_back() {
        let rig = Rig::new(
            r#"lock="$2"; key="$4"; shift 5
echo $$ > "$lock/$key.pid"
"$@""#,
        );
        rig.d
            .shared()
            .terminals
            .fail_retarget
            .store(true, Ordering::SeqCst);
        let params = json!({"agent": "kai", "session": "sess-r", "argv": ["sh", "-c", "cat"], "cwd": "/tmp", "env": plain()});
        match reach(
            &crate::tests::paths(&rig.m),
            "terminal.open",
            &params,
            false,
        ) {
            Reached::Refused(e) => assert!(e.to_string().contains("was ended"), "{e}"),
            other => panic!("{other:?}"),
        }
        assert!(
            !rig.claim_file("sess-r").exists(),
            "claim given back, not left on the daemon's pid"
        );
        let pid_file = Layout::new(&rig.m).dir.join("locks/sess-r.pid");
        let pid: u32 = std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        until("the terminal to be ended", || {
            !std::process::Command::new("kill")
                .args(["-s", "0", &pid.to_string()])
                .output()
                .unwrap()
                .status
                .success()
        });
    }

    #[test]
    fn a_terminal_started_after_its_caller_gave_up_is_ended() {
        let rig = Rig::new(
            r#"lock="$2"; key="$4"; shift 5
echo $$ > "$lock/$key.pid"
"$@""#,
        );
        // The supervisor watches a new stub for 600 ms before it answers: this caller stops waiting after 100.
        *rig.d.shared().terminals.answer_wait.lock().unwrap() = Some(Duration::from_millis(100));
        let late = crate::holder_link::LATE_ENDS.load(Ordering::SeqCst);
        let a = answered(open(&rig.m, "sess-l", &["sh", "-c", "cat"], &plain()));
        assert_eq!(a["status"], "direct", "{a}");
        assert!(!rig.claim_file("sess-l").exists(), "claim given back");
        let pid_file = Layout::new(&rig.m).dir.join("locks/sess-l.pid");
        until("the stub to have started", || pid_file.is_file());
        let pid: u32 = std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        until("the late path to take the end_held branch", || {
            crate::holder_link::LATE_ENDS.load(Ordering::SeqCst) > late
        });
        until("the late terminal to be ended", || {
            !std::process::Command::new("kill")
                .args(["-s", "0", &pid.to_string()])
                .output()
                .unwrap()
                .status
                .success()
        });
    }

    #[test]
    fn a_link_that_drops_keeps_the_queued_ends_and_answers_the_spawns() {
        let t = Terminals::new();
        let (reply, answer) = mpsc::sync_channel(0);
        t.queue
            .send(Req::Spawn(SpawnReq {
                spec: kith_holder::proto::Spawn {
                    conv: "c".into(),
                    claim: 1,
                    generation: 1,
                    argv: vec!["cat".into()],
                    env: std::collections::BTreeMap::default(),
                    cwd: String::new(),
                    mode: kith_holder::proto::Mode::Pty,
                    cols: 80,
                    rows: 24,
                    kill_on_lease_loss: false,
                    stub: None,
                    lock_dir: None,
                },
                agent: "kai".into(),
                deadline: Instant::now() + Duration::from_secs(5),
                busy_wait: Duration::ZERO,
                reply,
            }))
            .unwrap();
        t.queue.send(Req::End(super::held(9))).unwrap();
        let waiter = std::thread::spawn(move || answer.recv_timeout(Duration::from_secs(5)));
        t.set_link(false);
        assert!(matches!(waiter.join().unwrap(), Ok(Placed::Failed(_))));
        match t.next(Duration::ZERO) {
            Some(Req::End(h)) => assert_eq!(h, super::held(9)),
            _ => panic!("the End was dropped"),
        }
        assert!(t.next(Duration::ZERO).is_none());
    }

    #[test]
    fn a_holder_reached_through_a_command_answers_direct() {
        let t = sandbox();
        let m = t.path().join("m");
        let stub = fake_stub(t.path(), RUNNING);
        let mut cfg = config(Some(stub));
        cfg.holder.exe = None;
        // A command link, whether or not it connects: the holder it reaches is on another computer.
        cfg.holder.command = Some(vec!["/usr/bin/false".into()]);
        let d = serving(&m, cfg);
        let p = crate::tests::paths(&m);
        until("the supervisor to try the link", || {
            crate::status(&p)["holder"]["state"] != "starting"
        });
        let a = answered(open(&m, "sess-c", &["cat"], &json!({})));
        assert_eq!(a["status"], "direct", "{a}");
        assert!(a["note"].as_str().unwrap().contains("another computer"));
        assert!(!m.join("live/terminal/sess-c.claim").exists());
        d.stop();
    }

    #[test]
    fn a_stub_that_says_busy_gives_the_claim_back() {
        let rig = Rig::new("exit 75");
        let a = answered(open(&rig.m, "sess-b", &["sh", "-c", "cat"], &plain()));
        assert_eq!(a["status"], "busy", "{a}");
        assert!(!rig.claim_file("sess-b").exists(), "claim released");
    }

    #[test]
    fn a_spawn_the_holder_cannot_start_gives_the_claim_back() {
        let rig = Rig::new(RUNNING);
        // A cwd that is not there: the holder's spawn fails.
        let params = json!({"agent": "kai", "session": "sess-f", "argv": ["cat"], "cwd": "/nonexistent-kith-dir"});
        match reach(
            &crate::tests::paths(&rig.m),
            "terminal.open",
            &params,
            false,
        ) {
            Reached::Refused(e) => assert!(e.to_string().contains("could not start"), "{e}"),
            other => panic!("{other:?}"),
        }
        assert!(!rig.claim_file("sess-f").exists());
    }

    #[test]
    fn a_session_with_a_live_writer_is_busy_and_nothing_is_spawned() {
        let rig = Rig::new(RUNNING);
        let mut other = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        assert!(
            claim::take(&rig.m, "sess-w", other.id(), "terminal")
                .unwrap()
                .is_ok()
        );
        let a = answered(open(&rig.m, "sess-w", &["sh", "-c", "cat"], &plain()));
        assert_eq!(a["status"], "busy", "{a}");
        assert_eq!(a["held_by"]["pid"], other.id());
        assert!(
            !Layout::new(&rig.m).dir.join("locks/sess-w.args").exists(),
            "no stub ran"
        );
        other.kill().unwrap();
        other.wait().unwrap();
    }

    #[test]
    fn a_session_the_app_is_taking_over_is_busy_and_gives_the_claim_back() {
        let rig = Rig::new(RUNNING);
        let mut other = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        claim::take_takeover(&rig.m, "sess-t", other.id());
        let a = answered(open(&rig.m, "sess-t", &["sh", "-c", "cat"], &plain()));
        assert_eq!(a["status"], "busy", "{a}");
        assert!(a["note"].as_str().unwrap().contains("taken over"), "{a}");
        assert!(
            !rig.claim_file("sess-t").exists(),
            "the claim it took is given back"
        );
        other.kill().unwrap();
        other.wait().unwrap();
    }

    #[test]
    fn two_opens_of_one_session_at_once_let_one_win() {
        let rig = Rig::new(RUNNING);
        let go = |m: PathBuf| {
            std::thread::spawn(move || {
                answered(open(&m, "sess-2", &["sh", "-c", "cat"], &plain()))["status"].clone()
            })
        };
        let (a, b) = (go(rig.m.clone()), go(rig.m.clone()));
        let mut got = [a.join().unwrap(), b.join().unwrap()];
        got.sort_by_key(ToString::to_string);
        assert_eq!(got, [json!("busy"), json!("open")]);
    }

    #[test]
    fn without_a_holder_or_a_stub_the_answer_is_direct_and_nothing_is_taken() {
        let t = sandbox();
        let m = t.path().join("m");
        let stub = fake_stub(t.path(), RUNNING);
        // No holder link at all.
        let d = serving(
            &m,
            Config {
                holder_on: false,
                key_load: None,
                host: false,
                ..config(Some(stub.clone()))
            },
        );
        let a = answered(open(&m, "sess-d", &["cat"], &json!({})));
        assert_eq!(a["status"], "direct", "{a}");
        assert!(a["note"].as_str().unwrap().contains("--direct"));
        assert!(!m.join("live/terminal/sess-d.claim").exists());
        d.stop();
        // A holder program that is not there: the link never comes up.
        let mut cfg = config(Some(stub));
        cfg.holder.exe = Some(t.path().join("no-holder"));
        let d = serving(&m, cfg);
        let a = answered(open(&m, "sess-d", &["cat"], &json!({})));
        assert_eq!(a["status"], "direct", "{a}");
        d.stop();
        // No stub program.
        let d = serving(&m, config(Some(t.path().join("no-stub"))));
        let a = answered(open(&m, "sess-d", &["cat"], &json!({})));
        assert_eq!(a["status"], "direct", "{a}");
        assert!(a["note"].as_str().unwrap().contains("kith-stub"));
        d.stop();
    }

    #[test]
    fn params_that_do_not_fit_run_nothing() {
        let rig = Rig::new(RUNNING);
        let p = crate::tests::paths(&rig.m);
        for params in [
            json!({"agent": "kai", "session": "../x", "argv": ["cat"], "cwd": "/tmp"}),
            json!({"agent": "kai", "session": "ok", "argv": [], "cwd": "/tmp"}),
            json!({"agent": "kai", "session": "ok", "argv": ["cat"]}),
        ] {
            assert!(
                matches!(
                    reach(&p, "terminal.open", &params, false),
                    Reached::Refused(_)
                ),
                "{params}"
            );
        }
        assert!(!rig.claim_file("ok").exists());
    }
}
