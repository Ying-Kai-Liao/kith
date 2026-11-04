//! The writer claim: one file per Claude session, `<state>/live/terminal/<session>.claim`, created exclusively. Whoever
//! holds it is the one writer of that session, even before the registry file (written by the session's own hook seconds
//! later) exists and even while the host is down. The kernel (`kernel/bin/kith-sessions`) writes and reads the same file.
//!
//! `{"pid": <claimer>, "procStart": "<start stamp>", "at": "<rfc3339>", "kind": "terminal" | "app" | "bot" | "oneshot" | <any other string>, "takeover": bool}`
//!
//! A claim whose pid is gone, or whose pid now started at another time, is stale and is replaced under a lock file
//! (`<sid>.claim.lock`, flock). A claimer is a long-lived process: `kith <agent> --resume` claims for the shell
//! that then `exec`s into `kith-sessions run`, so the pid stays the same until claude has exited.
//!
//! The file is made whole in a temp file (`O_CREAT|O_EXCL`) and linked into place (`link` fails when the name exists), so
//! no reader ever sees an empty claim.
//!
//! Another kind of writer (a bot guest's `kith ask --thread`, see serve/host/README.md) calls
//! `take(state, sid, Kind::Bot, std::process::id(), false)` BEFORE it checks the registry and `hostBusy`, and `release`
//! when its turn is over.

