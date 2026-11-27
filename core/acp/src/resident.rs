//! A warm engine under `kith-stub --resident`, and the control channel the host drives its writer lock with (M3 cutover,
//! designs/2026-11-14-m3-cutover section 1.1). The protocol is `kith_stub::resident`'s: one `take <key> <claim>` /
//! `release <key> <claim>` / `loaded <key>` / `forgot <key>` line, one answer line.
//!
//! The ACP wire does not change: the engine inherits the stub's stdin and stdout, so [`crate::Acp::pid`] is the stub's pid and
//! the stub leads the process group. The control channel is one end of a socketpair; the stub's end goes to the child as an
//! inherited fd (3 or more, as `--ctl-fd` requires) and is closed here as soon as the child is started, so a dead host reads
//! as end-of-file in the stub. Ours is close-on-exec everywhere.
//!
//! On Windows (W12-7) the control channel is a named pipe the host creates (`kith_winsys::pipe`, owner-only) and the stub opens
//! as a client (`--ctl-pipe`). The stub is made inside a job the host holds, unnamed and kill on close (W12 rules H1, H2 and
//! rule 6), in one call, with its stdio on pipes the host made: a host that ends, however it ends, ends the stub, and the
//! stub's own job ends its engine. [`crate::Acp::end_now`] ends the host's job at once.

use crate::{Acp, Incoming};
use anyhow::Result;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// What the stub answered to a `take`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Take {
    /// The lock is held for the claim (also the answer to a repeated take of the same claim).
    Locked,
    /// Another stub, a `--direct` run or another claim of this stub holds the conversation.
    Busy,
    /// The stub could not say (a bad key, an I/O error). The lock is not held.
    Error(String),
}

/// What the stub answered to a `release`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Release {
    Released,
    /// The lock was not held for this claim and key (a repeat, or a release with no take); nothing changed.
    NotHeld,
    Error(String),
}

/// The host's end of a resident stub's control channel. Lines go out one at a time and each is answered before the next
/// (the stub serves in order), so the calls take turns on an inner lock. A call that fails to get its answer (the stub
/// died, a timeout, an answer for another claim) breaks the channel for good: a late answer could otherwise be read as
/// the next call's. The caller restarts the process, as it does for a stub that is gone.
pub struct Ctl {
    io: Mutex<Option<Box<dyn Wire>>>,
}

/// One line out and its answer line back, over whatever the platform's channel is (a socketpair, a named pipe).
pub(crate) trait Wire: Send {
    /// Writes `line` and a newline, flushes, and reads one line into `got` (bytes read; 0 is end of file).
    fn exchange(&mut self, line: &str, got: &mut String) -> std::io::Result<usize>;
}

impl Ctl {
    pub(crate) fn new(wire: impl Wire + 'static) -> Self {
        Self {
            io: Mutex::new(Some(Box::new(wire))),
        }
    }

    /// `take <key> <claim>`. `Err` is a channel that gave no answer (the stub is gone or wedged); the lock may or may not
    /// be held then, and a retry (same claim) is safe: the stub answers `locked` again for the claim it holds.
    pub fn take(&self, key: &str, claim: i64) -> std::io::Result<Take> {
        let (verb, said) = self.ask("take", key, claim)?;
        Ok(match verb.as_str() {
            "locked" => Take::Locked,
            "busy" => Take::Busy,
            _ => Take::Error(said),
        })
    }

    /// `release <key> <claim>`. Never follow it with a `take` of the same claim: the stub cannot know the claim is over.
    pub fn release(&self, key: &str, claim: i64) -> std::io::Result<Release> {
        let (verb, said) = self.ask("release", key, claim)?;
        Ok(match verb.as_str() {
            "released" => Release::Released,
            "not-held" => Release::NotHeld,
            _ => Release::Error(said),
        })
    }

    /// `loaded <key>`: the engine is about to load the conversation, so a run on the lock alone must stop it first
    /// (design 2026-11-16-offturn-lock-alone). `Ok(Err(reason))`: the stub answered but did not record it.
    pub fn loaded(&self, key: &str) -> std::io::Result<Result<(), String>> {
        self.mark("loaded", "marked", key)
    }

    /// `forgot <key>`: the engine no longer has the conversation for the host (its `warm_procs` row is gone too).
    pub fn forgot(&self, key: &str) -> std::io::Result<Result<(), String>> {
        self.mark("forgot", "unmarked", key)
    }

