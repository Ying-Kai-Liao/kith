//! P-W12c (docs/plans/windows-owner.md, W12): once the recorded process reads exited, no process of its kill-on-close job runs
//! user code. L holds the job; E, which L starts in it, and E's child each append to a file in a tight loop. L is terminated
//! hard and waited for; from then on neither file may grow. A file that grows after L has gone means the premise is false and
//! the launcher design does not hold on this Windows.
#![cfg(windows)]
#![allow(clippy::unwrap_used)]

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use kith_winsys::job::{Job, Launch, spawn_in_job};

const ROLE: &str = "KITH_PW12C_ROLE";
const DIR: &str = "KITH_PW12C_DIR";
const ME: &str = "role_entry";
const RUNS: usize = 100;

fn spin(path: &Path) -> ! {
    let mut f = OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .unwrap();
    let mut n: u64 = 0;
    loop {
        // A counter, one short line per write, as fast as the disk takes them.
        let _ = f.write_all(format!("{n}\n").as_bytes());
        n += 1;
    }
}

/// Not a test of its own: the entry point of L, E and E's child, which the probe starts by running this binary again.
/// Without a role it does nothing.
#[test]
fn role_entry() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let dir = PathBuf::from(std::env::var(DIR).unwrap());
    let exe = std::env::current_exe().unwrap();
    match role.as_str() {
        "launcher" => {
            let job = Job::create().unwrap();
            let mut env: Vec<(String, String)> = std::env::vars().collect();
            env.retain(|(k, _)| k != ROLE);
            env.push((ROLE.to_owned(), "engine".to_owned()));
            let args = ["--exact", ME, "--nocapture"].map(str::to_owned).to_vec();
            let launch = Launch {
                program: &exe,
                args: &args,
                env: &env,
                cwd: None,
                stdio: None,
            };
            let _engine = spawn_in_job(&job, &launch).unwrap();
            // Held until the test terminates this process.
            std::thread::sleep(Duration::from_secs(300));
        }
        "engine" => {
            // Made by the job's own process, so it is in the job without anything done here.
            // Never waited for: this process writes until the job ends it.
            #[allow(clippy::zombie_processes)]
            let _child = Command::new(exe)
                .args(["--exact", ME, "--nocapture"])
                .env(ROLE, "leaf")
                .spawn()
                .unwrap();
            spin(&dir.join("engine.txt"));
        }
        _ => spin(&dir.join("leaf.txt")),
    }
}

fn size(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |m| m.len())
}

#[test]
fn nothing_in_a_dead_launchers_job_writes_after_the_launcher_has_gone() {
    let base = std::env::temp_dir().join(format!("kith-pw12c-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    for run in 0..RUNS {
        let dir = base.join(run.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let (e, g) = (dir.join("engine.txt"), dir.join("leaf.txt"));
        let mut launcher = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", ME, "--nocapture"])
            .env(ROLE, "launcher")
            .env(DIR, &dir)
            .spawn()
            .unwrap();
        // Both writers are running before the launcher is hit.
        let t = Instant::now();
        while size(&e) < 4096 || size(&g) < 4096 {
            assert!(
                t.elapsed() < Duration::from_secs(60),
                "run {run}: the writers never got going (engine {}, leaf {})",
                size(&e),
                size(&g)
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        launcher.kill().unwrap();
        launcher.wait().unwrap();
        let (e1, g1) = (size(&e), size(&g));
        std::thread::sleep(Duration::from_millis(200));
        let (e2, g2) = (size(&e), size(&g));
        assert_eq!(
            (e1, g1),
            (e2, g2),
            "P-W12c FAILED on run {run}: after the launcher was gone the engine's file went {e1} -> {e2} bytes and the \
             grandchild's {g1} -> {g2} bytes in 200 ms"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    let _ = std::fs::remove_dir_all(&base);
}
