//! The holder against a real holder process, driven by the blocking client: spawn, list, kill, ack, the secret, the
//! generation fence, the whole tree, and a holder that outlives the process that started it.

use kith_holder::client::{Client, ConnectError};
use kith_holder::proto::{self as p, AckState, KillState, Mode, Why};
use kith_holder::{launch, wire};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
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
        // Built before any clock runs: a lease or a sleep in a test must not wait on cargo.
        stub_and_locks();
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

/// Starts `cmd` with the secret on its stdin, the way `launch::ensure` hands it to a holder.
fn spawn_with_secret(cmd: &mut Command) -> std::process::Child {
    let mut child = cmd.stdin(Stdio::piped()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, "{SECRET}").unwrap();
    child
}

fn env() -> BTreeMap<String, String> {
    std::env::vars()
        .filter(|(k, _)| k.eq_ignore_ascii_case("PATH") || k.eq_ignore_ascii_case("SystemRoot"))
        .collect()
}

/// The built `kith-stub` (the holder runs every pipes engine under it) and a lock folder for this test process.
fn stub_and_locks() -> (String, String) {
    static BUILT: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
            let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../stub/Cargo.toml");
            let built = Command::new(cargo)
                .args(["build", "--manifest-path"])
                .arg(&manifest)
                .status()
                .unwrap();
            assert!(built.success(), "kith-stub did not build");
            // The test binary is in target/<profile>/deps; the stub is in target/<profile>.
            let exe = std::env::current_exe().unwrap();
            let name = if cfg!(windows) {
                "kith-stub.exe"
            } else {
                "kith-stub"
            };
            let stub = exe.parent().unwrap().parent().unwrap().join(name);
            assert!(stub.is_file(), "no {}", stub.display());
            let locks = std::env::temp_dir().join(format!("kh-locks-{}", std::process::id()));
            (stub.display().to_string(), locks.display().to_string())
        })
        .clone()
}

fn spec(conv: &str, claim: u64, generation: u64, mode: Mode, script: &[&str]) -> p::Spawn {
    let (stub, locks) = stub_and_locks();
    p::Spawn {
        stub: (mode == Mode::Pipes).then_some(stub),
        lock_dir: (mode == Mode::Pipes).then_some(locks),
        conv: conv.into(),
        claim,
        generation,
        argv: script.iter().map(|s| s.to_string()).collect(),
        env: env(),
        cwd: std::env::temp_dir().display().to_string(),
        mode,
        cols: 80,
        rows: 24,
        kill_on_lease_loss: false,
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

/// An id of the holder `c` is connected to, as a core that stored it from that holder would have it.
fn h(c: &Client, id: u32) -> p::HeldId {
    p::HeldId {
        instance: c.welcome.instance.clone(),
        id,
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
        if c.kill(&h(c, id)).unwrap() == KillState::Gone {
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
            held[0].exit.clone()
        ),
        ("conv-a", 7, 1, None)
    );
    // Nothing to ack while it runs.
    assert_eq!(c.ack(&h(&c, id)).unwrap(), AckState::Running);
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
    assert_eq!(c.kill(&h(&c, id)).unwrap(), KillState::Gone);
    assert_eq!(c.kill(&h(&c, 9999)).unwrap(), KillState::Gone);
    // The exit stays listed until acked, then it is gone.
    assert_eq!(c.list().unwrap()[0].exit, Some(exit));
    assert_eq!(c.ack(&h(&c, id)).unwrap(), AckState::Acked);
    assert_eq!(c.ack(&h(&c, id)).unwrap(), AckState::Gone);
    assert!(c.list().unwrap().is_empty());
    assert_eq!(c.kill(&h(&c, id)).unwrap(), KillState::Gone);
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
    c.resize(&h(&c, id), 100, 30).unwrap();
    c.input(&h(&c, id), b"ping\r").unwrap();
    read_until(&mut c, id, "got:ping");
    assert!(String::from_utf8_lossy(&c.tail(&h(&c, id)).unwrap()).contains("got:ping"));
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
    let instance = old.welcome.instance.clone();
    let _ = old.send_for(p::KILL, &instance, id, &[]);
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
    assert_eq!((held.len(), held[0].exit.clone()), (1, None));
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
            held[0].exit.clone()
        ),
        (1, "conv-e", 3, Some(exit))
    );
    assert_eq!(next.ack(&h(&next, id)).unwrap(), AckState::Acked);
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
    parent.args(["start", "--dir"]).arg(&fx.dir);
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
    let mut parent = spawn_with_secret(&mut parent);
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
        (held.len(), held[0].id, held[0].pid, held[0].exit.clone()),
        (1, id, child_pid, None)
    );
    #[cfg(unix)]
    assert!(alive(child_pid as i32));
    kill_until_gone(&mut second, id);
    exit_of(&mut second, id);
    assert_eq!(second.ack(&h(&second, id)).unwrap(), AckState::Acked);
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
    let serve = || spawn_with_secret(Command::new(EXE).args(["serve", "--dir"]).arg(&fx.dir));
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

