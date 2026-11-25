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

// --- the folders of a person, the kernel's service specs, the mover and the repair ------------------------------------

use std::sync::Mutex;

use kith_kernel::secrets::{Found, PackageSource};

use crate::authority::revoke::{Life, LifeMove};

fn life_of(t: &tempfile::TempDir) -> Life {
    let r = t.path();
    Life {
        agents: r.join("agents"),
        state: r.join("state"),
        machine: r.join("machine"),
        kernel: r.join("kernel"),
        pkg: None,
    }
}

fn mv(life: &Life, old: &str, new: &str, earlier: &[&str]) -> LifeMove {
    LifeMove {
        life: life.clone(),
        old: old.to_owned(),
        new: new.to_owned(),
        earlier: earlier.iter().map(|s| (*s).to_owned()).collect(),
    }
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("gave up waiting for {what}");
}

/// A probe that holds the mover (and only it) while the flag is set.
fn hold_mover() -> (Arc<AtomicBool>, Probe) {
    let hold = Arc::new(AtomicBool::new(false));
    let h = hold.clone();
    let probe: Probe = Arc::new(move || {
        while super::current_job() == "move" && h.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    (hold, probe)
}

#[test]
fn a_persons_folders_are_read_again_for_every_call_and_a_stale_answer_is_a_miss() {
    let (t, c) = machine();
    let life = life_of(&t);
    write(&t.path().join("agents/house/memory/people/sam/n.md"), "x");
    let cache = PackageCache::start(None);
    let (r, kernel) = (roots(&c), c.kernel.clone());
    let ask = || cache.life(&r.0, &kernel, &life.state, "sam");
    {
        // no call in progress: nothing was validated for it
        assert!(ask().is_none());
    }
    let _call = begin(Instant::now(), true);
    assert!(ask().is_none() && pass_missed());
    assert!(cache.refresh(&r, Want::Life(life.state.clone(), "sam".into())));
    let got = ask().unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].0, "house");
    // a folder that went away is seen by the next call that asks, not before
    std::fs::remove_dir_all(t.path().join("agents/house/memory/people/sam")).unwrap();
    assert_eq!(ask().unwrap().len(), 1);
    assert!(cache.refresh(&r, Want::Life(life.state.clone(), "sam".into())));
    assert!(ask().unwrap().is_empty());
}

#[test]
fn a_kernel_service_spec_is_absent_bytes_or_changed() {
    let (_t, c) = machine();
    let cache = PackageCache::start(None);
    let (r, kernel) = (roots(&c), c.kernel.clone());
    let _call = begin(Instant::now(), false);
    let get = || {
        assert!(cache.refresh(&r, Want::Service("github".into())));
        cache.kernel_service(&r.0, &kernel, "github").unwrap()
    };
    assert_eq!(*get(), Found::Absent);
    write(&kernel.join("services/github.json"), "{\"a\": 1}");
    assert_eq!(*get(), Found::Bytes(b"{\"a\": 1}".to_vec()));
    write(&kernel.join("services/github.json"), "{\"a\": 22}");
    assert_eq!(*get(), Found::Bytes(b"{\"a\": 22}".to_vec()));
    std::fs::remove_file(kernel.join("services/github.json")).unwrap();
    assert_eq!(*get(), Found::Absent);
}

#[test]
fn a_rename_with_the_mover_blocked_answers_with_the_note_and_the_folders_follow_when_it_is_let_go()
{
    let (t, _c) = machine();
    let life = life_of(&t);
    let from = t.path().join("agents/house/memory/people/sam");
    write(&from.join("n.md"), "x");
    let (hold, probe) = hold_mover();
    hold.store(true, Ordering::SeqCst);
    let cache = PackageCache::start(Some(probe));
    let wait = cache.queue_move(mv(&life, "sam", "anna", &["sam"]));
    let began = Instant::now();
    let said = wait.finish(
        json!({"lines": ["sam is now anna"]}),
        Duration::from_millis(300),
    );
    assert!(began.elapsed() < Duration::from_secs(2));
    assert_eq!(
        said["lines"],
        json!([
            "sam is now anna",
            "note: the folders of sam move to anna when the disk answers"
        ])
    );
    assert!(from.exists(), "nothing moved while the disk did not answer");
    hold.store(false, Ordering::SeqCst);
    let to = t.path().join("agents/house/memory/people/anna");
    wait_for("the folders to follow", || to.join("n.md").exists());
    assert!(!from.exists());
}

#[test]
fn a_move_that_finishes_after_the_caller_gave_up_logs_its_notes() {
    let (t, _c) = machine();
    let life = life_of(&t);
    write(&t.path().join("agents/house/memory/people/sam/n.md"), "x");
    write(
        &t.path().join("agents/house/memory/people/anna/o.md"),
        "taken",
    );
    let (hold, probe) = hold_mover();
    hold.store(true, Ordering::SeqCst);
    let cache = PackageCache::start(Some(probe));
    let said: Arc<Mutex<Vec<String>>> = Arc::default();
    let s = said.clone();
    cache.set_log(Arc::new(move |n| s.lock().unwrap().push(n.to_owned())));
    let wait = cache.queue_move(mv(&life, "sam", "anna", &["sam"]));
    // the caller's wait ends first: its answer cannot carry the move's notes any more
    let _ = wait.finish(
        json!({"lines": ["sam is now anna"]}),
        Duration::from_millis(50),
    );
    hold.store(false, Ordering::SeqCst);
    wait_for("the notes in the log", || !said.lock().unwrap().is_empty());
    let said = said.lock().unwrap().clone();
    assert!(said[0].contains("is already there"), "{said:?}");
}

