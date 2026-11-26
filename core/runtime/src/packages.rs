//! The daemon's package facts (see `kith_kernel::secrets::Packages`): what the access rules ask of an agent's package
//! folder, answered from memory under the core lock and filled by ONE thread outside it. A folder on a disk that does not
//! answer (an external drive behind a prompt nobody sees) then stops that thread and nothing else: the calls that need
//! the fact give the narrower answer after [`REFRESH_WAIT`], and the calls that don't need it never notice.
//!
//! The same thread answers for the rest of what the authority rules ask of the disk: the folders a person has under the
//! agents and state folders (`Want::Life`), and the kernel's service specs (`Want::Service`). The folder moves a rename
//! leaves to be done once the core lock is let go (see [`PackageCache::queue_move`]), and the repair of the moves an
//! earlier run did not finish, run on a second thread, in the order they were queued. A stuck disk stops those two
//! threads; no call that holds the core lock waits on either.
//!
//! The call path (`Shared::dispatch`, via [`run_call`]): refresh the agents the params name, then run the call's pass
//! under the core lock. A fact the cache was not asked to validate for this call is a *miss*: the rules read it as
//! unavailable, the authority transaction notices the miss (`authority::store`) and rolls back with `NeedPackages`, the
//! lock is let go, the refresher is asked for what was missed, and the call runs again from scratch on a new snapshot.
//! At most two such rounds; a last pass answers misses as unavailable without raising. Nothing from an earlier pass
//! carries over but the cache's contents.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender, channel, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use kith_kernel::paths::Paths;
use kith_kernel::secrets::{AgentFacts, Found, PackageSource, read_names};
use serde_json::Value;

use crate::authority::revoke::{LifeMove, MoveReport, person_folders, read_service, run_move};
use crate::error::{Error, Result};

/// How long a call waits for the refresher before it takes the narrower answer.
pub(crate) const REFRESH_WAIT: Duration = Duration::from_millis(1500);
/// A fact validated this long before the call began still counts as fresh for it. Zero: the cost is one stat per named
/// agent per call, and nothing is read from memory that the call did not see checked.
const FRESH: Duration = Duration::ZERO;
/// Refresh rounds a call may spend on misses after its first pass.
const MISS_ROUNDS: usize = 2;
/// How long the caller of a rename waits, outside the lock, for its folders to move before it answers without them.
pub(crate) const MOVE_WAIT: Duration = Duration::from_secs(5);
/// Requests waiting for a refresher that is stuck are dropped past this many.
const MAX_QUEUED: usize = 256;
/// Agents kept; a call may name any string, so the map is emptied (and refilled by what is asked next) past this.
const MAX_AGENTS: usize = 1024;

type Roots = (PathBuf, PathBuf);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Want {
    /// These agents.
    Agents(Vec<String>),
    /// The names of all agents, and each of them.
    All,
    /// The folders kept for a person: under every agent, and in this state folder.
    Life(PathBuf, String),
    /// `<kernel>/services/<name>.json`.
    Service(String),
}

/// What a pass asked of the cache and did not get.
#[derive(Debug)]
struct Miss {
    roots: Roots,
    want: Want,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    size: u64,
    mtime: i128,
    ctime: i128,
}

fn stamp(path: &Path) -> Option<Stamp> {
    let m = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        Some(Stamp {
            dev: m.dev(),
            ino: m.ino(),
            size: m.size(),
            mtime: i128::from(m.mtime()) * 1_000_000_000 + i128::from(m.mtime_nsec()),
            ctime: i128::from(m.ctime()) * 1_000_000_000 + i128::from(m.ctime_nsec()),
        })
    }
    #[cfg(not(unix))]
    {
        let t = m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map_or(0, |d| i128::try_from(d.as_nanos()).unwrap_or(0));
        // No inode here: a file swapped in by rename shows as a new creation time instead (100 ns ticks since 1601).
        #[cfg(windows)]
        let ctime = {
            use std::os::windows::fs::MetadataExt as _;
            i128::from(m.creation_time()) * 100
        };
        #[cfg(not(windows))]
        let ctime = t;
        Some(Stamp {
            dev: 0,
            ino: 0,
            size: m.len(),
            mtime: t,
            ctime,
        })
    }
}

