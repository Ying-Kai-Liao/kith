//! Runs the engine as a child on Windows, inside a kill-on-close job the stub holds (docs/plans/windows-owner.md, W12 rules
//! H1, H2, H5 and H7).
//!
//! The job is unnamed, kills on close and has no breakaway flags; the stub keeps its only handle (not inheritable, never
//! duplicated). The engine is made inside it in one call (`PROC_THREAD_ATTRIBUTE_JOB_LIST`): never created and then assigned,
//! and never started plain when the job cannot be made. So a stub that is terminated hard takes the engine and everything the
//! engine started with it, as a process group does on the Mac, and the stub ends only after the job's last process has.
//!
//! The engine shares the stub's console, so Ctrl-C and Ctrl-Break reach both. The stub installs a handler that
//! swallows them (a handler is not inherited, unlike "ignore Ctrl-C", so the engine still sees the event) and
//! keeps waiting: it must outlive the engine, because it holds the lock. Console close / logoff / shutdown fall
//! through to the default and end the stub, which releases the lock and, by the job, ends the engine.

use std::ffi::{OsStr, OsString};
use std::io::{self, ErrorKind};
use std::os::windows::ffi::OsStrExt;
use std::ptr::{null, null_mut};
use std::thread::sleep;
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
    WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE,
    STD_OUTPUT_HANDLE, SetConsoleCtrlHandler,
};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
    GetExitCodeProcess, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute,
    WaitForSingleObject,
};

/// `ProcThreadAttributeJobList` (13) with the input flag: the jobs a new process is made in.
const PROC_THREAD_ATTRIBUTE_JOB_LIST: usize = 0x0002_000D;
/// `ProcThreadAttributeHandleList` (2) with the input flag: the only handles the new process inherits.
const PROC_THREAD_ATTRIBUTE_HANDLE_LIST: usize = 0x0002_0002;

unsafe extern "system" fn handler(event: u32) -> i32 {
    // Nonzero = handled.
    (event == CTRL_C_EVENT || event == CTRL_BREAK_EVENT) as i32
}

/// A kernel handle, closed on drop.
struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: the handle came from a call that returned it open and is closed once, here.
        unsafe { CloseHandle(self.0) };
    }
}

/// The engine, and the job that holds it. Dropping this closes the job's only handle, which ends whatever is still in it.
pub struct Spawned {
    process: Owned,
    job: Owned,
}

/// Starts the engine, or returns the stub's exit code when it cannot start. A resident stub does this before it serves
/// its control channel, as on Unix.
pub fn spawn(engine: &[OsString]) -> Result<Spawned, i32> {
    // SAFETY: handler matches the PHANDLER_ROUTINE signature.
    unsafe { SetConsoleCtrlHandler(Some(handler), 1) };
    let job = match make_job() {
        Ok(j) => j,
        Err(e) => {
            // Never run the engine outside a job: nothing would end it with the stub.
            eprintln!("kith-stub: cannot make the engine's job: {e}");
            return Err(126);
        }
    };
    match create_in_job(&job, engine) {
        Ok(process) => Ok(Spawned { process, job }),
        Err(e) => {
            eprintln!("kith-stub: cannot start {:?}: {e}", engine[0]);
            Err(if e.kind() == ErrorKind::NotFound {
                127
            } else {
                126
            })
        }
    }
}

pub fn run(engine: &[OsString]) -> i32 {
    match spawn(engine) {
        Ok(child) => wait(child),
        Err(code) => code,
    }
}

/// Holds until the engine and everything it started are gone; the stub's exit code is the engine's. The job closes when the
/// stub returns, so ending before the job is empty would kill what the engine left running.
pub fn wait(child: Spawned) -> i32 {
    // SAFETY: the handle is an open process with synchronize access.
    if unsafe { WaitForSingleObject(child.process.0, u32::MAX) } != WAIT_OBJECT_0 {
        return 1;
    }
    let mut code = 0;
    // SAFETY: the handle is an open process with query access; `code` is where the answer goes.
    let code = if unsafe { GetExitCodeProcess(child.process.0, &raw mut code) } == 0 {
        1
    } else {
        i32::from_ne_bytes(code.to_ne_bytes())
    };
    while active(&child.job).unwrap_or(0) > 0 {
        sleep(Duration::from_millis(50));
    }
    code
}

/// An unnamed job that ends its processes when its handle closes, with no breakaway flags. Null attributes make the handle
/// non-inheritable.
fn make_job() -> io::Result<Owned> {
    // SAFETY: null attributes mean the default security and a handle that is not inheritable; a null name makes it unnamed.
    let h = unsafe { CreateJobObjectW(null(), null()) };
    if h.is_null() {
        return Err(io::Error::last_os_error());
    }
    let job = Owned(h);
    // SAFETY: a zeroed structure is the documented starting point; only the limit flag is set.
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: `info` is the structure this information class takes, and its size is passed.
    let ok = unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&raw const info).cast(),
            u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).unwrap_or(0),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(job)
}