    fn mark(&self, verb: &str, ok: &str, key: &str) -> std::io::Result<Result<(), String>> {
        if key.is_empty() || key.contains([' ', '\n', '\r']) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "not a key",
            ));
        }
        let (said, why) = self.ask_line(&format!("{verb} {key}"), key)?;
        Ok(if said == ok { Ok(()) } else { Err(why) })
    }

    /// One line out, one line back: (the answer's first word, the reason that follows an `error`). An answer for another
    /// claim, or one that is not a line, is a broken channel.
    fn ask(&self, verb: &str, key: &str, claim: i64) -> std::io::Result<(String, String)> {
        if claim < 0 || key.is_empty() || key.contains([' ', '\n', '\r']) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "not a key and claim id",
            ));
        }
        self.ask_line(&format!("{verb} {key} {claim}"), &claim.to_string())
    }

    /// `line` out, its answer back; every answer but `error - ...` names `tag` (the claim, or the key of a mark).
    fn ask_line(&self, line: &str, tag: &str) -> std::io::Result<(String, String)> {
        let broken =
            |why: &str| std::io::Error::new(std::io::ErrorKind::BrokenPipe, why.to_owned());
        let mut guard = self
            .io
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let wire = guard
            .as_mut()
            .ok_or_else(|| broken("the control channel is closed"))?;
        let mut got = String::new();
        let sent = wire.exchange(line, &mut got);
        let answer = match sent {
            Ok(n) if n > 0 && got.ends_with('\n') => got.trim_end_matches(['\n', '\r']).to_owned(),
            Ok(_) => String::new(),
            Err(e) => {
                *guard = None;
                return Err(e);
            }
        };
        let mut words = answer.splitn(3, ' ');
        let (said, for_claim, why) = (
            words.next().unwrap_or(""),
            words.next(),
            words.next().unwrap_or(""),
        );
        // `error - <reason>` (a line the stub could not read) names no claim; every other answer names ours.
        if said.is_empty() || (for_claim != Some(tag) && said != "error") {
            *guard = None;
            return Err(broken("the stub's answer was not for this claim"));
        }
        Ok((said.to_owned(), why.to_owned()))
    }

    /// Whether the channel is still usable (it breaks on the first call that gets no answer).
    pub fn is_open(&self) -> bool {
        self.io
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }
}

#[cfg(unix)]
pub use unix::spawn_resident;

#[cfg(windows)]
pub use windows::spawn_resident;

