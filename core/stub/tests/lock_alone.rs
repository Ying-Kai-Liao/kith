//! `kith-stub --lock-alone` (design 2026-11-16-offturn-lock-alone, rule L-clear) against real processes: a resident stub marks
//! the conversations its engine has loaded (`loaded` / `forgot`), and a one-shot run on the lock alone clears them, holding the
//! lock and before its engine starts.
#![cfg(unix)]

use kith_procstart::{Probe, probe};
use kith_stub::{Taken, take};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const STUB: &str = env!("CARGO_BIN_EXE_kith-stub");

fn scratch(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "kith-stub-alone-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn runs(pid: u32) -> bool {
    !matches!(probe(pid), Probe::Gone)
}

/// The holder's end of the channel.
struct Chan {
    w: UnixStream,
    r: BufReader<UnixStream>,
}

impl Chan {
    fn say(&mut self, line: &str) -> String {
        self.w.write_all(format!("{line}\n").as_bytes()).unwrap();
        let mut s = String::new();
        self.r.read_line(&mut s).unwrap();
        s.trim_end().to_string()
    }
}

/// A warm engine under `kith-stub --resident`, leading its own process group as the host starts it (`Acp::start`). The engine
/// prints its pid, then waits on stdin.
fn resident(dir: &Path, group_leader: bool) -> (Child, Chan, u32) {
    let (mine, theirs) = UnixStream::pair().unwrap();
    let fd = theirs.as_raw_fd();
    let mut cmd = Command::new(STUB);
    cmd.args(["--resident", "--lock-dir"])
        .arg(dir)
        .args(["--ctl-fd", "9", "--", "sh", "-c", "echo $$; read x"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    if group_leader {
        cmd.process_group(0);
    }
    unsafe {
        cmd.pre_exec(move || {
            if fd == 9 {
                let f = libc::fcntl(9, libc::F_GETFD);
                libc::fcntl(9, libc::F_SETFD, f & !libc::FD_CLOEXEC);
            } else {
                libc::dup2(fd, 9);
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().unwrap();
    drop(theirs);
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    let out = child.stdout.as_mut().unwrap();
    while std::io::Read::read(out, &mut b).unwrap() == 1 && b[0] != b'\n' {
        line.push(b[0]);
    }
    let engine = String::from_utf8(line).unwrap().trim().parse().unwrap();
    let chan = Chan {
        r: BufReader::new(mine.try_clone().unwrap()),
        w: mine,
    };
    (child, chan, engine)
}

/// A one-shot run of `key` (on the lock alone when `alone`) whose engine says whether `watch` still runs as it starts.
fn one_shot(dir: &Path, key: &str, alone: bool, watch: u32) -> Output {
    let mut cmd = Command::new(STUB);
    cmd.arg("--lock-dir").arg(dir).args(["--key", key]);
    if alone {
        cmd.arg("--lock-alone");
    }
    // A zombie is not running: it waits only for its parent.
    let script = format!(
        "if kill -0 {watch} 2>/dev/null && ! ps -o stat= -p {watch} | grep -q Z; then echo alive; else echo gone; fi"
    );
    cmd.args(["--", "sh", "-c", &script]).output().unwrap()
}

fn marker(dir: &Path, key: &str, stub: u32) -> PathBuf {
    let start = kith_procstart::start_stamp(stub).expect("the stub's start");
    dir.join(format!("{key}.loaded"))
        .join(format!("{stub}-{start}"))
}

#[test]
fn loaded_and_forgot_mark_and_the_stubs_exit_unmarks() {
    let dir = scratch("marks");
    let (mut stub, mut c, _) = resident(&dir, true);
    let pid = stub.id();
    assert_eq!(c.say("loaded conv1"), "marked conv1");
    assert_eq!(c.say("loaded conv1"), "marked conv1", "idempotent");
    assert_eq!(c.say("loaded conv2"), "marked conv2");
    let (m1, m2) = (marker(&dir, "conv1", pid), marker(&dir, "conv2", pid));
    assert!(m1.is_file() && m2.is_file());
    assert_eq!(c.say("forgot conv1"), "unmarked conv1");
    assert!(!m1.exists() && m2.is_file());
    assert_eq!(c.say("forgot conv1"), "unmarked conv1", "a repeat is fine");
    assert!(c.say("loaded Bad/Key").starts_with("error - bad lock key"));
    assert!(c.say("loaded").starts_with("error - "), "a key is needed");
    // The engine exits: the stub removes what is left before it exits itself.
    drop(stub.stdin.take());
    stub.wait().unwrap();
    assert!(!m2.exists(), "the stub's exit removes its markers");
}

#[test]
fn a_lock_alone_run_kills_an_idle_warm_process_loaded_on_its_key_and_starts_after_it_is_gone() {
    let dir = scratch("kill");
    let (mut stub, mut c, engine) = resident(&dir, true);
    assert_eq!(c.say("loaded conv1"), "marked conv1");
    let m = marker(&dir, "conv1", stub.id());
    let out = one_shot(&dir, "conv1", true, engine);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "gone",
        "the warm engine was gone before the run's engine started"
    );
    // The stub is our child here: killed, a zombie until this wait. The host's stub is the same (its parent reaps it late).
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&stub.wait().unwrap()),
        Some(libc::SIGKILL)
    );
    assert!(!runs(engine));
    assert!(!m.exists(), "the killed stub's marker is removed");
    assert!(
        matches!(take(&dir, "conv1").unwrap(), Taken::Held(_)),
        "the run's lock went with it"
    );
}

#[test]
fn a_warm_process_whose_stub_holds_another_key_refuses_the_run_and_nothing_is_killed() {
    let dir = scratch("turn");
    let (mut stub, mut c, engine) = resident(&dir, true);
    assert_eq!(c.say("loaded conv1"), "marked conv1");
    assert_eq!(c.say("loaded conv2"), "marked conv2");
    assert_eq!(c.say("take conv2 7"), "locked 7");
    let out = one_shot(&dir, "conv1", true, engine);
    assert_eq!(out.status.code(), Some(75));
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("a host turn is running in that agent's process")
    );
    assert!(out.stdout.is_empty(), "the run's engine never started");
    assert!(runs(stub.id()) && runs(engine), "nothing was killed");
    assert!(
        matches!(take(&dir, "conv1").unwrap(), Taken::Held(_)),
        "the refused run let the lock go"
    );
    // The turn ends: the next run clears it.
    assert_eq!(c.say("release conv2 7"), "released 7");
    let out = one_shot(&dir, "conv1", true, engine);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = stub.wait();
}

#[test]
fn a_stale_marker_is_removed_and_nobody_is_signalled() {
    let dir = scratch("stale");
    let folder = dir.join("conv1.loaded");
    std::fs::create_dir_all(&folder).unwrap();
    // A pid that is gone, and a live pid (this test) with another start: both stale.
    let mut gone = Command::new("true").spawn().unwrap();
    gone.wait().unwrap();
    // Another start in this system's own stamp form (`e<secs>` on macOS, `b<boot>:<ticks>` on Linux): a stamp of another form
    // cannot be told apart and refuses the run instead.
    let own = kith_procstart::start_stamp(std::process::id()).expect("this test's start");
    let other = match own.rsplit_once(':') {
        Some((boot, _)) => format!("{boot}:1"),
        None => "e1".to_owned(),
    };
    let dead = folder.join(format!("{}-{other}", gone.id()));
    let reused = folder.join(format!("{}-{other}", std::process::id()));
    for f in [&dead, &reused] {
        std::fs::write(f, b"").unwrap();
    }
    let out = one_shot(&dir, "conv1", true, 0);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !dead.exists() && !reused.exists(),
        "stale markers are removed"
    );
}

#[test]
fn a_run_with_a_core_answer_never_clears() {
    let dir = scratch("answered");
    let (mut stub, mut c, engine) = resident(&dir, true);
    assert_eq!(c.say("loaded conv1"), "marked conv1");
    let out = one_shot(&dir, "conv1", false, engine);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "alive",
        "without --lock-alone the warm process is left alone"
    );
    assert!(runs(stub.id()) && marker(&dir, "conv1", stub.id()).is_file());
    drop(stub.stdin.take());
    let _ = stub.wait();
}

#[test]
fn a_warm_stub_that_leads_no_group_is_not_killed_and_the_run_is_busy() {
    let dir = scratch("nogroup");
    let (mut stub, mut c, engine) = resident(&dir, false);
    assert_eq!(c.say("loaded conv1"), "marked conv1");
    let out = one_shot(&dir, "conv1", true, engine);
    // Its engine would outlive a kill of the stub alone: refused, never half-killed.
    assert_eq!(out.status.code(), Some(75));
    assert!(runs(stub.id()) && runs(engine));
    drop(stub.stdin.take());
    let _ = stub.wait();
    wait_until("the engine to end", || !runs(engine));
}

#[test]
fn lock_alone_is_one_shot_only_and_a_busy_lock_is_still_busy() {
    let dir = scratch("flags");
    let out = Command::new(STUB)
        .args(["--resident", "--lock-alone", "--lock-dir"])
        .arg(&dir)
        .args(["--ctl-fd", "9", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(64));
    let held = take(&dir, "conv1").unwrap();
    assert!(matches!(held, Taken::Held(_)));
    let out = one_shot(&dir, "conv1", true, 0);
    assert_eq!(out.status.code(), Some(75), "busy before any clear");
}
