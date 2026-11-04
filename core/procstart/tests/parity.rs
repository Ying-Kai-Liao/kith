//! The Python reading (`kernel/lib/kith_procstart.py`, what `kith-sessions`, `kith-wake` and `kith-lifecycle` write) and
//! this crate's stamp the same live process identically, so a claim written by either side reads right on the other.
//! macOS and Linux (CI runs both); Windows has no Python writer.
#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::process::{Command, Stdio};

fn helper() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../kernel/lib/kith_procstart.py")
}

/// What the Python helper prints for `pid`; `None` when it prints nothing (exit 1).
fn python_stamp(pid: u32) -> Option<String> {
    let out = Command::new("python3")
        .args(["-I"])
        .arg(helper())
        .arg(pid.to_string())
        .output()
        .expect("python3 runs");
    let text = String::from_utf8(out.stdout).unwrap().trim().to_owned();
    (!text.is_empty()).then_some(text)
}

#[test]
fn rust_and_python_stamp_a_live_child_identically() {
    let mut child = Command::new("sleep")
        .arg("30")
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let rust = kith_procstart::start_stamp(child.id());
    let python = python_stamp(child.id());
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(rust.is_some(), "the child has a readable start");
    assert_eq!(rust, python);
}

#[test]
fn rust_and_python_stamp_this_process_identically() {
    let rust = kith_procstart::start_stamp(std::process::id());
    assert_eq!(rust, python_stamp(std::process::id()));
    let stamp = rust.unwrap();
    assert!(kith_procstart::is_stamp(&stamp), "{stamp}");
    // The Python side reads the stamp as one of its own, and not as a different process.
    let ok = Command::new("python3")
        .args(["-I", "-c", "import sys; sys.path.insert(0, sys.argv[1]); import kith_procstart as p; sys.exit(0 if p.is_stamp(sys.argv[2]) and p.same_process(sys.argv[2], sys.argv[2]) and p.comparable(sys.argv[2], sys.argv[2]) else 1)"])
        .arg(helper().parent().unwrap())
        .arg(&stamp)
        .status()
        .unwrap();
    assert!(ok.success());
}

#[test]
fn neither_reads_a_pid_nobody_has() {
    assert_eq!(kith_procstart::start_stamp(0x7fff_fff0), None);
    assert_eq!(python_stamp(0x7fff_fff0), None);
}

/// Another user's process (init): `proc_pidinfo` refuses it on macOS, and both sides then read the process table. Whatever
/// one can read the other reads identically.
#[test]
fn rust_and_python_agree_on_init() {
    assert_eq!(kith_procstart::start_stamp(1), python_stamp(1));
}
