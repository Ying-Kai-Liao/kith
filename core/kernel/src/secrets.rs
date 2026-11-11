//! Secrets, the read half: who may use which agent, and with what settings. A port of `kernel/bin/kith-auth`'s access
//! decision (`canonical`, `cut_off`, `may`, `resolve`, `resolve_bot`, `owner_cut`) and the one write the launch makes,
//! `take_once` (it uses up a "just this once" helper grant). The Python tool stays the source of truth while the
//! Python path is the default; `kernel/tests/difftest` fails when the two differ.
//!
//! auth.json is a file the host and `kith-auth` write too, so this module implements its protocol exactly: the same lock
//! (`auth.json.lock`, `flock` exclusive, held from the read of a read-modify-write until the transaction ends), the same
//! atomic write (a `0600` temp file in the same folder, fsynced, renamed over, the folder fsynced), the same tables and
//! the same filtering on save. It reads no secret value and never touches the Keychain: this is the access decision only.
//!
//! The file is read as loosely as Python reads it: a table or row of the wrong type counts as missing, where Python would
//! stop with a traceback.

use fs2::FileExt as _;
use serde_json::{Map, Value};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod jobs;
pub mod pyjson;
#[cfg(test)]
mod tests;

use jobs::Job;

/// A refusal `kith-auth` would `die` with: its message and exit code.
#[derive(Debug, PartialEq, Eq)]
pub struct Die {
    pub msg: String,
    pub code: i32,
}

type R<T> = Result<T, Die>;

fn die<T>(msg: impl Into<String>) -> R<T> {
    Err(Die {
        msg: msg.into(),
        code: 1,
    })
}

/// The tables `load_auth` gives every auth.json, in order.
const TABLES: [&str; 9] = [
    "profiles", "grants", "env", "people", "presets", "members", "invites", "retired", "once",
];
/// The membership row for every person kith knows.
const EVERYONE: &str = "*";
/// Modes that run any command without asking.
const FREE_SHELL: [&str; 2] = ["bypassPermissions", "auto"];
/// How deep `may` and `resolve` follow a chain of bots before they give up (Python's recursion limit stops a loop).
const DEPTH: u32 = 200;

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
}

impl Ctx {
    /// This process: its environment, its clock, the given kernel folder.
    pub fn from_process(kernel: &Path) -> Ctx {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let p = crate::paths::from_process(kernel);
        let home = crate::paths::process_home();
        Ctx {
            auth: env("KITH_AUTH").map_or_else(|| p.secrets.join("auth.json"), PathBuf::from),
            agents: p.agents,
            kernel: kernel.to_path_buf(),
            person: env("KITH_PERSON"),
            now_ms: now_ms(),
            sandbox: Sandbox::Os,
            serve: env("KITH_SERVE_DIR").map_or_else(
                || ServeDir::Endpoint {
                    kernel: kernel.to_path_buf(),
                    home,
                },
                |d| ServeDir::Fixed(PathBuf::from(d)),
            ),
        }
    }

    /// Where an agent's package is (`agent_dirs()` in `kith-auth`): `<agent>/role` when that holds an `agent.md`, else the
    /// agent's own folder, in the user's agents then the kernel's; else where it would be.
    fn package_dir(&self, agent: &str) -> PathBuf {
        let plain = !agent.is_empty() && !agent.contains('/') && !agent.starts_with('.');
        if plain {
            for root in [self.agents.clone(), self.kernel.join("agents")] {
                let d = root.join(agent);
                let pkg = if d.join("role/agent.md").is_file() {
                    d.join("role")
                } else {
                    d
                };
                if pkg.join("agent.md").is_file() {
                    return pkg;
                }
            }
        }
        self.agents.join(agent)
    }

    /// Whether `agent` is in `kith-auth`'s `agent_dirs()`: a package with an `agent.md`, in the user's agents or the
    /// kernel's. The owner's writes (M2-2) refuse an agent that is not.
    pub fn agent_exists(&self, agent: &str) -> bool {
        let plain = !agent.is_empty() && !agent.contains('/') && !agent.starts_with('.');
        plain && self.package_dir(agent).join("agent.md").is_file()
    }

    /// `agent_dirs()` in `kith-auth`: the names of the agents that have a package, in the user's agents and the kernel's.
    pub fn agent_names(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for root in [self.agents.clone(), self.kernel.join("agents")] {
            let Ok(dir) = std::fs::read_dir(root) else {
                continue;
            };
            let mut names: Vec<String> = dir
                .filter_map(Result::ok)
                .filter_map(|d| d.file_name().into_string().ok())
                .filter(|n| !n.starts_with('.') && self.agent_exists(n))
                .collect();
            names.sort();
            for n in names {
                if !out.contains(&n) {
                    out.push(n);
                }
            }
        }
        out
    }

