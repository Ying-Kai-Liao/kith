//! Named pipes only this account (and SYSTEM) can open: the host's run channel for `kith permit`.
//!
//! The same rules the core's pipes follow (`core/holder/src/platform/windows.rs`):
//! - The access list is part of `CreateNamedPipeW`, so there is no moment at which the pipe has another one. It names this
//!   account's SID and SYSTEM, nobody else, and carries an integrity label so a lower process of the same account stays out.
//! - The first instance of a name is made with `FILE_FLAG_FIRST_PIPE_INSTANCE`: another process that already made the name
//!   makes this fail, rather than getting our clients.
//! - Remote clients are refused (`PIPE_REJECT_REMOTE_CLIENTS`).
//! - A client opens with identification-level impersonation, so whoever serves the name cannot act as the client.
//!
//! The names are `\\.\pipe\...`: the pipe namespace is one for the whole machine (there is no `Local\` form for it as there
//! is for events), so the caller hashes its folder into the name. The access list, not a check of the client's SID, keeps other
//! accounts out: they cannot open the pipe at all. The server still gets the client's process id for the caller's own checks.
//!
//! I/O is overlapped so a read or a write can have a deadline (the handle is cancelled when it runs out); each call blocks
//! only its own thread.

use std::ffi::{OsStr, c_void};
use std::io::{self, Read, Write};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_OPERATION_ABORTED, ERROR_PIPE_BUSY,
    ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, GetLastError,
    HANDLE, INVALID_HANDLE_VALUE, LocalFree, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
    PIPE_ACCESS_DUPLEX, ReadFile, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId,
    PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
    PIPE_WAIT, WaitNamedPipeW,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, OpenProcess, OpenProcessToken,
    PROCESS_QUERY_LIMITED_INFORMATION, WaitForSingleObject,
};

use crate::sid::{self, Sid};
use crate::{Handle, last_err, wide};

/// One pipe handle (a server instance or a client end). Windows handles can be used from any thread.
struct Pipe(Handle);

// SAFETY: a kernel handle is not tied to a thread; the I/O calls on it are serialised by `&mut` on the owner.
unsafe impl Send for Pipe {}
// SAFETY: as above; `raw` is only read.
unsafe impl Sync for Pipe {}

impl Pipe {
    fn raw(&self) -> HANDLE {
        self.0.0
    }

    /// One overlapped operation, waited for up to `timeout` (for ever when `None`). A broken or closed pipe reads as the
    /// end of the stream (0 bytes), as it does for a socket; a deadline that runs out cancels the call and is `TimedOut`.
    fn io(
        &self,
        timeout: Option<Duration>,
        op: impl FnOnce(HANDLE, *mut OVERLAPPED) -> i32,
    ) -> io::Result<usize> {
        // SAFETY: null attributes and name; a manual-reset event, not signalled.
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            return Err(last_err());
        }
        let event = Handle(event);
        // SAFETY: OVERLAPPED is plain data and all-zero is its starting state.
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.hEvent = event.0;
        if op(self.raw(), &raw mut ov) == 0 {
            // SAFETY: reads the calling thread's last error.
            let err = unsafe { GetLastError() };
            if err != ERROR_IO_PENDING {
                return match err {
                    ERROR_BROKEN_PIPE | ERROR_NO_DATA | ERROR_PIPE_NOT_CONNECTED => Ok(0),
                    _ => Err(crate::win_err(err)),
                };
            }
            let ms = timeout.map_or(u32::MAX, |t| {
                u32::try_from(t.as_millis()).unwrap_or(u32::MAX - 1)
            });
            // SAFETY: `event` is an open event handle.
            let waited = unsafe { WaitForSingleObject(event.0, ms) };
            if waited == WAIT_TIMEOUT {
                // SAFETY: cancels the one call made with `ov`; the wait below ends once the kernel is done with `ov`, which
                // has to happen before it (and the buffer the call was given) goes out of scope.
                unsafe { CancelIoEx(self.raw(), &raw const ov) };
            }
        }
        let mut n = 0u32;
        // SAFETY: `ov` is the one the call was started with; waiting (the last argument) makes it final.
        let done = unsafe { GetOverlappedResult(self.raw(), &raw const ov, &raw mut n, 1) };
        if done == 0 {
            // SAFETY: reads the calling thread's last error.
            return match unsafe { GetLastError() } {
                ERROR_BROKEN_PIPE | ERROR_NO_DATA | ERROR_PIPE_NOT_CONNECTED => Ok(0),
                ERROR_OPERATION_ABORTED => Err(io::ErrorKind::TimedOut.into()),
                code => Err(crate::win_err(code)),
            };
        }
        Ok(n as usize)
    }
}

