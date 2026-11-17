//! `kith-stub --lock-alone` on Windows: bin/kith passes it when no core answered the claim, and the stub accepts it as the
//! plain lock. The clear it asks for (killing marked warm engines by process group) is Unix only and nothing is ever
//! marked on Windows, so there is nothing to clear; refusing the flag would fail a run that used to go ahead.
#![cfg(windows)]

use std::process::Command;

const STUB: &str = env!("CARGO_BIN_EXE_kith-stub");

#[test]
fn lock_alone_runs_the_engine_under_the_plain_lock() {
    let dir = std::env::temp_dir().join(format!("kith-stub-alone-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let status = Command::new(STUB)
        .arg("--lock-dir")
        .arg(&dir)
        .args([
            "--key",
            "alone",
            "--lock-alone",
            "--",
            "cmd",
            "/c",
            "exit 7",
        ])
        .status()
        .unwrap();
    assert_eq!(
        status.code(),
        Some(7),
        "the engine's code passes through, as without --lock-alone"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
