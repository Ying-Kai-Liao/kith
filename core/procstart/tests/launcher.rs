//! How this platform's process table (macOS: `KERN_PROCARGS2`, Linux: `/proc/<pid>/cmdline`) reads a launcher-form
//! command, which the schedules gate matches by exact words: `<script> run --quiet --script <path> tick`, with a space
//! in the folder as on a Mac install. The gate's own tests run on Linux in CI; this one runs on the macOS job too.
#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn a_launcher_form_command_is_read_with_each_word_exact() {
    // No tempfile crate: this crate links nothing but libc, and a new dev-dependency would move the shared Cargo.lock.
    let root = std::env::temp_dir().join(format!("kith-procstart-launcher-{}", std::process::id()));
    let dir = root.join("my home/bin");
    std::fs::create_dir_all(&dir).unwrap();
    let launcher = dir.join("kith-uv");
    // A loop of `sleep`, never `exec`: the shell keeps the arguments it was started with.
    std::fs::write(&launcher, "#!/bin/sh\nwhile :; do sleep 1; done\n").unwrap();
    let mut perm = std::fs::metadata(&launcher).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    std::fs::set_permissions(&launcher, perm).unwrap();
    let script = dir.join("kith-sched");
    let mut child = Command::new(&launcher)
        .args(["run", "--quiet", "--script"])
        .arg(&script)
        .arg("tick")
        .spawn()
        .unwrap();
    let want = ["run", "--quiet", "--script"];
    let until = Instant::now() + Duration::from_secs(10);
    let found = loop {
        let hit = kith_procstart::own_commands()
            .into_iter()
            .find(|(pid, args)| *pid == child.id() && args.iter().any(|a| a == "tick"));
        if hit.is_some() || Instant::now() > until {
            break hit;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    child.kill().unwrap();
    child.wait().unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    let (_, args) = found.expect("the process table lists the launcher with its arguments");
    // The interpreter may come first (`/bin/sh <script> ...`); the words after the launcher are exactly as given.
    let at = args.iter().position(|a| a == "run").unwrap();
    assert_eq!(args[at..at + 3], want);
    assert_eq!(args[at + 3], script.to_str().unwrap());
    assert_eq!(args[at + 4], "tick");
    assert!(
        args.iter().any(|a| a.ends_with("my home/bin/kith-uv")),
        "{args:?}"
    );
}
