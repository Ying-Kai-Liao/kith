//! Cancel and rollback of the transcripts step (the model's `Cancel`, `Rollback`, `ExportBack`, `RollbackDone`).
//!
//! Rollback is the framework's: the record says rolling-back (hooks stay in Writer mode and spool), [`export_back`] runs, the
//! tombstone must be gone, the record is cleared. `export_back`, in this order:
//! 1. drain the spool into the rows (newer-wins; each entry deleted once it is in);
//! 2. write each row to its file only where the file is missing or the row is newer by the one order key (A1/A3/A6), under
//!    the session's lock; files with no row (pruned) stay as they are, a file newer than its row stays;
//! 3. drain once more, which shrinks the window below;
//! 4. remove `registry.tombstone`, last, so Writer mode holds until the switch record is cleared too.
//!
//! The gap that is left (documented, A4): an entry a hook spools after step 3 and before the record is cleared waits for the
//! next Files-mode drain, i.e. the next session start on this Mac or kith-sched's hourly pass; until then the files do not show
//! that session start. The rows stay in core.db, so a later step finds them.
//!
//! Everything is idempotent: a crash in the middle is repaired by running `kith rollback transcripts` again.

use super::transcripts::{ctx, marker_of, state};
use crate::cutover::Env;
use crate::db::Core;
use crate::error::{Error, Result};
use crate::transcripts::reconcile::{drain_once, export_rows};

fn remove_marker(env: &Env) -> Result<Vec<String>> {
    let m = marker_of(state(env)?);
    match std::fs::remove_file(&m) {
        Ok(()) => Ok(vec![format!("transcripts: removed {}", m.display())]),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(Error::Io(format!(
            "cutover: transcripts: {}: {e}",
            m.display()
        ))),
    }
}

/// A step cancelled while `stepping`: the files were written all along, so only the tombstone goes. Spool entries a hook left
/// while it stood are drained by the Files-mode drains.
pub(super) fn cancel(env: &Env) -> Result<Vec<String>> {
    remove_marker(env)
}

pub(super) fn export_back(env: &Env, core: &mut Core) -> Result<Vec<String>> {
    let dir = kith_kernel::threads::registry_dir(state(env)?);
    let c = ctx(env);
    let mut lines = Vec::new();
    let first = drain_once(core, &c, &dir)?;
    lines.extend(first.notes);
    let written = export_rows(core, &dir)?;
    let second = drain_once(core, &c, &dir)?;
    lines.extend(second.notes);
    lines.push(format!(
        "transcripts: {} spool entr(ies) taken in, {written} registry file(s) written from rows (only where the row was newer)",
        first.drained + second.drained
    ));
    lines.extend(remove_marker(env)?);
    lines.push(
        "transcripts: an entry spooled after the last drain waits for the next session start or kith-sched's hourly drain".into(),
    );
    Ok(lines)
}