use super::registry::{dir, pid_runs, proc_start, safe_id};
use serde::{Deserialize, Serialize};
use std::io::Write as _;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Terminal,
    App,
    Bot,
    /// A `claude -p --resume` one-shot (`kith ask -c`, `kith <agent> -c "task"`, the bridge): keeps the host out like a terminal.
    Oneshot,
    /// The core is starting a held terminal for the session and the claim still names the core's own pid (`kith serve`),
    /// not the terminal's: nothing may signal it. `stop_holder` answers it as busy.
    Starting,
    /// A kind this build does not know (a newer binary wrote it, e.g. M1's holder kind). Read as a live holder judged by
    /// pid + start alone, like `kith-sessions` and the crate do, and never removed as garbage by `take_as`.
    #[serde(other)]
    Other,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Claim {
    pub pid: u32,
    #[serde(default)]
    pub proc_start: Option<String>,
    #[serde(default)]
    pub at: String,
    #[serde(default = "terminal")]
    pub kind: Kind,
    #[serde(default)]
    pub takeover: bool,
    /// Whose profile the holder runs with: an invited person's id or a bot guest's. Absent is the owner (old claim files,
    /// the terminal), and the revoke sweep never touches those. Set by the writer from what kith-auth resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person: Option<String>,
    /// The agent the run is on (with `person`): narrowing someone's access ends only the holders on agents they lost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

fn terminal() -> Kind {
    Kind::Terminal
}

impl Claim {
    /// Whether the claimer's process is the one that took the claim: `Some(true)` positively, `Some(false)` positively
    /// not (a reused pid), `None` when that cannot be told (an unreadable start, a stamp of the other form). A claim with no
    /// start recorded is judged by its pid alone (`Some(true)`). A claim from before stamps holds `ps -o lstart` text, which
    /// is compared the old way ([`super::registry::lstart_matches`]).
    pub fn identity(&self) -> Option<bool> {
        use kith_kernel::procstart::{comparable, is_stamp, same_process};
        match self.proc_start.as_deref().filter(|s| !s.trim().is_empty()) {
            None => Some(true),
            Some(claimed) if is_stamp(claimed) => proc_start(self.pid)
                .filter(|actual| comparable(claimed, actual))
                .map(|actual| same_process(claimed, &actual)),
            Some(text) => super::registry::lstart_matches(self.pid, text),
        }
    }

    /// The claimer still runs, and is not positively another process (a reused pid has another start). Held when the start
    /// cannot be read or compared: a wrong "free" would let a second writer in.
    pub fn is_live(&self) -> bool {
        pid_runs(self.pid) && self.identity() != Some(false)
    }

    /// Whether the claimer is positively the process that took the claim, which is what a signal needs: [`Self::is_live`]
    /// also says "held" when the start cannot be compared.
    pub fn is_claimer(&self) -> bool {
        self.identity() == Some(true)
    }
}

/// A take-over claims under its own name so the terminal's claim is left to its own `run`.
pub fn path(state: &Path, sid: &str, takeover: bool) -> PathBuf {
    dir(state).join(format!("{sid}.{}claim", if takeover { "takeover." } else { "" }))
}

/// Someone else holds the session.
#[derive(Debug, PartialEq, Eq)]
pub struct Held(pub Claim);

pub fn read(path: &Path) -> Option<Claim> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// The live claim on `sid` (either name) that keeps the host from writing, if any. A host turn's own claim (`Kind::App`,
/// see `agent/bot/turn.rs`) is not one: it only keeps a terminal out. A take-over's claim is.
pub fn holder(state: &Path, sid: &str) -> Option<Claim> {
    [false, true]
        .into_iter()
        .filter_map(|t| read(&path(state, sid, t)))
        .filter(|c| c.kind != Kind::App || c.takeover)
        .find(Claim::is_live)
}

/// Takes the claim for `pid` (a running process). `Err(Held)`: a live claim is there. A stale claim is replaced under a
/// lock file next to it, so two takers never both judge it stale and one delete the other's fresh claim.
pub fn take(state: &Path, sid: &str, kind: Kind, pid: u32, takeover: bool) -> std::io::Result<Result<(), Held>> {
    take_as(state, sid, kind, pid, takeover, None, None)
}

/// [`take`] for a run that has `person`'s profile on `agent` (`None`: the owner's).
pub fn take_as(
    state: &Path,
    sid: &str,
    kind: Kind,
    pid: u32,
    takeover: bool,
    person: Option<&str>,
    agent: Option<&str>,
) -> std::io::Result<Result<(), Held>> {
    if !safe_id(sid) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a session id"));
    }
    let dest = path(state, sid, takeover);
    let parent = dest.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mine = Claim {
        pid,
        proc_start: proc_start(pid),
        at: chrono::Utc::now().to_rfc3339(),
        kind,
        takeover,
        person: person.map(str::to_owned),
        agent: agent.map(str::to_owned),
    };
    let tmp = parent.join(format!(".{sid}.claim.{}.{}.tmp", std::process::id(), pid));
    write_new(&tmp, &serde_json::to_string(&mine).map_err(std::io::Error::other)?)?;
    let result = (|| {
        let _lock =
            Lock::exclusive(&parent.join(format!("{sid}.{}claim.lock", if takeover { "takeover." } else { "" })))?;
        for _ in 0..3 {
            match std::fs::hard_link(&tmp, &dest) {
                Ok(()) => return Ok(Ok(())),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
            if let Some(seen) = read(&dest).filter(Claim::is_live) {
                return Ok(Err(Held(seen)));
            }
            // Stale, or garbage: removed while we hold the lock.
            let _ = std::fs::remove_file(&dest);
        }
        Ok(Err(Held(read(&dest).unwrap_or_else(|| mine.clone()))))
    })();
    let _ = std::fs::remove_file(&tmp);
    // A process that took the session ends the intent of the one before it and the decision of its idle period (F12 rules
    // 12, 14), as the kernel's take_claim does; a take-over claim is the host's step towards a stop, not a new writer.
    if matches!(result, Ok(Ok(()))) && !takeover {
        super::intent::taken(state, sid);
    }
    result
}

/// Keeps a person's agent process findable between turns: the turn claim is given back when the turn ends, but the warm
/// process (with that person's profile) goes on, and after a host crash it can outlive the host. This claim names no
/// session (`proc-<pid>`), so it keeps no writer out; it only lets [`sweep`] find the process. Stale ones are pruned.
pub fn take_proc(state: &Path, pid: u32, person: &str, agent: Option<&str>) -> std::io::Result<()> {
    take_as(state, &format!("proc-{pid}"), Kind::App, pid, false, Some(person), agent)?.ok();
    if let Ok(entries) = std::fs::read_dir(dir(state)) {
        for e in entries.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("proc-") && name.ends_with(".claim") && read(&e.path()).is_some_and(|c| !c.is_live()) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    Ok(())
}

/// The live claims that name one of `persons` (what [`sweep`] would consider).
pub fn holders_of(state: &Path, persons: &[String]) -> Vec<Claim> {
    let Ok(entries) = std::fs::read_dir(dir(state)) else { return Vec::new() };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "claim"))
        .filter_map(|e| read(&e.path()))
        .filter(|c| c.person.as_deref().is_some_and(|p| persons.iter().any(|x| x == p)) && c.is_live())
        .collect()
}

