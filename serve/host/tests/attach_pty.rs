//! `kith kernel attach` on a real pty against a fake relay: the terminal is raw while attached and cooked again after every
//! way out (the relay's EXIT, the detach key, SIGTERM to the client), and keys / resizes reach the relay.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

const ATTACH: u8 = 1;
const INPUT: u8 = 2;
const RESIZE: u8 = 3;
const DETACH: u8 = 4;
const ATTACHED: u8 = 101;
const OUTPUT: u8 = 102;
const EXIT: u8 = 103;

struct Run {
    master: Box<dyn MasterPty + Send>,
    // Keeps the slave side open so the pty's settings outlive the client.
    _slave: Box<dyn portable_pty::SlavePty + Send>,
    child: Box<dyn Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    relay: UnixStream,
    shown: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    dir: PathBuf,
}

impl Drop for Run {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(15);
    while Instant::now() < end {
        if ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {what}");
}

fn read_frame(s: &mut UnixStream) -> (u8, Vec<u8>) {
    let mut h = [0u8; 9];
    s.read_exact(&mut h).expect("frame header");
    let len = u32::from_le_bytes(h[5..9].try_into().unwrap()) as usize;
    let mut p = vec![0; len];
    s.read_exact(&mut p).expect("frame payload");
    (h[0], p)
}

fn write_frame(s: &mut UnixStream, kind: u8, payload: &[u8]) {
    let mut f = vec![kind, 0, 0, 0, 0];
    f.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_le_bytes());
    f.extend_from_slice(payload);
    s.write_all(&f).unwrap();
}

fn lflags(master: &dyn MasterPty) -> libc::tcflag_t {
    let fd = master.as_raw_fd().expect("master fd");
    let mut t = std::mem::MaybeUninit::<libc::termios>::zeroed();
    // SAFETY: tcgetattr fills the struct on success, which is checked before it is read.
    unsafe {
        assert_eq!(libc::tcgetattr(fd, t.as_mut_ptr()), 0);
        t.assume_init().c_lflag
    }
}

const COOKED: libc::tcflag_t = libc::ICANON | libc::ECHO | libc::ISIG;

/// Starts the client on a pty, feeds it a ticket, accepts its connection and reads its ATTACH.
fn start(tag: &str) -> Run {
    let dir = std::env::temp_dir().join(format!("kat{}{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sock = dir.join("a.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    listener.set_nonblocking(true).unwrap();

    let pair = native_pty_system().openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 }).unwrap();
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_kith"));
    cmd.args(["kernel", "attach", "--endpoint", sock.to_str().unwrap(), "--ticket-stdin"]);
    cmd.env_remove("KITH_DETACH_KEY");
    let child = pair.slave.spawn_command(cmd).unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    let mut reader = pair.master.try_clone_reader().unwrap();
    let shown = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = shown.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            sink.lock().unwrap().extend_from_slice(&buf[..n]);
        }
    });
    writer.write_all(b"tkt-1\n").unwrap();

    let mut relay = None;
    wait_for("the client to connect", || {
        relay = listener.accept().ok().map(|(s, _)| s);
        relay.is_some()
    });
    let mut relay = relay.unwrap();
    relay.set_nonblocking(false).unwrap();
    relay.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
    let (k, p) = read_frame(&mut relay);
    assert_eq!(k, ATTACH);
    let v: serde_json::Value = serde_json::from_slice(&p).unwrap();
    assert_eq!(v["ticket"], "tkt-1");
    assert_eq!(v["role"], "owner");
    assert_eq!((v["cols"].as_u64(), v["rows"].as_u64()), (Some(80), Some(24)));
    // Before ATTACHED the terminal is still the user's.
    assert_eq!(lflags(&*pair.master) & COOKED, COOKED);
    Run { master: pair.master, _slave: pair.slave, child, writer, relay, shown, dir }
}

fn attach(run: &mut Run) {
    write_frame(&mut run.relay, ATTACHED, br#"{"v":1,"held_id":1,"role":"owner","cols":80,"rows":24,"replay":5}"#);
    write_frame(&mut run.relay, OUTPUT, b"hello");
    wait_for("raw mode", || lflags(&*run.master) & COOKED == 0);
    let shown = run.shown.clone();
    wait_for("the output", || String::from_utf8_lossy(&shown.lock().unwrap()).contains("hello"));
}

fn cooked_again(run: &Run) {
    wait_for("the terminal to be restored", || lflags(&*run.master) & COOKED == COOKED);
}

#[test]
fn raw_while_attached_and_restored_when_the_engine_exits() {
    let mut run = start("a");
    attach(&mut run);
    write_frame(&mut run.relay, EXIT, br#"{"code":5,"signal":null}"#);
    let status = run.child.wait().unwrap();
    assert_eq!(status.exit_code(), 5);
    cooked_again(&run);
}

#[test]
fn restored_when_the_client_is_terminated() {
    let mut run = start("b");
    attach(&mut run);
    let pid = run.child.process_id().expect("pid");
    // SAFETY: signals the client process this test started.
    unsafe { libc::kill(i32::try_from(pid).unwrap(), libc::SIGTERM) };
    let status = run.child.wait().unwrap();
    assert_ne!(status.exit_code(), 0);
    cooked_again(&run);
}

#[test]
fn keys_resizes_and_the_detach_key_reach_the_relay_and_detach_restores() {
    let mut run = start("c");
    attach(&mut run);
    run.writer.write_all(b"x").unwrap();
    assert_eq!(read_frame(&mut run.relay), (INPUT, b"x".to_vec()));
    run.master.resize(PtySize { rows: 30, cols: 100, pixel_width: 0, pixel_height: 0 }).unwrap();
    let (k, p) = read_frame(&mut run.relay);
    assert_eq!(k, RESIZE);
    let v: serde_json::Value = serde_json::from_slice(&p).unwrap();
    assert_eq!((v["cols"].as_u64(), v["rows"].as_u64()), (Some(100), Some(30)));
    run.writer.write_all(&[0x1c]).unwrap();
    assert_eq!(read_frame(&mut run.relay), (DETACH, Vec::new()));
    assert_eq!(run.child.wait().unwrap().exit_code(), 0);
    cooked_again(&run);
    assert!(String::from_utf8_lossy(&run.shown.lock().unwrap()).contains("detached; the agent keeps running"));
}