/// A connection, as a client or as a server's accepted end. Blocking, with an optional deadline on each read and write.
pub struct Stream {
    pipe: Pipe,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
}

impl Stream {
    fn new(pipe: Pipe) -> Self {
        Self {
            pipe,
            read_timeout: None,
            write_timeout: None,
        }
    }

    /// How long one read may wait (`None`: for ever).
    pub fn set_read_timeout(&mut self, t: Option<Duration>) {
        self.read_timeout = t;
    }

    /// How long one write may wait (`None`: for ever).
    pub fn set_write_timeout(&mut self, t: Option<Duration>) {
        self.write_timeout = t;
    }

    /// The process on the other end when this is a server's end (`GetNamedPipeClientProcessId`).
    pub fn peer_pid(&self) -> Option<u32> {
        let mut pid = 0u32;
        // SAFETY: the handle is an open pipe; `pid` is an out-pointer.
        let ok = unsafe { GetNamedPipeClientProcessId(self.pipe.raw(), &raw mut pid) };
        (ok != 0 && pid != 0).then_some(pid)
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        let p = buf.as_mut_ptr();
        self.pipe.io(self.read_timeout, |h, ov| {
            // SAFETY: `p` and `len` describe `buf`, which the call keeps until `io` has waited for it.
            unsafe { ReadFile(h, p, len, std::ptr::null_mut(), ov) }
        })
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        let p = buf.as_ptr();
        let n = self.pipe.io(self.write_timeout, |h, ov| {
            // SAFETY: `p` and `len` describe `buf`, which the call keeps until `io` has waited for it.
            unsafe { WriteFile(h, p, len, std::ptr::null_mut(), ov) }
        })?;
        if n == 0 && !buf.is_empty() {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A security descriptor made from SDDL text, freed on drop, with the `SECURITY_ATTRIBUTES` that point at it.
struct Security {
    sd: *mut c_void,
    attrs: Box<SECURITY_ATTRIBUTES>,
}

// SAFETY: the descriptor is read-only after it is made and is freed once, on drop.
unsafe impl Send for Security {}
// SAFETY: as above.
unsafe impl Sync for Security {}

impl Drop for Security {
    fn drop(&mut self) {
        // SAFETY: `sd` was allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW (LocalAlloc), freed once.
        unsafe { LocalFree(self.sd) };
    }
}

/// Whether this process runs elevated (its token says so, whatever the account is called).
fn elevated() -> io::Result<bool> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: the pseudo handle of this process needs no close; `token` is an out-pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
        return Err(last_err());
    }
    let token = Handle(token);
    let mut e = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut n = 0u32;
    // SAFETY: `e` is the size the call is told.
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            (&raw mut e).cast(),
            u32::try_from(size_of::<TOKEN_ELEVATION>()).unwrap_or(0),
            &raw mut n,
        )
    } == 0
    {
        return Err(last_err());
    }
    Ok(e.TokenIsElevated != 0)
}

/// The access list of our pipes: full control for this account and SYSTEM, and nothing inherited or default; the label keeps
/// processes of a lower integrity level out (an elevated host is not reached from an unelevated process).
fn owner_only() -> io::Result<Security> {
    let me = sid::current_user()?;
    let level = if elevated()? { "HI" } else { "ME" };
    let text = Sid::text(me.as_ptr());
    let sddl = wide(OsStr::new(&format!(
        "D:P(A;;GA;;;SY)(A;;GA;;;{text})S:(ML;;NRNW;;;{level})"
    )));
    let mut sd: *mut c_void = std::ptr::null_mut();
    // SAFETY: `sddl` is NUL-ended; on success `sd` is a LocalAlloc'd descriptor freed by `Security`.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &raw mut sd,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(last_err());
    }
    Ok(Security {
        sd,
        attrs: Box::new(SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(0),
            lpSecurityDescriptor: sd,
            bInheritHandle: 0,
        }),
    })
}