/// Ends the live holders of every claim whose person is one of `persons` (a revoked person and their bots), and their
/// child processes (the launcher's claude, ACP adapters and what they started). A claim without a person is the owner's
/// and is never touched. Each holder is checked first (`is_live`: pid and start time, so a reused pid is left alone).
/// `keep` spares a holder (a narrowed access: those still allowed on their agent). SIGTERM, then SIGKILL after `grace`. Blocking: call from `spawn_blocking`. Returns the holders it ended.
pub fn sweep(state: &Path, persons: &[String], keep: &dyn Fn(&Claim) -> bool, grace: std::time::Duration) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir(dir(state)) else { return Vec::new() };
    let mut holders: Vec<(PathBuf, Claim)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "claim") {
            continue;
        }
        let Some(claim) = read(&path) else { continue };
        if claim.person.as_deref().is_some_and(|p| persons.iter().any(|x| x == p))
            && !keep(&claim)
            && claim.is_live()
            && claim.is_claimer()
        {
            holders.push((path, claim));
        }
    }
    let me = std::process::id();
    let mut victims: Vec<u32> = Vec::new();
    for (path, claim) in &holders {
        if claim.pid == me || claim.pid <= 1 {
            continue;
        }
        // A session's holder gets its intent before the signal (F12 rule 8); a proc claim names no session.
        if let Some(sid) = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".claim"))
            && !sid.starts_with("proc-")
        {
            super::intent::write(state, sid.strip_suffix(".takeover").unwrap_or(sid), "access revoked", claim.pid);
        }
        // Children first found while the parent still lives; a one-shot's claimer is the launcher's shell, exec'd into
        // kith-stub (the conversation lock), whose child is claude: the claim's pid is the stub and its descendants are
        // claude and what it started. The registry entry records the stub as `launcher`, so an intent written here
        // for the claim's pid still reads as the run's reason.
        for pid in descendants(claim.pid).into_iter().chain([claim.pid]) {
            if pid != me && !victims.contains(&pid) {
                victims.push(pid);
            }
        }
    }
    signal(&victims, libc::SIGTERM);
    let deadline = std::time::Instant::now() + grace;
    while std::time::Instant::now() < deadline && victims.iter().any(|p| pid_runs(*p)) {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let left: Vec<u32> = victims.iter().copied().filter(|p| pid_runs(*p)).collect();
    signal(&left, libc::SIGKILL);
    let mut ended = Vec::new();
    for (path, claim) in holders {
        if claim.pid == me {
            continue;
        }
        // The holder got SIGKILL: its claim is stale (a zombie not yet reaped still counts as running). Given back under the
        // claim lock and only if it is still the holder's: a run waiting on the session may have taken it in the meantime,
        // and a plain remove would delete that run's claim, leaving two writers.
        if let Some(sid) = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".claim")) {
            release(state, sid.strip_suffix(".takeover").unwrap_or(sid), claim.pid, claim.takeover);
        }
        ended.push(claim.pid);
    }
    ended
}

#[cfg(unix)]
fn signal(pids: &[u32], sig: libc::c_int) {
    for pid in pids {
        if let Ok(p) = libc::pid_t::try_from(*pid) {
            // SAFETY: plain kill(2) on a pid we just read from `ps`.
            unsafe { libc::kill(p, sig) };
        }
    }
}

#[cfg(not(unix))]
fn signal(_pids: &[u32], _sig: i32) {}

