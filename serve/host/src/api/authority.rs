//! The authority facts the event filter needs (M2-15): one seam between `roles::event_for` and whoever knows who is
//! granted which agent. Chat facts (bots, rooms, entries, cards) stay in the host store; only "may person P use agent
//! A right now" goes through [`Authority`].
//!
//! Two answers, one of them chosen per event: [`HostData`] (today's, the people mirror in the host store) and
//! [`CoreData`] (kith-core's authority, over the owner socket: `people.may`). Nothing is switched: the core answers
//! only when `KITH_CORE_AUTHORITY=1` is set. The core's answer fails CLOSED: a core that is unreachable, a refusal, or
//! an empty core database (people rows written before the import, M2-8) all say "not granted", so an invitee sees nothing
//! until the core knows them. The owner (`who` None) never asks, so owner events are unchanged.
//!
//! A live event asks afresh: a revoke must stop the next event. A catch-up batch asks once per (person, agent). The core's
//! answer costs one owner-socket round trip per live event that reaches an invitee's own chat; the stream runs it off
//! the async threads and drops the event (fails closed) if the core takes longer than a few seconds.
//!
//! M2-16 puts the request guard on the same switch. `roles::resolve` (so `may_use`, `may_use_checked`, `granted_agents`
//! and `access::member`) asks [`core_verdict`] instead of kith-auth's `resolve` when `KITH_CORE_AUTHORITY=1`. Moved: is
//! person P granted agent A (`guard`'s `own_bot`, the `botguests` ask/thread/room/new-bot gates and `enforce`, the sweep
//! of claims), is a bot guest's owner still allowed (the core's `may` for a bot also needs its owner's access), is P
//! revoked or suspended (the core says no), does P hold the member preset (who may answer a card derived from grants:
//! `access::derive`, and the live check of `respondPermission` in `guard`), and the preset shown in `granted_agents`.
//! The ask is bounded: at most [`MAX_ASKS`] blocking threads in flight (the thread keeps its permit until the ask
//! returns, so a core that hangs cannot grow the pool), a request waits no longer than [`ASK_WAIT`] for its turn and its
//! answer, and then fails closed. The owner never asks: the guard is for invited people only, and a core answer of
//! "owner" is a no here. With an empty core database (before the import, M2-8) every member is refused and `rederive` reads
//! every open card of theirs as "no longer allowed": import first, then set the switch.
//!
//! Still on host data, not behind this seam: who a card names to answer it (`answerable_by` in `route`, stored when the
//! card opens and kept fresh by `access::rederive`, which itself asks the moved fact), the bot-guest-owner arm of
//! `route` and `owned_bot_chat_agent` (`store.bot_guest_of_person`, `owner_person`: chat/bot data, not a grant), the bot
//! guest's state in `event_for_bot` and `bot_call` (`BotState`, set by `enforce`/`end_bot` from the moved fact), the
//! person's shared projects (`store.person(..).projects`: the host's, not an access grant), and the member / bot /
//! unproven method lists (the contract).
//!
//! M2-17 puts the owner's side on the same switch: `api/people.rs setPersonAccess` writes kith-auth first (auth.json is
//! the truth until the switch, M2-18a) and then mirrors `people.grant` / `people.revoke` ([`owner_call`]), the listing of people reads the membership rows from `people.list`
//! (`members::auth_people_now`, also what the 10-second mirror reads), and `api/remote.rs accessRequests` /
//! `decideAccessRequest` use `people.requests|approve|deny` (`cloud::decide_in_core`, which stores exactly the device the
//! core answered). The host files each request it holds with `people.fileRequest` on every pull. The reads and the request
//! decisions fail CLOSED: a core that can't be reached is an error to the app ([`CoreDown`]), never an answer from the
//! host's copy. The owner's writes do not need the core: a mirror that fails is logged, and only closes the gate.

use super::roles;
use crate::store::Store;
use kith_kernel::paths::Paths;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The opt-in: set to `1` and the event filter asks the core's authority.
pub const SWITCH: &str = "KITH_CORE_AUTHORITY";

pub trait Authority {
    /// Whether `person` may use the hq agent `agent` now (a person who is unknown, revoked or suspended may not).
    fn granted(&self, person: &str, agent: &str) -> bool;
}

/// Today's answer: the host's people mirror.
pub struct HostData<'a>(pub &'a Store);

impl Authority for HostData<'_> {
    fn granted(&self, person: &str, agent: &str) -> bool {
        self.0.person(person).is_some_and(|p| p.agents.contains_key(agent))
    }
}