struct Entry {
    facts: Arc<AgentFacts>,
    watched: Vec<PathBuf>,
    stamps: Vec<Option<Stamp>>,
    validated: Instant,
    /// Changed so recently when it was read that the read may have missed the end of the change: read again next time.
    unsettled: bool,
}

/// A kernel service spec as read, and what it was read from. Reread only when the file's stamp changed.
struct Svc {
    found: Arc<Found>,
    stamp: Option<Stamp>,
    validated: Instant,
    unsettled: bool,
}

type Folders = Arc<Vec<(String, PathBuf)>>;

#[derive(Default)]
struct State {
    agents: HashMap<(PathBuf, PathBuf, String), Entry>,
    names: HashMap<Roots, (Instant, Arc<Vec<String>>)>,
    /// A person's folders, by (agents, state, id): looked at again for every call (nothing is stamped), so each entry is
    /// good only for the call that asked for it.
    lives: HashMap<(PathBuf, PathBuf, String), (Instant, Folders)>,
    services: HashMap<(PathBuf, String), Svc>,
    /// The notes of the repair already said, so a restart replaying it says each once.
    noted: HashSet<String>,
}

struct Req {
    roots: Roots,
    wants: Vec<Want>,
    done: SyncSender<()>,
}

/// A rename's folder move, queued while the core lock is still held (so the queue is the commit order) and run here.
struct MoveJob {
    mv: LifeMove,
    report: SyncSender<MoveReport>,
}

/// What the mover thread is asked, in the order it was asked.
#[allow(
    clippy::large_enum_variant,
    reason = "one message at a time on a channel; boxing it buys nothing"
)]
enum MoveMsg {
    Move(MoveJob),
    Repair(Vec<LifeMove>),
}

#[cfg(test)]
thread_local! {
    static JOB: std::cell::Cell<&'static str> = const { std::cell::Cell::new("") };
}

/// Which of the cache's threads this is ("refresh" or "move"): a test's probe blocks one of them.
#[cfg(test)]
pub(crate) fn current_job() -> &'static str {
    JOB.with(std::cell::Cell::get)
}

#[cfg(test)]
fn set_job(name: &'static str) {
    JOB.with(|j| j.set(name));
}

type Log = Arc<dyn Fn(&str) + Send + Sync>;

/// What a caller holds after it queued its rename's move: it waits for the report once the lock is let go.
pub(crate) struct MoveWait {
    rx: Receiver<MoveReport>,
    mv: LifeMove,
    /// The mover thread is gone (it never started, or ended): nothing moves the folders until the next start.
    gone: bool,
}

impl MoveWait {
    #[cfg(test)]
    pub(crate) fn gone_for_test(rx: Receiver<MoveReport>, mv: LifeMove) -> MoveWait {
        MoveWait { rx, mv, gone: true }
    }

    /// The rename's answer with the move's report in it; a move the disk has not finished by `bound` is said so (the
    /// rename itself stands, and the move will still run).
    pub(crate) fn finish(self, said: Value, bound: Duration) -> Value {
        if self.gone {
            return crate::authority::call::finish_unmoved(said, &self.mv);
        }
        let report = self.rx.recv_timeout(bound).ok();
        crate::authority::call::finish_move(said, &self.mv, report.as_ref())
    }
}

/// A hook the refresher runs before every disk read it makes: the outage tests block it.
pub(crate) type Probe = Arc<dyn Fn() + Send + Sync>;

pub(crate) struct PackageCache {
    state: Arc<Mutex<State>>,
    tx: Sender<Req>,
    mover: Sender<MoveMsg>,
    queued: Arc<AtomicUsize>,
    wait: Duration,
    log: Arc<Mutex<Option<Log>>>,
}

impl std::fmt::Debug for PackageCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PackageCache")
    }
}

impl PackageCache {
    /// Starts the refresher thread (`kith-core-packages`). It ends when the cache is dropped.
    pub(crate) fn start(probe: Option<Probe>) -> Arc<PackageCache> {
        Self::start_with(probe, REFRESH_WAIT)
    }

