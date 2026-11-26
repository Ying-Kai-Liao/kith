//! The access rules, portable: who may use which agent and with what settings, over one `auth.json` snapshot held as JSON.
//! A port of `kernel/bin/kith-auth`'s access decision (`canonical`, `cut_off`, `may`, `resolve`, `resolve_bot`, `owner_cut`)
//! and the one write a launch makes, `take_once`. Nothing here reads or writes auth.json, takes its lock, asks the password
//! database or runs `kith-auth`: that file protocol is `kith_kernel::secrets`' (Unix only), which re-exports all of this so
//! its callers need not know where it lives. The core's authority runs these same functions (rule WI14: one copy).
//!
//! The package facts the rules ask for (where an agent's package is, its jobs and capabilities) are [`Packages`] and `jobs`.

use serde_json::{Map, Value};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

mod jobs;
mod packages;
mod rules;
#[cfg(test)]
pub(crate) mod tests;

pub use jobs::{Job, PIECES, effective, valid_domain};
pub use packages::{
    AgentFacts, Found, PackageSource, Packages, note_disk_read, on_disk_read, read_names,
};
pub use rules::Rules;
// Only the Unix exec path (`secrets/exec.rs`) names these; the Windows build has no caller until W22b.
#[cfg_attr(not(unix), allow(unused_imports))]
pub(crate) use rules::{access_of, row_of};

/// A refusal `kith-auth` would `die` with: its message and exit code.
#[derive(Debug, PartialEq, Eq)]
pub struct Die {
    pub msg: String,
    pub code: i32,
}

pub(crate) type R<T> = Result<T, Die>;

pub(crate) fn die<T>(msg: impl Into<String>) -> R<T> {
    Err(Die {
        msg: msg.into(),
        code: 1,
    })
}

/// The tables `load_auth` gives every auth.json, in order.
#[cfg_attr(not(unix), allow(dead_code))] // only the Unix `fill_tables` reads it off tests
pub(crate) const TABLES: [&str; 9] = [
    "profiles", "grants", "env", "people", "presets", "members", "invites", "retired", "once",
];
/// The membership row for every person kith knows.
pub(crate) const EVERYONE: &str = "*";
/// Modes that run any command without asking.
pub(crate) const FREE_SHELL: [&str; 2] = ["bypassPermissions", "auto"];
/// How deep `may` and `resolve` follow a chain of bots before they give up (Python's recursion limit stops a loop).
pub(crate) const DEPTH: u32 = 200;

// --- where things are -------------------------------------------------------------------------------------------------

/// How the folder of kith serve's `cloud-sync.json` is found.
#[derive(Clone, Debug)]
pub enum ServeDir {
    /// This folder.
    Fixed(PathBuf),
    /// `<kernel>/bin/kith-serve endpoint` names it (`KITH_SERVE_DIR` already did not), else `<home>/.kith/serve`.
    Endpoint { kernel: PathBuf, home: PathBuf },
}

/// Whether this process is inside the OS sandbox the launch puts around a guest's run.
#[derive(Clone, Copy, Debug)]
pub enum Sandbox {
    Fixed(bool),
    /// Ask the OS: the sandbox closes the account's home folder to reads, so listing it fails. Any doubt is a guest.
    Os,
}

/// What the module needs to know about this run.
#[derive(Clone, Debug)]
pub struct Ctx {
    /// `KITH_AUTH`, else `<secrets>/auth.json`.
    pub auth: PathBuf,
    /// `KITH_AGENTS`: the user's agents.
    pub agents: PathBuf,
    /// The kernel folder: it ships agents of its own.
    pub kernel: PathBuf,
    /// `KITH_PERSON`: the person the run is for; unset or empty is the owner.
    pub person: Option<String>,
    /// Milliseconds since the epoch.
    pub now_ms: f64,
    pub sandbox: Sandbox,
    pub serve: ServeDir,
    /// Where the facts of the agents' package folders come from: the disk, or the daemon's cache.
    pub packages: Packages,
}

impl Ctx {
    pub(crate) fn serve_dir(&self) -> PathBuf {
        match &self.serve {
            ServeDir::Fixed(d) => d.clone(),
            ServeDir::Endpoint { kernel, home } => {
                let mut cmd = Command::new(kernel.join("bin/kith-serve"));
                cmd.arg("endpoint");
                let asked = crate::secrets::run_command_with_timeout(cmd, Duration::from_secs(4))
                    .and_then(|out| {
                        let words: Vec<&str> = out.split_whitespace().collect();
                        (words.len() == 2).then(|| PathBuf::from(words[0]))
                    });
                asked.unwrap_or_else(|| home.join(".kith/serve"))
            }
        }
    }

    pub(crate) fn in_guest_sandbox(&self) -> bool {
        match self.sandbox {
            Sandbox::Fixed(b) => b,
            Sandbox::Os => os_guest_sandbox(),
        }
    }
}

/// Whether this process is inside the guest sandbox, asked of the OS. On Unix the account's home folder (by the password
/// database) can't be listed. Elsewhere there is no such probe yet, and no answer is a guest, as it is in Python.
#[cfg(unix)]
fn os_guest_sandbox() -> bool {
    crate::secrets::guest_sandbox()
}

#[cfg(not(unix))]
fn os_guest_sandbox() -> bool {
    true
}

