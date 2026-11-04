// Adapted from herdr-core b63ecbd (crates/herdr-core/src/platform/unix.rs: connections, spawn_detached, spawn_pty,
// Pty::resize, wait). Copyright herdrdev and the herdr-core contributors, Apache-2.0. See NOTICE.
//
//! Linux and macOS: Unix sockets only this user can open, PTYs, process groups.

use super::{Spec, Started};
use crate::proto::{Exit, Mode};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

// --- Connections -----------------------------------------------------------

pub type Stream = std::os::unix::net::UnixStream;
pub type Listener = std::os::unix::net::UnixListener;

/// A clear error for a socket path the system cannot hold (sun_path is 104 bytes on macOS, 108 on Linux, with the
/// ending NUL), instead of the bare "path must be shorter" the bind or connect would give.
fn check_path_length(path: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let sun_path = unsafe { std::mem::zeroed::<libc::sockaddr_un>() }
        .sun_path
        .len();
    let len = path.as_os_str().as_bytes().len();
    if len >= sun_path {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "the holder's socket path is too long ({len} bytes, at most {} here): {}. Use a shorter folder",
                sun_path - 1,
                path.display()
            ),
        ));
    }
    Ok(())
}

pub fn connect(path: &Path) -> io::Result<Stream> {
    check_path_length(path)?;
    Stream::connect(path)
}

/// Where the holder of `dir` listens.
pub fn endpoint(dir: &Path) -> PathBuf {
    dir.join("holder.sock")
}

/// The socket lives in a folder only this user can enter, and is itself 0600.
pub fn listen(path: &Path) -> io::Result<Listener> {
    use std::os::unix::fs::PermissionsExt;
    check_path_length(path)?;
    let listener = Listener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Waits for a connection from this user. A socket in a private folder already keeps others out; the peer check
/// covers a folder someone made too open.
pub fn accept(listener: &Listener) -> io::Result<Stream> {
    loop {
        let (stream, _) = listener.accept()?;
        if peer_is_me(&stream) {
            return Ok(stream);
        }
    }
}

/// Whether an accept failure is worth retrying: out of descriptors or buffers, a signal, a connection that
/// vanished before it was taken.
pub fn accept_error_is_temporary(e: &io::Error) -> bool {
    use io::ErrorKind::{ConnectionAborted, Interrupted, WouldBlock};
    matches!(e.kind(), Interrupted | ConnectionAborted | WouldBlock)
        || matches!(
            e.raw_os_error(),
            Some(libc::EMFILE | libc::ENFILE | libc::ENOBUFS | libc::ENOMEM | libc::ECONNABORTED)
        )
}

#[cfg(target_os = "linux")]
fn peer_is_me(stream: &Stream) -> bool {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = size_of::<libc::ucred>() as libc::socklen_t;
    let ok = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &mut len,
        )
    };
    ok == 0 && cred.uid == unsafe { libc::geteuid() }
}

#[cfg(not(target_os = "linux"))]
fn peer_is_me(stream: &Stream) -> bool {
    let (mut uid, mut gid) = (0, 0);
    unsafe {
        libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) == 0 && uid == libc::geteuid()
    }
}

pub fn remove_endpoint(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// A directory only this user can enter.
pub fn private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

pub fn random_bytes(bytes: &mut [u8]) {
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(bytes))
        .expect("/dev/urandom readable");
}

/// Starts `cmd` in its own session (closing the terminal it was started from, or killing its process group, won't
/// end it), `input` on its stdin (then closed; it is how the secret travels, see `launch`), stdout to /dev/null,
/// stderr to `log`. Reaped by a thread if it ends while we live.
pub fn spawn_detached(mut cmd: Command, log: File, input: &[u8]) -> io::Result<()> {
    cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(log);
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn()?;
    // The pipe holds it until the child reads, so this does not wait on the child; dropping stdin closes it.
    let sent = child
        .stdin
        .take()
        .map_or(Ok(()), |mut s| s.write_all(input));
    std::thread::spawn(move || child.wait());
    sent
}

/// The holder was started inside a job that ends it with the starter. Never on Unix: it has its own session.
pub fn tied_to_starter() -> bool {
    false
}

// --- Processes -------------------------------------------------------------

/// A held process: the leader of its own session and process group, so the group is the tree.
pub struct Proc {
    pid: u32,
    child: Mutex<Option<Child>>,
    /// The terminal's master side, to resize.
    master: Option<File>,
}