    /// `builtin()` in `kith-auth`: one of the kernel's own agents (scratch ...): found under `<kernel>/agents` and not in
    /// the user's agents folder. Its life folder is one for everyone who runs it, so only the owner does.
    pub fn is_builtin(&self, agent: &str) -> bool {
        let has = |root: &Path| {
            let d = root.join(agent);
            !agent.is_empty()
                && !agent.contains('/')
                && !agent.starts_with('.')
                && (d.join("role/agent.md").is_file() || d.join("agent.md").is_file())
        };
        let kernel_agents = self.kernel.join("agents");
        let same = |a: &Path, b: &Path| match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(a), Ok(b)) => a == b,
            _ => a == b,
        };
        !same(&self.agents, &kernel_agents) && !has(&self.agents) && has(&kernel_agents)
    }

    fn serve_dir(&self) -> PathBuf {
        match &self.serve {
            ServeDir::Fixed(d) => d.clone(),
            ServeDir::Endpoint { kernel, home } => {
                let asked = run_with_timeout(
                    &kernel.join("bin/kith-serve"),
                    &["endpoint"],
                    Duration::from_secs(4),
                )
                .and_then(|out| {
                    let words: Vec<&str> = out.split_whitespace().collect();
                    (words.len() == 2).then(|| PathBuf::from(words[0]))
                });
                asked.unwrap_or_else(|| home.join(".kith/serve"))
            }
        }
    }

    fn in_guest_sandbox(&self) -> bool {
        match self.sandbox {
            Sandbox::Fixed(b) => b,
            Sandbox::Os => guest_sandbox(),
        }
    }
}

/// Whether this process is inside the guest sandbox: the account's home folder (by the OS, not `$HOME`) can't be listed.
/// No answer is a guest, as `kith_paths.guest_sandbox` has it. For the host's owner-only commands.
pub fn guest_sandbox() -> bool {
    os_home().is_none_or(|h| std::fs::read_dir(h).is_err())
}

pub fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
}

/// The account's home folder by the OS's password database (`pwd.getpwuid(...).pw_dir`), not `$HOME`: a guest's shell
/// can change that. Read natively (`getpwuid_r`): no program from `PATH`, none to time out into a wrong "guest". `None`
/// when the user has no entry: a guest, as `KeyError` is in Python.
pub(crate) fn os_home() -> Option<PathBuf> {
    crate::threads::os::passwd_home()
}

/// Run a program and read its standard output; `None` when it can't start, outlives `limit`, or fails.
pub(crate) fn run_with_timeout(program: &Path, args: &[&str], limit: Duration) -> Option<String> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    run_command_with_timeout(cmd, limit)
}

/// [`run_with_timeout`] for a command the caller has set up (its environment, say): stdin, stdout and stderr are set
/// here. `None` when it can't start, outlives `limit` (it is killed), or fails.
pub fn run_command_with_timeout(mut cmd: Command, limit: Duration) -> Option<String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut stdout, &mut s);
        s
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed() < limit => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let out = reader.join().ok()?;
    status.success().then_some(out)
}

// --- auth.json: the file protocol -------------------------------------------------------------------------------------

static NULL: Value = Value::Null;

fn get<'a>(v: &'a Value, k: &str) -> &'a Value {
    v.get(k).unwrap_or(&NULL)
}

fn has(v: &Value, k: &str) -> bool {
    v.as_object().is_some_and(|o| o.contains_key(k))
}

fn arr(v: &Value) -> &[Value] {
    v.as_array().map_or(&[], Vec::as_slice)
}

/// Python's truth value.
fn truthy(v: &Value) -> bool {
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
fn get_or(v: &Value, k: &str, default: Value) -> Value {
    v.get(k).cloned().unwrap_or(default)
}

/// `list(dict.fromkeys(items))`.
fn dedupe(items: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(items.len());
    for x in items {
        if !out.contains(&x) {
            out.push(x);
        }
    }
    out
}

fn strings(items: &[String]) -> Vec<Value> {
    items.iter().cloned().map(Value::String).collect()
}

/// `kith-auth`'s `load_auth`: auth.json with every table present. A missing file is empty; a damaged one is a `die`
/// with Python's own words for what is wrong with the JSON.
pub fn read_auth(path: &Path) -> R<Value> {
    let mut data = match std::fs::read(path) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            match serde_json::from_str::<Value>(&text) {
                Ok(v) => v,
                Err(e) => {
                    let why = pyjson::error_of(&text).unwrap_or_else(|| e.to_string());
                    return die(format!("{}: invalid JSON ({why})", path.display()));
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Object(Map::new()),
        Err(e) => return die(format!("{}: {e}", path.display())),
    };
    if let Some(d) = tombstone_die(path, &data) {
        return Err(d);
    }
    let Some(obj) = data.as_object_mut() else {
        return die(format!("{}: not a JSON object", path.display()));
    };
    for t in TABLES {
        obj.entry(t).or_insert_with(|| Value::Object(Map::new()));
    }
    Ok(data)
}

/// `kith_tombstone.Tombstoned`: auth.json is a cutover note (`kith kernel cutover step authority`), the authority moved
/// to the core. Nothing here answers from it or writes over it; exit 3 like the other tombstoned tools.
fn tombstone_die(path: &Path, data: &Value) -> Option<Die> {
    let note = data
        .as_object()
        .filter(|o| o.contains_key("kith_tombstone"))?;
    let module = note.get("module").and_then(Value::as_str).unwrap_or("?");
    let rollback = note
        .get("rollback")
        .and_then(Value::as_str)
        .filter(|r| !r.is_empty())
        .map_or_else(|| format!("kith rollback {module}"), str::to_owned);
    Some(Die {
        msg: format!(
            "{} moved to the Kith core (module {module}); this reader must not use it ({rollback})",
            path.display()
        ),
        code: 3,
    })
}

/// `load_auth()` with no lock: a snapshot for a read.
pub fn load_auth(c: &Ctx) -> R<Value> {
    read_auth(&c.auth)
}

/// An auth.json read under its lock: nobody else writes until this is dropped (the process ends in `kith-auth`, which
/// holds it that long; here, until the transaction is over).
pub struct Locked {
    _lock: File,
    pub auth: Value,
}

/// `load_auth(write=True)`: take `auth.json.lock` (exclusive, waiting for its holder) and read the file.
pub fn load_auth_locked(c: &Ctx) -> R<Locked> {
    let lock = lock_auth(c)?;
    let auth = read_auth(&c.auth)?;
    Ok(Locked { _lock: lock, auth })
}

fn lock_auth(c: &Ctx) -> R<File> {
    if let Some(parent) = c.auth.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Die {
            msg: format!("{}: {e}", parent.display()),
            code: 1,
        })?;
    }
    let mut name = c.auth.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    let lock = OpenOptions::new()
        .append(true)
        .create(true)
        .open(c.auth.with_file_name(name))
        .map_err(|e| Die {
            msg: format!("{}: {e}", c.auth.display()),
            code: 1,
        })?;
    lock.lock_exclusive().map_err(|e| Die {
        msg: format!("{}: {e}", c.auth.display()),
        code: 1,
    })?;
    Ok(lock)
}

