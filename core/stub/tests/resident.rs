//! `kith-stub --resident` against real processes: the per-turn control channel and crash survival.
//! Unix runs the channel over a socketpair; the Windows named-pipe path is in tests/resident_windows.rs.
#![cfg(unix)]

use kith_stub::{Taken, take};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const STUB: &str = env!("CARGO_BIN_EXE_kith-stub");

fn scratch(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "kith-stub-res-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn is_busy(dir: &Path, key: &str) -> bool {
    matches!(take(dir, key).unwrap(), Taken::Busy)
}

fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// What a starter's pre_exec does to hand the child `fd` as fd 9: dup2 is a no-op when fd is already 9 and would
/// leave FD_CLOEXEC set, so clear the flag instead.
unsafe fn pass_as_9(fd: i32) {
    unsafe {
        if fd == 9 {
            let f = libc::fcntl(9, libc::F_GETFD);
            libc::fcntl(9, libc::F_SETFD, f & !libc::FD_CLOEXEC);
        } else {
            libc::dup2(fd, 9);
        }
    }
}

/// The holder's end of the channel.
struct Chan {
    w: UnixStream,
    r: BufReader<UnixStream>,
}

impl Chan {
    fn new(s: UnixStream) -> Chan {
        Chan {
            r: BufReader::new(s.try_clone().unwrap()),
            w: s,
        }
    }
    fn say(&mut self, line: &str) -> String {
        self.send_raw(format!("{line}\n").as_bytes());
        self.reply()
    }
    fn reply(&mut self) -> String {
        let mut s = String::new();
        self.r.read_line(&mut s).unwrap();
        s.trim_end().to_string()
    }
    fn send_raw(&mut self, b: &[u8]) {
        self.w.write_all(b).unwrap();
    }
}