impl Proc {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        if let Some(master) = &self.master {
            let size = libc::winsize {
                ws_row: rows,
                ws_col: cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ as _, &size) };
        }
    }

    /// SIGKILL to the whole group. The pid stays reserved while any member lives, and the leader is not reaped
    /// until `reap`, which takes the same lock: so this never signals a recycled group.
    pub fn kill_tree(&self) {
        let leader = self.child.lock().unwrap_or_else(|p| p.into_inner());
        if leader.is_some() {
            unsafe { libc::killpg(self.pid as i32, libc::SIGKILL) };
        }
    }

    /// True once no process of the group is left (a zombie still counts: it goes when the leader is reaped). Also
    /// asked after the leader is reaped: the group id cannot be handed to a new group while a member lives, so
    /// ESRCH means every member is gone. (The one blind spot: a new, unrelated group that reuses the id after the
    /// whole group ended reads as non-empty; the entry goes at its ack.)
    pub fn tree_empty(&self) -> bool {
        let gone = unsafe { libc::killpg(self.pid as i32, 0) } != 0;
        gone && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }

    /// Blocks until the leader has ended, without reaping it (so its pid, and with it the group, stays ours).
    pub fn wait_exit(&self) {
        loop {
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let r = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.pid as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOWAIT,
                )
            };
            if r == 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                return;
            }
        }
    }

    /// Ends whatever the program left behind. Call after `wait_exit`, before `reap`.
    pub fn sweep(&self) {
        self.kill_tree();
    }

    pub fn reap(&self) -> io::Result<Exit> {
        let mut slot = self.child.lock().unwrap_or_else(|p| p.into_inner());
        let Some(mut child) = slot.take() else {
            return Err(io::Error::other("already reaped"));
        };
        loop {
            match child.wait() {
                Ok(status) => {
                    return Ok(Exit {
                        code: status.code(),
                        signal: status.signal(),
                        error: None,
                        lapsed: false,
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }
}

pub fn spawn(spec: &Spec) -> io::Result<Started> {
    let program = spec
        .argv
        .first()
        .ok_or_else(|| io::Error::other("empty argv"))?;
    let mut cmd = Command::new(program);
    cmd.args(&spec.argv[1..])
        .env_clear()
        .envs(spec.env)
        .current_dir(spec.cwd);
    match spec.mode {
        Mode::Pty => {
            let (mut master, mut slave) = (0, 0);
            let mut size = libc::winsize {
                ws_row: spec.rows,
                ws_col: spec.cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            let opened = unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &raw mut size,
                )
            };
            if opened != 0 {
                return Err(io::Error::last_os_error());
            }
            let (master, slave) = unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) };
            for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
                unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
            }
            cmd.stdin(Stdio::from(slave.try_clone()?))
                .stdout(Stdio::from(slave.try_clone()?))
                .stderr(Stdio::from(slave));
            unsafe {
                cmd.pre_exec(|| {
                    if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let child = cmd.spawn()?;
            // `cmd` still owns the slave fds until it drops: the reader would never see the end otherwise.
            drop(cmd);
            Ok(Started {
                proc: Proc {
                    pid: child.id(),
                    child: Mutex::new(Some(child)),
                    master: Some(master.try_clone()?),
                },
                input: Box::new(master.try_clone()?),
                out: Box::new(master),
                err: None,
            })
        }
        Mode::Pipes => {
            cmd.stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            unsafe {
                cmd.pre_exec(|| {
                    if libc::setsid() < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let mut child = cmd.spawn()?;
            let input: Box<dyn Write + Send> = Box::new(child.stdin.take().expect("piped"));
            let out: Box<dyn Read + Send> = Box::new(child.stdout.take().expect("piped"));
            let err: Box<dyn Read + Send> = Box::new(child.stderr.take().expect("piped"));
            Ok(Started {
                proc: Proc {
                    pid: child.id(),
                    child: Mutex::new(Some(child)),
                    master: None,
                },
                input,
                out,
                err: Some(err),
            })
        }
    }
}

/// Held for the holder's life; the lock goes when the process does.
pub struct InstanceLock(#[allow(dead_code)] File);

/// One holder per folder: an exclusive flock on `holder.lock`, taken before the socket is touched and never
/// released or deleted (the file stays, so no second holder can lock a fresh one). `Ok(None)`: another holds it.
pub fn lock_instance(dir: &Path) -> io::Result<Option<InstanceLock>> {
    // Rust opens with O_CLOEXEC, so the holder's programs do not inherit the lock.
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("holder.lock"))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(Some(InstanceLock(file)));
    }
    let err = io::Error::last_os_error();
    if err.kind() == io::ErrorKind::WouldBlock {
        Ok(None)
    } else {
        Err(err)
    }
}

/// The process on the other end of a connection, as the kernel reports it (Linux `SO_PEERCRED`, macOS
/// `LOCAL_PEERPID`); `None` where it can't be asked. A caller that relies on it treats `None` as "nobody".
#[cfg(target_os = "linux")]
pub fn peer_pid(stream: &Stream) -> Option<u32> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = size_of::<libc::ucred>() as libc::socklen_t;
    let ok = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &mut len,
        )
    };
    (ok == 0).then(|| u32::try_from(cred.pid).ok()).flatten()
}

#[cfg(target_os = "macos")]
pub fn peer_pid(stream: &Stream) -> Option<u32> {
    let mut pid: libc::pid_t = 0;
    let mut len = size_of::<libc::pid_t>() as libc::socklen_t;
    let ok = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&raw mut pid).cast(),
            &mut len,
        )
    };
    (ok == 0).then(|| u32::try_from(pid).ok()).flatten()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn peer_pid(_: &Stream) -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn a_long_socket_path_gets_a_clear_error() {
        let long = Path::new("/tmp").join("x".repeat(120)).join("holder.sock");
        for err in [
            connect(&long).expect_err("connect fails"),
            listen(&long).expect_err("listen fails"),
        ] {
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
            let text = err.to_string();
            assert!(
                text.contains("too long") && text.contains("shorter folder"),
                "{text}"
            );
        }
    }

    /// The exited leader is reaped but a member of its group lives on: the tree is not empty until that ends.
    #[test]
    fn a_reaped_leader_with_a_live_group_is_not_an_empty_tree() {
        let argv = ["sh", "-c", "sleep 300 & exit 0"].map(String::from);
        let env = BTreeMap::new();
        let started = spawn(&Spec {
            argv: &argv,
            env: &env,
            cwd: "/",
            mode: Mode::Pipes,
            cols: 80,
            rows: 24,
        })
        .unwrap();
        let proc = started.proc;
        proc.wait_exit();
        proc.reap().unwrap();
        // No sweep: the backgrounded sleep keeps the group alive.
        assert!(!proc.tree_empty(), "a member of the group is still running");
        unsafe { libc::killpg(proc.pid as i32, libc::SIGKILL) };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !proc.tree_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(proc.tree_empty());
    }
}
