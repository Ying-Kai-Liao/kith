//! The daemon's package facts (see `kith_kernel::secrets::Packages`): what the access rules ask of an agent's package
//! folder, answered from memory under the core lock and filled by ONE thread outside it. A folder on a disk that does not
//! answer (an external drive behind a prompt nobody sees) then stops that thread and nothing else: the calls that need
//! the fact give the narrower answer after [`REFRESH_WAIT`], and the calls that don't need it never notice.
//!
//! The call path (`Shared::dispatch`, via [`run_call`]): refresh the agents the params name, then run the call's pass
//! under the core lock. A fact the cache was not asked to validate for this call is a *miss*: the rules read it as
//! unavailable, the authority transaction notices the miss (`authority::store`) and rolls back with `NeedPackages`, the
//! lock is let go, the refresher is asked for what was missed, and the call runs again from scratch on a new snapshot.
//! At most two such rounds; a last pass answers misses as unavailable without raising. Nothing from an earlier pass
//! carries over but the cache's contents.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, SyncSender, channel, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use kith_kernel::paths::Paths;
use kith_kernel::secrets::{AgentFacts, PackageSource, read_names};
use serde_json::Value;

use crate::error::{Error, Result};

/// How long a call waits for the refresher before it takes the narrower answer.
pub(crate) const REFRESH_WAIT: Duration = Duration::from_millis(1500);
/// A fact validated this long before the call began still counts as fresh for it. Zero: the cost is one stat per named
/// agent per call, and nothing is read from memory that the call did not see checked.
const FRESH: Duration = Duration::ZERO;
/// Refresh rounds a call may spend on misses after its first pass.
const MISS_ROUNDS: usize = 2;
/// Requests waiting for a refresher that is stuck are dropped past this many.
const MAX_QUEUED: usize = 256;
/// Agents kept; a call may name any string, so the map is emptied (and refilled by what is asked next) past this.
const MAX_AGENTS: usize = 1024;

type Roots = (PathBuf, PathBuf);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Want {
    /// These agents.
    Agents(Vec<String>),
    /// The names of all agents, and each of them.
    All,
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
        Some(Stamp {
            dev: 0,
            ino: 0,
            size: m.len(),
            mtime: t,
            ctime: t,
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

#[derive(Default)]
struct State {
    agents: HashMap<(PathBuf, PathBuf, String), Entry>,
    names: HashMap<Roots, (Instant, Arc<Vec<String>>)>,
}

struct Req {
    roots: Roots,
    want: Want,
    done: SyncSender<()>,
}

/// A hook the refresher runs before every disk read it makes: the outage tests block it.
pub(crate) type Probe = Arc<dyn Fn() + Send + Sync>;

pub(crate) struct PackageCache {
    state: Arc<Mutex<State>>,
    tx: Sender<Req>,
    queued: Arc<AtomicUsize>,
    wait: Duration,
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
        let (s, q) = (state.clone(), queued.clone());
        let spawned = std::thread::Builder::new()
            .name("kith-core-packages".into())
            .spawn(move || {
                for req in rx {
                    serve(&s, &req, probe.as_deref());
                    q.fetch_sub(1, Ordering::SeqCst);
                    let _ = req.done.send(());
                }
            });
        // No thread: every ask times out and reads as unavailable, which is the safe side.
        drop(spawned);
        Arc::new(PackageCache {
            state,
            tx,
            queued,
            wait,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Asks the refresher for `want` and waits for it, at most [`REFRESH_WAIT`]. Whether it answered in time.
    fn refresh(&self, roots: &Roots, want: Want) -> bool {
        if self.queued.fetch_add(1, Ordering::SeqCst) >= MAX_QUEUED {
            self.queued.fetch_sub(1, Ordering::SeqCst);
            return false;
        }
        let (done, answered) = sync_channel(1);
        let req = Req {
            roots: roots.clone(),
            want,
            done,
        };
        if self.tx.send(req).is_err() {
            self.queued.fetch_sub(1, Ordering::SeqCst);
            return false;
        }
        answered.recv_timeout(self.wait).is_ok()
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
}

/// Whether a validation at `at` is good for the call on this thread: made since it began (less [`FRESH`]). A thread with
/// no call in progress (the daemon's own loops) takes what the cache has.
fn fresh(at: Instant) -> bool {
    CALL.with(|c| match &*c.borrow() {
        Some(call) => at + FRESH >= call.start,
        None => true,
    })
}

fn miss(roots: Roots, want: Want) {
    CALL.with(|c| {
        if let Some(call) = c.borrow_mut().as_mut()
            && call.raise
        {
            call.misses.push(Miss { roots, want });
        }
    });
}

// --- the refresher ----------------------------------------------------------------------------------------------------

fn serve(state: &Mutex<State>, req: &Req, probe: Option<&(dyn Fn() + Send + Sync)>) {
    let (agents, kernel) = (&req.roots.0, &req.roots.1);
    match &req.want {
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
                .insert(req.roots.clone(), (at, Arc::new(names.clone())));
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

// --- one call -----------------------------------------------------------------------------------------------------------

struct CallState {
    start: Instant,
    /// Whether a miss is recorded (and the transaction told): not on the last pass.
    raise: bool,
    misses: Vec<Miss>,
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

/// Runs one call that reads package facts: `pass` takes the core lock and decides. See the module doc.
pub(crate) fn run_call<R>(
    cache: &PackageCache,
    own: &Paths,
    params: &Value,
    pass: impl FnMut() -> Result<R>,
) -> Result<R> {
    let roots = crate::call::folders_in(Some(own), params)
        .ok()
        .map(|(folders, kernel)| (folders.agents, kernel));
    run_call_in(cache, roots.as_ref(), params, pass)
}

/// [`run_call`] for the folders of the call given (the authority tests have a ctx and no params to find them in).
pub(crate) fn run_call_in<R>(
    cache: &PackageCache,
    roots: Option<&Roots>,
    params: &Value,
    mut pass: impl FnMut() -> Result<R>,
) -> Result<R> {
    let start = Instant::now();
    // A refresher that does not answer in time is not asked again by this call: what it was asked for reads as unavailable.
    let mut stuck = false;
    if let Some(roots) = roots {
        let list = named(params);
        if !list.is_empty() {
            stuck = !cache.refresh(roots, Want::Agents(list));
        }
    }
    for round in 0..=MISS_ROUNDS {
        let guard = begin(start, round < MISS_ROUNDS && !stuck);
        let out = pass();
        let misses = take_misses();
        drop(guard);
        match out {
            Err(Error::NeedPackages(_)) if round < MISS_ROUNDS => {
                for m in merge(misses) {
                    stuck |= !cache.refresh(&m.roots, m.want);
                }
            }
            // After the last round a miss was never raised; this only keeps the loop total.
            other => return other,
        }
    }
    Err(need_packages())
}

/// One request per folder pair: the names of all agents if any was missed, else the agents.
fn merge(misses: Vec<Miss>) -> Vec<Miss> {
    let mut out: Vec<Miss> = Vec::new();
    for m in misses {
        match out.iter_mut().find(|o| o.roots == m.roots) {
            None => out.push(m),
            Some(o) => match (&mut o.want, m.want) {
                (Want::All, _) => {}
                (w, Want::All) => *w = Want::All,
                (Want::Agents(have), Want::Agents(more)) => {
                    for a in more {
                        if !have.contains(&a) {
                            have.push(a);
                        }
                    }
                }
            },
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
