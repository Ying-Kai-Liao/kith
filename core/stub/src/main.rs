//! `kith-stub`: takes a conversation's writer lock, runs the engine as its child and holds the lock to the
//! engine's exit (docs/plans/runtime-design.md section 4).
//!
//!     kith-stub --lock-dir <folder> --key <key> [--status-fd N] -- <engine argv...>
//!     kith-stub --resident --lock-dir <folder> (--ctl-fd N | --ctl-pipe <path>) -- <engine argv...>
//!
//! The second form is for a warm engine: no lock at start; the holder takes and releases conversation locks per
//! turn over a control channel (protocol in `kith_stub::resident`). `--ctl-fd N` (Unix, N >= 3) is an inherited
//! bidirectional fd, e.g. one end of a socketpair; the stub marks it close-on-exec so the engine never has it.
//! `--ctl-pipe <path>` (Windows) is a named pipe `\\.\pipe\...` the holder created and the stub opens as a
//! client: the holder already serves Mode::Pipes children over named pipes, and passing a raw handle number would
//! need a handle-list spawn attribute and make the number meaningless once the holder restarts. The engine is
//! still a child, the stub exits after it with its code, and a closed channel (the holder died) never releases
//! anything: held locks stay until the engine exits.
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

use kith_stub::{Lock, Taken, resident, take};

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

enum Ctl {
    #[cfg_attr(windows, allow(dead_code))] // Unix only; parse refuses it on Windows
    Fd(i32),
    #[cfg_attr(unix, allow(dead_code))] // Windows only; parse refuses it on Unix
    Pipe(OsString),
}

struct Args {
    lock_dir: OsString,
    /// Empty in resident mode.
    key: String,
    status_fd: Option<i32>,
    ctl: Option<Ctl>,
    engine: Vec<OsString>,
}

fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Args, String> {
    let mut lock_dir = None;
    let mut key = None;
    let mut status_fd = None;
    let mut resident = false;
    let mut ctl = None;
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
            Some("--resident") => resident = true,
            Some("--ctl-fd") => {
                let fd: i32 = args
                    .next()
                    .and_then(|v| v.to_str()?.parse().ok())
                    .ok_or("--ctl-fd needs a number")?;
                if fd < 3 || cfg!(windows) {
                    return Err("--ctl-fd needs an fd of 3 or more (Unix only)".into());
                }
                ctl = Some(Ctl::Fd(fd));
            }
            Some("--ctl-pipe") => {
                if cfg!(unix) {
                    return Err("--ctl-pipe is Windows only; use --ctl-fd".into());
                }
                ctl = Some(Ctl::Pipe(args.next().ok_or("--ctl-pipe needs a value")?));
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
                if resident {
                    if key.is_some() || status_fd.is_some() {
                        return Err("--resident takes no --key or --status-fd".into());
                    }
                    if ctl.is_none() {
                        return Err(
                            "--resident needs --ctl-fd (Unix) or --ctl-pipe (Windows)".into()
                        );
                    }
                } else if ctl.is_some() {
                    return Err("--ctl-fd / --ctl-pipe need --resident".into());
                }
                return Ok(Args {
                    lock_dir: lock_dir.ok_or("--lock-dir is required")?,
                    key: if resident {
                        String::new()
                    } else {
                        key.ok_or("--key is required")?
                    },
                    status_fd,
                    ctl,
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
///
/// Resident mode runs it per take, before `locked` is answered; an `Err` there frees the lock and answers `error`.
fn redeem_run_token() -> Result<(), String> {
    Ok(())
}

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

/// The channel must be a socket whose peer is the same user: "owner-only" is checked here, not promised by the starter.
#[cfg(unix)]
fn check_unix_channel(fd: i32) -> Result<(), String> {
    // SAFETY: fstat writes into st; fd was checked open.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } == -1 {
        return Err(format!(
            "--ctl-fd {fd}: {}",
            std::io::Error::last_os_error()
        ));
    }
    if st.st_mode & libc::S_IFMT != libc::S_IFSOCK {
        return Err(format!("--ctl-fd {fd} is not a socket"));
    }
    let uid = peer_uid(fd).map_err(|e| format!("--ctl-fd {fd}: peer credentials: {e}"))?;
    // SAFETY: geteuid has no arguments.
    if uid != unsafe { libc::geteuid() } {
        return Err(format!("--ctl-fd {fd}: the peer is uid {uid}, not ours"));
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn peer_uid(fd: i32) -> std::io::Result<u32> {
    // SAFETY: SO_PEERCRED fills a ucred of len bytes.
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = size_of::<libc::ucred>() as libc::socklen_t;
    let r = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if r == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(cred.uid)
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn peer_uid(fd: i32) -> std::io::Result<u32> {
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: getpeereid writes two ids.
    if unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(uid)
}

/// Opens the control channel as a (reader, writer) pair, both ends closed on exec / not inheritable.
fn open_ctl(ctl: &Ctl) -> Result<(std::fs::File, std::fs::File), String> {
    match ctl {
        #[cfg(unix)]
        Ctl::Fd(fd) => {
            use std::os::unix::io::FromRawFd;
            // SAFETY: F_GETFD only reads the descriptor flags.
            if unsafe { libc::fcntl(*fd, libc::F_GETFD) } == -1 {
                return Err(format!("--ctl-fd {fd} is not an open descriptor"));
            }
            check_unix_channel(*fd)?;
            // SAFETY: the starter passed fd as an open descriptor for us; we own it from here.
            let file = unsafe { std::fs::File::from_raw_fd(*fd) };
            // Before the engine starts: the engine must never hold the channel (it could answer for the holder).
            // SAFETY: setting FD_CLOEXEC on a descriptor we own.
            if unsafe { libc::fcntl(*fd, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
                return Err(format!(
                    "cannot set close-on-exec: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let w = file.try_clone().map_err(|e| e.to_string())?;
            Ok((file, w))
        }
        #[cfg(windows)]
        Ctl::Pipe(path) => {
            // Only a local named pipe: a regular file or a remote \\host\pipe\x path is not the holder's channel.
            if !path.to_string_lossy().starts_with(r"\\.\pipe\") {
                return Err(format!(
                    "--ctl-pipe {path:?} is not a local named pipe (\\\\.\\pipe\\...)"
                ));
            }
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .map_err(|e| format!("cannot open {path:?}: {e}"))?;
            // A squatter that created the name first would be served as the holder: its server must be our parent.
            let (server, parent) = (kith_stub::pipe_server_pid(&file), kith_stub::parent_pid());
            if server.is_none() || server != parent {
                return Err(format!(
                    "--ctl-pipe {path:?} is served by pid {server:?}, not by the parent {parent:?}"
                ));
            }
            let w = file.try_clone().map_err(|e| e.to_string())?;
            Ok((file, w))
        }
        #[cfg(unix)]
        Ctl::Pipe(_) => unreachable!("parse refuses --ctl-pipe on Unix"),
        #[cfg(windows)]
        Ctl::Fd(_) => unreachable!("parse refuses --ctl-fd on Windows"),
    }
}

/// Resident mode: serve the control channel on a thread, run the engine, exit with its code. The thread is not
/// joined: it ends with the process, and whatever lock it holds is closed by the OS then, after the engine.
fn resident_main(args: Args) -> ExitCode {
    let (r, w) = match open_ctl(args.ctl.as_ref().expect("parse checked")) {
        Ok(p) => p,
        Err(why) => {
            eprintln!("kith-stub: {why}");
            return ExitCode::from(USAGE);
        }
    };
    let dir = std::path::PathBuf::from(&args.lock_dir);
    std::thread::spawn(move || {
        resident::serve(&dir, std::io::BufReader::new(r), w, |lock| {
            if let Err(e) = write_holder(lock) {
                eprintln!("kith-stub: warning: holder record not written: {e}");
            }
            // Per take, before `locked` is answered (runtime-design section 4, step 3): the lock is ours, the turn
            // is not yet running.
            redeem_run_token()
        })
    });
    std::process::exit(run::run(&args.engine))
}

fn main() -> ExitCode {
    let args = match parse(std::env::args_os().skip(1)) {
        Ok(a) => a,
        Err(why) => {
            eprintln!(
                "kith-stub: {why}\nusage: kith-stub [--resident --ctl-fd N] --lock-dir <folder> [--key <key>] -- <engine argv...>"
            );
            return ExitCode::from(USAGE);
        }
    };
    if args.ctl.is_some() {
        return resident_main(args);
    }
    // Before take(): a closed fd N would be handed to the lock file (open picks the lowest free fd), and report()
    // would then write into the lock file and close it, freeing the lock under a running engine.
    #[cfg(unix)]
    if let Some(fd) = args.status_fd {
        // SAFETY: F_GETFD only reads the descriptor flags.
        if unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1 {
            eprintln!("kith-stub: --status-fd {fd} is not an open descriptor");
            return ExitCode::from(USAGE);
        }
    }
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
    if let Err(e) = redeem_run_token() {
        eprintln!("kith-stub: {e}");
        return ExitCode::from(LOCK_ERROR);
    }
    let code = run::run(&args.engine);
    drop(lock);
    // The engine's full code (Windows codes are 32 bits), not its low byte.
    std::process::exit(code)
}
