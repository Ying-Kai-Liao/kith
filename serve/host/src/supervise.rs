//! `kith serve --supervise`: the launchd job's launcher, in the host's own binary (it replaces `kith-serve-launch`).
//!
//! launchd credits file access (Full Disk Access, external volumes) to the program it starts and to its children, so
//! a job behind `kith-uv` runs `kith serve --port N --supervise` as a child of it, and that starts the real host as
//! a child of its own: never an exec, which would lose the credit. This process forwards SIGTERM, SIGINT and SIGHUP
//! to the host and exits with its code. A hung host ignores SIGTERM and would keep `host.lock`, so the first forwarded
//! signal also starts a timer (`KITH_SERVE_KILL_GRACE_S`, default 20 s) and the host gets SIGKILL when it runs out.
//!
//! It holds no lock, opens no store and starts exactly one child for its whole life: restarting is launchd's
//! `KeepAlive`, never this process's. The child stays in this process's group (no `setsid`), so launchd's group
//! cleanup still reaches it.

use std::ffi::OsString;
use std::time::Duration;

pub const GRACE_ENV: &str = "KITH_SERVE_KILL_GRACE_S";
const DEFAULT_GRACE_S: f64 = 20.0;
const FLAG: &str = "--supervise";

/// Whether `argv` (the process's own, program name first) is `serve --supervise`.
pub fn wanted(argv: &[OsString]) -> bool {
    let serve = argv.get(1).and_then(|a| a.to_str()).is_some_and(|a| matches!(a, "serve" | "run"));
    // A verb after `serve` (`serve install`) is not the foreground host, whatever follows it.
    let foreground = argv.get(2).is_none_or(|a| a.to_str().is_some_and(|a| a.starts_with('-')));
    serve && foreground && argv.iter().skip(2).any(|a| a == FLAG)
}

/// The host's arguments: this process's own, without the flag.
pub fn child_args(argv: &[OsString]) -> Vec<OsString> {
    argv.iter().skip(1).filter(|a| *a != FLAG).cloned().collect()
}

/// The grace before SIGKILL, from `KITH_SERVE_KILL_GRACE_S`: what the Python launcher accepted. A value that does not
/// parse, or is outside `0 <= g < 3600` (`inf` would never fire, a negative would kill at once), means 20 s.
pub fn grace(value: Option<&str>) -> Duration {
    let seconds = value
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|g| (0.0..3600.0).contains(g))
        .unwrap_or(DEFAULT_GRACE_S);
    Duration::from_secs_f64(seconds)
}

#[cfg(not(unix))]
pub async fn run(_argv: &[OsString]) -> i32 {
    eprintln!("kith serve --supervise is for launchd; it is not available on this platform");
    2
}

/// Runs the host as a child and returns this process's exit code.
#[cfg(unix)]
pub async fn run(argv: &[OsString]) -> i32 {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            eprintln!("kith serve --supervise: cannot find this program to start the host: {e}");
            return 1;
        }
    };
    // Registered before the child starts: a signal that arrives meanwhile must not take this process down.
    let signals = match unix::forward_signals() {
        Ok(rx) => rx,
        Err(e) => {
            eprintln!("kith serve --supervise: cannot listen for signals: {e}");
            return 1;
        }
    };
    let mut cmd = tokio::process::Command::new(exe);
    cmd.args(child_args(argv));
    unix::supervise(cmd, grace(std::env::var(GRACE_ENV).ok().as_deref()), signals).await
}

#[cfg(unix)]
mod unix {
    use std::time::Duration;
    use tokio::process::Command;
    use tokio::signal::unix::{SignalKind, signal};
    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
    use tokio::time::Instant;

    /// TERM, INT and HUP as they arrive, as signal numbers. Not `pthread_sigmask`: in a multi-thread runtime it only
    /// covers the calling thread.
    pub fn forward_signals() -> std::io::Result<UnboundedReceiver<i32>> {
        let mut term = signal(SignalKind::terminate())?;
        let mut int = signal(SignalKind::interrupt())?;
        let mut hup = signal(SignalKind::hangup())?;
        let (tx, rx) = unbounded_channel();
        tokio::spawn(async move {
            loop {
                let sig = tokio::select! {
                    _ = term.recv() => libc::SIGTERM,
                    _ = int.recv() => libc::SIGINT,
                    _ = hup.recv() => libc::SIGHUP,
                };
                if tx.send(sig).is_err() {
                    break;
                }
            }
        });
        Ok(rx)
    }