/// The environment a process was started with, as another process of this user can read it.
#[cfg(all(unix, not(target_os = "macos")))]
fn environment_of(pid: u32) -> String {
    let raw = std::fs::read(format!("/proc/{pid}/environ")).unwrap_or_default();
    String::from_utf8_lossy(&raw).replace('\0', " ")
}

/// The environment a process was started with, as another process of this user can read it.
#[cfg(target_os = "macos")]
fn environment_of(pid: u32) -> String {
    // The same call `ps -E` makes. The buffer holds argc, the program, the arguments and the environment.
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let mut buf = vec![0u8; 1 << 20];
    let mut len = buf.len();
    let ok = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    assert_eq!(ok, 0, "{}", std::io::Error::last_os_error());
    String::from_utf8_lossy(&buf[..len]).replace('\0', " ")
}

/// The secret reaches the holder over stdin: it is in neither the environment it was started with (readable by any
/// process of this user for the holder's life) nor its command line.
#[cfg(unix)]
#[test]
fn the_holders_environment_and_command_line_hold_no_secret() {
    // A control, so a reader that sees nothing at all cannot make this pass.
    // (Not a system program: macOS hides the environment of those. A holder waiting for its secret is one that
    // is not.)
    let mut control = Command::new(EXE)
        .args(["serve", "--dir"])
        .arg(std::env::temp_dir().join(format!("kh-{}-control", std::process::id())))
        .env("KITH_CONTROL_VAR", "control-value")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        environment_of(control.id()).contains("control-value"),
        "this system's process listing shows environments"
    );
    let _ = control.kill();
    let _ = control.wait();

    let fx = Fixture::new("env");
    fx.start();
    let c = fx.client(1);
    let seen = environment_of(c.welcome.pid);
    assert!(
        !seen.contains(SECRET),
        "the holder's environment has the secret: {seen}"
    );
    assert!(!seen.contains("KITH_HOLDER_SECRET"), "{seen}");
}

#[cfg(unix)]
#[test]
fn a_holder_on_unix_is_not_tied_to_its_starter() {
    let fx = Fixture::new("tied");
    fx.start();
    assert!(!fx.client(1).tied_to_starter());
}