    pub(crate) fn start_with(probe: Option<Probe>, wait: Duration) -> Arc<PackageCache> {
        let state = Arc::new(Mutex::new(State::default()));
        let queued = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = channel::<Req>();
        let (mover_tx, move_rx) = channel::<MoveMsg>();
        let log: Arc<Mutex<Option<Log>>> = Arc::new(Mutex::new(None));
        let (s, q, p) = (state.clone(), queued.clone(), probe.clone());
        let spawned = std::thread::Builder::new()
            .name("kith-core-packages".into())
            .spawn(move || {
                #[cfg(test)]
                set_job("refresh");
                for req in rx {
                    serve(&s, &req, p.as_deref());
                    q.fetch_sub(1, Ordering::SeqCst);
                    let _ = req.done.send(());
                }
            });
        // The folder moves have a thread of their own, in order: a refresh asked while a move waits on the disk is still
        // answered (a chain of renames needs the second one's facts while the first one's folders have not moved yet).
        let (s, l) = (state.clone(), log.clone());
        let folders = std::thread::Builder::new()
            .name("kith-core-folders".into())
            .spawn(move || {
                #[cfg(test)]
                set_job("move");
                for msg in move_rx {
                    if let Some(p) = probe.as_deref() {
                        p();
                    }
                    match msg {
                        MoveMsg::Move(job) => {
                            let (report, repair) = run_move(&job.mv);
                            say(&s, &l, repair);
                            // A caller that gave up waiting (or went) is not told: the notes (a folder that did not
                            // move) go to the log, or nobody would ever read them.
                            if let Err(lost) = job.report.send(report) {
                                say(&s, &l, lost.0.notes);
                            }
                        }
                        MoveMsg::Repair(list) => {
                            for mv in list {
                                say(&s, &l, run_move(&mv).0.notes);
                            }
                        }
                    }
                }
            });
        drop(folders);
        // No thread: every ask times out and reads as unavailable, which is the safe side.
        drop(spawned);
        Arc::new(PackageCache {
            state,
            tx,
            mover: mover_tx,
            queued,
            wait,
            log,
        })
    }

    /// Where the repair's notes go (the daemon's log).
    pub(crate) fn set_log(&self, f: Log) {
        *self
            .log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(f);
    }

    /// Queues a rename's folder move. The send never blocks, and the caller holds the core lock when it sends, so the
    /// moves run in the order the renames committed (A to B, then B to C, never C under B). A job is never dropped: the
    /// caller waits for the report, and the repair finds what a lost report leaves.
    pub(crate) fn queue_move(&self, mv: LifeMove) -> MoveWait {
        let (report, rx) = sync_channel(1);
        // A thread that is gone drops the job with its sender: the wait then ends at once and says the move is owed.
        let gone = self
            .mover
            .send(MoveMsg::Move(MoveJob {
                mv: mv.clone(),
                report,
            }))
            .is_err();
        MoveWait { rx, mv, gone }
    }