    /// Starts `cmd` and waits for it, passing `signals` on. The grace timer starts at the first one and is never
    /// reset; when it runs out the child gets SIGKILL. Exits with the child's code, or 128 + the signal that killed it.
    pub async fn supervise(mut cmd: Command, grace: Duration, mut signals: UnboundedReceiver<i32>) -> i32 {
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                eprintln!("kith serve --supervise: cannot start the host: {e}");
                return 1;
            }
        };
        let mut deadline: Option<Instant> = None;
        let mut killed = false;
        loop {
            let timer = async {
                match deadline {
                    Some(at) if !killed => tokio::time::sleep_until(at).await,
                    _ => std::future::pending().await,
                }
            };
            tokio::select! {
                status = child.wait() => return exit_code(status),
                Some(sig) = signals.recv() => {
                    // `id` is `None` once the child is reaped, so a reaped pid is never signalled.
                    if let Some(pid) = child.id().and_then(|p| i32::try_from(p).ok()) {
                        // SAFETY: plain kill(2) on our own unreaped child.
                        unsafe { libc::kill(pid, sig) };
                    }
                    deadline.get_or_insert_with(|| Instant::now() + grace);
                }
                () = timer => {
                    let _ = child.start_kill();
                    killed = true;
                }
            }
        }
    }

    fn exit_code(status: std::io::Result<std::process::ExitStatus>) -> i32 {
        use std::os::unix::process::ExitStatusExt as _;
        match status {
            Ok(s) => s.code().or_else(|| s.signal().map(|n| 128 + n)).unwrap_or(1),
            Err(e) => {
                eprintln!("kith serve --supervise: waiting for the host failed: {e}");
                1
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::time::Instant as StdInstant;
        use tokio::sync::mpsc::UnboundedSender;

        /// `sh -c script`, as a stand-in for the host.
        fn sh(script: &str) -> Command {
            let mut cmd = Command::new("/bin/sh");
            cmd.args(["-c", script]);
            cmd
        }

        fn quiet() -> (UnboundedSender<i32>, UnboundedReceiver<i32>) {
            unbounded_channel()
        }

        /// Sends `signals` (each after its delay), then keeps the channel open so it does not close.
        fn send_later(tx: UnboundedSender<i32>, signals: &'static [(u64, i32)]) {
            tokio::spawn(async move {
                for (after_ms, sig) in signals {
                    tokio::time::sleep(Duration::from_millis(*after_ms)).await;
                    tx.send(*sig).unwrap();
                }
                tokio::time::sleep(Duration::from_secs(60)).await;
            });
        }

        const SPIN: &str = "while :; do sleep 0.05; done";

        #[tokio::test]
        async fn the_childs_exit_code_is_this_ones() {
            for code in [0, 3] {
                let (_tx, rx) = quiet();
                assert_eq!(supervise(sh(&format!("exit {code}")), Duration::from_secs(20), rx).await, code);
            }
        }

        #[tokio::test]
        async fn a_child_killed_by_a_signal_gives_128_plus_it() {
            let (_tx, rx) = quiet();
            assert_eq!(supervise(sh("kill -9 $$"), Duration::from_secs(20), rx).await, 137);
        }

        #[tokio::test]
        async fn a_child_that_cannot_start_exits_1() {
            let (_tx, rx) = quiet();
            assert_eq!(supervise(Command::new("/no/such/host"), Duration::from_secs(20), rx).await, 1);
        }

        #[tokio::test]
        async fn term_is_forwarded_and_a_child_that_exits_on_it_is_not_killed() {
            let (tx, rx) = quiet();
            send_later(tx, &[(300, libc::SIGTERM)]);
            let started = StdInstant::now();
            // Exits 7 on TERM; a SIGKILL would give 137.
            let cmd = sh(&format!("trap 'exit 7' TERM; {SPIN}"));
            assert_eq!(supervise(cmd, Duration::from_secs(20), rx).await, 7);
            assert!(started.elapsed() < Duration::from_secs(5));
        }

        #[tokio::test]
        async fn int_and_hup_are_forwarded_too() {
            for (sig, name) in [(libc::SIGINT, "INT"), (libc::SIGHUP, "HUP")] {
                let (tx, rx) = quiet();
                let signals: &'static [(u64, i32)] = Box::leak(Box::new([(300, sig)]));
                send_later(tx, signals);
                let cmd = sh(&format!("trap 'exit 9' {name}; {SPIN}"));
                assert_eq!(supervise(cmd, Duration::from_secs(20), rx).await, 9, "{name}");
            }
        }

        #[tokio::test]
        async fn a_child_that_ignores_term_is_killed_after_the_grace() {
            let (tx, rx) = quiet();
            send_later(tx, &[(200, libc::SIGTERM)]);
            let started = StdInstant::now();
            let cmd = sh(&format!("trap '' TERM; {SPIN}"));
            assert_eq!(supervise(cmd, Duration::from_secs(1), rx).await, 137);
            let took = started.elapsed();
            assert!(took >= Duration::from_millis(1100) && took < Duration::from_secs(5), "{took:?}");
        }

        #[tokio::test]
        async fn a_second_term_does_not_restart_the_timer() {
            let (tx, rx) = quiet();
            // TERM at 0.2 s and again at 0.9 s; the kill is due at 1.2 s, not 1.9 s.
            send_later(tx, &[(200, libc::SIGTERM), (700, libc::SIGTERM)]);
            let started = StdInstant::now();
            let cmd = sh(&format!("trap '' TERM; {SPIN}"));
            assert_eq!(supervise(cmd, Duration::from_secs(1), rx).await, 137);
            let took = started.elapsed();
            assert!(took < Duration::from_millis(1700), "{took:?}");
        }

        #[tokio::test]
        async fn a_grace_of_zero_kills_at_once() {
            let (tx, rx) = quiet();
            send_later(tx, &[(200, libc::SIGTERM)]);
            let started = StdInstant::now();
            let cmd = sh(&format!("trap '' TERM; {SPIN}"));
            assert_eq!(supervise(cmd, Duration::ZERO, rx).await, 137);
            assert!(started.elapsed() < Duration::from_secs(2));
        }

        #[tokio::test]
        async fn the_grace_is_for_after_a_signal_not_from_the_start() {
            let (_tx, rx) = quiet();
            let started = StdInstant::now();
            assert_eq!(supervise(sh("sleep 0.5; exit 0"), Duration::ZERO, rx).await, 0);
            assert!(started.elapsed() >= Duration::from_millis(400));
        }

        #[tokio::test]
        async fn the_child_shares_this_ones_process_group() {
            // The child writes its own process group; launchd's group cleanup relies on it matching ours.
            let dir = std::env::temp_dir().join(format!("kith-supervise-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let file = dir.join("pgid");
            let (_tx, rx) = quiet();
            let script = format!("ps -o pgid= -p $$ > '{}'", file.display());
            assert_eq!(supervise(sh(&script), Duration::from_secs(20), rx).await, 0);
            let theirs: i32 = std::fs::read_to_string(&file).unwrap().trim().parse().unwrap();
            // SAFETY: getpgrp has no preconditions.
            assert_eq!(theirs, unsafe { libc::getpgrp() });
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    #[test]
    fn only_serve_with_the_flag_supervises() {
        assert!(wanted(&argv(&["kith", "serve", "--port", "1", "--supervise"])));
        assert!(wanted(&argv(&["kith-serve", "serve", "--supervise", "--port", "1"])));
        assert!(wanted(&argv(&["kith-serve", "run", "--supervise"])));
        assert!(!wanted(&argv(&["kith", "serve", "--port", "1"])));
        assert!(!wanted(&argv(&["kith", "serve", "install", "--supervise"])));
        assert!(!wanted(&argv(&["kith", "info", "--supervise"])));
        assert!(!wanted(&argv(&["kith-serve"])));
    }

    #[test]
    fn the_host_gets_this_ones_arguments_without_the_flag() {
        assert_eq!(
            child_args(&argv(&["kith", "serve", "--port", "19224", "--supervise"])),
            argv(&["serve", "--port", "19224"])
        );
        assert_eq!(
            child_args(&argv(&["kith", "serve", "--supervise", "--bind", "127.0.0.1"])),
            argv(&["serve", "--bind", "127.0.0.1"])
        );
    }

    #[test]
    fn the_grace_takes_what_the_python_launcher_took() {
        let s = Duration::from_secs;
        for bad in ["inf", "nan", "-1", "3600", "abc", "", "-inf", "4000"] {
            assert_eq!(grace(Some(bad)), s(20), "{bad:?}");
        }
        assert_eq!(grace(None), s(20));
        assert_eq!(grace(Some("0")), Duration::ZERO);
        assert_eq!(grace(Some("1")), s(1));
        assert_eq!(grace(Some(" 2.5 ")), Duration::from_millis(2500));
        assert_eq!(grace(Some("3599")), s(3599));
    }
}