/// One holder per folder, on every platform: a second lock on the folder is refused while the first is held, and
/// works again once it is let go.
#[test]
fn only_one_lock_on_a_holder_folder_at_a_time() {
    use kith_holder::platform;
    let fx = Fixture::new("lock");
    platform::private_dir(&fx.dir).unwrap();
    let first = platform::lock_instance(&fx.dir)
        .unwrap()
        .expect("first lock");
    assert!(platform::lock_instance(&fx.dir).unwrap().is_none());
    drop(first);
    // A process another test forks at this moment holds the descriptor until its exec closes it (O_CLOEXEC), so the
    // lock can come back a few milliseconds late.
    let deadline = Instant::now() + Duration::from_secs(5);
    while platform::lock_instance(&fx.dir).unwrap().is_none() {
        assert!(Instant::now() < deadline, "the lock was never released");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A folder whose socket path is over the limit (macOS 104 bytes, Linux 108) is a clear error, not a bind failure.
#[cfg(unix)]
#[test]
fn a_folder_with_too_long_a_socket_path_is_a_clear_error() {
    let dir = std::env::temp_dir().join("x".repeat(120));
    let err = launch::ensure(EXE.as_ref(), &dir, SECRET, Duration::from_secs(60)).unwrap_err();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(err.to_string().contains("too long"), "{err}");
}

#[test]
fn a_secret_is_one_line() {
    assert_eq!(launch::read_secret(&b"abc\n"[..]).unwrap(), "abc");
    assert!(launch::read_secret(&b"\n"[..]).is_err());
    assert!(launch::read_secret(&b""[..]).is_err());
}

// --- the holder over a command's stdio (`ssh host kith-holder --stdio`, here run directly) ---------------------------

impl Fixture {
    /// The relay a remote shell would run, without the shell: `kith-holder --stdio` on this folder.
    fn relay(&self) -> Command {
        let mut cmd = Command::new(EXE);
        cmd.arg("--stdio").arg("--dir").arg(&self.dir);
        cmd
    }

    fn over_stdio(&self, generation: u64) -> Client {
        Client::over_command(self.relay(), SECRET, generation, WAIT)
            .expect("hello over stdio accepted")
    }
}

#[test]
fn a_stdio_link_starts_the_holder_when_none_runs_and_it_outlives_the_link() {
    let fx = Fixture::new("sstart");
    // No `start` here: the relay starts the holder, detached, as the machine's own holder would be.
    let mut c = fx.over_stdio(1);
    let id = spawn(
        &mut c,
        &spec("conv-s", 1, 1, Mode::Pipes, &shell(sleep_line())),
    );
    drop(c);
    // The link is gone (the relay was killed); the holder and its process are not.
    let mut again = fx.client(1);
    assert_eq!(again.list().unwrap().len(), 1);
    kill_until_gone(&mut again, id);
}

#[test]
fn a_dropped_stdio_link_keeps_the_process_and_a_reconnect_resumes_from_the_tail() {
    let fx = Fixture::new("sdrop");
    fx.start();
    let mut c = fx.over_stdio(1);
    let sep = if cfg!(windows) { "&" } else { ";" };
    let line = format!("echo before-the-drop {sep} {}", sleep_line());
    let id = spawn(&mut c, &spec("conv-d", 7, 1, Mode::Pipes, &shell(&line)));
    read_until(&mut c, id, "before-the-drop");
    drop(c);
    let mut back = fx.over_stdio(1);
    let held = back.welcome.procs.clone();
    assert_eq!(
        (held.len(), held[0].id, held[0].claim, held[0].exit.clone()),
        (1, id, 7, None)
    );
    assert!(
        String::from_utf8_lossy(&back.tail(&h(&back, id)).unwrap()).contains("before-the-drop"),
        "the tail resumes what the link missed"
    );
    // The same spawn again (the link dropped mid-spawn) answers with the process already running.
    let again = spawn(&mut back, &spec("conv-d", 7, 1, Mode::Pipes, &shell(&line)));
    assert_eq!(again, id);
    assert_eq!(back.list().unwrap().len(), 1);
    kill_until_gone(&mut back, id);
}

#[test]
fn an_exit_during_a_stdio_drop_is_kept_until_acked_over_the_next_link() {
    let fx = Fixture::new("sexit");
    fx.start();
    let mut c = fx.over_stdio(1);
    let line = if cfg!(windows) {
        "ping -n 2 127.0.0.1 > nul & exit 4"
    } else {
        "sleep 1; exit 4"
    };
    let id = spawn(&mut c, &spec("conv-x", 9, 1, Mode::Pipes, &shell(line)));
    drop(c);
    // Nobody is connected when it exits.
    std::thread::sleep(Duration::from_millis(2500));
    let mut next = fx.over_stdio(1);
    let held = &next.welcome.procs;
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].exit.as_ref().map(|e| e.code), Some(Some(4)));
    assert_eq!(next.ack(&h(&next, id)).unwrap(), AckState::Acked);
    assert!(next.list().unwrap().is_empty());
}

#[test]
fn a_stdio_link_with_the_wrong_pairing_key_is_refused() {
    let fx = Fixture::new("skey");
    fx.start();
    match Client::over_command(fx.relay(), "not-the-secret-not-the-secret-00", 1, WAIT) {
        Err(ConnectError::Impostor) => {}
        other => panic!(
            "expected the holder to be distrusted, got {:?}",
            other.err()
        ),
    }
    // The failed link did not claim a generation or leave a connection behind.
    fx.over_stdio(1);
}

#[test]
fn an_older_generation_over_stdio_is_refused_after_a_newer_core_connected() {
    let fx = Fixture::new("sgen");
    fx.start();
    let mut first = fx.over_stdio(5);
    match Client::over_command(fx.relay(), SECRET, 4, WAIT) {
        Err(ConnectError::Refused(r)) => assert_eq!((r.why, r.current), (Why::StaleGeneration, 5)),
        other => panic!("expected a refusal, got {:?}", other.err()),
    }
    // A newer core over a second link is the only one that drives; the first is cut off.
    let mut newest = fx.over_stdio(6);
    assert!(first.closed(WAIT));
    assert!(newest.list().unwrap().is_empty());
    // A local-socket client of the older generation is refused just the same.
    assert!(matches!(
        Client::connect(&fx.dir, SECRET, 5),
        Err(ConnectError::Refused(_))
    ));
}

#[cfg(unix)]
#[test]
fn a_command_that_never_answers_is_ended_at_the_deadline_and_its_stderr_is_kept() {
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", "echo connection stuck >&2; exec sleep 300"]);
    let began = Instant::now();
    let err = Client::over_command(cmd, SECRET, 1, Duration::from_millis(600))
        .err()
        .expect("no welcome");
    assert!(began.elapsed() < Duration::from_secs(10));
    match err {
        ConnectError::Io(e) => {
            assert_eq!(e.kind(), std::io::ErrorKind::TimedOut);
            assert!(e.to_string().contains("connection stuck"), "{e}");
        }
        other => panic!("expected a timeout, got {other}"),
    }
}