/// The core's answer, from `people.may` (the kernel's `may`: a revoke beats every row, then the person's own row, then
/// the `*` row). With a memo it asks once per (person, agent) and remembers, for one catch-up batch only.
pub struct CoreData {
    pub folders: Paths,
    pub home: PathBuf,
    pub kernel: PathBuf,
    /// The variables `paths` reads, as the caller has them (where the agent folders are).
    pub env: serde_json::Map<String, Value>,
    memo: Option<Mutex<HashMap<(String, String), bool>>>,
}

impl CoreData {
    pub fn new(folders: Paths, home: PathBuf, kernel: PathBuf, env: serde_json::Map<String, Value>) -> Self {
        Self { folders, home, kernel, env, memo: None }
    }

    /// This process's view, as `kith people may` sends it.
    pub fn from_process() -> Self {
        // `known_home`, not `process_home`: with HOME unset the account's own home still answers.
        let home = kith_kernel::paths::known_home().unwrap_or_default();
        let folders = crate::kernel::folders(&home);
        let kernel = crate::kernel::current_kernel().unwrap_or_else(|| home.join("kith/kernel"));
        Self::new(folders, home, kernel, crate::core_front::caller_env())
    }

    /// The same view with a memo: for the catch-up of one stream, where a hundred entries of one chat are one question.
    pub fn with_memo(&self) -> Self {
        Self {
            folders: self.folders.clone(),
            home: self.home.clone(),
            kernel: self.kernel.clone(),
            env: self.env.clone(),
            memo: Some(Mutex::new(HashMap::new())),
        }
    }

    /// The caller's view params every `people.*` call carries (`home`, `kernel`, `env`).
    fn view(&self) -> serde_json::Map<String, Value> {
        let mut m = serde_json::Map::new();
        // An empty home is left out: the core then uses its own known home, or refuses, instead of being told "".
        if !self.home.as_os_str().is_empty() {
            m.insert("home".into(), self.home.to_string_lossy().into_owned().into());
        }
        m.insert("kernel".into(), self.kernel.to_string_lossy().into_owned().into());
        m.insert("env".into(), Value::Object(self.env.clone()));
        m
    }

    /// The core's answer for one pair; None when it could not be asked (no daemon, a refusal, a lost connection).
    /// Blocking: the async callers go through [`core_verdict`].
    pub fn verdict(&self, person: &str, agent: &str) -> Option<Verdict> {
        // Nobody to ask about: the owner's own events and calls carry no person.
        if person.is_empty() {
            return None;
        }
        let (home, kernel) = (self.home.to_str()?, self.kernel.to_str()?);
        let params = json!({"person": person, "agent": agent, "home": home, "kernel": kernel, "env": self.env});
        let a = crate::core_front::ask(&self.folders, "people.may", &params)?;
        Some(Verdict {
            allowed: a["allowed"] == true,
            owner: a["owner"] == true || a["why"] == "owner",
            preset: a["preset"].as_str().map(str::to_owned),
        })
    }

    fn ask(&self, person: &str, agent: &str) -> bool {
        // The owner is never a guest of this seam: the core can not grant more than the mirror does.
        self.verdict(person, agent).is_some_and(|v| v.allowed && !v.owner)
    }
}

/// What the core says of one (person, agent): `people.may`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub allowed: bool,
    /// The person is the computer's owner (who never comes through the guard).
    pub owner: bool,
    /// The preset their membership carries, when they hold the agent.
    pub preset: Option<String>,
}

/// Blocking threads asking the core for the request guard at once. The thread owns its permit until the ask returns, so
/// a core that hangs holds at most this many threads however many requests time out.
pub const MAX_ASKS: usize = 4;

/// The longest a request waits for its turn and the core's answer before it is refused.
pub const ASK_WAIT: Duration = Duration::from_secs(3);

static ASKS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();

/// The permits of [`MAX_ASKS`] (a test takes them all to stand for a core that hangs).
pub(super) fn asks() -> Arc<tokio::sync::Semaphore> {
    ASKS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(MAX_ASKS))).clone()
}

/// The process's view for the guard, built once (`from_process` probes the filesystem).
fn process_core() -> Arc<CoreData> {
    static CORE: OnceLock<Arc<CoreData>> = OnceLock::new();
    #[cfg(test)]
    if let Some(c) = TEST_CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone() {
        return c;
    }
    CORE.get_or_init(|| Arc::new(CoreData::from_process())).clone()
}

