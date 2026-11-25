#![allow(unsafe_op_in_unsafe_fn, dead_code, clippy::all, clippy::pedantic, clippy::nursery)]
//! Everything Windows-specific. Output is `R <tag> <key>=<value>` lines so the workflow log can be grepped; a probe that
//! fails prints the Win32 error, it never panics. Nothing here ships.

use std::ffi::c_void;
use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::io::{AsRawHandle, AsRawSocket, FromRawSocket};
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, LocalFree, SetHandleInformation,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Networking::WinSock::{
    INVALID_SOCKET, WSA_FLAG_OVERLAPPED, WSADATA, WSADuplicateSocketW, WSAGetLastError, WSAPROTOCOL_INFOW, WSASocketW,
    WSAStartup,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::Credentials::{CRED_TYPE_GENERIC, CREDENTIALW, CredEnumerateW, CredReadW};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows_sys::Win32::Security::{
    DeriveCapabilitySidsFromName, GetTokenInformation, OWNER_SECURITY_INFORMATION, PSID, SECURITY_ATTRIBUTES,
    SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation, JobObjectBasicProcessIdList,
    JobObjectExtendedLimitInformation, OpenJobObjectW, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::{CreateNamedPipeW, GetNamedPipeServerProcessId};
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, KEY_READ, RegCloseKey, RegOpenKeyExW};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess, GetExitCodeProcess, InitializeProcThreadAttributeList,
    OpenProcess, OpenProcessToken, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};

mod rbox;

type Res<T> = Result<T, String>;

const ATTR_ENABLED: u32 = 4; // SE_GROUP_ENABLED
const HR_ALREADY_EXISTS: u32 = 0x8007_00B7;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

unsafe fn from_pwstr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0;
    while *p.add(n) != 0 {
        n += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
}

fn last() -> u32 {
    unsafe { GetLastError() }
}

fn r(tag: &str, kv: impl AsRef<str>) {
    println!("R {tag} {}", kv.as_ref());
    let _ = std::io::stdout().flush();
}

fn opt(a: &[String], key: &str) -> Option<String> {
    a.iter().position(|x| x == key).and_then(|i| a.get(i + 1)).cloned()
}

fn opts(a: &[String], key: &str) -> Vec<String> {
    let mut v = Vec::new();
    let mut i = 0;
    while i < a.len() {
        if a[i] == key {
            if let Some(x) = a.get(i + 1) {
                v.push(x.clone());
            }
            i += 1;
        }
        i += 1;
    }
    v
}

fn flag(a: &[String], key: &str) -> bool {
    a.iter().any(|x| x == key)
}

/// Arguments after a bare `--`.
fn tail(a: &[String]) -> Vec<String> {
    a.iter().position(|x| x == "--").map(|i| a[i + 1..].to_vec()).unwrap_or_default()
}

fn quote_arg(s: &str) -> String {
    if !s.is_empty() && !s.contains([' ', '\t', '"']) {
        return s.to_string();
    }
    let mut out = String::from("\"");
    let mut bs = 0;
    for c in s.chars() {
        match c {
            '\\' => bs += 1,
            '"' => {
                out.push_str(&"\\".repeat(bs * 2 + 1));
                out.push('"');
                bs = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(bs));
                out.push(c);
                bs = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(bs * 2));
    out.push('"');
    out
}

fn cmdline(args: &[String]) -> String {
    args.iter().map(|s| quote_arg(s)).collect::<Vec<_>>().join(" ")
}

unsafe fn sid_str(sid: PSID) -> String {
    let mut s: *mut u16 = null_mut();
    if ConvertSidToStringSidW(sid, &mut s) == 0 {
        return format!("<ConvertSidToStringSid err {}>", last());
    }
    let o = from_pwstr(s);
    LocalFree(s.cast());
    o
}

// ---- SIDs, jobs -------------------------------------------------------------------------------------------------

/// A custom capability SID derived from a name (the first of the derived SIDs; the group SIDs are leaked).
unsafe fn cap_sid(name: &str) -> Res<PSID> {
    let n = wide(name);
    let (mut gs, mut gc, mut ss, mut sc): (*mut PSID, u32, *mut PSID, u32) = (null_mut(), 0, null_mut(), 0);
    if DeriveCapabilitySidsFromName(n.as_ptr(), &mut gs, &mut gc, &mut ss, &mut sc) == 0 {
        return Err(format!("DeriveCapabilitySidsFromName({name}) err {}", last()));
    }
    if sc == 0 {
        return Err(format!("DeriveCapabilitySidsFromName({name}) gave no capability SID"));
    }
    Ok(*ss)
}

unsafe fn well_known(kind: i32) -> Res<PSID> {
    let buf = Box::leak(vec![0u8; 68].into_boxed_slice());
    let mut n = 68u32;
    if windows_sys::Win32::Security::CreateWellKnownSid(kind, null_mut(), buf.as_mut_ptr().cast(), &mut n) == 0 {
        return Err(format!("CreateWellKnownSid({kind}) err {}", last()));
    }
    Ok(buf.as_mut_ptr().cast())
}

/// The AppContainer SID of a profile name; `create` also makes the profile (idempotent).
unsafe fn ac_sid(name: &str, create: bool) -> Res<PSID> {
    let n = wide(name);
    let mut sid: PSID = null_mut();
    if create {
        let hr = CreateAppContainerProfile(n.as_ptr(), n.as_ptr(), n.as_ptr(), null(), 0, &mut sid);
        if hr == 0 {
            return Ok(sid);
        }
        if hr as u32 != HR_ALREADY_EXISTS {
            return Err(format!("CreateAppContainerProfile({name}) hr {:#x}", hr as u32));
        }
    }
    let hr = DeriveAppContainerSidFromAppContainerName(n.as_ptr(), &mut sid);
    if hr != 0 {
        return Err(format!("DeriveAppContainerSidFromAppContainerName({name}) hr {:#x}", hr as u32));
    }
    Ok(sid)
}

unsafe fn make_job(name: Option<&str>) -> Res<HANDLE> {
    let w = name.map(wide);
    let p = w.as_ref().map_or(null(), |v| v.as_ptr());
    let h = CreateJobObjectW(null(), p);
    if h.is_null() {
        return Err(format!("CreateJobObject err {}", last()));
    }
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if SetInformationJobObject(
        h,
        JobObjectExtendedLimitInformation,
        std::ptr::from_ref(&info).cast(),
        std::mem::size_of_val(&info) as u32,
    ) == 0
    {
        return Err(format!("SetInformationJobObject err {}", last()));
    }
    Ok(h)
}

unsafe fn job_active(job: HANDLE) -> Option<u32> {
    let mut i: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = std::mem::zeroed();
    let ok = QueryInformationJobObject(
        job,
        JobObjectBasicAccountingInformation,
        std::ptr::from_mut(&mut i).cast(),
        std::mem::size_of_val(&i) as u32,
        null_mut(),
    );
    (ok != 0).then_some(i.ActiveProcesses)
}

unsafe fn job_pids(job: HANDLE) -> Vec<usize> {
    let mut buf = vec![0u64; 130];
    let ok = QueryInformationJobObject(
        job,
        JobObjectBasicProcessIdList,
        buf.as_mut_ptr().cast(),
        (buf.len() * 8) as u32,
        null_mut(),
    );
    if ok == 0 {
        return vec![];
    }
    let b = buf.as_ptr().cast::<u32>();
    let n = (*b.add(1)) as usize;
    let list = buf.as_ptr().cast::<u8>().add(8).cast::<usize>();
    (0..n.min(120)).map(|i| *list.add(i)).collect()
}

// ---- the one CreateProcess ---------------------------------------------------------------------------------------

struct Spawn<'a> {
    cmdline: String,
    cwd: Option<String>,
    env: Vec<(String, String)>,
    /// (AppContainer SID, capability SIDs): becomes SECURITY_CAPABILITIES.
    ac: Option<(PSID, Vec<PSID>)>,
    /// Becomes PROC_THREAD_ATTRIBUTE_JOB_LIST.
    jobs: Vec<HANDLE>,
    /// Stdout and stderr go here; the only inheritable handles (PROC_THREAD_ATTRIBUTE_HANDLE_LIST).
    out: Option<&'a File>,
    /// A primary token (a restricted one): the process is made with CreateProcessAsUserW.
    token: Option<HANDLE>,
    /// CREATE_SUSPENDED: the caller resumes `pi.hThread`.
    suspended: bool,
    /// A security descriptor for the new process object (null: the default).
    proc_sd: *mut c_void,
    /// The child gets this process's own stdin/stdout/stderr (a shim handing Claude Code's pipes to the box).
    pass_std: bool,
}

