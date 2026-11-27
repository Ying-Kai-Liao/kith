//! A run's job object, held by the process the claim records, and a process created inside it in one call
//! (docs/plans/windows-owner.md, W12 rules H1 and H2).
//!
//! The launcher is made with `PROC_THREAD_ATTRIBUTE_JOB_LIST`: the kernel puts the new process in the job as part of creating
//! it, so there is no moment at which it runs outside the job and none at which it sits suspended waiting to be assigned (a host
//! that dies between a suspended create and the resume would leave that process in the job for ever). If the job cannot take
//! the process, creating it fails and there is no process: nothing is ever started and then assigned, and there is no fallback
//! to a plain spawn.
//!
//! The job is unnamed (a name only lets a squatter fail a start) and kills on close: the process that made it keeps the only
//! handle, so when that process ends, however it ends, everything the run started goes with it, as a process group does on the
//! Mac. There are no breakaway flags, so nothing the run starts can leave the job. The handle is not inheritable and is never
//! duplicated into another process, so a child never keeps the job open. The holder of the job must therefore stay until the
//! job is empty (`wait_idle`).

use std::ffi::OsStr;
use std::io;
use std::os::windows::io::{AsRawHandle, BorrowedHandle};
use std::path::Path;
use std::ptr::{null, null_mut};
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation, WAIT_FAILED,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};

use crate::{Handle, last_err, wide};

/// `ProcThreadAttributeJobList` (13) with the input flag: the jobs a new process is made in.
const PROC_THREAD_ATTRIBUTE_JOB_LIST: usize = 0x0002_000D;
/// `ProcThreadAttributeHandleList` (2) with the input flag: the only handles the new process inherits.
const PROC_THREAD_ATTRIBUTE_HANDLE_LIST: usize = 0x0002_0002;
/// `STILL_ACTIVE`, what `GetExitCodeProcess` answers for a process that has not ended.
const STILL_ACTIVE: u32 = 259;

/// An open job object: the only handle to it, held by the process that made it.
pub struct Job(Handle);

// SAFETY: a kernel handle is not tied to a thread; the calls on it (query, terminate) take `&self` and the kernel serialises them.
unsafe impl Send for Job {}
// SAFETY: as above.
unsafe impl Sync for Job {}