/// A test's core (a daemon on a folder of its own) in place of the process's, and the guard's switch on while it is
/// set. Tests that set it hold `account::ONE_AT_A_TIME`.
#[cfg(test)]
pub static TEST_CORE: Mutex<Option<Arc<CoreData>>> = Mutex::new(None);

/// What the core says of `person` and `agent` for the request guard (`KITH_CORE_AUTHORITY=1`). None: it could not be
/// asked in time (a caller reads that as "no", or "try again" where it can). Off the async threads, [`MAX_ASKS`] at a
/// time, [`ASK_WAIT`] at most.
pub async fn core_verdict(person: &str, agent: &str) -> Option<Verdict> {
    if person.is_empty() {
        return None;
    }
    let core = process_core();
    let (person, agent) = (person.to_owned(), agent.to_owned());
    let sem = asks();
    let run = async move {
        let permit = sem.acquire_owned().await.ok()?;
        tokio::task::spawn_blocking(move || {
            let _held = permit;
            core.verdict(&person, &agent)
        })
        .await
        .ok()
        .flatten()
    };
    tokio::time::timeout(ASK_WAIT, run).await.ok().flatten()
}

impl Authority for CoreData {
    fn granted(&self, person: &str, agent: &str) -> bool {
        let Some(memo) = &self.memo else { return self.ask(person, agent) };
        let key = (person.to_owned(), agent.to_owned());
        if let Some(known) = memo.lock().ok().and_then(|m| m.get(&key).copied()) {
            return known;
        }
        let answer = self.ask(person, agent);
        if let Ok(mut m) = memo.lock() {
            m.insert(key, answer);
        }
        answer
    }
}

/// The answer chosen for one stream (or one call): built once, from the opt-in.
pub enum Selected {
    Host,
    Core(Arc<CoreData>),
}

impl Selected {
    #[cfg(test)]
    pub fn from_env() -> Self {
        if guard_on_core() { Self::Core(Arc::new(CoreData::from_process())) } else { Self::Host }
    }

    /// The answer for this moment: a live event asks again so a gate that closes mid-stream (a file-only write) or a
    /// switch that lands moves the next event, not the next stream (M2-18 gate 10).
    pub fn now() -> Self {
        if guard_on_core() { Self::Core(process_core()) } else { Self::Host }
    }

    pub fn is_core(&self) -> bool {
        matches!(self, Self::Core(_))
    }

    /// For a catch-up batch: one core question per (person, agent) instead of one per event. A live event never uses
    /// this, so a revoke still stops the next one.
    pub fn for_catch_up(&self) -> Self {
        match self {
            Self::Host => Self::Host,
            Self::Core(c) => Self::Core(Arc::new(c.with_memo())),
        }
    }

    pub fn event(&self, store: &Store, who: Option<&str>, v: Value) -> Option<Value> {
        match self {
            Self::Host => roles::event_for_with(&HostData(store), store, who, v),
            Self::Core(c) => roles::event_for_with(c.as_ref(), store, who, v),
        }
    }
}

/// Whether the opt-in is set AND the core's gate (`cutover.mayServe` for `authority`, asked as the Host) says it may serve.
/// The variable alone is not enough (M2-18a): while the core has not imported auth.json, or auth.json changed in a way
/// the core has not, the host stays on kith-auth. Until the first answer (the daemon starts beside the host) it is off.
///
/// A SWITCHED host (auth.json holds the core's tombstone, [`switched`]) needs neither: the core is the only store, so it
/// answers, and a core that does not answer gives no access. It never falls back to the tombstone through kith-auth.
pub fn core_selected() -> bool {
    switched() || (opted_in() && gate_open(&fingerprint_of(auth_path())))
}

/// Whether authority has moved to the core: auth.json is a tombstone (`kith migrate step authority`; the way back,
/// `kith rollback authority`, puts the file back). Read from the file kith-auth reads, so the host and kith-auth agree on
/// which side is the truth; cached by the file's fingerprint, so a call costs one stat.
pub fn switched() -> bool {
    #[cfg(test)]
    {
        if TEST_CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_none() {
            return false;
        }
        let file = TEST_AUTH_FILE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        file.is_some_and(|f| switched_at(&f))
    }
    #[cfg(not(test))]
    {
        // Every live event of every stream asks: the answer is kept for one gate period, like the gate's own latch.
        static AT_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        static YES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        static START: OnceLock<Instant> = OnceLock::new();
        use std::sync::atomic::Ordering::Relaxed;
        // +1 so a stored 0 means "never asked".
        let now = u64::try_from(START.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX - 1) + 1;
        let at = AT_MS.load(Relaxed);
        if at != 0 && now.saturating_sub(at) < u64::try_from(GATE_EVERY.as_millis()).unwrap_or(2000) {
            return YES.load(Relaxed);
        }
        let yes = switched_at(auth_path());
        YES.store(yes, Relaxed);
        AT_MS.store(now, Relaxed);
        yes
    }
}