/// Every process below `root`, from one `ps` listing.
fn descendants(root: u32) -> Vec<u32> {
    let Ok(out) = std::process::Command::new("ps").args(["-A", "-o", "pid=,ppid="]).output() else { return Vec::new() };
    let table: Vec<(u32, u32)> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
        })
        .collect();
    let mut found: Vec<u32> = Vec::new();
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for (pid, ppid) in &table {
            if *ppid == parent && !found.contains(pid) && *pid != root {
                found.push(*pid);
                frontier.push(*pid);
            }
        }
    }
    found
}

/// Whether `id` can be a Claude session id (it names a file under the state folder).
pub fn safe_session(id: &str) -> bool {
    safe_id(id)
}

/// What [`stop_holder`] came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Stopped {
    /// No live claim (none, or a stale one whose pid is gone or was reused): nothing was signalled.
    Nobody,
    /// The host's own turn holds it (the claimer is a child of this host): its Stop ends it, not this.
    OwnTurn(Claim),
    /// The core is still starting a held terminal and the claim names the core's pid: nothing was signalled.
    Busy(Claim),
    /// The claimer and its children were ended.
    Ended(Claim),
    /// SIGKILL was sent but something of the tree is still in the process table after the wait: the claim is kept, so a
    /// take-over is refused until it is really gone (Stop again).
    StillEnding(Claim),
    /// This platform cannot signal a process tree, or the claim's start cannot be matched to the process: nothing was done
    /// and the claim is kept.
    CantStop(Claim),
}

/// Ends the live holder of `sid` (the owner's Stop for a session held by an old run): SIGTERM to the claimer and its
/// claude child, SIGKILL for what is still there after `grace`. The claim is checked first (`is_live`: the pid is
/// running AND started when the claim says, so a reused pid is left alone) and the kill goes to that pid only.
///
/// This is a small kill helper of its own; it should merge with the claim sweep of the kernel-side lease work (#248).
pub fn stop_holder(state: &Path, sid: &str, grace: std::time::Duration) -> Stopped {
    stop_holder_unless(state, sid, grace, core_pid())
}

/// The running core daemon's pid, from its status (no database read). None when there is no core.
fn core_pid() -> Option<u32> {
    crate::core_front::daemon_pid()
}