#[cfg(unix)]
mod unix {
    use super::{Acp, Arc, Ctl, Incoming, Result, Wire, mpsc};
    use anyhow::Context;
    use std::io::{BufRead, BufReader, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::time::Duration;

    /// How long a stub gets to answer a line. A take or release is a file lock and a write: a stub that is silent for this
    /// long is wedged, and the channel is dropped (see [`Ctl`]).
    const ANSWER_WITHIN: Duration = Duration::from_secs(10);

    /// The socketpair's end and a reader over a copy of it.
    struct UnixWire(UnixStream, BufReader<UnixStream>);

    impl Wire for UnixWire {
        fn exchange(&mut self, line: &str, got: &mut String) -> std::io::Result<usize> {
            writeln!(self.0, "{line}")?;
            self.0.flush()?;
            self.1.read_line(got)
        }
    }

    fn ctl(stream: UnixStream) -> std::io::Result<Ctl> {
        stream.set_read_timeout(Some(ANSWER_WITHIN))?;
        stream.set_write_timeout(Some(ANSWER_WITHIN))?;
        let reader = BufReader::new(stream.try_clone()?);
        Ok(Ctl::new(UnixWire(stream, reader)))
    }

    /// The control channel's pair: ours (close-on-exec) and the stub's, moved above fd 2 (`--ctl-fd` needs 3 or more) and also
    /// close-on-exec until the child's own `pre_exec` clears it, so a spawn on another thread never leaks it.
    fn pair() -> std::io::Result<(UnixStream, OwnedFd)> {
        let (ours, theirs) = UnixStream::pair()?;
        let theirs = OwnedFd::from(theirs);
        // SAFETY: duplicating an fd we own; the new number is wrapped at once.
        let fd = unsafe { libc::fcntl(theirs.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `fd` is a fresh descriptor nothing else owns.
        Ok((ours, unsafe { OwnedFd::from_raw_fd(fd) }))
    }

    /// Starts `command` (a shell line, as [`Acp::spawn`] takes it) as `stub --resident --lock-dir <lock_dir> --ctl-fd N --
    /// /bin/sh -c "exec <command>"`. The host speaks ACP to it over its stdio and drives its writer lock through the returned
    /// [`Ctl`].
    pub fn spawn_resident(
        stub: &Path,
        lock_dir: &Path,
        command: &str,
        cwd: &str,
        env: &[(String, String)],
    ) -> Result<(Arc<Acp>, mpsc::UnboundedReceiver<Incoming>, Ctl)> {
        let (ours, theirs) = pair().context("making the control channel")?;
        let mut process = tokio::process::Command::new(stub);
        process
            .arg("--resident")
            .arg("--lock-dir")
            .arg(lock_dir)
            .arg("--ctl-fd")
            .arg(theirs.as_raw_fd().to_string())
            .args(["--", "/bin/sh", "-c", &format!("exec {command}")]);
        let fd = theirs.as_raw_fd();
        // SAFETY: only fcntl runs between fork and exec (async-signal-safe). Only this child gets the stub's end.
        unsafe {
            process.pre_exec(move || {
                if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let started = Acp::start(
            process,
            &format!("kith-stub --resident {command}"),
            cwd,
            env,
        );
        // The host keeps no copy of the stub's end, spawned or not: a dead host must read as end-of-file there.
        drop(theirs);
        let (acp, rx) = started?;
        Ok((acp, rx, ctl(ours).context("opening the control channel")?))
    }
}

#[cfg(windows)]
mod windows {
    use super::{Acp, Arc, Ctl, Incoming, Result, Wire, mpsc};
    use crate::client::{CORE_RUN_SOCKET_ENV, Proc, RUN_SOCKET_ENV, RUN_TOKEN_ENV, shell_command};
    use crate::held::Held;
    use anyhow::{Context, bail};
    use kith_winsys::job::{Job, Launch, spawn_in_job};
    use kith_winsys::pipe;
    use std::collections::BTreeMap;
    use std::io::{BufRead, BufReader, Write};
    use std::os::windows::io::{AsHandle, AsRawHandle, OwnedHandle};
    use std::path::Path;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    /// How long a stub gets to answer a line (as on Unix): a stub silent for this long is wedged and the channel is dropped.
    const ANSWER_WITHIN: Duration = Duration::from_secs(10);
    /// How long a started stub gets to open its end of the control channel. It does so first thing, before its engine starts.
    const OPEN_WITHIN: Duration = Duration::from_secs(10);

    /// The named pipe's stream and a buffered reader over it: one line out, one line back.
    struct PipeWire(BufReader<pipe::Stream>);

    impl Wire for PipeWire {
        fn exchange(&mut self, line: &str, got: &mut String) -> std::io::Result<usize> {
            let out = self.0.get_mut();
            out.write_all(format!("{line}\n").as_bytes())?;
            out.flush()?;
            self.0.read_line(got)
        }
    }

    /// A machine-wide pipe name no one else can guess or has taken: this host's pid, a counter and the clock. `listen` makes the
    /// first instance of the name or fails, so a squatter fails our start instead of being served.
    fn pipe_name() -> String {
        static N: AtomicU32 = AtomicU32::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        format!(
            r"\\.\pipe\kith-stub-ctl-{}-{}-{nanos:x}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// This process's environment with `extra` on top (names compare without regard to case), without the host's own run token
    /// and sockets: a process made in a job gets exactly the environment it is given.
    fn environment(extra: &[(String, String)]) -> Vec<(String, String)> {
        let mut all: BTreeMap<String, (String, String)> = BTreeMap::new();
        for (k, v) in std::env::vars() {
            if [RUN_TOKEN_ENV, RUN_SOCKET_ENV, CORE_RUN_SOCKET_ENV]
                .iter()
                .any(|own| own.eq_ignore_ascii_case(&k))
            {
                continue;
            }
            all.insert(k.to_uppercase(), (k, v));
        }
        for (k, v) in extra {
            all.insert(k.to_uppercase(), (k.clone(), v.clone()));
        }
        all.into_values().collect()
    }

    fn file_of(h: impl Into<OwnedHandle>) -> tokio::fs::File {
        tokio::fs::File::from_std(std::fs::File::from(h.into()))
    }

    /// Waits for the stub to open the control channel. A stub that ended first, or never comes, is a failed start.
    fn accept(
        listener: pipe::Listener,
        name: &str,
        stub_pid: u32,
        job: &Job,
    ) -> Result<pipe::Stream> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(listener.accept());
        });
        let until = Instant::now() + OPEN_WITHIN;
        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Ok(stream)) => {
                    // The pipe's access list keeps other accounts out; this keeps another process of ours out.
                    if stream.peer_pid() != Some(stub_pid) {
                        let _ = job.terminate(1);
                        bail!("the control channel was opened by another process than the stub");
                    }
                    return Ok(stream);
                }
                Ok(Err(e)) => {
                    let _ = job.terminate(1);
                    return Err(e).context("waiting for the stub on the control channel");
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = job.terminate(1);
                    bail!("the control channel's listener ended");
                }
            }
            if Instant::now() >= until || job.active_processes().is_ok_and(|n| n == 0) {
                let _ = job.terminate(1);
                // The listener's thread is waiting for a client: be one, so it ends.
                let _ = pipe::connect(name, Duration::from_millis(500));
                bail!("the stub did not open the control channel");
            }
        }
    }

    /// Starts `command` as `stub --resident --lock-dir <lock_dir> --ctl-pipe <pipe> -- <the engine's shell>`, in a new job this
    /// process holds (unnamed, kill on close, no breakaway: W12 H1 and rule 6), in one call (H2). The host speaks ACP to it over
    /// its stdio and drives its writer lock through the returned [`Ctl`]. When the host ends, or the returned [`Acp`] is dropped
    /// or ended ([`Acp::end_now`]), the stub and what it started end; the stub's own job (W12-1) covers its engine.
    pub fn spawn_resident(
        stub: &Path,
        lock_dir: &Path,
        command: &str,
        cwd: &str,
        env: &[(String, String)],
    ) -> Result<(Arc<Acp>, mpsc::UnboundedReceiver<Incoming>, Ctl)> {
        let job = Arc::new(Job::create().context("making the stub's job")?);
        let name = pipe_name();
        let listener = pipe::listen(&name).context("making the control channel")?;
        let shell = shell_command(command);
        let shell = shell.as_std();
        let mut args: Vec<String> = vec![
            "--resident".to_owned(),
            "--lock-dir".to_owned(),
            lock_dir.to_string_lossy().into_owned(),
            "--ctl-pipe".to_owned(),
            name.clone(),
            "--".to_owned(),
            shell.get_program().to_string_lossy().into_owned(),
        ];
        args.extend(shell.get_args().map(|a| a.to_string_lossy().into_owned()));
        // What the shell line needs in the environment (the host's `shell::exec` passes the line itself in a variable) goes first,
        // so `env` can still replace it; the stub's engine inherits the stub's environment.
        let mut wanted: Vec<(String, String)> = shell
            .get_envs()
            .filter_map(|(k, v)| {
                Some((
                    k.to_string_lossy().into_owned(),
                    v?.to_string_lossy().into_owned(),
                ))
            })
            .collect();
        wanted.extend(env.iter().cloned());
        let environment = environment(&wanted);
        let (stdin_r, stdin_w) = std::io::pipe().context("making the stub's stdin")?;
        let (stdout_r, stdout_w) = std::io::pipe().context("making the stub's stdout")?;
        let (stderr_r, stderr_w) = std::io::pipe().context("making the stub's stderr")?;
        // The stub's ends are never made inheritable here: this host starts other children from other threads, and one that
        // started while they were would hold them, so a dead stub would not read as end of file. The stub copies them out of
        // this process itself (`--stdio-from-parent`, by value), once it has proved this process serves its control channel.
        let ends = [
            stdin_r.as_handle().as_raw_handle() as usize,
            stdout_w.as_handle().as_raw_handle() as usize,
            stderr_w.as_handle().as_raw_handle() as usize,
        ];
        let mut args = args;
        args.splice(
            1..1,
            [
                "--stdio-from-parent".to_owned(),
                format!("{},{},{}", ends[0], ends[1], ends[2]),
            ],
        );
        let child = spawn_in_job(
            &job,
            &Launch {
                program: stub,
                args: &args,
                env: &environment,
                cwd: Some(Path::new(cwd)),
                stdio: None,
            },
        )
        .with_context(|| format!("starting `kith-stub --resident {command}`"))?;
        let pid = child.id();
        let mut stream = accept(listener, &name, pid, &job)?;
        // The stub has its copies once it has opened the control channel; the host keeps none of its ends, so a stub that is
        // gone reads as end of file here.
        drop((stdin_r, stdout_w, stderr_w));
        stream.set_read_timeout(Some(ANSWER_WITHIN));
        stream.set_write_timeout(Some(ANSWER_WITHIN));
        let (acp, rx) = Acp::attach(
            Box::new(file_of(stdin_w)),
            file_of(stdout_r),
            file_of(stderr_r),
            Proc::Held(Held::new(child, job.clone())),
            Some(pid),
            Some(job),
        );
        Ok((acp, rx, Ctl::new(PipeWire(BufReader::new(stream)))))
    }
}
