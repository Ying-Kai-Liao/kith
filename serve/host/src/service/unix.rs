//! The background service on macOS (a launchd agent) and Linux (a systemd user unit).

use super::{create_parent, current_exe, data_dir, home, run, wait_for_host_exit};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const LABEL: &str = crate::environment::SERVICE_LABEL;
const UNIT: &str = crate::environment::SYSTEMD_UNIT;

/// Stops the installed job, preserving its configuration for a later start.
/// A manually started daemon is never mistaken for the service we own.
pub fn stop() -> Result<()> {
    if cfg!(target_os = "macos") {
        launchd::unload(&mut launchctl, &target())?;
    } else if installed() {
        run("systemctl", &["--user", "stop", UNIT])?;
    }
    wait_for_host_exit()
}

pub fn start() -> Result<()> {
    if cfg!(target_os = "macos") {
        launchd::reload(&mut launchctl, &domain(), &target(), &launchd_plist())
    } else {
        run("systemctl", &["--user", "start", UNIT])
    }
}

/// Refuse to replace one installation while restarting a service owned by another.
pub fn require_executable(expected: &Path) -> Result<()> {
    let configured = if cfg!(target_os = "macos") {
        let output = Command::new("/usr/bin/plutil")
            .args(["-extract", "ProgramArguments.0", "raw", "-o", "-"])
            .arg(launchd_plist())
            .output()?;
        if !output.status.success() {
            bail!("cannot read the installed host service");
        }
        PathBuf::from(String::from_utf8(output.stdout)?.trim())
    } else {
        let unit = std::fs::read_to_string(systemd_unit())?;
        let command =
            unit.lines().find_map(|line| line.strip_prefix("ExecStart=")).context("service has no ExecStart")?;
        let args = shlex::split(command).context("invalid service ExecStart")?;
        let exe = args.first().context("service has no executable")?;
        PathBuf::from(exe.replace("%%", "%").replace("$$", "$"))
    };
    if configured.canonicalize()? != expected.canonicalize()? {
        bail!("the installed service runs another host binary; run that binary's update command");
    }
    Ok(())
}

fn launchd_plist() -> PathBuf {
    home().join(format!("Library/LaunchAgents/{LABEL}.plist"))
}

fn systemd_unit() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| home().join(".config")).join("systemd/user").join(UNIT)
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
/// that has it (hq's `hq-uv`) lets the host read folders on an external volume.
fn launcher() -> Vec<String> {
    let parse = |text: &str| serde_json::from_str::<Vec<String>>(text).ok().filter(|w| !w.is_empty());
    if let Some(words) = std::env::var("KITH_SERVE_LAUNCHER").ok().and_then(|t| parse(&t)) {
        return words;
    }
    let config = std::fs::read_to_string(data_dir().join("config.json")).ok();
    config
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| parse(&v.get("launcher")?.to_string()))
        .unwrap_or_default()
}

/// The `ProgramArguments` entries of the launchd job: the launcher's words, then the host and its arguments.
fn program_arguments(launcher: &[String], exe: &str, port: u16) -> String {
    let words = launcher.iter().map(String::as_str).chain([exe, "serve", "--port"]).map(str::to_owned);
    let mut out = String::new();
    for word in words.chain([port.to_string()]) {
        out.push_str("<string>");
        out.push_str(&xml_escape(&word));
        out.push_str("</string>");
    }
    out
}

/// The launchd job file. `hq_bin` pins the `hq` checkout the host runs agents from.
fn plist_text(
    launcher: &[String],
    exe: &str,
    port: u16,
    path: &str,
    data: &str,
    hq_bin: Option<&str>,
    log: &str,
) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key><array>{args}</array>
  <key>EnvironmentVariables</key><dict><key>PATH</key><string>{path}</string><key>KITH_SERVE_HOME</key><string>{data}</string>{hq}</dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ProcessType</key><string>Interactive</string>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#,
        args = program_arguments(launcher, exe, port),
        path = xml_escape(path),
        data = xml_escape(data),
        hq = hq_bin
            .map(|b| {
                // Both names: apps that recognise this job by HQ_BIN keep doing so (the kernel's entry point is KITH_KERNEL_BIN now).
                let b = xml_escape(b);
                format!("<key>KITH_KERNEL_BIN</key><string>{b}</string><key>HQ_BIN</key><string>{b}</string>")
            })
            .unwrap_or_default(),
        log = xml_escape(log),
    )
}

/// The systemd user unit, with the same environment as the launchd job.
fn unit_text(exe: &str, port: u16, path: &str, data: &str, hq_bin: Option<&str>) -> String {
    format!(
        "[Unit]\nDescription=Kith host\nAfter=network-online.target\n\n[Service]\nExecStart={exe} serve --port {port}\nEnvironment={path}\nEnvironment={data}\n{hq}Restart=always\nRestartSec=3\n\n[Install]\nWantedBy=default.target\n",
        exe = systemd_quote(exe, true),
        path = systemd_quote(&format!("PATH={path}"), false),
        data = systemd_quote(&format!("KITH_SERVE_HOME={data}"), false),
        hq = hq_bin
            .map(|b| {
                use std::fmt::Write as _;
                ["KITH_KERNEL_BIN", "HQ_BIN"].iter().fold(String::new(), |mut out, k| {
                    let _ = writeln!(out, "Environment={}", systemd_quote(&format!("{k}={b}"), false));
                    out
                })
            })
            .unwrap_or_default(),
    )
}

