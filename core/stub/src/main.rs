//! `kith-stub`: takes a conversation's writer lock, runs the engine as its child and holds the lock to the
//! engine's exit (docs/plans/runtime-design.md section 4).
//!
//!     kith-stub --lock-dir <folder> --key <key> -- <engine argv...>
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
use std::time::{SystemTime, UNIX_EPOCH};

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
    engine: Vec<OsString>,
}

fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Args, String> {
    let mut lock_dir = None;
    let mut key = None;
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
            Some("--") => {
                let engine: Vec<OsString> = args.collect();
                if engine.is_empty() {
                    return Err("no engine command after --".into());
                }
                return Ok(Args {
                    lock_dir: lock_dir.ok_or("--lock-dir is required")?,
                    key: key.ok_or("--key is required")?,
                    engine,
                });
            }
            _ => return Err(format!("unexpected argument {a:?}")),
        }
    }
    Err("missing -- before the engine command".into())
}

/// The sidecar `w-<key>.holder` next to the lock file: who holds the lock, for doctor and "waiting for your
/// terminal session". Two lines, `pid <stub pid>` and `started <unix seconds>`. It is a hint only: nothing may read
/// it through the lock, it can be stale after a crash (the next stub overwrites it; it is never deleted), and
/// a reader must check the pid is alive and the lock actually held before believing it.
fn write_holder(lock: &Lock) -> std::io::Result<()> {
    let path = lock.path().with_extension("holder");
    let tmp = path.with_extension(format!("holder.tmp{}", std::process::id()));
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut open = OpenOptions::new();
    open.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut open, 0o600);
    open.open(&tmp)?
        .write_all(format!("pid {}\nstarted {started}\n", std::process::id()).as_bytes())?;
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
    if let Err(e) = write_holder(&lock) {
        // The record is a hint; the lock is the truth. Do not refuse to run over it.
        eprintln!("kith-stub: warning: holder record not written: {e}");
    }
    let code = run::run(&args.engine);
    drop(lock);
    ExitCode::from((code & 0xff) as u8)
}