fn new_instance(name: &[u16], first: bool, security: &Security) -> io::Result<Pipe> {
    let flags = PIPE_ACCESS_DUPLEX
        | FILE_FLAG_OVERLAPPED
        | if first {
            FILE_FLAG_FIRST_PIPE_INSTANCE
        } else {
            0
        };
    let mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
    // SAFETY: `name` is NUL-ended; the attributes point at a descriptor that lives as long as `security`.
    let h = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            flags,
            mode,
            PIPE_UNLIMITED_INSTANCES,
            65536,
            65536,
            0,
            &raw const *security.attrs,
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(last_err());
    }
    Ok(Pipe(Handle(h)))
}

/// A pipe name that others connect to. The first instance exists from [`listen`] on, so a client that comes before
/// [`Listener::accept`] waits in the pipe rather than finding no name.
pub struct Listener {
    name: Vec<u16>,
    security: Security,
    next: Mutex<Option<Pipe>>,
}

/// Makes the name (`\\.\pipe\...`) with the owner-only list. Fails when the name already exists: someone else made it first.
pub fn listen(name: &str) -> io::Result<Listener> {
    let name = wide(OsStr::new(name));
    let security = owner_only()?;
    let first = new_instance(&name, true, &security)?;
    Ok(Listener {
        name,
        security,
        next: Mutex::new(Some(first)),
    })
}

impl Listener {
    /// Waits for a client. The next instance is made before this returns, so the name is never without one for long.
    pub fn accept(&self) -> io::Result<Stream> {
        let taken = self
            .next
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let pipe = match taken {
            Some(p) => p,
            None => new_instance(&self.name, false, &self.security)?,
        };
        match pipe.io(None, |h, ov| {
            // SAFETY: `h` is an open overlapped server instance and `ov` is the call's own.
            unsafe { ConnectNamedPipe(h, ov) }
        }) {
            Ok(_) => {}
            // a client that came before we waited is connected already
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_CONNECTED.cast_signed()) => {}
            Err(e) => return Err(e),
        }
        // This client is connected: a failure to make the next instance must not lose it. The slot stays empty and the next
        // accept makes one.
        if let Ok(next) = new_instance(&self.name, false, &self.security) {
            *self
                .next
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(next);
        }
        Ok(Stream::new(pipe))
    }
}

/// Who a client trusts to serve a pipe: the one account it runs as, and the group whose members the pipe may be owned by
/// instead (an elevated process creates its objects owned by `Administrators`). Held apart so a test can name others.
struct Trust {
    ours: Sid,
    admins: Sid,
}

impl Trust {
    fn this_account() -> io::Result<Self> {
        Ok(Self {
            ours: sid::current_user()?,
            admins: sid::administrators()?,
        })
    }
}

/// The rule of [`verify_server`] over what was read, so it is testable without a second account: the server process runs as
/// us, and the pipe says it was made by us or by `Administrators`. The second is what survives a pid that was reused (the
/// process check reads whoever has that pid now). Anything unreadable is a refusal.
fn judge(
    server: io::Result<Sid>,
    owner: io::Result<Sid>,
    trust: &Trust,
) -> Result<(), &'static str> {
    let server = server.map_err(|_| "its account could not be read")?;
    let owner = owner.map_err(|_| "the pipe's owner could not be read")?;
    if !server.is(trust.ours.as_ptr()) {
        return Err("another account made this name first");
    }
    if !(owner.is(trust.ours.as_ptr()) || owner.is(trust.admins.as_ptr())) {
        return Err("the pipe belongs to another account");
    }
    Ok(())
}

