//! `kith kernel room-ledger pre|post --agent A --home DIR`: `kernel/bin/kith-room-ledger` in Rust, the Pre/PostToolUse
//! hook for Write, Edit and `MultiEdit` that keeps the room-note ledger (`<state>/room-ledger/<agent>/<room>.json`: note
//! path under `memory/` -> sorted person ids). The Python stays the source of truth while the verb is not switched.
//!
//! The hook input is JSON on stdin; the run's room comes from `KITH_ROOM` and `KITH_ROOM_MEMBERS`, which the host sets
//! and the agent cannot change for its own hook. `pre` notes that the file already existed (no record yet: owner
//! only); `post` narrows the note's record to the people present now (a first write records them). It prints nothing,
//! exits 0 whatever happens (bad arguments, bad stdin, a ledger that cannot be written) and does not look at
//! `KITH_RUN_TOKEN`: a guest run is treated as any other. Unusual ledger content is read as the Python reads it
//! (see `recorded` and `strings_of`).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write as _};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};

use fs2::FileExt as _;
use serde_json::{Map, Value};

use crate::paths::realpath;
use crate::secrets::pyjson;

/// What a run reads from its process: the state folder and the run's room.
pub struct Ctx {
    pub state: PathBuf,
    /// `KITH_ROOM`, `""` when unset.
    pub room: String,
    /// `KITH_ROOM_MEMBERS`, `""` when unset.
    pub members: String,
}

impl Ctx {
    pub fn from_process(state: PathBuf) -> Self {
        let get = |n: &str| {
            std::env::var_os(n)
                .map(|v| v.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        Self {
            state,
            room: get("KITH_ROOM"),
            members: get("KITH_ROOM_MEMBERS"),
        }
    }
}

/// `kith_facets.ID.match(s)`: `[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`, where `$` also matches before one trailing newline.
fn is_id(s: &str) -> bool {
    let s = s.strip_suffix('\n').unwrap_or(s).as_bytes();
    !s.is_empty()
        && s.len() <= 64
        && s[0].is_ascii_alphanumeric()
        && s.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// Python's `str.strip()` (its whitespace also holds the four separators `\x1c`..`\x1f`).
fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c))
}

/// The people in this run's room, the owner implied: a run in another room, or none, is nobody. (The Python's
/// `env_members` can answer "unknown" or "owner only" too, and the hook turns both into nobody.)
fn present_now(ctx: &Ctx, room: &str) -> Vec<String> {
    if ctx.room != room {
        return Vec::new();
    }
    ctx.members
        .split(',')
        .map(|m| py_strip(m).to_lowercase())
        .filter(|m| is_id(m))
        .collect()
}

/// `(room, rel)` for a file under `<home>/memory/rooms/<room>/`, links resolved on both sides.
fn target(home: &str, data: &Value) -> Option<(String, String)> {
    let path = data.get("tool_input")?.get("file_path")?.as_str()?;
    // a NUL in the path is not refused: the Python's non-strict `realpath` carries on with it
    if !path.starts_with('/') {
        return None;
    }
    let mem = realpath(&Path::new(home).join("memory"));
    let real = realpath(Path::new(path));
    let within = real.strip_prefix(&mem).ok()?.to_str()?.to_owned();
    let parts: Vec<&str> = within.split('/').collect();
    if parts.len() < 3 || parts[0] != "rooms" || !is_id(parts[1]) {
        return None;
    }
    Some((parts[1].to_owned(), within))
}

fn ledger_file(state: &Path, agent: &str, room: &str) -> Option<PathBuf> {
    (is_id(agent) && is_id(room)).then(|| {
        state
            .join("room-ledger")
            .join(agent)
            .join(format!("{room}.json"))
    })
}

fn py_eq(left: &Value, right: &Value) -> bool {
    let as_num = |v: &Value| match v {
        Value::Bool(t) => Some(f64::from(u8::from(*t))),
        Value::Number(n) => n.as_f64(),
        _ => None,
    };
    match (as_num(left), as_num(right)) {
        (Some(x), Some(y)) => x == y,
        _ => left == right,
    }
}

/// Whether Python's `a < b` works for two values of a list being sorted (numbers and booleans together, strings
/// together, lists by their first difference).
fn comparable(left: &Value, right: &Value) -> bool {
    let num = |v: &Value| matches!(v, Value::Number(_) | Value::Bool(_));
    match (left, right) {
        (Value::String(_), Value::String(_)) => true,
        (Value::Array(x), Value::Array(y)) => match x.iter().zip(y).find(|(p, q)| !py_eq(p, q)) {
            Some((p, q)) => comparable(p, q),
            None => true,
        },
        _ => num(left) && num(right),
    }
}

/// `sorted(list)` does not raise (a list of one or none never compares anything).
fn sortable(items: &[Value]) -> bool {
    items.len() < 2 || items.iter().all(|v| comparable(&items[0], v))
}

