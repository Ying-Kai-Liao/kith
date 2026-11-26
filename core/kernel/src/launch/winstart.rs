//! Windows: starting a run's launcher. The Unix `start` `exec`s the engine; Windows has no exec, so the launcher stays resident
//! and a run there is a process in a kill-on-close job object the launcher itself holds (docs/plans/windows-owner.md, W12 rules
//! H1 to H5). The job is unnamed and its handle is the launcher's alone: when the launcher ends, however it ends, everything the
//! run started goes with it, so "the recorded process is gone" means "the writer is gone", as process groups give it on the Mac.
//!
//! The caller has attached the claim with this process's own pid before `start` is called (H3: the claim names the launcher
//! before anything is created in the job; `start` creates nothing before the job, and the job nothing before the claim). The
//! engine is made inside the job in one call (`kith_winsys::job::spawn_in_job`): never created, then assigned, so there is no
//! moment it runs outside the job, and a job that cannot be made or cannot take it fails the start (nothing runs, and there is
//! no plain spawn to fall back on). The launcher ends only after the job's last process has (H5).

use std::io;
use std::os::windows::io::{AsHandle as _, AsRawHandle as _};

use kith_winsys::job::{Job, Launch, spawn_in_job};

use super::assemble::{Env, which_in};

/// What a shell says when it cannot start a program: 127 when there is none, 126 when it can't run.
const NOT_FOUND: i32 = 127;
const CANT_RUN: i32 = 126;

/// Starts `program args...` with exactly `env` inside a job of its own, waits for it and for everything it started, and gives
/// its exit code. The program is found on `env`'s `PATH` (with `PATHEXT`); a batch file is refused (name the `.exe`). The run's
/// standard streams are this process's.
pub fn start(program: &str, args: &[String], env: &Env) -> i32 {
    let Some(path) = which_in(env, program) else {
        eprintln!("kith: can't run {program}: not found on PATH");
        return NOT_FOUND;
    };
    let job = match Job::create() {
        Ok(j) => j,
        Err(e) => return failed(program, &e),
    };
    let vars: Vec<(String, String)> = env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let cwd = std::env::current_dir().ok();
    let (stdin, stdout, stderr) = (io::stdin(), io::stdout(), io::stderr());
    // A service has no standard streams (a null or invalid handle): the run then has none either.
    let usable = [
        stdin.as_raw_handle(),
        stdout.as_raw_handle(),
        stderr.as_raw_handle(),
    ]
    .iter()
    .all(|h| !h.is_null() && *h as isize != -1);
    let launch = Launch {
        program: &path,
        args,
        env: &vars,
        cwd: cwd.as_deref(),
        stdio: usable.then(|| [stdin.as_handle(), stdout.as_handle(), stderr.as_handle()]),
    };
    let child = match spawn_in_job(&job, &launch) {
        Ok(c) => c,
        Err(e) => return failed(program, &e),
    };
    let code = match child.wait() {
        Ok(code) => i32::from_ne_bytes(code.to_ne_bytes()),
        Err(e) => return failed(program, &e),
    };
    // What the engine left running is still the run: the job's handle closes when this function returns, and that would kill
    // it. Waiting for the job to empty keeps the launcher (the recorded process) alive as long as the run has any writer.
    if let Err(e) = job.wait_idle() {
        return failed(program, &e);
    }
    code
}

fn failed(program: &str, e: &io::Error) -> i32 {
    eprintln!("kith: can't run {program}: {e}");
    if e.kind() == io::ErrorKind::NotFound {
        NOT_FOUND
    } else {
        CANT_RUN
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{Duration, Instant};

    fn env() -> Env {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_owned());
        [
            ("SystemRoot".to_owned(), root.clone()),
            ("PATH".to_owned(), format!("{root}\\System32")),
            ("PATHEXT".to_owned(), ".COM;.EXE;.BAT;.CMD".to_owned()),
        ]
        .into()
    }

    #[test]
    fn a_run_starts_in_its_job_and_gives_its_exit_code() {
        let args = ["/d", "/c", "exit 7"].map(str::to_owned).to_vec();
        assert_eq!(start("cmd.exe", &args, &env()), 7);
    }

    #[test]
    fn a_program_that_is_not_there_is_127_and_a_batch_file_is_not_run() {
        assert_eq!(start("kith-no-such-program", &[], &env()), 127);
        let dir = std::env::temp_dir().join(format!("kith-winstart-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bat = dir.join("tool.bat");
        std::fs::write(&bat, "@echo off\r\nexit 0\r\n").unwrap();
        let mut e = env();
        e.insert("PATH".to_owned(), dir.to_string_lossy().into_owned());
        assert_eq!(start("tool.bat", &[], &e), 126);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // The tests below run this test binary as the launcher, its engine and the engine's child, told apart by KITH_WINSTART_ROLE
    // (a test is the only way this crate has to run a function in a process of its own).
    const ROLE: &str = "KITH_WINSTART_ROLE";
    const DIR: &str = "KITH_WINSTART_DIR";
    const ME: &str = "launch::winstart::tests::role_entry";

    /// Not a test of its own: the entry point of the processes the test below starts. Without a role it does nothing.
    #[test]
    fn role_entry() {
        let Ok(role) = std::env::var(ROLE) else {
            return;
        };
        let dir = PathBuf::from(std::env::var(DIR).unwrap());
        let exe = std::env::current_exe().unwrap();
        let args = ["--exact", ME, "--nocapture"].map(str::to_owned).to_vec();
        match role.as_str() {
            "launcher" => {
                let mut e = env();
                e.insert(ROLE.to_owned(), "engine".to_owned());
                e.insert(DIR.to_owned(), dir.to_string_lossy().into_owned());
                std::process::exit(start(&exe.to_string_lossy(), &args, &e));
            }
            "engine" => {
                std::fs::write(dir.join("engine"), std::process::id().to_string()).unwrap();
                let mut child = Command::new(exe)
                    .args(&args)
                    .env(ROLE, "leaf")
                    .spawn()
                    .unwrap();
                let _ = child.wait();
            }
            _ => {
                std::fs::write(dir.join("leaf"), std::process::id().to_string()).unwrap();
                std::thread::sleep(Duration::from_secs(120));
            }
        }
    }

    fn pid_in(dir: &Path, name: &str) -> u32 {
        let t = Instant::now();
        loop {
            if let Ok(s) = std::fs::read_to_string(dir.join(name))
                && let Ok(pid) = s.trim().parse()
            {
                return pid;
            }
            assert!(
                t.elapsed() < Duration::from_secs(60),
                "{name} never started"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn gone(pid: u32) -> bool {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(20) {
            if kith_procstart::probe(pid) == kith_procstart::Probe::Gone {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    #[test]
    fn a_launcher_that_is_killed_takes_its_engine_and_the_engines_child_with_it() {
        let dir = std::env::temp_dir().join(format!("kith-winstart-kill-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut launcher = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", ME, "--nocapture"])
            .env(ROLE, "launcher")
            .env(DIR, &dir)
            .spawn()
            .unwrap();
        let (engine, leaf) = (pid_in(&dir, "engine"), pid_in(&dir, "leaf"));
        assert_ne!(kith_procstart::probe(engine), kith_procstart::Probe::Gone);
        assert_ne!(kith_procstart::probe(leaf), kith_procstart::Probe::Gone);
        // A hard kill: nothing in the launcher runs again, so only the job's close can end the rest.
        launcher.kill().unwrap();
        launcher.wait().unwrap();
        assert!(gone(engine), "the engine outlived its launcher");
        assert!(gone(leaf), "the engine's child outlived its launcher");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
