//! `kith-holder supervise`: the holder as the supervised root (M1). The service (launchd, systemd, the Windows Run key)
//! starts the holder; the holder serves in this process and keeps one child (the core: `kith serve`) running, starting
//! it again with a growing pause when it exits. Until now the core started the holder; here the order is the other
//! way round, so the core can crash, be updated or be killed while everything the holder owns stays up.
//!
//! This module only starts, waits for and restarts a program. It reads no file of the core and decides nothing about
//! it. It is a module of its own so `holder.rs` (spawn, lease) stays as it is.

use crate::holder;
use std::io;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How the child is kept running.
#[derive(Clone, Debug)]
pub struct Policy {
    /// The pause after the first quick exit; it doubles per quick exit in a row up to `max`.
    pub min: Duration,
    pub max: Duration,
    /// A child that ran at least this long was healthy: its next pause starts from `min` again.
    pub stable: Duration,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            min: Duration::from_secs(1),
            max: Duration::from_secs(30),
            stable: Duration::from_secs(60),
        }
    }
}

impl Policy {
    /// The pause before starting the child again, given the pause before the run that just ended (`None` for the first
    /// one) and how long that run lasted.
    pub fn next_pause(&self, previous: Option<Duration>, ran: Duration) -> Duration {
        match previous {
            Some(p) if ran < self.stable => (p * 2).min(self.max),
            _ => self.min,
        }
    }
}

fn start(argv: &[String]) -> io::Result<Child> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no program to supervise"))?;
    // Not the holder's stdin (that carried the secret and is spent); output goes where the service sends the holder's.
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .spawn()
}

/// Runs the holder in this process and keeps `argv` running beside it. Returns only when the holder stops
/// serving (then the child is ended too) or `argv` is empty; a service restarts the whole process after that.
pub fn run(cfg: holder::Config, argv: &[String], policy: &Policy) -> io::Result<()> {
    if argv.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no program to supervise",
        ));
    }
    let (done_tx, done_rx) = mpsc::channel();
    // `holder::run` ends the process itself on a clean shutdown; it comes back here only with an error.
    std::thread::spawn(move || {
        let _ = done_tx.send(holder::run(cfg));
    });
    let mut pause: Option<Duration> = None;
    loop {
        let began = Instant::now();
        let mut child = match start(argv) {
            Ok(c) => {
                eprintln!("holder: supervising pid {} ({})", c.id(), argv[0]);
                Some(c)
            }
            Err(e) => {
                eprintln!("holder: can't start {}: {e}", argv[0]);
                None
            }
        };
        // Wait for the child to exit, or for the holder to give up.
        loop {
            match done_rx.recv_timeout(Duration::from_millis(200)) {
                Ok(result) => {
                    if let Some(c) = child.as_mut() {
                        let _ = c.kill();
                        let _ = c.wait();
                    }
                    return result;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::other("the holder thread ended"));
                }
            }
            match child.as_mut().map(Child::try_wait) {
                None => break,
                Some(Ok(None)) => {}
                Some(Ok(Some(status))) => {
                    eprintln!("holder: {} ended ({status})", argv[0]);
                    break;
                }
                Some(Err(e)) => {
                    eprintln!("holder: can't wait for {}: {e}", argv[0]);
                    break;
                }
            }
        }
        let wait = policy.next_pause(pause, began.elapsed());
        pause = Some(wait);
        eprintln!(
            "holder: starting {} again in {} ms",
            argv[0],
            wait.as_millis()
        );
        // Sleep in slices so a holder that gave up is noticed.
        let end = Instant::now() + wait;
        while Instant::now() < end {
            if let Ok(result) = done_rx.recv_timeout(Duration::from_millis(100).min(wait)) {
                return result;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pause_doubles_for_quick_exits_up_to_the_cap_and_resets_after_a_long_run() {
        let p = Policy {
            min: Duration::from_secs(1),
            max: Duration::from_secs(5),
            stable: Duration::from_secs(60),
        };
        let quick = Duration::from_millis(50);
        let first = p.next_pause(None, quick);
        assert_eq!(first, Duration::from_secs(1));
        let second = p.next_pause(Some(first), quick);
        assert_eq!(second, Duration::from_secs(2));
        let third = p.next_pause(Some(second), quick);
        assert_eq!(third, Duration::from_secs(4));
        assert_eq!(p.next_pause(Some(third), quick), Duration::from_secs(5));
        assert_eq!(
            p.next_pause(Some(Duration::from_secs(5)), quick),
            Duration::from_secs(5)
        );
        assert_eq!(
            p.next_pause(Some(third), Duration::from_secs(61)),
            Duration::from_secs(1)
        );
    }
}