fn env_block(extra: &[(String, String)]) -> Vec<u16> {
    let mut vars: Vec<(String, String)> =
        std::env::vars().filter(|(k, _)| !extra.iter().any(|(e, _)| e.eq_ignore_ascii_case(k))).collect();
    vars.extend(extra.iter().cloned());
    vars.sort_by_key(|(k, _)| k.to_ascii_uppercase());
    let mut b = Vec::new();
    for (k, v) in vars {
        b.extend(format!("{k}={v}").encode_utf16());
        b.push(0);
    }
    b.push(0);
    b
}

unsafe fn spawn(o: &Spawn) -> Res<PROCESS_INFORMATION> {
    let mut caps: Vec<SID_AND_ATTRIBUTES> = Vec::new();
    let mut sc: SECURITY_CAPABILITIES = std::mem::zeroed();
    let mut count = 0u32;
    if let Some((ac, cs)) = &o.ac {
        for c in cs {
            caps.push(SID_AND_ATTRIBUTES { Sid: *c, Attributes: ATTR_ENABLED });
        }
        sc.AppContainerSid = *ac;
        sc.Capabilities = if caps.is_empty() { null_mut() } else { caps.as_mut_ptr() };
        sc.CapabilityCount = caps.len() as u32;
        count += 1;
    }
    let mut jobs = o.jobs.clone();
    if !jobs.is_empty() {
        count += 1;
    }
    // A box cannot open NUL itself (F12); the host opens it here and hands it over as stdin.
    let nul = File::open("NUL").map_err(|e| format!("open NUL: {e}"))?;
    let mut inherit: Vec<HANDLE> = Vec::new();
    let std_h: Option<(HANDLE, HANDLE, HANDLE)> = if o.pass_std {
        let g = |n| {
            let h = windows_sys::Win32::System::Console::GetStdHandle(n);
            if h.is_null() || h == INVALID_HANDLE_VALUE { nul.as_raw_handle() } else { h }
        };
        Some((
            g(windows_sys::Win32::System::Console::STD_INPUT_HANDLE),
            g(windows_sys::Win32::System::Console::STD_OUTPUT_HANDLE),
            g(windows_sys::Win32::System::Console::STD_ERROR_HANDLE),
        ))
    } else {
        o.out.map(|out| (nul.as_raw_handle(), out.as_raw_handle(), out.as_raw_handle()))
    };
    if let Some((i, u, e)) = std_h {
        for h in [i, u, e] {
            if !inherit.contains(&h) {
                inherit.push(h);
            }
        }
        for h in &inherit {
            SetHandleInformation(*h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT);
        }
        count += 1;
    }

    let mut si: STARTUPINFOEXW = std::mem::zeroed();
    si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    let mut flags = CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW;
    if o.suspended {
        flags |= 4; // CREATE_SUSPENDED
    }
    if let Some((i, u, e)) = std_h {
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = i;
        si.StartupInfo.hStdOutput = u;
        si.StartupInfo.hStdError = e;
    }

    let mut list_mem: Vec<u64> = Vec::new();
    let mut list: *mut c_void = null_mut();
    if count > 0 {
        let mut size = 0usize;
        InitializeProcThreadAttributeList(null_mut(), count, 0, &mut size);
        list_mem = vec![0u64; size / 8 + 1];
        list = list_mem.as_mut_ptr().cast();
        if InitializeProcThreadAttributeList(list, count, 0, &mut size) == 0 {
            return Err(format!("InitializeProcThreadAttributeList err {}", last()));
        }
        if o.ac.is_some()
            && UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                std::ptr::from_ref(&sc).cast(),
                std::mem::size_of::<SECURITY_CAPABILITIES>(),
                null_mut(),
                null(),
            ) == 0
        {
            return Err(format!("UpdateProcThreadAttribute(SECURITY_CAPABILITIES) err {}", last()));
        }
        if !jobs.is_empty()
            && UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                jobs.as_mut_ptr().cast::<c_void>(),
                jobs.len() * std::mem::size_of::<HANDLE>(),
                null_mut(),
                null(),
            ) == 0
        {
            return Err(format!("UpdateProcThreadAttribute(JOB_LIST) err {}", last()));
        }
        if !inherit.is_empty()
            && UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                inherit.as_mut_ptr().cast::<c_void>(),
                inherit.len() * std::mem::size_of::<HANDLE>(),
                null_mut(),
                null(),
            ) == 0
        {
            return Err(format!("UpdateProcThreadAttribute(HANDLE_LIST) err {}", last()));
        }
        si.lpAttributeList = list;
        flags |= EXTENDED_STARTUPINFO_PRESENT;
    }

    let mut cl = wide(&o.cmdline);
    let env = env_block(&o.env);
    let cwd = o.cwd.as_deref().map(wide);
    let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
    let mut psa: SECURITY_ATTRIBUTES = std::mem::zeroed();
    psa.nLength = std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32;
    psa.lpSecurityDescriptor = o.proc_sd;
    let procattr: *const SECURITY_ATTRIBUTES = if o.proc_sd.is_null() { null() } else { &psa };
    let inh = i32::from(std_h.is_some());
    let ok = match o.token {
        Some(t) => windows_sys::Win32::System::Threading::CreateProcessAsUserW(
            t,
            null(),
            cl.as_mut_ptr(),
            procattr,
            null(),
            inh,
            flags,
            env.as_ptr().cast(),
            cwd.as_ref().map_or(null(), |c| c.as_ptr()),
            std::ptr::from_ref(&si).cast(),
            &mut pi,
        ),
        None => CreateProcessW(
            null(),
            cl.as_mut_ptr(),
            procattr,
            null(),
            inh,
            flags,
            env.as_ptr().cast(),
            cwd.as_ref().map_or(null(), |c| c.as_ptr()),
            std::ptr::from_ref(&si).cast(),
            &mut pi,
        ),
    };
    let e = last();
    if !list.is_null() {
        DeleteProcThreadAttributeList(list);
    }
    drop(list_mem);
    if ok == 0 {
        return Err(format!("CreateProcessW err {e}"));
    }
    Ok(pi)
}

