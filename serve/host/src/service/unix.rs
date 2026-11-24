//! The background service on macOS (a launchd agent) and Linux (a systemd user unit).

use super::{create_parent, current_exe, data_dir, holder_words, home, host_program, run, wait_for_host_exit};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const LABEL: &str = crate::environment::SERVICE_LABEL;
const UNIT: &str = crate::environment::SYSTEMD_UNIT;
/// A test names its own job with this; only a label starting with `TEST_LABEL_PREFIX` is accepted.
const TEST_LABEL_ENV: &str = "KITH_SERVE_TEST_LABEL";
const TEST_LABEL_PREFIX: &str = "dev.kith.serve.test-";

/// The job this process may install, start or stop. launchd and systemd --user are per account, not per `$HOME`: a
/// throwaway HOME does not isolate them, and an install from one replaces the account's real job. So the real job
/// is only touched when `$HOME` is the account's own home; a test names a test job instead.
struct Job {
    label: String,
    unit: String,
}

fn job() -> Result<Job> {
    let real = real_home();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    choose_job(std::env::var(TEST_LABEL_ENV).ok().as_deref(), home.as_deref(), real.as_deref())
}

fn choose_job(test_label: Option<&str>, home: Option<&Path>, real_home: Option<&Path>) -> Result<Job> {
    if let Some(label) = test_label.filter(|l| !l.is_empty()) {
        let plain = label.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
        if !label.starts_with(TEST_LABEL_PREFIX) || label.len() == TEST_LABEL_PREFIX.len() || !plain {
            bail!(
                "{TEST_LABEL_ENV} must look like {TEST_LABEL_PREFIX}<id>, not {label:?}: the real job {LABEL} is never a test label"
            );
        }
        return Ok(Job { label: label.to_owned(), unit: format!("{label}.service") });
    }
    let (Some(home), Some(real)) = (home, real_home) else {
        bail!(
            "refusing to touch the job {LABEL}: cannot tell HOME ({}) from this account's home ({}); a test sets {TEST_LABEL_ENV}={TEST_LABEL_PREFIX}<id>",
            home.map_or("unset".into(), |h| h.display().to_string()),
            real_home.map_or("unknown".into(), |h| h.display().to_string()),
        );
    };
    let same = match (home.canonicalize(), real.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => home == real,
    };
    if !same {
        bail!(
            "refusing to touch the job {LABEL}: HOME is {} but this account's home is {}; a test sets {TEST_LABEL_ENV}={TEST_LABEL_PREFIX}<id>",
            home.display(),
            real.display()
        );
    }
    Ok(Job { label: LABEL.to_owned(), unit: UNIT.to_owned() })
}

/// The account's home from the password database (not `$HOME`, which is what is being checked).
fn real_home() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    // SAFETY: getpwuid_r fills `pwd` and `buf`, which outlive the read of pw_dir below.
    unsafe {
        let mut pwd: libc::passwd = std::mem::zeroed();
        let mut buf = vec![0u8; 16 * 1024];
        let mut out: *mut libc::passwd = std::ptr::null_mut();
        let rc = libc::getpwuid_r(libc::getuid(), &raw mut pwd, buf.as_mut_ptr().cast(), buf.len(), &raw mut out);
        if rc != 0 || out.is_null() || pwd.pw_dir.is_null() {
            return None;
        }
        let dir = std::ffi::CStr::from_ptr(pwd.pw_dir);
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(dir.to_bytes())))
    }
}

/// Stops the installed job, preserving its configuration for a later start.
/// A manually started daemon is never mistaken for the service we own.
pub fn stop() -> Result<()> {
    let job = job()?;
    if cfg!(target_os = "macos") {
        launchd::unload(&mut launchctl, &target(&job))?;
    } else if systemd_unit(&job).exists() {
        run("systemctl", &["--user", "stop", &job.unit])?;
    }
    wait_for_host_exit()
}

pub fn start() -> Result<()> {
    let job = job()?;
    if cfg!(target_os = "macos") {
        launchd::reload(&mut launchctl, &domain(), &target(&job), &launchd_plist(&job))
    } else {
        run("systemctl", &["--user", "start", &job.unit])
    }
}

