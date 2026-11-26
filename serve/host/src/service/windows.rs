//! The background service on Windows: the per-user Run key starts the host at
//! sign-in through `kith-servew.exe`, which keeps it running without a console
//! window (`src/bin/kith-servew.rs`).

use super::runvalue::{holder_dir, parse_run_value};
use super::{current_exe, data_dir, holder_words, host_program, run, wait_for_host_exit};
use anyhow::{Context, Result, bail, ensure};
use std::os::windows::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE: &str = crate::environment::WINDOWS_VALUE;
const SUPERVISOR: &str = crate::environment::SUPERVISOR;
const HOST: &str = "kith-serve.exe";
const HOLDER: &str = "kith-holder.exe";

/// How long `stop` lets the host leave on its own before it ends it.
const GRACE: std::time::Duration = std::time::Duration::from_secs(10);

const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
/// Out of the job of whoever ran `install` (Node puts its children in a job that
/// dies with it, and the service must outlive the app that installed it).
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

/// Stops the running service. Its configuration stays for a later start; a
/// manually started `kith-serve serve` is left alone.
pub fn stop() -> Result<()> {
    stop_gracefully();
    // Under the holder (opt-in, see `holder_wanted`) the supervising holder goes first, or it would start the host again.
    // Only that one, found by its folder: another holder on the machine (the core's detached one, which must outlive a
    // host restart, or a dev identity's) is never stopped by its image name.
    if let Ok((program, args)) = configured()
        && program.file_name().is_some_and(|n| n.eq_ignore_ascii_case(HOLDER))
        && let Some(dir) = holder_dir(&args)
    {
        stop_supervising_holder(dir)?;
    }
    // Not running is fine; `/T` takes the host and its agents with the supervisor.
    let _ = Command::new("taskkill")
        .args(["/F", "/T", "/IM", SUPERVISOR])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    wait_for_host_exit()
}

/// Signals the host's named stop event and waits up to [`GRACE`] for the host to leave, so it stops its agents and its core
/// the way it does on SIGTERM. No event (no host, or one older than the event) or a host that stays: the caller ends it hard.
/// The supervisor may start the host again in the meantime; the caller's `taskkill /T` takes that one too.
fn stop_gracefully() {
    let name = kith_winsys::event::stop_event_name(&data_dir());
    let Ok(Some(event)) = kith_winsys::event::StopEvent::open(&name) else { return };
    if event.signal().is_err() {
        return;
    }
    // `taskkill /T` walks down from a live parent. Once the host has left, what it started (the core's holder, which
    // outlives its starter on purpose) hangs from nothing, and `stop` is meant to end the whole tree. So the tree is
    // listed before the host is asked to leave and ended after it.
    let tree = supervisor_tree();
    let deadline = std::time::Instant::now() + GRACE;
    while std::time::Instant::now() < deadline {
        // The host holds `host.lock` until it is gone.
        if super::lock_host().is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    end_listed(&tree);
}

/// Runs a PowerShell script (the environment carries its input, so nothing needs quoting) and returns its output lines.
fn powershell(script: &str, env: &[(&str, &str)]) -> Vec<String> {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let exe = Path::new(&root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let mut command = Command::new(exe);
    command
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    for (k, v) in env {
        command.env(k, v);
    }
    command
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout).lines().map(|l| l.trim().to_owned()).filter(|l| !l.is_empty()).collect()
        })
        .unwrap_or_default()
}

/// Every process below a running `kith-servew.exe`, as `pid name`.
fn supervisor_tree() -> Vec<String> {
    let script = "$all = @(Get-CimInstance Win32_Process); \
        $todo = @($all | Where-Object { $_.Name -eq $env:KITH_ROOT_IMAGE } | ForEach-Object { $_.ProcessId }); \
        $seen = @{}; \
        while ($todo.Count -gt 0) { \
          $next = @(); \
          foreach ($p in $todo) { foreach ($c in @($all | Where-Object { $_.ParentProcessId -eq $p })) { \
            if (-not $seen.ContainsKey($c.ProcessId)) { $seen[$c.ProcessId] = $c.Name; $next += $c.ProcessId } } } \
          $todo = $next } \
        $seen.GetEnumerator() | ForEach-Object { \"$($_.Key) $($_.Value)\" }";
    powershell(script, &[("KITH_ROOT_IMAGE", SUPERVISOR)])
}

/// Ends the listed processes that are still running under the same name (a reused pid is left alone).
fn end_listed(tree: &[String]) {
    if tree.is_empty() {
        return;
    }
    let script = "foreach ($l in ($env:KITH_LISTED -split ';')) { $pid_, $name = $l -split ' ', 2; \
        $p = Get-Process -Id $pid_ -ErrorAction SilentlyContinue; \
        if ($p -and ($p.ProcessName + '.exe') -eq $name) { taskkill /F /PID $pid_ | Out-Null } }";
    powershell(script, &[("KITH_LISTED", &tree.join(";"))]);
}