pub fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
}

// --- reading the JSON as Python does ------------------------------------------------------------

pub(crate) static NULL: Value = Value::Null;

pub(crate) fn get<'a>(v: &'a Value, k: &str) -> &'a Value {
    v.get(k).unwrap_or(&NULL)
}

pub(crate) fn has(v: &Value, k: &str) -> bool {
    v.as_object().is_some_and(|o| o.contains_key(k))
}

pub(crate) fn arr(v: &Value) -> &[Value] {
    v.as_array().map_or(&[], Vec::as_slice)
}

/// Python's truth value.
pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `x.get(k, default)`: the value when the key is there, even a null.
pub(crate) fn get_or(v: &Value, k: &str, default: Value) -> Value {
    v.get(k).cloned().unwrap_or(default)
}

/// `list(dict.fromkeys(items))`.
pub(crate) fn dedupe(items: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(items.len());
    for x in items {
        if !out.contains(&x) {
            out.push(x);
        }
    }
    out
}

pub(crate) fn strings(items: &[String]) -> Vec<Value> {
    items.iter().cloned().map(Value::String).collect()
}

// --- the rules ----------------------------------------------------------------------

/// `canonical`: the person's id now, for an id that may be an old one.
pub fn canonical(auth: &Value, pid: &str) -> String {
    let people = get(auth, "people");
    if has(people, pid) {
        return pid.to_owned();
    }
    people
        .as_object()
        .and_then(|o| {
            o.iter()
                .find(|(_, v)| arr(get(v, "was")).iter().any(|w| w.as_str() == Some(pid)))
        })
        .map_or_else(|| pid.to_owned(), |(p, _)| p.clone())
}

/// The person marked `owner`, if any.
pub fn owner(auth: &Value) -> Option<String> {
    get(auth, "people")
        .as_object()?
        .iter()
        .find(|(_, v)| truthy(get(v, "owner")))
        .map(|(p, _)| p.clone())
}

pub fn is_owner(auth: &Value, person: &str) -> bool {
    person.is_empty() || owner(auth).as_deref() == Some(person)
}

/// The person a bot belongs to (their id as it is now), or `None` for anyone who is not a bot.
pub fn parent_of(auth: &Value, person: &str) -> Option<String> {
    let parent = get(get(get(auth, "people"), person), "bot_of");
    parent
        .as_str()
        .filter(|p| !p.is_empty())
        .map(|p| canonical(auth, p))
}

/// `cut_off`: whether the person, or the person a bot of theirs answers to, is revoked.
pub fn cut_off(auth: &Value, person: &str) -> bool {
    let mut seen: Vec<String> = Vec::new();
    let mut person = Some(person.to_owned());
    while let Some(p) = person.filter(|p| !p.is_empty() && !seen.contains(p)) {
        if truthy(get(get(get(auth, "people"), &p), "revoked")) {
            return true;
        }
        person = parent_of(auth, &p);
        seen.push(p);
    }
    false
}

/// Whether `have` (an access dict) covers value: a path inside a held folder (write covers read), a domain it lists.
pub fn held(have: &Value, kind: &str, value: &Value) -> bool {
    if kind == "domains" {
        return arr(get(have, "domains")).contains(value);
    }
    let Some(value) = value.as_str() else {
        return false;
    };
    let mut roots: Vec<&Value> = arr(get(have, "write")).iter().collect();
    if kind == "read" {
        roots.extend(arr(get(have, "read")));
    }
    roots
        .iter()
        .filter_map(|r| r.as_str())
        .any(|r| value == r || value.starts_with(&format!("{}/", r.trim_end_matches('/'))))
}

/// `take_once`: use up the "just this once" that let this helper launch, in the locked step that ran `may` (`resolve
/// --take-once`). True when one was used. The bot's own row, or the person's, of the caller.
pub fn take_once(auth: &mut Value, person: &str, agent: &str, via: Option<&str>) -> bool {
    let Some(left) = auth
        .get_mut("members")
        .and_then(|m| m.get_mut(via.unwrap_or("")))
        .and_then(|rows| rows.get_mut(person))
        .and_then(|own| own.get_mut("helper_once"))
        .and_then(Value::as_object_mut)
    else {
        return false;
    };
    left.shift_remove(agent).is_some()
}

/// `resolve --take-once` in one place for the CLI and the core: ask again counting a "just this once" grant, and use the
/// grant up in this same step when it let the launch through. The caller holds the auth lock and saves when the second
/// part is true.
pub fn resolve_taking_once(
    c: &Ctx,
    auth: &mut Value,
    agent: &str,
    person: &str,
    via: &str,
) -> Result<(Map<String, Value>, bool), Die> {
    let rules = Rules::new(c, auth);
    let mut res = rules.resolve(agent, person, Some(via), true)?;
    let (allowed, who) = (
        res.get("allowed") == Some(&Value::Bool(true)),
        res.get("person")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    );
    drop(rules);
    if allowed && take_once(auth, &who, agent, Some(via)) {
        let why = format!(
            "{} (just this once)",
            res.get("why").and_then(Value::as_str).unwrap_or_default()
        );
        res.insert("why".to_owned(), why.into());
        return Ok((res, true));
    }
    Ok((res, false))
}
