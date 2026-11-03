//! The holder against a real holder process, driven by the blocking client: spawn, list, kill, ack, the secret, the
//! generation fence, the whole tree, and a holder that outlives the process that started it.

use kith_holder::client::{Client, ConnectError};
use kith_holder::proto::{self as p, AckState, KillState, Mode, Why};
use kith_holder::{launch, wire};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

const SECRET: &str = "0123456789abcdef0123456789abcdef";
const EXE: &str = env!("CARGO_BIN_EXE_kith-holder");
const WAIT: Duration = Duration::from_secs(20);

/// A holder in its own folder; shut down (its processes killed) when dropped.
struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        // Short: a Unix socket path holds about a hundred bytes.
        let dir = std::env::temp_dir().join(format!("kh-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Fixture { dir }
    }

    fn start(&self) {
        launch::ensure(EXE.as_ref(), &self.dir, SECRET, Duration::from_secs(120))
            .expect("holder starts");
    }

    fn client(&self, generation: u64) -> Client {
        Client::connect(&self.dir, SECRET, generation).expect("hello accepted")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // The newest generation may be anything; ask with a huge one.
        if let Ok(mut c) = Client::connect(&self.dir, SECRET, u64::MAX - 1) {
            let _ = c.send(p::SHUTDOWN, 0, &[]);
            c.closed(Duration::from_secs(5));
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn env() -> BTreeMap<String, String> {
    std::env::vars()
        .filter(|(k, _)| k.eq_ignore_ascii_case("PATH") || k.eq_ignore_ascii_case("SystemRoot"))
        .collect()
}

fn spec(conv: &str, claim: u64, generation: u64, mode: Mode, script: &[&str]) -> p::Spawn {
    p::Spawn {
        conv: conv.into(),
        claim,
        generation,
        argv: script.iter().map(|s| s.to_string()).collect(),
        env: env(),
        cwd: std::env::temp_dir().display().to_string(),
        mode,
        cols: 80,
        rows: 24,
    }
}

/// A shell line as argv, whatever the platform's shell is.
fn shell(line: &str) -> Vec<&str> {
    if cfg!(windows) {
        vec!["cmd.exe", "/d", "/c", line]
    } else {
        vec!["/bin/sh", "-c", line]
    }
}

/// A shell line that runs until killed.
fn sleep_line() -> &'static str {
    if cfg!(windows) {
        "ping -n 300 127.0.0.1 > nul"
    } else {
        "exec sleep 300"
    }
}

fn spawn(c: &mut Client, s: &p::Spawn) -> u32 {
    c.spawn(s).unwrap();
    let (_, id, payload) = c.expect(p::SPAWNED, 0, WAIT).expect("spawned");
    let spawned: p::Spawned = wire::parse(&payload).unwrap();
    assert_eq!(
        (spawned.conv.as_str(), spawned.claim),
        (s.conv.as_str(), s.claim)
    );
    id
}

/// Reads the process's output (and stderr) until `needle` shows up.
fn read_until(c: &mut Client, id: u32, needle: &str) -> String {
    let deadline = Instant::now() + WAIT;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        if let Some((_, _, bytes)) = c.take(Duration::from_millis(200), |(k, i, _)| {
            (*k == p::OUTPUT || *k == p::STDERR) && *i == id
        }) {
            seen.extend(bytes);
            if String::from_utf8_lossy(&seen).contains(needle) {
                return String::from_utf8_lossy(&seen).into_owned();
            }
        }
    }
    panic!("no {needle:?} in {:?}", String::from_utf8_lossy(&seen));
}

/// Kills until the holder says gone, as a core would.
fn kill_until_gone(c: &mut Client, id: u32) {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if c.kill(id).unwrap() == KillState::Gone {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("kill never reached gone");
}

fn exit_of(c: &mut Client, id: u32) -> p::Exit {
    let (_, _, payload) = c.expect(p::EXITED, id, WAIT).expect("exited");
    wire::parse(&payload).unwrap()
}

#[test]
fn spawn_list_kill_and_ack() {
    let fx = Fixture::new("basic");
    fx.start();
    let mut c = fx.client(1);
    assert!(c.welcome.procs.is_empty());
    let line = if cfg!(windows) {
        "echo hello && ping -n 300 127.0.0.1 > nul"
    } else {
        "echo hello; exec sleep 300"
    };
    let id = spawn(&mut c, &spec("conv-a", 7, 1, Mode::Pipes, &shell(line)));
    read_until(&mut c, id, "hello");
    let held = c.list().unwrap();
    assert_eq!(held.len(), 1);
    assert_eq!(
        (
            held[0].conv.as_str(),
            held[0].claim,
            held[0].generation,
            held[0].exit
        ),
        ("conv-a", 7, 1, None)
    );
    // Nothing to ack while it runs.
    assert_eq!(c.ack(id).unwrap(), AckState::Running);
    // Spawning the same claim again answers the running process; it does not start a second one.
    assert_eq!(
        spawn(&mut c, &spec("conv-a", 7, 1, Mode::Pipes, &shell(line))),
        id
    );
    assert_eq!(c.list().unwrap().len(), 1);
    kill_until_gone(&mut c, id);
    let exit = exit_of(&mut c, id);
    if cfg!(unix) {
        assert_eq!(exit.signal, Some(9));
    }
    // Killing again, and killing something unknown, both say gone.
    assert_eq!(c.kill(id).unwrap(), KillState::Gone);
    assert_eq!(c.kill(9999).unwrap(), KillState::Gone);
    // The exit stays listed until acked, then it is gone.
    assert_eq!(c.list().unwrap()[0].exit, Some(exit));
    assert_eq!(c.ack(id).unwrap(), AckState::Acked);
    assert_eq!(c.ack(id).unwrap(), AckState::Gone);
    assert!(c.list().unwrap().is_empty());
    assert_eq!(c.kill(id).unwrap(), KillState::Gone);
}

#[test]
fn a_terminal_takes_input_and_keeps_a_tail() {
    let fx = Fixture::new("pty");
    fx.start();
    let mut c = fx.client(1);
    // cmd expands %x% when it parses the whole line, before `set /p` has run, so the Windows
    // line needs delayed expansion (!x!) to see the typed input; the holder delivered it fine.
    let argv: Vec<&str> = if cfg!(windows) {
        vec![
            "cmd.exe",
            "/d",
            "/v:on",
            "/c",
            "set /p x=& echo got:!x!& ping -n 300 127.0.0.1 > nul",
        ]
    } else {
        shell("read x; echo got:$x; exec sleep 300")
    };
    let id = spawn(&mut c, &spec("conv-t", 1, 1, Mode::Pty, &argv));
    c.send(p::RESIZE, id, br#"{"cols":100,"rows":30}"#).unwrap();
    c.send(p::INPUT, id, b"ping\r").unwrap();
    read_until(&mut c, id, "got:ping");
    assert!(String::from_utf8_lossy(&c.tail(id).unwrap()).contains("got:ping"));
    assert_eq!(c.list().unwrap()[0].cols, 100);
    kill_until_gone(&mut c, id);
}

#[test]
fn the_wrong_secret_is_refused() {
    let fx = Fixture::new("secret");
    fx.start();
    // A client with the wrong key cannot verify the holder's proof, so it stops there.
    match Client::connect(&fx.dir, "not-the-secret-not-the-secret-00", 1) {
        Err(ConnectError::Impostor) => {}
        other => panic!(
            "expected the holder to be distrusted, got {:?}",
            other.err()
        ),
    }
    // A raw peer that answers the challenge with a wrong proof is refused by the holder.
    let ep = kith_holder::platform::endpoint(&fx.dir);
    let mut raw = kith_holder::platform::connect(&ep).unwrap();
    let hello = p::Hello {
        version: p::VERSION,
        generation: 1,
        nonce: "aa".repeat(16),
    };
    wire::write_json(&mut raw, p::HELLO, 0, &hello).unwrap();
    let (kind, _, _) = wire::read(&mut raw).unwrap().unwrap();
    assert_eq!(kind, p::CHALLENGE);
    wire::write_json(&mut raw, p::AUTH, 0, &p::Auth { proof: "00".into() }).unwrap();
    let (kind, _, payload) = wire::read(&mut raw).unwrap().unwrap();
    assert_eq!(kind, p::REFUSED);
    let refused: p::Refused = wire::parse(&payload).unwrap();
    assert_eq!(refused.why, Why::BadSecret);
    // A refused hello did not claim a generation.
    fx.client(1);
}

#[test]
fn a_stale_generation_is_refused_and_a_newer_core_takes_over() {
    let fx = Fixture::new("fence");
    fx.start();
    let mut old = fx.client(5);
    let id = spawn(
        &mut old,
        &spec("conv-f", 1, 5, Mode::Pipes, &shell(sleep_line())),
    );
    match Client::connect(&fx.dir, SECRET, 4) {
        Err(ConnectError::Refused(r)) => assert_eq!((r.why, r.current), (Why::StaleGeneration, 5)),
        other => panic!("expected a refusal, got {:?}", other.err()),
    }
    let mut new = fx.client(6);
    // The old core is cut off: its connection is closed, and nothing it still sends counts.
    assert!(old.closed(WAIT));
    let _ = old.send(p::KILL, id, &[]);
    // The new core sees the old core's process and can control it.
    assert_eq!(new.welcome.procs.len(), 1);
    assert_eq!(new.welcome.procs[0].id, id);
    // A spawn decided under another generation is refused, even from the newest connection.
    new.spawn(&spec("conv-g", 2, 5, Mode::Pipes, &shell("exit 0")))
        .unwrap();
    let (_, _, payload) = new.expect(p::REFUSED, 0, WAIT).expect("refused");
    let refused: p::Refused = wire::parse(&payload).unwrap();
    assert_eq!((refused.why, refused.current), (Why::StaleGeneration, 6));
    // And the old core's kill did nothing.
    std::thread::sleep(Duration::from_millis(300));
    let held = new.list().unwrap();
    assert_eq!((held.len(), held[0].exit), (1, None));
    kill_until_gone(&mut new, id);
}

#[test]
fn an_exit_is_kept_until_it_is_acked_across_cores() {
    let fx = Fixture::new("exit");
    fx.start();
    let mut c = fx.client(1);
    let id = spawn(&mut c, &spec("conv-e", 3, 1, Mode::Pipes, &shell("exit 3")));
    let exit = exit_of(&mut c, id);
    assert_eq!(exit.code, Some(3));
    drop(c);
    // A later core finds the exit unacked, with its conversation and claim, and acks it.
    let mut next = fx.client(2);
    let held = &next.welcome.procs;
    assert_eq!(
        (
            held.len(),
            held[0].conv.as_str(),
            held[0].claim,
            held[0].exit
        ),
        (1, "conv-e", 3, Some(exit))
    );
    assert_eq!(next.ack(id).unwrap(), AckState::Acked);
    assert!(next.list().unwrap().is_empty());
}

#[cfg(unix)]
fn alive(pid: i32) -> bool {
    // A zombie still answers signal 0; the holder's own children are reaped, a grandchild is init's.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(unix)]
#[test]
fn kill_takes_the_whole_tree() {
    let fx = Fixture::new("tree");
    fx.start();
    let mut c = fx.client(1);
    let id = spawn(
        &mut c,
        &spec(
            "conv-k",
            1,
            1,
            Mode::Pipes,
            &shell("sleep 300 & echo child:$!; wait"),
        ),
    );
    let seen = read_until(&mut c, id, "child:");
    let pid: i32 = seen
        .split("child:")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(alive(pid));
    kill_until_gone(&mut c, id);
    let deadline = Instant::now() + WAIT;
    while alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!alive(pid), "the grandchild outlived the kill");
}

#[cfg(windows)]
#[test]
fn kill_takes_the_whole_tree() {
    // cmd waits for ping: the job holds both, and "gone" means the job is empty.
    let fx = Fixture::new("tree");
    fx.start();
    let mut c = fx.client(1);
    let id = spawn(
        &mut c,
        &spec(
            "conv-k",
            1,
            1,
            Mode::Pipes,
            &shell("ping -n 300 127.0.0.1 > nul"),
        ),
    );
    std::thread::sleep(Duration::from_millis(500));
    kill_until_gone(&mut c, id);
    exit_of(&mut c, id);
}

/// The holder is started by a process that then dies, with its whole process group on Unix; the holder and its
/// process stay, and a newer core finds and controls the process: "restart keeps a test process".
#[test]
fn the_holder_and_its_process_outlive_the_process_that_started_it() {
    let fx = Fixture::new("survive");
    // `start` is the would-be daemon: it starts the holder detached and exits.
    let mut parent = Command::new(EXE);
    parent
        .args(["start", "--dir"])
        .arg(&fx.dir)
        .env(launch::SECRET_VAR, SECRET);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own session, so killing its group below reaches it and whatever it left, as a dying daemon's would.
        unsafe {
            parent.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    let mut parent = parent.spawn().unwrap();
    assert!(parent.wait().unwrap().success());
    #[cfg(unix)]
    unsafe {
        libc::killpg(parent.id() as i32, libc::SIGKILL);
    }

    let mut first = fx.client(1);
    let holder_pid = first.welcome.pid;
    let id = spawn(
        &mut first,
        &spec("conv-s", 1, 1, Mode::Pipes, &shell(sleep_line())),
    );
    let child_pid = first.list().unwrap()[0].pid;
    // The core goes away too (a crash or an update).
    drop(first);
    std::thread::sleep(Duration::from_millis(300));

    let mut second = fx.client(2);
    assert_eq!(second.welcome.pid, holder_pid, "the same holder answers");
    let held = second.list().unwrap();
    assert_eq!(
        (held.len(), held[0].id, held[0].pid, held[0].exit),
        (1, id, child_pid, None)
    );
    #[cfg(unix)]
    assert!(alive(child_pid as i32));
    kill_until_gone(&mut second, id);
    exit_of(&mut second, id);
    assert_eq!(second.ack(id).unwrap(), AckState::Acked);
}

#[test]
fn a_holder_that_does_not_know_the_key_fails_the_handshake() {
    use kith_holder::{auth, platform};
    let fx = Fixture::new("fake");
    platform::private_dir(&fx.dir).unwrap();
    let listener = platform::listen(&platform::endpoint(&fx.dir)).unwrap();
    let fake = std::thread::spawn(move || {
        let mut s = platform::accept(&listener).unwrap();
        let (_, _, payload) = wire::read(&mut s).unwrap().unwrap();
        let hello: p::Hello = wire::parse(&payload).unwrap();
        let nonce = "ff".repeat(16);
        // Well formed, but made with another key: all a holder without the secret can do.
        let proof = auth::proof(
            b"not-the-key-not-the-key",
            auth::HOLDER,
            &hello.nonce,
            &nonce,
            1,
        );
        wire::write_json(&mut s, p::CHALLENGE, 0, &p::Challenge { nonce, proof }).unwrap();
        // Whatever it says next is never read as a welcome.
        let _ = wire::read(&mut s);
    });
    match Client::connect(&fx.dir, SECRET, 1) {
        Err(ConnectError::Impostor) => {}
        other => panic!("expected the impostor to be refused, got {:?}", other.err()),
    }
    fake.join().unwrap();
}

/// Two holders started at once on a stale socket: the second cannot take the folder.
#[cfg(unix)]
#[test]
fn two_concurrent_starts_on_a_stale_socket_leave_one_holder() {
    let fx = Fixture::new("race");
    std::fs::create_dir_all(&fx.dir).unwrap();
    // A socket file nobody listens on, as a crashed holder leaves it.
    drop(std::os::unix::net::UnixListener::bind(fx.dir.join("holder.sock")).unwrap());
    let serve = || {
        Command::new(EXE)
            .args(["serve", "--dir"])
            .arg(&fx.dir)
            .env(launch::SECRET_VAR, SECRET)
            .spawn()
            .unwrap()
    };
    let mut kids = [serve(), serve()];
    std::thread::sleep(Duration::from_secs(2));
    let running = kids
        .iter_mut()
        .map(|k| k.try_wait().unwrap().is_none())
        .filter(|alive| *alive)
        .count();
    assert_eq!(running, 1, "exactly one holder keeps the folder");
    fx.client(1);
    for k in &mut kids {
        let _ = k.kill();
        let _ = k.wait();
    }
}
