// Adapted from herdr-core b63ecbd (crates/herdr-core/src/platform/windows.rs: the overlapped named-pipe Stream and
// Listener, pipe_security, spawn_detached, ConPTY loading and spawn_pty, command_line/batch_line/resolve/quote,
// env_block). Copyright herdrdev and the herdr-core contributors, Apache-2.0. See NOTICE.
// Kith's own: one job object per spawn that is the tree (kill, "empty" check), the pipes-mode spawn, endpoint naming.
//
//! Windows: named pipes only this account can open, ConPTY, one job object per held process, detaching out of the
//! parent's job.

use super::{Spec, Started};
use crate::proto::{Exit, Mode};
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::fs::File;
use std::io::{self, Read, Write};
use std::mem::{size_of, zeroed};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::Console::*;
use windows_sys::Win32::System::IO::*;
use windows_sys::Win32::System::JobObjects::*;
use windows_sys::Win32::System::Pipes::*;
use windows_sys::Win32::System::Threading::*;
use windows_sys::core::BOOL;

fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    s.encode_wide().chain(Some(0)).collect()
}

fn check(ok: BOOL) -> io::Result<()> {
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

// --- Connections: named pipes in overlapped mode ---------------------------
//
// Synchronous I/O on one pipe is serialized by Windows: a read waiting in one thread would hold up every write from
// another. Overlapped handles let both run at once; each call still blocks its own thread until done.

struct Pipe {
    handle: OwnedHandle,
}

impl Pipe {
    fn raw(&self) -> HANDLE {
        self.handle.as_raw_handle() as HANDLE
    }

    /// One overlapped operation, waited for.
    fn io(&self, op: impl FnOnce(HANDLE, *mut OVERLAPPED) -> BOOL) -> io::Result<usize> {
        unsafe {
            let event = CreateEventW(null(), 1, 0, null());
            if event.is_null() {
                return Err(io::Error::last_os_error());
            }
            let event = OwnedHandle::from_raw_handle(event as RawHandle);
            let mut ov: OVERLAPPED = zeroed();
            ov.hEvent = event.as_raw_handle() as HANDLE;
            if op(self.raw(), &mut ov) == 0 {
                let err = GetLastError();
                if err != ERROR_IO_PENDING {
                    return match err {
                        ERROR_BROKEN_PIPE | ERROR_NO_DATA | ERROR_PIPE_NOT_CONNECTED => Ok(0),
                        _ => Err(io::Error::from_raw_os_error(err as i32)),
                    };
                }
            }
            let mut n = 0u32;
            if GetOverlappedResult(self.raw(), &ov, &mut n, 1) == 0 {
                return match GetLastError() {
                    ERROR_BROKEN_PIPE
                    | ERROR_NO_DATA
                    | ERROR_PIPE_NOT_CONNECTED
                    | ERROR_OPERATION_ABORTED => Ok(0),
                    err => Err(io::Error::from_raw_os_error(err as i32)),
                };
            }
            Ok(n as usize)
        }
    }

    fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.io(|h, ov| unsafe {
            ReadFile(
                h,
                buf.as_mut_ptr(),
                buf.len().min(u32::MAX as usize) as u32,
                null_mut(),
                ov,
            )
        })
    }

    fn write(&self, buf: &[u8]) -> io::Result<usize> {
        let n = self.io(|h, ov| unsafe {
            WriteFile(
                h,
                buf.as_ptr(),
                buf.len().min(u32::MAX as usize) as u32,
                null_mut(),
                ov,
            )
        })?;
        if n == 0 && !buf.is_empty() {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        Ok(n)
    }
}

/// A connection to a holder (blocking, safe to read and write from different threads at once).
#[derive(Clone)]
pub struct Stream(Arc<Pipe>);

impl Stream {
    pub fn try_clone(&self) -> io::Result<Stream> {
        Ok(self.clone())
    }

    /// Pipes can't close one direction: either way ends the connection, and wakes any read waiting on it.
    pub fn shutdown(&self, _: std::net::Shutdown) -> io::Result<()> {
        unsafe {
            CancelIoEx(self.0.raw(), null());
            DisconnectNamedPipe(self.0.raw());
        }
        Ok(())
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn connect(path: &Path) -> io::Result<Stream> {
    let name = wide(path.as_os_str());
    for _ in 0..50 {
        let h = unsafe {
            // Identification only: whoever serves this name can't act as us.
            let flags = FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION;
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                flags,
                null_mut(),
            )
        };
        if h != INVALID_HANDLE_VALUE {
            return Ok(Stream(Arc::new(Pipe {
                handle: unsafe { OwnedHandle::from_raw_handle(h as RawHandle) },
            })));
        }
        // Every instance busy: one frees up as soon as the server accepts.
        if unsafe { GetLastError() } != ERROR_PIPE_BUSY {
            return Err(io::Error::last_os_error());
        }
        unsafe { WaitNamedPipeW(name.as_ptr(), 100) };
    }
    Err(io::Error::from(io::ErrorKind::TimedOut))
}

/// The pipe of the holder whose folder is `dir`: pipe names are global, so the folder is hashed into it.
pub fn endpoint(dir: &Path) -> PathBuf {
    let mut hash = 0xcbf29ce484222325u64;
    for b in dir.to_string_lossy().to_lowercase().bytes() {
        hash = (hash ^ u64::from(b)).wrapping_mul(0x100000001b3);
    }
    PathBuf::from(format!(r"\\.\pipe\kith-holder-{hash:016x}"))
}

/// Who may open our pipes: this account and SYSTEM (the default also lets everyone read, and names Administrators
/// as owner when elevated). The integrity label keeps lower processes out.
fn pipe_security() -> io::Result<*const SECURITY_ATTRIBUTES> {
    use windows_sys::Win32::Security::Authorization::*;
    use windows_sys::Win32::Security::*;
    static MADE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    if let Some(sa) = MADE.get() {
        return Ok(*sa as *const SECURITY_ATTRIBUTES);
    }
    unsafe {
        let mut token = null_mut();
        check(OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY,
            &mut token,
        ))?;
        let token = OwnedHandle::from_raw_handle(token as RawHandle);
        let mut n = 0u32;
        GetTokenInformation(
            token.as_raw_handle() as HANDLE,
            TokenUser,
            null_mut(),
            0,
            &mut n,
        );
        let mut user = vec![0usize; (n as usize).div_ceil(size_of::<usize>())];
        check(GetTokenInformation(
            token.as_raw_handle() as HANDLE,
            TokenUser,
            user.as_mut_ptr().cast(),
            n,
            &mut n,
        ))?;
        let mut sid = null_mut();
        check(ConvertSidToStringSidW(
            (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid,
            &mut sid,
        ))?;
        let sid_text = String::from_utf16_lossy(std::slice::from_raw_parts(
            sid,
            (0..).take_while(|&i| *sid.add(i) != 0).count(),
        ));
        LocalFree(sid.cast());
        let mut elevation: TOKEN_ELEVATION = zeroed();
        check(GetTokenInformation(
            token.as_raw_handle() as HANDLE,
            TokenElevation,
            (&raw mut elevation).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut n,
        ))?;
        let level = if elevation.TokenIsElevated != 0 {
            "HI"
        } else {
            "ME"
        };
        let sddl =
            wide(format!("D:P(A;;GA;;;SY)(A;;GA;;;{sid_text})S:(ML;;NRNW;;;{level})").as_ref());
        let mut sd = null_mut();
        check(ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            null_mut(),
        ))?;
        // Made once, kept for the life of the process.
        let sa = Box::leak(Box::new(SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd,
            bInheritHandle: 0,
        }));
        Ok(*MADE.get_or_init(|| sa as *const _ as usize) as *const SECURITY_ATTRIBUTES)
    }
}

