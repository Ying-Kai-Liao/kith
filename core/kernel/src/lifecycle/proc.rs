//! The process that will be claude, parked before its exec (claim-then-exec, as a one-shot does in `kernel/bin/kith`): the
//! compaction names its pid in the writer claim before claude starts, so a sweep that dies while claude writes leaves the
//! claim held by the live writer. A port of `parked`, `reap`, `lock_stub` and `stub_word` of `kernel/bin/kith-lifecycle`.
//!
//! The fork is raw: Rust's `Command` waits for the exec before it returns, and the pid is wanted first. The child does only
//! what is safe after a fork (`setpgid`, `read`, `chdir`, `dup2`, `execve`, `write`, `_exit`) on strings made before it.
#![allow(unsafe_code)]

use std::ffi::{CString, OsStr, OsString};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::paths::Env;

/// How long a compaction may run before claude is killed.
pub const COMPACT_TIMEOUT: Duration = Duration::from_secs(900);

/// A parked child: write `1` to `go` to start it, close `go` to make it exit unrun; an exec that fails writes why to `err`.
pub struct Parked {
    pub pid: i32,
    pub go: RawFd,
    pub err: RawFd,
    /// How Python names each file `execvpe` tried, for the parent to say which one failed ([`Parked::why`]).
    shown: Vec<Vec<u8>>,
}

impl Parked {
    /// What the child wrote to `err` when its exec failed (its errno and the step, nothing else: the child may not allocate),
    /// as the Python's exception text: `[Errno 2] No such file or directory: '/x/claude'`. Empty when it said nothing.
    pub fn why(&self, said: &[u8]) -> String {
        let (Some(e), Some(at)) = (said.get(..4), said.get(4..12)) else {
            return String::from_utf8_lossy(said).into_owned();
        };
        let errno = i32::from_le_bytes([e[0], e[1], e[2], e[3]]);
        let at = u64::from_le_bytes([at[0], at[1], at[2], at[3], at[4], at[5], at[6], at[7]]);
        let shown = usize::try_from(at)
            .ok()
            .and_then(|i| self.shown.get(i))
            .map_or_else(
                || "''".to_owned(),
                |b| String::from_utf8_lossy(b).into_owned(),
            );
        // SAFETY: strerror returns a NUL-terminated string that stays valid until the next call; it is copied at once.
        let text = unsafe { std::ffi::CStr::from_ptr(libc::strerror(errno)) }
            .to_string_lossy()
            .into_owned();
        format!("[Errno {errno}] {text}: {shown}")
    }
}

fn cstr(s: &OsStr) -> CString {
    CString::new(
        s.as_bytes()
            .iter()
            .copied()
            .filter(|b| *b != 0)
            .collect::<Vec<u8>>(),
    )
    .unwrap_or_default()
}

fn cloexec(fd: RawFd) {
    // SAFETY: fcntl on a descriptor this module just made.
    unsafe {
        let f = libc::fcntl(fd, libc::F_GETFD);
        libc::fcntl(fd, libc::F_SETFD, f | libc::FD_CLOEXEC);
    }
}

/// A pipe whose ends do not survive an exec (PEP 446's default in the Python).
pub fn pipe() -> std::io::Result<(RawFd, RawFd)> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: `fds` has room for the two descriptors pipe(2) fills.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    cloexec(fds[0]);
    cloexec(fds[1]);
    Ok((fds[0], fds[1]))
}

pub fn close(fd: RawFd) {
    // SAFETY: closing a descriptor this module owns; a second close only fails.
    unsafe { libc::close(fd) };
}

