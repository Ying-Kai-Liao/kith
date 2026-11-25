//! W20b additions: variant R (a restricted token in a job) next to variant A (AppContainer), one `xbox` front for both,
//! a host-side process watcher (P-G3n), the in-box native escape attempts, a host-made MSYS object directory (P-G2c),
//! NUL (P-G18), the per-install-SID isolation test (P-G17), and the kill-time measure (F23 again). Nothing here ships.
use super::*;

use std::os::windows::process::CommandExt;
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertStringSidToSidW, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    ACL, CreateRestrictedToken, GetSecurityDescriptorDacl, IsTokenRestricted, SetTokenInformation, TOKEN_DEFAULT_DACL,
    TOKEN_MANDATORY_LABEL,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::System::Threading::{CreateMutexW, OpenMutexW, ResumeThread};

pub(super) unsafe fn sid_from(s: &str) -> Res<PSID> {
    let mut sid: PSID = null_mut();
    if ConvertStringSidToSidW(wide(s).as_ptr(), &mut sid) == 0 {
        return Err(format!("ConvertStringSidToSid({s}) err {}", last()));
    }
    Ok(sid)
}

/// This process's logon SID (the desktop, window station and BaseNamedObjects directory grant it; a restricted token that
/// leaves it out may not even initialise).
unsafe fn logon_sid() -> Res<PSID> {
    let mut t: HANDLE = null_mut();
    if OpenProcessToken(GetCurrentProcess(), 8, &mut t) == 0 {
        return Err(format!("OpenProcessToken err {}", last()));
    }
    let mut buf = vec![0u64; 128];
    let mut n = 0u32;
    if GetTokenInformation(t, 28, buf.as_mut_ptr().cast(), 1024, &mut n) == 0 {
        return Err(format!("GetTokenInformation(TokenLogonSid) err {}", last()));
    }
    // TOKEN_GROUPS: u32 count (padded to 8), then SID_AND_ATTRIBUTES[]; the first Sid pointer sits at offset 8.
    let sid = *buf.as_ptr().cast::<u8>().add(8).cast::<PSID>();
    // the SID lives in `buf`: copy it out so it outlives this call
    let len = windows_sys::Win32::Security::GetLengthSid(sid) as usize;
    let keep = Box::leak(vec![0u8; len].into_boxed_slice());
    std::ptr::copy_nonoverlapping(sid.cast::<u8>(), keep.as_mut_ptr(), len);
    Ok(keep.as_mut_ptr().cast())
}

/// An arbitrary, stable SID for a run or a grant (a domain-style SID nobody owns): capability SIDs may not be accepted as
/// restricting SIDs, and the restricting list takes any SID.
unsafe fn run_sid(name: &str) -> Res<PSID> {
    let fnv = |seed: u32| {
        let mut h: u32 = 2_166_136_261 ^ seed.wrapping_mul(16_777_619);
        for b in name.bytes() {
            h ^= u32::from(b);
            h = h.wrapping_mul(16_777_619);
        }
        h
    };
    sid_from(&format!("S-1-5-21-{}-{}-{}-{}", fnv(1), fnv(2), fnv(3), 1000 + fnv(4) % 100_000))
}

pub unsafe fn cmd_rsid(a: &[String]) -> Res<()> {
    println!("{}", sid_str(run_sid(a.first().ok_or("name")?)?));
    Ok(())
}

pub(super) unsafe fn sddl_to_sd(sddl: &str) -> Res<*mut c_void> {
    let mut sd = null_mut();
    if ConvertStringSecurityDescriptorToSecurityDescriptorW(wide(sddl).as_ptr(), 1, &mut sd, null_mut()) == 0 {
        return Err(format!("SDDL parse ({sddl}) err {}", last()));
    }
    Ok(sd)
}

unsafe fn set_default_dacl(token: HANDLE, sddl: &str) -> Res<()> {
    let sd = sddl_to_sd(sddl)?;
    let (mut present, mut defaulted) = (0, 0);
    let mut dacl: *mut ACL = null_mut();
    if GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted) == 0 {
        return Err(format!("GetSecurityDescriptorDacl err {}", last()));
    }
    let info = TOKEN_DEFAULT_DACL { DefaultDacl: dacl };
    if SetTokenInformation(token, 6, std::ptr::from_ref(&info).cast(), std::mem::size_of_val(&info) as u32) == 0 {
        return Err(format!("SetTokenInformation(TokenDefaultDacl) err {}", last()));
    }
    Ok(())
}

