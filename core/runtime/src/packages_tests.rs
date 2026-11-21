//! The package cache: the same answers as the disk on a healthy one, a changed file read again (a replace by rename
//! included), and the narrower answer for a disk that does not answer.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use kith_kernel::secrets::{Ctx, Packages, Rules, Sandbox, ServeDir};
use serde_json::json;

use super::{PackageCache, Probe, Want, begin, pass_missed};

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// A machine: `house` (package under role/, jobs and domains, a service spec), `garden` (agent.md in its own folder),
/// `sound`, and the kernel's built-in `scratch`.
fn machine() -> (tempfile::TempDir, Ctx) {
    let t = tempfile::tempdir().unwrap();
    let r = t.path();
    write(
        &r.join("agents/house/role/agent.md"),
        "---\nname: house\n---\n",
    );
    write(
        &r.join("agents/house/role/capabilities.yaml"),
        "guest_domains: [a.example]\njobs:\n  - name: work\n    domains: [b.example]\n",
    );
    write(&r.join("agents/house/role/services/x.json"), "{\"k\": 1}");
    write(
        &r.join("agents/garden/agent.md"),
        "---\nname: garden\n---\n",
    );
    write(
        &r.join("agents/sound/role/agent.md"),
        "---\nname: sound\n---\n",
    );
    write(
        &r.join("kernel/agents/scratch/role/agent.md"),
        "---\nname: scratch\n---\n",
    );
    let c = Ctx {
        auth: r.join("secrets/auth.json"),
        agents: r.join("agents"),
        kernel: r.join("kernel"),
        person: None,
        now_ms: 1_800_000_000_000.0,
        sandbox: Sandbox::Fixed(false),
        serve: ServeDir::Fixed(r.join("serve")),
        packages: Packages::Disk,
    };
    (t, c)
}

fn cached(c: &Ctx, cache: &Arc<PackageCache>) -> Ctx {
    let mut c = c.clone();
    c.packages = Packages::Source(cache.clone());
    c
}

fn roots(c: &Ctx) -> (std::path::PathBuf, std::path::PathBuf) {
    (c.agents.clone(), c.kernel.clone())
}

const NAMES: [&str; 8] = [
    "house", "garden", "sound", "scratch", "nope", "", "../x", ".hidden",
];

#[test]
fn the_cache_gives_the_answers_the_disk_gives() {
    let (_t, disk) = machine();
    let cache = PackageCache::start(None);
    // Facts count for the call that had them validated: the call begins first.
    let _call = begin(Instant::now(), false);
    assert!(cache.refresh(&roots(&disk), Want::All));
    assert!(cache.refresh(
        &roots(&disk),
        Want::Agents(NAMES.iter().map(|n| (*n).to_owned()).collect())
    ));
    let mem = cached(&disk, &cache);
    for n in NAMES {
        assert_eq!(disk.package_dir(n), mem.package_dir(n), "package_dir {n:?}");
        assert_eq!(
            disk.agent_exists(n),
            mem.agent_exists(n),
            "agent_exists {n:?}"
        );
        assert_eq!(disk.is_builtin(n), mem.is_builtin(n), "is_builtin {n:?}");
        assert_eq!(
            disk.service_file(n, "x.json"),
            mem.service_file(n, "x.json"),
            "service_file {n:?}"
        );
        let auth = json!({});
        assert_eq!(
            Rules::new(&disk, &auth).agent_domains(n),
            Rules::new(&mem, &auth).agent_domains(n),
            "agent_domains {n:?}"
        );
        assert_eq!(
            Rules::new(&disk, &auth).declared_job(n, "work"),
            Rules::new(&mem, &auth).declared_job(n, "work"),
            "declared_job {n:?}"
        );
    }
    assert_eq!(disk.agent_names(), mem.agent_names());
    assert_eq!(disk.agent_names(), ["garden", "house", "sound", "scratch"]);
    assert!(!pass_missed());
}

#[test]
fn a_changed_file_is_read_again_and_so_is_one_replaced_by_rename_with_the_same_mtime() {
    let (t, disk) = machine();
    let cache = PackageCache::start(None);
    let mem = cached(&disk, &cache);
    let _call = begin(Instant::now(), false);
    let auth = json!({});
    let caps = t.path().join("agents/house/role/capabilities.yaml");
    let domains = || Rules::new(&mem, &auth).agent_domains("house");
    assert!(cache.refresh(&roots(&disk), Want::Agents(vec!["house".into()])));
    assert_eq!(domains(), ["a.example"]);
    // The domain is removed: the next call drops it.
    std::fs::write(&caps, "jobs:\n").unwrap();
    assert!(cache.refresh(&roots(&disk), Want::Agents(vec!["house".into()])));
    assert_eq!(domains(), Vec::<String>::new());
    // Replaced by rename, same size, same mtime, another inode: only the inode and the change time tell.
    std::fs::write(&caps, "guest_domains: [a.example]\n").unwrap();
    assert!(cache.refresh(&roots(&disk), Want::Agents(vec!["house".into()])));
    assert_eq!(domains(), ["a.example"]);
    let (before, mtime) = {
        use std::os::unix::fs::MetadataExt as _;
        let m = std::fs::metadata(&caps).unwrap();
        (m.ino(), m.modified().unwrap())
    };
    let tmp = caps.with_extension("new");
    std::fs::write(&tmp, "guest_domains: [b.example]\n").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&tmp)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    std::fs::rename(&tmp, &caps).unwrap();
    {
        use std::os::unix::fs::MetadataExt as _;
        assert_ne!(std::fs::metadata(&caps).unwrap().ino(), before);
    }
    assert!(cache.refresh(&roots(&disk), Want::Agents(vec!["house".into()])));
    assert_eq!(domains(), ["b.example"]);
}

