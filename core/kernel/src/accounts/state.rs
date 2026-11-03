//! The state file: what each account is doing right now (`kernel/lib/kith_accounts.py`, "the state file"). One file for the
//! whole Mac, `<machine>/accounts/state.json`, `{"version": 1, "accounts": {"claude:work": {"out_until", "reason", "since",
//! "cleared_at", "usage", "last_started"}}}`. This side reads it and notes `last_started` for a launch; marking an account
//! out or clearing it, and usage readings, stay with the Python commands.
//!
//! Writers take an exclusive lock on `state.json.lock` (the same `flock` the Python side takes), read the file again under
//! it, and swap a new one in (temp file and rename), so two runs marking different accounts at once lose nothing. A corrupt
//! file is kept as `state.json.corrupt`, then replaced on the next write. A disk that can't be written to loses the mark;
//! the run goes on.

use super::{Map, create_new, pyjson, temp_in};
use fs2::FileExt as _;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// A usage reading older than this says nothing about now (as the host's).
const USAGE_STALE: f64 = 30.0 * 60.0;

/// An account that is out of usage until a time, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    pub until: i64,
    pub reason: String,
}

pub fn state_file(machine: &Path) -> PathBuf {
    machine.join("accounts").join("state.json")
}

/// Python's `float(v)`: numbers, booleans and numeric text; anything else raises there, `None` here.
fn pyfloat(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Python's `int(v)`: numbers (cut towards zero), booleans and whole-number text.
#[allow(clippy::cast_possible_truncation)]
fn pyint(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::Bool(b) => Some(i64::from(*b)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Python's `str(v)` for the values a reason can be.
fn pystr(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_owned(),
        Value::Null => "None".to_owned(),
        other => pyjson::dumps(other),
    }
}

/// Python truthiness of a JSON value.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn reason_of(v: &Map) -> String {
    match v.get("reason") {
        Some(r) if truthy(Some(r)) => pystr(r),
        _ => "rate_limit".to_owned(),
    }
}

/// `(accounts, corrupt)`: the file's accounts; empty and `corrupt` when it exists but isn't ours.
fn read_raw(machine: &Path) -> (Map, bool) {
    let text = match std::fs::read(state_file(machine)) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Map::new(), false),
        Err(_) => return (Map::new(), true),
    };
    let Ok(Value::Object(mut d)) = serde_json::from_slice::<Value>(&text) else {
        return (Map::new(), true);
    };
    let Some(Value::Object(accts)) = d.remove("accounts") else {
        return (Map::new(), true);
    };
    (
        accts.into_iter().filter(|(_, v)| v.is_object()).collect(),
        false,
    )
}

/// The file's text for these accounts, as Python writes it.
fn render(accts: &Map) -> String {
    let mut top = Map::new();
    top.insert("version".to_owned(), Value::from(1));
    top.insert("accounts".to_owned(), Value::Object(accts.clone()));
    pyjson::dumps_pretty(&Value::Object(top)) + "\n"
}