/// Refuse to replace one installation while restarting a service owned by another.
pub fn require_executable(expected: &Path) -> Result<()> {
    let job = job()?;
    let configured = if cfg!(target_os = "macos") {
        let output = Command::new("/usr/bin/plutil")
            .args(["-extract", "ProgramArguments", "json", "-o", "-"])
            .arg(launchd_plist(&job))
            .output()?;
        if !output.status.success() {
            bail!("cannot read the installed host service");
        }
        let words: Vec<String> = serde_json::from_slice(&output.stdout).context("invalid service ProgramArguments")?;
        PathBuf::from(host_program(&words).context("service has no executable")?)
    } else {
        let unit = std::fs::read_to_string(systemd_unit(&job))?;
        let command =
            unit.lines().find_map(|line| line.strip_prefix("ExecStart=")).context("service has no ExecStart")?;
        let args = shlex::split(command).context("invalid service ExecStart")?;
        let exe = host_program(&args).context("service has no executable")?;
        PathBuf::from(exe.replace("%%", "%").replace("$$", "$"))
    };
    if configured.canonicalize()? != expected.canonicalize()? {
        bail!("the installed service runs another host binary; run that binary's update command");
    }
    Ok(())
}

fn launchd_plist(job: &Job) -> PathBuf {
    home().join(format!("Library/LaunchAgents/{}.plist", job.label))
}

fn systemd_unit(job: &Job) -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| home().join(".config")).join("systemd/user").join(&job.unit)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// One quoted systemd word: a PATH with spaces (WSL's `Program Files`) stays whole, and `%`
/// is never read as a specifier. `ExecStart` also expands `$`, so it is doubled there.
fn systemd_quote(s: &str, exec: bool) -> String {
    let s = s.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%");
    format!("\"{}\"", if exec { s.replace('$', "$$") } else { s })
}

/// The words to run the host behind, from `KITH_SERVE_LAUNCHER` or `launcher` in `<data dir>/config.json`
/// (a JSON array of strings). launchd credits file access to the first program it starts, so a launcher
/// that has it (hq's `hq-uv`) lets the host read folders on an external volume. The flag says the words were the
/// old `kith-serve-launch` form and were replaced (see `legacy_launcher`).
fn launcher() -> (Vec<String>, bool) {
    let parse = |text: &str| serde_json::from_str::<Vec<String>>(text).ok().filter(|w| !w.is_empty());
    let words = std::env::var("KITH_SERVE_LAUNCHER").ok().and_then(|t| parse(&t)).or_else(|| {
        let config = std::fs::read_to_string(data_dir().join("config.json")).ok()?;
        parse(&serde_json::from_str::<serde_json::Value>(&config).ok()?.get("launcher")?.to_string())
    });
    match words {
        Some(words) => match legacy_launcher(&words, |script| Path::new(script).exists()) {
            Some(mapped) => (mapped, true),
            None => (words, false),
        },
        None => (Vec::new(), false),
    }
}

/// The old launcher, `[<uv>, .., --script, <path>/kith-serve-launch]`, as the supervisor's: `[<uv>, run, --quiet,
/// --no-project, --]`. Only once the script file is gone (the next release deletes it): while it ships, the
/// launcher stays as written, so a reinstall does not change the live job and putting the old launcher back in
/// `config.json` is a real rollback. Once it is gone, a reinstall writes the new job without a `config.json` edit.
fn legacy_launcher(words: &[String], exists: impl Fn(&str) -> bool) -> Option<Vec<String>> {
    let [first, .., flag, script] = words else { return None };
    let named = script == "kith-serve-launch" || script.ends_with("/kith-serve-launch");
    (flag == "--script" && named && !exists(script))
        .then(|| [first.as_str(), "run", "--quiet", "--no-project", "--"].map(str::to_owned).to_vec())
}

/// The `ProgramArguments` entries of the launchd job: the launcher's words, then the host and its arguments.
/// `--supervise` goes after `--port N`: the schedules cutover gate finds the host as the word before the last
/// `serve` that is followed by `--port` (`schedules_gate::program_of`).
fn program_arguments(launcher: &[String], supervise: bool, exe: &str, port: u16) -> String {
    let words = launcher.iter().map(String::as_str).chain([exe, "serve", "--port"]).map(str::to_owned);
    let mut out = String::new();
    for word in words.chain([port.to_string()]).chain(supervise.then(|| "--supervise".to_owned())) {
        out.push_str("<string>");
        out.push_str(&xml_escape(&word));
        out.push_str("</string>");
    }
    out
}