/// Python's `repr` of a str: quoted with `'`, unless it holds one and no `"`.
fn py_repr(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::from(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

/// The names `execvpe` tries for `file` and how Python reports each: a path with a directory part is tried alone (a `str`),
/// a bare name along `PATH` of the child's env (`bytes`: `b'/usr/bin/claude'`).
fn candidates(file: &OsStr, path: &OsStr) -> Vec<(CString, Vec<u8>)> {
    let name = |p: &Path, bytes: bool| -> Vec<u8> {
        let shown = p.to_string_lossy();
        if bytes {
            format!("b{}", py_repr(&shown)).into_bytes()
        } else {
            py_repr(&shown).into_bytes()
        }
    };
    if Path::new(file)
        .parent()
        .is_some_and(|p| !p.as_os_str().is_empty())
    {
        let p = PathBuf::from(file);
        return vec![(cstr(p.as_os_str()), name(&p, false))];
    }
    let dirs: Vec<PathBuf> = if path.is_empty() {
        vec![PathBuf::from("/bin"), PathBuf::from("/usr/bin")]
    } else {
        std::env::split_paths(path).collect()
    };
    dirs.iter()
        .map(|d| {
            let p = d.join(file);
            (cstr(p.as_os_str()), name(&p, true))
        })
        .collect()
}

fn write_all(fd: RawFd, bytes: &[u8]) {
    // SAFETY: write(2) of a live buffer; a short write on a pipe to our parent is not retried.
    unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// Forks the process that will be claude: it execs `argv` (in `cwd`, with `env`, stdio on /dev/null) when `1` is written to
/// `go`, and exits without running anything when `go` closes unwritten (also when this process dies). Its pid is claude's
/// (or the kith-stub's that runs claude). `group`: it leads a process group of its own, which [`reap`] kills as a whole.
/// `keep`: a descriptor that survives the exec (kith-stub's status pipe).
pub fn parked(
    argv: &[OsString],
    cwd: &Path,
    env: &[(OsString, OsString)],
    group: bool,
    keep: Option<RawFd>,
) -> std::io::Result<Parked> {
    let (go_r, go_w) = pipe()?;
    let (err_r, err_w) = pipe()?;
    let cargs: Vec<CString> = argv.iter().map(|a| cstr(a)).collect();
    let mut argv_ptrs: Vec<*const libc::c_char> = cargs.iter().map(|a| a.as_ptr()).collect();
    argv_ptrs.push(std::ptr::null());
    let cenv: Vec<CString> = env
        .iter()
        .map(|(k, v)| {
            let mut kv = k.as_bytes().to_vec();
            kv.push(b'=');
            kv.extend_from_slice(v.as_bytes());
            cstr(OsStr::from_bytes(&kv))
        })
        .collect();
    let mut envp: Vec<*const libc::c_char> = cenv.iter().map(|a| a.as_ptr()).collect();
    envp.push(std::ptr::null());
    let path = env
        .iter()
        .find(|(k, _)| k == "PATH")
        .map_or_else(OsString::new, |(_, v)| v.clone());
    let tries = candidates(&argv[0], &path);
    let cwd_c = cstr(cwd.as_os_str());
    // Everything the child needs is made here: it may not allocate (the host is multi-threaded) or look anything up.
    // SAFETY: open(2) of a constant path.
    let null = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    // SAFETY: the child below calls only async-signal-safe functions on data made above, then execs or `_exit`s.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        let e = std::io::Error::last_os_error();
        for fd in [go_r, go_w, err_r, err_w, null] {
            close(fd);
        }
        return Err(e);
    }
    if pid == 0 {
        // SAFETY: see above: the child's whole life.
        unsafe {
            libc::close(go_w);
            libc::close(err_r);
            if group {
                libc::setpgid(0, 0);
            }
            if let Some(fd) = keep {
                let f = libc::fcntl(fd, libc::F_GETFD);
                libc::fcntl(fd, libc::F_SETFD, f & !libc::FD_CLOEXEC);
            }
            let mut byte = [0u8; 1];
            if libc::read(go_r, byte.as_mut_ptr().cast(), 1) == 1 && byte[0] == b'1' {
                // (errno, index of the name tried; MAX for the chdir)
                let mut first: Option<(i32, u64)> = None;
                let mut last: Option<(i32, u64)> = None;
                if libc::chdir(cwd_c.as_ptr()) != 0 {
                    first = Some((errno(), u64::MAX));
                } else {
                    for fd in 0..3 {
                        libc::dup2(null, fd);
                    }
                    for (i, (full, _)) in tries.iter().enumerate() {
                        libc::execve(full.as_ptr(), argv_ptrs.as_ptr(), envp.as_ptr());
                        let e = errno();
                        last = Some((e, i as u64));
                        // execvpe's rule: the first error that is not "not there" wins, else the last
                        if first.is_none() && e != libc::ENOENT && e != libc::ENOTDIR {
                            first = Some((e, i as u64));
                        }
                    }
                }
                if let Some((e, at)) = first.or(last) {
                    let mut said = [0u8; 12];
                    said[..4].copy_from_slice(&e.to_le_bytes());
                    said[4..].copy_from_slice(&at.to_le_bytes());
                    write_all(err_w, &said);
                }
            }
            libc::_exit(127);
        }
    }
    close(go_r);
    close(err_w);
    close(null);
    Ok(Parked {
        pid,
        go: go_w,
        err: err_r,
        shown: tries.into_iter().map(|t| t.1).collect(),
    })
}

/// Reads until the descriptor ends.
pub fn read_to_end(fd: RawFd) -> Vec<u8> {
    let mut all = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        // SAFETY: read(2) into a live buffer.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            return all;
        }
        all.extend_from_slice(&buf[..usize::try_from(n).unwrap_or(0)]);
    }
}