fn new_instance(name: &[u16], first: bool) -> io::Result<Pipe> {
    let flags = PIPE_ACCESS_DUPLEX
        | FILE_FLAG_OVERLAPPED
        | if first {
            FILE_FLAG_FIRST_PIPE_INSTANCE
        } else {
            0
        };
    let mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
    let h = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            flags,
            mode,
            PIPE_UNLIMITED_INSTANCES,
            65536,
            65536,
            0,
            pipe_security()?,
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(Pipe {
        handle: unsafe { OwnedHandle::from_raw_handle(h as RawHandle) },
    })
}

/// A pipe name others connect to (blocking accepts).
pub struct Listener {
    name: Vec<u16>,
    next: Mutex<Option<Pipe>>,
}

pub fn listen(path: &Path) -> io::Result<Listener> {
    let name = wide(path.as_os_str());
    let first = new_instance(&name, true)?;
    Ok(Listener {
        name,
        next: Mutex::new(Some(first)),
    })
}

pub fn accept(listener: &Listener) -> io::Result<Stream> {
    let pipe = match listener
        .next
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take()
    {
        Some(pipe) => pipe,
        None => new_instance(&listener.name, false)?,
    };
    match pipe.io(|h, ov| unsafe { ConnectNamedPipe(h, ov) }) {
        Ok(_) => {}
        // A client that came before we waited is connected already.
        Err(e) if e.raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32) => {}
        Err(e) => return Err(e),
    }
    // This client is connected already: a failure to make the next instance must not lose it. The slot stays
    // empty and the next accept tries again (and is retried like any temporary failure).
    if let Ok(next) = new_instance(&listener.name, false) {
        *listener.next.lock().unwrap_or_else(|p| p.into_inner()) = Some(next);
    }
    Ok(Stream(Arc::new(pipe)))
}