/// The launchd job file. `hq_bin` pins the `hq` checkout the host runs agents from; `kernel` is a developer's
/// `KITH_KERNEL` (a host built from a checkout runs agents on it), kept so the job starts with what installed it.
#[allow(clippy::too_many_arguments)] // one value per key of the job file
fn plist_text(
    label: &str,
    launcher: &[String],
    supervise: bool,
    exe: &str,
    port: u16,
    path: &str,
    data: &str,
    home: &str,
    hq_bin: Option<&str>,
    kernel: Option<&str>,
    log: &str,
) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{label}</string>
  <key>ProgramArguments</key><array>{args}</array>
  <key>EnvironmentVariables</key><dict><key>PATH</key><string>{path}</string><key>KITH_SERVE_HOME</key><string>{data}</string><key>HOME</key><string>{home}</string>{hq}</dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>{exit_timeout}
  <key>ProcessType</key><string>Interactive</string>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#,
        label = xml_escape(label),
        args = program_arguments(launcher, supervise, exe, port),
        // The supervisor's kill (20 s after a signal) must come before launchd's own SIGKILL of the launcher (its
        // default is 20 s too), so the job ends with the host's exit status and not a group sweep.
        exit_timeout = if supervise { "\n  <key>ExitTimeOut</key><integer>30</integer>" } else { "" },
        path = xml_escape(path),
        data = xml_escape(data),
        home = xml_escape(home),
        hq = hq_bin
            .map(|b| {
                // Both names: apps that recognise this job by HQ_BIN keep doing so (the kernel's entry point is KITH_KERNEL_BIN now).
                let b = xml_escape(b);
                format!("<key>KITH_KERNEL_BIN</key><string>{b}</string><key>HQ_BIN</key><string>{b}</string>")
            })
            .unwrap_or_default()
            + &kernel.map(|k| format!("<key>KITH_KERNEL</key><string>{}</string>", xml_escape(k))).unwrap_or_default(),
        log = xml_escape(log),
    )
}

/// The systemd user unit, with the same environment as the launchd job.
#[allow(clippy::too_many_arguments)] // one value per key of the unit
fn unit_text(
    prefix: &[String],
    exe: &str,
    port: u16,
    path: &str,
    data: &str,
    home: &str,
    hq_bin: Option<&str>,
    kernel: Option<&str>,
) -> String {
    format!(
        "[Unit]\nDescription=Kith host\nAfter=network-online.target\n\n[Service]\nExecStart={prefix}{exe} serve --port {port}\nEnvironment={path}\nEnvironment={data}\nEnvironment={home}\n{hq}Restart=always\nRestartSec=3\n\n[Install]\nWantedBy=default.target\n",
        prefix = prefix.iter().fold(String::new(), |out, w| out + &systemd_quote(w, true) + " "),
        exe = systemd_quote(exe, true),
        path = systemd_quote(&format!("PATH={path}"), false),
        data = systemd_quote(&format!("KITH_SERVE_HOME={data}"), false),
        home = systemd_quote(&format!("HOME={home}"), false),
        hq = hq_bin
            .map(|b| {
                use std::fmt::Write as _;
                ["KITH_KERNEL_BIN", "HQ_BIN"].iter().fold(String::new(), |mut out, k| {
                    let _ = writeln!(out, "Environment={}", systemd_quote(&format!("{k}={b}"), false));
                    out
                })
            })
            .unwrap_or_default()
            + &kernel
                .map(|k| format!("Environment={}\n", systemd_quote(&format!("KITH_KERNEL={k}"), false)))
                .unwrap_or_default(),
    )
}