/// A test points the host at an auth.json of its own (a real tombstone file, or none) while it holds `TEST_CORE`; read
/// only then, like `TEST_GATE_SHUT`.
#[cfg(test)]
pub static TEST_AUTH_FILE: Mutex<Option<PathBuf>> = Mutex::new(None);

fn switched_at(auth: &Path) -> bool {
    static SEEN: Mutex<Option<(String, bool)>> = Mutex::new(None);
    let fingerprint = fingerprint_of(auth);
    // A file that can't be statted is read each time (cheap, and the answer differs by why).
    if fingerprint == "none" {
        return tombstoned(auth);
    }
    let mut seen = SEEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((fp, yes)) = seen.as_ref()
        && *fp == fingerprint
    {
        return *yes;
    }
    let yes = tombstoned(auth);
    *seen = Some((fingerprint, yes));
    yes
}

/// Whether the file holds a tombstone (`kith_tombstone.py`: a JSON object with `kith_tombstone`). Only a missing file is
/// "not one" (the unswitched rules stay). A file that exists but can't be opened or read (permissions, I/O, not UTF-8)
/// counts as switched: it fails closed, as kith-auth's `load_auth` refuses a file it can't read.
fn tombstoned(auth: &Path) -> bool {
    use std::io::Read as _;
    let mut text = String::new();
    let f = match std::fs::File::open(auth) {
        Ok(f) => f,
        Err(e) => return e.kind() != std::io::ErrorKind::NotFound,
    };
    if f.take(65536).read_to_string(&mut text).is_err() {
        return true;
    }
    if !text.contains("\"kith_tombstone\"") {
        return false;
    }
    serde_json::from_str::<Value>(&text).is_ok_and(|v| v.as_object().is_some_and(|o| o.contains_key("kith_tombstone")))
}

pub fn opted_in() -> bool {
    std::env::var(SWITCH).is_ok_and(|v| v == "1")
}

/// The gate's last answer, what auth.json looked like and when the ask was made.
struct Gate {
    ok: bool,
    fingerprint: String,
    /// When the ask began: the answer is at least this old. A latch past [`GATE_MAX_AGE`] is closed, so a gate thread
    /// that hangs on a hung daemon (or died) cannot leave the last "yes" standing.
    asked_at: Instant,
}

static GATE: Mutex<Option<Gate>> = Mutex::new(None);

/// How often the gate is asked again. A change of auth.json closes it at once (the fingerprint no longer matches) until
/// the next answer, so this is how long a switched-on host can stay on kith-auth after a harmless write, not how long a
/// stale core can answer.
const GATE_EVERY: Duration = Duration::from_secs(2);

/// How old the latch may be and still count: a few asks. One slow ask passes; a thread that is stuck or gone does not.
const GATE_MAX_AGE: Duration = Duration::from_secs(3 * GATE_EVERY.as_secs());

fn gate_open(now: &str) -> bool {
    gate_open_at(now, Instant::now())
}

fn gate_open_at(fingerprint: &str, at: Instant) -> bool {
    GATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().is_some_and(|g| {
        g.ok && g.fingerprint == fingerprint && at.saturating_duration_since(g.asked_at) < GATE_MAX_AGE
    })
}

fn close_gate() {
    *GATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
        Some(Gate { ok: false, fingerprint: String::new(), asked_at: Instant::now() });
}

/// Closes the gate when the gate thread ends for any reason, a panic included.
struct CloseOnExit;

impl Drop for CloseOnExit {
    fn drop(&mut self) {
        close_gate();
        if std::thread::panicking() {
            tracing::error!("the authority gate thread panicked; staying on kith-auth until the host restarts");
        }
    }
}

/// The auth.json the core's gate reads: `folders.secrets/auth.json`, which is how the core resolves it from the
/// `home`, `kernel` and `env` this process sends (`folders_for` + `ctx_from`).
fn auth_of(core: &CoreData) -> PathBuf {
    core.folders.secrets.join("auth.json")
}