/// [`stop_holder`] with the core's pid given. A claim naming the core's own pid is never signalled, whatever its kind:
/// that is `kith serve` (or an older kernel's claim of it), and a Stop would end the daemon, not a terminal.
pub(super) fn stop_holder_unless(state: &Path, sid: &str, grace: std::time::Duration, core: Option<u32>) -> Stopped {
    // A claim on a pid that was reused (positively another process) holds nothing: given back, so it does not sit there
    // as a held thread. (The readers already see it as free.)
    for takeover in [false, true] {
        if let Some(c) = read(&path(state, sid, takeover)).filter(|c| pid_runs(c.pid) && c.identity() == Some(false)) {
            release(state, sid, c.pid, takeover);
        }
    }
    let Some(claim) = holder(state, sid) else { return Stopped::Nobody };
    if claim.kind == Kind::Starting || core == Some(claim.pid) {
        return Stopped::Busy(claim);
    }
    if parent_of(claim.pid) == Some(std::process::id()) {
        return Stopped::OwnTurn(claim);
    }
    if cfg!(not(unix)) {
        return Stopped::CantStop(claim);
    }
    match claim.identity() {
        Some(true) => {}
        // Positively another process (the pid was reused since the claim was written): nothing to stop, and nothing is
        // signalled. The claim is given back so the thread is free.
        Some(false) => {
            release(state, sid, claim.pid, claim.takeover);
            return Stopped::Nobody;
        }
        // The claim's start cannot be matched to the process (ps unreadable, another stamp form): it keeps holding the
        // session, and nothing is signalled.
        None => return Stopped::CantStop(claim),
    }
    // Descendants first, the claimer last (as the revoke `sweep` does): for a one-shot the claimer is kith-stub, which
    // holds the conversation's lock, and the lock must not be free while claude still runs.
    let mut victims = children_of(claim.pid, 4);
    victims.push(claim.pid);
    // Recorded before the signal lands (F12 rule 8): a session gone with no intent reads as a crash.
    super::intent::write(state, sid, "stopped from the app", claim.pid);
    signal(&victims, libc::SIGTERM);
    let deadline = std::time::Instant::now() + grace;
    while std::time::Instant::now() < deadline && victims.iter().any(|p| pid_runs(*p)) {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // Still the claimer we judged? (The pid can only have gone, but a rest of the tree is signalled by pid alone.)
    let left: Vec<u32> = victims.iter().copied().filter(|p| pid_runs(*p)).collect();
    signal(&left, libc::SIGKILL);
    // Wait (bounded) for them to be gone before the claim is given back: a take-over started at once would otherwise
    // find the stub's lock still held and come back busy (75).
    let reaped = std::time::Instant::now() + STOP_REAP;
    while std::time::Instant::now() < reaped && victims.iter().any(|p| still_there(*p)) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // A process that outlived SIGKILL (stuck in the kernel) may still hold the stub's lock: the claim stays with it.
    if victims.iter().any(|p| still_there(*p)) {
        return Stopped::StillEnding(claim);
    }
    release(state, sid, claim.pid, claim.takeover);
    Stopped::Ended(claim)
}

/// How long [`stop_holder`] waits, after SIGKILL, for the processes to leave the table.
const STOP_REAP: std::time::Duration = std::time::Duration::from_secs(2);

/// Whether `pid` still holds anything: running, and not a zombie waiting for its parent (a zombie has released its
/// descriptors, so its flock is gone).
fn still_there(pid: u32) -> bool {
    pid_runs(pid)
        && std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .ok()
            .is_none_or(|o| !String::from_utf8_lossy(&o.stdout).trim_start().starts_with('Z'))
}

fn ps_pids(args: &[&str]) -> Vec<u32> {
    std::process::Command::new("ps")
        .args(args)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).split_whitespace().filter_map(|w| w.parse().ok()).collect())
        .unwrap_or_default()
}

fn parent_of(pid: u32) -> Option<u32> {
    ps_pids(&["-o", "ppid=", "-p", &pid.to_string()]).first().copied()
}

/// The descendants of `pid` down `depth` levels (claude and the tools it started).
fn children_of(pid: u32, depth: usize) -> Vec<u32> {
    if depth == 0 {
        return Vec::new();
    }
    let out = std::process::Command::new("pgrep").args(["-P", &pid.to_string()]).output();
    let kids: Vec<u32> = out
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).split_whitespace().filter_map(|w| w.parse().ok()).collect())
        .unwrap_or_default();
    let mut all = kids.clone();
    for k in kids {
        all.extend(children_of(k, depth - 1));
    }
    all
}

/// An advisory lock on a file, held until dropped.
struct Lock(#[allow(dead_code)] std::fs::File);

impl Lock {
    fn exclusive(path: &Path) -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd as _;
            use std::os::unix::fs::OpenOptionsExt as _;
            let file = std::fs::OpenOptions::new().write(true).create(true).truncate(false).mode(0o600).open(path)?;
            // SAFETY: the descriptor is open for the life of `file`.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(Self(file))
        }
        #[cfg(not(unix))]
        {
            Ok(Self(std::fs::OpenOptions::new().write(true).create(true).truncate(false).open(path)?))
        }
    }
}

/// Gives the claim back; only the claimer's own file is removed.
pub fn release(state: &Path, sid: &str, pid: u32, takeover: bool) {
    let p = path(state, sid, takeover);
    // Under the claim lock: a taker may have replaced our (stale) claim with its own between the read and the remove.
    let Some(parent) = p.parent() else { return };
    let _lock =
        Lock::exclusive(&parent.join(format!("{sid}.{}claim.lock", if takeover { "takeover." } else { "" }))).ok();
    if read(&p).is_some_and(|c| c.pid == pid) {
        let _ = std::fs::remove_file(p);
    }
}

fn write_new(path: &Path, body: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt as _;
    let _ = std::fs::remove_file(path);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    opts.mode(0o600);
    opts.open(path)?.write_all(body.as_bytes())
}