/// Installs and starts the host as a per-user background service. The current
/// PATH is captured so the service finds `npx`, `claude`, `codex`, …
pub fn install(port: u16) -> Result<()> {
    let job = job()?;
    let exe = current_exe().context("locating the kith-serve binary")?;
    let exe = exe.to_string_lossy();
    let path = std::env::var("PATH").unwrap_or_default();
    // The login-shell PATH lookup would otherwise pick another kith than the one that installed the job.
    let hq_bin = ["KITH_KERNEL_BIN", "HQ_BIN"].iter().find_map(|k| std::env::var(k).ok()).filter(|s| !s.is_empty());
    let kernel = std::env::var(crate::kernel::KERNEL_ENV).ok().filter(|s| !s.is_empty());
    // The job must not fall back to the account's real HOME (launchd/systemd would give it that): the core finds its
    // state from HOME, so a job installed from another HOME would open the real one. For the real job they are equal.
    let home_dir = std::env::var("HOME").unwrap_or_default();
    let log = data_dir().join("host.log");
    std::fs::create_dir_all(data_dir()).context("creating the data directory")?;
    // Opt-in: the holder is the supervised program and starts the host (see `holder_wanted`).
    let holder = holder_words()?.unwrap_or_default();
    let (launcher, legacy) = launcher();
    // The holder has its own supervisor; with no launcher the job starts the host itself and launchd's signals reach it.
    let supervise = supervises(&launcher, &holder);
    if cfg!(target_os = "macos") {
        if legacy {
            note_legacy_launcher(&log);
        }
        let plist = plist_text(
            &job.label,
            &[launcher, holder.clone()].concat(),
            supervise,
            &exe,
            port,
            &path,
            &data_dir().to_string_lossy(),
            &home_dir,
            hq_bin.as_deref(),
            kernel.as_deref(),
            &log.to_string_lossy(),
        );
        let file = launchd_plist(&job);
        create_parent(&file)?;
        // Not loaded yet is the normal case here.
        launchd::unload(&mut launchctl, &target(&job))?;
        wait_for_host_exit()?;
        std::fs::write(&file, plist).with_context(|| format!("writing {}", file.display()))?;
        launchd::reload(&mut launchctl, &domain(), &target(&job), &file)?;
    } else {
        let unit = unit_text(
            &holder,
            &exe,
            port,
            &path,
            &data_dir().to_string_lossy(),
            &home_dir,
            hq_bin.as_deref(),
            kernel.as_deref(),
        );
        let file = systemd_unit(&job);
        create_parent(&file)?;
        std::fs::write(&file, unit).with_context(|| format!("writing {}", file.display()))?;
        run("systemctl", &["--user", "daemon-reload"])?;
        run("systemctl", &["--user", "stop", &job.unit])?;
        wait_for_host_exit()?;
        run("systemctl", &["--user", "enable", "--now", &job.unit])?;
        println!("Tip: `loginctl enable-linger $USER` keeps the host running while you're logged out.");
    }
    Ok(())
}

/// Whether the job starts the host through `--supervise`: behind a launcher, and not under the holder's supervisor.
fn supervises(launcher: &[String], holder: &[String]) -> bool {
    !launcher.is_empty() && holder.is_empty()
}

/// One line in `host.log`: `config.json` still names the launcher script this install replaced.
fn note_legacy_launcher(log: &Path) {
    use std::io::Write as _;
    let line = "install: the launcher still names kith-serve-launch (config.json or KITH_SERVE_LAUNCHER); the job runs `kith serve --supervise` behind its first word instead. Set launcher to [<kith-uv>, run, --quiet, --no-project, --] to drop this note.\n";
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Refuses (an error) from a HOME that is not this account's own; see `job`. Callers that edit other state first
/// (the Claude Code status line) run it before they touch anything.
pub fn guard() -> Result<()> {
    job().map(|_| ())
}

/// Best effort: every step tolerates "already gone". Only a refused job is an error.
pub fn uninstall() -> Result<()> {
    let job = job()?;
    if cfg!(target_os = "macos") {
        let _ = launchd::unload(&mut launchctl, &target(&job));
        let _ = std::fs::remove_file(launchd_plist(&job));
    } else {
        let _ = run("systemctl", &["--user", "disable", "--now", &job.unit]);
        let _ = std::fs::remove_file(systemd_unit(&job));
        let _ = run("systemctl", &["--user", "daemon-reload"]);
    }
    Ok(())
}

pub fn installed() -> bool {
    // A refused job (a throwaway HOME) is not installed as far as this process can say.
    job().is_ok_and(
        |job| if cfg!(target_os = "macos") { launchd_plist(&job).exists() } else { systemd_unit(&job).exists() },
    )
}

fn domain() -> String {
    format!("gui/{}", current_uid())
}

fn target(job: &Job) -> String {
    format!("gui/{}/{}", current_uid(), job.label)
}

/// Runs the real `launchctl`: (exit code, stderr).
fn launchctl(args: &[&str]) -> (i32, String) {
    match Command::new("launchctl").args(args).stdin(Stdio::null()).output() {
        Ok(o) => (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stderr).trim().to_owned()),
        Err(e) => (-1, format!("cannot run launchctl: {e}")),
    }
}

