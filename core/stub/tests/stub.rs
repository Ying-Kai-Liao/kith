//! The `kith-stub` binary against real processes (Unix; the Windows path is the same lock plus std::process and is
//! covered by CI compiling it). Ctrl-Z / job control with a real shell is manual: see the PR.
#![cfg(unix)]

use kith_stub::{Taken, take};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::io::FromRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const STUB: &str = env!("CARGO_BIN_EXE_kith-stub");

fn scratch(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "kith-stub-bin-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn stub(dir: &Path, key: &str, engine: &[&str]) -> Command {
    let mut c = Command::new(STUB);
    c.arg("--lock-dir").arg(dir).args(["--key", key, "--"]).args(engine);
    c
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

fn first_line(child: &mut Child) -> String {
    let mut s = String::new();
    BufReader::new(child.stdout.as_mut().unwrap()).read_line(&mut s).unwrap();
    s.trim().to_string()
}

fn kill(pid: i32, sig: i32) {
    unsafe { libc::kill(pid, sig) };
}

/// Engine that prints its pid and then sleeps.
const SLEEPER: &str = "echo $$; exec sleep 30";

#[test]
fn second_stub_on_same_key_is_busy() {
    let dir = scratch("busy");
    let mut a = stub(&dir, "k1", &["sh", "-c", SLEEPER]).stdout(Stdio::piped()).spawn().unwrap();
    let engine: i32 = first_line(&mut a).parse().unwrap();
    let b = stub(&dir, "k1", &["true"]).output().unwrap();
    assert_eq!(b.status.code(), Some(75));
    assert!(String::from_utf8_lossy(&b.stderr).starts_with("kith-stub: busy"));
    // Another key is free.
    assert_eq!(stub(&dir, "k2", &["true"]).status().unwrap().code(), Some(0));
    kill(engine, libc::SIGKILL);
    assert_eq!(a.wait().unwrap().code(), Some(128 + libc::SIGKILL));
    assert!(!is_busy(&dir, "k1"));
}

#[test]
fn exit_codes_pass_through_and_release_the_lock() {
    let dir = scratch("codes");
    assert_eq!(stub(&dir, "k", &["sh", "-c", "exit 7"]).status().unwrap().code(), Some(7));
    assert_eq!(stub(&dir, "k", &["sh", "-c", "kill -TERM $$"]).status().unwrap().code(), Some(128 + libc::SIGTERM));
    let missing = stub(&dir, "k", &["/nonexistent/engine"]).output().unwrap();
    assert_eq!(missing.status.code(), Some(127));
    assert!(!is_busy(&dir, "k"));
    assert_eq!(stub(&dir, "k", &[]).status().unwrap().code(), Some(64));
    assert_eq!(stub(&dir, "a/b", &["true"]).status().unwrap().code(), Some(70));
}

#[test]
fn killing_the_starter_keeps_stub_engine_and_lock() {
    let dir = scratch("starter");
    // The starter is a shell that backgrounds the stub, prints both pids and waits.
    let script = format!("{STUB:?} --lock-dir {dir:?} --key k -- sh -c 'echo $$; exec sleep 30' & echo $!; wait");
    let mut starter = Command::new("sh").args(["-c", &script]).stdout(Stdio::piped()).spawn().unwrap();
    let stub_pid: i32 = first_line(&mut starter).parse().unwrap();
    let engine: i32 = first_line(&mut starter).parse().unwrap();
    wait_until("lock held", || is_busy(&dir, "k"));

    kill(starter.id() as i32, libc::SIGKILL);
    starter.wait().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(alive(stub_pid) && alive(engine));
    assert!(is_busy(&dir, "k"), "the lock must outlive the process that started the stub");

    kill(engine, libc::SIGKILL);
    wait_until("lock released after engine exit", || !is_busy(&dir, "k"));
}

#[test]
fn engines_background_child_does_not_keep_the_lock() {
    let dir = scratch("nohup");
    let mut s = stub(&dir, "k", &["sh", "-c", "nohup sleep 30 >/dev/null 2>&1 & echo $!"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let sleeper: i32 = first_line(&mut s).parse().unwrap();
    assert_eq!(s.wait().unwrap().code(), Some(0));
    assert!(alive(sleeper));
    assert!(!is_busy(&dir, "k"), "a surviving grandchild must not hold the lock");
    kill(sleeper, libc::SIGKILL);
}

#[test]
fn holder_record_names_the_stub() {
    let dir = scratch("holder");
    let mut s = stub(&dir, "k", &["sh", "-c", SLEEPER]).stdout(Stdio::piped()).spawn().unwrap();
    let engine: i32 = first_line(&mut s).parse().unwrap();
    let path = dir.join("w-k.holder");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with(&format!("pid={}\nstarted=", s.id())), "{text:?}");
    let mode = std::os::unix::fs::MetadataExt::mode(&std::fs::metadata(&path).unwrap());
    assert_eq!(mode & 0o777, 0o600);
    kill(engine, libc::SIGKILL);
    s.wait().unwrap();
    // A stale record is overwritten by the next stub.
    std::fs::write(&path, "pid=1\nstarted=0\n").unwrap();
    assert_eq!(stub(&dir, "k", &["true"]).status().unwrap().code(), Some(0));
    assert!(!std::fs::read_to_string(&path).unwrap().starts_with("pid=1\n"));
}

#[test]
fn ctrl_c_on_a_terminal_reaches_the_engine_not_the_stub() {
    let dir = scratch("pty");
    let (mut master, slave) = unsafe {
        let (mut m, mut s) = (0, 0);
        assert_eq!(libc::openpty(&mut m, &mut s, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()), 0);
        // Not inherited by the stub beyond the dup'd stdio.
        libc::fcntl(m, libc::F_SETFD, libc::FD_CLOEXEC);
        libc::fcntl(s, libc::F_SETFD, libc::FD_CLOEXEC);
        (std::fs::File::from_raw_fd(m), s)
    };
    let engine = "trap 'echo got-int; exit 3' INT; echo ready; while :; do sleep 0.1; done";
    let mut cmd = stub(&dir, "k", &["sh", "-c", engine]);
    unsafe {
        cmd.stdin(Stdio::from_raw_fd(libc::dup(slave)))
            .stdout(Stdio::from_raw_fd(libc::dup(slave)))
            .stderr(Stdio::from_raw_fd(libc::dup(slave)))
            .pre_exec(move || {
                // The stub leads a new session whose controlling terminal is the pty, so it starts as the
                // foreground group, as a shell's job would.
                libc::setsid();
                libc::ioctl(0, libc::TIOCSCTTY as _, 0);
                Ok(())
            });
    }
    let mut stub = cmd.spawn().unwrap();
    unsafe { libc::close(slave) };

    // Read the master until `want` shows up.
    let mut seen = String::new();
    let read_until = |master: &mut std::fs::File, seen: &mut String, want: &str| {
        let end = Instant::now() + Duration::from_secs(20);
        while !seen.contains(want) {
            assert!(Instant::now() < end, "timed out waiting for {want:?}; saw {seen:?}");
            let mut fds =
                libc::pollfd { fd: std::os::unix::io::AsRawFd::as_raw_fd(&*master), events: libc::POLLIN, revents: 0 };
            if unsafe { libc::poll(&mut fds, 1, 100) } > 0 {
                let mut buf = [0u8; 256];
                match master.read(&mut buf) {
                    Ok(n) if n > 0 => seen.push_str(&String::from_utf8_lossy(&buf[..n])),
                    _ => {}
                }
            }
        }
    };
    read_until(&mut master, &mut seen, "ready");
    master.write_all(&[0x03]).unwrap();
    read_until(&mut master, &mut seen, "got-int");
    let status = stub.wait().unwrap();
    assert_eq!(status.code(), Some(3), "the stub passes the engine's code; it did not die of the interrupt");
    assert!(!is_busy(&dir, "k"));
}

/// The holder kills a terminal run with `killpg(stub, KILL)` and decides "gone" from `killpg(stub, 0)`: both must
/// see the engine, so the engine must be in the stub's group.
#[test]
fn engine_shares_the_stubs_group() {
    let dir = scratch("group");
    let mut cmd = stub(&dir, "k", &["sh", "-c", SLEEPER]);
    cmd.stdout(Stdio::piped());
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut s = cmd.spawn().unwrap();
    let stub_pid = s.id() as i32;
    let engine: i32 = first_line(&mut s).parse().unwrap();
    // Even with the stub gone the group is not empty while the engine lives.
    kill(stub_pid, libc::SIGKILL);
    s.wait().unwrap();
    #[cfg(target_os = "linux")]
    wait_until("engine dies with the stub (PDEATHSIG)", || !alive(engine));
    #[cfg(not(target_os = "linux"))]
    {
        assert_eq!(unsafe { libc::killpg(stub_pid, 0) }, 0, "the group must still hold the engine");
        assert_eq!(unsafe { libc::getpgid(engine) }, stub_pid);
        assert_eq!(unsafe { libc::killpg(stub_pid, libc::SIGKILL) }, 0);
        wait_until("engine killed by the group kill", || !alive(engine));
    }
}

#[test]
fn status_fd_says_locked_then_busy() {
    let dir = scratch("statusfd");
    let mut a = stub(&dir, "k", &["sh", "-c", SLEEPER]).stdout(Stdio::piped()).spawn().unwrap();
    let engine: i32 = first_line(&mut a).parse().unwrap();
    // A pipe whose write end becomes fd 9 in the stub.
    let mut fds = [0 as libc::c_int; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let (r, w) = (fds[0], fds[1]);
    let mut cmd = Command::new(STUB);
    cmd.arg("--lock-dir").arg(&dir).args(["--key", "k", "--status-fd", "9", "--", "true"]);
    unsafe {
        cmd.pre_exec(move || {
            libc::dup2(w, 9);
            Ok(())
        });
    }
    assert_eq!(cmd.status().unwrap().code(), Some(75));
    unsafe { libc::close(w) };
    let mut text = String::new();
    unsafe { std::fs::File::from_raw_fd(r) }.read_to_string(&mut text).unwrap();
    assert_eq!(text, "busy\n");
    kill(engine, libc::SIGKILL);
    a.wait().unwrap();
}
