//! `record` in both modes of the registry (`kernel/lib/kith_registry.py`, the `transcripts` cutover module):
//!
//! - Files (no marker): the hook writes `<sid>.json` itself, then drains the inbox ([`drain`]): an entry a Writer-mode hook
//!   left before a rollback goes in place of the file when it is newer.
//! - Writer (the marker exists, or `transcripts.switch` says more than `stepping`): the core is the only writer. The hook
//!   writes a spool entry `inbox/<sid>.<recordedAt>.<pid>.json` first (a hook killed mid-call loses nothing), then calls
//!   `session.record`, and deletes its own entry only after an answer that settles it. Anything else leaves it for the
//!   core's drain, or for a Files-mode drain after a rollback.
//!
//! The one order key is `(recordedAt, procStart as text, arrival)`, later wins; a record without `recordedAt` counts its
//! file's mtime in nanoseconds.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value, json};

use super::core::{Core, CoreErr, registry_switched};
use super::registry::{build, sid_lock, write_entry};
use super::{Ctx, create_private_dir};

/// A `SessionStart` hook has 5 s in all, and the ps walk comes first.
const CALL: Duration = Duration::from_secs(3);
/// A dot-temp this old in the inbox is a hook killed mid-write.
const TEMP_AGE: Duration = Duration::from_secs(300);

fn inbox(reg: &Path) -> PathBuf {
    reg.join("inbox")
}

/// The registry's file-name rule (`kith_registry.safe_id`, the host's `registry::safe_id`).
fn safe_id(sid: &str) -> bool {
    super::safe_id(sid)
}

/// The hook: the lines to print to stderr, each after `kith-sessions record: ` (the exit code stays 0, listing never
/// blocks a session).
pub fn record(c: &Ctx, core: Option<&dyn Core>, stdin: &str) -> Vec<String> {
    let b = match build(c, stdin) {
        Ok(Some(b)) => b,
        Ok(None) => return Vec::new(),
        Err(e) => return vec![e],
    };
    let Some(reg) = c.reg_dir() else {
        return Vec::new();
    };
    if registry_switched(c) {
        return record_in_core(&reg, core, &b.sid, &b.entry);
    }
    if let Err(e) = write_entry(&reg, &b) {
        return vec![e];
    }
    drain(c);
    // The step's marker landed after the first look: the core may have read the folder already.
    if registry_switched(c) {
        return record_in_core(&reg, core, &b.sid, &b.entry);
    }
    Vec::new()
}

/// Writer mode's record: the spool entry first, then `session.record`; only an answer that settles it deletes this hook's
/// own entry. Never writes `<sid>.json`: the core writes it from its row.
fn record_in_core(reg: &Path, core: Option<&dyn Core>, sid: &str, rec: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    if !safe_id(sid) {
        lines.push(format!(
            "session id '{sid}' is not one the registry can name; not listed"
        ));
        return lines;
    }
    let entry = match spool(reg, sid, rec) {
        Ok(p) => Some(p),
        Err(e) => {
            // Disk full, a permission: still ask the core, only the safety net is gone.
            lines.push(format!("no spool entry ({e}); asking the core anyway"));
            None
        }
    };
    let (what, why) = call(core, rec);
    if matches!(what, Call::Settled | Call::Invalid)
        && let Some(p) = &entry
    {
        let _ = std::fs::remove_file(p);
    }
    match what {
        Call::Invalid => lines.push(format!("the core refused the record ({why}); not listed")),
        Call::Keep => lines.push(format!("kept for the core ({why})")),
        Call::Settled => {}
    }
    lines
}

enum Call {
    /// The core has it, or newer: delete the spool.
    Settled,
    /// The core will never take it: delete, drop.
    Invalid,
    /// Not serving, no answer, a timeout, a rejected call: the spool stays.
    Keep,
}

fn call(core: Option<&dyn Core>, rec: &Value) -> (Call, String) {
    let Some(core) = core else {
        return (Call::Keep, "no kith program to ask".to_owned());
    };
    let ans = match core.call("session.record", &json!({"record": rec}), Some(CALL)) {
        Ok(a) => a,
        Err(CoreErr::Refused(e)) => {
            return (Call::Keep, format!("the core rejected the call: {e}"));
        }
        Err(CoreErr::NoAnswer(why)) => return (Call::Keep, why),
    };
    let text = |k: &str| {
        ans.get(k)
            .filter(|v| super::resolve::truthy(v))
            .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
    };
    if matches!(
        ans.get("outcome").and_then(Value::as_str),
        Some("recorded" | "older")
    ) {
        return (Call::Settled, String::new());
    }
    if ans.get("refused").and_then(Value::as_str) == Some("invalid") {
        return (
            Call::Invalid,
            text("why").unwrap_or_else(|| "invalid record".to_owned()),
        );
    }
    (
        Call::Keep,
        text("why")
            .or_else(|| text("refused"))
            .unwrap_or_else(|| "no outcome in the answer".to_owned()),
    )
}