/// `launchctl bootout` returns before the job is gone, and a `bootstrap` of the same label straight after fails with
/// EIO (5). So: wait until `print` says the job is gone, retry the bootstrap, and check the job is loaded at the end.
mod launchd {
    use anyhow::{Result, bail};
    use std::path::Path;
    use std::time::{Duration, Instant};

    pub type Runner<'a> = dyn FnMut(&[&str]) -> (i32, String) + 'a;

    const RETRIES: usize = 5;
    const BACKOFF_MS: [u64; 4] = [500, 1000, 2000, 2000];
    const GONE_TIMEOUT: Duration = Duration::from_secs(10);

    fn loaded(run: &mut Runner, target: &str) -> bool {
        run(&["print", target]).0 == 0
    }

    pub fn unload(run: &mut Runner, target: &str) -> Result<()> {
        unload_within(run, target, GONE_TIMEOUT, Duration::from_millis(200))
    }

    fn unload_within(run: &mut Runner, target: &str, timeout: Duration, poll: Duration) -> Result<()> {
        // Not loaded is the normal case: the exit code is ignored.
        run(&["bootout", target]);
        let deadline = Instant::now() + timeout;
        while loaded(run, target) {
            if Instant::now() > deadline {
                bail!("{target} is still loaded {}s after launchctl bootout", timeout.as_secs());
            }
            std::thread::sleep(poll);
        }
        Ok(())
    }

    pub fn reload(run: &mut Runner, domain: &str, target: &str, plist: &Path) -> Result<()> {
        reload_with(run, domain, target, plist, |attempt| {
            std::thread::sleep(Duration::from_millis(BACKOFF_MS[attempt.min(BACKOFF_MS.len() - 1)]));
        })
    }

    fn reload_with(
        run: &mut Runner,
        domain: &str,
        target: &str,
        plist: &Path,
        mut pause: impl FnMut(usize),
    ) -> Result<()> {
        let plist = plist.to_string_lossy();
        let mut last = String::new();
        let mut bootstrapped = false;
        for attempt in 0..RETRIES {
            let (code, err) = run(&["bootstrap", domain, &plist]);
            if code == 0 {
                bootstrapped = true;
                break;
            }
            last = err;
            // 5 is EIO, 37 "already in progress"; anything else (a bad plist) will not get better.
            if !matches!(code, 5 | 37) || attempt == RETRIES - 1 {
                break;
            }
            pause(attempt);
        }
        // A loaded job is not enough: it may be another install's plist that is running (two restarts racing).
        if !bootstrapped || !loaded(run, target) {
            bail!(
                "{target} is not loaded (plist {plist}); last launchctl said: {}. Load it by hand: launchctl bootstrap {domain} \"{plist}\"",
                if last.is_empty() { "nothing" } else { &last }
            );
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::path::PathBuf;

        type Calls = std::rc::Rc<std::cell::RefCell<Vec<String>>>;

        /// A launchctl that fails `bootstrap` `fail_times` times with `code`, then loads the job.
        fn fake(fail_times: usize, code: i32) -> (impl FnMut(&[&str]) -> (i32, String), Calls) {
            let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let seen = log.clone();
            let (mut failed, mut is_loaded) = (0, false);
            (
                move |args: &[&str]| {
                    seen.borrow_mut().push(args[0].to_owned());
                    match args[0] {
                        "bootstrap" if failed < fail_times => {
                            failed += 1;
                            (code, "Bootstrap failed: 5: Input/output error".into())
                        }
                        "bootstrap" => {
                            is_loaded = true;
                            (0, String::new())
                        }
                        "print" => (if is_loaded { 0 } else { 113 }, String::new()),
                        _ => (0, String::new()),
                    }
                },
                log,
            )
        }

        #[test]
        fn bootstrap_is_retried_after_eio_until_it_loads() {
            let (mut run, log) = fake(2, 5);
            let mut pauses = 0;
            reload_with(&mut run, "gui/1", "gui/1/x", &PathBuf::from("/p.plist"), |_| pauses += 1).unwrap();
            assert_eq!(pauses, 2);
            assert_eq!(log.borrow().iter().filter(|c| *c == "bootstrap").count(), 3);
        }

        #[test]
        fn a_job_that_never_loads_is_an_error_with_the_manual_command() {
            let (mut run, log) = fake(usize::MAX, 5);
            let err = reload_with(&mut run, "gui/1", "gui/1/x", &PathBuf::from("/p.plist"), |_| {}).unwrap_err();
            let text = err.to_string();
            assert!(text.contains("gui/1/x") && text.contains("/p.plist") && text.contains("Input/output"), "{text}");
            assert!(text.contains("launchctl bootstrap gui/1"), "{text}");
            assert_eq!(log.borrow().iter().filter(|c| *c == "bootstrap").count(), RETRIES);
        }

        #[test]
        fn a_job_loaded_by_someone_else_is_not_a_success() {
            let mut run = |args: &[&str]| match args[0] {
                "bootstrap" => (5, "Bootstrap failed: 5: Input/output error".to_owned()),
                _ => (0, String::new()),
            };
            assert!(reload_with(&mut run, "gui/1", "gui/1/x", &PathBuf::from("/p.plist"), |_| {}).is_err());
        }

        #[test]
        fn a_bad_plist_is_not_retried() {
            let (mut run, log) = fake(usize::MAX, 133);
            assert!(reload_with(&mut run, "gui/1", "gui/1/x", &PathBuf::from("/p.plist"), |_| {}).is_err());
            assert_eq!(log.borrow().iter().filter(|c| *c == "bootstrap").count(), 1);
        }

        #[test]
        fn unload_waits_until_the_job_is_gone() {
            let mut prints = 0;
            let mut run = |args: &[&str]| {
                if args[0] == "print" {
                    prints += 1;
                    return (if prints < 3 { 0 } else { 113 }, String::new());
                }
                (3, "No such process".into())
            };
            unload_within(&mut run, "gui/1/x", Duration::from_secs(5), Duration::from_millis(1)).unwrap();
            assert_eq!(prints, 3);
        }

        #[test]
        fn unload_gives_up_when_the_job_stays() {
            let mut run = |_: &[&str]| (0, String::new());
            let err = unload_within(&mut run, "gui/1/x", Duration::from_millis(20), Duration::from_millis(5));
            assert!(err.is_err());
        }
    }
}

fn current_uid() -> String {
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map_or_else(|| "501".into(), |s| s.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::{LABEL, choose_job};
    use super::{legacy_launcher, plist_text, program_arguments, systemd_quote, unit_text};
    use std::path::Path;

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| (*s).to_owned()).collect()
    }