#[test]
fn a_spawn_at_or_below_the_highest_claim_is_refused() {
    let fx = Fixture::new("stale");
    fx.start();
    let mut c = fx.client(1);
    let id = spawn(
        &mut c,
        &spec("conv-s", 5, 1, Mode::Pipes, &shell(sleep_line())),
    );
    // Even after the claim-5 process is killed and acked, claim 4 is still behind what the holder saw.
    kill_until_gone(&mut c, id);
    exit_of(&mut c, id);
    assert_eq!(c.ack(&h(&c, id)).unwrap(), AckState::Acked);
    c.spawn(&spec("conv-s", 4, 1, Mode::Pipes, &shell(sleep_line())))
        .unwrap();
    let (_, _, payload) = c.expect(p::STALE_CLAIM, 0, WAIT).expect("stale claim");
    let stale: p::StaleClaim = wire::parse(&payload).unwrap();
    assert_eq!(
        (stale.conv.as_str(), stale.claim, stale.highest),
        ("conv-s", 4, 5)
    );
    assert!(c.list().unwrap().is_empty());
    // The same claim again, its job having run and been acked, is refused too.
    c.spawn(&spec("conv-s", 5, 1, Mode::Pipes, &shell(sleep_line())))
        .unwrap();
    let (_, _, payload) = c.expect(p::STALE_CLAIM, 0, WAIT).expect("stale claim");
    let stale: p::StaleClaim = wire::parse(&payload).unwrap();
    assert_eq!((stale.claim, stale.highest), (5, 5));
    // A start that failed did not run the job: the same claim may try again.
    let mut no_stub = spec("conv-r", 2, 1, Mode::Pipes, &shell(sleep_line()));
    no_stub.stub = Some("/no/such/kith-stub".into());
    c.spawn(&no_stub).unwrap();
    c.expect(p::SPAWN_FAILED, 0, WAIT).expect("spawn failed");
    let r = spawn(
        &mut c,
        &spec("conv-r", 2, 1, Mode::Pipes, &shell(sleep_line())),
    );
    kill_until_gone(&mut c, r);
    // A higher claim, and another conversation at a low one, still start.
    let a = spawn(
        &mut c,
        &spec("conv-s", 6, 1, Mode::Pipes, &shell(sleep_line())),
    );
    let b = spawn(
        &mut c,
        &spec("conv-o", 1, 1, Mode::Pipes, &shell(sleep_line())),
    );
    kill_until_gone(&mut c, a);
    kill_until_gone(&mut c, b);
}

