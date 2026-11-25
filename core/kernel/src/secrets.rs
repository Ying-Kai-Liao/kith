//! Secrets, the read half: who may use which agent, and with what settings. A port of `kernel/bin/kith-auth`'s access
//! decision and the one write the launch makes, `take_once` (it uses up a "just this once" helper grant). The rules
//! themselves (`canonical`, `cut_off`, `may`, `resolve`, ...) live in the portable [`crate::access`], which this module
//! re-exports whole, so no caller changed; what stays here is the Unix file protocol around them. The Python tool stays the
//! source of truth while the Python path is the default; `kernel/tests/difftest` fails when the two differ.
//!
//! auth.json is a file the host and `kith-auth` write too, so this module implements its protocol exactly: the same lock
//! (`auth.json.lock`, `flock` exclusive, held from the read of a read-modify-write until the transaction ends), the same
//! atomic write (a `0600` temp file in the same folder, fsynced, renamed over, the folder fsynced), the same tables and
//! the same filtering on save. The access decision (`may`, `resolve`) reads no secret value and never touches the Keychain.
//! The two verbs that start servers do (retire-python P1-15): `mcp-config` writes the `--mcp-config` of an agent's granted
//! shared services and `exec` starts one with its keys in its environment only, reading a value where its profile says
//! (`/usr/bin/security` for the Keychain, a variable, a file). Every verb reads the tables through [`Authority`]: the file,
//! or on the authority tombstone the core, asked in process.
//!
//! The file is read as loosely as Python reads it: a table or row of the wrong type counts as missing, where Python would
//! stop with a traceback.

use fs2::FileExt as _;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

mod authority;
#[cfg(test)]
mod authority_tests;
mod cmd;
mod exec;
#[cfg(test)]
mod exec_tests;
mod grants;
mod guest;
#[cfg(test)]
mod kit;
mod mcp;
#[cfg(test)]
mod mcp_tests;
pub mod pyjson;
mod session;
#[cfg(test)]
mod tests;

pub use authority::{Authority, install_core};
pub use cmd::run_command_with_timeout;
pub use mcp::{McpConfig, Starter, exec_program, mcp_config};
// The access rules, every name they had here (the private ones for the children of this module).
pub use crate::access::*;

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
            packages: Packages::Disk,
        }
    }
}

/// Whether this process is inside the guest sandbox: the account's home folder (by the OS, not `$HOME`) can't be listed.
/// No answer is a guest, as `kith_paths.guest_sandbox` has it. For the host's owner-only commands.
pub fn guest_sandbox() -> bool {
    os_home().is_none_or(|h| std::fs::read_dir(h).is_err())
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

// --- auth.json: the file protocol -------------------------------------------------------------------------------------

/// auth.json as it is on disk: a missing file is empty; a damaged one is a `die` with Python's own words for what is wrong
/// with the JSON. Nothing is filled in and a tombstone is not looked for.
fn read_json(path: &Path) -> R<Value> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            match serde_json::from_str::<Value>(&text) {
                Ok(v) => Ok(v),
                Err(e) => {
                    let why = pyjson::error_of(&text).unwrap_or_else(|| e.to_string());
                    die(format!("{}: invalid JSON ({why})", path.display()))
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Value::Object(Map::new())),
        Err(e) => die(format!("{}: {e}", path.display())),
    }
}

/// `data` with every table present (`load_auth`'s `setdefault`s); it must be an object.
fn fill_tables(path: &Path, mut data: Value) -> R<Value> {
    let Some(obj) = data.as_object_mut() else {
        return die(format!("{}: not a JSON object", path.display()));
    };
    for t in TABLES {
        obj.entry(t).or_insert_with(|| Value::Object(Map::new()));
    }
    Ok(data)
}

/// `kith-auth`'s `load_auth`: auth.json with every table present, or the tombstone's refusal.
pub fn read_auth(path: &Path) -> R<Value> {
    let data = read_json(path)?;
    if let Some(d) = tombstone_die(path, &data) {
        return Err(d);
    }
    fill_tables(path, data)
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
    // The two that start servers (retire-python P1-15): the config of the granted shared services, and the start of one.
    match verb {
        "mcp-config" => return mcp::run(c, args),
        "exec" => return exec::run(c, args),
        _ => {}
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
    let authority = Authority::open(c)?;
    let via = opts
        .get("--via")
        .map(String::as_str)
        .filter(|v| !v.is_empty());
    if authority.is_core() {
        // The tombstone: the core answers, with the params `kith-auth` routes. Never the replaced file.
        if !resolve && rest.len() == 2 {
            return authority.may(rest[0], rest[1], via, once);
        }
        if resolve && rest.len() == 1 {
            let person = opts
                .get("--person")
                .filter(|p| !p.is_empty())
                .or(c.person.as_ref());
            return authority.resolve(rest[0], person.map(String::as_str), via, once);
        }
        return Err(Die {
            msg: format!("{verb}: see kith-auth --help"),
            code: 2,
        });
    }
    let auth = authority.tables()?;
    let rules = Rules::new(c, &auth);
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