/// `save_auth`: invites that have run out and empty tables go, then the file is replaced atomically. Needs the lock.
pub fn save_auth(c: &Ctx, held: &Locked) -> R<()> {
    let fail = |e: std::io::Error| Die {
        msg: format!("{}: {e}", c.auth.display()),
        code: 1,
    };
    let mut data = held.auth.as_object().cloned().unwrap_or_default();
    let now = c.now_ms / 1000.0;
    let live: Map<String, Value> = get(&Value::Object(data.clone()), "invites")
        .as_object()
        .map(|inv| {
            inv.iter()
                .filter(|(_, i)| get(i, "expires").as_f64().unwrap_or(0.0) > now)
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default();
    data.insert("invites".to_owned(), Value::Object(live));
    data.retain(|k, v| truthy(v) || matches!(k.as_str(), "profiles" | "grants" | "env"));
    let text = pyjson::dumps_indent(&Value::Object(data)) + "\n";

    let dir = c
        .auth
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(fail)?;
    let tmp = unique_tmp(&c.auth);
    let write = || -> std::io::Result<()> {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.flush()?;
        f.sync_all()?;
        // Under the lock, right before the replace: a tombstone is never written over.
        if let Some(d) = std::fs::read(&c.auth)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| tombstone_die(&c.auth, &v))
        {
            return Err(std::io::Error::other(Tombstoned(d)));
        }
        std::fs::rename(&tmp, &c.auth)?;
        File::open(dir)?.sync_all()
    };
    write()
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
        .map_err(
            |e| match e.get_ref().and_then(|i| i.downcast_ref::<Tombstoned>()) {
                Some(Tombstoned(d)) => Die {
                    msg: d.msg.clone(),
                    code: d.code,
                },
                None => fail(e),
            },
        )
}

/// A tombstone seen by `save_auth` inside its write closure, carried out as an io error.
#[derive(Debug)]
struct Tombstoned(Die);

impl std::fmt::Display for Tombstoned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.msg)
    }
}

impl std::error::Error for Tombstoned {}

/// A name of its own in the file's folder, as `tempfile.mkstemp(prefix='auth.json.', suffix='.tmp')` makes one.
fn unique_tmp(auth: &Path) -> PathBuf {
    static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let name = auth
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    auth.with_file_name(format!(
        "{name}.{:x}{:x}{n:x}.tmp",
        std::process::id(),
        nanos
    ))
}

// --- the rules ----------------------------------------------------------------------------------------------------------

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

/// `bots_of`: the bots a person owns, sorted.
fn bots_of(auth: &Value, person: &str) -> Vec<String> {
    let mut out: Vec<String> = get(auth, "people")
        .as_object()
        .map(|o| {
            o.iter()
                .filter(|(_, v)| {
                    get(v, "bot_of")
                        .as_str()
                        .is_some_and(|b| !b.is_empty() && canonical(auth, b) == person)
                })
                .map(|(b, _)| b.clone())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// A row of a membership table, or none for a JSON null.
fn not_null(v: &Value) -> Option<&Value> {
    (!v.is_null()).then_some(v)
}

/// `row_of`: the person's membership of agent: their own row, else the agent's `*` row, else none.
fn row_of<'a>(auth: &'a Value, agent: &str, person: &str) -> Option<&'a Value> {
    let rows = get(get(auth, "members"), agent);
    not_null(rows.get(person).unwrap_or_else(|| get(rows, EVERYONE)))
}

fn own_row<'a>(auth: &'a Value, agent: &str, person: &str) -> Option<&'a Value> {
    not_null(get(get(get(auth, "members"), agent), person))
}

/// The person's ticks (`jobs`) on their own row of pkg.
fn ticks<'a>(auth: &'a Value, pkg: &str, person: &str) -> Option<&'a Map<String, Value>> {
    own_row(auth, pkg, person).and_then(|o| get(o, "jobs").as_object())
}

/// The access a membership row holds beyond the box (strings only), as `access_of` builds it.
fn access_of(row: &Value) -> Map<String, Value> {
    let a = get(row, "access");
    ["read", "write", "domains"]
        .into_iter()
        .map(|k| {
            let vs: Vec<Value> = arr(get(a, k))
                .iter()
                .filter(|v| v.is_string())
                .cloned()
                .collect();
            (k.to_owned(), Value::Array(vs))
        })
        .collect()
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

fn free_shell(v: &Value) -> bool {
    v.as_str().is_some_and(|m| FREE_SHELL.contains(&m))
}

/// Whether a helpers list names the agent.
fn calls(helpers: &[Value], agent: &str) -> bool {
    helpers.iter().any(|h| h.as_str() == Some(agent))
}

/// What a run of an agent for a person gets, evaluated over one auth.json snapshot.
pub struct Rules<'a> {
    c: &'a Ctx,
    auth: &'a Value,
    jobs: RefCell<HashMap<String, Rc<Vec<Job>>>>,
    depth: Cell<u32>,
}