impl Job {
    /// Makes an unnamed job that ends every process in it when its handle closes (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`), with
    /// no breakaway flags. Null security attributes make the handle non-inheritable.
    pub fn create() -> io::Result<Self> {
        // SAFETY: null attributes mean the default security and a handle that is not inheritable; a null name makes it unnamed.
        let h = unsafe { CreateJobObjectW(null(), null()) };
        if h.is_null() {
            return Err(last_err());
        }
        // The handle closes on drop, so a job that cannot be limited is never used.
        let job = Self(Handle(h));
        // SAFETY: a zeroed structure is the documented starting point; only the limit flag is set.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `info` is the structure this information class takes, and its size is passed.
        let ok = unsafe {
            SetInformationJobObject(
                job.0.0,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).unwrap_or(0),
            )
        };
        if ok == 0 {
            return Err(last_err());
        }
        Ok(job)
    }

    /// How many processes of the job are running now.
    pub fn active_processes(&self) -> io::Result<u32> {
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION {
            TotalUserTime: 0,
            TotalKernelTime: 0,
            ThisPeriodTotalUserTime: 0,
            ThisPeriodTotalKernelTime: 0,
            TotalPageFaultCount: 0,
            TotalProcesses: 0,
            ActiveProcesses: 0,
            TotalTerminatedProcesses: 0,
        };
        // SAFETY: `info` is the structure this information class fills, and its size is passed.
        let ok = unsafe {
            QueryInformationJobObject(
                self.0.0,
                JobObjectBasicAccountingInformation,
                (&raw mut info).cast(),
                u32::try_from(size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>()).unwrap_or(0),
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(last_err());
        }
        Ok(info.ActiveProcesses)
    }

    /// Waits until the job has no running process. The holder of a kill-on-close job must not end before this (W12 H5): closing
    /// the handle with a process still in the job kills it. A job answers no wait of its own for "empty", so this polls.
    pub fn wait_idle(&self) -> io::Result<()> {
        while self.active_processes()? > 0 {
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }

    /// Ends every process of the job at once (the stop of a run).
    pub fn terminate(&self, exit_code: u32) -> io::Result<()> {
        // SAFETY: the handle is an open job with terminate access.
        if unsafe { TerminateJobObject(self.0.0, exit_code) } == 0 {
            Err(last_err())
        } else {
            Ok(())
        }
    }

    /// Whether `child` is in this job.
    pub fn holds(&self, child: &Child) -> io::Result<bool> {
        let mut inside = 0;
        // SAFETY: both handles are open; `inside` is where the answer goes.
        let ok = unsafe { IsProcessInJob(child.process.0, self.0.0, &raw mut inside) };
        if ok == 0 {
            return Err(last_err());
        }
        Ok(inside != 0)
    }
}

/// What to start: a resolved program (a file, not a name to search for) and the whole environment of the process.
pub struct Launch<'a> {
    pub program: &'a Path,
    /// After the program name.
    pub args: &'a [String],
    /// The process's entire environment: nothing is inherited.
    pub env: &'a [(String, String)],
    pub cwd: Option<&'a Path>,
    /// Stdin, stdout and stderr. These three are the only handles the process inherits; `None` gives it none.
    pub stdio: Option<[BorrowedHandle<'a>; 3]>,
}

/// A process started in a job.
pub struct Child {
    pid: u32,
    process: Handle,
}

// SAFETY: a process handle is not tied to a thread; wait, exit-code and terminate calls on it take `&self`.
unsafe impl Send for Child {}
// SAFETY: as above.
unsafe impl Sync for Child {}

impl Child {
    pub fn id(&self) -> u32 {
        self.pid
    }

    /// Waits for it to end and gives its exit code.
    pub fn wait(&self) -> io::Result<u32> {
        // SAFETY: the handle is an open process with synchronize access.
        match unsafe { WaitForSingleObject(self.process.0, u32::MAX) } {
            WAIT_OBJECT_0 => self.exit_code().map(|c| c.unwrap_or(1)),
            WAIT_FAILED => Err(last_err()),
            other => Err(io::Error::other(format!(
                "kith-winsys: unexpected wait result {other}"
            ))),
        }
    }

    /// Waits up to `timeout`; `Some(code)` when it ended.
    pub fn wait_for(&self, timeout: Duration) -> io::Result<Option<u32>> {
        let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
        // SAFETY: the handle is an open process with synchronize access.
        match unsafe { WaitForSingleObject(self.process.0, ms) } {
            WAIT_OBJECT_0 => self.exit_code(),
            WAIT_TIMEOUT => Ok(None),
            WAIT_FAILED => Err(last_err()),
            other => Err(io::Error::other(format!(
                "kith-winsys: unexpected wait result {other}"
            ))),
        }
    }

    fn exit_code(&self) -> io::Result<Option<u32>> {
        let mut code = 0;
        // SAFETY: the handle is an open process with query access; `code` is where the answer goes.
        if unsafe { GetExitCodeProcess(self.process.0, &raw mut code) } == 0 {
            return Err(last_err());
        }
        Ok((code != STILL_ACTIVE).then_some(code))
    }

    /// Ends this process alone.
    pub fn kill(&self) -> io::Result<()> {
        // SAFETY: the handle is an open process with terminate access.
        if unsafe { TerminateProcess(self.process.0, 1) } == 0 {
            Err(last_err())
        } else {
            Ok(())
        }
    }
}

/// Starts `launch` inside `job`, in one call. An error means no process was made.
pub fn spawn_in_job(job: &Job, launch: &Launch<'_>) -> io::Result<Child> {
    let (child, thread) = create(job, launch, false)?;
    drop(thread);
    Ok(child)
}

/// `spawn_in_job`, but the process is made suspended (its first instruction has not run) and the main thread's handle is
/// given back: for a test that asks the job about the process before it can do anything. Never used to start a run.
#[cfg(test)]
fn spawn_in_job_suspended(job: &Job, launch: &Launch<'_>) -> io::Result<(Child, Handle)> {
    create(job, launch, true)
}

fn create(job: &Job, l: &Launch<'_>, suspended: bool) -> io::Result<(Child, Handle)> {
    let ext = l
        .program
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if ext == "cmd" || ext == "bat" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{}: a batch file is not started here (cmd reads its arguments again); name the program's .exe",
                l.program.display()
            ),
        ));
    }
    let program = wide(l.program.as_os_str());
    let mut line: Vec<String> = vec![quote(&l.program.to_string_lossy())];
    line.extend(l.args.iter().map(|a| quote(a)));
    let mut command_line = wide(OsStr::new(&line.join(" ")));
    let mut block = env_block(l.env);
    let cwd = l.cwd.map(|c| wide(c.as_os_str()));
    let mut handles: Vec<HANDLE> = l
        .stdio
        .as_ref()
        .map(|s| s.iter().map(AsRawHandle::as_raw_handle).collect())
        .unwrap_or_default();
    // A handle list with one handle twice (stdout and stderr on one handle) is refused as invalid.
    handles.sort_by_key(|h| *h as usize);
    handles.dedup();
    // A handle list takes inheritable handles only; the three the caller named are made so (they stay so).
    for h in &handles {
        // SAFETY: the handle is one the caller holds open (a borrowed handle).
        if unsafe { SetHandleInformation(*h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) } == 0 {
            return Err(last_err());
        }
    }
    let jobs: [HANDLE; 1] = [job.0.0];
    let count = if handles.is_empty() { 1 } else { 2 };
    let mut size = 0usize;
    // SAFETY: the first call only asks how large the list must be (it fails with a size, by design).
    unsafe { InitializeProcThreadAttributeList(null_mut(), count, 0, &raw mut size) };
    let mut backing = vec![0u8; size];
    let list: LPPROC_THREAD_ATTRIBUTE_LIST = backing.as_mut_ptr().cast();
    // SAFETY: `backing` is `size` bytes, as the first call asked.
    if unsafe { InitializeProcThreadAttributeList(list, count, 0, &raw mut size) } == 0 {
        return Err(last_err());
    }
    let made = (|| {
        // SAFETY: `jobs` outlives the create call below; the attribute copies nothing, so it must (it does).
        if unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_JOB_LIST,
                jobs.as_ptr().cast(),
                size_of_val(&jobs),
                null_mut(),
                null(),
            )
        } == 0
        {
            return Err(last_err());
        }
        if !handles.is_empty() {
            // SAFETY: as above, for `handles`.
            if unsafe {
                UpdateProcThreadAttribute(
                    list,
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
                    handles.as_ptr().cast(),
                    size_of_val(handles.as_slice()),
                    null_mut(),
                    null(),
                )
            } == 0
            {
                return Err(last_err());
            }
        }
        // SAFETY: a zeroed STARTUPINFOEXW is the documented starting point; `cb` and the list are set below.
        let mut si: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        si.StartupInfo.cb = u32::try_from(size_of::<STARTUPINFOEXW>()).unwrap_or(0);
        si.lpAttributeList = list;
        // Without this the process would inherit this one's own standard handles.
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        let [i, o, e] = match &l.stdio {
            Some(s) => [
                s[0].as_raw_handle(),
                s[1].as_raw_handle(),
                s[2].as_raw_handle(),
            ],
            None => [INVALID_HANDLE_VALUE; 3],
        };
        si.StartupInfo.hStdInput = i;
        si.StartupInfo.hStdOutput = o;
        si.StartupInfo.hStdError = e;
        // SAFETY: a zeroed PROCESS_INFORMATION is what the call fills.
        let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        let mut flags = EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT;
        if suspended {
            flags |= CREATE_SUSPENDED;
        }
        // SAFETY: every pointer is to a live, NUL-ended buffer or null; `si` carries the attribute list; `pi` is filled.
        let ok = unsafe {
            CreateProcessW(
                program.as_ptr(),
                command_line.as_mut_ptr(),
                null(),
                null(),
                i32::from(!handles.is_empty()),
                flags,
                block.as_mut_ptr().cast(),
                cwd.as_ref().map_or(null(), Vec::as_ptr),
                &raw const si.StartupInfo,
                &raw mut pi,
            )
        };
        if ok == 0 {
            return Err(last_err());
        }
        Ok((
            Child {
                pid: pi.dwProcessId,
                process: Handle(pi.hProcess),
            },
            Handle(pi.hThread),
        ))
    })();
    // SAFETY: the list was initialised above and is deleted once.
    unsafe { DeleteProcThreadAttributeList(list) };
    made
}