/// `rel in kith_facets.read_ledger(agent, room)`: an unreadable ledger is an empty one, and so is one that is not an
/// object or holds a list `sorted()` cannot sort (the Python sorts every list before it answers).
fn recorded(file: &Path, rel: &str) -> bool {
    let Ok(text) = fs::read_to_string(file) else {
        return false;
    };
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    let all_sort = map
        .values()
        .all(|v| v.as_array().is_none_or(|a| sortable(a)));
    all_sort && map.get(rel).is_some_and(Value::is_array)
}

/// `set(value)` of a recorded entry, as strings: a list's strings (numbers, booleans and null are hashable and never
/// equal an id), a string's characters, an object's keys; anything else raises.
fn strings_of(v: &Value) -> io::Result<Vec<String>> {
    let bad = || io::Error::other("not iterable or not hashable");
    match v {
        Value::Array(items) => {
            let mut out = Vec::new();
            for x in items {
                match x {
                    Value::String(s) => out.push(s.clone()),
                    Value::Number(_) | Value::Bool(_) | Value::Null => {}
                    Value::Array(_) | Value::Object(_) => return Err(bad()),
                }
            }
            Ok(out)
        }
        Value::String(s) => Ok(s.chars().map(String::from).collect()),
        Value::Object(m) => Ok(m.keys().cloned().collect()),
        _ => Err(bad()),
    }
}

/// An exclusive flock on `<ledger>.lock`, held until dropped.
struct Lock(#[allow(dead_code)] File);

fn lock(file: &Path) -> io::Result<Lock> {
    let mut name = file.as_os_str().to_owned();
    name.push(".lock");
    let f = OpenOptions::new().append(true).create(true).open(name)?;
    f.lock_exclusive()?;
    Ok(Lock(f))
}

fn id_list(mut list: Vec<String>) -> Value {
    list.sort();
    list.dedup();
    Value::Array(list.into_iter().map(Value::String).collect())
}

/// `kith_facets.record_room_write`: narrow the record of a note to `present`. A note that already existed with no
/// record is first set to the owner alone. Under the lock; any failure is an error (the hook drops it).
fn record(file: &Path, rel: &str, present: &[String], existed: bool) -> io::Result<()> {
    let parent = file.parent().unwrap_or(Path::new("."));
    if !parent.is_dir() {
        // parents with the default mode, the ledger folder itself 0700, as `Path.mkdir(parents=True, mode=0o700)`
        if let Some(up) = parent.parent() {
            fs::create_dir_all(up)?;
        }
        match fs::DirBuilder::new().mode(0o700).create(parent) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && parent.is_dir() => {}
            Err(e) => return Err(e),
        }
    }
    let _lock = lock(file)?;
    let mut map = match fs::read_to_string(file)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    {
        Some(Value::Object(m)) => m,
        None => Map::new(),
        Some(_) => return Err(io::Error::other("ledger is not an object")),
    };
    let value = if let Some(old) = map.get(rel) {
        id_list(
            strings_of(old)?
                .into_iter()
                .filter(|s| present.contains(s))
                .collect(),
        )
    } else if existed {
        id_list(Vec::new())
    } else {
        id_list(present.to_vec())
    };
    map.insert(rel.to_owned(), value);
    let mut tmp_name = file.file_name().unwrap_or_default().to_owned();
    tmp_name.push(format!(".tmp{}", std::process::id()));
    let tmp = file.with_file_name(tmp_name);
    File::create(&tmp)?.write_all(pyjson::dumps_indent_n(&Value::Object(map), 1).as_bytes())?;
    fs::rename(&tmp, file)
}

/// The hook: always 0 and silent.
pub fn run(ctx: &Ctx, args: &[String], stdin: &mut dyn Read) -> i32 {
    let [mode, flag_agent, agent, flag_home, home, ..] = args else {
        return 0;
    };
    if !matches!(mode.as_str(), "pre" | "post") || flag_agent != "--agent" || flag_home != "--home"
    {
        return 0;
    }
    let mut raw = Vec::new();
    if stdin.read_to_end(&mut raw).is_err() {
        return 0;
    }
    let Ok(data) = serde_json::from_slice::<Value>(&raw) else {
        return 0;
    };
    let Some((room, rel)) = target(home, &data) else {
        return 0;
    };
    // a record that cannot be written leaves the note unrecorded: owner only
    let _ = (|| -> io::Result<()> {
        let file =
            ledger_file(&ctx.state, agent, &room).ok_or_else(|| io::Error::other("bad id"))?;
        if mode == "pre" {
            let existed = Path::new(home).join("memory").join(&rel).is_file();
            if existed && !recorded(&file, &rel) {
                record(&file, &rel, &[], true)?;
            }
            Ok(())
        } else {
            record(&file, &rel, &present_now(ctx, &room), false)
        }
    })();
    0
}

#[cfg(test)]
mod tests;