impl<'a> Rules<'a> {
    pub fn new(c: &'a Ctx, auth: &'a Value) -> Self {
        Rules {
            c,
            auth,
            jobs: RefCell::default(),
            depth: Cell::new(0),
        }
    }

    fn deeper(&self) -> R<()> {
        self.depth.set(self.depth.get() + 1);
        if self.depth.get() > DEPTH {
            return die("maximum recursion depth exceeded");
        }
        Ok(())
    }

    /// `jobs_of`: the jobs the agent's package declares.
    fn jobs_of(&self, pkg: &str) -> Rc<Vec<Job>> {
        let mut cache = self.jobs.borrow_mut();
        Rc::clone(cache.entry(pkg.to_owned()).or_insert_with(|| {
            Rc::new(jobs::read(
                &self.c.package_dir(pkg).join("capabilities.yaml"),
            ))
        }))
    }

    /// Whether the agent's package declares a job of this name (capabilities.yaml `jobs:`).
    pub fn declares_job(&self, agent: &str, name: &str) -> bool {
        self.jobs_of(agent).iter().any(|j| j.name == name)
    }

    /// `acting`: the person this run works for; never the owner inside a guest's sandbox: then it is no one (`-`).
    pub fn acting(&self) -> String {
        let named = self
            .c
            .person
            .clone()
            .filter(|p| !p.is_empty())
            .or_else(|| owner(self.auth))
            .unwrap_or_default();
        let person = canonical(self.auth, &named);
        if is_owner(self.auth, &person) && self.c.in_guest_sandbox() {
            "-".to_owned()
        } else {
            person
        }
    }

    /// `job_grants`: what the person's ticks on their own row of pkg grant, each cut to the package as it is now.
    fn job_grants(&self, pkg: &str, person: &str) -> [Vec<Value>; 3] {
        let mut out: [Vec<Value>; 3] = Default::default();
        if person == EVERYONE {
            return out;
        }
        let Some(ticks) = ticks(self.auth, pkg, person) else {
            return out;
        };
        for (name, tick) in ticks {
            let jobs = self.jobs_of(pkg);
            let granted = jobs::effective(tick, jobs.iter().find(|j| j.name == *name));
            for (k, vs) in granted.into_iter().enumerate() {
                for x in vs {
                    if !out[k].contains(&x) {
                        out[k].push(x);
                    }
                }
            }
        }
        out
    }

    /// `helpers_in`: the helpers a run on this membership row (of pkg) may call: the row's list, the person's ticked
    /// jobs' and, with once, an unused "just this once" on their own row that hasn't run out.
    fn helpers_in(&self, pkg: &str, person: &str, row: Option<&Value>, once: bool) -> Vec<Value> {
        let Some(row) = row.filter(|r| r.is_object()) else {
            return Vec::new();
        };
        let mut out: Vec<Value> = arr(get(row, "helpers")).to_vec();
        out.extend(self.job_grants(pkg, person)[1].iter().cloned());
        if once
            && let Some(left) =
                own_row(self.auth, pkg, person).and_then(|o| get(o, "helper_once").as_object())
        {
            for (h, until) in left {
                if until.as_f64().is_some_and(|u| u > self.c.now_ms) {
                    out.push(Value::String(h.clone()));
                }
            }
        }
        dedupe(out)
    }

    /// `may`: (allowed, why). Access is a membership row; a helper is allowed when the caller's membership lists it or a
    /// ticked job of it does (once: or a "just this once" on it, see `helpers_in`).
    pub fn may(
        &self,
        person: &str,
        agent: &str,
        via: Option<&str>,
        once: bool,
    ) -> R<(bool, String)> {
        self.deeper()?;
        let via = via.filter(|v| !v.is_empty());
        let person = canonical(self.auth, person);
        let person = person.as_str();
        if is_owner(self.auth, person) {
            return Ok((true, "owner".to_owned()));
        }
        let entry = get(get(self.auth, "people"), person);
        if !has(get(self.auth, "people"), person) {
            return Ok((
                false,
                format!("'{person}' is not a person in kith (kith-auth people)"),
            ));
        }
        if truthy(get(entry, "revoked")) {
            // before any row, the `*` row included: revoke beats every row, and undo finds them all as they were
            return Ok((false, format!("{person}'s access was removed")));
        }
        if self.c.is_builtin(agent) {
            // before any row, the `*` row and a helper list included: fail closed
            return Ok((
                false,
                format!("{agent} is a built-in agent: only the owner runs it"),
            ));
        }
        if truthy(get(entry, "bot_of")) {
            return self.bot_may(person, agent, via, once);
        }
        if row_of(self.auth, agent, person).is_some() {
            let own = has(get(get(self.auth, "members"), agent), person);
            return Ok((
                true,
                if own { "member" } else { "member (everyone)" }.to_owned(),
            ));
        }
        if let Some(via) = via {
            let helpers = self.helpers_in(via, person, row_of(self.auth, via, person), once);
            if calls(&helpers, agent) {
                return Ok((true, format!("helper of {via}")));
            }
        }
        Ok((
            false,
            format!(
                "{person} may not use {agent}{}",
                via.map_or(String::new(), |v| format!(" (not in their {v} helpers)"))
            ),
        ))
    }

