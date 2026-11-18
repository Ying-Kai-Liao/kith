//! The daemon's registry tick (architect MUST 1): while the `transcripts` module serves, the spool is drained and the rows
//! and files reconciled at the daemon's start and every [`EVERY`], and the registry is pruned every [`prune::EVERY`]. Until
//! the module is switched a pass reads the switch and does nothing: the daemon's pass touches no registry file before that. (The transcripts step itself does, under its lifetime lock: its re-read drains the spool, which writes files from rows, and a rollback's `export_back` writes rows to files.)

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{prune, reconcile, record};
use crate::daemon::Shared;
use crate::log;

/// How often the spool is drained and the files reconciled.
pub(crate) const EVERY: Duration = Duration::from_secs(60);

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

/// One pass; `pruned` is when the last prune ran. Answers the notes it logged.
pub(crate) fn pass(shared: &Shared, pruned: &mut Option<Instant>) -> Vec<String> {
    let out = shared.with_core(|c| {
        if record::serving(c).is_err() {
            return Ok(Vec::new());
        }
        let Some((dir, ctx)) = record::place(c) else {
            return Ok(Vec::new());
        };
        let mut notes = Vec::new();
        let drained = reconcile::drain_once(c, &ctx, &dir)?;
        notes.extend(drained.notes);
        let rec = reconcile::reconcile_once(c, &ctx, &dir)?;
        notes.extend(rec.notes);
        if rec.taken > 0 || rec.written > 0 {
            notes.push(format!(
                "{} record(s) taken from files, {} file(s) written from rows",
                rec.taken, rec.written
            ));
        }
        if pruned.is_none_or(|t| t.elapsed() >= prune::EVERY) {
            *pruned = Some(Instant::now());
            let gone = prune::prune_once(c, &dir, now_ns(), &prune::running)?;
            if !gone.is_empty() {
                notes.push(format!("pruned {} session record(s)", gone.len()));
            }
        }
        Ok(notes)
    });
    let notes = out.unwrap_or_else(|e| vec![format!("pass failed: {e}")]);
    for n in &notes {
        log::note(&shared.logs, &format!("daemon: transcripts: {n}"));
    }
    notes
}

/// The tick's thread: a pass at once, then one every [`EVERY`] until the daemon stops.
pub(crate) fn spawn(shared: Arc<Shared>) {
    let logs = shared.logs.clone();
    let spawned = std::thread::Builder::new()
        .name("kith-core-transcripts".into())
        .spawn(move || {
            let mut pruned = None;
            while !shared.stopping() {
                pass(&shared, &mut pruned);
                let mut waited = Duration::ZERO;
                while waited < EVERY && !shared.stopping() {
                    std::thread::sleep(Duration::from_millis(100));
                    waited += Duration::from_millis(100);
                }
            }
        });
    if let Err(e) = spawned {
        // The tick is not the daemon's way in: without it records still go in, only an older hook's file waits.
        log::note(&logs, &format!("daemon: no transcripts tick: {e}"));
    }
}
