//! Terminal and app threads: the writer claim of a Claude session, the session registry and what ends a claim. A port of
//! `kernel/bin/kith-sessions` (`record`, `resolve`, `claim`, `terminal`, `proc`, `sweep`) and of the host's
//! `terminal/claim.rs`, `registry.rs` and `intent.rs`, which stay the live implementations while the verbs are not
//! switched. The crate writes the existing file protocol exactly (no version field: the old writers know none), so
//! Python, the host and this crate can all be actors on one `<state>/live/terminal` folder:
//!
//! - `<sid>.claim`: `{"pid","procStart","at","kind"[,"person"[,"agent"]]}`, made whole in a temp file and linked into
//!   place (the link fails when a claim is there), 0600. Whoever holds it is the one writer of the session. A claim whose
//!   pid is gone, or started at another time, is stale and replaced under `<sid>.claim.lock` (flock). A take-over (the
//!   host's) uses `<sid>.takeover.claim` and `<sid>.takeover.claim.lock` and adds `"takeover":true`.
//! - `<sid>.json`: the registry entry `record` writes (dir 0700, file 0600, tmp + rename).
//! - `proc-<pid>.claim`: a session-less claim naming a person, so the revoke sweep finds the run.
//!
//! The differences from the three live implementations are listed in the PR that added this module (and in the tests
//! named `*_differs_*`); none of them changes what a reader of the folder sees for a claim a live writer made.

pub mod cli;
pub mod intent;
// The one place with `unsafe` (kill(2)); the crate denies it everywhere else.
#[allow(unsafe_code)]
pub(crate) mod os;
pub mod registry;
pub mod resolve;
#[cfg(test)]
mod tests;

use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use fs2::FileExt as _;
use serde_json::{Map, Value};

use crate::paths::Env;
use crate::secrets::pyjson;

pub use resolve::{Host, Resolved, resolve};

/// What a run of `kith kernel threads` needs to know about where it is.
pub struct Ctx<'a> {
    /// `$KITH_STATE`; `None` when it is not set (the verbs say so, as `kith-sessions` does).
    pub state: Option<PathBuf>,
    /// The Mac's folder (`accounts/claude/*` holds the extra logins' transcripts).
    pub machine: Option<PathBuf>,
    pub home: PathBuf,
    pub env: Env<'a>,
}

impl Ctx<'_> {
    /// `<state>/live/terminal`, where the registry and the claims are.
    pub fn reg_dir(&self) -> Option<PathBuf> {
        self.state.as_deref().map(registry_dir)
    }

    /// Claude's own folder for this run: `CLAUDE_CONFIG_DIR`, else `~/.claude`.
    pub fn claude_dir(&self) -> PathBuf {
        (self.env)("CLAUDE_CONFIG_DIR")
            .filter(|d| !d.is_empty())
            .map_or_else(|| self.home.join(".claude"), PathBuf::from)
    }
}

/// `<state>/live/terminal`.
pub fn registry_dir(state: &Path) -> PathBuf {
    state.join("live").join("terminal")
}

/// A session id is what names files: only what Claude makes (the host's `safe_id`; `kith-sessions` checks nothing).
pub fn safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
}

/// Now as `kith-sessions` writes it: `2026-11-03T04:05:06Z`.
pub fn iso_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (days, rem) = (i64::try_from(secs / 86_400).unwrap_or(0), secs % 86_400);
    // Days since 1970-01-01 to a civil date (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn tmp_name(base: &str) -> String {
    format!(
        ".{base}.{}.{}.tmp",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

fn create_new_private(path: &Path, body: &str) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let _ = std::fs::remove_file(path);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(body.as_bytes())
}

/// `json.dump` of `value` to `path` through a temp file and a rename, 0600, in a 0700 folder (made when missing;
/// `chmod_dir`: also set when it exists, as the registry's `write_atomic` does).
pub(crate) fn write_json(path: &Path, value: &Value, chmod_dir: bool) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let parent = path.parent().unwrap_or(Path::new("."));
    create_private_dir(parent)?;
    if chmod_dir {
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = parent.join(tmp_name(&name));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(pyjson::dumps(value).as_bytes())?;
    drop(file);
    std::fs::rename(&tmp, path)
}