    /// `bot_row`: the bot's own membership row for agent, else the caller's (a helper runs with the caller's row). The
    /// `*` row never reaches a bot: a bot gets an agent only by name.
    fn bot_row(&self, agent: &str, bot: &str, via: Option<&str>, once: bool) -> Option<&'a Value> {
        if let Some(own) = own_row(self.auth, agent, bot) {
            return Some(own);
        }
        let via = via?;
        let theirs = own_row(self.auth, via, bot);
        let helpers = self.helpers_in(via, bot, theirs, once);
        if calls(&helpers, agent) { theirs } else { None }
    }

    /// `bot_may`: a bot may use an agent only with a row of its own AND while its owner may use it right now.
    fn bot_may(&self, bot: &str, agent: &str, via: Option<&str>, once: bool) -> R<(bool, String)> {
        let parent = parent_of(self.auth, bot);
        let Some(parent) = parent.filter(|p| has(get(self.auth, "people"), p)) else {
            let raw = get(get(get(self.auth, "people"), bot), "bot_of")
                .as_str()
                .unwrap_or_default()
                .to_owned();
            return Ok((false, format!("{bot}'s owner '{raw}' is gone")));
        };
        if self.bot_row(agent, bot, via, once).is_none() {
            return Ok((false, format!("{bot} may not use {agent}")));
        }
        if !self.may(&parent, agent, via, false)?.0 {
            return Ok((
                false,
                format!("{bot} may not use {agent}: its owner {parent} may not"),
            ));
        }
        Ok((true, format!("bot of {parent}")))
    }

    /// `preset`: a named bundle, from auth.json or the built-in `guest` and `member`.
    fn preset(&self, name: &Value) -> R<Value> {
        let key = name.as_str().unwrap_or_default();
        let own = get(get(self.auth, "presets"), key);
        if truthy(own) {
            return Ok(own.clone());
        }
        match key {
            "guest" => Ok(serde_json::json!({"mode": "default"})),
            "member" => Ok(serde_json::json!({"mode": "acceptEdits"})),
            _ => die(format!(
                "no preset '{key}' (kith-auth preset {key} mode=..)"
            )),
        }
    }

    /// `agent_domains`: the hosts every guest of `agent` may reach, whoever they are: the ones its package declares
    /// (`guest_domains:` in capabilities.yaml, the owner's consent at install) and the ones the owner kept for everyone
    /// on a card (`agent_access`). Plain host names only.
    pub fn agent_domains(&self, agent: &str) -> Vec<String> {
        let mut all = jobs::guest_domains(&self.c.package_dir(agent).join("capabilities.yaml"));
        if let Some(kept) = get(get(get(self.auth, "agent_access"), agent), "domains").as_array() {
            all.extend(kept.iter().filter_map(Value::as_str).map(str::to_owned));
        }
        let mut out: Vec<String> = Vec::new();
        for d in all {
            if jobs::valid_domain(&d) && !out.contains(&d) {
                out.push(d);
            }
        }
        out
    }

    /// `with_jobs`: the run's access with its ticked jobs' domains added, its doors and the names of its ticks, all from
    /// the person's own row of pkg.
    fn with_jobs(
        &self,
        pkg: &str,
        person: &str,
        mut access: Map<String, Value>,
        agent: &str,
    ) -> Map<String, Value> {
        let granted = self.job_grants(pkg, person);
        let mut domains: Vec<Value> = arr(access.get("domains").unwrap_or(&NULL)).to_vec();
        domains.extend(granted[0].iter().cloned());
        domains.extend(self.agent_domains(agent).into_iter().map(Value::String));
        access.insert("domains".to_owned(), Value::Array(dedupe(domains)));
        let mut names: Vec<String> = Vec::new();
        if let Some(ticks) = ticks(self.auth, pkg, person)
            && !ticks.is_empty()
        {
            let jobs = self.jobs_of(pkg);
            names = ticks
                .keys()
                .filter(|n| jobs.iter().any(|j| j.name == **n))
                .cloned()
                .collect();
            names.sort();
        }
        let mut out = Map::new();
        out.insert("access".to_owned(), Value::Object(access));
        out.insert("doors".to_owned(), Value::Array(granted[2].clone()));
        out.insert("jobs".to_owned(), Value::Array(strings(&names)));
        out
    }

    /// `resolve`: what a run of agent for person gets: whether it may run, and the person's settings over the agent's. A
    /// helper reached through the caller's helpers list runs with the caller's membership, so calling one never loosens it.
    pub fn resolve(
        &self,
        agent: &str,
        person: &str,
        via: Option<&str>,
        once: bool,
    ) -> R<Map<String, Value>> {
        self.deeper()?;
        let via = via.filter(|v| !v.is_empty());
        let person = canonical(self.auth, person);
        let person = person.as_str();
        let (ok, why) = self.may(person, agent, via, once)?;
        let me = get(get(self.auth, "people"), person);
        let mut out = Map::new();
        out.insert("person".to_owned(), person.into());
        out.insert("owner".to_owned(), is_owner(self.auth, person).into());
        out.insert("allowed".to_owned(), ok.into());
        out.insert("why".to_owned(), why.into());
        for k in ["name", "lang", "tz"] {
            if let Some(v) = me.get(k) {
                out.insert(k.to_owned(), v.clone());
            }
        }
        if !ok || out["owner"] == Value::Bool(true) {
            return Ok(out);
        }
        if truthy(get(me, "bot_of")) {
            return self.resolve_bot(agent, person, via, out, once);
        }
        let (row, pkg) = match row_of(self.auth, agent, person) {
            Some(r) => (r, agent),
            None => match via.and_then(|v| row_of(self.auth, v, person).map(|r| (r, v))) {
                Some(found) => found,
                None => return die(format!("{person} has no membership of {agent}")),
            },
        };
        let p = self.preset(&get_or(row, "preset", "guest".into()))?;
        let mode = {
            let asked = get(row, "mode");
            let mode = if truthy(asked) {
                asked.clone()
            } else {
                get_or(&p, "mode", "default".into())
            };
            let trusted = if truthy(asked) {
                get(row, "trusted")
            } else {
                get(&p, "trusted")
            };
            if free_shell(&mode) && !truthy(trusted) {
                Value::from("default")
            } else {
                mode
            }
        };
        let deny = dedupe([arr(get(&p, "deny")), arr(get(row, "deny"))].concat());
        let row_agent = row_of(self.auth, agent, person).is_some();
        out.insert("preset".to_owned(), get_or(row, "preset", "guest".into()));
        out.insert("mode".to_owned(), mode.clone());
        out.insert("deny".to_owned(), Value::Array(deny));
        out.insert("model".to_owned(), get_or(row, "model", "".into()));
        out.insert(
            "env".to_owned(),
            get_or(row, "env", Value::Object(Map::new())),
        );
        out.insert(
            "helpers".to_owned(),
            Value::Array(self.helpers_in(pkg, person, Some(row), false)),
        );
        out.insert("budget".to_owned(), get_or(row, "budget", "".into()));
        out.insert(
            "budget_agent".to_owned(),
            if row_agent {
                agent.into()
            } else {
                via.map_or(Value::Null, Value::from)
            },
        );
        out.insert(
            "was".to_owned(),
            get_or(me, "was", Value::Array(Vec::new())),
        );
        out.insert(
            "trusted".to_owned(),
            (truthy(get(row, "trusted")) || truthy(get(&p, "trusted"))).into(),
        );
        out.extend(self.with_jobs(pkg, person, access_of(row), agent));
        let asked = if truthy(get(row, "mode")) {
            get(row, "mode")
        } else {
            get(&p, "mode")
        };
        if free_shell(asked) && &mode != asked {
            out.insert("downgraded".to_owned(), asked.clone());
        }
        Ok(out)
    }

    /// `resolve_bot`: a bot's run: a guest, inside what its owner gets. It spends from its owner's budget, counted with
    /// everything else the owner's line holds (`was`).
    fn resolve_bot(
        &self,
        agent: &str,
        bot: &str,
        via: Option<&str>,
        mut out: Map<String, Value>,
        once: bool,
    ) -> R<Map<String, Value>> {
        let Some(row) = self.bot_row(agent, bot, via, once) else {
            return die(format!("{bot} has no membership of {agent}"));
        };
        let pkg = if own_row(self.auth, agent, bot).is_some() {
            agent
        } else {
            via.unwrap_or_default()
        };
        let parent = parent_of(self.auth, bot).unwrap_or_default();
        let pr = self.resolve(agent, &parent, via, false)?;
        let p = self.preset(&"guest".into())?;
        let owner_parent = is_owner(self.auth, &parent);
        let mut helpers = self.helpers_in(pkg, bot, Some(row), false);
        if !owner_parent {
            let theirs = arr(pr.get("helpers").unwrap_or(&NULL)).to_vec();
            helpers.retain(|h| theirs.contains(h));
        }
        let mut was: Vec<Value> = Vec::new();
        for b in bots_of(self.auth, &parent).into_iter().filter(|b| b != bot) {
            was.push(Value::String(b.clone()));
            was.extend(
                arr(get(get(get(self.auth, "people"), &b), "was"))
                    .iter()
                    .cloned(),
            );
        }
        let (budget, budget_agent) = if owner_parent {
            // the owner's own runs are uncapped; the optional cap covers their bots together and leaves out their own runs
            let cap = get(self.auth, "owner_bots_budget");
            (
                if truthy(cap) { cap.clone() } else { "".into() },
                Value::from(""),
            )
        } else {
            let mut all: Vec<Value> = vec![Value::String(parent.clone())];
            all.extend(
                arr(get(get(get(self.auth, "people"), &parent), "was"))
                    .iter()
                    .cloned(),
            );
            all.extend(arr(pr.get("was").unwrap_or(&NULL)).iter().cloned());
            all.extend(was);
            was = all;
            (
                pr.get("budget").cloned().unwrap_or_else(|| "".into()),
                pr.get("budget_agent").cloned().unwrap_or_else(|| "".into()),
            )
        };
        let mode = {
            let m = get(&p, "mode");
            if free_shell(m) && !truthy(get(&p, "trusted")) {
                Value::from("default")
            } else {
                get_or(&p, "mode", "default".into())
            }
        };
        let deny = dedupe(
            [
                arr(get(&p, "deny")),
                arr(get(row, "deny")),
                arr(pr.get("deny").unwrap_or(&NULL)),
            ]
            .concat(),
        );
        let model = if truthy(get(row, "model")) {
            get(row, "model").clone()
        } else {
            pr.get("model").cloned().unwrap_or_else(|| "".into())
        };
        out.insert("preset".to_owned(), "guest".into());
        out.insert("mode".to_owned(), mode);
        out.insert("deny".to_owned(), Value::Array(deny));
        out.insert("model".to_owned(), model);
        out.insert(
            "env".to_owned(),
            get_or(row, "env", Value::Object(Map::new())),
        );
        out.insert("helpers".to_owned(), Value::Array(helpers));
        out.insert("budget".to_owned(), budget);
        out.insert("budget_agent".to_owned(), budget_agent);
        out.insert("was".to_owned(), Value::Array(dedupe(was)));
        let grants = self.with_jobs(pkg, bot, access_of(row), agent);
        out.extend(self.owner_cut(agent, bot, grants, via, Some(&pr))?);
        Ok(out)
    }

    /// `owner_cut`: a bot's grants (any of access, doors, jobs) never more than its owner holds: each cut to what the
    /// owner's run of agent (through via) has. The owner's own bots are capped by nothing.
    fn owner_cut(
        &self,
        agent: &str,
        bot: &str,
        grants: Map<String, Value>,
        via: Option<&str>,
        pr: Option<&Map<String, Value>>,
    ) -> R<Map<String, Value>> {
        let parent = parent_of(self.auth, bot).unwrap_or_default();
        if is_owner(self.auth, &parent) {
            return Ok(grants);
        }
        let own;
        let pr = if let Some(p) = pr {
            p
        } else {
            own = self.resolve(agent, &parent, via, false)?;
            &own
        };
        let mut out = grants;
        if let Some(access) = out.get("access").and_then(Value::as_object) {
            let theirs = pr.get("access").unwrap_or(&NULL);
            let cut: Map<String, Value> = access
                .iter()
                .map(|(k, vs)| {
                    (
                        k.clone(),
                        Value::Array(
                            arr(vs)
                                .iter()
                                .filter(|v| held(theirs, k, v))
                                .cloned()
                                .collect(),
                        ),
                    )
                })
                .collect();
            out.insert("access".to_owned(), Value::Object(cut));
        }
        for k in ["doors", "jobs"] {
            if let Some(vs) = out.get(k) {
                let theirs = arr(pr.get(k).unwrap_or(&NULL));
                let kept: Vec<Value> = arr(vs)
                    .iter()
                    .filter(|x| theirs.contains(x))
                    .cloned()
                    .collect();
                out.insert(k.to_owned(), Value::Array(kept));
            }
        }
        Ok(out)
    }

    /// `kith-auth who <platform> <user-id>` (M2-23): which person owns a chat handle, with the owner, whether the owner is
    /// linked on that platform, and the revoke and held flags the channels gate on.
    pub fn who(&self, platform: &str, uid: &str) -> R<Value> {
        // A database imported before `people.handles` existed has none for anyone: a chat sender would match nobody (or the
        // wrong person), so it says so and refuses until the import is run again.
        if truthy(get(self.auth, "handles_stale")) {
            return die(
                "cannot match a chat handle to a person: this database was imported before people.handles existed; import again first",
            );
        }
        let people = get(self.auth, "people");
        let pid = people.as_object().and_then(|o| {
            o.iter()
                .find(|(_, v)| get(get(v, "handles"), platform).as_str() == Some(uid))
                .map(|(p, _)| p.clone())
        });
        let me = owner(self.auth);
        let flag = |f: &dyn Fn(&str) -> bool| pid.as_deref().is_some_and(f);
        let mut out = Map::new();
        out.insert(
            "person".into(),
            pid.clone().map_or(Value::Null, Value::String),
        );
        out.insert("owner".into(), flag(&|p| is_owner(self.auth, p)).into());
        out.insert(
            "was".into(),
            pid.as_deref().map_or_else(
                || Value::Array(Vec::new()),
                |p| get_or(get(people, p), "was", Value::Array(Vec::new())),
            ),
        );
        out.insert(
            "owner_id".into(),
            me.clone().map_or(Value::Null, Value::String),
        );
        out.insert(
            "owner_linked".into(),
            me.as_deref()
                .is_some_and(|m| truthy(get(get(get(people, m), "handles"), platform)))
                .into(),
        );
        out.insert("revoked".into(), flag(&|p| cut_off(self.auth, p)).into());
        out.insert("held".into(), flag(&|p| self.bridge_held(p)).into());
        Ok(Value::Object(out))
    }

    /// `kith-auth granted [--for AGENT] <service>`: the profiles an agent holds for a service, as written (one name or a
    /// list).
    pub fn granted(&self, agent: &str, service: &str) -> R<Vec<Value>> {
        // `grants` is filled by the authority import; a database imported before it did has none, which says nothing.
        if truthy(get(self.auth, "secrets_stale")) {
            return die(
                "cannot read the grants: this database was imported before they were kept; import again first",
            );
        }
        let g = get(get(get(self.auth, "grants"), agent), service);
        Ok(match g {
            Value::Array(items) => items.clone(),
            g if truthy(g) => vec![g.clone()],
            _ => Vec::new(),
        })
    }

    /// `cloud_sync` and `bridge_held`: kith serve's channels serve a person other than the owner only while this Mac is
    /// in sync with the Kith cloud; a linked Mac whose host hasn't renewed the lease lately holds them.
    pub fn bridge_held(&self, person: &str) -> bool {
        if person.is_empty() || is_owner(self.auth, person) {
            return false;
        }
        let Ok(text) = std::fs::read_to_string(self.c.serve_dir().join("cloud-sync.json")) else {
            return false;
        };
        let Ok(Value::Object(s)) = serde_json::from_str::<Value>(&text) else {
            return false;
        };
        let linked = s.get("linked").is_some_and(truthy);
        let until = match s.get("until") {
            None | Some(Value::Null | Value::Bool(false)) => Some(0.0),
            Some(Value::Bool(true)) => Some(1.0),
            Some(Value::Number(n)) => n.as_f64(),
            Some(Value::String(t)) if t.is_empty() => Some(0.0),
            Some(Value::String(t)) => t.trim().parse::<f64>().ok(),
            Some(other) => (!truthy(other)).then_some(0.0),
        };
        // a value Python's `float()` refuses reads as "never linked"
        until.is_some_and(|u| linked && u <= self.c.now_ms)
    }
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