#[test]
fn an_id_from_another_holder_instance_touches_nothing() {
    let fx = Fixture::new("foreign");
    fx.start();
    let mut c = fx.client(1);
    let line = if cfg!(windows) {
        "echo hello && ping -n 300 127.0.0.1 > nul"
    } else {
        "echo hello; exec sleep 300"
    };
    let id = spawn(&mut c, &spec("conv-i", 1, 1, Mode::Pipes, &shell(line)));
    read_until(&mut c, id, "hello");
    assert_eq!(c.list().unwrap()[0].held_id(), h(&c, id));
    let other = "0".repeat(32);
    assert_ne!(other, c.welcome.instance);
    c.send_for(p::KILL, &other, id, &[]).unwrap();
    let (_, _, payload) = c.expect(p::KILLED, id, WAIT).expect("killed");
    let killed: p::Killed = wire::parse(&payload).unwrap();
    assert_eq!(killed.state, KillState::NotHere);
    c.send_for(p::ACK, &other, id, &[]).unwrap();
    let (_, _, payload) = c.expect(p::ACKED, id, WAIT).expect("acked");
    let acked: p::Acked = wire::parse(&payload).unwrap();
    assert_eq!(acked.state, AckState::NotHere);
    c.send_for(p::TAIL, &other, id, &[]).unwrap();
    let (_, _, tail) = c.expect(p::TAILED, id, WAIT).expect("tailed");
    assert!(tail.is_empty());
    c.send_for(p::RESIZE, &other, id, br#"{"cols":120,"rows":40}"#)
        .unwrap();
    c.send_for(p::INPUT, &other, id, b"").unwrap();
    // The process is still running, unresized, and its tail is still there.
    let held = c.list().unwrap();
    assert_eq!(
        (held.len(), held[0].exit.clone(), held[0].cols),
        (1, None, 80)
    );
    assert!(String::from_utf8_lossy(&c.tail(&h(&c, id)).unwrap()).contains("hello"));
    kill_until_gone(&mut c, id);
}

fn tagged(mut s: p::Spawn) -> p::Spawn {
    s.kill_on_lease_loss = true;
    s
}

/// The listing entry of `id`.
fn entry(c: &mut Client, id: u32) -> p::Held {
    c.list().unwrap().into_iter().find(|e| e.id == id).unwrap()
}

fn ended(c: &mut Client, id: u32) -> bool {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if entry(c, id).exit.is_some() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn at_expiry_the_tagged_run_is_killed_and_the_owner_run_is_kept() {
    let fx = Fixture::new("lease1");
    fx.start();
    let mut c = fx.client(1);
    let owner = spawn(
        &mut c,
        &spec("conv-owner", 1, 1, Mode::Pipes, &shell(sleep_line())),
    );
    // No renewal yet: no lease is armed, nothing expires, and a tagged run may not start without one.
    std::thread::sleep(Duration::from_millis(300));
    assert!(entry(&mut c, owner).exit.is_none());
    let guest = tagged(spec("conv-guest", 1, 1, Mode::Pipes, &shell(sleep_line())));
    c.spawn(&guest).unwrap();
    c.expect(p::LEASE_LOST, 0, WAIT).expect("refused unarmed");
    assert_eq!(c.list().unwrap().len(), 1);
    c.renew(Duration::from_millis(1500)).unwrap();
    let guest = spawn(&mut c, &guest);
    assert!(ended(&mut c, guest), "the tagged run outlived its lease");
    // Its exit is kept until acked, like any exit; the owner run is untouched.
    let exit = entry(&mut c, guest).exit.expect("exit kept");
    assert!(exit.signal.is_some() || exit.code != Some(0), "{exit:?}");
    assert!(entry(&mut c, owner).exit.is_none());
    assert_eq!(c.ack(&h(&c, guest)).unwrap(), AckState::Acked);
    kill_until_gone(&mut c, owner);
}

#[test]
fn a_link_that_drops_and_never_returns_runs_out_the_lease() {
    let fx = Fixture::new("lease5");
    fx.start();
    let mut c = fx.client(1);
    c.renew(Duration::from_millis(1500)).unwrap();
    let guest = spawn(
        &mut c,
        &tagged(spec("conv-guest", 1, 1, Mode::Pipes, &shell(sleep_line()))),
    );
    let owner = spawn(
        &mut c,
        &spec("conv-owner", 1, 1, Mode::Pipes, &shell(sleep_line())),
    );
    drop(c);
    // The same core (same generation) comes back after the lease ran out: the tagged run is dead, the owner's kept.
    std::thread::sleep(Duration::from_millis(2500));
    let mut c = fx.client(1);
    assert!(entry(&mut c, guest).exit.is_some());
    assert!(entry(&mut c, owner).exit.is_none());
    kill_until_gone(&mut c, owner);
}

#[test]
fn a_huge_renewal_is_capped_and_does_not_panic_the_connection() {
    let fx = Fixture::new("lease6");
    fx.start();
    let mut c = fx.client(1);
    c.renew(Duration::from_millis(u64::MAX)).unwrap();
    let guest = spawn(
        &mut c,
        &tagged(spec("conv-guest", 1, 1, Mode::Pipes, &shell(sleep_line()))),
    );
    // Still answering, and the tagged run holds a (capped) lease.
    assert!(entry(&mut c, guest).exit.is_none());
    kill_until_gone(&mut c, guest);
}

#[test]
fn renewals_keep_a_tagged_run_alive() {
    let fx = Fixture::new("lease2");
    fx.start();
    let mut c = fx.client(1);
    c.renew(Duration::from_secs(3)).unwrap();
    let guest = spawn(
        &mut c,
        &tagged(spec("conv-guest", 1, 1, Mode::Pipes, &shell(sleep_line()))),
    );
    let until = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < until {
        c.renew(Duration::from_secs(3)).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(entry(&mut c, guest).exit.is_none());
    }
    kill_until_gone(&mut c, guest);
}

#[test]
fn while_the_lease_is_lost_a_tagged_spawn_is_refused_and_a_renewal_restores_it() {
    let fx = Fixture::new("lease3");
    fx.start();
    let mut c = fx.client(1);
    c.renew(Duration::from_millis(200)).unwrap();
    std::thread::sleep(Duration::from_millis(600));
    let guest = tagged(spec("conv-guest", 1, 1, Mode::Pipes, &shell(sleep_line())));
    c.spawn(&guest).unwrap();
    let (_, _, payload) = c.expect(p::LEASE_LOST, 0, WAIT).expect("lease lost answer");
    let lost: p::LeaseLost = wire::parse(&payload).unwrap();
    assert_eq!((lost.conv.as_str(), lost.claim), ("conv-guest", 1));
    assert!(c.list().unwrap().is_empty());
    // An owner run still starts.
    let owner = spawn(
        &mut c,
        &spec("conv-owner", 1, 1, Mode::Pipes, &shell(sleep_line())),
    );
    // A renewal restores the lease, and the refused claim was not used up.
    c.renew(Duration::from_secs(60)).unwrap();
    let guest_id = spawn(&mut c, &guest);
    std::thread::sleep(Duration::from_millis(300));
    assert!(entry(&mut c, guest_id).exit.is_none());
    kill_until_gone(&mut c, guest_id);
    kill_until_gone(&mut c, owner);
}

#[test]
fn an_older_cores_renewals_do_not_keep_the_lease() {
    let fx = Fixture::new("lease4");
    fx.start();
    let mut old = fx.client(1);
    old.renew(Duration::from_secs(600)).unwrap();
    let guest = spawn(
        &mut old,
        &tagged(spec("conv-guest", 1, 1, Mode::Pipes, &shell(sleep_line()))),
    );
    let mut new = fx.client(2);
    assert!(old.closed(WAIT));
    new.renew(Duration::from_millis(400)).unwrap();
    // The superseded core keeps sending long renewals; none counts.
    let _ = old.renew(Duration::from_secs(600));
    assert!(
        ended(&mut new, guest),
        "an old core's renewal kept the lease"
    );
}

// --- The warm engine's writer lock, through its resident stub (M3-4) ---------------------------------------------

/// Whether another writer holds conversation `key` now, asked the way a `--direct` run asks: a one-shot stub. Exit 75
/// is busy; a free lock is taken and given back as the probe ends.
fn lock_busy(key: &str) -> bool {
    let (stub, locks) = stub_and_locks();
    let status = Command::new(stub)
        .args(["--lock-dir", &locks, "--key", key, "--"])
        .args(shell("exit 0"))
        .status()
        .unwrap();
    status.code() == Some(75)
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + WAIT;
    while !f() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn warm(c: &mut Client, conv: &str, claim: u64) -> p::HeldId {
    let id = spawn(c, &spec(conv, claim, 1, Mode::Pipes, &shell(sleep_line())));
    h(c, id)
}

fn word(a: &p::LockAnswer) -> (p::LockOp, u64, p::LockWord) {
    (a.op, a.claim, a.word)
}

fn kill_pid(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .output();
    }
}

#[test]
fn a_turns_lock_is_taken_and_released_through_the_stub() {
    use p::LockOp::{Release, Take};
    use p::LockWord::{Busy, Error, Locked, NotHeld, Released};
    let fx = Fixture::new("lk1");
    fx.start();
    let mut c = fx.client(1);
    let held = warm(&mut c, "lock-a", 1);
    // An idle warm engine holds nothing: the lock is per turn.
    assert!(!lock_busy("lock-a"));
    assert_eq!(
        word(&c.take_lock(&held, "lock-a", 1).unwrap()),
        (Take, 1, Locked)
    );
    assert!(lock_busy("lock-a"));
    // A take retried after a lost answer is the same answer.
    assert_eq!(
        word(&c.take_lock(&held, "lock-a", 1).unwrap()),
        (Take, 1, Locked)
    );
    // Another claim while claim 1 holds it: busy, and claim 1's lock is still there to release.
    assert_eq!(
        word(&c.take_lock(&held, "lock-a", 2).unwrap()),
        (Take, 2, Busy)
    );
    assert_eq!(
        word(&c.release_lock(&held, "lock-a", 2).unwrap()),
        (Release, 2, NotHeld)
    );
    assert_eq!(
        word(&c.release_lock(&held, "lock-a", 3).unwrap()),
        (Release, 3, NotHeld)
    );
    assert!(lock_busy("lock-a"), "a wrong claim released nothing");
    assert_eq!(
        word(&c.release_lock(&held, "lock-a", 1).unwrap()),
        (Release, 1, Released)
    );
    assert!(!lock_busy("lock-a"));
    // A release retried is not-held, and a claim that was released is never taken again.
    assert_eq!(
        word(&c.release_lock(&held, "lock-a", 1).unwrap()),
        (Release, 1, NotHeld)
    );
    let again = c.take_lock(&held, "lock-a", 1).unwrap();
    assert_eq!(
        (word(&again), again.reason.as_deref()),
        ((Take, 1, Error), Some("claim-released"))
    );
    assert!(!lock_busy("lock-a"));
    // The next claim takes it, and releases it.
    assert_eq!(
        word(&c.take_lock(&held, "lock-a", 2).unwrap()),
        (Take, 2, Locked)
    );
    assert!(lock_busy("lock-a"));
    assert_eq!(
        word(&c.release_lock(&held, "lock-a", 2).unwrap()),
        (Release, 2, Released)
    );
    assert!(!lock_busy("lock-a"));
    kill_until_gone(&mut c, held.id);
}

#[test]
fn a_claim_older_than_the_current_one_is_refused_and_a_key_cannot_carry_a_second_command() {
    use p::LockWord::{Error, Locked};
    let fx = Fixture::new("lk2");
    fx.start();
    let mut c = fx.client(1);
    let held = warm(&mut c, "lock-b", 5);
    let old = c.take_lock(&held, "lock-b", 4).unwrap();
    assert_eq!(
        (old.word, old.claim, old.reason.as_deref()),
        (Error, 4, Some("stale-claim"))
    );
    assert!(!lock_busy("lock-b"));
    // A line break or a space in the key would be a second command on the stub's channel.
    let bad = c.take_lock(&held, "lock-b\ntake lock-evil 5", 5).unwrap();
    assert_eq!((bad.word, bad.reason.as_deref()), (Error, Some("bad-key")));
    let bad = c.take_lock(&held, "lock-b lock-evil", 5).unwrap();
    assert_eq!((bad.word, bad.reason.as_deref()), (Error, Some("bad-key")));
    assert!(!lock_busy("lock-evil") && !lock_busy("lock-b"));
    assert_eq!(c.take_lock(&held, "lock-b", 5).unwrap().word, Locked);
    // A process of another holder instance is not ours to lock for.
    let foreign = p::HeldId {
        instance: "f".repeat(32),
        id: held.id,
    };
    let a = c.take_lock(&foreign, "lock-b", 6).unwrap();
    assert_eq!((a.word, a.reason.as_deref()), (Error, Some("not-here")));
    kill_until_gone(&mut c, held.id);
}

#[test]
fn answers_are_kept_per_claim_until_acked() {
    use p::LockOp::{Release, Take};
    use p::LockWord::{Busy, Locked, Released};
    let fx = Fixture::new("lk3");
    fx.start();
    let mut c = fx.client(1);
    let held = warm(&mut c, "lock-c", 1);
    c.take_lock(&held, "lock-c", 1).unwrap();
    c.take_lock(&held, "lock-c", 2).unwrap();
    c.release_lock(&held, "lock-c", 1).unwrap();
    let kept = |c: &mut Client| -> Vec<(u64, p::LockOp, p::LockWord)> {
        let mut locks = c.list().unwrap()[0].locks.clone();
        locks.sort_by_key(|a| (a.claim, a.op));
        locks.iter().map(|a| (a.claim, a.op, a.word)).collect()
    };
    // The busy for claim 2 did not replace claim 1's answers, and the release sits beside the take.
    assert_eq!(
        kept(&mut c),
        vec![(1, Take, Locked), (1, Release, Released), (2, Take, Busy)]
    );
    c.ack_lock(&held, 1).unwrap();
    wait_for("the ack", || kept(&mut c) == vec![(2, Take, Busy)]);
    kill_until_gone(&mut c, held.id);
}

#[test]
fn a_stub_killed_mid_turn_frees_the_lock_and_later_asks_fail() {
    use p::LockWord::{Error, Locked, NotHeld};
    let fx = Fixture::new("lk4");
    fx.start();
    let mut c = fx.client(1);
    let held = warm(&mut c, "lock-d", 1);
    assert_eq!(c.take_lock(&held, "lock-d", 1).unwrap().word, Locked);
    assert!(lock_busy("lock-d"));
    let pid = c.list().unwrap()[0].pid;
    kill_pid(pid);
    // The OS released the lock with the stub, whatever the engine does.
    wait_for("the lock to free", || !lock_busy("lock-d"));
    let (_, _, payload) = c
        .expect(p::EXITED, held.id, WAIT)
        .expect("the exit is reported");
    let _: p::Exit = wire::parse(&payload).unwrap();
    assert_eq!(c.take_lock(&held, "lock-d", 2).unwrap().word, Error);
    assert_eq!(c.release_lock(&held, "lock-d", 1).unwrap().word, NotHeld);
    assert_eq!(c.ack(&held).unwrap(), AckState::Acked);
}

#[cfg(unix)]
#[test]
fn a_holder_killed_mid_turn_does_not_free_the_lock_while_the_stub_lives() {
    let fx = Fixture::new("lk5");
    fx.start();
    let mut c = fx.client(1);
    let held = warm(&mut c, "lock-e", 1);
    assert_eq!(
        c.take_lock(&held, "lock-e", 1).unwrap().word,
        p::LockWord::Locked
    );
    let (holder, stub) = (c.welcome.pid as i32, c.list().unwrap()[0].pid as i32);
    unsafe { libc::kill(holder, libc::SIGKILL) };
    wait_for(
        "the holder to die",
        || unsafe { libc::kill(holder, 0) } != 0,
    );
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        lock_busy("lock-e"),
        "the stub keeps the lock without its holder"
    );
    // Ending the stub's group (what the next holder instance's core does) frees it.
    unsafe { libc::killpg(stub, libc::SIGKILL) };
    wait_for("the lock to free", || !lock_busy("lock-e"));
}

#[cfg(unix)]
#[test]
fn the_engine_does_not_inherit_the_control_channel() {
    let fx = Fixture::new("lk6");
    fx.start();
    let mut c = fx.client(1);
    // fd 9 is a control: the check can see an open descriptor at all. Nothing else from 3 up may be open.
    let line = "exec 9</dev/null; for n in 3 4 5 6 7 8 9 10 11 12 13 14 15 16; do \
                [ -e /dev/fd/$n ] && echo open-$n; done; echo checked; exec sleep 300";
    let id = spawn(&mut c, &spec("lock-f", 1, 1, Mode::Pipes, &shell(line)));
    let seen = read_until(&mut c, id, "checked");
    let open: Vec<&str> = seen.lines().filter(|l| l.starts_with("open-")).collect();
    assert_eq!(open, vec!["open-9"], "{seen}");
    // The stub does hold a channel, though: a take works.
    assert_eq!(
        c.take_lock(&h(&c, id), "lock-f", 1).unwrap().word,
        p::LockWord::Locked
    );
    kill_until_gone(&mut c, id);
}

#[cfg(unix)]
#[test]
fn closing_the_engines_stdin_ends_a_warm_engine_through_the_stub() {
    let fx = Fixture::new("lk7");
    fx.start();
    let mut c = fx.client(1);
    let id = spawn(&mut c, &spec("lock-g", 1, 1, Mode::Pipes, &shell("cat")));
    let held = h(&c, id);
    assert_eq!(
        c.take_lock(&held, "lock-g", 1).unwrap().word,
        p::LockWord::Locked
    );
    c.send_for(p::INPUT, &held.instance, id, &[]).unwrap();
    let exit = exit_of(&mut c, id);
    assert_eq!(exit.code, Some(0));
    assert!(!lock_busy("lock-g"), "the lock went with the stub");
}

#[test]
fn a_missing_or_absent_stub_fails_the_spawn_with_an_install_hint() {
    let fx = Fixture::new("lk8");
    fx.start();
    let mut c = fx.client(1);
    let mut missing = spec("lock-h", 1, 1, Mode::Pipes, &shell(sleep_line()));
    missing.stub = Some(if cfg!(windows) {
        r"C:\no\such\kith-stub.exe".into()
    } else {
        "/no/such/kith-stub".into()
    });
    let mut none = spec("lock-h", 1, 1, Mode::Pipes, &shell(sleep_line()));
    none.stub = None;
    for bad in [missing, none] {
        c.spawn(&bad).unwrap();
        let (_, _, payload) = c.expect(p::SPAWN_FAILED, 0, WAIT).expect("spawn failed");
        let failed: p::SpawnFailed = wire::parse(&payload).unwrap();
        assert_eq!((failed.conv.as_str(), failed.claim), ("lock-h", 1));
        assert!(
            failed.error.contains("kith-stub") && failed.error.contains("Install"),
            "{}",
            failed.error
        );
    }
    assert!(
        c.list().unwrap().is_empty(),
        "no engine ran without its stub"
    );
    // The claim was not used up: the same one starts with a stub.
    let held = warm(&mut c, "lock-h", 1);
    kill_until_gone(&mut c, held.id);
}

#[test]
fn a_core_of_the_older_protocol_is_refused_at_hello() {
    let fx = Fixture::new("lk9");
    fx.start();
    let ep = kith_holder::platform::endpoint(&fx.dir);
    let mut raw = kith_holder::platform::connect(&ep).unwrap();
    let hello = p::Hello {
        version: p::VERSION - 1,
        generation: 1,
        nonce: "aa".repeat(16),
    };
    wire::write_json(&mut raw, p::HELLO, 0, &hello).unwrap();
    let (kind, _, payload) = wire::read(&mut raw).unwrap().unwrap();
    assert_eq!(kind, p::REFUSED);
    let refused: p::Refused = wire::parse(&payload).unwrap();
    assert_eq!(refused.why, Why::BadVersion);
}