/// The same path for every request, resolved once (no path work per request beyond the one stat).
fn auth_path() -> &'static Path {
    static AUTH: OnceLock<PathBuf> = OnceLock::new();
    AUTH.get_or_init(|| auth_of(&CoreData::from_process()))
}

/// auth.json's inode, size and mtime (the framework's own fingerprint of it), or `none`. `kith-auth` writes the file
/// as a temporary file and `os.replace`s it over (`save_auth`), so every write is a new inode: the stat sees it even
/// when size and mtime would repeat.
fn fingerprint_of(auth: &Path) -> String {
    match std::fs::metadata(auth) {
        Ok(md) => {
            use std::os::unix::fs::MetadataExt as _;
            format!("{}:{}:{}:{}", md.ino(), md.len(), md.mtime(), md.mtime_nsec())
        }
        Err(_) => "none".into(),
    }
}

/// What the gate's answer means for the host: whether to use the core, and the line to log when it is no (or a warning
/// with a yes). `None` is a core that did not answer.
pub fn read_gate(answer: Option<&Value>) -> (bool, Option<String>) {
    let Some(a) = answer else {
        return (false, Some("the core did not answer the authority gate; staying on kith-auth".into()));
    };
    let warning = a["warning"].as_str().map(str::to_owned);
    if a["ok"] == true {
        return (true, warning);
    }
    let msg = match warning {
        // "authority: N difference(s) between the core and the old store (...)": the import is behind the file.
        Some(w) => match w.split_once(" difference(s)").and_then(|(head, _)| head.rsplit(' ').next()?.parse::<usize>().ok()) {
            Some(n) => format!(
                "the core missed {n} changes, run `kith kernel core-migrate --authority --force`; staying on kith-auth ({w})"
            ),
            None => format!("{w}; staying on kith-auth"),
        },
        None => "authority is not switched to the core (run `kith kernel core-migrate --authority`, then `kith migrate step authority`); staying on kith-auth"
            .into(),
    };
    (false, Some(msg))
}

/// One ask of the gate: the core's `cutover.mayServe` for `authority` on the Host connection, with this process's view of
/// the folders (the core reads the caller's own auth.json, not its own).
fn ask_gate(core: &CoreData) -> (bool, Option<String>) {
    let (Some(home), Some(kernel)) = (core.home.to_str(), core.kernel.to_str()) else {
        return (false, Some("home or kernel folder is not UTF-8; staying on kith-auth".into()));
    };
    let params = json!({"module": "authority", "home": home, "kernel": kernel, "env": core.env});
    read_gate(crate::core_front::call_as_host(&core.folders, "cutover.mayServe", &params).as_ref())
}

/// `KITH_CORE_AUTHORITY=1` at host start: from now on the gate is asked every [`GATE_EVERY`] on a thread of its own and
/// the answer is kept with auth.json's fingerprint. Without the variable nothing runs and the host is on kith-auth.
pub fn start_gate() {
    // A switched host asks too (no variable needed): its latch is read only while unswitched, but keeps the log honest.
    if !opted_in() && !switched() {
        return;
    }
    let spawned = std::thread::Builder::new().name("authority-gate".into()).spawn(|| {
        let _closes = CloseOnExit;
        let core = CoreData::from_process();
        let auth = auth_of(&core);
        let mut last: Option<(bool, Option<String>)> = None;
        loop {
            // Fingerprint first: a write that lands during the ask leaves the answer older than the file, and it is dropped.
            let asked_at = Instant::now();
            let fingerprint = fingerprint_of(&auth);
            let (ok, line) = ask_gate(&core);
            *GATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Gate { ok, fingerprint, asked_at });
            if last.as_ref() != Some(&(ok, line.clone())) {
                match (ok, &line) {
                    (true, None) => tracing::info!("KITH_CORE_AUTHORITY=1: the core serves authority"),
                    (true, Some(w)) => tracing::warn!("KITH_CORE_AUTHORITY=1: the core serves authority. {w}"),
                    (false, w) => tracing::warn!(
                        "KITH_CORE_AUTHORITY=1 is not honoured: {}",
                        w.as_deref().unwrap_or("the core does not serve authority")
                    ),
                }
                last = Some((ok, line));
            }
            std::thread::sleep(GATE_EVERY);
        }
    });
    if let Err(e) = spawned {
        tracing::warn!(
            "KITH_CORE_AUTHORITY=1 is not honoured: the gate thread would not start ({e}); staying on kith-auth"
        );
    }
}

