//! The run-token table (M0e): who a request on the run socket is, decided by the token alone.
//!
//! A token is minted for one run (agent, person, conversation, and the claim it holds), kept only in memory by the
//! SHA-256 of the token, and honoured for a request only when it is known AND the connecting process is the run's
//! process or one below it (the ancestry check `serve/host/src/runauth.rs` keeps for host.sock: on macOS a process of
//! the same user can read another's environment, so a token alone could be lifted from a neighbouring run).
//!
//! Dies when: the run is ended (`run.end`), the daemon restarts (the table is memory; runs are minted again later), or
//! the run's process is gone or its pid belongs to another process now (the start time taken at bind no longer matches).
//!
//! Also here: the allow-lists for the arguments a run may pass to the three tools it can reach, ported from
//! `runauth.rs` (`recall_args`, `memory_args`, `project_args`). Same rules, same refusal texts.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::error::{Error, Result};
use crate::owner::random_secret;

/// A request line longer than this is refused unread.
pub(crate) const MAX_REQUEST: usize = 64 * 1024;
const MAX_ARGS: usize = 64;
const MAX_ARG_LEN: usize = 4096;
/// How far up the process tree a peer is followed before it counts as not descending.
const MAX_HOPS: usize = 64;
/// More live tokens than this means something leaks them; minting refuses instead of growing without end.
const MAX_RUNS: usize = 4096;

/// Whom a token stands for. Everything the core decides about a run-socket request comes from here, never from its
/// arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunIdentity {
    pub run_id: String,
    pub agent: String,
    pub person: String,
    pub conversation: String,
    /// The writer claim the run holds. Carried, not acted on: claims are M3.
    pub claim: Option<String>,
    pub room: Option<String>,
    /// Fixed when the token is minted; `project-id` answers about this one only.
    pub project: Option<String>,
}

struct Entry {
    identity: RunIdentity,
    /// The process the run was started as, with the start time read at bind. A process whose start time cannot be
    /// read is never bound (Windows until `GetProcessTimes` lands): without it a reused pid would revive the token.
    bound: Option<Bound>,
}

struct Bound {
    pid: u32,
    started: String,
}

/// `(peer, ancestor)`: whether `peer` is `ancestor` or one of its descendants.
pub(crate) type Ancestry = fn(u32, u32) -> bool;
/// The start time of a process, `None` when unknown. A pid that is reused has another one.
pub(crate) type StartTime = fn(u32) -> Option<String>;

/// The live tokens, by the SHA-256 of the token (the map never holds a token itself).
pub(crate) struct RunTokens {
    entries: Mutex<HashMap<[u8; 32], Entry>>,
    ancestry: Ancestry,
    start_time: StartTime,
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

impl RunTokens {
    pub(crate) fn new() -> Self {
        Self::with(descends, crate::lock::process_start)
    }

    /// With the process facts replaced, for tests that use made-up pids.
    pub(crate) fn with(ancestry: Ancestry, start_time: StartTime) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ancestry,
            start_time,
        }
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, HashMap<[u8; 32], Entry>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A fresh token (256 random bits as hex) for `who`; `who.run_id` is filled in here. Returns the token, shown once.
    pub(crate) fn mint(&self, mut who: RunIdentity) -> Result<(String, RunIdentity)> {
        let token = random_secret()?;
        random_secret()?[..32].clone_into(&mut who.run_id);
        let mut entries = self.locked();
        if entries.len() >= MAX_RUNS {
            return Err(Error::Io(
                "kith-core: too many live run tokens; end the runs that are over".into(),
            ));
        }
        entries.insert(
            digest(&token),
            Entry {
                identity: who.clone(),
                bound: None,
            },
        );
        Ok((token, who))
    }

    /// Binds the run to the process it was started as, with that process's start time. False for an unknown run,
    /// and for a process whose
    /// start time cannot be read (the token stays unbound, so no request resolves).
    pub(crate) fn bind(&self, run_id: &str, pid: u32) -> bool {
        let Some(started) = (self.start_time)(pid) else {
            return false;
        };
        let mut entries = self.locked();
        let Some(e) = entries.values_mut().find(|e| e.identity.run_id == run_id) else {
            return false;
        };
        e.bound = Some(Bound { pid, started });
        true
    }