    const UV: [&str; 5] = ["/k/bin/kith-uv", "run", "--quiet", "--no-project", "--"];

    fn plist(launcher: &[String], supervise: bool) -> String {
        plist_text(
            "dev.kith.serve.test-x1",
            launcher,
            supervise,
            "/h/kith",
            19224,
            "/usr/bin",
            "/d",
            "/h",
            None,
            None,
            "/d/host.log",
        )
    }

    #[test]
    fn a_job_behind_a_launcher_runs_the_supervisor_after_the_port() {
        let args = program_arguments(&words(&UV), true, "/h/kith", 19224);
        let strings: Vec<&str> = args.split("<string>").skip(1).map(|s| s.trim_end_matches("</string>")).collect();
        assert_eq!(
            strings,
            [
                "/k/bin/kith-uv",
                "run",
                "--quiet",
                "--no-project",
                "--",
                "/h/kith",
                "serve",
                "--port",
                "19224",
                "--supervise"
            ]
        );
        // The flag is last: the cutover gate looks for `serve --port`, and `require_executable` for the word after `--`.
        let plain = program_arguments(&words(&UV), false, "/h/kith", 19224);
        assert!(!plain.contains("--supervise"));
    }

    #[test]
    fn the_host_program_is_found_in_the_supervised_job() {
        let args = program_arguments(&words(&UV), true, "/h/kith", 19224);
        let all: Vec<String> =
            args.split("<string>").skip(1).map(|s| s.trim_end_matches("</string>").to_owned()).collect();
        assert_eq!(super::super::host_program(&all).unwrap(), "/h/kith");
    }

    #[test]
    fn only_a_launcher_without_the_holder_supervises() {
        let (uv, holder) = (words(&UV), words(&["/h/kith-holder", "supervise", "--"]));
        assert!(super::supervises(&uv, &[]));
        assert!(!super::supervises(&[], &[]), "no launcher: launchd starts the host itself");
        assert!(!super::supervises(&uv, &holder), "the holder has its own supervisor");
        assert!(!super::supervises(&[], &holder));
    }

