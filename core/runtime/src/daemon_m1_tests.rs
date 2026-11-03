//! The daemon's M1-0 follow-ups: it keeps retrying behind an embedded command, and a stop that races its start leaves
//! nothing held.

use std::time::{Duration, Instant};

use serde_json::json;

use super::{Config, Daemon, Reached};
use crate::error::Error;
use crate::holder_link::HolderConfig;
use crate::layout::Layout;
use crate::tests::{machine, open, paths};

fn quiet() -> Config {
    Config {
        holder: HolderConfig::default(),
        holder_on: false,
        // Much shorter than the test waits: the daemon must keep going past it behind an embedded command.
        wait_for_embedded: Duration::from_millis(100),
    }
}

#[test]
fn a_daemon_keeps_retrying_behind_an_embedded_command_and_says_so_once() {
    let (_t, m) = machine();
    let p = paths(&m);
    let embedded = open(&Layout::new(&m));
    let d = Daemon::spawn_with(&p, quiet());
    // Well past wait_for_embedded: still waiting, not failed with "already running".
    match d.ready(Duration::from_millis(1200)) {
        Err(Error::AlreadyRunning(_)) => panic!("the daemon gave up behind an embedded command"),
        Err(_) => {}
        Ok(()) => panic!("it cannot serve while the command holds the core"),
    }
    drop(embedded);
    d.ready(Duration::from_secs(10))
        .expect("it comes up once the command is done");
    assert!(matches!(
        crate::reach(&p, "kernel.version", &json!({}), true),
        Reached::Answered(_)
    ));
    let log = std::fs::read_to_string(m.join(crate::log::FILE)).expect("core.log");
    assert_eq!(log.matches("daemon: a command").count(), 1, "{log}");
}

#[test]
fn a_daemon_stopped_while_it_waits_or_at_once_leaves_the_core_free() {
    let (_t, m) = machine();
    let p = paths(&m);
    let l = Layout::new(&m);
    // Stopped at once, racing its own start.
    let begun = Instant::now();
    let d = Daemon::spawn_with(&p, quiet());
    d.stop();
    assert!(
        begun.elapsed() < Duration::from_secs(5),
        "{:?}",
        begun.elapsed()
    );
    let end = Instant::now() + Duration::from_secs(10);
    while let Ok(crate::Embedded::DaemonRunning { .. }) | Err(_) =
        crate::embedded_with(&l, crate::db::MIGRATIONS)
    {
        assert!(
            Instant::now() < end,
            "the stopped daemon still holds the core"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    // Stopped while waiting behind a command.
    let embedded = open(&l);
    let d = Daemon::spawn_with(&p, quiet());
    std::thread::sleep(Duration::from_millis(300));
    let begun = Instant::now();
    d.stop();
    assert!(begun.elapsed() < Duration::from_secs(5));
    drop(embedded);
    std::thread::sleep(Duration::from_millis(300));
    assert!(matches!(
        crate::reach(&p, "kernel.version", &json!({}), true),
        Reached::NoDaemon
    ));
}