/// `_write_atomic(path, data, 0o600)`: a temp file beside it, then the swap.
pub fn write_atomic(path: &Path, data: &[u8], mode: u32) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = temp_in(dir, &format!(".{name}."), "");
    let result = (|| {
        let mut f = create_new(&tmp, mode)?;
        f.write_all(data)?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn lock(machine: &Path) -> std::io::Result<std::fs::File> {
    let f = state_file(machine);
    if let Some(dir) = f.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut name = f.into_os_string();
    name.push(".lock");
    let l = std::fs::OpenOptions::new()
        .write(true)
        .truncate(false)
        .create(true)
        .open(PathBuf::from(name))?;
    l.lock_exclusive()?;
    Ok(l)
}

/// `change(accounts)` under the lock, then written back atomically. A disk that can't be written to loses the mark and the
/// run goes on, but the loss is noted in the host's log (`crate::log`) instead of vanishing.
pub fn update(machine: &Path, change: impl FnOnce(&mut Map)) {
    let done = (|| -> std::io::Result<()> {
        let _lock = lock(machine)
            .map_err(|e| std::io::Error::new(e.kind(), format!("can't take the lock: {e}")))?;
        let (mut accts, corrupt) = read_raw(machine);
        if corrupt {
            let f = state_file(machine);
            let _ = std::fs::rename(&f, f.with_file_name("state.json.corrupt"));
        }
        change(&mut accts);
        write_atomic(&state_file(machine), render(&accts).as_bytes(), 0o600)
            .map_err(|e| std::io::Error::new(e.kind(), format!("can't write it: {e}")))
    })();
    if let Err(e) = done {
        crate::log::note(&format!(
            "accounts state: a change to {} was lost: {e}",
            state_file(machine).display()
        ));
    }
}

/// A launch is real: note when the account last started.
pub fn record_started(machine: &Path, entry: &str, now: i64) {
    update(machine, |accts| {
        let e = accts
            .entry(entry.to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(e) = e.as_object_mut() {
            e.insert("last_started".to_owned(), Value::from(now));
        }
    });
}

/// The host's own file (read only: it holds it in memory and rewrites it until it moves to the state file) and the file the
/// first kernel chain wrote beside it (imported once, then renamed).
fn legacy_files(machine: &Path) -> (PathBuf, PathBuf) {
    let d = machine.join("serve");
    (d.join("backups.json"), d.join("kernel-backups-out.json"))
}

/// The entries of a legacy file that name a time in the future or past (`out_until` above 0); nothing at all when any is
/// unreadable, as the Python's one `try` around the lot.
fn legacy_marks(path: &Path) -> Vec<(String, Map)> {
    let Ok(text) = std::fs::read(path) else {
        return vec![];
    };
    let Ok(Value::Object(d)) = serde_json::from_slice::<Value>(&text) else {
        return vec![];
    };
    let mut out = vec![];
    for (k, v) in d {
        if let Value::Object(m) = v {
            let until = match m.get("out_until") {
                None => 0.0,
                Some(u) => match pyfloat(u) {
                    Some(f) => f,
                    None => return vec![],
                },
            };
            if until > 0.0 {
                out.push((k, m));
            }
        }
    }
    out
}

/// `serve/kernel-backups-out.json` into the state file, once: the later `out_until` wins; the old file is renamed.
fn import_kernel_marks(machine: &Path) {
    let old = legacy_files(machine).1;
    if !old.exists() {
        return;
    }
    let _ = (|| -> Option<()> {
        let _lock = lock(machine).ok()?;
        let marks = legacy_marks(&old);
        let (mut accts, _) = read_raw(machine);
        for (k, v) in marks {
            let e = accts
                .entry(k)
                .or_insert_with(|| Value::Object(Map::new()))
                .as_object_mut()?;
            let until = pyfloat(v.get("out_until")?)?;
            let have = e.get("out_until").map_or(Some(0.0), pyfloat)?;
            if until > have {
                let since = match v.get("since") {
                    Some(s) if truthy(Some(s)) => pyint(s)?,
                    _ => pyint(v.get("out_until")?)?,
                };
                e.insert(
                    "out_until".to_owned(),
                    Value::from(pyint(v.get("out_until")?)?),
                );
                e.insert(
                    "reason".to_owned(),
                    Value::String(reason_of(&v).chars().take(40).collect()),
                );
                e.insert("since".to_owned(), Value::from(since));
            }
        }
        write_atomic(&state_file(machine), render(&accts).as_bytes(), 0o600).ok()?;
        let mut renamed = old.clone().into_os_string();
        renamed.push(".imported");
        std::fs::rename(&old, PathBuf::from(renamed)).ok()
    })();
}

/// The state file's accounts (none when there is no file or it is corrupt), the old kernel marks imported first.
pub fn read(machine: &Path) -> Map {
    import_kernel_marks(machine);
    read_raw(machine).0
}

/// Every account that is out of usage until a time in the future: this file, and the host's own file while the host still
/// keeps one (a mark of the host's older than the account's last `clear` is ignored).
pub fn out_marks(machine: &Path, state: &Map, now: f64) -> HashMap<String, Mark> {
    let mut marks: HashMap<String, Mark> = HashMap::new();
    for (k, v) in legacy_marks(&legacy_files(machine).0) {
        let cleared = state.get(&k).and_then(Value::as_object);
        let num = |m: Option<&Map>, key: &str| {
            m.and_then(|m| m.get(key)).and_then(pyfloat).unwrap_or(0.0)
        };
        let until = v.get("out_until").and_then(pyfloat).unwrap_or(0.0);
        // A mark the host made before our `clear` is ignored; one it made or extended after it (a later since, or an
        // out_until past the one that was cleared: the host keeps `since` when it extends) counts.
        if until > now
            && (num(Some(&v), "since") > num(cleared, "cleared_at")
                || until > num(cleared, "cleared_host_until"))
        {
            #[allow(clippy::cast_possible_truncation)]
            marks.insert(
                k,
                Mark {
                    until: until as i64,
                    reason: reason_of(&v),
                },
            );
        }
    }
    for (k, v) in state {
        let Some(v) = v.as_object() else { continue };
        let Some(until) = v.get("out_until").map_or(Some(0.0), pyfloat) else {
            continue;
        };
        #[allow(clippy::cast_precision_loss)] // epoch seconds fit an f64
        let have = marks.get(k).map_or(0.0, |m| m.until as f64);
        if until > now && until > have {
            let Some(i) = v.get("out_until").and_then(pyint) else {
                continue;
            };
            marks.insert(
                k.clone(),
                Mark {
                    until: i,
                    reason: reason_of(v),
                },
            );
        }
    }
    marks
}

/// The fullest window's percent of a fresh reading; `None` when there is none (or it is stale or damaged).
pub fn usage_level(entry: Option<&Value>, now: f64) -> Option<f64> {
    let u = entry?.get("usage")?.as_object().filter(|u| !u.is_empty())?;
    if now - pyfloat(u.get("read_at")?)? > USAGE_STALE {
        return None;
    }
    u.get("windows")?
        .as_array()?
        .iter()
        .map(|w| pyfloat(w.get("percent")?))
        .collect::<Option<Vec<f64>>>()?
        .into_iter()
        .reduce(f64::max)
}