/// Ends the `kith-holder supervise` process whose command line names `dir` as a whole quoted word (install quotes every
/// word, so `...\holder` never matches `...\holder-dev`), with its tree (the host and the core it supervises). The
/// folder goes in through the environment so it needs no quoting. A failure is an error: a supervisor left running
/// would start the host again under the new one.
fn stop_supervising_holder(dir: &str) -> Result<()> {
    // Contains("") matches every process.
    ensure!(!dir.trim().is_empty(), "the installed holder service names no folder");
    let script = "$m = @(Get-CimInstance Win32_Process -Filter \"Name='kith-holder.exe'\" \
        | Where-Object { $_.CommandLine.Contains('\"supervise\"') -and $_.CommandLine.Contains('\"' + $env:KITH_HOLDER_DIR + '\"') }); \
        $m | ForEach-Object { taskkill /F /T /PID $_.ProcessId | Out-Null }; \
        Start-Sleep -Milliseconds 500; \
        $left = @($m | Where-Object { Get-Process -Id $_.ProcessId -ErrorAction SilentlyContinue }); \
        if ($left.Count -gt 0) { exit 1 }";
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let powershell = Path::new(&root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let status = Command::new(&powershell)
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("KITH_HOLDER_DIR", dir)
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("running {}", powershell.display()))?;
    ensure!(
        status.success(),
        "could not stop the running holder service ({status}); end kith-holder.exe for {dir} by hand"
    );
    Ok(())
}

pub fn start() -> Result<()> {
    let (program, args) = configured()?;
    // No inherited pipes: the supervisor never exits, and whoever ran `install` (the app) would
    // wait on them forever.
    let spawn = |flags| {
        Command::new(&program)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(flags)
            .spawn()
    };
    let detached = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
    spawn(detached | CREATE_BREAKAWAY_FROM_JOB)
        .or_else(|_| spawn(detached))
        .with_context(|| format!("starting {}", program.display()))?;
    Ok(())
}

/// Refuse to replace one installation while restarting a service owned by another.
pub fn require_executable(expected: &Path) -> Result<()> {
    let (program, args) = configured()?;
    // Under the holder the Run key starts `kith-holder supervise ... -- kith-servew serve ...`: the host is what follows `--`.
    let supervisor = if program.file_name().is_some_and(|n| n.eq_ignore_ascii_case(HOLDER)) {
        PathBuf::from(host_program(&args).context("cannot read the installed host service")?)
    } else {
        program
    };
    if supervisor.with_file_name(HOST).canonicalize()? != expected.canonicalize()? {
        bail!("the installed service runs another host binary; run that binary's update command");
    }
    Ok(())
}

/// Registers the host to start at sign-in and starts it now.
pub fn install(port: u16) -> Result<()> {
    let supervisor = current_exe().context("locating the kith-serve binary")?.with_file_name(SUPERVISOR);
    ensure!(supervisor.is_file(), "{} is missing; reinstall Kith", supervisor.display());
    std::fs::create_dir_all(data_dir()).context("creating the data directory")?;
    stop()?;
    let mut command = format!("\"{}\" serve --port {port}", supervisor.display());
    // Opt-in: `kith-holder supervise ... --` in front. Every word is quoted so a path with spaces stays one word.
    if let Some(words) = holder_words()? {
        let quoted: Vec<String> = words.iter().map(|w| format!("\"{w}\"")).collect();
        command = format!("{} {command}", quoted.join(" "));
    }
    run("reg", &["add", RUN_KEY, "/v", VALUE, "/t", "REG_SZ", "/d", &command, "/f"])?;
    start()
}

/// No launchd/systemd here, so nothing to guard.
pub fn guard() -> Result<()> {
    Ok(())
}

/// Best effort: every step tolerates "already gone".
pub fn uninstall() -> Result<()> {
    let _ = Command::new("reg").args(["delete", RUN_KEY, "/v", VALUE, "/f"]).stderr(Stdio::null()).status();
    let _ = stop();
    Ok(())
}

pub fn installed() -> bool {
    configured().is_ok()
}

/// The supervisor and arguments the Run key starts.
fn configured() -> Result<(PathBuf, Vec<String>)> {
    let output = Command::new("reg").args(["query", RUN_KEY, "/v", VALUE]).output().context("running reg")?;
    ensure!(output.status.success(), "the host service isn't installed");
    parse_run_value(&String::from_utf8_lossy(&output.stdout)).context("cannot read the installed host service")
}
