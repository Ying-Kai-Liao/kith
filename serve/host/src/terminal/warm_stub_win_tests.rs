//! [`ResidentStub`] on Windows (W12-7) over a REAL `kith-stub --resident`: the writer lock through the named pipe, and the stub
//! in a job the host holds (rule 6): ended with the host's job, whether the host ends it (`kill_group`) or lets go of it, and its
//! engine with it (the stub's own job, W12-1). The engine is `ping`, which ignores stdin and runs about two minutes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::core_writer::WarmStub;
use super::registry::pid_runs;
use super::warm_stub::{Launch, ResidentStub};
use kith_acp::{Release, Take};
use std::path::PathBuf;
use std::process::Command;
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
            let stub = std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().join("kith-stub.exe");
            assert!(stub.is_file(), "no {}", stub.display());
            stub
        })
        .clone()
}

fn locks(name: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    std::env::temp_dir().join(format!(
        "kith-wsw-{name}-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ))
}

fn warm(locks: &std::path::Path) -> ResidentStub {
    let launcher = Box::new(|account: Option<&str>| Launch {
        account: account.unwrap_or("main").to_owned(),
        command: "ping -n 120 127.0.0.1 >nul".into(),
        cwd: std::env::temp_dir().to_string_lossy().into_owned(),
        env: Vec::new(),
    });
    ResidentStub::spawn(&stub_bin(), locks, launcher, Some("main")).unwrap()
}

fn until(what: &str, mut ok: impl FnMut() -> bool) {
    let by = Instant::now() + Duration::from_secs(15);
    while !ok() {
        assert!(Instant::now() < by, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Every process below `root` now (its engine's shell and what that started), by the parent pids of one snapshot.
fn below(root: u32) -> Vec<u32> {
    let table = kith_winsys::procs::table().unwrap();
    let mut found = vec![root];
    let mut i = 0;
    while i < found.len() {
        let at = found[i];
        let kids: Vec<u32> =
            table.iter().filter(|r| r.parent == at && !found.contains(&r.pid)).map(|r| r.pid).collect();
        found.extend(kids);
        i += 1;
    }
    found.remove(0);
    found
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_lock_is_taken_and_released_over_the_named_pipe() {
    let dir = locks("lock");
    let a = warm(&dir);
    let b = warm(&dir);
    assert_ne!(a.pid(), b.pid());
    assert!(pid_runs(a.pid()));
    assert_eq!(a.take("sess-1", 7).unwrap(), Take::Locked);
    // A retried take of the same claim is `locked` again; another stub on the same conversation is busy.
    assert_eq!(a.take("sess-1", 7).unwrap(), Take::Locked);
    assert_eq!(b.take("sess-1", 8).unwrap(), Take::Busy);
    assert_eq!(a.release("sess-1", 7).unwrap(), Release::Released);
    assert_eq!(b.take("sess-1", 8).unwrap(), Take::Locked, "free once released");
    a.kill_group();
    b.kill_group();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resident_stub_and_its_engine_end_with_the_hosts_job() {
    let dir = locks("job");
    let a = warm(&dir);
    let (pid, held) = (a.pid(), a.take("sess-1", 1).unwrap());
    assert_eq!(held, Take::Locked);
    // The engine starts after the control channel opens: wait for the tree to exist, so the check below means something.
    let mut tree = Vec::new();
    until("the engine to start", || {
        tree = below(pid);
        tree.len() >= 2
    });
    // The host ends the job: the stub, its engine's shell and the engine are gone, and the stub's pid is free for the claim to end.
    a.kill_group();
    until("the stub to end", || !pid_runs(pid));
    for p in tree {
        until("the engine's processes to end", || !pid_runs(p));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stub_the_host_lets_go_of_ends_as_a_host_that_exits_does() {
    let dir = locks("drop");
    let a = warm(&dir);
    let pid = a.pid();
    assert_eq!(a.take("sess-1", 1).unwrap(), Take::Locked);
    let mut tree = Vec::new();
    until("the engine to start", || {
        tree = below(pid);
        tree.len() >= 2
    });
    // Dropping the last handle of the host's job is what a host exit does: nothing is left for a restart to find.
    drop(a);
    until("the stub to end", || !pid_runs(pid));
    for p in tree {
        until("the engine's processes to end", || !pid_runs(p));
    }
    // Another stub can take the lock at once: the old one's is gone with it.
    let b = warm(&dir);
    assert_eq!(b.take("sess-1", 2).unwrap(), Take::Locked);
    b.kill_group();
}