pub fn write_byte(fd: RawFd, b: u8) {
    write_all(fd, &[b]);
}

fn wait(pid: i32, flags: i32) -> Option<i32> {
    let mut status = 0;
    loop {
        // SAFETY: waitpid(2) on our own child.
        let got = unsafe { libc::waitpid(pid, &raw mut status, flags) };
        if got < 0 && errno() == libc::EINTR {
            continue;
        }
        if got <= 0 {
            return None;
        }
        return Some(if libc::WIFEXITED(status) {
            libc::WEXITSTATUS(status)
        } else if libc::WIFSIGNALED(status) {
            -libc::WTERMSIG(status)
        } else {
            status
        });
    }
}

/// Waits for `pid`: its exit code (negative: the signal), or `None` when it ran past `timeout` (it is then killed and
/// reaped). `group`: `pid` leads its own process group; the kill takes the group, so a kith-stub in front does not leave
/// claude running without the lock, and the claim is given back only once claude has left the process table.
pub fn reap(pid: i32, timeout: Option<Duration>, group: bool) -> Option<i32> {
    let end = timeout.map(|t| Instant::now() + t);
    loop {
        if let Some(rc) = wait(pid, if end.is_some() { libc::WNOHANG } else { 0 }) {
            return Some(rc);
        }
        let Some(end) = end else {
            // a blocking wait that found no child: nothing to wait for
            return Some(0);
        };
        if Instant::now() >= end {
            // SAFETY: SIGKILL to our own child (or the group it leads).
            unsafe {
                if group {
                    libc::killpg(pid, libc::SIGKILL);
                } else {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
            wait(pid, 0);
            if group {
                for _ in 0..100 {
                    // SAFETY: signal 0 only asks whether the group has a member.
                    if unsafe { libc::killpg(pid, 0) } != 0 {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            return None;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        && CString::new(p.as_os_str().as_bytes()).is_ok_and(|c| {
            // SAFETY: access(2) on a NUL-free path.
            unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 }
        })
}

/// `shutil.which`: the first executable file named `name` on `PATH` (or `name` itself when it has a directory part).
pub fn which(name: &str, env: Env) -> Option<PathBuf> {
    if name.contains('/') {
        return executable(Path::new(name)).then(|| PathBuf::from(name));
    }
    let path = env("PATH").unwrap_or_else(|| "/usr/bin:/bin".to_owned());
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| executable(p))
}

/// The conversation's writer lock, as `kernel/bin/kith` takes it for a run with a session id: the run starts under
/// `kith-stub --lock-dir <machine>/core/locks --key SID --`, which holds the lock until claude exits. The program is
/// `KITH_STUB_BIN`, else beside the kith program (where it really lives first, then as given). No program, or an id
/// kith-stub's keys cannot hold: no prefix, and the second value says so (a warning; `KITH_REQUIRE_STUB=1` makes it a
/// refusal).
pub fn lock_stub(
    sid: &str,
    machine: Option<&Path>,
    kith_bin: Option<&Path>,
    env: Env,
) -> (Vec<OsString>, Option<String>) {
    if sid.is_empty()
        || !sid.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
    {
        return (
            Vec::new(),
            Some(format!(
                "session id \"{sid}\" cannot be locked (only a-z 0-9 . _ - are allowed)"
            )),
        );
    }
    let mut stub = env("KITH_STUB_BIN")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    if stub.is_none()
        && let Some(kbin) = kith_bin
    {
        let real = crate::paths::realpath(kbin);
        for d in [real.parent(), kbin.parent()].into_iter().flatten() {
            let c = d.join("kith-stub");
            if executable(&c) {
                stub = Some(c);
                break;
            }
        }
    }
    let Some(stub) = stub.filter(|s| executable(s)) else {
        return (
            Vec::new(),
            Some(
                "no kith-stub program beside kith, so this conversation cannot be locked"
                    .to_owned(),
            ),
        );
    };
    let machine = machine.map(Path::to_path_buf).unwrap_or_default();
    (
        vec![
            stub.into_os_string(),
            "--lock-dir".into(),
            machine.join("core").join("locks").into_os_string(),
            "--key".into(),
            sid.into(),
            "--".into(),
        ],
        None,
    )
}

/// What kith-stub wrote on its status pipe before it ran claude: `locked`, `busy`, or empty (it hit a lock error).
pub fn stub_word(fd: RawFd) -> String {
    let mut buf = [0u8; 64];
    // SAFETY: read(2) into a live buffer.
    let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf8_lossy(&buf[..usize::try_from(n).unwrap_or(0)])
        .trim()
        .to_owned()
}