    /// Ends a run's token. False when it was not there (already ended, or the daemon restarted since).
    pub(crate) fn end(&self, run_id: &str) -> bool {
        let mut entries = self.locked();
        let key = entries
            .iter()
            .find(|(_, e)| e.identity.run_id == run_id)
            .map(|(k, _)| *k);
        key.is_some_and(|k| entries.remove(&k).is_some())
    }

    /// `run.mint`, `run.bind` and `run.end`, called by the owner (the owner socket; they are owner-only rows). `None`
    /// for any other method.
    pub(crate) fn owner_call(&self, method: &str, params: &Value) -> Option<Result<Value>> {
        if !matches!(method, "run.mint" | "run.bind" | "run.end") {
            return None;
        }
        Some(
            crate::call::check(kith_api::Principal::Owner, method, params)
                .and_then(|_| self.owner_run(method, params)),
        )
    }

    fn owner_run(&self, method: &str, params: &Value) -> Result<Value> {
        let get = |k: &str| params.get(k).and_then(Value::as_str);
        let opt = |k: &str| get(k).filter(|v| !v.is_empty()).map(str::to_owned);
        let need = |k: &str| {
            opt(k).ok_or_else(|| Error::BadParams(format!("kith-core: {method}: {k} is empty")))
        };
        match method {
            "run.mint" => {
                let (token, who) = self.mint(RunIdentity {
                    run_id: String::new(),
                    agent: need("agent")?,
                    person: need("person")?,
                    conversation: need("conversation")?,
                    claim: opt("claim"),
                    room: opt("room"),
                    project: opt("project"),
                })?;
                Ok(serde_json::json!({"runId": who.run_id, "token": token}))
            }
            "run.bind" => {
                let pid = params
                    .get("pid")
                    .and_then(Value::as_u64)
                    .and_then(|p| u32::try_from(p).ok())
                    .filter(|p| *p > 1)
                    .ok_or_else(|| Error::BadParams("kith-core: run.bind: not a pid".into()))?;
                Ok(serde_json::json!({"bound": self.bind(&need("runId")?, pid)}))
            }
            _ => Ok(serde_json::json!({"ended": self.end(&need("runId")?)})),
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.locked().len()
    }

    /// The one place a token becomes an identity: the token must be live and the peer must be the run's own process
    /// or below it, and that process must still be the one the run started as. Unknown, ended, unbound, foreign
    /// peers and a run whose process is gone are all `None`.
    pub(crate) fn resolve(&self, token: &str, peer: Option<u32>) -> Option<RunIdentity> {
        let key = digest(token);
        let (identity, pid, started) = {
            let entries = self.locked();
            let e = entries.get(&key)?;
            let b = e.bound.as_ref()?;
            (e.identity.clone(), b.pid, b.started.clone())
        };
        // `ps` runs with the table unlocked.
        if !(self.ancestry)(peer?, pid) {
            return None;
        }
        if (self.start_time)(pid).as_deref() != Some(started.as_str()) {
            // The run's process is gone, its pid is another process's now, or its start time cannot be read: the token
            // is dead, drop it.
            self.locked().remove(&key);
            return None;
        }
        Some(identity)
    }
}

/// Whether `pid` is `ancestor` or has it among its parents. Asks `ps`, which both macOS and Linux have; any failure
/// (a process gone, `ps` missing, Windows) is "no". On Windows only the run's own pid matches until a process-tree
/// walk lands there: a grandchild is refused, never let through.
#[cfg(unix)]
pub(crate) fn descends(pid: u32, ancestor: u32) -> bool {
    let mut cur = pid;
    for _ in 0..MAX_HOPS {
        if cur == ancestor {
            return true;
        }
        if cur <= 1 {
            return false;
        }
        let Ok(out) = std::process::Command::new("ps")
            .args(["-o", "ppid=", "-p", &cur.to_string()])
            .output()
        else {
            return false;
        };
        let Some(parent) = String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse::<u32>()
            .ok()
        else {
            return false;
        };
        cur = parent;
    }
    false
}

#[cfg(not(unix))]
pub(crate) fn descends(pid: u32, ancestor: u32) -> bool {
    let _ = MAX_HOPS;
    pid == ancestor
}

// --- the arguments a run may pass ---------------------------------------------------------------------------------

/// The flags of `kith recall` that only narrow a search (the kernel filters by the caller's audience first); who asks
/// is never an argument.
const VALUE_FLAGS: &[&str] = &["--project", "--room", "--thread", "--tier", "-n", "--limit"];

fn text(arg: &Value) -> Option<&str> {
    arg.as_str().filter(|a| a.len() <= MAX_ARG_LEN)
}

/// The query words, `--json` and the narrowing flags, nothing else (a word starting with `-` is not a query). The
/// result starts with a flag, so a query that begins with `index` (or `forget`) can never reach a subcommand.
pub(crate) fn recall_args(args: &[Value]) -> std::result::Result<Vec<String>, &'static str> {
    if args.len() > MAX_ARGS {
        return Err("too many arguments");
    }
    let mut words = Vec::new();
    let (mut json, mut tier, mut limit) = (false, "2".to_owned(), None::<String>);
    let mut flags: Vec<(String, String)> = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let Some(arg) = text(arg) else {
            return Err("bad argument");
        };
        if arg == "--json" {
            json = true;
        } else if VALUE_FLAGS.contains(&arg) {
            let Some(value) = it.next().and_then(text) else {
                return Err("a flag needs a value");
            };
            match arg {
                "--tier" => value.clone_into(&mut tier),
                "-n" | "--limit" => limit = Some(value.to_owned()),
                _ => flags.push((arg.to_owned(), value.to_owned())),
            }
        } else if arg.starts_with('-') {
            return Err("that flag isn't available here");
        } else {
            words.push(arg.to_owned());
        }
    }
    if words.is_empty() {
        return Err("a query is needed");
    }
    let mut out = vec!["--tier".to_owned(), tier];
    if let Some(n) = limit {
        out.extend(["-n".to_owned(), n]);
    }
    for (k, v) in flags {
        out.extend([k, v]);
    }
    if json {
        out.push("--json".into());
    }
    out.extend(words);
    Ok(out)
}