#[test]
fn a_rename_with_no_mover_thread_says_the_folders_move_at_the_next_start() {
    let (t, _c) = machine();
    let life = life_of(&t);
    let cache = PackageCache::start(None);
    // the mover's channel is closed the way a thread that never started closes it
    let wait = {
        let (report, rx) = std::sync::mpsc::sync_channel(1);
        drop(report);
        crate::packages::MoveWait::gone_for_test(rx, mv(&life, "sam", "anna", &["sam"]))
    };
    drop(cache);
    let said = wait.finish(
        json!({"lines": ["sam is now anna"]}),
        Duration::from_secs(1),
    );
    assert_eq!(
        said["lines"],
        json!([
            "sam is now anna",
            "note: the folders of sam move to anna at the next start"
        ])
    );
}

#[test]
fn a_chain_of_renames_queued_behind_a_blocked_mover_ends_under_the_last_id_and_nothing_under_the_middle()
 {
    let (t, _c) = machine();
    let life = life_of(&t);
    let a = t.path().join("agents/house/memory/people/a");
    write(&a.join("n.md"), "x");
    write(&t.path().join("agents/house/files/people/a/f.md"), "x");
    write(&t.path().join("state/people/a.md"), "x");
    let (hold, probe) = hold_mover();
    hold.store(true, Ordering::SeqCst);
    let cache = PackageCache::start(Some(probe));
    // A to B, then B to C, both committed while the mover could not run
    let first = cache.queue_move(mv(&life, "a", "b", &["a"]));
    let second = cache.queue_move(mv(&life, "b", "c", &["a", "b"]));
    // a refresh is still answered: the mover is not the refresher
    assert!(cache.refresh(
        &(life.agents.clone(), life.kernel.clone()),
        Want::Life(life.state.clone(), "c".into())
    ));
    hold.store(false, Ordering::SeqCst);
    let (_, _) = (
        first.finish(json!({"lines": ["a is now b"]}), Duration::from_secs(5)),
        second.finish(json!({"lines": ["b is now c"]}), Duration::from_secs(5)),
    );
    for under in ["agents/house/memory/people", "agents/house/files/people"] {
        let d = t.path().join(under);
        assert!(!d.join("a").exists() && !d.join("b").exists(), "{under}");
        assert!(d.join("c").exists(), "{under}");
    }
    assert!(t.path().join("state/people/c.md").exists());
    assert!(!t.path().join("state/people/b.md").exists());
    assert!(!t.path().join("state/people/a.md").exists());
}

#[test]
fn the_repair_moves_old_folders_to_the_current_id_leaves_a_taken_one_and_says_so_once() {
    let (t, _c) = machine();
    let life = life_of(&t);
    let mem = |id: &str| t.path().join(format!("agents/house/memory/people/{id}"));
    let files = |id: &str| t.path().join(format!("agents/house/files/people/{id}"));
    write(&mem("a").join("n.md"), "x");
    write(&files("a").join("f.md"), "x");
    write(&files("c").join("other.md"), "already here");
    // a removed person (no row, so no repair is asked for) keeps what they have
    write(&mem("gone").join("n.md"), "x");
    let cache = PackageCache::start(None);
    let said: Arc<Mutex<Vec<String>>> = Arc::default();
    let s = said.clone();
    cache.set_log(Arc::new(move |n| s.lock().unwrap().push(n.to_owned())));
    cache.queue_repair(vec![mv(&life, "a", "c", &[])]);
    cache.queue_repair(vec![mv(&life, "a", "c", &[])]);
    wait_for("the repair", || mem("c").join("n.md").exists());
    // the second job finds what the first left and notes it too; the queue is in order, so give it time to have run
    std::thread::sleep(Duration::from_millis(300));
    assert!(!mem("a").exists());
    assert!(
        files("a").join("f.md").exists(),
        "the taken destination is left alone"
    );
    assert!(files("c").join("other.md").exists());
    assert!(mem("gone").join("n.md").exists());
    let said: Vec<String> = said.lock().unwrap().clone();
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("is already there"), "{said:?}");
}

#[test]
fn only_a_person_with_a_row_has_old_ids_to_repair() {
    let auth = json!({"people": {
        "c": {"was": ["a", "b"]},
        "kai": {"owner": true},
        "bot": {"bot_of": "c", "was": ["old-bot"]},
    }});
    let mut got = crate::authority::revoke::earlier_ids(&auth);
    got.sort();
    assert_eq!(
        got,
        [
            ("a".to_owned(), "c".to_owned()),
            ("b".to_owned(), "c".to_owned())
        ]
    );
}
