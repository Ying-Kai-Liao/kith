//! The daemon front on real sockets, in this process: the owner socket, its hello and token, the lock against embedded
//! commands, the holder link when there is no holder or a bad one. (A real daemon process and a real `kith-holder` are
//! in `serve/host/tests/core_daemon_e2e.rs`.)

use std::time::{Duration, Instant};

use kith_kernel::paths::Paths;
use serde_json::{Value, json};

use super::{Config, Daemon, Reached};
use crate::Error;
use crate::holder_link::HolderConfig;
use crate::layout::Layout;
use crate::tests::{machine, open, paths};

fn quiet() -> Config {
    Config {
        holder_on: false,
        wait_for_embedded: Duration::from_secs(10),
        ..Config::default()
    }
}

fn serving(p: &Paths, cfg: Config) -> Daemon {
    let d = Daemon::spawn_with(p, cfg);
    d.ready(Duration::from_secs(10)).expect("the daemon serves");
    d
}

fn read(p: &Paths, method: &str, params: &Value) -> Reached {
    crate::reach(p, method, params, true)
}

fn until(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("gave up waiting for {what}");
}

#[test]
fn a_daemon_serves_the_table_over_the_owner_socket_and_leaves_when_stopped() {
    let (_t, m) = machine();
    let p = paths(&m);
    let d = serving(&p, quiet());
    match read(&p, "kernel.version", &json!({})) {
        Reached::Answered(v) => assert_eq!(v["abi"], 1),
        other => panic!("{other:?}"),
    }
    match read(&p, "paths", &json!({})) {
        Reached::Answered(v) => assert_eq!(v["KITH_MACHINE"], m.to_string_lossy().as_ref()),
        other => panic!("{other:?}"),
    }
    // A refusal about the call is an answer too, and nothing is run for it.
    assert!(matches!(
        read(&p, "agents.list", &json!({})),
        Reached::Refused(Error::NotServed(_))
    ));
    assert!(matches!(
        read(&p, "no.such", &json!({})),
        Reached::Refused(Error::NoSuchMethod(_))
    ));
    assert!(matches!(
        read(&p, "kernel.version", &json!({"x": 1})),
        Reached::Refused(Error::BadParams(_))
    ));
    // Two answers; the three refusals are not counted as served.
    assert_eq!(d.served(), 2);
    drop(d);
    assert!(matches!(
        read(&p, "kernel.version", &json!({})),
        Reached::NoDaemon
    ));
    #[cfg(unix)]
    assert!(
        !crate::owner::endpoint(&Layout::new(&m)).exists(),
        "the socket goes with the daemon"
    );
}

