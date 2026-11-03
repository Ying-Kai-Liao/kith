//! The background service on Windows: the per-user Run key starts the host at
//! sign-in through `kith-servew.exe`, which keeps it running without a console
//! window (`src/bin/kith-servew.rs`).

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
    // Not running is fine; `/T` takes the host and its agents with the supervisor. A holder that supervises (opt-in, see
    // `holder_wanted`) goes first, or it would start the host again.
    for image in [HOLDER, SUPERVISOR] {
        let _ = Command::new("taskkill")
            .args(["/F", "/T", "/IM", image])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    wait_for_host_exit()
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

/// `    KithServe    REG_SZ    "C:\…\kith-servew.exe" serve --port 19222` → its parts.
fn parse_run_value(query: &str) -> Option<(PathBuf, Vec<String>)> {
    let value = query.lines().find_map(|line| Some(line.split_once("REG_SZ")?.1.trim()))?;
    let mut words = split_words(value).into_iter();
    let program = words.next()?;
    Some((program.into(), words.collect()))
}

/// Splits on spaces, a double-quoted run being one word (no escapes: Windows paths cannot hold a quote).
fn split_words(text: &str) -> Vec<String> {
    let (mut words, mut word, mut quoted, mut any) = (Vec::new(), String::new(), false, false);
    for c in text.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if std::mem::take(&mut any) {
                    words.push(std::mem::take(&mut word));
                }
            }
            c => word.push(c),
        }
    }
    if any {
        words.push(word);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_run_value() {
        let query = "\r\nHKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Run\r\n    KithServe    REG_SZ    \"C:\\Program Files\\Kith\\kith-servew.exe\" serve --port 19222\r\n\r\n";
        let (program, args) = parse_run_value(query).unwrap();
        assert_eq!(program, PathBuf::from("C:\\Program Files\\Kith\\kith-servew.exe"));
        assert_eq!(args, ["serve", "--port", "19222"]);
    }

    #[test]
    fn reads_a_run_value_with_the_holder_in_front() {
        let query = "    KithServe    REG_SZ    \"C:\\K it\\kith-holder.exe\" \"supervise\" \"--dir\" \"C:\\a b\\holder\" \"--\" \"C:\\K it\\kith-servew.exe\" serve --port 19222\r\n";
        let (program, args) = parse_run_value(query).unwrap();
        assert_eq!(program, PathBuf::from("C:\\K it\\kith-holder.exe"));
        assert_eq!(args[1..3], ["--dir", "C:\\a b\\holder"]);
        assert_eq!(host_program(&args).unwrap(), "C:\\K it\\kith-servew.exe");
    }
}