/// Waits up to `secs`; kills the process on timeout. Returns the exit code, or None on timeout.
unsafe fn wait(pi: &PROCESS_INFORMATION, secs: u64) -> Option<u32> {
    if WaitForSingleObject(pi.hProcess, (secs * 1000) as u32) == WAIT_TIMEOUT {
        TerminateProcess(pi.hProcess, 99);
        return None;
    }
    let mut code = 0u32;
    GetExitCodeProcess(pi.hProcess, &mut code);
    Some(code)
}

fn out_file(path: &str) -> Res<File> {
    File::create(path).map_err(|e| format!("create {path}: {e}"))
}

fn dump(path: &str, tag: &str) {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            for l in s.lines() {
                println!("OUT({tag})| {l}");
            }
        }
        Err(e) => println!("OUT({tag})| <unreadable {e}>"),
    }
}

/// A box: an AppContainer profile, its capability SIDs and a job. The profile name carries the run.
struct Boxx {
    name: String,
    ac: PSID,
    caps: Vec<PSID>,
    job: HANDLE,
}

unsafe fn make_box(name: &str, cap_names: &[String], internet: bool, job_name: Option<&str>) -> Res<Boxx> {
    let ac = ac_sid(&format!("kithprobe.{name}"), true)?;
    let mut caps = Vec::new();
    for c in cap_names {
        caps.push(cap_sid(c)?);
    }
    if internet {
        caps.push(well_known(85)?); // WinCapabilityInternetClientSid
    }
    let job = make_job(job_name)?;
    Ok(Boxx { name: name.to_string(), ac, caps, job })
}

impl Boxx {
    unsafe fn spawn(
        &self,
        args: &[String],
        env: &[(String, String)],
        cwd: Option<String>,
        out: &File,
    ) -> Res<PROCESS_INFORMATION> {
        spawn(&Spawn {
            cmdline: cmdline(args),
            cwd,
            env: env.to_vec(),
            ac: Some((self.ac, self.caps.clone())),
            jobs: vec![self.job],
            out: Some(out),
            token: None,
            suspended: false,
            proc_sd: null_mut(),
            pass_std: false,
        })
    }
    unsafe fn delete_profile(&self) {
        DeleteAppContainerProfile(wide(&format!("kithprobe.{}", self.name)).as_ptr());
    }
}

// ---- subcommands: building ------------------------------------------------------------------------------------------