/// One restricted primary token of this process's token. `restrict` is the restricting-SID list; `il` is 0x1000 (Low) or
/// 0x2000 (Medium).
unsafe fn make_rtoken(restrict: &[PSID], disable: &[PSID], il: u32, default_dacl: Option<&str>) -> Res<HANDLE> {
    let mut t: HANDLE = null_mut();
    // ASSIGN_PRIMARY | DUPLICATE | QUERY | ADJUST_DEFAULT
    if OpenProcessToken(GetCurrentProcess(), 0x1 | 0x2 | 0x8 | 0x80, &mut t) == 0 {
        return Err(format!("OpenProcessToken err {}", last()));
    }
    let rs: Vec<SID_AND_ATTRIBUTES> = restrict.iter().map(|s| SID_AND_ATTRIBUTES { Sid: *s, Attributes: 0 }).collect();
    let ds: Vec<SID_AND_ATTRIBUTES> = disable.iter().map(|s| SID_AND_ATTRIBUTES { Sid: *s, Attributes: 0 }).collect();
    let mut nt: HANDLE = null_mut();
    // DISABLE_MAX_PRIVILEGE (1): drops every privilege but SeChangeNotify.
    if CreateRestrictedToken(t, 1, ds.len() as u32, ds.as_ptr(), 0, null(), rs.len() as u32, rs.as_ptr(), &mut nt) == 0
    {
        let e = last();
        let mut each = Vec::new();
        for s in restrict {
            let one = [SID_AND_ATTRIBUTES { Sid: *s, Attributes: 0 }];
            let mut x: HANDLE = null_mut();
            let ok = CreateRestrictedToken(t, 1, 0, null(), 0, null(), 1, one.as_ptr(), &mut x);
            each.push(format!("{}={}", sid_str(*s), if ok != 0 { "ok".to_string() } else { format!("err{}", last()) }));
        }
        return Err(format!("CreateRestrictedToken err {e}; each alone: {}", each.join(" ")));
    }
    let label = TOKEN_MANDATORY_LABEL {
        Label: SID_AND_ATTRIBUTES { Sid: sid_from(&format!("S-1-16-{il}"))?, Attributes: 0x20 },
    };
    if SetTokenInformation(nt, 25, std::ptr::from_ref(&label).cast(), std::mem::size_of_val(&label) as u32) == 0 {
        return Err(format!("SetTokenInformation(IntegrityLevel {il:#x}) err {}", last()));
    }
    if let Some(d) = default_dacl {
        set_default_dacl(nt, d)?;
    }
    Ok(nt)
}

