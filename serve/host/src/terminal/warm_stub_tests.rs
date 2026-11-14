//! [`ResidentStub`] over a REAL `kith-stub --resident` whose engine is `cat` (it exits when the host drops its stdin): the lock
//! through the control channel, a restart on another account, and the group kill.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::core_writer::WarmStub;
use super::registry::pid_runs;
use super::warm_stub::{Launch, ResidentStub};
use kith_acp::{Release, Take};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn stub_bin() -> PathBuf {
    static BUILT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
            let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../core/stub/Cargo.toml");
            let built = Command::new(cargo).args(["build", "--manifest-path"]).arg(&manifest).status().unwrap();
            assert!(built.success(), "kith-stub did not build");
            // The test binary is in target/<profile>/deps; the stub is in target/<profile>.
            let stub = std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().join("kith-stub");
            assert!(stub.is_file(), "no {}", stub.display());
            stub
        })
        .clone()
}

fn locks(name: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    std::env::temp_dir().join(format!(
        "kith-ws-{name}-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ))
}

/// A warm process; `asked` records the account each (re)start was launched for.
fn warm(locks: &std::path::Path, account: Option<&str>) -> (ResidentStub, Arc<Mutex<Vec<Option<String>>>>) {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    let launcher = Box::new(move |account: Option<&str>| {
        seen.lock().unwrap().push(account.map(str::to_owned));
        // The default account is named, so the host can always say which one the process runs as.
        Launch {
            account: account.unwrap_or("main").to_owned(),
            command: "cat".into(),
            cwd: "/tmp".into(),
            env: Vec::new(),
        }
    });
    let stub = ResidentStub::spawn(&stub_bin(), locks, launcher, account).unwrap();
    (stub, asked)
}

fn until(what: &str, mut ok: impl FnMut() -> bool) {
    let by = Instant::now() + Duration::from_secs(5);
    while !ok() {
        assert!(Instant::now() < by, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_lock_is_taken_held_against_another_stub_and_released() {
    let dir = locks("lock");
    let (a, _) = warm(&dir, Some("main"));
    let (b, _) = warm(&dir, None);
    assert_eq!(b.account().as_deref(), Some("main"), "the default account has a name");
    assert_ne!(a.pid(), b.pid());
    assert!(pid_runs(a.pid()));
    assert_eq!(a.account().as_deref(), Some("main"));
    assert_eq!(a.take("sess-1", 7).unwrap(), Take::Locked);
    // A retried take of the same claim is `locked` again; another stub on the same conversation is busy.
    assert_eq!(a.take("sess-1", 7).unwrap(), Take::Locked);
    assert_eq!(b.take("sess-1", 8).unwrap(), Take::Busy);
    assert_eq!(a.release("sess-1", 7).unwrap(), Release::Released);
    assert_eq!(a.release("sess-1", 7).unwrap(), Release::NotHeld, "a repeat, or a release with no take, is fine");
    assert_eq!(b.take("sess-1", 8).unwrap(), Take::Locked);
    a.kill_group();
    b.kill_group();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_is_a_new_process_on_the_account_asked_and_the_old_lock_goes_with_it() {
    let dir = locks("restart");
    let (mut s, asked) = warm(&dir, Some("main"));
    let old = s.pid();
    assert_eq!(s.take("sess-1", 1).unwrap(), Take::Locked);
    s.restart(Some("spare")).unwrap();
    assert_ne!(s.pid(), old);
    assert!(!pid_runs(old), "the old group is gone before the next starts");
    assert_eq!(s.account().as_deref(), Some("spare"));
    assert_eq!(*asked.lock().unwrap(), [Some("main".to_owned()), Some("spare".to_owned())]);
    assert!(s.take_incoming().is_some(), "the new process has its own incoming channel");
    // The dead stub's lock is free for the new one (another claim of the same conversation).
    assert_eq!(s.take("sess-1", 2).unwrap(), Take::Locked);
    // No account named: the process keeps the one it has.
    s.restart(None).unwrap();
    assert_eq!(s.account().as_deref(), Some("spare"));
    s.kill_group();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_group_ends_the_stub_and_the_channel_answers_nothing_after() {
    let dir = locks("kill");
    let (s, _) = warm(&dir, None);
    let pid = s.pid();
    assert_eq!(s.take("sess-1", 1).unwrap(), Take::Locked);
    s.kill_group();
    until("the stub to be gone", || !pid_runs(pid));
    assert!(s.take("sess-1", 2).is_err(), "a dead stub gives no answer");
    assert!(!s.ctl_open(), "and the channel is broken for good");
}