/// `box --name N [--cap C]... [--internet] [--job-name J] [--timeout S] [--linger S] [--out F] [--cwd D] [--env K=V]...
///  [--keep-profile] -- cmd args...`
unsafe fn cmd_box(a: &[String]) -> Res<()> {
    let name = opt(a, "--name").ok_or("--name required")?;
    let b = make_box(&name, &opts(a, "--cap"), flag(a, "--internet"), opt(a, "--job-name").as_deref())?;
    let out_path = opt(a, "--out").unwrap_or_else(|| format!("{}\\boxout-{name}.txt", std::env::temp_dir().display()));
    let out = out_file(&out_path)?;
    let env: Vec<(String, String)> = opts(a, "--env")
        .iter()
        .filter_map(|e| e.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
        .collect();
    let t0 = Instant::now();
    let pi = b.spawn(&tail(a), &env, opt(a, "--cwd"), &out)?;
    r("box", format!("{name} spawned pid={} caps={} ac={}", pi.dwProcessId, b.caps.len(), sid_str(b.ac)));
    let timeout: u64 = opt(a, "--timeout").and_then(|s| s.parse().ok()).unwrap_or(60);
    match wait(&pi, timeout) {
        Some(c) => r("box", format!("{name} exit={c} ms={}", t0.elapsed().as_millis())),
        None => r("box", format!("{name} TIMEOUT after {timeout}s (process killed)")),
    }
    r("box", format!("{name} job_active_after_exit={:?} job_pids={:?}", job_active(b.job), job_pids(b.job)));
    if let Some(s) = opt(a, "--linger").and_then(|s| s.parse::<u64>().ok()) {
        std::thread::sleep(Duration::from_secs(s));
        r("box", format!("{name} job_active_after_linger={:?} job_pids={:?}", job_active(b.job), job_pids(b.job)));
    }
    dump(&out_path, &name);
    let t = Instant::now();
    TerminateJobObject(b.job, 1);
    r("box", format!("{name} terminated job; active_now={:?} after_ms={}", job_active(b.job), t.elapsed().as_millis()));
    CloseHandle(b.job);
    CloseHandle(pi.hProcess);
    CloseHandle(pi.hThread);
    if !flag(a, "--keep-profile") {
        b.delete_profile();
    }
    Ok(())
}

unsafe fn cmd_capsid(a: &[String]) -> Res<()> {
    println!("{}", sid_str(cap_sid(a.first().ok_or("name")?)?));
    Ok(())
}

unsafe fn cmd_acsid(a: &[String]) -> Res<()> {
    println!("{}", sid_str(ac_sid(&format!("kithprobe.{}", a.first().ok_or("name")?), false)?));
    Ok(())
}

unsafe fn token_user_sid() -> Res<String> {
    let mut t: HANDLE = null_mut();
    if OpenProcessToken(GetCurrentProcess(), 8, &mut t) == 0 {
        return Err(format!("OpenProcessToken err {}", last()));
    }
    let mut buf = vec![0u64; 64];
    let mut n = 0u32;
    if GetTokenInformation(t, 1, buf.as_mut_ptr().cast(), 512, &mut n) == 0 {
        return Err(format!("GetTokenInformation(TokenUser) err {}", last()));
    }
    let sid = *buf.as_ptr().cast::<PSID>();
    Ok(sid_str(sid))
}

unsafe fn cmd_usersid() -> Res<()> {
    println!("{}", token_user_sid()?);
    Ok(())
}

/// What this process's token says: AppContainer or not, integrity, AppContainer SID.
unsafe fn cmd_ac_info() -> Res<()> {
    let mut t: HANDLE = null_mut();
    if OpenProcessToken(GetCurrentProcess(), 8, &mut t) == 0 {
        return Err(format!("OpenProcessToken err {}", last()));
    }
    let mut is_ac = 0u32;
    let mut n = 0u32;
    GetTokenInformation(t, 29, std::ptr::from_mut(&mut is_ac).cast(), 4, &mut n);
    r(
        "acinfo",
        format!("is_appcontainer={is_ac} is_restricted={}", windows_sys::Win32::Security::IsTokenRestricted(t)),
    );
    let mut buf = vec![0u64; 64];
    if GetTokenInformation(t, 25, buf.as_mut_ptr().cast(), 512, &mut n) != 0 {
        r("acinfo", format!("integrity_sid={}", sid_str(*buf.as_ptr().cast::<PSID>())));
    }
    let mut buf = vec![0u64; 64];
    if GetTokenInformation(t, 31, buf.as_mut_ptr().cast(), 512, &mut n) != 0 {
        r("acinfo", format!("appcontainer_sid={}", sid_str(*buf.as_ptr().cast::<PSID>())));
    }
    r("acinfo", format!("user_sid={}", token_user_sid().unwrap_or_default()));
    Ok(())
}

// ---- subcommands: what a box can reach (P-G3/G4/G5/G6) ------------------------------------------------------------

unsafe fn cmd_open_proc(a: &[String]) -> Res<()> {
    let pid: u32 = a.first().and_then(|s| s.parse().ok()).ok_or("pid")?;
    for (label, rights) in [
        ("QUERY_LIMITED", 0x1000u32),
        ("SYNCHRONIZE", 0x10_0000),
        ("VM_READ", 0x10),
        ("DUP_HANDLE", 0x40),
        ("TERMINATE", 0x1),
        ("ALL_ACCESS", 0x1F_FFFF),
    ] {
        let h = OpenProcess(rights, 0, pid);
        if h.is_null() {
            r("openproc", format!("pid={pid} {label}=denied err={}", last()));
        } else {
            r("openproc", format!("pid={pid} {label}=OPENED"));
            CloseHandle(h);
        }
    }
    Ok(())
}

unsafe fn cmd_cred_read(a: &[String]) -> Res<()> {
    let target = wide(a.first().ok_or("target")?);
    let mut p: *mut CREDENTIALW = null_mut();
    if CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut p) == 0 {
        r("credread", format!("target={} denied/absent err={}", a[0], last()));
    } else {
        r("credread", format!("target={} READ blob_size={}", a[0], (*p).CredentialBlobSize));
    }
    let mut count = 0u32;
    let mut arr: *mut *mut CREDENTIALW = null_mut();
    if CredEnumerateW(null(), 0, &mut count, &mut arr) == 0 {
        r("credenum", format!("err={}", last()));
    } else {
        r("credenum", format!("count={count}"));
    }
    Ok(())
}