// --- the command line ---------------------------------------------------------------------------------------------------

/// What a `kith-auth` command prints and exits with.
#[derive(Debug, PartialEq, Eq)]
pub struct Outcome {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

impl Outcome {
    fn out(text: String, code: i32) -> Outcome {
        Outcome {
            stdout: text + "\n",
            stderr: String::new(),
            code,
        }
    }

    fn died(d: &Die) -> Outcome {
        Outcome {
            stdout: String::new(),
            stderr: format!("kith-auth: {}\n", d.msg),
            code: d.code,
        }
    }
}

/// `kith-auth may ...` and `kith-auth resolve ...`: the arguments after the verb, as `kith-auth` reads them.
pub fn run(c: &Ctx, verb: &str, args: &[String]) -> Outcome {
    match command(c, verb, args) {
        Ok(o) => o,
        Err(d) => Outcome::died(&d),
    }
}

fn usage(verb: &str) -> String {
    if verb == "resolve" {
        "usage: kith kernel secrets resolve <agent> [--person ID] [--via AGENT] [--take-once] [--json]".to_owned()
    } else {
        "usage: kith kernel secrets may <id> <agent> [--via AGENT] [--once]".to_owned()
    }
}

/// `kith-auth`'s own usage text (`kith-auth -h`), printed on stdout with exit 0.
const HELP: &str = include_str!("secrets/usage.txt");

fn command(c: &Ctx, verb: &str, args: &[String]) -> R<Outcome> {
    if matches!(verb, "-h" | "--help") {
        return Ok(Outcome {
            stdout: HELP.to_owned(),
            stderr: String::new(),
            code: 0,
        });
    }
    if !matches!(verb, "may" | "resolve") {
        return Err(Die {
            msg: format!("unknown command: {verb} (kith-auth --help)"),
            code: 2,
        });
    }
    let resolve = verb == "resolve";
    // An argument that starts with `-` and is no flag of this verb is usage, never an agent or a person: `--help` must
    // not read auth.json (kith-auth would take it as a name).
    let mut skip = false;
    for a in args {
        if std::mem::take(&mut skip) {
            continue;
        }
        match a.as_str() {
            "--via" | "--person" => skip = true,
            "--take-once" | "--json" if resolve => {}
            "--once" if !resolve => {}
            f if f.starts_with('-') && f != "-" => {
                return Err(Die {
                    msg: usage(verb),
                    code: 2,
                });
            }
            _ => {}
        }
    }
    let once = args
        .iter()
        .any(|a| a == if resolve { "--take-once" } else { "--once" });
    // `kith kernel secrets resolve ... --json` is accepted; the answer is JSON either way
    let (mut opts, mut rest): (HashMap<&str, String>, Vec<&str>) = (HashMap::new(), Vec::new());
    let mut it = args
        .iter()
        .map(String::as_str)
        .filter(|a| !matches!(*a, "--take-once" | "--once") && (!resolve || *a != "--json"));
    while let Some(a) = it.next() {
        if matches!(a, "--via" | "--person") {
            opts.insert(a, it.next().unwrap_or_default().to_owned());
        } else {
            rest.push(a);
        }
    }
    let auth = load_auth(c)?;
    let rules = Rules::new(c, &auth);
    let via = opts
        .get("--via")
        .map(String::as_str)
        .filter(|v| !v.is_empty());
    if !resolve && rest.len() == 2 {
        // --once: a "just this once" counts (kith-permit's ask_helper, so a yes answered late isn't asked again)
        let (ok, why) = rules.may(rest[0], rest[1], via, once)?;
        return Ok(Outcome::out(why, i32::from(!ok)));
    }
    if resolve && rest.len() == 1 {
        let agent = rest[0];
        let person = opts
            .get("--person")
            .filter(|p| !p.is_empty())
            .cloned()
            .unwrap_or_else(|| rules.acting());
        let mut res = rules.resolve(agent, &person, via, false)?;
        let held = rules.bridge_held(&person);
        // not while held: bin/kith asks again until in sync, and the once goes to the launch that runs
        if once && via.is_some() && res["allowed"] == Value::Bool(false) && !held {
            // --take-once (bin/kith's helper launch): a "just this once" lets it run and is used up in one locked step with
            // the check of revoke and access, so a removal between the card and the launch still wins
            drop(rules);
            let mut locked = load_auth_locked(c)?;
            let (r, spent) =
                resolve_taking_once(c, &mut locked.auth, agent, &person, via.unwrap_or_default())?;
            res = r;
            if spent {
                save_auth(c, &locked)?;
            }
        }
        // held: as in `who`, this Mac is out of sync with the Kith cloud and person isn't the owner
        res.insert("held".to_owned(), held.into());
        return Ok(Outcome::out(pyjson::dumps(&Value::Object(res)), 0));
    }
    Err(Die {
        msg: format!("{verb}: see kith-auth --help"),
        code: 2,
    })
}