/// Whether an accept failure is worth retrying: out of memory or handles, a client that vanished mid-connect.
pub fn accept_error_is_temporary(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::Interrupted
        || matches!(
            e.raw_os_error().map(|c| c as u32),
            Some(
                ERROR_NOT_ENOUGH_MEMORY
                    | ERROR_TOO_MANY_OPEN_FILES
                    | ERROR_NO_SYSTEM_RESOURCES
                    | ERROR_PIPE_BUSY
                    | ERROR_NO_DATA
                    | ERROR_BROKEN_PIPE
                    | ERROR_OPERATION_ABORTED
            )
        )
}

/// Pipe names vanish with their last handle.
pub fn remove_endpoint(_: &Path) {}

/// The folder holds only the holder's log (the secret is never written down); it sits in the user's profile.
pub fn private_dir(path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path)
}

pub fn random_bytes(bytes: &mut [u8]) {
    use windows_sys::Win32::Security::Cryptography::{
        BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
    };
    let status = unsafe {
        BCryptGenRandom(
            null_mut(),
            bytes.as_mut_ptr(),
            bytes.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    assert_eq!(status, 0, "BCryptGenRandom failed");
}

// --- Command lines ---------------------------------------------------------

/// `argv` as Windows starts it: the program found on PATH with PATHEXT, batch files through cmd (npm installs
/// agents as `.cmd` shims).
fn command_line(argv: &[String], env: &BTreeMap<String, String>) -> io::Result<Vec<u16>> {
    let program = argv.first().ok_or_else(|| io::Error::other("empty argv"))?;
    let path = env
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("PATH"))
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    let exts = env
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("PATHEXT"))
        .map_or(".COM;.EXE;.BAT;.CMD".into(), |(_, v)| v.clone());
    let found = resolve(program, &path, &exts).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("{program}: not found on PATH"),
        )
    })?;
    let ext = found
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if ext == "cmd" || ext == "bat" {
        return batch_line(&found, &argv[1..]).map(|l| wide(l.as_ref()));
    }
    let line: Vec<String> = std::iter::once(found.display().to_string())
        .chain(argv[1..].iter().cloned())
        .map(|a| quote(&a))
        .collect();
    Ok(wide(line.join(" ").as_ref()))
}

/// A .cmd/.bat (how npm installs agents) runs through cmd, which reads `& | < > ^ %` itself: each argument is
/// quoted, `"` doubled and `%` defused, so a prompt arrives as one literal argument (the scheme Rust's std uses
/// since BatBadBut). cmd can't carry a line break in an argument.
fn batch_line(script: &Path, args: &[String]) -> io::Result<String> {
    let mut line = format!("cmd.exe /e:ON /v:OFF /d /c \"\"{}\"", script.display());
    for arg in args {
        if arg.contains(['\r', '\n', '\0']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{} is a batch file: it can't take an argument with a line break",
                    script.display()
                ),
            ));
        }
        let plain = |c: char| {
            c.is_ascii_alphanumeric()
                || r"#$*+-./:?@\_".contains(c)
                || !c.is_ascii() && !c.is_control()
        };
        let quoted = arg.is_empty() || arg.ends_with('\\') || !arg.chars().all(plain);
        line.push(' ');
        if quoted {
            line.push('"');
        }
        let mut slashes = 0;
        for c in arg.chars() {
            match c {
                '\\' => slashes += 1,
                '"' => {
                    line.push_str(&"\\".repeat(slashes));
                    line.push('"'); // doubled
                    slashes = 0;
                }
                // `%cd:~,%` expands to nothing, so `%VAR%` can't pair up.
                '%' => {
                    line.push_str("%%cd:~,");
                    slashes = 0;
                }
                _ => slashes = 0,
            }
            line.push(c);
        }
        if quoted {
            line.push_str(&"\\".repeat(slashes));
            line.push('"');
        }
    }
    line.push('"');
    Ok(line)
}

