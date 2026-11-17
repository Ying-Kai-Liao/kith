//! The hourly prune of the registry once the core writes it, with `kith-sched prune_terminal`'s rule: a record older than
//! seven days whose process is not running (no pid: gone) goes, its file and its `session_records` row together, under the
//! session's lock. Its `engine_sessions` and `conversations` rows stay, so `conversation.transcript` still answers for it.
//! The age is the row's order key (`recordedAt`, or for an older hook's record the file's modification time when it was
//! read): what the file's modification time was for `kith-sched`, since the hook rewrote the file at every session start.

use std::path::Path;

use rusqlite::params;
use serde_json::Value;

use super::project::{file_of, with_lock};
use super::record;
use crate::db::Core;
use crate::error::Result;

/// `kith-sched`'s `TERMINAL_DAYS`.
pub(crate) const DAYS: i64 = 7;

/// How often the daemon prunes (`kith-sched`'s `PRUNE_EVERY`).
pub(crate) const EVERY: std::time::Duration = std::time::Duration::from_secs(3600);

const NS_PER_DAY: i64 = 86_400 * 1_000_000_000;

/// One prune at `now_ns`; `running(pid)` says whether a process runs. Answers the sessions that went.
pub(crate) fn prune_once(
    core: &mut Core,
    dir: &Path,
    now_ns: i64,
    running: &dyn Fn(i64) -> bool,
) -> Result<Vec<String>> {
    let cutoff = now_ns.saturating_sub(DAYS * NS_PER_DAY);
    let old: Vec<(String, i64)> = {
        let mut q = core.conn().prepare(
            "SELECT sid, recorded_at FROM session_records WHERE recorded_at <= ?1 ORDER BY sid",
        )?;
        q.query_map([cutoff], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    let mut gone = Vec::new();
    for (sid, at) in old {
        let pid =
            record::row(core.conn(), &sid)?.and_then(|(m, _)| m.get("pid").and_then(Value::as_i64));
        if pid.is_some_and(|p| p > 0 && running(p)) {
            continue;
        }
        let path = file_of(dir, &sid);
        let pruned = with_lock(&path, || -> Result<bool> {
            let tx = core.claim_tx()?;
            // Only the row that was judged: a record that came in since keeps its session.
            let n = tx.execute(
                "DELETE FROM session_records WHERE sid = ?1 AND recorded_at = ?2",
                params![sid, at],
            )?;
            tx.commit()?;
            if n > 0 {
                match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
            Ok(n > 0)
        })?;
        if pruned {
            gone.push(sid);
        }
    }
    Ok(gone)
}

/// Whether a process runs, as `kith-sched`'s `running` asks it (a pid we may not signal is kept, which only keeps a record).
pub(crate) fn running(pid: i64) -> bool {
    u32::try_from(pid).is_ok_and(|p| !crate::gate::pid_gone(p))
}
