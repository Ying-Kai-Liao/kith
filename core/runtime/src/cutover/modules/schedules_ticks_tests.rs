#![allow(clippy::unwrap_used)]
//! Gate condition 6: the step waits out the `kith-sched tick`s that were running when it took `tick.lock`. Every test
//! injects the process lister; a real one would see this Mac's own ticks.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::schedules::Schedules;
use super::schedules_gate::{Tick, list_ticks, sched_program, wait_out_old_ticks};
use super::schedules_tests::{Lifted, World, opts, world};
use crate::cutover::{Module, rollback, state, step, tombstone};
use crate::secrets::keystore::KeyStore;

thread_local! {
    /// What the injected lister answers, call by call; the last answer repeats.
    static LISTINGS: RefCell<Vec<Vec<Tick>>> = const { RefCell::new(Vec::new()) };
}

fn script(listings: Vec<Vec<Tick>>) {
    LISTINGS.with(|l| *l.borrow_mut() = listings);
}

#[allow(clippy::unnecessary_wraps, reason = "the Lister shape")]
fn scripted(_program: &Path) -> Result<Vec<Tick>, String> {
    Ok(LISTINGS.with(|l| {
        let mut l = l.borrow_mut();
        if l.len() > 1 {
            l.remove(0)
        } else {
            l.first().cloned().unwrap_or_default()
        }
    }))
}

fn tick(pid: u32) -> Tick {
    Tick {
        pid,
        stamp: Some(format!("e{pid}")),
    }
}

/// launchd's `dev.kith.sched` plist, running a real file, so condition 6 has a program to look for.
fn install_sched(w: &World) {
    let bin = w.env.home.join("cli/current/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let prog = bin.join("kith-sched");
    std::fs::write(&prog, "#!/bin/sh\n").unwrap();
    let dir = w.env.home.join("Library/LaunchAgents");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("dev.kith.sched.plist"),
        format!(
            "<plist><dict><key>ProgramArguments</key><array><string>{}</string><string>tick</string></array></dict></plist>",
            prog.display()
        ),
    )
    .unwrap();
}

fn reg(w: &World, wait: Duration) -> Vec<Box<dyn Module>> {
    let store = w.store.clone();
    let keys: std::sync::Arc<dyn Fn() -> Box<dyn KeyStore>> =
        std::sync::Arc::new(move || -> Box<dyn KeyStore> { Box::new(store.clone()) });
    vec![Box::new(Lifted(
        Schedules::with_keys(keys).tick_wait(wait).ticks(scripted),
    ))]
}

#[test]
fn a_tick_that_started_before_the_lock_and_is_still_running_refuses_the_step_with_6() {
    let w = world(false);
    install_sched(&w);
    let before = std::fs::read(w.mel_jobs()).unwrap();
    script(vec![vec![tick(4242)]]);
    let e = step(
        &reg(&w, Duration::from_millis(300)),
        &w.env,
        "schedules",
        opts(),
    )
    .unwrap_err()
    .to_string();
    assert!(
        e.contains("6: a kith-sched tick (pid 4242) is still running; try again in a minute"),
        "{e}"
    );
    assert!(state::read(&w.env.layout, "schedules").unwrap().is_none());
    assert_eq!(std::fs::read(w.mel_jobs()).unwrap(), before);
    assert!(!tombstone::is_tombstone(&w.state_file()));
}

#[test]
fn a_tick_that_ends_during_the_wait_lets_the_step_through() {
    let w = world(false);
    install_sched(&w);
    script(vec![vec![tick(4242)], vec![tick(4242)], vec![]]);
    step(
        &reg(&w, Duration::from_secs(5)),
        &w.env,
        "schedules",
        opts(),
    )
    .unwrap();
    assert!(tombstone::is_tombstone(&w.state_file()));
}

#[test]
fn a_tick_that_started_after_the_list_was_taken_is_ignored() {
    let w = world(false);
    install_sched(&w);
    // Nothing at the listing; a new one (launchd's new code, blocked on our lock) from then on.
    script(vec![vec![], vec![tick(777)]]);
    step(
        &reg(&w, Duration::from_millis(300)),
        &w.env,
        "schedules",
        opts(),
    )
    .unwrap();
}

#[test]
fn a_rollback_notes_an_old_tick_and_never_refuses() {
    let w = world(false);
    install_sched(&w);
    script(vec![]);
    step(
        &reg(&w, Duration::from_secs(1)),
        &w.env,
        "schedules",
        opts(),
    )
    .unwrap();
    script(vec![vec![tick(4242)]]);
    rollback(
        &reg(&w, Duration::from_millis(100)),
        &w.env,
        "schedules",
        opts(),
    )
    .unwrap();
    assert!(!tombstone::is_tombstone(&w.state_file()));
}

#[test]
fn the_wait_matches_by_pid_alone_when_a_stamp_is_unreadable_and_by_stamp_otherwise() {
    let program = PathBuf::from("/x/kith-sched");
    let wait = Duration::from_millis(150);
    // The old tick's stamp could not be read: the same pid is the same tick.
    script(vec![
        vec![Tick {
            pid: 9,
            stamp: None,
        }],
        vec![tick(9)],
    ]);
    assert!(
        wait_out_old_ticks(scripted, &program, wait)
            .unwrap_err()
            .contains("pid 9")
    );
    // Same pid, other stamp: the pid was reused, the old tick is gone.
    script(vec![
        vec![tick(9)],
        vec![Tick {
            pid: 9,
            stamp: Some("e1".into()),
        }],
    ]);
    assert_eq!(wait_out_old_ticks(scripted, &program, wait), Ok(()));
}