fn resolve(program: &str, path: &str, exts: &str) -> Option<PathBuf> {
    let with_exts = |base: PathBuf| -> Option<PathBuf> {
        if base.extension().is_some() && base.is_file() {
            return Some(base);
        }
        exts.split(';')
            .filter(|e| !e.is_empty())
            .map(|e| PathBuf::from(format!("{}{e}", base.display())))
            .find(|p| p.is_file())
    };
    let p = Path::new(program);
    if p.components().count() > 1 || p.is_absolute() {
        return with_exts(p.to_path_buf());
    }
    std::env::split_paths(path).find_map(|dir| with_exts(dir.join(program)))
}

/// One argument as Windows' command line parsing reads it back.
fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.into();
    }
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            c => {
                out.push_str(&"\\".repeat(slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(slashes * 2));
    out.push('"');
    out
}

fn env_block(env: &BTreeMap<String, String>) -> Vec<u16> {
    let mut block: Vec<u16> = Vec::new();
    for (k, v) in env {
        block.extend(format!("{k}={v}").encode_utf16());
        block.push(0);
    }
    block.push(0);
    block
}

// --- Detaching -------------------------------------------------------------

/// Starts `cmd` with no console (no new process group: that would turn off Ctrl+C for every program it starts), out
/// of the job of the process it was started from (that process ending, or its job closing, won't end it), stdin and
/// stdout to NUL and stderr to `log`. It inherits only those two handles: std's spawn hands down every inheritable
/// one, and a pipe of an ssh session living on in the holder keeps that session from ending.
pub fn spawn_detached(cmd: Command, log: File) -> io::Result<()> {
    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    for (k, v) in cmd.get_envs() {
        let k = k.to_string_lossy().into_owned();
        match v {
            Some(v) => env.insert(k, v.to_string_lossy().into_owned()),
            None => env.remove(&k),
        };
    }
    let parts: Vec<String> = std::iter::once(cmd.get_program())
        .chain(cmd.get_args())
        .map(|a| quote(&a.to_string_lossy()))
        .collect();
    let mut line = wide(parts.join(" ").as_ref());
    let mut block = env_block(&env);
    let nul = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("NUL")?;
    unsafe {
        let handles = [nul.as_raw_handle() as HANDLE, log.as_raw_handle() as HANDLE];
        for h in handles {
            check(SetHandleInformation(
                h,
                HANDLE_FLAG_INHERIT,
                HANDLE_FLAG_INHERIT,
            ))?;
        }
        let mut n = 0usize;
        InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut n);
        let mut attrs = vec![0u8; n];
        let list = attrs.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        check(InitializeProcThreadAttributeList(list, 1, 0, &mut n))?;
        let set = UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            handles.as_ptr().cast(),
            size_of_val(&handles),
            null_mut(),
            null(),
        );
        let mut si: STARTUPINFOEXW = zeroed();
        si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        si.lpAttributeList = list;
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = handles[0];
        si.StartupInfo.hStdOutput = handles[0];
        si.StartupInfo.hStdError = handles[1];
        let mut pi: PROCESS_INFORMATION = zeroed();
        let flags = EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | DETACHED_PROCESS;
        let mut start = |flags| {
            CreateProcessW(
                null(),
                line.as_mut_ptr(),
                null(),
                null(),
                1,
                flags,
                block.as_mut_ptr().cast(),
                null(),
                &si.StartupInfo,
                &mut pi,
            ) != 0
        };
        let mut started = set != 0 && start(flags | CREATE_BREAKAWAY_FROM_JOB);
        // A job that won't let us leave: stay in it and live as long as it does, rather than not start.
        if !started && set != 0 && GetLastError() == ERROR_ACCESS_DENIED {
            started = start(flags);
        }
        let err = io::Error::last_os_error();
        DeleteProcThreadAttributeList(list);
        if !started {
            return Err(err);
        }
        CloseHandle(pi.hThread);
        CloseHandle(pi.hProcess);
    }
    Ok(())
}

// --- Processes: ConPTY or pipes, always in a job of their own ---------------