/// An exclusive flock on a lock file (made 0600 when missing), held until dropped.
struct Lock(#[allow(dead_code)] File);

impl Lock {
    fn exclusive(path: &Path) -> std::io::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt as _;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        file.lock_exclusive()?;
        Ok(Self(file))
    }
}

// --- the claim ---------------------------------------------------------------------------------------------------------

/// A writer claim as the files carry it. Unknown keys are ignored and missing ones read as `kith-sessions` reads them,
/// so a claim written before a field existed (no `kind`, no `procStart`, no `person`) is still a claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    /// `None`: the file has no usable pid (a string, a float, nothing): never live.
    pub pid: Option<i64>,
    pub proc_start: Option<String>,
    pub at: String,
    /// `terminal`, `oneshot`, `app`, `bot`...: any text; an unknown kind is held like a known one.
    pub kind: String,
    pub person: Option<String>,
    pub agent: Option<String>,
    pub takeover: bool,
}

impl Claim {
    /// The claim a JSON object holds; `None` when the text is not an object (damaged files read as no claim).
    pub fn from_value(v: &Value) -> Option<Self> {
        let o = v.as_object()?;
        let text = |k: &str| o.get(k).and_then(Value::as_str).map(str::to_owned);
        Some(Self {
            pid: o.get("pid").and_then(Value::as_i64),
            proc_start: text("procStart"),
            at: text("at").unwrap_or_default(),
            kind: text("kind").unwrap_or_else(|| "terminal".to_owned()),
            person: text("person"),
            agent: text("agent"),
            takeover: o.get("takeover").and_then(Value::as_bool).unwrap_or(false),
        })
    }

    /// The file's JSON, in `kith-sessions` order. `agent` is written only with a `person`, and `takeover` only when set,
    /// as the Python never writes either alone.
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("pid".into(), self.pid.map_or(Value::Null, Value::from));
        m.insert(
            "procStart".into(),
            self.proc_start.clone().map_or(Value::Null, Value::from),
        );
        m.insert("at".into(), self.at.clone().into());
        m.insert("kind".into(), self.kind.clone().into());
        if let Some(p) = &self.person {
            m.insert("person".into(), p.clone().into());
            if let Some(a) = &self.agent {
                m.insert("agent".into(), a.clone().into());
            }
        }
        if self.takeover {
            m.insert("takeover".into(), true.into());
        }
        Value::Object(m)
    }

    /// The pid as the messages print it (`None` for a claim without one).
    pub fn pid_text(&self) -> String {
        self.pid
            .map_or_else(|| "None".to_owned(), |p| p.to_string())
    }

    /// The claimer still runs, and is the process that took the claim (a reused pid started at another time). A claim
    /// that records no start time, or one `ps` cannot give, is judged by the pid alone.
    pub fn is_live(&self) -> bool {
        let Some(pid) = self.pid else { return false };
        if !os::pid_runs(pid) {
            return false;
        }
        match (
            self.proc_start.as_deref().filter(|s| !s.is_empty()),
            os::proc_start(pid),
        ) {
            (Some(claimed), Some(actual)) => {
                claimed.split_whitespace().collect::<Vec<_>>().join(" ") == actual
            }
            _ => true,
        }
    }
}

/// Someone else holds the session.
#[derive(Debug, PartialEq, Eq)]
pub struct Held(pub Claim);

/// `<sid>.claim`, or `<sid>.takeover.claim`.
pub fn claim_path(dir: &Path, sid: &str, takeover: bool) -> PathBuf {
    dir.join(format!(
        "{sid}.{}claim",
        if takeover { "takeover." } else { "" }
    ))
}

