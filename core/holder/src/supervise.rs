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

enum Event {
    /// This holder owns the folder and listens.
    Serving,
    /// `holder::run` came back (it ends the process itself on a clean shutdown, so this is an error).
    Ended(io::Result<()>),
}

/// Whether this process serves the folder. Another live holder may own it (the core's detached one survives a
/// service restart): the core then runs against that one and this process keeps asking for the folder.
struct Holding {
    cfg: holder::Config,
    events: mpsc::Receiver<Event>,
    sender: mpsc::Sender<Event>,
    serving: bool,
    /// An attempt (a `holder::run` thread) is in flight.
    trying: bool,
    next_try: Instant,
    backoff: Duration,
}

impl Holding {
    fn try_now(&mut self) {
        self.trying = true;
        let (cfg, tx) = (self.cfg.clone(), self.sender.clone());
        std::thread::spawn(move || {
            let ready = tx.clone();
            let result = holder::run_with(cfg, move || {
                let _ = ready.send(Event::Serving);
            });
            let _ = tx.send(Event::Ended(result));
        });
    }

    /// An attempt came back without the folder: ask again later, slower each time.
    fn lost(&mut self, max: Duration) {
        self.trying = false;
        self.next_try = Instant::now() + self.backoff;
        self.backoff = (self.backoff * 2).min(max);
    }

    /// Waits up to `wait` for news. `Some(result)`: this holder served and then stopped, so the process must end.
    fn poll(&mut self, wait: Duration, policy: &Policy) -> Option<io::Result<()>> {
        match self.events.recv_timeout(wait) {
            Ok(Event::Serving) => {
                self.serving = true;
                self.trying = false;
                eprintln!("holder: serving {}", self.cfg.dir.display());
            }
            Ok(Event::Ended(result)) if self.serving => return Some(result),
            Ok(Event::Ended(_)) => self.lost(policy.max),
            Err(_) => {}
        }
        if !self.serving && !self.trying && Instant::now() >= self.next_try {
            self.try_now();
        }
        None
    }
}

/// Runs the holder in this process and keeps `argv` running beside it. The child starts once this holder serves, or
/// once another live holder is known to own the folder; it is never ended because this holder could not get the
/// folder (it keeps asking, quietly). Returns only when this holder served and then stopped (the child is ended
/// too) or on a start-up error; a service restarts the whole process after that.
pub fn run(cfg: holder::Config, argv: &[String], policy: &Policy) -> io::Result<()> {
    if argv.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no program to supervise",
        ));
    }
    let (sender, events) = mpsc::channel();
    let mut holding = Holding {
        cfg,
        events,
        sender,
        serving: false,
        trying: false,
        next_try: Instant::now(),
        backoff: policy.min,
    };
    holding.try_now();
    // Before the child: wait until the folder is ours or known to be another live holder's.
    match holding.events.recv() {
        Ok(Event::Serving) => {
            holding.serving = true;
            holding.trying = false;
        }
        Ok(Event::Ended(Err(e))) if e.kind() == io::ErrorKind::AddrInUse => {
            eprintln!(
                "holder: {} is held by another holder; running {} against it and asking for the folder again",
                holding.cfg.dir.display(),
                argv[0]
            );
            holding.lost(policy.max);
        }
        Ok(Event::Ended(result)) => return result,
        Err(_) => return Err(io::Error::other("the holder thread ended")),
    }
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
            if let Some(result) = holding.poll(Duration::from_millis(200), policy) {
                if let Some(c) = child.as_mut() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
                return result;
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
            if let Some(result) = holding.poll(Duration::from_millis(100).min(wait), policy) {
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
