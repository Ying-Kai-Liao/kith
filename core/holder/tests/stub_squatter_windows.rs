//! A process that made the control pipe's name before the holder is refused by the stub: the stub opens
//! `--ctl-pipe` only when the pipe's server is its own parent (closes the "Not verified" gap of the stub's tests).
#![cfg(windows)]

use std::process::Command;
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
use windows_sys::Win32::System::Pipes::{
    CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
};

#[test]
fn a_pipe_served_by_another_process_than_the_parent_is_refused() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../stub/Cargo.toml");
    assert!(
        Command::new(cargo)
            .args(["build", "--manifest-path"])
            .arg(&manifest)
            .status()
            .unwrap()
            .success()
    );
    // The test binary is in target/<profile>/deps; the stub is in target/<profile>.
    let exe = std::env::current_exe().unwrap();
    let stub = exe
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("kith-stub.exe");
    let locks = std::env::temp_dir().join(format!("kh-squat-{}", std::process::id()));
    let name = format!(r"\\.\pipe\kith-squat-{}", std::process::id());
    let wname: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    // SAFETY: plain CreateNamedPipeW with default security; the handle lives as long as this test.
    let server = unsafe {
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
    assert_ne!(server, INVALID_HANDLE_VALUE);
    // The stub's parent is cmd.exe, not this process, which serves the pipe: a squatter from the stub's view.
    let out = Command::new("cmd.exe")
        .args(["/d", "/c"])
        .arg(&stub)
        .args(["--resident", "--lock-dir"])
        .arg(&locks)
        .args(["--ctl-pipe", &name, "--", "cmd.exe", "/c", "exit 0"])
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(64), "{said}");
    assert!(said.contains("not by the parent"), "{said}");
}
