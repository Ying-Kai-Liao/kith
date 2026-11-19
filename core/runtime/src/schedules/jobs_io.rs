//! The one way the core writes an agent's `jobs.json`: under the exclusive side of `<machine>/sched/<agent>.jobs.lock` (the
//! lock `kith-sched` edits under and the readers take shared), replacing the file whole as `kith-sched` does. The routine
//! import (cutover) and the `kith schedule` write verbs (`edit.rs`) both write through here.
//!
//! The lock is let go before anything touches core.db: the fire takes it shared INSIDE its claim transaction, so a writer that
//! kept it into one would reverse the order and make the fire wait out its read timeout.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::error::{Error, Result};

/// `jobs.json` replaced whole (a temp file in the same folder, synced, then renamed), as `kith-sched` writes it: two-space
/// indent, a newline at the end, never half written.
pub(crate) fn write_list(file: &Path, jobs: &[Value]) -> Result<()> {
    use std::io::Write as _;
    let io = |e: std::io::Error| Error::Io(format!("kith-core: schedule: {}: {e}", file.display()));
    let dir = file.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).map_err(io)?;
    let tmp = dir.join(format!(".jobs.json.{}.tmp", std::process::id()));
    let mut body = serde_json::to_string_pretty(&Value::Array(jobs.to_vec()))
        .map_err(|e| Error::Io(format!("kith-core: schedule: {e}")))?;
    body.push('\n');
    {
        let mut f = std::fs::File::create(&tmp).map_err(io)?;
        f.write_all(body.as_bytes()).map_err(io)?;
        f.sync_all().map_err(io)?;
    }
    std::fs::rename(&tmp, file).map_err(io)
}

/// The exclusive side of the agent's jobs lock; let go on drop.
pub(crate) struct JobsLock(#[allow(dead_code, reason = "held for its drop")] std::fs::File);

/// Takes it, waiting at most `wait` for another writer.
pub(crate) fn lock_jobs(machine: &Path, agent: &str, wait: Duration) -> Result<JobsLock> {
    use fs2::FileExt as _;
    let dir = machine.join("sched");
    let path = dir.join(format!("{agent}.jobs.lock"));
    let io = |e: std::io::Error| Error::Io(format!("kith-core: schedule: {}: {e}", path.display()));
    std::fs::create_dir_all(&dir).map_err(io)?;
    let f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(io)?;
    let until = Instant::now() + wait;
    loop {
        match f.try_lock_exclusive() {
            Ok(()) => return Ok(JobsLock(f)),
            Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
                if Instant::now() >= until {
                    return Err(Error::Refused(format!(
                        "kith-core: schedule: {} is held by another writer (kith-sched?) for more than {}s; try again",
                        path.display(),
                        wait.as_secs()
                    )));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(io(e)),
        }
    }
}
