//! A resident stub's pipe ends reach no other process the host starts (W12-7): a dead stub reads as end of file. The ends are
//! never inheritable in the host; the stub copies them itself. Here other children are started in a loop while stubs start.
#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use kith_acp::spawn_resident;
use serde_json::json;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

fn stub_bin() -> PathBuf {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../stub/Cargo.toml");
    let built = Command::new(cargo)
        .args(["build", "--manifest-path"])
        .arg(&manifest)
        .status()
        .unwrap();
    assert!(built.success(), "kith-stub did not build");
    // The test binary is in target/<profile>/deps; the stub is in target/<profile>.
    let stub = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("kith-stub.exe");
    assert!(stub.is_file(), "no {}", stub.display());
    stub
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_child_the_host_starts_meanwhile_does_not_keep_a_dead_stubs_pipes_open() {
    let stub = stub_bin();
    let locks = std::env::temp_dir().join(format!("kith-acp-leak-{}", std::process::id()));
    let cwd = std::env::temp_dir().to_string_lossy().into_owned();
    let stop = Arc::new(AtomicBool::new(false));
    // Other children, started the plain way (inheriting every inheritable handle) from another thread while stubs start.
    let spawner = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut kept: Vec<Child> = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                if let Ok(c) = Command::new("ping")
                    .args(["-n", "60", "127.0.0.1"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                {
                    kept.push(c);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            for c in &mut kept {
                let _ = c.kill();
            }
        })
    };
    for _ in 0..5 {
        let (acp, _rx, _ctl) =
            spawn_resident(&stub, &locks, "ping -n 120 127.0.0.1 >nul", &cwd, &[]).unwrap();
        // The engine never answers: the request ends only when the stub's stdout reads as end of file.
        let answer = acp.send("session/new", json!({})).await.unwrap();
        acp.end_now();
        let ended = tokio::time::timeout(Duration::from_secs(5), answer.response()).await;
        assert!(
            ended.is_ok(),
            "the dead stub's stdout never read as end of file"
        );
    }
    stop.store(true, Ordering::Relaxed);
    spawner.join().unwrap();
}
