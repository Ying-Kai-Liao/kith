//! `kith-stub --resident --ctl-pipe` on Windows: the holder side is a named-pipe server made here.
#![cfg(windows)]

use kith_stub::{Taken, take};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::io::FromRawHandle;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
};

const STUB: &str = env!("CARGO_BIN_EXE_kith-stub");

fn is_busy(dir: &Path, key: &str) -> bool {
    matches!(take(dir, key).unwrap(), Taken::Busy)
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(30);
    while !f() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn say(w: &mut File, r: &mut BufReader<File>, line: &str) -> String {
    w.write_all(format!("{line}\n").as_bytes()).unwrap();
    let mut s = String::new();
    r.read_line(&mut s).unwrap();
    s.trim_end().to_string()
}

#[test]
fn take_release_and_a_dead_holder_on_a_named_pipe() {
    let id = std::process::id();
    let dir: PathBuf = std::env::temp_dir().join(format!("kith-stub-res-win-{id}"));
    let _ = std::fs::remove_dir_all(&dir);
    let name = format!(r"\\.\pipe\kith-stub-res-{id}");
    let wname: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    // SAFETY: plain CreateNamedPipeW with default security; the handle is owned by the File below.
    let h = unsafe {
        CreateNamedPipeW(
            wname.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            1,
            4096,
            4096,
            0,
            std::ptr::null(),
        )
    };
    assert_ne!(h, INVALID_HANDLE_VALUE);
    // `ping -n 6` is about five seconds: the engine lives long enough to see the holder die.
    let mut stub = Command::new(STUB)
        .args(["--resident", "--lock-dir"])
        .arg(&dir)
        .args(["--ctl-pipe", &name, "--", "ping", "-n", "6", "127.0.0.1"])
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    // SAFETY: h is a valid pipe handle we created; waits for the stub to connect.
    unsafe { ConnectNamedPipe(h, std::ptr::null_mut()) };
    let mut w = unsafe { File::from_raw_handle(h as _) };
    let mut r = BufReader::new(w.try_clone().unwrap());

    assert_eq!(say(&mut w, &mut r, "take conv1 c1"), "locked c1");
    assert_eq!(say(&mut w, &mut r, "take conv1 c1"), "locked c1");
    assert!(is_busy(&dir, "conv1"));
    assert_eq!(say(&mut w, &mut r, "take conv1 c2"), "busy c2");
    assert_eq!(say(&mut w, &mut r, "release conv1 c2"), "not-held c2");
    assert!(is_busy(&dir, "conv1"));
    assert_eq!(
        say(&mut w, &mut r, "bogus"),
        "error - expected: <take|release> <conv-key> <claim-id>"
    );
    assert_eq!(say(&mut w, &mut r, "release conv1 c1"), "released c1");
    assert!(!is_busy(&dir, "conv1"));
    assert_eq!(say(&mut w, &mut r, "take conv1 c2"), "locked c2");

    // The holder goes away mid-turn: the lock stays until the engine is gone.
    drop(r);
    drop(w);
    std::thread::sleep(Duration::from_millis(500));
    assert!(is_busy(&dir, "conv1"));
    assert!(stub.try_wait().unwrap().is_none());
    wait_until("stub exit", || stub.try_wait().unwrap().is_some());
    assert!(!is_busy(&dir, "conv1"));
}