unsafe fn cmd_reg_read() -> Res<()> {
    for key in ["Software\\kithprobe", "Environment", "Software\\Microsoft\\Windows\\CurrentVersion"] {
        let mut h = null_mut();
        let e = RegOpenKeyExW(HKEY_CURRENT_USER, wide(key).as_ptr(), 0, KEY_READ, &mut h);
        if e == 0 {
            r("regread", format!("HKCU\\{key}=OPENED"));
            RegCloseKey(h);
        } else {
            r("regread", format!("HKCU\\{key}=denied/absent err={e}"));
        }
    }
    Ok(())
}

/// `pipe-create NAME [--first] [--hold S] [--sddl SDDL]`
unsafe fn cmd_pipe_create(a: &[String]) -> Res<()> {
    let name = a.first().ok_or("name")?;
    let path = wide(&format!("\\\\.\\pipe\\{name}"));
    let mut sa: SECURITY_ATTRIBUTES = std::mem::zeroed();
    sa.nLength = std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32;
    let mut psa: *const SECURITY_ATTRIBUTES = null();
    if let Some(sddl) = opt(a, "--sddl") {
        let mut sd = null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(wide(&sddl).as_ptr(), 1, &mut sd, null_mut()) == 0 {
            return Err(format!("SDDL parse err {}", last()));
        }
        sa.lpSecurityDescriptor = sd;
        psa = &sa;
    }
    let mut open = PIPE_ACCESS_DUPLEX;
    if flag(a, "--first") {
        open |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    let h = CreateNamedPipeW(path.as_ptr(), open, 0, 255, 4096, 4096, 0, psa);
    if h == INVALID_HANDLE_VALUE {
        r("pipecreate", format!("{name} FAILED err={} first={}", last(), flag(a, "--first")));
        return Ok(());
    }
    let mut owner: PSID = null_mut();
    let mut sd = null_mut();
    let e = GetSecurityInfo(
        h,
        SE_KERNEL_OBJECT,
        OWNER_SECURITY_INFORMATION,
        &mut owner,
        null_mut(),
        null_mut(),
        null_mut(),
        &mut sd,
    );
    r(
        "pipecreate",
        format!(
            "{name} CREATED first={} owner_sid={} (err {e}) creator_user={}",
            flag(a, "--first"),
            if e == 0 { sid_str(owner) } else { "?".into() },
            token_user_sid().unwrap_or_default()
        ),
    );
    if let Some(s) = opt(a, "--hold").and_then(|s| s.parse::<u64>().ok()) {
        std::thread::sleep(Duration::from_secs(s));
    }
    CloseHandle(h);
    Ok(())
}

/// `pipe-connect NAME [--expect-owner SID]`
unsafe fn cmd_pipe_connect(a: &[String]) -> Res<()> {
    let name = a.first().ok_or("name")?;
    let path = wide(&format!("\\\\.\\pipe\\{name}"));
    for (label, access) in [("RW", 0xC000_0000u32), ("READ_CONTROL_only", 0x2_0000), ("RW+READ_CONTROL", 0xC002_0000)] {
        let h = CreateFileW(path.as_ptr(), access, 0, null(), OPEN_EXISTING, 0, null_mut());
        if h == INVALID_HANDLE_VALUE {
            r("pipeconnect", format!("{name} open({label}) FAILED err={}", last()));
            continue;
        }
        let mut pid = 0u32;
        let pe = GetNamedPipeServerProcessId(h, &mut pid);
        let mut owner: PSID = null_mut();
        let mut sd = null_mut();
        let e = GetSecurityInfo(
            h,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut sd,
        );
        let o = if e == 0 { sid_str(owner) } else { format!("<GetSecurityInfo err {e}>") };
        let m = opt(a, "--expect-owner").map(|x| format!(" owner_matches_expected={}", x == o)).unwrap_or_default();
        r("pipeconnect", format!("{name} open({label}) OK server_pid={pid}(ok={pe}) owner_sid={o}{m}"));
        CloseHandle(h);
        break;
    }
    Ok(())
}

/// `pipe-squat`: a box holds a pipe name, then the unboxed host tries to create the same name as first instance.
unsafe fn cmd_pipe_squat(a: &[String]) -> Res<()> {
    let name = opt(a, "--pipe").unwrap_or_else(|| "kith-host-squat".into());
    let b = make_box("squat", &opts(a, "--cap"), false, None)?;
    let out_path = format!("{}\\squat-out.txt", std::env::temp_dir().display());
    let out = out_file(&out_path)?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?.display().to_string();
    let pi =
        b.spawn(&[exe.clone(), "pipe-create".into(), name.clone(), "--hold".into(), "8".into()], &[], None, &out)?;
    std::thread::sleep(Duration::from_secs(3));
    r("squat", "host now creates the same name as FIRST instance:");
    cmd_pipe_create(&[name.clone(), "--first".into()])?;
    r("squat", "host connects to the box-held pipe:");
    cmd_pipe_connect(&[name.clone()])?;
    wait(&pi, 15);
    dump(&out_path, "box-side");
    TerminateJobObject(b.job, 1);
    b.delete_profile();
    Ok(())
}

/// `pair-test`: can one box open a process of another box, of the same box, and can the host open the box's?
unsafe fn cmd_pair_test(a: &[String]) -> Res<()> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?.display().to_string();
    let ba = make_box("pa", &opts(a, "--cap"), false, None)?;
    let bb = make_box("pb", &opts(a, "--cap"), false, None)?;
    let dir = std::env::temp_dir().display().to_string();
    let (fa, fb, fs) = (
        out_file(&format!("{dir}\\pa.txt"))?,
        out_file(&format!("{dir}\\pb.txt"))?,
        out_file(&format!("{dir}\\ps.txt"))?,
    );
    let sleeper = ba.spawn(&["ping.exe".into(), "-n".into(), "60".into(), "127.0.0.1".into()], &[], None, &fa)?;
    let pid = sleeper.dwProcessId;
    r("pair", format!("box A sleeper pid={pid}"));
    let same = ba.spawn(&[exe.clone(), "open-proc".into(), pid.to_string()], &[], None, &fs)?;
    wait(&same, 20);
    dump(&format!("{dir}\\ps.txt"), "same-box-opens-A");
    let other = bb.spawn(&[exe, "open-proc".into(), pid.to_string()], &[], None, &fb)?;
    wait(&other, 20);
    dump(&format!("{dir}\\pb.txt"), "other-box-opens-A");
    r("pair", "the unboxed host (same user) opens A's process:");
    cmd_open_proc(&[pid.to_string()])?;
    TerminateJobObject(ba.job, 1);
    TerminateJobObject(bb.job, 1);
    ba.delete_profile();
    bb.delete_profile();
    Ok(())
}