/// F23 again: pre-open every pid of the job, TerminateJobObject, time until each handle signals.
unsafe fn kill_measure(tag: &str, job: HANDLE) {
    let pids = job_pids(job);
    let mut hs = Vec::new();
    for p in &pids {
        let h = OpenProcess(0x10_0000, 0, *p as u32);
        if h.is_null() {
            r(tag, format!("pid {p} SYNCHRONIZE open err {}", last()));
        } else {
            hs.push((*p, h));
        }
    }
    r(tag, format!("pre-kill job_pids={pids:?} active={:?} handles_opened={}", job_active(job), hs.len()));
    let t = Instant::now();
    TerminateJobObject(job, 1);
    let mut zero_at = None;
    while t.elapsed() < Duration::from_secs(10) {
        if job_active(job) == Some(0) {
            zero_at = Some(t.elapsed().as_millis());
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    r(tag, format!("ActiveProcesses==0 after {zero_at:?} ms"));
    for (p, h) in hs {
        let w = WaitForSingleObject(h, 20_000);
        r(tag, format!("pid {p} handle signalled={} after {} ms", w == 0, t.elapsed().as_millis()));
        CloseHandle(h);
    }
}

/// `xbox --kind a|r --name N ...  -- cmd args`: one front for variant A (AppContainer) and variant R (restricted token).
/// A: `--ac PROFILE` (default kithprobe.<name>) `--cap C`... `--wk N`... (well-known capability numbers, 85 internetClient).
/// R: `--run CAP` (default kith.run.<name>) `--grant CAP`... `--il low|medium` `--no-sys` `--restricted-sid`
///    `--no-default-dacl`. Both: `--timeout S --linger S --out F --cwd D --env K=V... --job-name J --kill-measure
///    --keep-profile --pass-std`.
pub unsafe fn cmd_xbox(a: &[String]) -> Res<()> {
    let kind = opt(a, "--kind").unwrap_or_else(|| "r".into());
    let name = opt(a, "--name").ok_or("--name required")?;
    let job = make_job(opt(a, "--job-name").as_deref())?;
    let out_path = opt(a, "--out").unwrap_or_else(|| format!("{}\\xbox-{name}.txt", std::env::temp_dir().display()));
    let out = out_file(&out_path)?;
    let env: Vec<(String, String)> = opts(a, "--env")
        .iter()
        .filter_map(|e| e.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
        .collect();
    let mut sp = Spawn {
        cmdline: cmdline(&tail(a)),
        cwd: opt(a, "--cwd"),
        env,
        ac: None,
        jobs: vec![job],
        out: Some(&out),
        token: None,
        suspended: false,
        proc_sd: null_mut(),
        pass_std: flag(a, "--pass-std"),
    };
    let mut ac_name = String::new();
    if kind == "a" {
        ac_name = opt(a, "--ac").unwrap_or_else(|| format!("kithprobe.{name}"));
        let ac = ac_sid(&ac_name, true)?;
        let mut caps = Vec::new();
        for c in opts(a, "--cap") {
            caps.push(cap_sid(&c)?);
        }
        for w in opts(a, "--wk") {
            caps.push(well_known(w.parse().map_err(|_| "--wk number")?)?);
        }
        r("xbox", format!("{name} kind=a ac={} caps={}", sid_str(ac), caps.len()));
        sp.ac = Some((ac, caps));
    } else {
        let run = run_sid(&opt(a, "--run").unwrap_or_else(|| format!("kith.run.{name}")))?;
        let mut rs = vec![run];
        let mut names = vec![format!("run={}", sid_str(run))];
        for g in opts(a, "--grant") {
            rs.push(run_sid(&g)?);
        }
        if !flag(a, "--no-sys") {
            rs.push(sid_from("S-1-1-0")?); // Everyone
            rs.push(sid_from("S-1-5-32-545")?); // Users
            names.push("Everyone,Users".into());
        }
        if !flag(a, "--no-logon-sid") {
            let l = logon_sid()?;
            names.push(format!("logon={}", sid_str(l)));
            rs.push(l);
        }
        if flag(a, "--restricted-sid") {
            rs.push(sid_from("S-1-5-12")?);
            names.push("RESTRICTED".into());
        }
        if flag(a, "--auth-users") {
            rs.push(sid_from("S-1-5-11")?);
            names.push("AuthenticatedUsers".into());
        }
        if flag(a, "--user-sid") {
            rs.push(sid_from(&token_user_sid()?)?);
            names.push("USER(diagnostic only)".into());
        }
        let il = if opt(a, "--il").as_deref() == Some("low") { 0x1000 } else { 0x2000 };
        let dd = opt(a, "--dd-sddl").or_else(|| {
            (!flag(a, "--no-default-dacl")).then(|| {
                format!("D:(A;;GA;;;{})(A;;GA;;;SY)(A;;GA;;;{})", sid_str(run), token_user_sid().unwrap_or_default())
            })
        });
        let disable: Vec<PSID> = opts(a, "--disable-sid").iter().map(|x| sid_from(x)).collect::<Res<_>>()?;
        let tok = make_rtoken(&rs, &disable, il, dd.as_deref())?;
        r("xbox", format!("{name} kind=r il={il:#x} restricting={} total={}", names.join(","), rs.len()));
        sp.token = Some(tok);
    }
    let t0 = Instant::now();
    let pi = spawn(&sp).map_err(|e| format!("spawn: {e}"))?;
    r("xbox", format!("{name} spawned pid={}", pi.dwProcessId));
    let timeout: u64 = opt(a, "--timeout").and_then(|s| s.parse().ok()).unwrap_or(60);
    match wait(&pi, timeout) {
        Some(c) => r("xbox", format!("{name} exit={c} ms={}", t0.elapsed().as_millis())),
        None => r("xbox", format!("{name} TIMEOUT after {timeout}s (process killed)")),
    }
    r("xbox", format!("{name} job_active_after_exit={:?} job_pids={:?}", job_active(job), job_pids(job)));
    if let Some(s) = opt(a, "--linger").and_then(|s| s.parse::<u64>().ok()) {
        std::thread::sleep(Duration::from_secs(s));
        r("xbox", format!("{name} job_active_after_linger={:?} job_pids={:?}", job_active(job), job_pids(job)));
    }
    dump(&out_path, &name);
    if flag(a, "--kill-measure") {
        kill_measure(&format!("kill-{name}"), job);
    } else {
        TerminateJobObject(job, 1);
    }
    CloseHandle(job);
    CloseHandle(pi.hProcess);
    CloseHandle(pi.hThread);
    if kind == "a" && !flag(a, "--keep-profile") && opt(a, "--ac").is_none() {
        DeleteAppContainerProfile(wide(&ac_name).as_ptr());
    }
    Ok(())
}

// ---- P-G3n: the host watches every new process --------------------------------------------------------------------

struct ProcInfo {
    ac: i32,
    restricted: i32,
    il: String,
    in_job: Option<bool>,
}

unsafe fn proc_info(pid: u32, job: HANDLE) -> Option<ProcInfo> {
    let p = OpenProcess(0x1000, 0, pid);
    if p.is_null() {
        return None;
    }
    let mut t: HANDLE = null_mut();
    let mut info = ProcInfo { ac: -1, restricted: -1, il: "?".into(), in_job: None };
    if !job.is_null() {
        let mut res = 0;
        if IsProcessInJob(p, job, &mut res) != 0 {
            info.in_job = Some(res != 0);
        }
    }
    if OpenProcessToken(p, 8, &mut t) != 0 {
        let mut n = 0u32;
        let mut is_ac = 0u32;
        if GetTokenInformation(t, 29, std::ptr::from_mut(&mut is_ac).cast(), 4, &mut n) != 0 {
            info.ac = is_ac as i32;
        }
        info.restricted = IsTokenRestricted(t);
        let mut buf = vec![0u64; 64];
        if GetTokenInformation(t, 25, buf.as_mut_ptr().cast(), 512, &mut n) != 0 {
            info.il = sid_str(*buf.as_ptr().cast::<PSID>());
        }
        CloseHandle(t);
    } else {
        info.il = format!("token-open-err-{}", last());
    }
    CloseHandle(p);
    Some(info)
}

unsafe fn snapshot() -> Vec<(u32, u32, String)> {
    let s = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
    let mut v = Vec::new();
    if s == INVALID_HANDLE_VALUE {
        return v;
    }
    let mut e: PROCESSENTRY32W = std::mem::zeroed();
    e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
    let mut ok = Process32FirstW(s, &mut e);
    while ok != 0 {
        let n = e.szExeFile.iter().position(|c| *c == 0).unwrap_or(260);
        v.push((e.th32ProcessID, e.th32ParentProcessID, String::from_utf16_lossy(&e.szExeFile[..n])));
        ok = Process32NextW(s, &mut e);
    }
    CloseHandle(s);
    v
}

/// `watch --secs N [--job NAME]`: records every process that appears while it runs, with parent, token and job.
pub unsafe fn cmd_watch(a: &[String]) -> Res<()> {
    let secs: u64 = opt(a, "--secs").and_then(|s| s.parse().ok()).unwrap_or(60);
    let jobname = opt(a, "--job");
    let mut job: HANDLE = null_mut();
    let mut seen: std::collections::HashSet<u32> = snapshot().iter().map(|p| p.0).collect();
    r("watch", format!("baseline {} processes; watching {secs}s job={jobname:?}", seen.len()));
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if job.is_null() {
            if let Some(j) = &jobname {
                job = OpenJobObjectW(4, 0, wide(j).as_ptr());
            }
        }
        for (pid, ppid, name) in snapshot() {
            if !seen.insert(pid) {
                continue;
            }
            match proc_info(pid, job) {
                Some(i) => r(
                    "watch",
                    format!(
                        "new pid={pid} name={name} ppid={ppid} appcontainer={} restricted={} il={} in_run_job={:?}",
                        i.ac, i.restricted, i.il, i.in_job
                    ),
                ),
                None => {
                    r("watch", format!("new pid={pid} name={name} ppid={ppid} unreadable (gone, or another identity)"))
                }
            }
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    Ok(())
}

fn lg(dir: &str, m: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(format!("{dir}\\g3n.log")) {
        let _ = f.write_all(format!("{m}\n").as_bytes());
    }
}

/// Runs INSIDE the box. Every start is `ping -n <N>`; N names the method:
/// 51 CreateProcess BREAKAWAY, 52 cmd start, 53 ShellExecuteEx open, 54 ShellExecuteEx runas, 55 WScript.Shell.Run,
/// 56 Shell.Application.ShellExecute, 57 WMI Win32_Process.Create (those three from cscript g3n.js), 58 schtasks /run,
/// 59 sc start, 60 MMC20.Application (out-of-process COM, from g3n.js), 61 BITS notify command.
pub unsafe fn cmd_g3n_box(a: &[String]) -> Res<()> {
    let dir = a.first().ok_or("outdir")?.clone();
    let js = a.get(1).cloned().unwrap_or_else(|| "C:\\probe\\bin\\g3n.js".into());
    lg(&dir, "box native started");
    let ping = |n: u32| vec!["-n".to_string(), n.to_string(), "127.0.0.1".to_string()];
    match std::process::Command::new("ping.exe")
        .args(ping(51))
        .creation_flags(0x0100_0000 | 0x0800_0000) // BREAKAWAY_FROM_JOB | NO_WINDOW
        .spawn()
    {
        Ok(c) => lg(&dir, &format!("51 CreateProcess BREAKAWAY ok pid={}", c.id())),
        Err(e) => lg(&dir, &format!("51 CreateProcess BREAKAWAY FAILED {e} os={:?}", e.raw_os_error())),
    }
    match std::process::Command::new("cmd.exe").args(["/c", "start", "/b", "ping", "-n", "52", "127.0.0.1"]).status() {
        Ok(s) => lg(&dir, &format!("52 cmd start rc={s}")),
        Err(e) => lg(&dir, &format!("52 cmd start FAILED {e}")),
    }
    for (n, verb) in [(53u32, "open"), (54, "runas")] {
        let d = dir.clone();
        // runas can wait forever for a consent prompt nobody sees: never block the rest on it
        let h = std::thread::spawn(move || {
            let mut sei: windows_sys::Win32::UI::Shell::SHELLEXECUTEINFOW = std::mem::zeroed();
            sei.cbSize = std::mem::size_of_val(&sei) as u32;
            sei.fMask = 0x40 | 0x400; // NOCLOSEPROCESS | FLAG_NO_UI
            let v = wide(verb);
            let f = wide("ping.exe");
            let p = wide(&format!("-n {n} 127.0.0.1"));
            sei.lpVerb = v.as_ptr();
            sei.lpFile = f.as_ptr();
            sei.lpParameters = p.as_ptr();
            sei.nShow = 0;
            let ok = windows_sys::Win32::UI::Shell::ShellExecuteExW(&mut sei);
            lg(&d, &format!("{n} ShellExecuteEx {verb} ok={ok} err={} hInstApp={:?}", last(), sei.hInstApp));
        });
        std::thread::sleep(Duration::from_secs(4));
        if !h.is_finished() {
            lg(&dir, &format!("{n} ShellExecuteEx {verb} still blocked after 4 s (left running)"));
        }
    }
    let o = std::process::Command::new("cscript.exe").args(["//nologo", &js, &format!("{dir}\\g3n.log")]).output();
    lg(&dir, &format!("55-57,60 cscript g3n.js: {:?}", o.map(|o| String::from_utf8_lossy(&o.stdout).to_string())));
    let o = std::process::Command::new("schtasks.exe")
        .args(["/create", "/tn", "kithprobe58", "/tr", "ping.exe -n 58 127.0.0.1", "/sc", "once", "/st", "23:59", "/f"])
        .output();
    lg(
        &dir,
        &format!(
            "58 schtasks create: {:?}",
            o.map(|o| format!("rc={} {}", o.status, String::from_utf8_lossy(&o.stdout)))
        ),
    );
    let o = std::process::Command::new("schtasks.exe").args(["/run", "/tn", "kithprobe58"]).output();
    lg(
        &dir,
        &format!("58 schtasks run: {:?}", o.map(|o| format!("rc={} {}", o.status, String::from_utf8_lossy(&o.stdout)))),
    );
    let o = std::process::Command::new("sc.exe")
        .args(["create", "kithprobesvc59", "binPath=", "cmd.exe /c ping -n 59 127.0.0.1"])
        .output();
    lg(
        &dir,
        &format!("59 sc create: {:?}", o.map(|o| format!("rc={} {}", o.status, String::from_utf8_lossy(&o.stdout)))),
    );
    let o = std::process::Command::new("sc.exe").args(["start", "kithprobesvc59"]).output();
    lg(
        &dir,
        &format!("59 sc start: {:?}", o.map(|o| format!("rc={} {}", o.status, String::from_utf8_lossy(&o.stdout)))),
    );
    for args in [
        vec!["/create", "kithprobe61"],
        vec!["/addfile", "kithprobe61", "file:///C:/probe/bin/g3n.js", &format!("{dir}\\bits.js")],
        vec!["/setnotifycmdline", "kithprobe61", "ping.exe", "-n 61 127.0.0.1"],
        vec!["/resume", "kithprobe61"],
    ] {
        let o = std::process::Command::new("bitsadmin.exe").args(&args).output();
        lg(
            &dir,
            &format!(
                "61 bitsadmin {}: {:?}",
                args[0],
                o.map(|o| format!("rc={} {}", o.status, String::from_utf8_lossy(&o.stdout).trim()))
            ),
        );
    }
    std::thread::sleep(Duration::from_secs(4));
    lg(&dir, "box native done");
    std::fs::write(format!("{dir}\\g3n-done.txt"), "done").ok();
    Ok(())
}

// ---- P-G2c: a host-made MSYS object directory --------------------------------------------------------------------

#[repr(C)]
struct UStr {
    len: u16,
    max: u16,
    buf: *mut u16,
}

#[repr(C)]
struct ObjAttr {
    len: u32,
    root: HANDLE,
    name: *const UStr,
    attr: u32,
    sd: *mut c_void,
    sqos: *mut c_void,
}

/// `objdir-hold \BaseNamedObjects\msys-2.0S5-<key> --sddl SDDL [--hold S]`: NtCreateDirectoryObject with an SD, held open.
pub unsafe fn cmd_objdir_hold(a: &[String]) -> Res<()> {
    let path = a.first().ok_or("object path")?;
    let h = GetModuleHandleW(wide("ntdll.dll").as_ptr());
    let f = GetProcAddress(h, b"NtCreateDirectoryObject\0".as_ptr()).ok_or("no NtCreateDirectoryObject")?;
    type F = unsafe extern "system" fn(*mut HANDLE, u32, *const ObjAttr) -> i32;
    let f: F = std::mem::transmute(f);
    let mut w: Vec<u16> = path.encode_utf16().collect();
    let us = UStr { len: (w.len() * 2) as u16, max: (w.len() * 2) as u16, buf: w.as_mut_ptr() };
    let sd = match opt(a, "--sddl") {
        Some(s) => sddl_to_sd(&s)?,
        None => null_mut(),
    };
    let oa = ObjAttr {
        len: std::mem::size_of::<ObjAttr>() as u32,
        root: null_mut(),
        name: &us,
        attr: 0x40, // OBJ_CASE_INSENSITIVE
        sd,
        sqos: null_mut(),
    };
    let mut dir: HANDLE = null_mut();
    let st = f(&mut dir, 0x000F_000F, &oa);
    r("objdir", format!("{path} NtCreateDirectoryObject status={:#x} (0 = created)", st as u32));
    if st == 0 {
        std::thread::sleep(Duration::from_secs(opt(a, "--hold").and_then(|s| s.parse().ok()).unwrap_or(30)));
        CloseHandle(dir);
    }
    Ok(())
}

// ---- P-G18: NUL --------------------------------------------------------------------------------------------------

pub unsafe fn cmd_nul_open() -> Res<()> {
    for path in ["NUL", "\\\\.\\NUL", "\\\\?\\GLOBALROOT\\Device\\Null"] {
        for (label, access) in
            [("read", 0x8000_0000u32), ("write", 0x4000_0000), ("read+write", 0xC000_0000), ("READ_CONTROL", 0x2_0000)]
        {
            let h = CreateFileW(wide(path).as_ptr(), access, 3, null(), OPEN_EXISTING, 0, null_mut());
            if h == INVALID_HANDLE_VALUE {
                r("nul", format!("{path} {label} DENIED err={}", last()));
            } else {
                r("nul", format!("{path} {label} OPENED"));
                CloseHandle(h);
            }
        }
    }
    cmd_sddl_of(&["\\\\.\\NUL".into()])
}

/// `sddl-of PATH`: owner, DACL and integrity label of a file, folder or device, read with READ_CONTROL.
pub unsafe fn cmd_sddl_of(a: &[String]) -> Res<()> {
    let path = a.first().ok_or("path")?;
    let h = CreateFileW(wide(path).as_ptr(), 0x2_0000, 7, null(), OPEN_EXISTING, 0x0200_0000, null_mut());
    if h == INVALID_HANDLE_VALUE {
        r("sddl", format!("{path} open(READ_CONTROL) err={}", last()));
        return Ok(());
    }
    let mut sd = null_mut();
    let e =
        GetSecurityInfo(h, SE_FILE_OBJECT, 0x1 | 0x4 | 0x10, null_mut(), null_mut(), null_mut(), null_mut(), &mut sd);
    if e != 0 {
        r("sddl", format!("{path} GetSecurityInfo err={e}"));
        return Ok(());
    }
    let mut s: *mut u16 = null_mut();
    if ConvertSecurityDescriptorToStringSecurityDescriptorW(sd, 1, 0x1 | 0x4 | 0x10, &mut s, null_mut()) == 0 {
        r("sddl", format!("{path} convert err={}", last()));
    } else {
        r("sddl", format!("{path} {}", from_pwstr(s)));
    }
    CloseHandle(h);
    Ok(())
}

pub unsafe fn cmd_sid_of(a: &[String]) -> Res<()> {
    println!("{}", sid_str(sid_from(a.first().ok_or("sid")?)?));
    Ok(())
}

// ---- P-G17: one per-install AppContainer SID, per-run capability SIDs in the default DACL ----------------------------

pub unsafe fn cmd_mutex_hold(a: &[String]) -> Res<()> {
    let name = a.first().ok_or("name")?;
    let m = CreateMutexW(null(), 0, wide(name).as_ptr());
    r("mutex", format!("{name} create handle_null={} err={}", m.is_null(), last()));
    std::thread::sleep(Duration::from_secs(opt(a, "--hold").and_then(|s| s.parse().ok()).unwrap_or(20)));
    Ok(())
}

pub unsafe fn cmd_mutex_open(a: &[String]) -> Res<()> {
    let name = a.first().ok_or("name")?;
    let m = OpenMutexW(0x10_0000, 0, wide(name).as_ptr());
    if m.is_null() {
        r("mutexopen", format!("{name} DENIED/absent err={}", last()));
    } else {
        r("mutexopen", format!("{name} OPENED"));
        CloseHandle(m);
    }
    Ok(())
}

/// `p17`: box A (capability A) and box B (capability B) share one AppContainer SID. A's process gets an explicit SD and a
/// default DACL naming only capability A and the host user. Can B open A's process and A's mutex? A control box with
/// capability A can (so the SD works).
pub unsafe fn cmd_p17(_a: &[String]) -> Res<()> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?.display().to_string();
    let dir = std::env::temp_dir().display().to_string();
    let ac = ac_sid("kithprobe.inst", true)?;
    let (cap_a, cap_b) = (cap_sid("kith.run.A")?, cap_sid("kith.run.B")?);
    let user = token_user_sid()?;
    let sddl = format!("D:(A;;GA;;;{})(A;;GA;;;{user})", sid_str(cap_a));
    let sd = sddl_to_sd(&sddl)?;
    let job = make_job(None)?;
    let fa = out_file(&format!("{dir}\\p17-a.txt"))?;
    let mk = |caps: Vec<PSID>, args: Vec<String>, o: &File, sd: *mut c_void, suspended: bool| {
        spawn(&Spawn {
            cmdline: cmdline(&args),
            cwd: None,
            env: vec![],
            ac: Some((ac, caps)),
            jobs: vec![job],
            out: Some(o),
            token: None,
            suspended,
            proc_sd: sd,
            pass_std: false,
        })
    };
    let pa = mk(
        vec![cap_a],
        vec![exe.clone(), "mutex-hold".into(), "kith-p17-mutex".into(), "--hold".into(), "40".into()],
        &fa,
        sd,
        true,
    )?;
    let mut tok: HANDLE = null_mut();
    if OpenProcessToken(pa.hProcess, 0x80 | 0x8, &mut tok) == 0 {
        r("p17", format!("open A's token for ADJUST_DEFAULT err={}", last()));
    } else {
        match set_default_dacl(tok, &sddl) {
            Ok(()) => r("p17", "TokenDefaultDacl set on A's lowbox token before it ran"),
            Err(e) => r("p17", format!("set default DACL on A's lowbox token FAILED: {e}")),
        }
    }
    ResumeThread(pa.hThread);
    std::thread::sleep(Duration::from_secs(3));
    r("p17", format!("A pid={} sd={sddl}", pa.dwProcessId));
    let run = |who: &str, cap: PSID, args: Vec<String>| -> Res<()> {
        let path = format!("{dir}\\p17-{who}.txt");
        let o = out_file(&path)?;
        let p = mk(vec![cap], args, &o, null_mut(), false)?;
        wait(&p, 20);
        dump(&path, who);
        Ok(())
    };
    let pid = pa.dwProcessId.to_string();
    run("B-opens-A-process", cap_b, vec![exe.clone(), "open-proc".into(), pid.clone()])?;
    run("A2-opens-A-process-control", cap_a, vec![exe.clone(), "open-proc".into(), pid])?;
    run("B-opens-A-mutex", cap_b, vec![exe.clone(), "mutex-open".into(), "kith-p17-mutex".into()])?;
    run("A2-opens-A-mutex-control", cap_a, vec![exe.clone(), "mutex-open".into(), "kith-p17-mutex".into()])?;
    r("p17", "host (same user, unboxed) opens A's process:");
    cmd_open_proc(&[pa.dwProcessId.to_string()])?;
    TerminateJobObject(job, 1);
    DeleteAppContainerProfile(wide("kithprobe.inst").as_ptr());
    Ok(())
}

// ---- P-G2f: anonymous pipes ----------------------------------------------------------------------------------------

pub unsafe fn cmd_pipe_anon() -> Res<()> {
    let (mut rd, mut wr): (HANDLE, HANDLE) = (null_mut(), null_mut());
    if windows_sys::Win32::System::Pipes::CreatePipe(&mut rd, &mut wr, null(), 0) == 0 {
        r("pipeanon", format!("CreatePipe (anonymous) FAILED err={}", last()));
    } else {
        let mut f = <File as std::os::windows::io::FromRawHandle>::from_raw_handle(wr);
        let _ = f.write_all(b"x");
        r("pipeanon", "CreatePipe (anonymous) OK, wrote 1 byte");
        CloseHandle(rd);
    }
    // Rust's std makes its stdio pipes as NAMED pipes with a random name: this fails in a box that cannot make one.
    match std::process::Command::new("cmd.exe").args(["/c", "echo", "pipe-child-ok"]).output() {
        Ok(o) => r(
            "pipeanon",
            format!(
                "std Command::output (a child with piped stdout) OK: {}",
                String::from_utf8_lossy(&o.stdout).trim()
            ),
        ),
        Err(e) => r("pipeanon", format!("std Command::output FAILED {e} os={:?}", e.raw_os_error())),
    }
    Ok(())
}

/// In-box workload for the kill measure when no bash is available: a few detached children.
pub unsafe fn cmd_kill_test(_a: &[String]) -> Res<()> {
    for n in 90..93 {
        let _ = std::process::Command::new("ping.exe").args(["-n", &n.to_string(), "127.0.0.1"]).spawn();
    }
    let _ = std::process::Command::new("cmd.exe").args(["/c", "start", "/b", "ping", "-n", "93", "127.0.0.1"]).status();
    std::thread::sleep(Duration::from_secs(40));
    Ok(())
}

/// The kith-boxsh stand-in's boxed mode (KITH_BOX=r): runs the real bash in a restricted token and a job with Claude
/// Code's own stdin/stdout/stderr, and returns its exit code. Env: KITH_BOX_RUN (capability name of the run), KITH_BOX_GRANTS
/// (comma list of capability names), KITH_BOX_IL (low|medium), KITH_BOX_RESTRICTED_SID=1.
pub unsafe fn shim_box(real: &str, args: &[String]) -> Res<i32> {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let run = run_sid(&env("KITH_BOX_RUN").unwrap_or_else(|| "kith.run.shim".into()))?;
    let mut rs = vec![run, sid_from("S-1-1-0")?, sid_from("S-1-5-32-545")?];
    for g in env("KITH_BOX_GRANTS").unwrap_or_default().split(',').filter(|g| !g.is_empty()) {
        rs.push(run_sid(g)?);
    }
    if env("KITH_BOX_RESTRICTED_SID").is_some() {
        rs.push(sid_from("S-1-5-12")?);
    }
    let il = if env("KITH_BOX_IL").as_deref() == Some("low") { 0x1000 } else { 0x2000 };
    let dd = format!("D:(A;;GA;;;{})(A;;GA;;;SY)(A;;GA;;;{})", sid_str(run), token_user_sid()?);
    let tok = make_rtoken(&rs, &[], il, Some(&dd))?;
    let job = make_job(None)?;
    let mut cl = vec![real.to_string()];
    cl.extend(args.iter().cloned());
    let pi = spawn(&Spawn {
        cmdline: cmdline(&cl),
        cwd: None,
        env: vec![],
        ac: None,
        jobs: vec![job],
        out: None,
        token: Some(tok),
        suspended: false,
        proc_sd: null_mut(),
        pass_std: true,
    })?;
    WaitForSingleObject(pi.hProcess, 0xFFFF_FFFF);
    let mut code = 0u32;
    GetExitCodeProcess(pi.hProcess, &mut code);
    TerminateJobObject(job, 1);
    Ok(code as i32)
}