/// Closes once: when its program ended (so the last output arrives and its reader ends), or when it is let go.
struct PseudoConsole(Mutex<Option<usize>>);

impl PseudoConsole {
    fn close(&self) {
        if let Some(hpc) = self.0.lock().unwrap_or_else(|p| p.into_inner()).take() {
            unsafe { ClosePseudoConsole(hpc as HPCON) };
        }
    }
}

impl Drop for PseudoConsole {
    fn drop(&mut self) {
        self.close();
    }
}

/// A held process and everything it starts, in one job that kills on close: closing the job (the holder dying, or
/// the exit being acked) ends the tree.
pub struct Proc {
    pid: u32,
    handle: OwnedHandle,
    job: OwnedHandle,
    console: Option<Arc<PseudoConsole>>,
}

impl Proc {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        if let Some(console) = &self.console
            && let Some(hpc) = *console.0.lock().unwrap_or_else(|p| p.into_inner())
        {
            unsafe {
                ResizePseudoConsole(
                    hpc as HPCON,
                    COORD {
                        X: cols as i16,
                        Y: rows as i16,
                    },
                )
            };
        }
    }

    /// Ends every process of the job.
    pub fn kill_tree(&self) {
        unsafe { TerminateJobObject(self.job.as_raw_handle() as HANDLE, 1) };
    }

    /// True once no process of the job is left.
    pub fn tree_empty(&self) -> bool {
        unsafe {
            let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = zeroed();
            let ok = QueryInformationJobObject(
                self.job.as_raw_handle() as HANDLE,
                JobObjectBasicAccountingInformation,
                (&raw mut info).cast(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                null_mut(),
            );
            ok != 0 && info.ActiveProcesses == 0
        }
    }

    /// Blocks until the program has ended.
    pub fn wait_exit(&self) {
        unsafe { WaitForSingleObject(self.handle.as_raw_handle() as HANDLE, INFINITE) };
    }

    /// Ends whatever the program left behind. Call after `wait_exit`, before `reap`.
    pub fn sweep(&self) {
        self.kill_tree();
    }

    /// Its exit code (Windows has no signals). Then closes its console, so its last output arrives and the reader
    /// ends.
    pub fn reap(&self) -> io::Result<Exit> {
        let mut code = 0u32;
        check(unsafe { GetExitCodeProcess(self.handle.as_raw_handle() as HANDLE, &mut code) })?;
        if let Some(console) = &self.console {
            console.close();
        }
        Ok(Exit {
            code: Some(code as i32),
            signal: None,
        })
    }
}

fn new_job() -> io::Result<OwnedHandle> {
    unsafe {
        let job = CreateJobObjectW(null(), null());
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = OwnedHandle::from_raw_handle(job as RawHandle);
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
        // No breakaway: whatever the program starts stays in the tree.
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        check(SetInformationJobObject(
            job.as_raw_handle() as HANDLE,
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ))?;
        Ok(job)
    }
}

/// One anonymous pipe: (read end, write end), neither inheritable.
fn anon_pipe() -> io::Result<(File, File)> {
    unsafe {
        let (mut r, mut w) = (null_mut(), null_mut());
        check(CreatePipe(&mut r, &mut w, null(), 0))?;
        Ok((
            File::from_raw_handle(r as RawHandle),
            File::from_raw_handle(w as RawHandle),
        ))
    }
}