/// Refuses a server that is not running as this account and a pipe that is not owned by it (`PermissionDenied`, naming why),
/// and one whose account cannot be read: a pipe this client cannot vouch for is never written to.
fn verify_server(pipe: &Pipe, trust: &Trust) -> io::Result<()> {
    let refuse = |why: &str| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "kith-winsys: the pipe's server is not this account's ({why}); nothing was sent"
            ),
        )
    };
    let mut pid = 0u32;
    // SAFETY: the handle is an open pipe; `pid` is an out-pointer.
    if unsafe { GetNamedPipeServerProcessId(pipe.raw(), &raw mut pid) } == 0 || pid == 0 {
        return Err(refuse("its process could not be found"));
    }
    // SAFETY: a query-only handle on that process, closed by `Handle`.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    let server = if process.is_null() {
        Err(last_err())
    } else {
        let process = Handle(process);
        sid::process_user(process.0)
    };
    judge(server, sid::object_owner(pipe.raw()), trust).map_err(refuse)
}

/// Opens the pipe `name` as a client, waiting up to `timeout` while every instance is busy. `NotFound` when nobody listens.
pub fn connect(name: &str, timeout: Duration) -> io::Result<Stream> {
    connect_trusting(name, timeout, &Trust::this_account()?)
}

fn connect_trusting(name: &str, timeout: Duration, trust: &Trust) -> io::Result<Stream> {
    let wname = wide(OsStr::new(name));
    let deadline = Instant::now() + timeout;
    loop {
        // SAFETY: `wname` is NUL-ended. Identification only: whoever serves this name cannot act as us.
        let h = unsafe {
            CreateFileW(
                wname.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                std::ptr::null_mut(),
            )
        };
        if h != INVALID_HANDLE_VALUE {
            let pipe = Pipe(Handle(h));
            // Before any byte is written: whoever made the name first is not necessarily the host. Another account can
            // create the name before the host does (the list on a pipe keeps others from opening it, not from serving it).
            verify_server(&pipe, trust)?;
            return Ok(Stream::new(pipe));
        }
        let e = last_err();
        let left = deadline.saturating_duration_since(Instant::now());
        if e.raw_os_error() != Some(ERROR_PIPE_BUSY.cast_signed()) || left.is_zero() {
            return Err(
                if left.is_zero() && e.raw_os_error() == Some(ERROR_PIPE_BUSY.cast_signed()) {
                    io::ErrorKind::TimedOut.into()
                } else {
                    e
                },
            );
        }
        // every instance is busy: one frees up as soon as the server accepts
        let ms = u32::try_from(left.as_millis().min(100)).unwrap_or(100);
        // SAFETY: `wname` is NUL-ended.
        unsafe { WaitNamedPipeW(wname.as_ptr(), ms) };
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn unique(tag: &str) -> String {
        format!(r"\\.\pipe\kith-winsys-test-{tag}-{}", std::process::id())
    }

    #[test]
    fn a_line_goes_to_the_server_and_an_answer_comes_back() {
        let name = unique("echo");
        let l = Arc::new(listen(&name).unwrap());
        let server = {
            let l = l.clone();
            std::thread::spawn(move || {
                let mut s = l.accept().unwrap();
                assert_eq!(s.peer_pid(), Some(std::process::id()));
                let mut buf = [0u8; 16];
                let n = s.read(&mut buf).unwrap();
                s.write_all(&buf[..n]).unwrap();
            })
        };
        let mut c = connect(&name, Duration::from_secs(5)).unwrap();
        c.write_all(b"hello\n").unwrap();
        let mut got = [0u8; 16];
        let n = c.read(&mut got).unwrap();
        assert_eq!(&got[..n], b"hello\n");
        server.join().unwrap();
    }

    #[test]
    fn a_second_listener_on_the_name_is_refused() {
        let name = unique("first");
        let _l = listen(&name).unwrap();
        // FILE_FLAG_FIRST_PIPE_INSTANCE: the name is taken, so squatting on it fails instead of sharing it
        assert!(listen(&name).is_err());
    }

    #[test]
    fn a_read_with_a_deadline_gives_up_and_the_stream_stays_usable() {
        let name = unique("slow");
        let l = Arc::new(listen(&name).unwrap());
        let server = {
            let l = l.clone();
            std::thread::spawn(move || {
                let mut s = l.accept().unwrap();
                std::thread::sleep(Duration::from_millis(600));
                s.write_all(b"late").unwrap();
            })
        };
        let mut c = connect(&name, Duration::from_secs(5)).unwrap();
        c.set_read_timeout(Some(Duration::from_millis(100)));
        let mut buf = [0u8; 8];
        assert_eq!(
            c.read(&mut buf).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        c.set_read_timeout(Some(Duration::from_secs(5)));
        let n = c.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"late");
        server.join().unwrap();
    }

    fn trust_of(ours: Sid, admins: Sid) -> Trust {
        Trust { ours, admins }
    }

    fn unreadable() -> io::Result<Sid> {
        Err(io::Error::other("unreadable"))
    }

    #[test]
    fn the_judgement_refuses_another_account_and_what_it_cannot_read() {
        let me = || sid::current_user().unwrap();
        let admins = || sid::administrators().unwrap();
        // SYSTEM stands in for the foreign account: a SID that is well formed and is not ours
        let other = || sid::system().unwrap();
        let t = trust_of(me(), admins());
        assert!(judge(Ok(me()), Ok(me()), &t).is_ok());
        // an elevated maker owns its objects as Administrators
        assert!(judge(Ok(me()), Ok(admins()), &t).is_ok());
        assert!(judge(Ok(other()), Ok(me()), &t).is_err());
        // a reused pid: the process now is ours, the pipe is not
        assert!(judge(Ok(me()), Ok(other()), &t).is_err());
        assert!(judge(unreadable(), Ok(me()), &t).is_err());
        assert!(judge(Ok(me()), unreadable(), &t).is_err());
    }

    #[test]
    fn an_impostor_server_is_refused_before_a_byte_is_written() {
        // The server is this process (so a real pipe, a real server pid and a real owner); the client is told to trust
        // SYSTEM only, which makes the server an impostor to it. The server records every byte it is sent.
        let name = unique("impostor");
        let l = Arc::new(listen(&name).unwrap());
        let got = Arc::new(Mutex::new(Vec::new()));
        let server = {
            let (l, got) = (l.clone(), got.clone());
            std::thread::spawn(move || {
                let mut s = l.accept().unwrap();
                let mut buf = [0u8; 64];
                // the client drops its end after the refusal: end of stream, with nothing before it
                while let Ok(n) = s.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    got.lock().unwrap().extend_from_slice(&buf[..n]);
                }
            })
        };
        let system = || sid::system().unwrap();
        let e = connect_trusting(&name, Duration::from_secs(5), &trust_of(system(), system()))
            .err()
            .unwrap();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        assert!(e.to_string().contains("nothing was sent"), "{e}");
        server.join().unwrap();
        assert!(got.lock().unwrap().is_empty());
    }

    #[test]
    fn the_pipe_reports_its_owner() {
        // The owner read back from a live pipe is ours or Administrators (an elevated runner), never unreadable.
        let name = unique("owner");
        let l = Arc::new(listen(&name).unwrap());
        let server = {
            let l = l.clone();
            std::thread::spawn(move || drop(l.accept().unwrap()))
        };
        let c = connect(&name, Duration::from_secs(5)).unwrap();
        let owner = sid::object_owner(c.pipe.raw()).unwrap();
        assert!(
            owner.is(sid::current_user().unwrap().as_ptr())
                || owner.is(sid::administrators().unwrap().as_ptr())
        );
        server.join().unwrap();
    }

    #[test]
    fn a_server_of_our_own_account_is_accepted() {
        // the server here is this process: the same account, so the client goes on
        let name = unique("verify");
        let l = Arc::new(listen(&name).unwrap());
        let server = {
            let l = l.clone();
            std::thread::spawn(move || drop(l.accept().unwrap()))
        };
        assert!(connect(&name, Duration::from_secs(5)).is_ok());
        server.join().unwrap();
    }

    #[test]
    fn no_listener_is_not_found() {
        let e = connect(&unique("nobody"), Duration::from_millis(200))
            .err()
            .unwrap();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn the_pipe_is_made_with_an_owner_only_list() {
        // Read back from the live pipe, not from the text it was made from: the list is the one it was created with.
        let name = unique("acl");
        let _l = listen(&name).unwrap();
        assert_eq!(
            crate::acl::check(std::path::Path::new(&name)).unwrap(),
            crate::acl::Check::OwnerOnly
        );
    }
}