    /// Queues the repair: each old id's folders go to the person's current id when it has none of its own.
    pub(crate) fn queue_repair(&self, list: Vec<LifeMove>) {
        if !list.is_empty() {
            let _ = self.mover.send(MoveMsg::Repair(list));
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Asks the refresher for `want` and waits for it, at most [`REFRESH_WAIT`]. Whether it answered in time.
    #[cfg(test)]
    pub(crate) fn refresh(&self, roots: &Roots, want: Want) -> bool {
        self.refresh_all(roots, vec![want])
    }

    /// [`Self::refresh`] for several facts in one request, and one wait.
    pub(crate) fn refresh_all(&self, roots: &Roots, wants: Vec<Want>) -> bool {
        if self.queued.fetch_add(1, Ordering::SeqCst) >= MAX_QUEUED {
            self.queued.fetch_sub(1, Ordering::SeqCst);
            return false;
        }
        let (done, answered) = sync_channel(1);
        let req = Req {
            roots: roots.clone(),
            wants,
            done,
        };
        if self.tx.send(req).is_err() {
            self.queued.fetch_sub(1, Ordering::SeqCst);
            return false;
        }
        answered.recv_timeout(self.wait).is_ok()
    }

    /// A removal's answer with the line that tells what is kept. The removal is committed and the lock let go: only
    /// this wording waits for the folders (at most [`REFRESH_WAIT`]); the line says it can't tell when they don't come.
    pub(crate) fn finish_kept(
        &self,
        said: Value,
        note: &crate::authority::revoke::KeptNote,
    ) -> Value {
        let roots = (note.life.agents.clone(), note.life.kernel.clone());
        let want = Want::Life(note.life.state.clone(), note.pid.clone());
        // Outside a call nothing is "fresh" (see `fresh`): a fact read after this was asked for is the one to use.
        let asked = Instant::now();
        self.refresh_all(&roots, vec![want]);
        let key = (
            note.life.agents.clone(),
            note.life.state.clone(),
            note.pid.clone(),
        );
        let folders = self
            .lock()
            .lives
            .get(&key)
            .filter(|(at, _)| *at >= asked)
            .map(|(_, f)| (**f).clone());
        crate::authority::call::finish_kept(said, folders)
    }
}

impl PackageSource for PackageCache {
    fn facts(&self, agents: &Path, kernel: &Path, agent: &str) -> Option<Arc<AgentFacts>> {
        let key = (agents.to_path_buf(), kernel.to_path_buf(), agent.to_owned());
        let found = self
            .lock()
            .agents
            .get(&key)
            .filter(|e| fresh(e.validated))
            .map(|e| e.facts.clone());
        if found.is_none() {
            miss((key.0, key.1), Want::Agents(vec![agent.to_owned()]));
        }
        found
    }

    fn names(&self, agents: &Path, kernel: &Path) -> Option<Arc<Vec<String>>> {
        let roots = (agents.to_path_buf(), kernel.to_path_buf());
        let found = self
            .lock()
            .names
            .get(&roots)
            .filter(|(at, _)| fresh(*at))
            .map(|(_, n)| n.clone());
        if found.is_none() {
            miss(roots, Want::All);
        }
        found
    }

    fn life(
        &self,
        agents: &Path,
        kernel: &Path,
        state: &Path,
        pid: &str,
    ) -> Option<Arc<Vec<(String, PathBuf)>>> {
        let found = self.life_soft(agents, kernel, state, pid);
        if found.is_none() {
            miss(
                (agents.to_path_buf(), kernel.to_path_buf()),
                Want::Life(state.to_path_buf(), pid.to_owned()),
            );
        }
        found
    }

    fn life_soft(
        &self,
        agents: &Path,
        _kernel: &Path,
        state: &Path,
        pid: &str,
    ) -> Option<Arc<Vec<(String, PathBuf)>>> {
        let key = (agents.to_path_buf(), state.to_path_buf(), pid.to_owned());
        self.lock()
            .lives
            .get(&key)
            .filter(|(at, _)| fresh(*at))
            .map(|(_, f)| f.clone())
    }

    fn kernel_service(&self, agents: &Path, kernel: &Path, svc: &str) -> Option<Arc<Found>> {
        let found = self
            .lock()
            .services
            .get(&(kernel.to_path_buf(), svc.to_owned()))
            .filter(|e| fresh(e.validated))
            .map(|e| e.found.clone());
        if found.is_none() {
            miss(
                (agents.to_path_buf(), kernel.to_path_buf()),
                Want::Service(svc.to_owned()),
            );
        }
        found
    }
}

/// Whether a validation at `at` is good for the call on this thread: made since it began (less [`FRESH`]). A thread with
/// no call in progress (the daemon's own loops) has no validation to go by: nothing is fresh for it.
fn fresh(at: Instant) -> bool {
    CALL.with(|c| match &*c.borrow() {
        Some(call) => at + FRESH >= call.start,
        // The daemon's own loops have no call to have been checked for: nothing was validated for them, so a fact is a miss
        // (I4: facts are checked during the call that uses them).
        None => false,
    })
}

fn miss(roots: Roots, want: Want) {
    CALL.with(|c| {
        if let Some(call) = c.borrow_mut().as_mut() {
            call.missed += 1;
            if call.raise {
                call.misses.push(Miss { roots, want });
            }
        }
    });
}

// --- the refresher ----------------------------------------------------------------------------------------------------

/// Says each of the repair's notes once per process.
fn say(state: &Mutex<State>, log: &Mutex<Option<Log>>, notes: Vec<String>) {
    let log = log
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    for n in notes {
        let first = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .noted
            .insert(n.clone());
        if first && let Some(f) = &log {
            f(&n);
        }
    }
}

fn serve(state: &Mutex<State>, req: &Req, probe: Option<&(dyn Fn() + Send + Sync)>) {
    for want in &req.wants {
        serve_one(state, &req.roots, want, probe);
    }
}

fn serve_one(
    state: &Mutex<State>,
    roots: &Roots,
    want: &Want,
    probe: Option<&(dyn Fn() + Send + Sync)>,
) {
    let (agents, kernel) = (&roots.0, &roots.1);
    match want {
        Want::Life(st, pid) => {
            if let Some(p) = probe {
                p();
            }
            let folders = Arc::new(person_folders(agents, st, pid));
            let mut g = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if g.lives.len() >= MAX_AGENTS {
                g.lives.clear();
            }
            g.lives.insert(
                (agents.clone(), st.clone(), pid.clone()),
                (Instant::now(), folders),
            );
        }
        Want::Service(svc) => validate_service(state, kernel, svc, probe),
        Want::Agents(list) => {
            for a in list {
                validate(state, agents, kernel, a, probe);
            }
        }
        Want::All => {
            if let Some(p) = probe {
                p();
            }
            let names = read_names(agents, kernel);
            let at = Instant::now();
            state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .names
                .insert(roots.clone(), (at, Arc::new(names.clone())));
            // The agents the cache holds that are no longer listed are looked at too, so a deleted one reads as gone.
            let mut all = names;
            all.extend(
                state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .agents
                    .keys()
                    .filter(|k| k.0 == *agents && k.1 == *kernel)
                    .map(|k| k.2.clone()),
            );
            all.sort();
            all.dedup();
            for a in &all {
                validate(state, agents, kernel, a, probe);
            }
        }
    }
}

/// Stats what the agent's facts came from and reads them again only if one changed (or the entry is new).
fn validate(
    state: &Mutex<State>,
    agents: &Path,
    kernel: &Path,
    agent: &str,
    probe: Option<&(dyn Fn() + Send + Sync)>,
) {
    let key = (agents.to_path_buf(), kernel.to_path_buf(), agent.to_owned());
    let lock = || {
        state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    // Cloned out: the state lock is never held across a disk read, because the core lock's callers take it.
    let known = lock()
        .agents
        .get(&key)
        .map(|e| (e.watched.clone(), e.stamps.clone(), e.unsettled));
    if let Some(p) = probe {
        p();
    }
    if let Some((watched, stamps, false)) = known {
        let now: Vec<Option<Stamp>> = watched.iter().map(|w| stamp(w)).collect();
        if now == stamps {
            if let Some(e) = lock().agents.get_mut(&key) {
                e.validated = Instant::now();
            }
            return;
        }
    }
    let began = SystemTime::now();
    let facts = AgentFacts::read(agents, kernel, agent);
    let watched = facts.watched(agents, kernel, agent);
    let stamps: Vec<Option<Stamp>> = watched.iter().map(|w| stamp(w)).collect();
    let began_ns = began
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| i128::try_from(d.as_nanos()).unwrap_or(0));
    let unsettled = stamps
        .iter()
        .flatten()
        .any(|s| s.mtime >= began_ns || s.ctime >= began_ns);
    let mut st = lock();
    if st.agents.len() >= MAX_AGENTS {
        st.agents.clear();
    }
    st.agents.insert(
        key,
        Entry {
            facts: Arc::new(facts),
            watched,
            stamps,
            validated: Instant::now(),
            unsettled,
        },
    );
}

/// Stats a kernel service spec and reads it again only if it changed (or the entry is new).
fn validate_service(
    state: &Mutex<State>,
    kernel: &Path,
    svc: &str,
    probe: Option<&(dyn Fn() + Send + Sync)>,
) {
    let key = (kernel.to_path_buf(), svc.to_owned());
    let lock = || {
        state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    let path = kernel.join("services").join(format!("{svc}.json"));
    let known = lock().services.get(&key).map(|e| (e.stamp, e.unsettled));
    if let Some(p) = probe {
        p();
    }
    if let Some((was, false)) = known
        && stamp(&path) == was
    {
        if let Some(e) = lock().services.get_mut(&key) {
            e.validated = Instant::now();
        }
        return;
    }
    let began = SystemTime::now();
    let found = read_service(kernel, svc);
    let now = stamp(&path);
    let began_ns = began
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| i128::try_from(d.as_nanos()).unwrap_or(0));
    let unsettled = now.is_some_and(|s| s.mtime >= began_ns || s.ctime >= began_ns);
    let mut st = lock();
    if st.services.len() >= MAX_AGENTS {
        st.services.clear();
    }
    st.services.insert(
        key,
        Svc {
            found: Arc::new(found),
            stamp: now,
            validated: Instant::now(),
            unsettled,
        },
    );
}

// --- one call -----------------------------------------------------------------------------------------------------------

struct CallState {
    start: Instant,
    /// Whether a miss is recorded (and the transaction told): not on the last pass.
    raise: bool,
    misses: Vec<Miss>,
    /// Every miss of the pass, raised or not: a pass that cannot raise (the last round, or a refresher that did not answer)
    /// still must not take an "unavailable" fact for a definite no (see [`miss_count`]).
    missed: usize,
}

thread_local! {
    static CALL: RefCell<Option<CallState>> = const { RefCell::new(None) };
}

struct CallGuard;

fn begin(start: Instant, raise: bool) -> CallGuard {
    CALL.with(|c| {
        *c.borrow_mut() = Some(CallState {
            start,
            raise,
            misses: Vec::new(),
            missed: 0,
        });
    });
    CallGuard
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        CALL.with(|c| *c.borrow_mut() = None);
    }
}

/// Whether this pass met a fact the cache could not give: the authority transaction rolls back and says so.
pub(crate) fn pass_missed() -> bool {
    CALL.with(|c| c.borrow().as_ref().is_some_and(|c| !c.misses.is_empty()))
}

/// How many facts this pass has found missing so far, raised or not. A rule that decides *no* from a fact and has no
/// agent in its params to refresh first (`doors.sweep`, `schedule.tick`) compares it before and after: a change means the
/// answer rested on an unavailable fact. Rolling back is [`pass_missed`]'s (the pass can raise); on the last round the
/// caller leaves the thing undecided instead.
pub(crate) fn miss_count() -> usize {
    CALL.with(|c| c.borrow().as_ref().map_or(0, |c| c.missed))
}

/// The error a transaction that [`pass_missed`] ends with.
pub(crate) fn need_packages() -> Error {
    Error::NeedPackages("kith-core: package facts are being read".into())
}

fn take_misses() -> Vec<Miss> {
    CALL.with(|c| {
        c.borrow_mut()
            .as_mut()
            .map(|c| std::mem::take(&mut c.misses))
            .unwrap_or_default()
    })
}

/// The agents a call's params name: what the first refresh is for.
fn named(params: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |s: &str| {
        if !s.is_empty() && !out.iter().any(|o| o == s) {
            out.push(s.to_owned());
        }
    };
    for k in ["agent", "via", "helper", "pkg"] {
        if let Some(s) = params.get(k).and_then(Value::as_str) {
            add(s);
        }
    }
    for a in params
        .get("agents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(s) = a.as_str() {
            // a name may come as `agent@person` (see `secret_writes`)
            add(s.split('@').next().unwrap_or(s));
        }
    }
    out
}

