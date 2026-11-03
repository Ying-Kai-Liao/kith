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

const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
/// Out of the job of whoever ran `install` (Node puts its children in a job that
/// dies with it, and the service must outlive the app that installed it).
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

/// Stops the running service. Its configuration stays for a later start; a
/// manually started `kith-serve serve` is left alone.
pub fn stop() -> Result<()> {
    // Under the holder (opt-in, see `holder_wanted`) the supervising holder goes first, or it would start the host again.
    // Only that one, found by its folder: another holder on the machine (the core's detached one, which must outlive a
    // host restart, or a dev identity's) is never stopped by its image name.
    if let Ok((program, args)) = configured()
        && program.file_name().is_some_and(|n| n.eq_ignore_ascii_case(HOLDER))
        && let Some(dir) = holder_dir(&args)
    {
        stop_supervising_holder(dir);
    }
    // Not running is fine; `/T` takes the host and its agents with the supervisor.
    let _ = Command::new("taskkill")
        .args(["/F", "/T", "/IM", SUPERVISOR])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    wait_for_host_exit()
}

/// Ends the `kith-holder supervise` process whose command line names `dir`, with its tree (the host and the core it
/// supervises). The folder goes in through the environment so it needs no quoting.
fn stop_supervising_holder(dir: &str) {
    let script = "Get-CimInstance Win32_Process -Filter \"Name='kith-holder.exe'\" \
        | Where-Object { $_.CommandLine -like '*supervise*' -and $_.CommandLine.Contains($env:KITH_HOLDER_DIR) } \
        | ForEach-Object { taskkill /F /T /PID $_.ProcessId }";
    let _ = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("KITH_HOLDER_DIR", dir)
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
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
    let supervisor = if program.file_name().is_some_and(|n| n == HOLDER) {
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

/// Best effort: every step tolerates "already gone".
pub fn uninstall() {
    let _ = Command::new("reg").args(["delete", RUN_KEY, "/v", VALUE, "/f"]).stderr(Stdio::null()).status();
    let _ = stop();
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