fn lock_path(claim: &Path) -> PathBuf {
    let mut name = claim
        .file_name()
        .map(std::ffi::OsString::from)
        .unwrap_or_default();
    name.push(".lock");
    claim.with_file_name(name)
}

/// `<something>.claim` (not a temp file or a lock).
fn is_claim_file(name: &str) -> bool {
    Path::new(name).extension().is_some_and(|e| e == "claim")
}

/// A claim file's claim; `None` for a missing, empty, damaged or non-object file.
pub fn read_claim(path: &Path) -> Option<Claim> {
    Claim::from_value(&serde_json::from_str::<Value>(&std::fs::read_to_string(path).ok()?).ok()?)
}

/// The live claim on `sid` under either name (any kind: the host's own turn claim included, which the host itself does
/// not count against a send; that rule stays in the host).
pub fn live_holder(dir: &Path, sid: &str) -> Option<Claim> {
    [false, true]
        .into_iter()
        .filter_map(|t| read_claim(&claim_path(dir, sid, t)))
        .find(Claim::is_live)
}

/// How a claim is taken.
#[derive(Clone, Copy, Debug)]
pub struct Take<'a> {
    pub kind: &'a str,
    /// Whose profile the run has (absent: the owner).
    pub person: Option<&'a str>,
    /// With `person`: the agent, so narrowing access ends only the runs on agents the person lost.
    pub agent: Option<&'a str>,
    /// `kith-lifecycle` taking the session to compact or end keeps the intent and decision other takers clear.
    pub lifecycle_owner: bool,
    /// A take-over claim of the host: its own file and lock; it ends no intent and prunes nothing.
    pub takeover: bool,
}

impl<'a> Take<'a> {
    pub fn new(kind: &'a str) -> Self {
        Self {
            kind,
            person: None,
            agent: None,
            lifecycle_owner: false,
            takeover: false,
        }
    }
}

/// Takes the writer claim on `sid` for `pid` (`take_claim`): `Ok(Ok(()))` taken, `Ok(Err(Held))` a live claim holds it.
/// The claim is made whole in a temp file and linked into place (exclusive); a stale or damaged one is removed and
/// replaced under the lock file next to it, so two takers never both judge it stale and one deletes the other's fresh
/// claim. A taker ends the session's intent and pending decision (F12 rules 12, 14).
pub fn take_claim(c: &Ctx, sid: &str, pid: i64, how: &Take) -> std::io::Result<Result<(), Held>> {
    let Some(dir) = c.reg_dir() else {
        return Err(std::io::Error::other(
            "KITH_STATE is not set, so the session cannot be claimed",
        ));
    };
    if !safe_id(sid) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a session id",
        ));
    }
    take_in(&dir, c.state.as_deref(), sid, pid, how)
}

