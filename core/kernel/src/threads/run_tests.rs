#![allow(clippy::unwrap_used, unsafe_code)]
//! `run` against real children: fake `claude` scripts and a fake `kith-stub`, the real fork, signals and descriptors. The test
//! process is the wrapper, so its own pid is the one whose claims are handed back.

use std::cell::RefCell;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use super::resolve::NoHost;
use super::run::run;
use super::tests::{Tmp, ctx};

/// What a fake engine does first: list itself in the registry under the pid it runs as, as the `SessionStart` hook would.
const ENTRY: &str = r#"
d="$KITH_STATE/live/terminal"; sid=11111111-1111-4111-8111-111111111111
now=$(date -u +%Y-%m-%dT%H:%M:%SZ)
entry() { mkdir -p "$d"; printf '{"v":1,"sessionId":"%s","agent":"coder","cwd":"/","transcriptPath":"%s","pid":%s,"startedAt":"%s","recordedAt":1}' "$sid" "$TRANSCRIPT" "$$" "$now" > "$d/$sid.json"; }
"#;

const SID: &str = "11111111-1111-4111-8111-111111111111";

fn script(t: &Tmp, name: &str, body: &str) -> String {
    let p = t.0.join(name);
    fs::write(&p, format!("#!/bin/sh\n{body}")).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    p.to_string_lossy().into_owned()
}

/// The transcript the fake engines name (it exists, so the resume note is due).
fn transcript(t: &Tmp) -> String {
    let p = t.0.join("home").join("t.jsonl");
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, "{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}\n").unwrap();
    p.to_string_lossy().into_owned()
}

fn go(t: &Tmp, env: &dyn Fn(&str) -> Option<String>, cmd: &[&str]) -> (i32, Vec<String>) {
    let c = ctx(t, env);
    let notes = RefCell::new(Vec::new());
    let cmd: Vec<String> = cmd.iter().map(|s| (*s).to_owned()).collect();
    let code = run(
        &c,
        &NoHost,
        None,
        &t.0.join("kernel"),
        "coder",
        &cmd,
        &|l| {
            notes.borrow_mut().push(l.to_owned());
        },
    );
    (code, notes.into_inner())
}

fn slow(k: &str) -> Option<String> {
    match k {
        "KITH_IDLE_POLL" => Some("1000".to_owned()),
        _ => None,
    }
}