/// A `kith-sched`-named shell script that sleeps, run as `<script> <arg>`.
fn sleeper(dir: &Path, arg: &str) -> std::process::Child {
    std::fs::create_dir_all(dir).unwrap();
    let file = dir.join("kith-sched");
    std::fs::write(&file, "#!/bin/sh\nsleep 30\n").unwrap();
    let mut perm = std::fs::metadata(&file).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    std::fs::set_permissions(&file, perm).unwrap();
    std::process::Command::new(&file).arg(arg).spawn().unwrap()
}

#[test]
fn the_real_lister_finds_this_homes_ticks_and_not_run_turn_or_another_homes() {
    let t = tempfile::tempdir().unwrap();
    let mine = t.path().join("my home/bin");
    let other = t.path().join("other/bin");
    let mut tick_p = sleeper(&mine, "tick");
    let mut run_turn = sleeper(&t.path().join("my home/run"), "run-turn");
    let mut theirs = sleeper(&other, "tick");
    let program = mine.join("kith-sched").canonicalize().unwrap();
    // The new processes may not have exec'd their interpreters yet: "not listed" means nothing before they show.
    shows_own_argv(tick_p.id(), "tick");
    shows_own_argv(run_turn.id(), "run-turn");
    shows_own_argv(theirs.id(), "tick");
    let until = std::time::Instant::now() + Duration::from_secs(5);
    let found = loop {
        let l = list_ticks(&program).unwrap();
        if l.iter().any(|x| x.pid == tick_p.id()) || std::time::Instant::now() > until {
            break l;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let pids: Vec<u32> = found.iter().map(|x| x.pid).collect();
    // The shell may have forked its `sleep` and not yet exec'd it: that child shows the same arguments, so it may be listed too.
    assert!(pids.contains(&tick_p.id()), "{found:?}");
    assert!(
        !pids.contains(&run_turn.id()) && !pids.contains(&theirs.id()),
        "{found:?}"
    );
    assert!(found.iter().all(|x| x.stamp.is_some()));
    for c in [&mut tick_p, &mut run_turn, &mut theirs] {
        c.kill().unwrap();
        c.wait().unwrap();
    }
}

#[test]
fn the_program_is_the_one_launchd_runs_through_every_symlink() {
    let w = world(false);
    assert_eq!(sched_program(&w.env.home), None);
    install_sched(&w);
    let want = w
        .env
        .home
        .join("cli/current/bin/kith-sched")
        .canonicalize()
        .unwrap();
    assert_eq!(sched_program(&w.env.home), Some(want));
}

fn unreadable(_: &Path) -> Result<Vec<Tick>, String> {
    Err("the process list cannot be read here".into())
}

#[test]
fn a_process_list_that_cannot_be_read_refuses_and_never_reads_as_no_tick() {
    let why = wait_out_old_ticks(
        unreadable,
        Path::new("/x/kith-sched"),
        Duration::from_millis(50),
    )
    .unwrap_err();
    assert!(why.contains("cannot be read"), "{why}");
}

/// Kills and reaps its children, also when an assert fails first.
struct Reaped(Vec<std::process::Child>);

impl Drop for Reaped {
    fn drop(&mut self) {
        for c in &mut self.0 {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Waits until the process table shows `pid` with its own arguments (a freshly spawned one lists its parent's, or
/// nothing, until it has exec'd), so that "is not listed" is a statement about a process that can be listed.
fn shows_own_argv(pid: u32, verb: &str) {
    let until = std::time::Instant::now() + Duration::from_secs(10);
    while !kith_kernel::procstart::own_commands()
        .iter()
        .any(|(p, args)| *p == pid && args.iter().any(|a| a == verb))
    {
        assert!(std::time::Instant::now() < until, "pid {pid} never showed `{verb}`");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The form the installer writes for launchd's `dev.kith.sched`: `kith-uv run --quiet --script <kith-sched> tick`. The
/// launcher is a real program here (a script that loops on `sleep`, never `exec`, so it keeps its own arguments), so
/// `ps` shows the same argument shape as on Kai's Mac.
#[test]
fn the_real_lister_finds_a_tick_behind_the_uv_launcher_and_not_its_run_turn() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("my home/bin");
    std::fs::create_dir_all(&dir).unwrap();
    let launcher = dir.join("kith-uv");
    std::fs::write(&launcher, "#!/bin/sh\nwhile :; do sleep 1; done\n").unwrap();
    let mut perm = std::fs::metadata(&launcher).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    std::fs::set_permissions(&launcher, perm).unwrap();
    let sched = dir.join("kith-sched");
    std::fs::write(&sched, "#!/usr/bin/env python3\n").unwrap();
    let run = |verb: &str| {
        std::process::Command::new(&launcher)
            .args(["run", "--quiet", "--script"])
            .arg(&sched)
            .arg(verb)
            .spawn()
            .unwrap()
    };
    let (tick_p, run_turn) = (run("tick"), run("run-turn"));
    let (tick_pid, run_pid) = (tick_p.id(), run_turn.id());
    let _reap = Reaped(vec![tick_p, run_turn]);
    shows_own_argv(tick_pid, "tick");
    shows_own_argv(run_pid, "run-turn");
    // `/var` is `/private/var` on a Mac: the program is given as the lister resolves it.
    let found = list_ticks(&sched.canonicalize().unwrap()).unwrap();
    let pids: Vec<u32> = found.iter().map(|x| x.pid).collect();
    assert!(pids.contains(&tick_pid), "{found:?}");
    assert!(!pids.contains(&run_pid), "{found:?}");
    assert!(found.iter().all(|x| x.stamp.is_some()));
}