fn take_in(
    dir: &Path,
    state: Option<&Path>,
    sid: &str,
    pid: i64,
    how: &Take,
) -> std::io::Result<Result<(), Held>> {
    create_private_dir(dir)?;
    let mine = Claim {
        pid: Some(pid),
        proc_start: os::proc_start(pid),
        at: iso_utc(),
        kind: how.kind.to_owned(),
        person: how.person.map(str::to_owned),
        agent: how.agent.map(str::to_owned),
        takeover: how.takeover,
    };
    let dest = claim_path(dir, sid, how.takeover);
    let tmp = dir.join(tmp_name(&format!("{sid}.claim")));
    create_new_private(&tmp, &pyjson::dumps(&mine.to_value()))?;
    let outcome = (|| {
        let lock = Lock::exclusive(&lock_path(&dest))?;
        for _ in 0..3 {
            match std::fs::hard_link(&tmp, &dest) {
                Ok(()) => return Ok(Ok(lock)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
            if let Some(seen) = read_claim(&dest).filter(Claim::is_live) {
                return Ok(Err(Held(seen)));
            }
            // Stale or garbage; nobody else judges it while we hold the lock.
            let _ = std::fs::remove_file(&dest);
        }
        Ok(Err(Held(read_claim(&dest).unwrap_or_else(|| mine.clone()))))
    })();
    let _ = std::fs::remove_file(&tmp);
    match outcome? {
        Ok(lock) => {
            // The lock is given up before the others are taken in turn: two takers each holding one and wanting the
            // other's would wait on each other for good.
            drop(lock);
            if !how.takeover {
                prune_stale(dir, &dest);
                if !how.lifecycle_owner
                    && let Some(state) = state
                {
                    intent::taken(state, sid);
                }
            }
            Ok(Ok(()))
        }
        Err(held) => Ok(Err(held)),
    }
}

/// Removes the session claims whose holder is gone (a one-shot exec'd claude, so nothing of its gives its claim back): a
/// stale claim is free anyway, this only keeps a fresh run's claim from staying behind for good. Each under its lock,
/// re-read, as a taker would judge it. The host's take-over claims and the proc claims are left to their owners.
fn prune_stale(dir: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let f = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if f == keep
            || !is_claim_file(&name)
            || name.starts_with("proc-")
            || name.ends_with(".takeover.claim")
        {
            continue;
        }
        let Ok(_lock) = Lock::exclusive(&lock_path(&f)) else {
            continue;
        };
        if f.exists() && !read_claim(&f).as_ref().is_some_and(Claim::is_live) {
            let _ = std::fs::remove_file(&f);
        }
    }
}

/// Gives back every claim `pid` holds, each under its lock and only if it is still the holder's (a taker may have replaced
/// the claim since the listing). The terminal's own run ended.
pub fn release_claims(c: &Ctx, pid: i64) {
    let Some(dir) = c.reg_dir() else { return };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for e in entries.flatten() {
        let f = e.path();
        if f.extension().is_none_or(|x| x != "claim") {
            continue;
        }
        let Ok(_lock) = Lock::exclusive(&lock_path(&f)) else {
            continue;
        };
        if read_claim(&f).is_some_and(|c| c.pid == Some(pid)) {
            let _ = std::fs::remove_file(&f);
        }
    }
}

/// Gives back the claim `pid` holds on `sid` under the name asked for (the host's `release`).
pub fn release_one(c: &Ctx, sid: &str, pid: i64, takeover: bool) {
    let Some(dir) = c.reg_dir() else { return };
    let path = claim_path(&dir, sid, takeover);
    let _lock = Lock::exclusive(&lock_path(&path)).ok();
    if read_claim(&path).is_some_and(|c| c.pid == Some(pid)) {
        let _ = std::fs::remove_file(path);
    }
}

/// How long a one-shot waits for a held session (seconds; `KITH_CLAIM_WAIT`, default 5).
pub fn claim_wait(env: Env) -> Duration {
    let secs = env("KITH_CLAIM_WAIT")
        .map_or(5.0, |v| v.trim().parse::<f64>().unwrap_or(5.0))
        .max(0.0);
    Duration::try_from_secs_f64(secs).unwrap_or(Duration::from_secs(5))
}

/// [`take_claim`], retried for up to `wait` while a live writer holds the session. `None` when taken (also when `pid`
/// already holds it), else the live claim that still holds it.
pub fn claim_waiting(
    c: &Ctx,
    sid: &str,
    pid: i64,
    how: &Take,
    wait: Duration,
) -> std::io::Result<Option<Claim>> {
    let end = Instant::now() + wait;
    loop {
        match take_claim(c, sid, pid, how)? {
            Ok(()) => return Ok(None),
            Err(Held(held)) if held.pid == Some(pid) => return Ok(None),
            Err(Held(held)) => {
                if Instant::now() >= end {
                    return Ok(Some(held));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// `claim` / `terminal`: takes the claim for a launcher's shell, and refuses while a take-over runs. The error is the line
/// `kith-sessions` dies with (without its `kith: `).
pub fn claim_start(
    c: &Ctx,
    sid: &str,
    pid: i64,
    kind: &str,
    wait: Duration,
    person: Option<&str>,
    agent: Option<&str>,
) -> Result<(), String> {
    let Some(dir) = c.reg_dir() else {
        return Err("KITH_STATE is not set, so the session cannot be claimed".to_owned());
    };
    let how = Take {
        person,
        agent,
        ..Take::new(kind)
    };
    if let Some(held) = claim_waiting(c, sid, pid, &how, wait).map_err(|e| e.to_string())? {
        return Err(format!(
            "that session is busy: held by {} (pid {}); try again in a moment",
            held.kind,
            held.pid_text()
        ));
    }
    if read_claim(&claim_path(&dir, sid, true))
        .as_ref()
        .is_some_and(Claim::is_live)
    {
        release_claims(c, pid);
        return Err(
            "that session is being taken over in the app right now; try again in a moment"
                .to_owned(),
        );
    }
    Ok(())
}

/// `proc`: a run that has a person's profile is findable by the revoke sweep for its whole life. `proc-<pid>.claim`
/// names no session, so it keeps no writer out. Claims of processes that are gone are cleaned up here.
pub fn proc_claim(c: &Ctx, pid: i64, person: &str, agent: &str) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let Some(dir) = c.reg_dir() else {
        return Ok(());
    };
    create_private_dir(&dir)?;
    for e in std::fs::read_dir(&dir)?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("proc-")
            && is_claim_file(&name)
            && !read_claim(&e.path()).as_ref().is_some_and(Claim::is_live)
        {
            let _ = std::fs::remove_file(e.path());
        }
    }
    let mine = Claim {
        pid: Some(pid),
        proc_start: os::proc_start(pid),
        at: iso_utc(),
        kind: "app".to_owned(),
        person: Some(person.to_owned()),
        agent: Some(agent.to_owned()),
        takeover: false,
    };
    let tmp = dir.join(tmp_name(&format!("proc-{pid}.claim")));
    std::fs::write(&tmp, pyjson::dumps(&mine.to_value()))?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&tmp, dir.join(format!("proc-{pid}.claim")))
}

/// The kernel's twin of the host's claim sweep, for a revoke or removal made with no host running: the live holders of
/// claims that name one of `persons`, and their descendants, get SIGTERM, and SIGKILL after `grace`. A claim without a
/// person is the owner's and is never touched; each holder is checked live first (pid and start time), so a reused pid is
/// left alone. Returns the pids it signalled.
pub fn sweep_persons(c: &Ctx, persons: &[String], grace: Duration) -> Vec<i64> {
    let Some(dir) = c.reg_dir() else {
        return Vec::new();
    };
    let mut holders = Vec::new();
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        if e.path().extension().is_none_or(|x| x != "claim") {
            continue;
        }
        if let Some(claim) = read_claim(&e.path())
            && claim.person.as_ref().is_some_and(|p| persons.contains(p))
            && claim.is_live()
            && let Some(pid) = claim.pid.filter(|p| *p > 1)
        {
            holders.push(pid);
        }
    }
    if holders.is_empty() {
        return Vec::new();
    }
    let mut kids: std::collections::HashMap<i64, Vec<i64>> = std::collections::HashMap::new();
    for (pid, ppid) in os::ps_table() {
        kids.entry(ppid).or_default().push(pid);
    }
    let me = i64::from(std::process::id());
    let (mut victims, mut todo) = (Vec::new(), holders);
    while let Some(pid) = todo.pop() {
        if !victims.contains(&pid) && pid != me {
            victims.push(pid);
            todo.extend(kids.get(&pid).cloned().unwrap_or_default());
        }
    }
    for (sig, wait) in [(os::SIGTERM, grace), (os::SIGKILL, Duration::ZERO)] {
        for pid in &victims {
            os::signal(*pid, sig);
        }
        let end = Instant::now() + wait;
        while !wait.is_zero() && Instant::now() < end && victims.iter().any(|p| os::pid_runs(*p)) {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    victims
}
