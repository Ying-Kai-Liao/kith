//! Windows tests of the claims and their locks (W12-4): `LockFileEx` through `fs2`, liveness through procstart, and the sweep that
//! ends a holder. The Unix suites (`tests.rs` and the writer ones) start `sh` and read file modes, so they stay Unix only.
#![allow(clippy::unwrap_used)]

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use fs2::FileExt as _;

use super::*;

static N: AtomicU32 = AtomicU32::new(0);

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        // A folder with a space in its name: the paths here are the user's.
        let dir = std::env::temp_dir().join(format!(
            "kith threads {}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Tmp(dir)
    }

    fn state(&self) -> PathBuf {
        self.0.join("state")
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn no_env(_: &str) -> Option<String> {
    None
}

fn ctx<'a>(t: &Tmp, env: &'a dyn Fn(&str) -> Option<String>) -> Ctx<'a> {
    Ctx {
        state: Some(t.state()),
        machine: Some(t.0.join("machine")),
        home: t.0.join("home"),
        env,
    }
}

/// A process that lives until the guard drops: a claimer that is not the test itself.
struct Sleeper(Child);

impl Sleeper {
    fn new() -> Sleeper {
        Sleeper(
            Command::new("cmd")
                .args(["/C", "ping -n 60 127.0.0.1 >NUL"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }

    fn pid(&self) -> i64 {
        i64::from(self.0.id())
    }

    fn end(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for Sleeper {
    fn drop(&mut self) {
        self.end();
    }
}

fn me() -> i64 {
    i64::from(std::process::id())
}

fn open(path: &Path) -> File {
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .unwrap()
}

fn wait_for(path: &Path) {
    let end = Instant::now() + Duration::from_secs(20);
    while !path.exists() {
        assert!(Instant::now() < end, "{} never appeared", path.display());
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_second_handle_cannot_take_the_lock_while_the_first_holds_it() {
    let t = Tmp::new();
    let path = t.0.join("a b.claim.lock");
    let first = Lock::exclusive(&path).unwrap();
    let second = open(&path);
    assert!(second.try_lock_exclusive().is_err());
    assert!(second.try_lock_shared().is_err());
    drop(first);
    second.try_lock_exclusive().unwrap();
    // And the other way: a shared lock keeps the exclusive one out.
    second.unlock().unwrap();
    second.lock_shared().unwrap();
    let third = open(&path);
    assert!(third.try_lock_exclusive().is_err());
    third.try_lock_shared().unwrap();
}

#[test]
fn the_registry_lock_is_given_up_after_its_wait_and_taken_again_once_free() {
    let t = Tmp::new();
    let reg = t.0.join("reg");
    let held = registry::sid_lock(&reg, "abc-1").unwrap();
    let began = Instant::now();
    assert!(registry::sid_lock(&reg, "abc-1").is_none());
    assert!(began.elapsed() >= Duration::from_millis(900));
    // another session's lock is another file
    assert!(registry::sid_lock(&reg, "abc-2").is_some());
    drop(held);
    assert!(registry::sid_lock(&reg, "abc-1").is_some());
}

/// The child half of `a_lock_ends_with_its_process`: with `KITH_TEST_LOCK` set it takes the lock, says so and waits to be ended;
/// run on its own it is a test that does nothing.
#[test]
fn lock_holder() {
    let Some(path) = std::env::var_os("KITH_TEST_LOCK") else {
        return;
    };
    let path = PathBuf::from(path);
    let lock = Lock::exclusive(&path).unwrap();
    fs::write(path.with_extension("ready"), "held").unwrap();
    std::thread::sleep(Duration::from_secs(120));
    drop(lock);
}

#[test]
fn a_lock_ends_with_its_process() {
    let t = Tmp::new();
    let path = t.0.join("holder.lock");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "threads::win_tests::lock_holder",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("KITH_TEST_LOCK", &path)
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    wait_for(&path.with_extension("ready"));
    let ours = open(&path);
    assert!(ours.try_lock_exclusive().is_err(), "the child holds it");
    // Ended hard, as a crash: nothing in it runs to let go.
    child.kill().unwrap();
    child.wait().unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    while ours.try_lock_exclusive().is_err() {
        assert!(
            Instant::now() < end,
            "the dead process still holds the lock"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_live_claim_holds_the_session_and_a_dead_ones_is_replaced() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let mut holder = Sleeper::new();
    take_claim(&c, "sess-1", holder.pid(), &Take::new("terminal"))
        .unwrap()
        .unwrap();
    let claim = read_claim(&claim_path(&registry_dir(&t.state()), "sess-1", false)).unwrap();
    assert_eq!(claim.pid, Some(holder.pid()));
    assert!(
        claim
            .proc_start
            .as_deref()
            .is_some_and(crate::procstart::is_stamp)
    );
    assert!(claim.is_live());
    assert!(claim.is_claimer());
    let Err(Held(seen)) = take_claim(&c, "sess-1", me(), &Take::new("terminal")).unwrap() else {
        panic!("a live holder keeps the session");
    };
    assert_eq!(seen.pid, Some(holder.pid()));
    holder.end();
    assert!(!claim.is_live());
    take_claim(&c, "sess-1", me(), &Take::new("terminal"))
        .unwrap()
        .unwrap();
    let now = read_claim(&claim_path(&registry_dir(&t.state()), "sess-1", false)).unwrap();
    assert_eq!(now.pid, Some(me()));
    release_claims(&c, me());
    assert!(read_claim(&claim_path(&registry_dir(&t.state()), "sess-1", false)).is_none());
}

#[test]
fn a_reused_pid_is_not_the_claimer() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    // A claim for this very pid, but a start that is not ours: the pid was handed on.
    let dir = registry_dir(&t.state());
    fs::create_dir_all(&dir).unwrap();
    let theirs = Claim {
        pid: Some(me()),
        proc_start: Some("e1".to_owned()),
        at: iso_utc(),
        kind: "terminal".to_owned(),
        person: None,
        agent: None,
        takeover: false,
    };
    write_json(
        &claim_path(&dir, "sess-2", false),
        &theirs.to_value(),
        false,
    )
    .unwrap();
    assert!(!theirs.is_live());
    take_claim(&c, "sess-2", me(), &Take::new("terminal"))
        .unwrap()
        .unwrap();
    assert!(
        read_claim(&claim_path(&dir, "sess-2", false))
            .unwrap()
            .is_claimer()
    );
}

#[test]
fn ending_a_holder_ends_its_children_too() {
    let mut top = Command::new("cmd")
        .args(["/C", "ping -n 60 127.0.0.1 >NUL & rem"])
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    // `cmd /C` runs ping as its child: the sweep takes both.
    let end = Instant::now() + Duration::from_secs(10);
    let mut kid = None;
    while kid.is_none() && Instant::now() < end {
        kid = os::ps_table()
            .into_iter()
            .find(|(_, parent)| *parent == i64::from(top.id()))
            .map(|(pid, _)| pid);
        std::thread::sleep(Duration::from_millis(50));
    }
    let kid = kid.expect("cmd started ping");
    let ended = end_holders(vec![i64::from(top.id())], Duration::ZERO);
    assert!(ended.contains(&i64::from(top.id())) && ended.contains(&kid));
    top.wait().unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    while os::pid_runs(kid) {
        assert!(Instant::now() < end, "the child is still running");
        std::thread::sleep(Duration::from_millis(50));
    }
}