/// [`core_selected`] for the request guard: also on while a test has given it a core (`TEST_CORE`), so a test does not
/// have to set a variable the other tests of the process would see.
pub fn guard_on_core() -> bool {
    #[cfg(test)]
    if TEST_CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_some() {
        return switched() || !TEST_GATE_SHUT.load(std::sync::atomic::Ordering::SeqCst);
    }
    core_selected()
}

/// A test shuts the gate while its core stays up and opted in (what a write by kith-auth does for up to `GATE_EVERY`).
/// Only read while `TEST_CORE` is set, and every test that sets `TEST_CORE` holds the rig's `ONE_AT_A_TIME` first, so
/// no other test sees it.
#[cfg(test)]
pub static TEST_GATE_SHUT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether a write to kith-auth is mirrored to the core: the opt-in (`KITH_CORE_AUTHORITY=1`), not the gate. The first
/// write moves the fingerprint and closes the gate for up to `GATE_EVERY`, so a second change in that window would
/// skip the mirror and leave the gate closed until `core-migrate --force`. A mirror into a core that differs only
/// brings it closer to the file. Also on while a test has given the guard a core (`TEST_CORE`).
pub fn mirrors_to_core() -> bool {
    #[cfg(test)]
    if TEST_CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_some() {
        return true;
    }
    opted_in() || switched()
}

/// The core refused the call itself: the rules said no (its words, without the `authority: ` the core puts in front).
#[derive(Debug)]
pub struct CoreRefused(pub String);

impl std::fmt::Display for CoreRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CoreRefused {}

/// The core could not be asked (no daemon, a mismatch, a lost connection). A caller fails CLOSED on it: it is an error
/// to the app, and nothing is written another way.
#[derive(Debug)]
pub struct CoreDown(pub String);

impl std::fmt::Display for CoreDown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Not "nothing was changed": a `setPersonAccess` that asked for several grants may have made the earlier ones.
        write!(f, "Kith's core isn't answering ({}). Changes made before this point may stand.", self.0)
    }
}

impl std::error::Error for CoreDown {}

/// An owner's call to the core over the owner socket (M2-17: `setPersonAccess`, the access requests), off the async
/// threads. `repeatable` is for reads: a write is never sent twice, and a connection lost after it went out is an error
/// that says it may have run. There is no fallback to kith-auth, whatever happened.
pub async fn owner_call(method: &'static str, mut params: Value, repeatable: bool) -> anyhow::Result<Value> {
    let core = process_core();
    if let Some(o) = params.as_object_mut() {
        o.extend(core.view());
    }
    let answer =
        tokio::task::spawn_blocking(move || crate::core_front::call_owner(&core.folders, method, &params, repeatable))
            .await
            .map_err(|e| anyhow::anyhow!(CoreDown(e.to_string())))?;
    match answer {
        crate::core_front::Answer::Done(v) => Ok(v),
        crate::core_front::Answer::Refused(said) => {
            let said = said.strip_prefix("authority: ").unwrap_or(&said).to_owned();
            Err(anyhow::anyhow!(CoreRefused(said)))
        }
        crate::core_front::Answer::Down { why, lost: false } => Err(anyhow::anyhow!(CoreDown(why))),
        crate::core_front::Answer::Down { why, lost: true } => {
            Err(anyhow::anyhow!(CoreDown(format!("{why}; it may have run"))))
        }
    }
}