/// The person ids a method looks the folders of up, which the first refresh asks for. A narrowing (suspend, undo,
/// removeBot) is not here, and neither is a removal: its line is worded after the commit ([`PackageCache::finish_kept`]).
fn life_ids<'a>(method: &str, params: &'a Value) -> Vec<&'a str> {
    let keys: &[&str] = match method {
        "people.addPerson" | "people.retired" => &["id"],
        "people.renamePerson" => &["to"],
        _ => &[],
    };
    keys.iter()
        .filter_map(|k| params.get(*k).and_then(Value::as_str))
        .collect()
}

/// The kernel service a secret writer's params name (`service`, or the front of `profile`).
fn service_names<'a>(method: &str, params: &'a Value) -> Vec<&'a str> {
    if !method.starts_with("people.secret") {
        return Vec::new();
    }
    let mut out = Vec::new();
    if let Some(s) = params.get("service").and_then(Value::as_str) {
        out.push(s);
    }
    if let Some(p) = params.get("profile").and_then(Value::as_str) {
        out.push(p.split_once('/').map_or(p, |(s, _)| s));
    }
    if method == "people.secretBrowser" {
        out.push("browser");
    }
    out.retain(|s| !s.is_empty() && !s.contains(['/', '\\']) && !s.starts_with('.'));
    out
}