/// Installs and starts the host as a per-user background service. The current
/// PATH is captured so the service finds `npx`, `claude`, `codex`, …
pub fn install(port: u16) -> Result<()> {
    let exe = current_exe().context("locating the kith-serve binary")?;
    let exe = exe.to_string_lossy();
    let path = std::env::var("PATH").unwrap_or_default();
    // The login-shell PATH lookup would otherwise pick another kith than the one that installed the job.
    let hq_bin = ["KITH_KERNEL_BIN", "HQ_BIN"].iter().find_map(|k| std::env::var(k).ok()).filter(|s| !s.is_empty());
    let log = data_dir().join("host.log");
    std::fs::create_dir_all(data_dir()).context("creating the data directory")?;
    if cfg!(target_os = "macos") {
        let plist = plist_text(
            &launcher(),
            &exe,
            port,
            &path,
            &data_dir().to_string_lossy(),
            hq_bin.as_deref(),
            &log.to_string_lossy(),
        );
        let file = launchd_plist();
        create_parent(&file)?;
        // Not loaded yet is the normal case here.
        launchd::unload(&mut launchctl, &target())?;
        wait_for_host_exit()?;
        std::fs::write(&file, plist).with_context(|| format!("writing {}", file.display()))?;
        launchd::reload(&mut launchctl, &domain(), &target(), &file)?;
    } else {
        let unit = unit_text(&exe, port, &path, &data_dir().to_string_lossy(), hq_bin.as_deref());
        let file = systemd_unit();
        create_parent(&file)?;
        std::fs::write(&file, unit).with_context(|| format!("writing {}", file.display()))?;
        run("systemctl", &["--user", "daemon-reload"])?;
        run("systemctl", &["--user", "stop", UNIT])?;
        wait_for_host_exit()?;
        run("systemctl", &["--user", "enable", "--now", UNIT])?;
        println!("Tip: `loginctl enable-linger $USER` keeps the host running while you're logged out.");
    }
    Ok(())
}

/// Best effort: every step tolerates "already gone".
pub fn uninstall() {
    if cfg!(target_os = "macos") {
        let _ = launchd::unload(&mut launchctl, &target());
        let _ = std::fs::remove_file(launchd_plist());
    } else {
        let _ = run("systemctl", &["--user", "disable", "--now", UNIT]);
        let _ = std::fs::remove_file(systemd_unit());
        let _ = run("systemctl", &["--user", "daemon-reload"]);
    }
}

pub fn installed() -> bool {
    if cfg!(target_os = "macos") { launchd_plist().exists() } else { systemd_unit().exists() }
}

fn domain() -> String {
    format!("gui/{}", current_uid())
}

fn target() -> String {
    format!("gui/{}/{LABEL}", current_uid())
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
    use super::{plist_text, program_arguments, systemd_quote, unit_text};

    #[test]
    fn the_job_runs_behind_a_launcher_when_one_is_set() {
        assert_eq!(
            program_arguments(&[], "/h/kith-serve", 19224),
            "<string>/h/kith-serve</string><string>serve</string><string>--port</string><string>19224</string>"
        );
        let launcher = ["/hq/hq-uv".to_owned(), "run".to_owned(), "a&b".to_owned()];
        assert_eq!(
            program_arguments(&launcher, "/h/kith-serve", 19224),
            "<string>/hq/hq-uv</string><string>run</string><string>a&amp;b</string><string>/h/kith-serve</string><string>serve</string><string>--port</string><string>19224</string>"
        );
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
        let plist =
            plist_text(&[], "/h/kith-serve", 19224, "/usr/bin", "/d", Some("/my hq/kernel/bin/hq&"), "/d/host.log");
        assert!(plist.contains("<key>HQ_BIN</key><string>/my hq/kernel/bin/hq&amp;</string>"), "{plist}");
        assert!(plist.contains("<key>KITH_KERNEL_BIN</key><string>/my hq/kernel/bin/hq&amp;</string>"), "{plist}");
        assert!(plist.contains("<key>KITH_SERVE_HOME</key><string>/d</string>"));
        let unit = unit_text("/h/kith-serve", 19224, "/usr/bin", "/d", Some("/my hq/kernel/bin/hq"));
        assert!(unit.contains("Environment=\"HQ_BIN=/my hq/kernel/bin/hq\"\n"), "{unit}");
        assert!(unit.contains("Environment=\"KITH_KERNEL_BIN=/my hq/kernel/bin/hq\"\n"), "{unit}");
        assert!(unit.contains("Environment=\"KITH_SERVE_HOME=/d\"\n"));
    }

    #[test]
    fn the_job_files_have_no_hq_bin_when_it_is_not_set() {
        assert!(!plist_text(&[], "/h/kith-serve", 19224, "/usr/bin", "/d", None, "/d/host.log").contains("HQ_BIN"));
        assert!(!unit_text("/h/kith-serve", 19224, "/usr/bin", "/d", None).contains("HQ_BIN"));
    }
}
