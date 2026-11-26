//! Windows: starting a run's launcher. The Unix `start` `exec`s the engine; Windows has no exec, and a run there is a process in
//! a job object of its own, named by the run, which is what "is it gone" and "stop it" ask later (W12). The launcher is made
//! inside that job in one call (`kith_winsys::job::spawn_in_job`): never created, then assigned, so there is no moment it
//! runs outside the job, and a job that cannot take it fails the start (nothing runs).

use std::io;
use std::os::windows::io::{AsHandle as _, AsRawHandle as _};

use kith_winsys::job::{Job, Launch, spawn_in_job};

use super::assemble::{Env, which_in};

/// What a shell says when it cannot start a program: 127 when there is none, 126 when it can't run.
const NOT_FOUND: i32 = 127;
const CANT_RUN: i32 = 126;

/// The name of the job of the run `run`: `kith-run-<run>`, with the run's claim id (never a pid: pids are reused).
pub fn job_name(run: &str) -> String {
    format!("kith-run-{run}")
}

/// Starts `program args...` with exactly `env` inside the job `job`, waits for it, and gives its exit code. The program is found
/// on `env`'s `PATH` (with `PATHEXT`); a batch file is refused (name the `.exe`). The run's standard streams are this process's.
pub fn start(program: &str, args: &[String], env: &Env, job: &str) -> i32 {
    let Some(path) = which_in(env, program) else {
        eprintln!("kith: can't run {program}: not found on PATH");
        return NOT_FOUND;
    };
    let job = match Job::create(job) {
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
    match child.wait() {
        Ok(code) => i32::from_ne_bytes(code.to_ne_bytes()),
        Err(e) => failed(program, &e),
    }
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
        let name = job_name(&format!("test-{}", std::process::id()));
        let args = ["/d", "/c", "exit 7"].map(str::to_owned).to_vec();
        assert_eq!(start("cmd.exe", &args, &env(), &name), 7);
        // Nobody holds the job once the run is over: it is gone, as a run that ended is.
        assert!(Job::open(&name).unwrap().is_none());
    }

    #[test]
    fn a_program_that_is_not_there_is_127_and_a_batch_file_is_not_run() {
        let name = job_name(&format!("test-none-{}", std::process::id()));
        assert_eq!(start("kith-no-such-program", &[], &env(), &name), 127);
        let dir = std::env::temp_dir().join(format!("kith-winstart-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bat = dir.join("tool.bat");
        std::fs::write(&bat, "@echo off\r\nexit 0\r\n").unwrap();
        let mut e = env();
        e.insert("PATH".to_owned(), dir.to_string_lossy().into_owned());
        assert_eq!(start("tool.bat", &[], &e, &name), 126);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
