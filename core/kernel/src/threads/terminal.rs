//! The host's paths over the same claim files: what `serve/host/src/terminal/claim.rs` does beyond what the CLI needs, so
//! one crate holds everything both sides do and a later switch can move them together. Nothing calls this yet; the host
//! keeps its own code until the `threads` verbs are switched (docs/plans/core-design.md section 3).
//!
//! - [`writer_holder`]: `claim::holder`, the live claim that keeps the host from writing (a host turn's own `app` claim is
//!   not one, a take-over's is).
//! - [`holders_of`] and [`sweep`]: the owner's revoke and narrowing sweep with its `keep` predicate and its intents, which
//!   releases what it ended (the CLI's [`super::sweep_persons`] does neither).
//! - [`stop_holder`]: the owner's Stop for a session held by an old run.
//!
//! These take the state folder, as the host does, not a [`super::Ctx`]. Pids are `i64` here (the host's are `u32`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{Claim, claim_path, intent, is_claim_file, os, read_claim, registry_dir, release_in};

/// The live claim on `sid` (either name) that keeps the host from writing, if any. A host turn's own claim (kind `app`, see
/// `agent/bot/turn.rs`) is not one: it only keeps a terminal out. A take-over's claim is.
pub fn writer_holder(state: &Path, sid: &str) -> Option<Claim> {
    let dir = registry_dir(state);
    [false, true]
        .into_iter()
        .filter_map(|t| read_claim(&claim_path(&dir, sid, t)))
        .filter(|c| c.kind != "app" || c.takeover)
        .find(Claim::is_live)
}

fn names(c: &Claim, persons: &[String]) -> bool {
    c.person
        .as_deref()
        .is_some_and(|p| persons.iter().any(|x| x == p))
}

fn claim_files(state: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(registry_dir(state))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| is_claim_file(&n.to_string_lossy()))
        })
        .collect()
}

/// The live claims that name one of `persons` (what [`sweep`] would consider).
pub fn holders_of(state: &Path, persons: &[String]) -> Vec<Claim> {
    claim_files(state)
        .iter()
        .filter_map(|p| read_claim(p))
        .filter(|c| names(c, persons) && c.is_live())
        .collect()
}

fn kids_table() -> HashMap<i64, Vec<i64>> {
    let mut kids: HashMap<i64, Vec<i64>> = HashMap::new();
    for (pid, ppid) in os::ps_table() {
        kids.entry(ppid).or_default().push(pid);
    }
    kids
}

/// Every process below `root`, root excluded.
fn descendants(kids: &HashMap<i64, Vec<i64>>, root: i64) -> Vec<i64> {
    let (mut found, mut frontier) = (Vec::new(), vec![root]);
    while let Some(parent) = frontier.pop() {
        for pid in kids.get(&parent).into_iter().flatten() {
            if *pid != root && !found.contains(pid) {
                found.push(*pid);
                frontier.push(*pid);
            }
        }
    }
    found
}

/// The descendants of `pid` down `depth` levels (claude and the tools it started).
fn children_of(kids: &HashMap<i64, Vec<i64>>, pid: i64, depth: usize) -> Vec<i64> {
    if depth == 0 {
        return Vec::new();
    }
    let direct = kids.get(&pid).cloned().unwrap_or_default();
    let mut all = direct.clone();
    for k in direct {
        all.extend(children_of(kids, k, depth - 1));
    }
    all
}

fn signal_all(pids: &[i64], sig: libc::c_int) {
    for p in pids {
        os::signal(*p, sig);
    }
}