/// What the first refresh of a call asks for: the agents, the folders and the service specs its params name. The rest
/// (a spec that only a row of the database names) is missed, rolled back and asked for on the next round.
pub(crate) fn first_wants(method: &str, params: &Value, state: Option<&Path>) -> Vec<Want> {
    let mut out = Vec::new();
    let list = named(params);
    if !list.is_empty() {
        out.push(Want::Agents(list));
    }
    if let Some(state) = state {
        for id in life_ids(method, params) {
            out.push(Want::Life(state.to_path_buf(), id.to_owned()));
        }
    }
    for svc in service_names(method, params) {
        out.push(Want::Service(svc.to_owned()));
    }
    out.dedup();
    out
}

/// Runs one call that reads package facts: `pass` takes the core lock and decides. See the module doc.
pub(crate) fn run_call<R>(
    cache: &PackageCache,
    own: &Paths,
    method: &str,
    params: &Value,
    pass: impl FnMut() -> Result<R>,
) -> Result<R> {
    let (roots, state) = match crate::call::folders_in(Some(own), params) {
        Ok((folders, kernel)) => (Some((folders.agents, kernel)), Some(folders.state)),
        Err(_) => (None, None),
    };
    run_call_in(
        cache,
        roots.as_ref(),
        first_wants(method, params, state.as_deref()),
        pass,
    )
}