#[test]
fn a_new_agent_and_a_deleted_one_show_at_the_next_refresh() {
    let (t, disk) = machine();
    let cache = PackageCache::start(None);
    let mem = cached(&disk, &cache);
    let _call = begin(Instant::now(), false);
    assert!(cache.refresh(&roots(&disk), Want::All));
    assert!(!mem.agent_exists("late"));
    write(
        &t.path().join("agents/late/role/agent.md"),
        "---\nname: late\n---\n",
    );
    std::fs::remove_dir_all(t.path().join("agents/sound")).unwrap();
    assert!(cache.refresh(&roots(&disk), Want::All));
    assert!(mem.agent_exists("late"));
    assert!(!mem.agent_exists("sound"));
    assert_eq!(mem.agent_names(), disk.agent_names());
}

/// A refresher whose read never ends until `release` is set.
fn stuck() -> (Probe, Arc<AtomicBool>) {
    let hold = Arc::new(AtomicBool::new(true));
    let h = hold.clone();
    let probe: Probe = Arc::new(move || {
        while h.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    (probe, hold)
}

#[test]
fn a_refresher_that_does_not_answer_gives_the_narrower_answer_and_the_wait_is_bounded() {
    let (_t, disk) = machine();
    let (probe, hold) = stuck();
    let cache = PackageCache::start_with(Some(probe), Duration::from_millis(200));
    let mem = cached(&disk, &cache);
    let began = Instant::now();
    assert!(!cache.refresh(&roots(&disk), Want::Agents(vec!["house".into()])));
    assert!(began.elapsed() < Duration::from_secs(2));
    let _call = begin(Instant::now(), true);
    let auth = json!({});
    assert_eq!(
        Rules::new(&mem, &auth).agent_domains("house"),
        Vec::<String>::new()
    );
    assert!(
        Rules::new(&mem, &auth)
            .declared_job("house", "work")
            .is_none()
    );
    assert!(!mem.agent_exists("house"));
    assert!(
        mem.is_builtin("house"),
        "an unreadable package is owner-only"
    );
    assert_eq!(mem.agent_names(), Vec::<String>::new());
    assert!(
        mem.unreadable("house")
            .unwrap()
            .contains("can't read house's package")
    );
    assert!(
        pass_missed(),
        "the transaction is told to roll back and ask again"
    );
    hold.store(false, Ordering::SeqCst);
}

#[test]
fn a_fact_validated_before_the_call_began_is_a_miss() {
    let (_t, disk) = machine();
    let cache = PackageCache::start(None);
    let mem = cached(&disk, &cache);
    assert!(cache.refresh(&roots(&disk), Want::Agents(vec!["house".into()])));
    std::thread::sleep(Duration::from_millis(5));
    {
        let _call = begin(Instant::now(), true);
        assert!(!mem.agent_exists("house"));
        assert!(pass_missed());
    }
    // The last pass answers a miss as unavailable without raising it.
    let _call = begin(Instant::now(), false);
    assert!(!mem.agent_exists("house"));
    assert!(!pass_missed());
}

#[test]
fn a_thread_with_no_call_takes_nothing_from_the_cache_and_a_miss_is_counted_even_when_not_raised() {
    let (_t, disk) = machine();
    let cache = PackageCache::start(None);
    let mem = cached(&disk, &cache);
    assert!(cache.refresh(&roots(&disk), Want::Agents(vec!["house".into()])));
    // The daemon's own loops have no call that had the fact checked: it is unavailable to them, however fresh.
    assert!(!mem.agent_exists("house"));
    assert_eq!(super::miss_count(), 0);
    let _call = begin(Instant::now(), false);
    assert!(!mem.agent_exists("house"));
    assert!(!pass_missed());
    assert_eq!(super::miss_count(), 1);
}

#[test]
#[should_panic(expected = "read under the core lock")]
fn the_structural_check_catches_a_disk_read_under_the_lock() {
    super::forbid_disk_under_lock();
    let (_t, disk) = machine();
    crate::daemon::mark_lock_held(true);
    let _ = disk.agent_exists("house");
}