/// Starts a resident stub whose engine is `sh -c script` with stdin piped (closing it lets scripts that `read` end).
fn resident(dir: &Path, script: &str) -> (Child, Chan) {
    let (mine, theirs) = UnixStream::pair().unwrap();
    let their_fd = theirs.as_raw_fd();
    let mut cmd = Command::new(STUB);
    cmd.args(["--resident", "--lock-dir"])
        .arg(dir)
        .args(["--ctl-fd", "9", "--", "sh", "-c", script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    unsafe {
        cmd.pre_exec(move || {
            pass_as_9(their_fd);
            Ok(())
        });
    }
    let child = cmd.spawn().unwrap();
    drop(theirs);
    (child, Chan::new(mine))
}

/// One line of the child's stdout, read a byte at a time: a `BufReader` made per call could take the next line into
/// its buffer and drop it with itself (two pids printed together lost the second).
fn first_line(child: &mut Child) -> String {
    let out = child.stdout.as_mut().unwrap();
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    while std::io::Read::read(out, &mut b).unwrap() == 1 && b[0] != b'\n' {
        line.push(b[0]);
    }
    String::from_utf8(line).unwrap().trim().to_string()
}

#[test]
fn take_release_and_the_claim_rules() {
    let dir = scratch("rules");
    let (mut stub, mut c) = resident(&dir, "read x; true");
    assert!(!is_busy(&dir, "conv1"), "no lock before the first take");

    assert_eq!(c.say("take conv1 c1"), "locked c1");
    assert!(is_busy(&dir, "conv1"));
    // A retried line is idempotent.
    assert_eq!(c.say("take conv1 c1"), "locked c1");
    // Another claim of the same conversation, and another conversation, are busy: one lock at a time.
    assert_eq!(c.say("take conv1 c2"), "busy c2");
    assert_eq!(c.say("take conv2 c3"), "busy c3");
    assert!(!is_busy(&dir, "conv2"));
    // A wrong claim (or key) releases nothing.
    assert_eq!(c.say("release conv1 c2"), "not-held c2");
    assert_eq!(c.say("release conv2 c1"), "not-held c1");
    assert!(is_busy(&dir, "conv1"));
    assert_eq!(c.say("release conv1 c1"), "released c1");
    assert!(!is_busy(&dir, "conv1"));
    assert_eq!(c.say("release conv1 c1"), "not-held c1");
    // Free again: the next turn's claim can take it, and the file was never removed.
    assert_eq!(c.say("take conv1 c2"), "locked c2");
    assert!(dir.join("w-conv1.lock").exists());

    drop(stub.stdin.take());
    assert_eq!(stub.wait().unwrap().code(), Some(0));
    assert!(!is_busy(&dir, "conv1"), "the lock goes with the stub");
}

#[test]
fn garbled_lines_get_errors_and_serving_goes_on() {
    let dir = scratch("garbled");
    let (mut stub, mut c) = resident(&dir, "read x; true");
    for bad in [
        "",
        "hello",
        "take",
        "take a",
        "take a b c",
        "grab conv1 c1",
        "take conv1 bad/claim",
        "take A/B c1",
        "take  conv1 c1",
    ] {
        let reply = c.say(bad);
        assert!(reply.starts_with("error "), "{bad:?} -> {reply:?}");
    }
    // Over-long line: one error, and its tail is not read as a command.
    let long = format!("{} take conv1 c1", "x".repeat(5000));
    assert_eq!(c.say(&long), "error - line-too-long");
    assert!(!is_busy(&dir, "conv1"));
    // A CRLF line works; non-UTF-8 is an error.
    assert_eq!(c.say("take conv1 c1\r"), "locked c1");
    c.send_raw(b"take \xff c1\n");
    let s = c.reply();
    assert!(s.starts_with("error "), "{s:?}");
    assert_eq!(c.say("release conv1 c1"), "released c1");
    drop(stub.stdin.take());
    assert_eq!(stub.wait().unwrap().code(), Some(0));
}

#[test]
fn a_line_cut_off_by_a_closed_channel_is_not_acted_on() {
    let dir = scratch("cut");
    let (mut stub, mut c) = resident(&dir, "read x; true");
    assert_eq!(c.say("release conv1 c1"), "not-held c1");
    c.send_raw(b"take conv1 c1");
    drop(c);
    std::thread::sleep(Duration::from_millis(300));
    assert!(!is_busy(&dir, "conv1"));
    drop(stub.stdin.take());
    stub.wait().unwrap();
}

#[test]
fn a_dead_holder_leaves_the_lock_until_the_engine_exits_and_the_exit_code_passes() {
    let dir = scratch("crash");
    let (mut stub, mut c) = resident(&dir, "echo $$; read x; exit 7");
    let engine: i32 = first_line(&mut stub).parse().unwrap();
    assert_eq!(c.say("take conv1 c1"), "locked c1");
    // The holder dies mid-turn: the OS closes its end of the channel (what kill -9 does).
    drop(c);
    std::thread::sleep(Duration::from_millis(500));
    assert!(alive(engine));
    assert!(
        is_busy(&dir, "conv1"),
        "a closed channel must not release the lock"
    );
    assert!(
        stub.try_wait().unwrap().is_none(),
        "the stub outlives the channel"
    );

    // Engine exits -> stub exits with its code, and only then is the lock free.
    drop(stub.stdin.take());
    assert_eq!(stub.wait().unwrap().code(), Some(7));
    assert!(!is_busy(&dir, "conv1"));
}

#[test]
fn a_killed_holder_process_is_the_same_as_a_closed_channel() {
    let dir = scratch("kill9");
    // The holder is a real process that owns the channel's stub end: a shell that starts the stub and is
    // kill -9'd while the lock is held.
    let (mine, theirs) = UnixStream::pair().unwrap();
    let their_fd = theirs.as_raw_fd();
    let script = format!(
        "{STUB:?} --resident --lock-dir {dir:?} --ctl-fd 9 -- sh -c 'echo $$; sleep 30' & echo $!; wait"
    );
    let mut holder = Command::new("sh");
    holder.args(["-c", &script]).stdout(Stdio::piped());
    unsafe {
        holder.pre_exec(move || {
            pass_as_9(their_fd);
            Ok(())
        });
    }
    let mut holder = holder.spawn().unwrap();
    drop(theirs);
    let stub_pid: i32 = first_line(&mut holder).parse().unwrap();
    let engine: i32 = first_line(&mut holder).parse().unwrap();
    let mut c = Chan::new(mine);
    assert_eq!(c.say("take conv1 c1"), "locked c1");
    unsafe { libc::kill(holder.id() as i32, libc::SIGKILL) };
    holder.wait().unwrap();
    drop(c);
    std::thread::sleep(Duration::from_millis(500));
    assert!(alive(stub_pid) && alive(engine));
    assert!(is_busy(&dir, "conv1"));
    unsafe { libc::kill(engine, libc::SIGKILL) };
    wait_until("lock free after the engine died", || {
        !is_busy(&dir, "conv1")
    });
    wait_until("stub gone", || !alive(stub_pid));
}

#[test]
fn an_escaped_engine_child_holds_nothing_after_engine_and_stub_exit() {
    let dir = scratch("escape");
    // The sleeper starts after the take, as a tool forked mid-turn would, so it could inherit the lock fd.
    let (mut stub, mut c) = resident(
        &dir,
        "read go; nohup sleep 30 >/dev/null 2>&1 & echo $!; read x; true",
    );
    assert_eq!(c.say("take conv1 c1"), "locked c1");
    stub.stdin.as_mut().unwrap().write_all(b"go\n").unwrap();
    let sleeper: i32 = first_line(&mut stub).parse().unwrap();
    assert!(is_busy(&dir, "conv1"));
    drop(stub.stdin.take());
    assert_eq!(stub.wait().unwrap().code(), Some(0));
    assert!(alive(sleeper));
    assert!(
        !is_busy(&dir, "conv1"),
        "a surviving engine child must not hold the lock"
    );
    unsafe { libc::kill(sleeper, libc::SIGKILL) };
}

#[test]
fn the_engine_never_has_the_channel() {
    let dir = scratch("fds");
    // The engine reports whether fd 9 is open in it.
    let (mut stub, mut c) = resident(
        &dir,
        "if (: <&9) 2>/dev/null; then echo open; else echo closed; fi; read x",
    );
    assert_eq!(first_line(&mut stub), "closed");
    assert_eq!(c.say("take conv1 c1"), "locked c1");
    drop(stub.stdin.take());
    stub.wait().unwrap();
}

#[test]
fn take_against_a_direct_one_shot_stub_is_busy_then_free() {
    let dir = scratch("oneshot");
    let mut one = Command::new(STUB)
        .arg("--lock-dir")
        .arg(&dir)
        .args(["--key", "conv1", "--", "sh", "-c", "read x; true"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    wait_until("one-shot holds", || is_busy(&dir, "conv1"));
    let (mut stub, mut c) = resident(&dir, "read x; true");
    assert_eq!(c.say("take conv1 c1"), "busy c1");
    assert_eq!(c.say("release conv1 c1"), "not-held c1");
    drop(one.stdin.take());
    one.wait().unwrap();
    assert_eq!(c.say("take conv1 c1"), "locked c1");
    drop(stub.stdin.take());
    stub.wait().unwrap();
}

#[test]
fn flags_are_checked() {
    let dir = scratch("flags");
    let run = |args: &[&str]| {
        let mut cmd = Command::new(STUB);
        cmd.args(args)
            .arg("--lock-dir")
            .arg(&dir)
            .args(["--", "true"]);
        // Whatever the harness left open at 77 or 9 is not ours to rely on.
        unsafe {
            cmd.pre_exec(|| {
                libc::close(77);
                libc::close(9);
                Ok(())
            });
        }
        cmd.status().unwrap().code()
    };
    assert_eq!(run(&["--resident"]), Some(64));
    assert_eq!(run(&["--resident", "--ctl-fd", "2"]), Some(64));
    assert_eq!(run(&["--resident", "--ctl-fd", "77"]), Some(64));
    assert_eq!(
        run(&["--resident", "--ctl-fd", "9", "--key", "k"]),
        Some(64)
    );
    assert_eq!(run(&["--ctl-fd", "9", "--key", "k"]), Some(64));
}

#[test]
fn sigterm_to_the_stub_reaches_the_engine() {
    let dir = scratch("term");
    let (mut stub, mut c) = resident(&dir, "echo $$; exec sleep 30");
    let engine: i32 = first_line(&mut stub).parse().unwrap();
    assert_eq!(c.say("take conv1 c1"), "locked c1");
    unsafe { libc::kill(stub.id() as i32, libc::SIGTERM) };
    assert_eq!(stub.wait().unwrap().code(), Some(128 + libc::SIGTERM));
    assert!(!alive(engine));
    assert!(!is_busy(&dir, "conv1"));
}

#[test]
fn a_ctl_fd_that_is_not_a_socket_is_refused() {
    let dir = scratch("notsock");
    let mut fds = [0 as libc::c_int; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let w = fds[1];
    let mut cmd = Command::new(STUB);
    cmd.args(["--resident", "--lock-dir"])
        .arg(&dir)
        .args(["--ctl-fd", "9", "--", "true"])
        .stderr(Stdio::piped());
    unsafe {
        cmd.pre_exec(move || {
            pass_as_9(w);
            Ok(())
        });
    }
    let out = cmd.output().unwrap();
    unsafe {
        libc::close(fds[0]);
        libc::close(fds[1]);
    }
    assert_eq!(out.status.code(), Some(64));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a socket"));
}

/// The redeem hook runs after the lock is taken and before `locked` is answered, in this process and with the lock held:
/// a second take of the key from inside it is busy (so the redeem sees the lock it is redeeming under), and the hook is
/// not run in a child (it sees this process's pid). A hook that fails frees the lock again.
#[test]
fn the_redeem_hook_runs_in_process_under_the_lock() {
    use std::cell::Cell;
    let dir = scratch("hook");
    let (ran, busy_inside, same_process) = (Cell::new(0), Cell::new(false), Cell::new(false));
    let me = std::process::id();
    let input = b"take convh c1\nrelease convh c1\ntake convh c2\n".as_slice();
    let mut out = Vec::new();
    kith_stub::resident::serve(&dir, input, &mut out, |lock| {
        ran.set(ran.get() + 1);
        busy_inside.set(matches!(take(&dir, "convh"), Ok(Taken::Busy)));
        same_process.set(std::process::id() == me && lock.path().exists());
        // The second take's redeem fails.
        if ran.get() == 2 {
            return Err("redeem refused".into());
        }
        Ok(())
    });
    let said = String::from_utf8(out).unwrap();
    assert_eq!(
        said.lines().collect::<Vec<_>>(),
        ["locked c1", "released c1", "error c2 redeem refused"]
    );
    assert_eq!(ran.get(), 2);
    assert!(busy_inside.get() && same_process.get());
    assert!(!is_busy(&dir, "convh"), "a failed redeem leaves no lock");
}