/// [`run_call`] for the folders of the call given (the authority tests have a ctx and no params to find them in), and the
/// facts the first refresh asks for.
pub(crate) fn run_call_in<R>(
    cache: &PackageCache,
    roots: Option<&Roots>,
    wants: Vec<Want>,
    mut pass: impl FnMut() -> Result<R>,
) -> Result<R> {
    let start = Instant::now();
    // A refresher that does not answer in time is not asked again by this call: what it was asked for reads as unavailable.
    let mut stuck = false;
    if let Some(roots) = roots
        && !wants.is_empty()
    {
        stuck = !cache.refresh_all(roots, wants);
    }
    for round in 0..=MISS_ROUNDS {
        let guard = begin(start, round < MISS_ROUNDS && !stuck);
        let out = pass();
        let misses = take_misses();
        drop(guard);
        match out {
            Err(Error::NeedPackages(_)) if round < MISS_ROUNDS => {
                for (roots, wants) in merge(misses) {
                    stuck |= !cache.refresh_all(&roots, wants);
                }
            }
            // After the last round a miss was never raised; this only keeps the loop total.
            other => return other,
        }
    }
    Err(need_packages())
}

/// One request per folder pair: the names of all agents if any was missed, else the agents; and each other fact once.
fn merge(misses: Vec<Miss>) -> Vec<(Roots, Vec<Want>)> {
    let mut out: Vec<(Roots, Vec<Want>)> = Vec::new();
    for m in misses {
        let group = if let Some(i) = out.iter().position(|(r, _)| *r == m.roots) {
            i
        } else {
            out.push((m.roots.clone(), Vec::new()));
            out.len() - 1
        };
        let wants = &mut out[group].1;
        match m.want {
            Want::All => {
                wants.retain(|w| !matches!(w, Want::Agents(_)));
                if !wants.contains(&Want::All) {
                    wants.push(Want::All);
                }
            }
            Want::Agents(more) => {
                if wants.contains(&Want::All) {
                    continue;
                }
                match wants.iter_mut().find_map(|w| match w {
                    Want::Agents(have) => Some(have),
                    _ => None,
                }) {
                    Some(have) => {
                        for a in more {
                            if !have.contains(&a) {
                                have.push(a);
                            }
                        }
                    }
                    None => wants.push(Want::Agents(more)),
                }
            }
            other => {
                if !wants.contains(&other) {
                    wants.push(other);
                }
            }
        }
    }
    out
}

/// Makes every read of a package folder on a thread that holds the core lock (as far as `daemon::mark_lock_held` says) a
/// panic: the structural test of the rule that the lock never waits on a disk. Once per process.
#[cfg(test)]
pub(crate) fn forbid_disk_under_lock() {
    kith_kernel::secrets::on_disk_read(|| {
        assert!(
            !crate::daemon::lock_held(),
            "a package folder was read under the core lock"
        );
    });
}

#[cfg(test)]
#[path = "packages_tests.rs"]
mod tests;