/// What a kith-auth call comes to in the core: the `people.*` method and its params, or None for a call the core has no
/// method for (a switched host refuses those; it never runs kith-auth on a tombstone).
fn core_form(args: &[&str]) -> Option<(&'static str, Value)> {
    let settings = |from: usize, keys: &[&str]| -> Option<Value> {
        let mut m = serde_json::Map::new();
        for a in &args[from..] {
            let (k, v) = a.split_once('=')?;
            keys.contains(&k).then(|| m.insert(k.to_owned(), v.into()))?;
        }
        Some(Value::Object(m))
    };
    Some(match args {
        ["people", "--json" | _] => ("people.listPeople", json!({})),
        ["retired", id] => ("people.retired", json!({"id": id})),
        ["member", agent, person, "--rm"] => ("people.revoke", json!({"person": person, "agent": agent})),
        ["member", agent, person, ..] => {
            let mut p = settings(3, &["preset", "mode", "budget", "deny", "model"])?;
            p["person"] = (*person).into();
            p["agent"] = (*agent).into();
            ("people.grant", p)
        }
        // The spending limits and the grants ledger (`limits.rs`, `people.rs`): kith-auth's own reads and writes of them.
        ["bots-cap", amount] => ("people.botsCap", json!({"amount": amount})),
        ["budgets", agent, "--json"] => ("people.budgets", json!({"agent": agent})),
        ["ledger", "--rm", key] => ("people.removeGrant", json!({"key": key})),
        ["ledger", "--json", rest @ ..] => {
            let mut p = serde_json::Map::new();
            for pair in rest.chunks(2) {
                let [flag, value] = pair else { return None };
                p.insert(flag.strip_prefix("--").filter(|k| matches!(*k, "person" | "agent"))?.to_owned(), (*value).into());
            }
            ("people.ledger", Value::Object(p))
        }
        // A chat handle (a member's join binds the account, the end of access lets it go).
        ["handle", person, platform, "--rm"] => {
            ("people.handle", json!({"person": person, "platform": platform, "remove": true}))
        }
        ["handle", person, platform, uid] => ("people.handle", json!({"person": person, "platform": platform, "uid": uid})),
        ["bot", id, "--rm"] => ("people.removeBot", json!({"id": id})),
        ["bot", id, "--of", owner, ..] => {
            let mut p = settings(4, &["name", "provider"])?;
            p["id"] = (*id).into();
            p["of"] = (*owner).into();
            ("people.addBot", p)
        }
        ["person", id, "--rm", "--unused"] => ("people.removePerson", json!({"id": id, "unused": true})),
        ["person", id, "--revoke", "--local"] => ("people.suspend", json!({"id": id})),
        ["person", id, "--unrevoke", "--local"] => ("people.undo", json!({"id": id})),
        ["person", id, "--clear-temp"] => ("people.clearTemp", json!({"id": id})),
        ["person", id, ..] => {
            let mut p = settings(2, &["name", "lang", "tz"])?;
            p["id"] = (*id).into();
            ("people.addPerson", p)
        }
        _ => return None,
    })
}

/// The host's kith-auth calls for people, members and bots (`people.rs`, `botguests.rs`): kith-auth while the host is not
/// switched (as it was), the matching `people.*` method of the core once it is ([`switched`]). A switched host whose core
/// does not answer gets an error and nothing is written another way.
pub(crate) async fn run_auth(bin: &Path, args: &[&str]) -> anyhow::Result<String> {
    if !switched() {
        // Today's call, with its stderr warning logged.
        return crate::remote::account::run(bin, args).await;
    }
    run_auth_noted(bin, args).await.map(|(out, _)| out)
}