/// One argument as Windows' command line parsing reads it back.
fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\u{b}', '"']) {
        return arg.to_owned();
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

/// `NAME=value` entries, NUL-separated and ended by an extra NUL, sorted by name without regard to case as the call documents.
fn env_block(env: &[(String, String)]) -> Vec<u16> {
    let mut sorted: Vec<&(String, String)> = env.iter().collect();
    sorted.sort_by_key(|(k, _)| k.to_uppercase());
    let mut block: Vec<u16> = Vec::new();
    for (k, v) in sorted {
        block.extend(format!("{k}={v}").encode_utf16());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::os::windows::io::AsHandle;
    use windows_sys::Win32::Foundation::{DuplicateHandle, GetHandleInformation};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    /// The query right of a job (winnt.h).
    const JOB_OBJECT_QUERY: u32 = 0x4;

    fn system_root() -> String {
        std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_owned())
    }

    /// `cmd.exe /d /c ping -n 60 127.0.0.1`: runs about a minute without a console.
    fn sleeper(
        env: &[(String, String)],
    ) -> (std::path::PathBuf, Vec<String>, Vec<(String, String)>) {
        let cmd = Path::new(&system_root()).join("System32").join("cmd.exe");
        let args = ["/d", "/c", "ping -n 60 127.0.0.1 >nul"]
            .map(str::to_owned)
            .to_vec();
        let mut env = env.to_vec();
        env.push(("SystemRoot".to_owned(), system_root()));
        env.push(("PATH".to_owned(), format!("{}\\System32", system_root())));
        (cmd, args, env)
    }

    #[test]
    fn the_child_is_in_the_job_before_it_runs_a_single_instruction() {
        let job = Job::create().unwrap();
        let (program, args, env) = sleeper(&[]);
        let launch = Launch {
            program: &program,
            args: &args,
            env: &env,
            cwd: None,
            stdio: None,
        };
        let (child, _main_thread) = spawn_in_job_suspended(&job, &launch).unwrap();
        // The main thread was never resumed: whatever the answer is, it is the state at creation.
        assert!(job.holds(&child).unwrap());
        assert_eq!(job.active_processes().unwrap(), 1);
        job.terminate(1).unwrap();
        assert_eq!(child.wait().unwrap(), 1);
    }

    #[test]
    fn a_job_that_cannot_take_the_process_fails_the_launch_and_leaves_no_process() {
        let job = Job::create().unwrap();
        // A copy of the handle with the right to query only: the kernel may not put a process in it.
        let mut copy: HANDLE = null_mut();
        // SAFETY: both handles are this process's own; `copy` is where the new handle goes.
        let ok = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                job.0.0,
                GetCurrentProcess(),
                &raw mut copy,
                JOB_OBJECT_QUERY,
                0,
                0,
            )
        };
        assert_ne!(ok, 0);
        let query_only = Job(Handle(copy));
        let (program, args, env) = sleeper(&[]);
        let launch = Launch {
            program: &program,
            args: &args,
            env: &env,
            cwd: None,
            stdio: None,
        };
        assert!(spawn_in_job(&query_only, &launch).is_err());
        assert_eq!(job.active_processes().unwrap(), 0);
    }

    #[test]
    fn a_batch_file_is_refused_by_name() {
        let job = Job::create().unwrap();
        let program = Path::new("C:\\x\\tool.cmd");
        let launch = Launch {
            program,
            args: &[],
            env: &[],
            cwd: None,
            stdio: None,
        };
        let e = spawn_in_job(&job, &launch).err().unwrap();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn the_job_handle_is_not_inheritable() {
        let job = Job::create().unwrap();
        let mut flags = 0;
        // SAFETY: the handle is open; `flags` is where the answer goes.
        assert_ne!(unsafe { GetHandleInformation(job.0.0, &raw mut flags) }, 0);
        assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
    }

    #[test]
    fn closing_the_handle_ends_the_child_and_its_child_even_when_the_child_inherited_handles() {
        let job = Job::create().unwrap();
        let (program, args, env) = sleeper(&[]);
        // The child is given inheritable handles (the three std ones), as a run's launcher gives its engine: the job handle
        // must not be among what it inherits, or it would keep the job, and so itself, alive.
        let nul = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("NUL")
            .unwrap();
        let launch = Launch {
            program: &program,
            args: &args,
            env: &env,
            cwd: None,
            stdio: Some([nul.as_handle(), nul.as_handle(), nul.as_handle()]),
        };
        let child = spawn_in_job(&job, &launch).unwrap();
        // cmd and the ping it starts.
        for _ in 0..100 {
            if job.active_processes().unwrap() >= 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(job.active_processes().unwrap() >= 2);
        drop(job);
        assert!(
            child.wait_for(Duration::from_secs(10)).unwrap().is_some(),
            "the child outlived the closed job: it kept a handle to it"
        );
    }

    #[test]
    fn two_jobs_are_independent() {
        let (a, b) = (Job::create().unwrap(), Job::create().unwrap());
        let (program, args, env) = sleeper(&[]);
        let launch = Launch {
            program: &program,
            args: &args,
            env: &env,
            cwd: None,
            stdio: None,
        };
        let child = spawn_in_job(&a, &launch).unwrap();
        assert!(a.holds(&child).unwrap());
        assert!(!b.holds(&child).unwrap());
        a.terminate(3).unwrap();
        assert_eq!(child.wait().unwrap(), 3);
    }

    #[test]
    fn wait_idle_returns_once_the_last_process_has_ended() {
        let job = Job::create().unwrap();
        let cmd = Path::new(&system_root()).join("System32").join("cmd.exe");
        let args = ["/d", "/c", "exit 0"].map(str::to_owned).to_vec();
        let env = [("SystemRoot".to_owned(), system_root())];
        let launch = Launch {
            program: &cmd,
            args: &args,
            env: &env,
            cwd: None,
            stdio: None,
        };
        spawn_in_job(&job, &launch).unwrap();
        job.wait_idle().unwrap();
        assert_eq!(job.active_processes().unwrap(), 0);
    }

    #[test]
    fn arguments_are_quoted_for_the_command_line() {
        assert_eq!(quote("plain"), "plain");
        assert_eq!(quote("a b"), "\"a b\"");
        assert_eq!(quote("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(quote("dir\\"), "dir\\");
        assert_eq!(quote("a b\\"), "\"a b\\\\\"");
        assert_eq!(quote(""), "\"\"");
    }
}