/// `search`, then `-n N` (clamped), `--json` and the query words. `--all`, `--agent` (the agent is the token's) and
/// every other dash word are refused. The result starts with `search`, so no query word reaches another subcommand.
pub(crate) fn memory_args(args: &[Value]) -> std::result::Result<Vec<String>, &'static str> {
    if args.len() > MAX_ARGS {
        return Err("too many arguments");
    }
    let (mut words, mut json, mut limit) = (Vec::new(), false, None::<u32>);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let Some(arg) = text(arg) else {
            return Err("bad argument");
        };
        if arg == "--json" {
            json = true;
        } else if arg == "-n" {
            let n = it
                .next()
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty() && v.len() <= 6);
            let Some(n) = n.filter(|v| v.bytes().all(|b| b.is_ascii_digit())) else {
                return Err("-n needs a number");
            };
            limit = Some(
                n.parse::<u32>()
                    .map_err(|_| "-n needs a number")?
                    .clamp(1, 50),
            );
        } else if arg.starts_with('-') {
            return Err("that flag isn't available here");
        } else {
            words.push(arg.to_owned());
        }
    }
    if words.is_empty() {
        return Err("a query is needed");
    }
    let mut out = vec!["search".to_owned()];
    if let Some(n) = limit {
        out.extend(["-n".to_owned(), n.to_string()]);
    }
    if json {
        out.push("--json".into());
    }
    out.extend(words);
    Ok(out)
}

/// Nothing (the run's own project), or `--info <that project>`. Every other verb, a directory and another project's
/// id are refused. The result is empty or starts with `--info`.
pub(crate) fn project_args(
    args: &[Value],
    project: Option<&str>,
) -> std::result::Result<Vec<String>, &'static str> {
    let args = args.iter().map(text).collect::<Option<Vec<_>>>();
    match args.as_deref() {
        None => Err("bad argument"),
        Some([]) => Ok(Vec::new()),
        Some(["--info", id]) if project == Some(*id) && !id.starts_with('-') => {
            Ok(vec!["--info".to_owned(), (*id).to_owned()])
        }
        Some(_) => Err("only this run's own project is available here"),
    }
}
