//! A warm engine under `kith-stub --resident`, and the control channel the host drives its writer lock with (M3 cutover,
//! designs/2026-11-14-m3-cutover section 1.1). The protocol is `kith_stub::resident`'s: one `take <key> <claim>` /
//! `release <key> <claim>` line, one answer line.
//!
//! The ACP wire does not change: the engine inherits the stub's stdin and stdout, so [`crate::Acp::pid`] is the stub's pid and
//! the stub leads the process group. The control channel is one end of a socketpair; the stub's end goes to the child as an
//! inherited fd (3 or more, as `--ctl-fd` requires) and is closed here as soon as the child is started, so a dead host reads
//! as end-of-file in the stub. Ours is close-on-exec everywhere. Unix only: on Windows the stub takes a named pipe, which
//! the host does not create yet, so [`spawn_resident`] refuses there.

use crate::{Acp, Incoming};
use anyhow::Result;
use std::sync::Arc;
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

#[cfg(unix)]
pub use unix::{Ctl, spawn_resident};

#[cfg(not(unix))]
pub use other::{Ctl, spawn_resident};

#[cfg(unix)]
mod unix {
    use super::{Acp, Arc, Incoming, Release, Result, Take, mpsc};
    use anyhow::Context;
    use std::io::{BufRead, BufReader, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::sync::Mutex;
    use std::time::Duration;

    /// How long a stub gets to answer a line. A take or release is a file lock and a write: a stub that is silent for this
    /// long is wedged, and the channel is dropped (see [`Ctl`]).
    const ANSWER_WITHIN: Duration = Duration::from_secs(10);

    /// The host's end of a resident stub's control channel. Lines go out one at a time and each is answered before the next
    /// (the stub serves in order), so the calls take turns on an inner lock. A call that fails to get its answer (the stub
    /// died, a timeout, an answer for another claim) breaks the channel for good: a late answer could otherwise be read as
    /// the next call's. The caller restarts the process, as it does for a stub that is gone.
    pub struct Ctl {
        io: Mutex<Option<(UnixStream, BufReader<UnixStream>)>>,
    }

    impl Ctl {
        fn new(stream: UnixStream) -> std::io::Result<Self> {
            stream.set_read_timeout(Some(ANSWER_WITHIN))?;
            stream.set_write_timeout(Some(ANSWER_WITHIN))?;
            let reader = BufReader::new(stream.try_clone()?);
            Ok(Self { io: Mutex::new(Some((stream, reader))) })
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

        /// One line out, one line back: (the answer's first word, the reason that follows an `error`). An answer for another
        /// claim, or one that is not a line, is a broken channel.
        fn ask(&self, verb: &str, key: &str, claim: i64) -> std::io::Result<(String, String)> {
            let broken = |why: &str| std::io::Error::new(std::io::ErrorKind::BrokenPipe, why.to_owned());
            if claim < 0 || key.is_empty() || key.contains([' ', '\n', '\r']) {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a key and claim id"));
            }
            let mut guard = self.io.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let (stream, reader) = guard.as_mut().ok_or_else(|| broken("the control channel is closed"))?;
            let mut got = String::new();
            let sent = writeln!(stream, "{verb} {key} {claim}")
                .and_then(|()| stream.flush())
                .and_then(|()| reader.read_line(&mut got));
            let answer = match sent {
                Ok(n) if n > 0 && got.ends_with('\n') => got.trim_end_matches(['\n', '\r']).to_owned(),
                Ok(_) => String::new(),
                Err(e) => {
                    *guard = None;
                    return Err(e);
                }
            };
            let mut words = answer.splitn(3, ' ');
            let (said, for_claim, why) = (words.next().unwrap_or(""), words.next(), words.next().unwrap_or(""));
            // `error - <reason>` (a line the stub could not read) names no claim; every other answer names ours.
            if said.is_empty() || (for_claim != Some(&claim.to_string()) && said != "error") {
                *guard = None;
                return Err(broken("the stub's answer was not for this claim"));
            }
            Ok((said.to_owned(), why.to_owned()))
        }

        /// Whether the channel is still usable (it breaks on the first call that gets no answer).
        pub fn is_open(&self) -> bool {
            self.io.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_some()
        }
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
        let started = Acp::start(process, &format!("kith-stub --resident {command}"), cwd, env);
        // The host keeps no copy of the stub's end, spawned or not: a dead host must read as end-of-file there.
        drop(theirs);
        let (acp, rx) = started?;
        Ok((acp, rx, Ctl::new(ours).context("opening the control channel")?))
    }
}

#[cfg(not(unix))]
mod other {
    use super::{Acp, Arc, Incoming, Release, Result, Take, mpsc};
    use std::path::Path;

    pub struct Ctl;

    impl Ctl {
        pub fn take(&self, _key: &str, _claim: i64) -> std::io::Result<Take> {
            Err(std::io::Error::other("the resident stub needs Unix"))
        }
        pub fn release(&self, _key: &str, _claim: i64) -> std::io::Result<Release> {
            Err(std::io::Error::other("the resident stub needs Unix"))
        }
        pub fn is_open(&self) -> bool {
            false
        }
    }

    pub fn spawn_resident(
        _stub: &Path,
        _lock_dir: &Path,
        _command: &str,
        _cwd: &str,
        _env: &[(String, String)],
    ) -> Result<(Arc<Acp>, mpsc::UnboundedReceiver<Incoming>, Ctl)> {
        anyhow::bail!("a resident stub needs Unix until the named-pipe spawn lands")
    }
}
