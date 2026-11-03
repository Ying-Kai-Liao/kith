//! `kith-stub`: takes a conversation's writer lock, runs the engine as its child and holds the lock to the
//! engine's exit (docs/plans/runtime-design.md section 4).
//!
//!     kith-stub --lock-dir <folder> --key <key> [--status-fd N] -- <engine argv...>
//!
//! `--status-fd N` (Unix, N >= 3): before the engine starts the stub writes `locked\n` or `busy\n` to that inherited
//! fd and closes it, so the engine never sees it. It makes "busy" unambiguous (the engine can itself exit 75);
//! exit 75 stays as the fallback.
//!
//! The lock folder is a required flag because this binary links no kith crate and so cannot know where a machine's
//! data lives. The caller passes `<machine>/core/locks`, which the guest guard lists already deny (`core/**`).
//!
//! The engine is a child, never an exec: the stub's lock handle is close-on-exec, so an exec would drop it, and the
//! lock must outlive everything the engine starts. The stub stays parent to the engine's exit and exits with the
//! engine's code (128+signal when a signal killed it on Unix).
//!
//! Exit codes of the stub's own failures. They share the number space with the engine's codes, so a caller that
//! must tell them apart reads the one-line message on stderr (always starts with `kith-stub:`).
//!   64  usage (bad or missing flags)
//!   70  the lock could not be taken for a reason other than busy (bad key, folder not ours, I/O)
//!   75  busy: another stub holds the key's lock (EX_TEMPFAIL)
//!   126 the engine was found but could not be started
//!   127 the engine was not found
//!
//! Killing the stub with SIGKILL releases the lock (the OS closes its handle) and leaves the engine orphaned
//! and unguarded; stubs started by the holder, which can restart them, come later. Killing whatever *started* the
//! stub changes nothing: stub and engine keep running and the lock stays held.

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use kith_stub::{Lock, Taken, take};

#[cfg(unix)]
mod run_unix;
#[cfg(unix)]
use run_unix as run;
#[cfg(windows)]
mod run_windows;
#[cfg(windows)]
use run_windows as run;

const USAGE: u8 = 64;
const LOCK_ERROR: u8 = 70;
const BUSY: u8 = 75;

struct Args {
    lock_dir: OsString,
    key: String,
    status_fd: Option<i32>,
    engine: Vec<OsString>,
}

fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Args, String> {
    let mut lock_dir = None;
    let mut key = None;
    let mut status_fd = None;
    while let Some(a) = args.next() {
        match a.to_str() {
            Some("--lock-dir") => lock_dir = Some(args.next().ok_or("--lock-dir needs a value")?),
            Some("--key") => {
                key = Some(
                    args.next()
                        .ok_or("--key needs a value")?
                        .into_string()
                        .map_err(|_| "--key is not valid UTF-8")?,
                )
            }
            Some("--status-fd") => {
                let fd: i32 = args
                    .next()
                    .and_then(|v| v.to_str()?.parse().ok())
                    .ok_or("--status-fd needs a number")?;
                if fd < 3 || cfg!(windows) {
                    return Err("--status-fd needs an fd of 3 or more (Unix only)".into());
                }
                status_fd = Some(fd);
            }
            Some("--") => {
                let engine: Vec<OsString> = args.collect();
                if engine.is_empty() {
                    return Err("no engine command after --".into());
                }
                return Ok(Args {
                    lock_dir: lock_dir.ok_or("--lock-dir is required")?,
                    key: key.ok_or("--key is required")?,
                    status_fd,
                    engine,
                });
            }
            _ => return Err(format!("unexpected argument {a:?}")),
        }
    }
    Err("missing -- before the engine command".into())
}

/// Same source as core/runtime's `process_start` (this crate links no kith crate): `ps`, squeezed to single spaces.
/// Empty where there is no `ps` (Windows has no start time there yet either).
fn process_start(pid: u32) -> String {
    let out = std::process::Command::new("ps")
        .env("LC_ALL", "C")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output();
    out.map(|o| {
        String::from_utf8_lossy(&o.stdout)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    })
    .unwrap_or_default()
}

/// Tells the starter, on the status fd, whether the lock is ours; the fd is closed here so the engine never has it.
fn report(fd: Option<i32>, word: &str) {
    #[cfg(unix)]
    if let Some(fd) = fd {
        use std::os::unix::io::FromRawFd;
        // SAFETY: the starter passed fd as an open descriptor for us; we own it and close it here.
        let _ = unsafe { std::fs::File::from_raw_fd(fd) }.write_all(format!("{word}\n").as_bytes());
    }
    #[cfg(not(unix))]
    let _ = (fd, word);
}

/// Where the run-token redeem goes (runtime-design section 4, step 3): after the lock is ours and before the
/// engine starts. Nothing to do yet.
fn redeem_run_token() {}

/// The sidecar `w-<key>.holder` next to the lock file: who holds the lock, for doctor and "waiting for your
/// terminal session". Two lines, `pid=<stub pid>` and `started=<OS start time of that pid>`, in the text the
/// runtime's own pid checks use (`ps -o lstart=` in the C locale), so a reused pid is told from the stub. It is a hint only: nothing may read
/// it through the lock, it can be stale after a crash (the next stub overwrites it; it is never deleted), and
/// a reader must check the pid is alive and the lock actually held before believing it.
fn write_holder(lock: &Lock) -> std::io::Result<()> {
    let path = lock.path().with_extension("holder");
    let tmp = path.with_extension(format!("holder.tmp{}", std::process::id()));
    let started = process_start(std::process::id());
    let mut open = OpenOptions::new();
    open.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut open, 0o600);
    open.open(&tmp)?
        .write_all(format!("pid={}\nstarted={started}\n", std::process::id()).as_bytes())?;
    // Rename, so a reader never sees half a record.
    std::fs::rename(&tmp, &path)
}

fn main() -> ExitCode {
    let args = match parse(std::env::args_os().skip(1)) {
        Ok(a) => a,
        Err(why) => {
            eprintln!(
                "kith-stub: {why}\nusage: kith-stub --lock-dir <folder> --key <key> -- <engine argv...>"
            );
            return ExitCode::from(USAGE);
        }
    };
    let lock = match take(Path::new(&args.lock_dir), &args.key) {
        Ok(Taken::Held(l)) => l,
        Ok(Taken::Busy) => {
            report(args.status_fd, "busy");
            eprintln!(
                "kith-stub: busy: another session holds the lock for {:?}",
                args.key
            );
            return ExitCode::from(BUSY);
        }
        Err(e) => {
            eprintln!("kith-stub: {e}");
            return ExitCode::from(LOCK_ERROR);
        }
    };
    report(args.status_fd, "locked");
    if let Err(e) = write_holder(&lock) {
        // The record is a hint; the lock is the truth. Do not refuse to run over it.
        eprintln!("kith-stub: warning: holder record not written: {e}");
    }
    redeem_run_token();
    let code = run::run(&args.engine);
    drop(lock);
    // The engine's full code (Windows codes are 32 bits), not its low byte.
    std::process::exit(code)
}