fn active(job: &Owned) -> io::Result<u32> {
    // SAFETY: a zeroed structure is what this information class fills.
    let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is the structure this information class fills, and its size is passed.
    let ok = unsafe {
        QueryInformationJobObject(
            job.0,
            JobObjectBasicAccountingInformation,
            (&raw mut info).cast(),
            u32::try_from(size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>()).unwrap_or(0),
            null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(info.ActiveProcesses)
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// One argument as Windows' command line parsing reads it back.
fn quote(arg: &OsStr) -> String {
    let arg = arg.to_string_lossy();
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\u{b}', '"']) {
        return arg.into_owned();
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

/// Makes the engine inside `job` in one call, with the stub's environment and directory and the stub's three standard handles
/// (the only handles it inherits). An error means no process was made.
fn create_in_job(job: &Owned, engine: &[OsString]) -> io::Result<Owned> {
    let mut line: Vec<u16> = wide(OsStr::new(
        &engine
            .iter()
            .map(|a| quote(a))
            .collect::<Vec<_>>()
            .join(" "),
    ));
    // SAFETY: plain calls; an invalid or null handle (no console, a service) is skipped below.
    let mut std_handles: [HANDLE; 3] = unsafe {
        [
            GetStdHandle(STD_INPUT_HANDLE),
            GetStdHandle(STD_OUTPUT_HANDLE),
            GetStdHandle(STD_ERROR_HANDLE),
        ]
    };
    let usable = std_handles
        .iter()
        .all(|h| !h.is_null() && *h != INVALID_HANDLE_VALUE);
    if !usable {
        std_handles = [INVALID_HANDLE_VALUE; 3];
    }
    // A handle list takes inheritable handles only, and refuses one that is listed twice.
    let mut listed: Vec<HANDLE> = if usable {
        std_handles.to_vec()
    } else {
        Vec::new()
    };
    listed.sort_by_key(|h| *h as usize);
    listed.dedup();
    for h in &listed {
        // SAFETY: the handle is one this process holds open.
        if unsafe { SetHandleInformation(*h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) } == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let jobs: [HANDLE; 1] = [job.0];
    let count = if listed.is_empty() { 1 } else { 2 };
    let mut size = 0usize;
    // SAFETY: the first call only asks how large the list must be (it fails with a size, by design).
    unsafe { InitializeProcThreadAttributeList(null_mut(), count, 0, &raw mut size) };
    let mut backing = vec![0u8; size];
    let attrs: LPPROC_THREAD_ATTRIBUTE_LIST = backing.as_mut_ptr().cast();
    // SAFETY: `backing` is `size` bytes, as the first call asked.
    if unsafe { InitializeProcThreadAttributeList(attrs, count, 0, &raw mut size) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let made = (|| {
        // SAFETY: `jobs` outlives the create call below; the attribute copies nothing, so it must (it does).
        if unsafe {
            UpdateProcThreadAttribute(
                attrs,
                0,
                PROC_THREAD_ATTRIBUTE_JOB_LIST,
                jobs.as_ptr().cast(),
                size_of_val(&jobs),
                null_mut(),
                null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if !listed.is_empty() {
            // SAFETY: as above, for `listed`.
            if unsafe {
                UpdateProcThreadAttribute(
                    attrs,
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
                    listed.as_ptr().cast(),
                    size_of_val(listed.as_slice()),
                    null_mut(),
                    null(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        // SAFETY: a zeroed STARTUPINFOEXW is the documented starting point; `cb` and the list are set below.
        let mut si: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        si.StartupInfo.cb = u32::try_from(size_of::<STARTUPINFOEXW>()).unwrap_or(0);
        si.lpAttributeList = attrs;
        // Without this the engine would take the standard handles of whatever it inherits.
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = std_handles[0];
        si.StartupInfo.hStdOutput = std_handles[1];
        si.StartupInfo.hStdError = std_handles[2];
        // SAFETY: a zeroed PROCESS_INFORMATION is what the call fills.
        let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: the command line is a live, NUL-ended buffer; the other pointers are null (the stub's own environment and
        // directory, the program found from the command line); `si` carries the attribute list; `pi` is filled.
        let ok = unsafe {
            CreateProcessW(
                null(),
                line.as_mut_ptr(),
                null(),
                null(),
                i32::from(!listed.is_empty()),
                EXTENDED_STARTUPINFO_PRESENT,
                null(),
                null(),
                &raw const si.StartupInfo,
                &raw mut pi,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        drop(Owned(pi.hThread));
        Ok(Owned(pi.hProcess))
    })();
    // SAFETY: the list was initialised above and is deleted once.
    unsafe { DeleteProcThreadAttributeList(attrs) };
    made
}