#[test]
fn a_verb_that_depends_on_the_environment_answers_for_the_caller_not_the_daemon() {
    let (_t, m) = machine();
    let p = paths(&m);
    let _d = serving(&p, quiet());
    let view = json!({"home": "/nowhere/home", "kernel": "/k/kernel", "env": {"KITH_HOME": "/caller/kith"}});
    match read(&p, "paths", &view) {
        Reached::Answered(v) => {
            assert_eq!(
                std::path::Path::new(v["KITH_HOME"].as_str().expect("home")),
                std::path::Path::new("/caller/kith")
            );
            assert_ne!(v["KITH_MACHINE"], m.to_string_lossy().as_ref());
        }
        other => panic!("{other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn the_socket_the_token_and_the_folder_are_owner_only() {
    use std::os::unix::fs::PermissionsExt as _;
    let (_t, m) = machine();
    let p = paths(&m);
    let l = Layout::new(&m);
    let d = serving(&p, quiet());
    let mode = |path: &std::path::Path| {
        std::fs::metadata(path)
            .expect("exists")
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode(&l.dir), 0o700);
    assert_eq!(mode(&crate::owner::endpoint(&l)), 0o600);
    assert_eq!(mode(&l.token), 0o600);
    // And the daemon refuses to run on a folder that is open to others (the same rule as core.db).
    drop(d);
    std::fs::set_permissions(&l.dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let d = Daemon::spawn_with(&p, quiet());
    assert!(matches!(
        d.ready(Duration::from_secs(10)),
        Err(Error::Permissions(_))
    ));
}

/// Lines of JSON over the owner socket, by hand, to show what the daemon does with a client that does not follow the
/// protocol.
#[cfg(unix)]
struct Raw {
    writer: kith_holder::platform::Stream,
    reader: std::io::BufReader<kith_holder::platform::Stream>,
}

#[cfg(unix)]
impl Raw {
    fn connect(m: &std::path::Path) -> Raw {
        let s = kith_holder::platform::connect(&crate::owner::endpoint(&Layout::new(m)))
            .expect("connect");
        s.set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeout");
        Raw {
            writer: s.try_clone().expect("clone"),
            reader: std::io::BufReader::new(s),
        }
    }
    fn send(&mut self, id: u64, method: &str, params: &Value) {
        use std::io::Write as _;
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.writer, "{line}").expect("write");
    }
    fn recv(&mut self) -> Option<Value> {
        use std::io::BufRead as _;
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(n) if n > 0 => serde_json::from_str(&line).ok(),
            _ => None,
        }
    }
}

#[cfg(unix)]
#[test]
fn a_client_that_skips_the_handshake_or_gets_the_token_wrong_is_refused_and_runs_nothing() {
    let (_t, m) = machine();
    let p = paths(&m);
    let d = serving(&p, quiet());

    // A call with no hello.
    let mut c = Raw::connect(&m);
    c.send(1, "kernel.version", &json!({}));
    assert_eq!(
        c.recv().expect("an error")["error"]["data"]["kind"],
        "protocol"
    );
    assert!(c.recv().is_none(), "and the connection is closed");

    // Hello, then a proof made with the wrong token.
    let mut c = Raw::connect(&m);
    let schema = kith_api::schema_hash();
    c.send(1, "hello", &json!({"protocol": crate::owner::PROTOCOL, "version": "x", "schema": schema, "nonce": "aa"}));
    let hello = c.recv().expect("hello");
    assert_eq!(hello["result"]["compatible"], true);
    c.send(2, "auth", &json!({"proof": "00".repeat(32)}));
    assert_eq!(
        c.recv().expect("refusal")["error"]["data"]["kind"],
        "unauthorized"
    );
    assert!(c.recv().is_none());

    // Hello, then the right proof: the protocol as written, by hand.
    let token = std::fs::read_to_string(Layout::new(&m).token).expect("token");
    let mut c = Raw::connect(&m);
    c.send(1, "hello", &json!({"protocol": crate::owner::PROTOCOL, "version": "x", "schema": schema, "nonce": "bb"}));
    let hello = c.recv().expect("hello");
    let server_nonce = hello["result"]["nonce"].as_str().expect("nonce").to_owned();
    // The daemon proved the token first.
    let theirs =
        kith_holder::auth::proof(token.trim().as_bytes(), "daemon", "bb", &server_nonce, 0);
    assert_eq!(hello["result"]["proof"], theirs.as_str());
    let mine = kith_holder::auth::proof(token.trim().as_bytes(), "cli", "bb", &server_nonce, 0);
    c.send(2, "auth", &json!({"proof": mine}));
    assert_eq!(c.recv().expect("ok")["result"]["ok"], true);
    c.send(3, "kernel.version", &json!({}));
    assert_eq!(c.recv().expect("answer")["result"]["abi"], 1);
    assert_eq!(
        d.served(),
        1,
        "only the call that went through the handshake ran"
    );
}

#[test]
fn a_token_that_is_not_the_daemons_is_not_a_daemon_to_talk_to() {
    let (_t, m) = machine();
    let p = paths(&m);
    let d = serving(&p, quiet());
    // The client reads the token from the file; with another one it cannot prove the daemon is the one that wrote it.
    std::fs::write(Layout::new(&m).token, "not-the-daemons-token").expect("write");
    assert!(matches!(
        read(&p, "kernel.version", &json!({})),
        Reached::NoDaemon
    ));
    assert_eq!(d.served(), 0);
}

#[test]
fn a_protocol_or_method_table_that_differs_falls_back_with_a_note_and_runs_nothing() {
    let (_t, m) = machine();
    let p = paths(&m);
    let d = serving(&p, quiet());
    let l = Layout::new(&m);
    for identity in [
        (crate::owner::PROTOCOL + 1, kith_api::schema_hash()),
        (crate::owner::PROTOCOL, "0000000000000000"),
    ] {
        match crate::owner::reach_as(&l, "kernel.version", &json!({}), true, identity) {
            Reached::Mismatch(note) => {
                assert!(note.contains("disagree"), "{note}");
                assert!(
                    note.contains(&format!("pid {}", std::process::id())),
                    "{note}"
                );
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(d.served(), 0);
    // The honest client still gets through.
    assert!(matches!(
        read(&p, "kernel.version", &json!({})),
        Reached::Answered(_)
    ));
}

#[test]
fn a_restart_takes_a_newer_generation_and_a_second_daemon_is_refused() {
    let (_t, m) = machine();
    let p = paths(&m);
    let generation = |p: &Paths| {
        crate::status(p)["daemon"]["generation"]
            .as_i64()
            .expect("generation")
    };
    let first = serving(&p, quiet());
    let g1 = generation(&p);
    // One lock: the second daemon finds the first one's name in core.holder and refuses at once.
    let second = Daemon::spawn_with(&p, quiet());
    let started = Instant::now();
    match second.ready(Duration::from_secs(5)) {
        Err(Error::AlreadyRunning(m)) => assert!(m.contains("holds the lock"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "refused cleanly, not after a wait"
    );
    assert_eq!(generation(&p), g1, "and the first one is untouched");
    drop(first);
    let again = serving(&p, quiet());
    assert!(generation(&p) > g1, "a restart is a newer core");
    drop(again);
}

#[test]
fn the_daemon_waits_out_an_embedded_command_and_an_embedded_call_defers_to_the_daemon() {
    let (_t, m) = machine();
    let p = paths(&m);
    let l = Layout::new(&m);
    let embedded = open(&l);
    let d = Daemon::spawn_with(&p, quiet());
    assert!(
        d.ready(Duration::from_millis(400)).is_err(),
        "the daemon is waiting for the command, not failing and not taking the lock"
    );
    assert!(matches!(
        read(&p, "kernel.version", &json!({})),
        Reached::NoDaemon
    ));
    drop(embedded);
    d.ready(Duration::from_secs(10))
        .expect("it takes over once the command is done");
    let log = std::fs::read_to_string(m.join(crate::log::FILE)).expect("core.log");
    assert!(log.contains("daemon: a command"), "{log}");

    // While the daemon holds the lock an embedded open finds it, and the call goes to the socket.
    assert!(matches!(
        crate::embedded_with(&l, crate::db::MIGRATIONS),
        Ok(crate::Embedded::DaemonRunning { .. })
    ));
    assert!(matches!(
        read(&p, "kernel.version", &json!({})),
        Reached::Answered(_)
    ));
}

#[test]
fn a_daemon_that_gives_up_waiting_says_who_held_the_core() {
    let (_t, m) = machine();
    let p = paths(&m);
    let _embedded = open(&Layout::new(&m));
    let d = Daemon::spawn_with(
        &p,
        Config {
            wait_for_embedded: Duration::from_millis(200),
            ..quiet()
        },
    );
    assert!(matches!(
        d.ready(Duration::from_secs(5)),
        Err(Error::AlreadyRunning(_))
    ));
}

#[cfg(unix)]
#[test]
fn a_socket_file_left_by_a_dead_daemon_is_replaced_by_the_next_one() {
    let (_t, m) = machine();
    let p = paths(&m);
    let l = Layout::new(&m);
    crate::perms::ensure_dir(&l.dir).expect("dir");
    {
        // A bound and abandoned socket: the file stays, nothing listens.
        let _gone =
            std::os::unix::net::UnixListener::bind(crate::owner::endpoint(&l)).expect("bind");
    }
    assert!(crate::owner::endpoint(&l).exists());
    assert!(matches!(
        read(&p, "kernel.version", &json!({})),
        Reached::NoDaemon
    ));
    let _d = serving(&p, quiet());
    assert!(matches!(
        read(&p, "kernel.version", &json!({})),
        Reached::Answered(_)
    ));
}

// --- the holder link -----------------------------------------------------------------------------------------------

fn holder_cfg(exe: std::path::PathBuf) -> Config {
    Config {
        holder_on: true,
        wait_for_embedded: Duration::from_secs(10),
        holder: HolderConfig {
            exe: Some(exe),
            idle: Duration::from_secs(5),
            poll: Duration::from_millis(100),
            retry_min: Duration::from_millis(50),
            retry_max: Duration::from_millis(200),
            kill_wait: Duration::from_secs(5),
            command: None,
        },
    }
}

/// A daemon whose holder link is a command's stdio (the `ssh host kith-holder --stdio` of M0f).
fn command_cfg(argv: &[&str]) -> Config {
    let mut cfg = holder_cfg(std::path::PathBuf::new());
    cfg.holder.exe = None;
    cfg.holder.command = Some(argv.iter().map(|a| (*a).to_owned()).collect());
    cfg
}

fn holder_state(p: &Paths) -> String {
    crate::status(p)["holder"]["state"]
        .as_str()
        .unwrap_or("none")
        .to_owned()
}

#[test]
fn no_holder_program_stops_nothing() {
    let (_t, m) = machine();
    let p = paths(&m);
    let d = serving(&p, holder_cfg(m.join("no-such-kith-holder")));
    until("the holder to be reported absent", || {
        holder_state(&p) == "absent"
    });
    // The daemon serves, the CLI is answered, and the core log says why there is no holder, once.
    assert!(matches!(
        read(&p, "kernel.version", &json!({})),
        Reached::Answered(_)
    ));
    let status = crate::status(&p);
    assert!(
        status["holder"]["error"]
            .as_str()
            .expect("error")
            .contains("no kith-holder")
    );
    let log = std::fs::read_to_string(m.join(crate::log::FILE)).expect("core.log");
    assert_eq!(log.matches("holder: no kith-holder").count(), 1, "{log}");
    drop(d);
}

#[cfg(unix)]
#[test]
fn a_stdio_link_that_cannot_run_or_speaks_garbage_stops_nothing() {
    for argv in [
        vec!["/no/such/ssh", "host", "kith-holder", "--stdio"],
        // Anything on stdout that is not a proved holder is a failed connection, not an error for the daemon.
        vec!["/bin/sh", "-c", "echo not a holder; sleep 5"],
    ] {
        let (_t, m) = machine();
        let p = paths(&m);
        let d = serving(&p, command_cfg(&argv));
        until("the stdio link to be reported unreachable", || {
            holder_state(&p) == "unreachable"
        });
        // The daemon still answers its owner.
        assert!(crate::status(&p)["holder"]["error"].is_string());
        drop(d);
    }
}

#[test]
fn a_holder_program_that_will_not_start_stops_nothing() {
    let (_t, m) = machine();
    let p = paths(&m);
    // There, but not a program (no execute bit): starting it fails at once.
    let exe = m.join("kith-holder");
    std::fs::create_dir_all(&m).expect("machine");
    std::fs::write(&exe, "not a program").expect("write");
    let _d = serving(&p, holder_cfg(exe));
    until("the holder to be reported unreachable", || {
        holder_state(&p) == "unreachable"
    });
    assert!(matches!(
        read(&p, "kernel.version", &json!({})),
        Reached::Answered(_)
    ));
    let error = crate::status(&p)["holder"]["error"]
        .as_str()
        .expect("error")
        .to_owned();
    assert!(error.contains("can't start"), "{error}");
}

/// A fake holder on the real socket path: `answer` is what it does with each connection.
#[cfg(unix)]
fn fake_holder(
    m: &std::path::Path,
    answer: fn(kith_holder::platform::Stream),
) -> std::path::PathBuf {
    let l = Layout::new(m);
    crate::perms::ensure_dir(&l.dir).expect("core dir");
    std::fs::create_dir_all(&l.holder_dir).expect("holder dir");
    let ep = kith_holder::platform::endpoint(&l.holder_dir);
    let listener = std::os::unix::net::UnixListener::bind(&ep).expect("bind");
    std::thread::spawn(move || {
        while let Ok((s, _)) = listener.accept() {
            std::thread::spawn(move || answer(s));
        }
    });
    std::env::current_exe().expect("exe")
}

#[cfg(unix)]
#[test]
fn a_holder_that_answers_garbage_or_a_refusal_stops_nothing_and_proves_nothing() {
    use kith_holder::{proto, wire};
    fn garbage(mut s: kith_holder::platform::Stream) {
        use std::io::Write as _;
        let _ = s.write_all(b"\xff\xfe this is not a frame, not even close to one \x00\x01");
    }
    fn refusal(mut s: kith_holder::platform::Stream) {
        let _ = wire::read(&mut s);
        // A fake holder's easiest lie: a newer generation, before it has proved anything.
        let _ = wire::write_json(
            &mut s,
            proto::REFUSED,
            0,
            &proto::Refused {
                why: proto::Why::StaleGeneration,
                current: 999_999,
            },
        );
    }
    for answer in [garbage as fn(_), refusal as fn(_)] {
        let (_t, m) = machine();
        let p = paths(&m);
        let exe = fake_holder(&m, answer);
        let d = serving(&p, holder_cfg(exe));
        let before = crate::status(&p)["daemon"]["generation"]
            .as_i64()
            .expect("generation");
        until("the holder to be reported unreachable", || {
            holder_state(&p) == "unreachable"
        });
        // The daemon still serves, at the generation core.db gave it: a refusal is a failed connection, not news.
        assert!(matches!(
            read(&p, "kernel.version", &json!({})),
            Reached::Answered(_)
        ));
        assert_eq!(
            crate::status(&p)["daemon"]["generation"].as_i64(),
            Some(before)
        );
        let claims_still_open = d.shared.with_core(|c| c.claim_tx().map(|_| ()));
        assert!(claims_still_open.is_ok(), "{claims_still_open:?}");
    }
}

// --- step 7: a holder that is not the one the core last knew --------------------------------------------------------

#[cfg(unix)]
fn leader() -> (u32, String) {
    // Its own session, so its pid is its group's: what a held process is. A thread reaps it when it ends (a real
    // orphan is reaped by init; this one is our child, and a zombie still counts as a live group).
    let mut child = std::process::Command::new("python3")
        .args(["-c", "import os,time; os.setsid(); time.sleep(120)"])
        .spawn()
        .expect("python3");
    let pid = child.id();
    std::thread::spawn(move || child.wait());
    let mut started = None;
    until("the process to have a start time", || {
        started = crate::lock::process_start(pid);
        started.is_some()
    });
    // Let the setsid happen before anything signals the group.
    std::thread::sleep(Duration::from_millis(300));
    (pid, started.expect("start time"))
}

#[cfg(unix)]
fn signal(pid: u32, sig: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-s", sig, &pid.to_string()])
        .output()
        .is_ok_and(|o| o.status.success())
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    signal(pid, "0")
}

#[cfg(unix)]
#[test]
fn a_new_holder_instance_ends_what_the_old_one_held_matched_by_pid_and_start_time() {
    use crate::db::{HolderRecord, RecordedProc};
    let (_t, m) = machine();
    let p = paths(&m);
    let d = serving(&p, quiet());
    let cfg = HolderConfig {
        kill_wait: Duration::from_secs(10),
        ..HolderConfig::default()
    };
    let (held_pid, held_start) = leader();
    let (stranger_pid, _) = leader();
    let record = |instance: Option<&str>, procs: Vec<RecordedProc>| {
        let rec = HolderRecord {
            instance: instance.map(str::to_owned),
            procs,
        };
        d.shared
            .with_core(|c| c.save_holder_record(&rec))
            .expect("record");
    };
    let proc = |pid, started: &str| RecordedProc {
        pid,
        started: started.to_owned(),
        conv: "c".into(),
    };
    let recorded = || d.shared.with_core(|c| c.holder_record()).expect("record");

    // 1. A core.db from M0a records no instance: the first hello records it and ends nothing.
    crate::holder_link::reconcile(&d.shared, "one", &[], &cfg).expect("first hello");
    assert_eq!(recorded().instance.as_deref(), Some("one"));
    assert!(alive(held_pid) && alive(stranger_pid));

    // 2. The same instance again: nothing to do.
    crate::holder_link::reconcile(&d.shared, "one", &[], &cfg).expect("same");
    assert!(alive(held_pid));

    // 3. The old instance reported a process that is still its own (same pid, same start time) and one whose pid has
    //    since been handed to another process (the recorded start time is not that process's). A new instance appears.
    record(
        Some("one"),
        vec![
            proc(held_pid, &held_start),
            proc(stranger_pid, "Thu Jan  1 00:00:00 1970"),
        ],
    );
    crate::holder_link::reconcile(&d.shared, "two", &[], &cfg).expect("takeover");
    assert!(
        !alive(held_pid),
        "confirmed gone before the new instance is recorded"
    );
    assert!(alive(stranger_pid), "a reused pid is never hit");
    assert_eq!(recorded().instance.as_deref(), Some("two"));

    // 4. Processes the new instance lists itself are not the old one's, whatever the record says.
    record(
        Some("two"),
        vec![proc(stranger_pid, "Thu Jan  1 00:00:00 1970")],
    );
    crate::holder_link::reconcile(&d.shared, "three", &[proc(stranger_pid, "x")], &cfg)
        .expect("listed");
    assert!(alive(stranger_pid));
    signal(stranger_pid, "KILL");
}