    #[test]
    fn exit_timeout_is_only_in_a_supervised_job() {
        let supervised = plist(&words(&UV), true);
        assert!(supervised.contains("<key>ExitTimeOut</key><integer>30</integer>"), "{supervised}");
        assert!(supervised.contains("<string>--supervise</string></array>"), "{supervised}");
        for plain in [plist(&[], false), plist(&words(&UV), false)] {
            assert!(!plain.contains("ExitTimeOut") && !plain.contains("--supervise"), "{plain}");
        }
    }

    #[test]
    fn the_old_launcher_maps_to_the_supervisors_once_its_script_is_gone() {
        let want = words(&["/k/bin/kith-uv", "run", "--quiet", "--no-project", "--"]);
        let old = words(&[
            "/k/bin/kith-uv",
            "run",
            "--quiet",
            "--script",
            "/k/cli/current/share/kith/kernel/bin/kith-serve-launch",
        ]);
        let (gone, there) = (|_: &str| false, |_: &str| true);
        assert_eq!(legacy_launcher(&old, gone), Some(want.clone()));
        // The script still there: the launcher stays as written.
        assert_eq!(legacy_launcher(&old, there), None);
        // A bare name counts when missing.
        assert_eq!(
            legacy_launcher(&words(&["uv", "--script", "kith-serve-launch"]), gone),
            Some(words(&["uv", "run", "--quiet", "--no-project", "--"]))
        );
        // Anything else is left as the owner wrote it.
        assert_eq!(legacy_launcher(&words(&UV), gone), None);
        assert_eq!(legacy_launcher(&words(&["/k/bin/kith-uv", "run", "--script", "/x/other-launch"]), gone), None);
        assert_eq!(legacy_launcher(&words(&["kith-serve-launch"]), gone), None);
        assert_eq!(legacy_launcher(&words(&["--script", "/x/kith-serve-launch"]), gone), None);
        assert_eq!(legacy_launcher(&[], gone), None);
    }

    #[test]
    fn the_job_runs_behind_a_launcher_when_one_is_set() {
        assert_eq!(
            program_arguments(&[], false, "/h/kith-serve", 19224),
            "<string>/h/kith-serve</string><string>serve</string><string>--port</string><string>19224</string>"
        );
        let launcher = ["/hq/hq-uv".to_owned(), "run".to_owned(), "a&b".to_owned()];
        assert_eq!(
            program_arguments(&launcher, false, "/h/kith-serve", 19224),
            "<string>/hq/hq-uv</string><string>run</string><string>a&amp;b</string><string>/h/kith-serve</string><string>serve</string><string>--port</string><string>19224</string>"
        );
    }

    #[test]
    fn the_holder_goes_in_front_of_the_host_in_both_job_files() {
        let holder: Vec<String> =
            ["/h/kith-holder", "supervise", "--dir", "/d/my core/holder", "--secret-file", "/d/s", "--"]
                .map(str::to_owned)
                .to_vec();
        let args = program_arguments(&holder, false, "/h/kith-serve", 19224);
        assert!(args.starts_with("<string>/h/kith-holder</string><string>supervise</string>"), "{args}");
        assert!(args.ends_with("<string>--</string><string>/h/kith-serve</string><string>serve</string><string>--port</string><string>19224</string>"), "{args}");
        let unit = unit_text(&holder, "/h/kith-serve", 19224, "/usr/bin", "/d", "/h", None, None);
        let line = unit.lines().find_map(|l| l.strip_prefix("ExecStart=")).unwrap();
        let words = shlex::split(line).unwrap();
        assert_eq!(words[0], "/h/kith-holder");
        assert_eq!(words[3], "/d/my core/holder");
        assert_eq!(super::super::host_program(&words).unwrap(), "/h/kith-serve");
    }