/// [`run_auth`] with what kith-auth said on stderr (nothing from the core: its copy is the only one, so no copy can miss).
pub(crate) async fn run_auth_noted(bin: &Path, args: &[&str]) -> anyhow::Result<(String, String)> {
    if !switched() {
        return crate::remote::account::run_noted(bin, args).await;
    }
    let Some((method, params)) = core_form(args) else {
        anyhow::bail!("`kith-auth {}` has no core method: authority is on the core", args.join(" "));
    };
    // A read repeats harmlessly; a write is never sent twice.
    let read = matches!(method, "people.listPeople" | "people.retired");
    let said = owner_call(method, params, read).await?;
    // `kith-auth people X` and `retired X` answer by their exit code.
    let rows = said["rows"].as_array();
    match (method, args) {
        ("people.listPeople", ["people", "--json"]) => Ok((serde_json::to_string(&said["rows"])?, String::new())),
        ("people.listPeople", ["people", id]) => {
            if !rows.is_some_and(|r| r.iter().any(|p| p["id"] == *id)) {
                anyhow::bail!("no person `{id}`");
            }
            Ok((String::new(), String::new()))
        }
        ("people.listPeople", _) => Ok((serde_json::to_string(&said["rows"])?, String::new())),
        ("people.retired", _) if said["retired"] != true => anyhow::bail!("`{}` is not retired", args[1]),
        _ => Ok((
            said["lines"].as_array().into_iter().flatten().filter_map(Value::as_str).collect::<Vec<_>>().join("\n"),
            String::new(),
        )),
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;

    #[test]
    fn a_denied_gate_names_how_many_changes_the_core_missed() {
        let a = json!({"ok": false, "warning": "authority: 3 difference(s) between the core and the old store (x); not served from the core"});
        let (open, line) = read_gate(Some(&a));
        let line = line.unwrap();
        assert!(!open);
        assert!(
            line.contains("the core missed 3 changes, run `kith kernel core-migrate --authority --force`"),
            "{line}"
        );
        let (open, line) = read_gate(Some(&json!({"ok": false, "warning": null})));
        assert!(!open && line.unwrap().contains("not switched"));
        assert_eq!(read_gate(Some(&json!({"ok": true, "warning": null}))), (true, None));
        let (open, line) = read_gate(Some(&json!({"ok": true, "warning": "WARNING x"})));
        assert!(open && line.as_deref() == Some("WARNING x"));
    }

    #[test]
    fn a_tombstone_in_auth_json_means_switched_and_a_rollback_undoes_it() {
        let dir = std::env::temp_dir().join(format!("kith-switched-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let auth = dir.join("auth.json");
        assert!(!switched_at(&auth), "a missing file is not a tombstone");
        std::fs::write(&auth, r#"{"people": {}, "mentions": "kith_tombstone"}"#).unwrap();
        assert!(!switched_at(&auth), "the word alone is not the key");
        std::fs::write(&auth, r#"{"kith_tombstone": 1, "module": "authority", "rollback": "kith rollback authority"}"#)
            .unwrap();
        assert!(switched_at(&auth));
        // The way back puts the file back (a new inode, as kith-auth writes it).
        std::fs::remove_file(&auth).unwrap();
        std::fs::write(&auth, r#"{"people": {}}"#).unwrap();
        assert!(!switched_at(&auth));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_view_without_a_known_home_sends_none_rather_than_an_empty_one() {
        let folders = || crate::kernel::folders(Path::new("/h"));
        let none = CoreData::new(folders(), PathBuf::new(), PathBuf::from("/k"), serde_json::Map::new());
        assert!(none.view().get("home").is_none(), "the core would refuse \"\" as no home");
        let some = CoreData::new(folders(), PathBuf::from("/h"), PathBuf::from("/k"), serde_json::Map::new());
        assert_eq!(some.view()["home"], "/h");
    }

    /// The latch is one static: the tests that touch it take turns.
    static LATCH: Mutex<()> = Mutex::new(());

    fn open_latch(at: Instant) {
        *GATE.lock().unwrap() = Some(Gate { ok: true, fingerprint: "1:2:3:4".into(), asked_at: at });
    }

    #[test]
    fn the_gate_is_closed_until_asked_and_closes_when_auth_json_changes() {
        let _one = LATCH.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *GATE.lock().unwrap() = None;
        // Not asked yet (or no thread running): closed.
        assert!(!gate_open("1:2:3:4"));
        open_latch(Instant::now());
        assert!(gate_open("1:2:3:4"));
        assert!(!gate_open("1:2:9:9"), "a changed auth.json closes it at once");
        *GATE.lock().unwrap() = None;
    }

    #[test]
    fn a_stale_latch_reads_closed() {
        let _one = LATCH.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let asked = Instant::now();
        open_latch(asked);
        assert!(gate_open_at("1:2:3:4", asked + GATE_EVERY), "one ask old is fine");
        assert!(!gate_open_at("1:2:3:4", asked + GATE_MAX_AGE), "a thread stuck on a hung daemon closes it by age");
        assert!(!gate_open_at("1:2:3:4", asked + GATE_MAX_AGE * 10));
        *GATE.lock().unwrap() = None;
    }

    #[test]
    fn a_panicking_gate_thread_leaves_the_gate_closed() {
        let _one = LATCH.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        open_latch(Instant::now());
        assert!(gate_open("1:2:3:4"));
        let t = std::thread::spawn(|| {
            let _closes = CloseOnExit;
            panic!("the gate thread dies");
        });
        assert!(t.join().is_err());
        assert!(!gate_open("1:2:3:4"));
        *GATE.lock().unwrap() = None;
    }

    #[test]
    fn the_gate_and_the_core_read_the_same_auth_json() {
        // The core resolves the file from the home, kernel and env this process sends it (`folders_for`); the gate thread's
        // fingerprint and `core_selected` stat what that comes to.
        let core = CoreData::from_process();
        let lookup = |name: &str| core.env.get(name).and_then(Value::as_str).map(str::to_owned);
        let theirs = kith_kernel::paths::paths(&core.home, &core.kernel, &lookup).secrets.join("auth.json");
        assert_eq!(auth_of(&core), theirs);
        assert_eq!(auth_path(), theirs.as_path());
    }
}