/// The record's spool entry `inbox/<sid>.<recordedAt>.<pid>.json`, written atomically (0700 folder, 0600 file); its path.
fn spool(reg: &Path, sid: &str, rec: &Value) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;
    create_private_dir(reg)?;
    let dir = inbox(reg);
    create_private_dir(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let at = rec.get("recordedAt").map_or_else(String::new, |v| {
        v.as_u64().map_or_else(|| v.to_string(), |n| n.to_string())
    });
    let path = dir.join(format!("{sid}.{at}.{}.json", std::process::id()));
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    ));
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(crate::secrets::pyjson::dumps(rec).as_bytes())?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// The inbox's entries as `(path, sid)`, by name. Names that start with `.` (temp files) or do not end in `.json` are
/// skipped; any other name is an entry, its sid the part before the first dot (`None` when the name is not
/// `<sid>.<recordedAt>.<pid>.json` with a safe sid: the drain deletes it as invalid).
fn entries(reg: &Path) -> Vec<(PathBuf, Option<String>)> {
    let Ok(rd) = std::fs::read_dir(inbox(reg)) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
        .into_iter()
        .filter(|n| !n.starts_with('.') && Path::new(n).extension().is_some_and(|x| x == "json"))
        .map(|n| {
            let parts: Vec<&str> = n.split('.').collect();
            let sid = (parts.len() == 4 && safe_id(parts[0])).then(|| parts[0].to_owned());
            (inbox(reg).join(&n), sid)
        })
        .collect()
}

/// Deletes the inbox's dot-names (a spool write's temp files) older than the age; a younger one may be a write in progress.
fn sweep_temps(reg: &Path) {
    let Ok(rd) = std::fs::read_dir(inbox(reg)) else {
        return;
    };
    for e in rd.flatten() {
        if !e.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let Ok(m) = std::fs::symlink_metadata(e.path()) else {
            continue;
        };
        if m.is_file()
            && m.modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > TEMP_AGE)
        {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// What a record must be to be taken (the contract's validation): a JSON object, a safe sid naming this sid, `v` the integer
/// 1, no `person` key.
fn valid(rec: &Value, sid: &str) -> bool {
    let Some(o) = rec.as_object() else {
        return false;
    };
    safe_id(sid)
        && o.get("sessionId").and_then(Value::as_str) == Some(sid)
        && o.get("v")
            .is_some_and(|v| v.is_i64() && v.as_i64() == Some(1))
        && !o.contains_key("person")
}

type Key = (i128, String, i64);

/// The one order key: `(recordedAt, procStart as text, arrival)`; a record without an integer `recordedAt` counts its
/// file's mtime in nanoseconds, or -1 with no file to look at.
fn order_key(rec: &Map<String, Value>, path: Option<&Path>, arrival: i64) -> Key {
    let at = match rec.get("recordedAt") {
        Some(v) if v.is_i64() || v.is_u64() => {
            i128::from(v.as_i64().unwrap_or(0)).max(i128::from(v.as_u64().unwrap_or(0)))
        }
        _ => path
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(-1, |d| i128::try_from(d.as_nanos()).unwrap_or(-1)),
    };
    let start = match rec.get("procStart") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    };
    (at, start, arrival)
}

fn read_any(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Files mode only: puts each inbox entry newer (by the order key) than its `<sid>.json` in place of it, deletes the rest,
/// each under the sid's lock (a lock not had within the wait skips that entry for the next drain). An entry the core would
/// refuse as invalid is deleted, never placed. In Writer mode it does nothing: the core drains.
pub fn drain(c: &Ctx) -> usize {
    use std::os::unix::fs::PermissionsExt as _;
    let Some(reg) = c.reg_dir() else {
        return 0;
    };
    if registry_switched(c) {
        return 0;
    }
    sweep_temps(&reg);
    let mut moved = 0;
    for (n, (path, sid)) in (1_i64..).zip(entries(&reg)) {
        let Some(sid) = sid else {
            let _ = std::fs::remove_file(&path);
            continue;
        };
        let Some(_held) = sid_lock(&reg, &sid) else {
            continue;
        };
        let rec = read_any(&path);
        let Some(rec) = rec.filter(|r| valid(r, &sid)) else {
            let _ = std::fs::remove_file(&path);
            continue;
        };
        let (Some(rec), target) = (rec.as_object(), reg.join(format!("{sid}.json"))) else {
            continue;
        };
        let have = target
            .exists()
            .then(|| read_any(&target))
            .flatten()
            .and_then(|v| v.as_object().cloned());
        let newer = have
            .as_ref()
            .is_none_or(|h| order_key(rec, Some(&path), n) > order_key(h, Some(&target), 0));
        if newer {
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            if std::fs::rename(&path, &target).is_ok() {
                moved += 1;
            }
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
    moved
}