    #[test]
    fn unit_values_stay_one_word() {
        let path = r#"/usr/bin:/mnt/c/Program Files/a"b\c:/x%y"#;
        assert_eq!(
            systemd_quote(&format!("PATH={path}"), false),
            r#""PATH=/usr/bin:/mnt/c/Program Files/a\"b\\c:/x%%y""#
        );
        let exe = systemd_quote("/home/me/My $bin/kith-serve", true);
        assert_eq!(exe, r#""/home/me/My $$bin/kith-serve""#);
        let parsed = shlex::split(&format!("{exe} serve")).unwrap();
        assert_eq!(parsed[0].replace("%%", "%").replace("$$", "$"), "/home/me/My $bin/kith-serve");
    }

    #[test]
    fn the_job_files_keep_hq_bin_when_it_is_set() {
        let plist = plist_text(
            LABEL,
            &[],
            false,
            "/h/kith-serve",
            19224,
            "/usr/bin",
            "/d",
            "/h",
            Some("/my hq/kernel/bin/hq&"),
            Some("/co&/kernel"),
            "/d/host.log",
        );
        assert!(plist.contains("<key>HQ_BIN</key><string>/my hq/kernel/bin/hq&amp;</string>"), "{plist}");
        assert!(plist.contains("<key>KITH_KERNEL_BIN</key><string>/my hq/kernel/bin/hq&amp;</string>"), "{plist}");
        assert!(plist.contains("<key>KITH_SERVE_HOME</key><string>/d</string>"));
        // A job never falls back to the account's real HOME (and so real state).
        assert!(plist.contains("<key>HOME</key><string>/h</string>"), "{plist}");
        assert!(plist.contains("<key>KITH_KERNEL</key><string>/co&amp;/kernel</string>"), "{plist}");
        let unit = unit_text(
            &[],
            "/h/kith-serve",
            19224,
            "/usr/bin",
            "/d",
            "/h",
            Some("/my hq/kernel/bin/hq"),
            Some("/co/kernel"),
        );
        assert!(unit.contains("Environment=\"HQ_BIN=/my hq/kernel/bin/hq\"\n"), "{unit}");
        assert!(unit.contains("Environment=\"KITH_KERNEL_BIN=/my hq/kernel/bin/hq\"\n"), "{unit}");
        assert!(unit.contains("Environment=\"KITH_SERVE_HOME=/d\"\n"));
        assert!(unit.contains("Environment=\"HOME=/h\"\n"), "{unit}");
        assert!(unit.contains("Environment=\"KITH_KERNEL=/co/kernel\"\n"), "{unit}");
    }

    #[test]
    fn the_job_files_have_no_hq_bin_when_it_is_not_set() {
        assert!(
            !plist_text(LABEL, &[], false, "/h/kith-serve", 19224, "/usr/bin", "/d", "/h", None, None, "/d/host.log")
                .contains("HQ_BIN")
        );
        assert!(!unit_text(&[], "/h/kith-serve", 19224, "/usr/bin", "/d", "/h", None, None).contains("HQ_BIN"));
    }

    #[test]
    fn the_real_job_is_only_touched_from_the_accounts_own_home() {
        let home = Path::new("/Users/me");
        let job = choose_job(None, Some(home), Some(home)).unwrap();
        assert_eq!(job.label, LABEL);
        let err = choose_job(None, Some(Path::new("/var/folders/x/home")), Some(home)).err().unwrap().to_string();
        assert!(err.contains("/var/folders/x/home") && err.contains("/Users/me") && !err.contains('\n'), "{err}");
        assert!(choose_job(None, None, Some(home)).is_err(), "HOME unset");
        assert!(choose_job(None, Some(home), None).is_err(), "no passwd entry");
        assert!(choose_job(Some(""), Some(Path::new("/tmp/x")), Some(home)).is_err(), "an empty label is no opt-in");
    }

    #[test]
    fn a_symlink_to_the_real_home_is_the_real_home() {
        let dir = std::env::temp_dir().join(format!("kith-guard-{}", std::process::id()));
        let real = dir.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(choose_job(None, Some(&link), Some(&real)).is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_test_names_its_own_job_and_never_the_real_one() {
        let (elsewhere, home) = (Path::new("/tmp/throwaway"), Path::new("/Users/me"));
        let job = choose_job(Some("dev.kith.serve.test-ab12"), Some(elsewhere), Some(home)).unwrap();
        assert_eq!(job.label, "dev.kith.serve.test-ab12");
        assert_eq!(job.unit, "dev.kith.serve.test-ab12.service");
        for bad in [
            "dev.kith.serve",
            "dev.kith.serve.test-",
            "dev.kith.serve.test-a b",
            "dev.kith.cost",
            "x/dev.kith.serve.test-1",
        ] {
            assert!(choose_job(Some(bad), Some(elsewhere), Some(home)).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_account_home_comes_from_the_password_database() {
        assert!(super::real_home().is_some_and(|h| h.is_absolute()));
    }
}