unsafe fn cmd_injob(a: &[String]) -> Res<()> {
    let pid: u32 = opt(a, "--pid").and_then(|s| s.parse().ok()).ok_or("--pid")?;
    let job = opt(a, "--job").ok_or("--job")?;
    let p = OpenProcess(0x1000, 0, pid);
    if p.is_null() {
        r("injob", format!("pid={pid} open err {}", last()));
        return Ok(());
    }
    let j = OpenJobObjectW(4, 0, wide(&job).as_ptr());
    if j.is_null() {
        r("injob", format!("job {job} open err {}", last()));
        return Ok(());
    }
    let mut res = 0;
    IsProcessInJob(p, j, &mut res);
    r("injob", format!("pid={pid} in_job({job})={}", res != 0));
    Ok(())
}

// ---- subcommands: network (P-G7, P-G11) -----------------------------------------------------------------------------

fn cmd_tcp_listen(a: &[String]) -> Res<()> {
    let l = std::net::TcpListener::bind(format!(
        "{}:{}",
        opt(a, "--bind").unwrap_or_else(|| "127.0.0.1".into()),
        opt(a, "--port").unwrap_or_else(|| "0".into())
    ))
    .map_err(|e| e.to_string())?;
    let port = l.local_addr().map_err(|e| e.to_string())?.port();
    if let Some(f) = opt(a, "--port-file") {
        std::fs::write(&f, port.to_string()).map_err(|e| e.to_string())?;
    }
    r("tcplisten", format!("port={port}"));
    l.set_nonblocking(true).map_err(|e| e.to_string())?;
    let end = Instant::now() + Duration::from_secs(opt(a, "--hold").and_then(|s| s.parse().ok()).unwrap_or(20));
    while Instant::now() < end {
        if let Ok((mut s, peer)) = l.accept() {
            s.set_nonblocking(false).ok();
            let mut b = [0u8; 4];
            let n = s.read(&mut b).unwrap_or(0);
            let _ = s.write_all(b"pong");
            r("tcplisten", format!("accepted from {peer} read={n}"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn cmd_connect(a: &[String]) -> Res<()> {
    use std::net::ToSocketAddrs;
    let target = a.first().ok_or("addr")?;
    let addrs: Vec<_> = match target.to_socket_addrs() {
        Ok(i) => i.collect(),
        Err(e) => {
            r("connect", format!("{target} RESOLVE FAILED {e} os={:?}", e.raw_os_error()));
            return Ok(());
        }
    };
    let Some(addr) = addrs.first() else {
        r("connect", format!("{target} resolved to nothing"));
        return Ok(());
    };
    match std::net::TcpStream::connect_timeout(addr, Duration::from_secs(6)) {
        Ok(mut s) => {
            let _ = s.write_all(b"ping");
            s.set_read_timeout(Some(Duration::from_secs(2))).ok();
            let mut b = [0u8; 4];
            let n = s.read(&mut b).unwrap_or(0);
            r("connect", format!("{target} ({addr}) CONNECTED reply_bytes={n}"));
        }
        Err(e) => r("connect", format!("{target} ({addr}) FAILED {e} os={:?}", e.raw_os_error())),
    }
    Ok(())
}

fn cmd_loop_self() -> Res<()> {
    let l = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| format!("bind {e}"))?;
    let port = l.local_addr().map_err(|e| e.to_string())?.port();
    let t = std::thread::spawn(move || {
        if let Ok((mut s, _)) = l.accept() {
            let mut b = [0u8; 4];
            let _ = s.read(&mut b);
            let _ = s.write_all(b"pong");
        }
    });
    cmd_connect(&[format!("127.0.0.1:{port}")])?;
    let _ = t.join();
    Ok(())
}

fn cmd_loop_two() -> Res<()> {
    let l = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| format!("bind {e}"))?;
    let port = l.local_addr().map_err(|e| e.to_string())?.port();
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let child = std::process::Command::new(exe)
        .arg("connect")
        .arg(format!("127.0.0.1:{port}"))
        .output()
        .map_err(|e| e.to_string())?;
    let (mut s, peer) = l.accept().map_err(|e| e.to_string())?;
    let mut b = [0u8; 4];
    let _ = s.read(&mut b);
    r("looptwo", format!("parent accepted from {peer}"));
    println!("{}", String::from_utf8_lossy(&child.stdout).trim_end());
    Ok(())
}

// ---- subcommands: files (P-G8) ---------------------------------------------------------------------------------------

fn cmd_try_open(a: &[String]) -> Res<()> {
    let path = a.first().ok_or("path")?;
    let mode = a.get(1).map_or("r", String::as_str);
    let res = match mode {
        "w" => std::fs::OpenOptions::new().append(true).open(path).map(|_| ()),
        "c" => std::fs::OpenOptions::new().write(true).create_new(true).open(path).map(|_| ()),
        _ => File::open(path).map(|_| ()),
    };
    match res {
        Ok(()) => r("tryopen", format!("{path} mode={mode} OK")),
        Err(e) => r("tryopen", format!("{path} mode={mode} DENIED/FAILED {e} os={:?}", e.raw_os_error())),
    }
    Ok(())
}

fn cmd_mktree(a: &[String]) -> Res<()> {
    let root = a.first().ok_or("dir")?;
    let dirs: u32 = a.get(1).and_then(|s| s.parse().ok()).unwrap_or(1000);
    let files: u32 = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(100);
    let t = Instant::now();
    for d in 0..dirs {
        let dp = format!("{root}\\d{d}");
        std::fs::create_dir_all(&dp).map_err(|e| e.to_string())?;
        for f in 0..files {
            File::create(format!("{dp}\\f{f}.txt")).map_err(|e| e.to_string())?;
        }
    }
    r("mktree", format!("{root} dirs={dirs} files={} ms={}", dirs * files, t.elapsed().as_millis()));
    Ok(())
}

// ---- subcommands: job tree (P-G10) -----------------------------------------------------------------------------------

/// The run's side: a plain process inside the outer job that makes a nested job and a box in it.
unsafe fn cmd_g10_run() -> Res<()> {
    let b = make_box("g10", &[], false, Some("Local\\kith-g10-inner"))?;
    let dir = std::env::temp_dir().display().to_string();
    let out = out_file(&format!("{dir}\\g10-box.txt"))?;
    let line = "start /b ping -n 120 127.0.0.1 >nul & start /b ping -n 121 127.0.0.1 >nul & ping -n 122 127.0.0.1 >nul";
    match b.spawn(&["cmd.exe".into(), "/c".into(), line.into()], &[], None, &out) {
        Ok(pi) => r("g10run", format!("box spawned in nested job pid={}", pi.dwProcessId)),
        Err(e) => r("g10run", format!("box spawn in nested job FAILED: {e}")),
    }
    std::thread::sleep(Duration::from_secs(40));
    Ok(())
}

unsafe fn cmd_g10() -> Res<()> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?.display().to_string();
    let outer = make_job(Some("Local\\kith-g10-outer"))?;
    let dir = std::env::temp_dir().display().to_string();
    let out_path = format!("{dir}\\g10-run.txt");
    let out = out_file(&out_path)?;
    let pi = spawn(&Spawn {
        cmdline: cmdline(&[exe, "g10-run".into()]),
        cwd: None,
        env: vec![],
        ac: None,
        jobs: vec![outer],
        out: Some(&out),
        token: None,
        suspended: false,
        proc_sd: null_mut(),
        pass_std: false,
    })?;
    r("g10", format!("run process pid={} in outer job", pi.dwProcessId));
    std::thread::sleep(Duration::from_secs(5));
    let pids = job_pids(outer);
    r("g10", format!("outer job active={:?} pids={pids:?}", job_active(outer)));
    let t = Instant::now();
    TerminateJobObject(outer, 1);
    let mut reached = None;
    while t.elapsed() < Duration::from_secs(15) {
        if job_active(outer) == Some(0) {
            reached = Some(t.elapsed());
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    r("g10", format!("TerminateJobObject(outer) -> ActiveProcesses==0 after {:?}", reached));
    let mut alive = Vec::new();
    for p in pids {
        let h = OpenProcess(0x10_0000, 0, p as u32);
        if !h.is_null() {
            if WaitForSingleObject(h, 0) == WAIT_TIMEOUT {
                alive.push(p);
            }
            CloseHandle(h);
        }
    }
    r("g10", format!("pids still alive after: {alive:?}"));
    dump(&out_path, "g10-run");
    dump(&format!("{dir}\\g10-box.txt"), "g10-box");
    Ok(())
}

// ---- subcommands: WSADuplicateSocket (P-G11) -------------------------------------------------------------------------

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).filter_map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()).collect()
}

/// The unboxed host duplicates an accepted socket into a boxed child by pid, and the child uses it.
unsafe fn cmd_sock_dup(a: &[String]) -> Res<()> {
    let dir = opt(a, "--dir").ok_or("--dir (a folder the box may read and write)")?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?.display().to_string();
    let l = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let client = std::net::TcpStream::connect(l.local_addr().map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let (server, _) = l.accept().map_err(|e| e.to_string())?;
    let b = make_box("sockdup", &opts(a, "--cap"), false, None)?;
    let info_file = format!("{dir}\\sockinfo.txt");
    let _ = std::fs::remove_file(&info_file);
    let out_path = format!("{dir}\\sock-child.txt");
    let out = out_file(&out_path)?;
    let pi = b.spawn(&[exe, "sock-child".into(), info_file.clone()], &[], None, &out)?;
    let mut info: WSAPROTOCOL_INFOW = std::mem::zeroed();
    let rc = WSADuplicateSocketW(server.as_raw_socket() as usize, pi.dwProcessId, &mut info);
    r("sockdup", format!("WSADuplicateSocket into boxed pid={} rc={rc} wsaerr={}", pi.dwProcessId, WSAGetLastError()));
    let bytes =
        std::slice::from_raw_parts(std::ptr::from_ref(&info).cast::<u8>(), std::mem::size_of::<WSAPROTOCOL_INFOW>());
    std::fs::write(format!("{info_file}.tmp"), hex(bytes)).map_err(|e| e.to_string())?;
    std::fs::rename(format!("{info_file}.tmp"), &info_file).map_err(|e| e.to_string())?;
    let mut c = client;
    c.write_all(b"hello").map_err(|e| e.to_string())?;
    c.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let mut buf = [0u8; 5];
    let n = c.read(&mut buf).unwrap_or(0);
    r(
        "sockdup",
        format!("host client got {n} bytes back from the boxed child: {:?}", String::from_utf8_lossy(&buf[..n])),
    );
    wait(&pi, 15);
    dump(&out_path, "sock-child");
    TerminateJobObject(b.job, 1);
    b.delete_profile();
    Ok(())
}

unsafe fn cmd_sock_child(a: &[String]) -> Res<()> {
    let f = a.first().ok_or("info file")?;
    let end = Instant::now() + Duration::from_secs(15);
    let text = loop {
        if let Ok(t) = std::fs::read_to_string(f) {
            break t;
        }
        if Instant::now() > end {
            return Err("info file never appeared".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let mut wd: WSADATA = std::mem::zeroed();
    WSAStartup(0x0202, &mut wd);
    let bytes = unhex(text.trim());
    let mut info: WSAPROTOCOL_INFOW = std::mem::zeroed();
    std::ptr::copy_nonoverlapping(
        bytes.as_ptr(),
        std::ptr::from_mut(&mut info).cast::<u8>(),
        bytes.len().min(std::mem::size_of::<WSAPROTOCOL_INFOW>()),
    );
    let s = WSASocketW(-1, -1, -1, &info, 0, WSA_FLAG_OVERLAPPED);
    if s == INVALID_SOCKET {
        r("sockchild", format!("WSASocket from duplicated info FAILED wsaerr={}", WSAGetLastError()));
        return Ok(());
    }
    let mut st = std::net::TcpStream::from_raw_socket(s as _);
    let mut buf = [0u8; 5];
    st.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let n = st.read(&mut buf).unwrap_or(0);
    let _ = st.write_all(b"world");
    r(
        "sockchild",
        format!("duplicated socket usable in box: read {n} bytes {:?}, replied", String::from_utf8_lossy(&buf[..n])),
    );
    Ok(())
}

// ---- P-G1: a shell stand-in for CLAUDE_CODE_GIT_BASH_PATH --------------------------------------------------------------

/// Run as any name that is not a subcommand: logs how Claude Code called it, then hands over to the real Git Bash.
fn fakesh(args: &[String]) {
    let log = std::env::var("KITH_SH_LOG").unwrap_or_else(|_| "C:\\probe\\sh.log".into());
    let claude_env: Vec<String> = std::env::vars()
        .filter(|(k, _)| k.starts_with("CLAUDE") || k.starts_with("KITH"))
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    let line = format!(
        "exe={:?} args={:?} cwd={:?} env={:?}\n",
        std::env::current_exe().ok(),
        &args[1..],
        std::env::current_dir().ok(),
        claude_env
    );
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log) {
        let _ = f.write_all(line.as_bytes());
    }
    let real = std::env::var("KITH_REAL_BASH").unwrap_or_else(|_| "C:\\Program Files\\Git\\bin\\bash.exe".into());
    if std::env::var("KITH_BOX").as_deref() == Ok("r") {
        match unsafe { rbox::shim_box(&real, &args[1..]) } {
            Ok(c) => std::process::exit(c),
            Err(e) => {
                eprintln!("fakesh: boxed run failed: {e}");
                std::process::exit(126);
            }
        }
    }
    match std::process::Command::new(real).args(&args[1..]).status() {
        Ok(s) => std::process::exit(s.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("fakesh: cannot run real bash: {e}");
            std::process::exit(127);
        }
    }
}

// ---- dispatch -------------------------------------------------------------------------------------------------------

pub fn main() {
    let args: Vec<String> = std::env::args().collect();
    let sub = args.get(1).map(String::as_str).unwrap_or("");
    let known = [
        "box",
        "capsid",
        "acsid",
        "usersid",
        "ac-info",
        "open-proc",
        "cred-read",
        "reg-read",
        "pipe-create",
        "pipe-connect",
        "pipe-squat",
        "pair-test",
        "injob",
        "tcp-listen",
        "connect",
        "loop-self",
        "loop-two",
        "try-open",
        "mktree",
        "g10",
        "g10-run",
        "sock-dup",
        "sock-child",
        "xbox",
        "watch",
        "g3n-box",
        "objdir-hold",
        "nul-open",
        "sddl-of",
        "p17",
        "mutex-hold",
        "mutex-open",
        "pipe-anon",
        "sid-of",
        "rsid",
        "kill-test",
    ];
    if !known.contains(&sub) {
        fakesh(&args);
        return;
    }
    let a = &args[2..];
    let res = unsafe {
        match sub {
            "box" => cmd_box(a),
            "capsid" => cmd_capsid(a),
            "acsid" => cmd_acsid(a),
            "usersid" => cmd_usersid(),
            "ac-info" => cmd_ac_info(),
            "open-proc" => cmd_open_proc(a),
            "cred-read" => cmd_cred_read(a),
            "reg-read" => cmd_reg_read(),
            "pipe-create" => cmd_pipe_create(a),
            "pipe-connect" => cmd_pipe_connect(a),
            "pipe-squat" => cmd_pipe_squat(a),
            "pair-test" => cmd_pair_test(a),
            "injob" => cmd_injob(a),
            "tcp-listen" => cmd_tcp_listen(a),
            "connect" => cmd_connect(a),
            "loop-self" => cmd_loop_self(),
            "loop-two" => cmd_loop_two(),
            "try-open" => cmd_try_open(a),
            "mktree" => cmd_mktree(a),
            "g10" => cmd_g10(),
            "g10-run" => cmd_g10_run(),
            "sock-dup" => cmd_sock_dup(a),
            "sock-child" => cmd_sock_child(a),
            "xbox" => rbox::cmd_xbox(a),
            "watch" => rbox::cmd_watch(a),
            "g3n-box" => rbox::cmd_g3n_box(a),
            "objdir-hold" => rbox::cmd_objdir_hold(a),
            "nul-open" => rbox::cmd_nul_open(),
            "sddl-of" => rbox::cmd_sddl_of(a),
            "p17" => rbox::cmd_p17(a),
            "mutex-hold" => rbox::cmd_mutex_hold(a),
            "mutex-open" => rbox::cmd_mutex_open(a),
            "pipe-anon" => rbox::cmd_pipe_anon(),
            "sid-of" => rbox::cmd_sid_of(a),
            "rsid" => rbox::cmd_rsid(a),
            "kill-test" => rbox::cmd_kill_test(a),
            _ => Err("unknown".into()),
        }
    };
    if let Err(e) = res {
        r(sub, format!("ERROR {e}"));
        std::process::exit(1);
    }
}