/// Starts the process suspended, with `attr` added to its attribute list, puts it in `job`, and lets it run: so
/// nothing it starts can be outside the job.
fn create_in_job(
    spec: &Spec,
    inherit: &[HANDLE],
    std_handles: Option<[HANDLE; 3]>,
    pseudo: Option<HPCON>,
    job: &OwnedHandle,
) -> io::Result<(u32, OwnedHandle)> {
    let mut cmd = command_line(spec.argv, spec.env)?;
    let mut block = env_block(spec.env);
    let cwd = wide(spec.cwd.as_ref());
    unsafe {
        let count = u32::from(!inherit.is_empty()) + u32::from(pseudo.is_some());
        let mut n = 0usize;
        InitializeProcThreadAttributeList(null_mut(), count, 0, &mut n);
        let mut attrs = vec![0u8; n];
        let list = attrs.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        check(InitializeProcThreadAttributeList(list, count, 0, &mut n))?;
        let result = (|| {
            if !inherit.is_empty() {
                check(UpdateProcThreadAttribute(
                    list,
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                    inherit.as_ptr().cast(),
                    size_of_val(inherit),
                    null_mut(),
                    null(),
                ))?;
            }
            if let Some(hpc) = pseudo {
                check(UpdateProcThreadAttribute(
                    list,
                    0,
                    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                    hpc as *const c_void,
                    size_of::<HPCON>(),
                    null_mut(),
                    null(),
                ))?;
            }
            let mut si: STARTUPINFOEXW = zeroed();
            si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
            si.lpAttributeList = list;
            // Otherwise it inherits the holder's own stdio and writes there, not where it was told to.
            si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            let [i, o, e] = std_handles.unwrap_or([INVALID_HANDLE_VALUE; 3]);
            si.StartupInfo.hStdInput = i;
            si.StartupInfo.hStdOutput = o;
            si.StartupInfo.hStdError = e;
            let mut pi: PROCESS_INFORMATION = zeroed();
            let mut flags =
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED;
            if pseudo.is_none() {
                flags |= CREATE_NO_WINDOW;
            }
            check(CreateProcessW(
                null(),
                cmd.as_mut_ptr(),
                null(),
                null(),
                i32::from(!inherit.is_empty()),
                flags,
                block.as_mut_ptr().cast(),
                cwd.as_ptr(),
                &si.StartupInfo,
                &mut pi,
            ))?;
            let handle = OwnedHandle::from_raw_handle(pi.hProcess as RawHandle);
            let thread = OwnedHandle::from_raw_handle(pi.hThread as RawHandle);
            if AssignProcessToJobObject(
                job.as_raw_handle() as HANDLE,
                handle.as_raw_handle() as HANDLE,
            ) == 0
            {
                let err = io::Error::last_os_error();
                TerminateProcess(handle.as_raw_handle() as HANDLE, 1);
                return Err(err);
            }
            ResumeThread(thread.as_raw_handle() as HANDLE);
            Ok((pi.dwProcessId, handle))
        })();
        DeleteProcThreadAttributeList(list);
        result
    }
}

pub fn spawn(spec: &Spec) -> io::Result<Started> {
    let job = new_job()?;
    match spec.mode {
        Mode::Pty => {
            // Ctrl+C ignored is inherited (an ssh session sets it): the program inherits ours, so take Ctrl+C again.
            unsafe { SetConsoleCtrlHandler(None, 0) };
            let (in_r, in_w) = anon_pipe()?;
            let (out_r, out_w) = anon_pipe()?;
            let mut hpc: HPCON = 0;
            let hr = unsafe {
                CreatePseudoConsole(
                    COORD {
                        X: spec.cols as i16,
                        Y: spec.rows as i16,
                    },
                    in_r.as_raw_handle() as HANDLE,
                    out_w.as_raw_handle() as HANDLE,
                    0,
                    &mut hpc,
                )
            };
            if hr != 0 {
                return Err(io::Error::other(format!(
                    "CreatePseudoConsole failed: {hr:#x}"
                )));
            }
            let console = Arc::new(PseudoConsole(Mutex::new(Some(hpc as usize))));
            let (pid, handle) = create_in_job(spec, &[], None, Some(hpc), &job)?;
            // The console holds its own ends now.
            drop((in_r, out_w));
            Ok(Started {
                proc: Proc {
                    pid,
                    handle,
                    job,
                    console: Some(console),
                },
                input: Box::new(in_w),
                out: Box::new(out_r),
                err: None,
            })
        }
        Mode::Pipes => {
            let (child_in, input) = anon_pipe()?;
            let (out, child_out) = anon_pipe()?;
            let (err, child_err) = anon_pipe()?;
            let ends = [
                child_in.as_raw_handle() as HANDLE,
                child_out.as_raw_handle() as HANDLE,
                child_err.as_raw_handle() as HANDLE,
            ];
            for h in ends {
                check(unsafe {
                    SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT)
                })?;
            }
            let (pid, handle) = create_in_job(spec, &ends, Some(ends), None, &job)?;
            drop((child_in, child_out, child_err));
            Ok(Started {
                proc: Proc {
                    pid,
                    handle,
                    job,
                    console: None,
                },
                input: Box::new(input),
                out: Box::new(out),
                err: Some(Box::new(err)),
            })
        }
    }
}

/// Nothing to hold: the first-instance flag on the pipe already allows one holder per folder.
pub struct InstanceLock;

pub fn lock_instance(_dir: &Path) -> io::Result<Option<InstanceLock>> {
    Ok(Some(InstanceLock))
}