fn wait_gone(victims: &[i64], grace: Duration) {
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline && victims.iter().any(|p| os::pid_runs(*p)) {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `<sid>` of a `<sid>.claim` file (a take-over's is `<sid>.takeover`).
fn session_of_file(path: &Path) -> Option<String> {
    path.file_name()?
        .to_str()?
        .strip_suffix(".claim")
        .map(str::to_owned)
}

/// Ends the live holders of every claim whose person is one of `persons` (a revoked person and their bots), and their
/// child processes (the launcher's claude, ACP adapters and what they started). A claim without a person is the owner's
/// and is never touched. Each holder is checked first ([`Claim::is_live`]: pid and start time, so a reused pid is left
/// alone). `keep` spares a holder (a narrowed access: those still allowed on their agent). The intent of a session's holder
/// is written before the signal (F12 rule 8), SIGTERM comes first and SIGKILL after `grace`, and the ended holders' claims
/// are given back under their locks and only if still theirs. Blocking. Returns the holders it ended.
pub fn sweep(
    state: &Path,
    persons: &[String],
    keep: &dyn Fn(&Claim) -> bool,
    grace: Duration,
) -> Vec<i64> {
    let mut holders: Vec<(PathBuf, Claim, i64)> = Vec::new();
    for path in claim_files(state) {
        let Some(claim) = read_claim(&path) else {
            continue;
        };
        if let Some(pid) = claim.pid
            && names(&claim, persons)
            && !keep(&claim)
            && claim.is_live()
        {
            holders.push((path, claim, pid));
        }
    }
    let me = i64::from(std::process::id());
    let kids = kids_table();
    let mut victims: Vec<i64> = Vec::new();
    for (path, _, pid) in &holders {
        if *pid == me || *pid <= 1 {
            continue;
        }
        // A session's holder gets its intent before the signal (F12 rule 8); a proc claim names no session.
        if let Some(sid) = session_of_file(path)
            && !sid.starts_with("proc-")
        {
            let sid = sid.strip_suffix(".takeover").unwrap_or(&sid);
            intent::write_intent(state, sid, "access revoked", *pid);
        }
        // The claim's pid is the writing process (a one-shot's launcher shell exec'd claude); its children are what it
        // started.
        for p in descendants(&kids, *pid).into_iter().chain([*pid]) {
            if p != me && !victims.contains(&p) {
                victims.push(p);
            }
        }
    }
    signal_all(&victims, os::SIGTERM);
    wait_gone(&victims, grace);
    let left: Vec<i64> = victims
        .iter()
        .copied()
        .filter(|p| os::pid_runs(*p))
        .collect();
    signal_all(&left, os::SIGKILL);
    let dir = registry_dir(state);
    let mut ended = Vec::new();
    for (path, claim, pid) in holders {
        if pid == me {
            continue;
        }
        // The holder got SIGKILL: its claim is stale. Given back under the claim lock and only if it is still the
        // holder's: a run waiting on the session may have taken it in the meantime, and a plain remove would delete that
        // run's claim, leaving two writers.
        if let Some(sid) = session_of_file(&path) {
            release_in(
                &dir,
                sid.strip_suffix(".takeover").unwrap_or(&sid),
                pid,
                claim.takeover,
            );
        }
        ended.push(pid);
    }
    ended
}

/// What [`stop_holder`] came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Stopped {
    /// No live claim (none, or a stale one whose pid is gone or was reused): nothing was signalled.
    Nobody,
    /// The host's own turn holds it (the claimer is a child of this host): its Stop ends it, not this.
    OwnTurn(Claim),
    /// The claimer and its children were ended.
    Ended(Claim),
}

/// Ends the live holder of `sid` (the owner's Stop for a session held by an old run): SIGTERM to the claimer and what it
/// started (three levels down), SIGKILL for what is still there after `grace`. The claim is checked first
/// ([`Claim::is_live`]: the pid runs AND started when the claim says, so a reused pid is left alone) and the kill goes to
/// that pid and its children only. The intent is written before the signal (F12 rule 8), and the claim is given back after.
pub fn stop_holder(state: &Path, sid: &str, grace: Duration) -> Stopped {
    let Some(claim) = writer_holder(state, sid) else {
        return Stopped::Nobody;
    };
    let Some(pid) = claim.pid else {
        return Stopped::Nobody;
    };
    if os::parent_of(pid) == Some(i64::from(std::process::id())) {
        return Stopped::OwnTurn(claim);
    }
    let mut victims = children_of(&kids_table(), pid, 3);
    victims.insert(0, pid);
    intent::write_intent(state, sid, "stopped from the app", pid);
    signal_all(&victims, os::SIGTERM);
    wait_gone(&victims, grace);
    let left: Vec<i64> = victims
        .iter()
        .copied()
        .filter(|p| os::pid_runs(*p))
        .collect();
    signal_all(&left, os::SIGKILL);
    release_in(&registry_dir(state), sid, pid, claim.takeover);
    Stopped::Ended(claim)
}
