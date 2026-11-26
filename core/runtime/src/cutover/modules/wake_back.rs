//! The `wake` rollback's export (WK6 brief section 2): the core's rows written back as the files `kernel/bin/kith-wake` reads,
//! under `host.lock` and both queue locks, with the record at `rolling-back` (Python stays in Core mode and the core refuses
//! its writes until the record is cleared, so nobody writes the queue meanwhile).
//!
//! Order, each step safe to run again after a crash (the rows do not change, so the files come out the same):
//! 1. the waiting runs are handed back owed and a running run is kept with its process (`wake::rollback::hand_back`);
//! 2. every item is written to `queue/<sha256(session)[:32]>.json`, `answered.json` and `waits/<id>.json` atomically;
//! 3. every other `*.json` of `queue/` and `waits/` is moved to a `.before-rollback-<at>/` folder beside it: those are the
//!    files from before the switch, and one left in place would bring back a request the core answered since, to go out twice;
//! 4. the tombstone goes last: before it is gone Python still reads Core mode.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::wake::{queue_dir, state, tombstone_of, waits_dir};
use crate::cutover::Env;
use crate::db::Core;
use crate::error::{Error, Result};
use crate::wake::{file_key, number, rollback};

fn io(what: &Path, e: &std::io::Error) -> Error {
    Error::Io(format!("cutover: wake: {}: {e}", what.display()))
}

fn private_dir(dir: &Path) -> Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| io(dir, &e))
}

/// Temporary file, then rename, as `kith-wake`'s `write_atomic` does.
fn write_atomic(path: &Path, v: &Value) -> Result<()> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| io(&tmp, &e))?;
    f.write_all(v.to_string().as_bytes())
        .and_then(|()| f.sync_all())
        .map_err(|e| io(&tmp, &e))?;
    std::fs::rename(&tmp, path).map_err(|e| io(path, &e))
}

/// Moves every `*.json` of `dir` that is not in `keep` to `dir/.before-rollback-<at>/`. Returns the names moved.
fn move_aside(dir: &Path, keep: &BTreeSet<String>, at: i64) -> Result<Vec<String>> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io(dir, &e)),
    };
    let mut names: Vec<String> = rd
        .filter_map(std::result::Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            !n.starts_with('.')
                && Path::new(n)
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("json"))
                && !keep.contains(n)
        })
        .collect();
    names.sort();
    if names.is_empty() {
        return Ok(names);
    }
    let aside: PathBuf = dir.join(format!(".before-rollback-{at}"));
    private_dir(&aside)?;
    for n in &names {
        std::fs::rename(dir.join(n), aside.join(n)).map_err(|e| io(&dir.join(n), &e))?;
    }
    Ok(names)
}

pub(super) fn export(env: &Env, core: &mut Core) -> Result<Vec<String>> {
    let state = state(env)?;
    let (queue, waits) = (queue_dir(state), waits_dir(state));
    let now = crate::holder_link::now_ms();
    let at = now / 1000;
    let back = rollback::hand_back(core, now)?;
    private_dir(&queue)?;
    private_dir(&waits)?;
    let mut lines = vec![format!(
        "wake: {} run(s) that were waiting are owed again; {} run(s) in flight go back with their process",
        back.owed, back.running
    )];
    let (mut kept_q, mut kept_w) = (BTreeSet::new(), BTreeSet::new());
    let mut items = 0;
    for (session, it) in &back.state.items {
        if it.requests.is_empty() {
            continue;
        }
        let mut v = it.to_json();
        v["v"] = json!(1);
        let name = format!("{}.json", file_key(session));
        write_atomic(&queue.join(&name), &v)?;
        kept_q.insert(name);
        items += 1;
    }
    if !back.state.answered.is_empty() {
        let mut o = Map::new();
        for (id, a) in &back.state.answered {
            o.insert(
                id.clone(),
                json!({"how": a.how, "t": number(a.t), "run": a.run}),
            );
        }
        write_atomic(&queue.join("answered.json"), &Value::Object(o))?;
        kept_q.insert("answered.json".into());
    }
    for (id, w) in &back.state.waiters {
        let name = format!("{id}.json");
        write_atomic(&waits.join(&name), &w.to_json())?;
        kept_w.insert(name);
    }
    lines.push(format!(
        "wake: wrote {} item(s), {} answered id(s) and {} waiter(s)",
        items,
        back.state.answered.len(),
        kept_w.len()
    ));
    for (dir, keep) in [(&queue, &kept_q), (&waits, &kept_w)] {
        let moved = move_aside(dir, keep, at)?;
        if !moved.is_empty() {
            lines.push(format!(
                "wake: {} file(s) from before the switch moved to {}/.before-rollback-{at}",
                moved.len(),
                dir.display()
            ));
        }
    }
    let tomb = tombstone_of(state);
    match std::fs::remove_file(&tomb) {
        Ok(()) => lines.push(format!("wake: removed {}", tomb.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(io(&tomb, &e)),
    }
    Ok(lines)
}