fn put_claim(t: &Tmp, name: &str, pid: i64) {
    t.put(
        name,
        &format!(r#"{{"pid": {pid}, "procStart": null, "at": "2026-01-01T00:00:00Z", "kind": "terminal"}}"#),
    );
}

#[test]
fn the_exit_code_passes_and_a_signal_death_is_128_plus_n() {
    let t = Tmp::new();
    let three = script(&t, "three", "exit 3\n");
    assert_eq!(go(&t, &slow, &[&three]).0, 3);
    let term = script(&t, "term", "kill -TERM $$; sleep 5\n");
    assert_eq!(go(&t, &slow, &[&term]).0, 143);
}

#[test]
fn a_clean_exit_is_a_quit_and_a_crash_is_not_and_the_resume_note_follows_both() {
    let t = Tmp::new();
    let tr = transcript(&t);
    let state = t.state();
    let env = move |k: &str| match k {
        "KITH_IDLE_POLL" => Some("1000".to_owned()),
        _ => None,
    };
    let ok = script(
        &t,
        "ok",
        &format!(
            "TRANSCRIPT={tr}; KITH_STATE={}\n{ENTRY}entry\n",
            state.display()
        ),
    );
    let (code, notes) = go(&t, &env, &[&ok]);
    assert_eq!(code, 0);
    let intent =
        fs::read_to_string(state.join("sessions/intent").join(format!("{SID}.json"))).unwrap();
    assert!(intent.contains(r#""why": "quit""#), "{intent}");
    assert_eq!(
        notes,
        [format!(
            "Resume it as coder with: kith coder --resume {SID}"
        )]
    );

    fs::remove_file(state.join("sessions/intent").join(format!("{SID}.json"))).unwrap();
    let bad = script(
        &t,
        "bad",
        &format!(
            "TRANSCRIPT={tr}; KITH_STATE={}\n{ENTRY}entry; exit 1\n",
            state.display()
        ),
    );
    let (code, notes) = go(&t, &env, &[&bad]);
    assert_eq!(code, 1);
    assert!(
        !state
            .join("sessions/intent")
            .join(format!("{SID}.json"))
            .exists(),
        "a crash leaves no intent"
    );
    assert_eq!(notes.len(), 1);
}

#[test]
fn a_run_that_could_not_start_still_gives_its_claims_back_and_says_why() {
    let t = Tmp::new();
    put_claim(&t, "s-1.claim", i64::from(std::process::id()));
    put_claim(&t, "s-2.claim", 99_999);
    let (code, notes) = go(&t, &slow, &["/nonexistent/claude", "a"]);
    assert_eq!(code, 127);
    assert_eq!(
        notes,
        ["kith: cannot run /nonexistent/claude: [Errno 2] No such file or directory"]
    );
    assert!(
        !t.reg().join("s-1.claim").exists(),
        "the wrapper's own claim is handed back"
    );
    assert!(
        t.reg().join("s-2.claim").exists(),
        "another process's claim is kept"
    );
}

/// A descriptor that survives exec, at `at` or above: what a launcher's shell leaves open.
fn leak(path: &Path, at: i32) -> i32 {
    use std::os::unix::io::IntoRawFd as _;
    let fd = fs::File::open(path).unwrap().into_raw_fd();
    // SAFETY: F_DUPFD with no close-on-exec on a descriptor this test owns.
    let high = unsafe { libc::fcntl(fd, libc::F_DUPFD, at) };
    // SAFETY: closing the descriptor `open` returned.
    unsafe { libc::close(fd) };
    assert!(high >= at);
    high
}

#[test]
fn claude_gets_descriptors_0_to_2_and_nothing_else_and_under_the_stub_the_status_pipe_as_9() {
    let t = Tmp::new();
    let file = t.0.join("kernel-file");
    fs::write(&file, "x").unwrap();
    // Leaked at 7 and 9 and well above: none may reach the engine, and the one at 9 must not be mistaken for the status pipe.
    let leaked: Vec<i32> = [7, 9, 40].iter().map(|at| leak(&file, *at)).collect();
    let out = t.0.join("seen");
    let probe = |extra: &str| {
        let tests: Vec<String> = leaked
            .iter()
            .map(|fd| {
                format!(
                    "[ -e /dev/fd/{fd} ] && echo \"leaked {fd}\" >> {o}; ",
                    o = out.display()
                )
            })
            .collect();
        let tests = tests.concat();
        format!("{tests}{extra}echo done >> {o}\n", o = out.display())
    };
    let see = script(&t, "see", &probe(""));
    assert_eq!(go(&t, &slow, &[&see]).0, 0);
    // Under the stub: fd 9 is the status pipe (its write end), so the leaked fd 9 is the pipe now, and nothing else is there.
    let stub = script(
        &t,
        "kith-stub",
        &format!(
            "fd=\"\"; while [ $# -gt 0 ]; do case \"$1\" in --status-fd) fd=$2; shift 2;; --) shift; break;; *) shift;; esac; done\n\
             [ \"$fd\" = 9 ] && echo \"status 9\" >> {o}\n\
             for n in 7 8 10 11 12 40; do [ -e /dev/fd/$n ] && echo \"leaked $n\" >> {o}; done\n\
             echo locked >&9\nexit 0\n",
            o = out.display()
        ),
    );
    let ok = script(&t, "ok", "exit 0\n");
    assert_eq!(go(&t, &slow, &[&stub, "--key", "s", "--", &ok]).0, 0);
    for fd in leaked {
        // SAFETY: the descriptors `leak` made.
        unsafe { libc::close(fd) };
    }
    let seen = fs::read_to_string(out).unwrap();
    assert_eq!(
        seen.lines().collect::<Vec<_>>(),
        ["done", "status 9"],
        "{seen}"
    );
}

#[test]
fn a_stub_that_cannot_start_is_told_even_with_the_low_descriptors_taken() {
    let t = Tmp::new();
    // Hold descriptors until 8 and 9 are the next free ones: before the pipes were moved above 9, the pipe that says an exec failed
    // landed on it, the child's dup2 closed it, and the failure was read as the stub's own word.
    let mut held = Vec::new();
    loop {
        let f = fs::File::open("/dev/null").unwrap();
        let fd = std::os::unix::io::AsRawFd::as_raw_fd(&f);
        held.push(f);
        if fd >= 7 || held.len() > 64 {
            break;
        }
    }
    let gone =
        t.0.join("no-such-dir")
            .join("kith-stub")
            .to_string_lossy()
            .into_owned();
    let (code, notes) = go(&t, &slow, &[&gone, "--key", "s", "--", "claude"]);
    drop(held);
    assert_eq!(code, 127);
    assert!(
        notes.iter().any(|n| n.starts_with("kith: cannot run ")
            && n.ends_with("[Errno 2] No such file or directory")),
        "{notes:?}"
    );
}

#[test]
fn claude_starts_with_default_signals_and_an_empty_mask() {
    let t = Tmp::new();
    // The Rust runtime ignores SIGPIPE, and this thread blocks SIGUSR2: neither may reach the engine.
    // SAFETY: sigaction/pthread_sigmask on this test's own thread, undone below.
    let mut old: libc::sigset_t = unsafe { std::mem::zeroed() };
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&raw mut set);
        libc::sigaddset(&raw mut set, libc::SIGUSR2);
        libc::pthread_sigmask(libc::SIG_BLOCK, &raw const set, &raw mut old);
    }
    let pipe = script(&t, "pipe", "kill -PIPE $$; echo alive\n");
    let usr2 = script(&t, "usr2", "kill -USR2 $$; echo alive\n");
    let a = go(&t, &slow, &[&pipe]).0;
    let b = go(&t, &slow, &[&usr2]).0;
    // SAFETY: puts back what was there.
    unsafe {
        libc::pthread_sigmask(libc::SIG_SETMASK, &raw const old, std::ptr::null_mut());
    }
    assert_eq!(
        a,
        128 + libc::SIGPIPE,
        "SIGPIPE is at its default in the engine"
    );
    assert_eq!(
        b,
        128 + libc::SIGUSR2,
        "SIGUSR2 is not blocked in the engine"
    );
}

#[test]
fn term_reaches_the_child_and_the_wrapper_outlives_it() {
    let t = Tmp::new();
    let up = t.0.join("up");
    let sleeper = script(
        &t,
        "sleeper",
        &format!("touch {}; sleep 30\n", up.display()),
    );
    let hit = std::thread::spawn(move || {
        // Only once the child runs: it is then this run, not another test's, that holds the handlers.
        while !up.exists() {
            std::thread::sleep(Duration::from_millis(20));
        }
        // SAFETY: a TERM to this very process, which `run` has a handler for while its child runs.
        unsafe { libc::kill(libc::getpid(), libc::SIGTERM) };
    });
    let (code, _) = go(&t, &slow, &[&sleeper]);
    hit.join().unwrap();
    assert_eq!(code, 128 + libc::SIGTERM);
}

#[test]
fn a_stale_take_over_mark_is_cleared_without_a_note_and_a_fresh_one_is_the_note() {
    let t = Tmp::new();
    let tr = transcript(&t);
    let state = t.state();
    let marker = t.reg().join(format!("{SID}.taken"));
    let body = |wait: &str, mark: &str| {
        format!(
            "TRANSCRIPT={tr}; KITH_STATE={}\n{ENTRY}entry; {wait}{mark}\n",
            state.display()
        )
    };
    // an old mark, from a run that crashed
    t.put(&format!("{SID}.taken"), "");
    fs::File::options()
        .write(true)
        .open(&marker)
        .unwrap()
        .set_modified(UNIX_EPOCH)
        .unwrap();
    let old_mark = script(&t, "stale", &body("", ""));
    let (_, notes) = go(&t, &slow, &[&old_mark]);
    assert!(!marker.exists());
    assert_eq!(
        notes,
        [format!(
            "Resume it as coder with: kith coder --resume {SID}"
        )]
    );

    let fresh = script(
        &t,
        "fresh",
        &body("sleep 1; ", &format!("touch {}", marker.display())),
    );
    let (code, notes) = go(&t, &slow, &[&fresh]);
    assert_eq!(code, 0);
    assert!(!marker.exists());
    assert_eq!(
        notes,
        [format!(
            "continued in Kith — get it back with: kith coder --resume {SID}"
        )]
    );
}

/// `kith-stub` as `run` sees it: `--status-fd N` before the `--`, then the engine as its own child.
const STUB: &str = r#"fd=""; while [ $# -gt 0 ]; do case "$1" in --status-fd) fd=$2; shift 2;; --) shift; break;; *) shift;; esac; done
"$@"; rc=$?
[ -n "$fd" ] && [ -n "$STUB_WORD" ] && [ "$STUB_WORD" != none ] && echo "$STUB_WORD" >&"$fd"
exit ${STUB_RC:-$rc}
"#;

#[test]
fn the_stubs_word_decides_busy_never_started_and_a_lock_error() {
    let t = Tmp::new();
    let stub = t.0.join("kith-stub").to_string_lossy().into_owned();
    let ok = script(&t, "ok", "exit 0\n");
    let gone = t.0.join("no-such-engine").to_string_lossy().into_owned();
    // The stub's word and exit code are written into the script itself: nothing process-wide changes.
    let word = |w: &str, rc: &str| {
        script(
            &t,
            "kith-stub",
            &format!("STUB_WORD={w}; STUB_RC={rc}\n{STUB}"),
        );
    };
    word("busy", "");
    assert_eq!(go(&t, &slow, &[&stub, "--key", "s", "--", &ok]).0, 75);
    word("locked", "");
    let (code, notes) = go(&t, &slow, &[&stub, "--key", "s", "--", &gone]);
    assert_eq!(code, 127);
    assert!(
        notes
            .iter()
            .any(|n| n == "kith: the engine did not start (127); it never ran"),
        "{notes:?}"
    );
    word("none", "70");
    let (code, notes) = go(&t, &slow, &[&stub, "--key", "s", "--", &ok]);
    assert_eq!(code, 70);
    assert_eq!(
        notes,
        ["kith: the conversation lock could not be taken (not busy); this run did not start"]
    );
}

fn quick_sleep(k: &str) -> Option<String> {
    match k {
        "KITH_IDLE_POLL" => Some("0.2".to_owned()),
        // 1.2 s idle
        "KITH_SLEEP_MINUTES" => Some("0.02".to_owned()),
        _ => None,
    }
}

#[test]
fn an_idle_session_is_stopped_by_its_wrapper_with_an_intent_and_a_busy_one_is_left_alone() {
    let t = Tmp::new();
    let tr = transcript(&t);
    let state = t.state();
    let idle = script(
        &t,
        "idle",
        &format!(
            "TRANSCRIPT={tr}; KITH_STATE={}\n{ENTRY}entry; sleep 20\n",
            state.display()
        ),
    );
    let started = std::time::Instant::now();
    let (code, _) = go(&t, &quick_sleep, &[&idle]);
    assert_eq!(code, 128 + libc::SIGTERM);
    assert!(started.elapsed() < Duration::from_secs(15));
    let intent =
        fs::read_to_string(state.join("sessions/intent").join(format!("{SID}.json"))).unwrap();
    assert!(intent.contains(r#""why": "idle""#), "{intent}");

    // a tool call with no result yet: outstanding work (rule 7), so it is never stopped
    fs::remove_file(state.join("sessions/intent").join(format!("{SID}.json"))).unwrap();
    fs::write(
        &tr,
        "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"Bash\"}]}}\n",
    )
    .unwrap();
    let busy = script(
        &t,
        "busy",
        &format!(
            "TRANSCRIPT={tr}; KITH_STATE={}\n{ENTRY}entry; sleep 3\n",
            state.display()
        ),
    );
    let (code, _) = go(&t, &quick_sleep, &[&busy]);
    assert_eq!(code, 0, "left to finish");
    let intent =
        fs::read_to_string(state.join("sessions/intent").join(format!("{SID}.json"))).unwrap();
    assert!(intent.contains(r#""why": "quit""#), "{intent}");
}
